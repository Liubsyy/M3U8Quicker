use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use futures::StreamExt;
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

use crate::error::AppError;
use crate::models::{
    live_group_for_status, DownloadId, LiveProgressEvent, LiveProtocol, LiveRecordStatus,
    LiveRecordTask, RequestHeaders,
};
use crate::persistence;

const FLV_HEADER_SKIP_LEN: usize = 13; // 9 bytes FLV signature + 4 bytes PreviousTagSize0
const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(500);

/// Outcome of one recording session when it ended cleanly without cancel.
/// Cancellation paths surface as `AppError::Cancelled` and are interpreted via
/// the stop reason stored in `LiveStopSignal`.
pub enum LiveRecordOutcome {
    /// Remote stream ended (EOF) without user intervention.
    Finished,
}

/// Build the absolute output file path for a live task.
pub fn live_output_file_path(task: &LiveRecordTask) -> PathBuf {
    let extension = task.protocol.default_extension();
    Path::new(&task.output_dir).join(format!("{}.{}", task.filename, extension))
}

/// Reason driving the worker to stop (must be set by the command layer
/// before triggering the cancel token, so the worker knows what to do).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveStopReason {
    Pause,
    Stop,
    Cancel,
}

pub struct LiveStopSignal {
    pub reason: Mutex<Option<LiveStopReason>>,
    pub token: CancellationToken,
}

impl LiveStopSignal {
    pub fn new() -> Self {
        Self {
            reason: Mutex::new(None),
            token: CancellationToken::new(),
        }
    }

    pub async fn trigger(&self, reason: LiveStopReason) {
        {
            let mut current = self.reason.lock().await;
            *current = Some(reason);
        }
        self.token.cancel();
    }
}

pub async fn run_live_record(
    app_handle: AppHandle,
    live_records: Arc<Mutex<HashMap<DownloadId, LiveRecordTask>>>,
    client: Arc<RwLock<reqwest::Client>>,
    task_id: DownloadId,
    headers: Arc<RequestHeaders>,
    signal: Arc<LiveStopSignal>,
    append: bool,
) {
    let task_snapshot = {
        let map = live_records.lock().await;
        map.get(&task_id).cloned()
    };
    let Some(task) = task_snapshot else {
        return;
    };

    let output_path = live_output_file_path(&task);
    let url = task.url.clone();
    let protocol = task.protocol;
    let initial_bytes = task.total_bytes;
    let initial_duration_ms = task.duration_ms;
    let client = client.read().await.clone();

    let result = match protocol {
        LiveProtocol::Flv => {
            run_flv_record(
                app_handle.clone(),
                live_records.clone(),
                client,
                task_id.clone(),
                url,
                headers,
                output_path.clone(),
                signal.clone(),
                initial_bytes,
                initial_duration_ms,
                append,
            )
            .await
        }
    };

    let stop_reason = signal.reason.lock().await.clone();
    let final_status = match result {
        Ok(LiveRecordOutcome::Finished) => Some(LiveRecordStatus::Recorded),
        Err(AppError::Cancelled) => match stop_reason {
            Some(LiveStopReason::Pause) => Some(LiveRecordStatus::Paused),
            Some(LiveStopReason::Stop) => Some(LiveRecordStatus::Recorded),
            Some(LiveStopReason::Cancel) => Some(LiveRecordStatus::Cancelled),
            None => Some(LiveRecordStatus::Paused),
        },
        Err(err) => Some(LiveRecordStatus::Failed(err.to_string())),
    };

    if matches!(final_status, Some(LiveRecordStatus::Cancelled)) {
        let _ = tokio::fs::remove_file(&output_path).await;
    }

    if let Some(status) = final_status {
        let mut snapshot = None;
        {
            let mut map = live_records.lock().await;
            if let Some(task) = map.get_mut(&task_id) {
                task.status = status.clone();
                task.speed_bytes_per_sec = 0;
                let now = task.touch();
                if matches!(
                    status,
                    LiveRecordStatus::Recorded
                        | LiveRecordStatus::Failed(_)
                        | LiveRecordStatus::Cancelled
                ) {
                    task.completed_at = Some(now);
                }
                if matches!(status, LiveRecordStatus::Cancelled) {
                    task.file_path = None;
                }
                snapshot = Some(task.clone());
            }
        }
        if let Some(task) = snapshot {
            let _ = persistence::save_live_task(&app_handle, &task).await;
            emit_live_progress_from_task(&app_handle, &task);
        }
    }
}

async fn run_flv_record(
    app_handle: AppHandle,
    live_records: Arc<Mutex<HashMap<DownloadId, LiveRecordTask>>>,
    client: reqwest::Client,
    task_id: DownloadId,
    url: String,
    headers: Arc<RequestHeaders>,
    output_path: PathBuf,
    signal: Arc<LiveStopSignal>,
    initial_bytes: u64,
    initial_duration_ms: u64,
    append: bool,
) -> Result<LiveRecordOutcome, AppError> {
    if let Some(parent) = output_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let mut request = client.get(&url);
    for (name, value) in headers.iter() {
        request = request.header(name, value);
    }

    let response = tokio::select! {
        result = request.send() => result.map_err(AppError::from)?,
        _ = signal.token.cancelled() => return Err(AppError::Cancelled),
    };
    let response = response.error_for_status()?;

    let mut stream = response.bytes_stream();

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(&output_path)
        .await?;

    let mut total_bytes: u64 = initial_bytes;
    let mut skipped: usize = 0;
    let need_skip = append; // resume: drop FLV header from new connection

    let start_instant = Instant::now();
    let mut last_emit = Instant::now();
    let mut last_emit_bytes = total_bytes;

    emit_live_progress(
        &app_handle,
        &live_records,
        &task_id,
        LiveRecordStatus::Recording,
        total_bytes,
        0,
        initial_duration_ms,
    )
    .await;

    loop {
        let next = tokio::select! {
            item = stream.next() => item,
            _ = signal.token.cancelled() => {
                file.flush().await?;
                drop(file);
                return Err(AppError::Cancelled);
            }
        };

        let Some(chunk) = next else {
            break;
        };
        let chunk = chunk.map_err(|e| AppError::Network(e.to_string()))?;

        let mut payload: &[u8] = &chunk;
        if need_skip && skipped < FLV_HEADER_SKIP_LEN {
            let remaining = FLV_HEADER_SKIP_LEN - skipped;
            if payload.len() <= remaining {
                skipped += payload.len();
                continue;
            } else {
                payload = &payload[remaining..];
                skipped = FLV_HEADER_SKIP_LEN;
            }
        }

        file.write_all(payload).await?;
        total_bytes += payload.len() as u64;

        if last_emit.elapsed() >= PROGRESS_EMIT_INTERVAL {
            let delta_bytes = total_bytes.saturating_sub(last_emit_bytes);
            let elapsed = last_emit.elapsed().as_secs_f64().max(0.001);
            let speed = (delta_bytes as f64 / elapsed) as u64;
            last_emit_bytes = total_bytes;
            last_emit = Instant::now();

            let duration_ms =
                initial_duration_ms + start_instant.elapsed().as_millis() as u64;

            emit_live_progress(
                &app_handle,
                &live_records,
                &task_id,
                LiveRecordStatus::Recording,
                total_bytes,
                speed,
                duration_ms,
            )
            .await;
        }
    }

    file.flush().await?;
    drop(file);

    let duration_ms = initial_duration_ms + start_instant.elapsed().as_millis() as u64;
    {
        let mut map = live_records.lock().await;
        if let Some(task) = map.get_mut(&task_id) {
            task.total_bytes = total_bytes;
            task.duration_ms = duration_ms;
        }
    }

    Ok(LiveRecordOutcome::Finished)
}

async fn emit_live_progress(
    app_handle: &AppHandle,
    live_records: &Arc<Mutex<HashMap<DownloadId, LiveRecordTask>>>,
    task_id: &str,
    status: LiveRecordStatus,
    total_bytes: u64,
    speed: u64,
    duration_ms: u64,
) {
    let mut snapshot = None;
    {
        let mut map = live_records.lock().await;
        if let Some(task) = map.get_mut(task_id) {
            task.total_bytes = total_bytes;
            task.speed_bytes_per_sec = speed;
            task.duration_ms = duration_ms;
            task.status = status.clone();
            task.touch();
            snapshot = Some(task.clone());
        }
    }

    if let Some(task) = snapshot {
        emit_live_progress_from_task(app_handle, &task);
    }
}

fn emit_live_progress_from_task(app_handle: &AppHandle, task: &LiveRecordTask) {
    let updated_at = task
        .updated_at
        .unwrap_or_else(Utc::now)
        .to_rfc3339();
    let event = LiveProgressEvent {
        id: task.id.clone(),
        status: task.status.clone(),
        group: live_group_for_status(&task.status),
        total_bytes: task.total_bytes,
        speed_bytes_per_sec: task.speed_bytes_per_sec,
        duration_ms: task.duration_ms,
        updated_at,
    };
    let _ = app_handle.emit("live-progress", event);
}
