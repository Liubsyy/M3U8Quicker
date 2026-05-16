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
use url::Url;

use crate::error::AppError;
use crate::models::{
    live_group_for_status, DownloadId, HlsMediaKind, LiveProgressEvent, LiveProtocol,
    LiveRecordStatus, LiveRecordTask, RequestHeaders,
};
use crate::persistence;

const FLV_HEADER_SKIP_LEN: usize = 13; // 9 bytes FLV signature + 4 bytes PreviousTagSize0
const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(500);
const HLS_MIN_REFRESH_INTERVAL: Duration = Duration::from_millis(800);
const HLS_MAX_REFRESH_INTERVAL: Duration = Duration::from_secs(6);
const HLS_PLAYLIST_FETCH_TIMEOUT: Duration = Duration::from_secs(15);
const LIVE_RECORD_RETRY_INTERVAL: Duration = Duration::from_secs(2);

/// Outcome of one recording session when it ended cleanly without cancel.
/// Cancellation paths surface as `AppError::Cancelled` and are interpreted via
/// the stop reason stored in `LiveStopSignal`.
pub enum LiveRecordOutcome {
    /// Remote stream ended (EOF) without user intervention.
    Finished,
}

/// Build the absolute output file path for a single-file live task (FLV).
pub fn live_output_file_path(task: &LiveRecordTask) -> PathBuf {
    let extension = task.protocol.default_extension();
    Path::new(&task.output_dir).join(format!("{}.{}", task.filename, extension))
}

/// Build the working directory path for an HLS live task. Segments + local index.m3u8 are
/// written here during recording and moved to a permanent location on Stop.
pub fn live_hls_temp_dir(task: &LiveRecordTask) -> PathBuf {
    Path::new(&task.output_dir).join(format!(".live_{}", task.id))
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

    let url = task.url.clone();
    let protocol = task.protocol;
    let initial_bytes = task.total_bytes;
    let initial_duration_ms = task.duration_ms;
    let client = client.read().await.clone();

    let mut append_next = append;
    let final_status = loop {
        let (current_bytes, current_duration_ms) = current_live_record_counters(
            &live_records,
            &task_id,
            initial_bytes,
            initial_duration_ms,
        )
        .await;

        let result = match protocol {
            LiveProtocol::Flv => {
                let output_path = live_output_file_path(&task);
                run_flv_record(
                    app_handle.clone(),
                    live_records.clone(),
                    client.clone(),
                    task_id.clone(),
                    url.clone(),
                    headers.clone(),
                    output_path,
                    signal.clone(),
                    current_bytes,
                    current_duration_ms,
                    append_next,
                )
                .await
            }
            LiveProtocol::Hls => {
                let temp_dir = task
                    .temp_dir
                    .clone()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| live_hls_temp_dir(&task));
                run_hls_record(
                    app_handle.clone(),
                    live_records.clone(),
                    client.clone(),
                    task_id.clone(),
                    url.clone(),
                    headers.clone(),
                    temp_dir,
                    signal.clone(),
                    current_bytes,
                    current_duration_ms,
                    append_next,
                )
                .await
            }
        };

        match result {
            Ok(LiveRecordOutcome::Finished) => break Some(LiveRecordStatus::Recorded),
            Err(AppError::Cancelled) => {
                let stop_reason = signal.reason.lock().await.clone();
                break match stop_reason {
                    Some(LiveStopReason::Pause) => Some(LiveRecordStatus::Paused),
                    Some(LiveStopReason::Stop) => Some(LiveRecordStatus::Recorded),
                    Some(LiveStopReason::Cancel) => Some(LiveRecordStatus::Cancelled),
                    None => Some(LiveRecordStatus::Paused),
                };
            }
            Err(err) => {
                eprintln!(
                    "[live_recorder] recording attempt failed, keep recording: {}",
                    err
                );
                let (total_bytes, duration_ms) = current_live_record_counters(
                    &live_records,
                    &task_id,
                    current_bytes,
                    current_duration_ms,
                )
                .await;
                emit_live_progress(
                    &app_handle,
                    &live_records,
                    &task_id,
                    LiveRecordStatus::Recording,
                    total_bytes,
                    0,
                    duration_ms,
                )
                .await;
                append_next = true;
                if wait_or_cancel(&signal, LIVE_RECORD_RETRY_INTERVAL).await {
                    let stop_reason = signal.reason.lock().await.clone();
                    break match stop_reason {
                        Some(LiveStopReason::Pause) => Some(LiveRecordStatus::Paused),
                        Some(LiveStopReason::Stop) => Some(LiveRecordStatus::Recorded),
                        Some(LiveStopReason::Cancel) => Some(LiveRecordStatus::Cancelled),
                        None => Some(LiveRecordStatus::Paused),
                    };
                }
            }
        }
    };

    let task_snapshot_for_finalize = {
        let map = live_records.lock().await;
        map.get(&task_id).cloned()
    };

    if let (Some(status), Some(mut task)) = (final_status.clone(), task_snapshot_for_finalize) {
        // Protocol-specific finalization on disk.
        match task.protocol {
            LiveProtocol::Flv => {
                if matches!(status, LiveRecordStatus::Cancelled) {
                    if let Some(path) = task.file_path.as_ref() {
                        let _ = tokio::fs::remove_file(path).await;
                    }
                }
            }
            LiveProtocol::Hls => {
                let temp_dir = task
                    .temp_dir
                    .clone()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| live_hls_temp_dir(&task));
                match status {
                    LiveRecordStatus::Recorded => {
                        // Move temp dir to a stable named directory next to output_dir.
                        if temp_dir.exists() {
                            let playlist = temp_dir.join("index.m3u8");
                            if let Err(err) = finalize_local_hls_playlist(&playlist).await {
                                eprintln!(
                                    "[live_recorder] finalize playlist {} failed: {}",
                                    playlist.display(),
                                    err
                                );
                            }
                            let final_dir = pick_available_dir(&PathBuf::from(&task.output_dir), &task.filename);
                            match tokio::fs::rename(&temp_dir, &final_dir).await {
                                Ok(()) => {
                                    let final_playlist = final_dir.join("index.m3u8");
                                    task.file_path = Some(final_playlist.to_string_lossy().to_string());
                                    task.temp_dir = None;
                                }
                                Err(err) => {
                                    eprintln!(
                                        "[live_recorder] rename {} -> {} failed: {}",
                                        temp_dir.display(),
                                        final_dir.display(),
                                        err
                                    );
                                    // Fall back to keeping the temp dir; expose the playlist path inside it.
                                    let playlist = temp_dir.join("index.m3u8");
                                    if playlist.exists() {
                                        task.file_path = Some(playlist.to_string_lossy().to_string());
                                    }
                                }
                            }
                        }
                    }
                    LiveRecordStatus::Cancelled | LiveRecordStatus::Failed(_) => {
                        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
                        task.file_path = None;
                        task.temp_dir = None;
                    }
                    _ => {}
                }
            }
        }

        // Update state + emit + persist.
        let mut snapshot = None;
        {
            let mut map = live_records.lock().await;
            if let Some(entry) = map.get_mut(&task_id) {
                entry.status = status.clone();
                entry.speed_bytes_per_sec = 0;
                entry.file_path = task.file_path.clone();
                entry.temp_dir = task.temp_dir.clone();
                entry.hls_media_kind = task.hls_media_kind;
                entry.segment_count = task.segment_count;
                let now = entry.touch();
                if matches!(
                    status,
                    LiveRecordStatus::Recorded
                        | LiveRecordStatus::Failed(_)
                        | LiveRecordStatus::Cancelled
                ) {
                    entry.completed_at = Some(now);
                }
                snapshot = Some(entry.clone());
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

async fn run_hls_record(
    app_handle: AppHandle,
    live_records: Arc<Mutex<HashMap<DownloadId, LiveRecordTask>>>,
    client: reqwest::Client,
    task_id: DownloadId,
    url: String,
    headers: Arc<RequestHeaders>,
    temp_dir: PathBuf,
    signal: Arc<LiveStopSignal>,
    initial_bytes: u64,
    initial_duration_ms: u64,
    append: bool,
) -> Result<LiveRecordOutcome, AppError> {
    tokio::fs::create_dir_all(&temp_dir).await?;

    // Sync working state into task so other readers can see it.
    {
        let mut map = live_records.lock().await;
        if let Some(task) = map.get_mut(&task_id) {
            task.temp_dir = Some(temp_dir.to_string_lossy().to_string());
        }
    }

    let mut total_bytes = initial_bytes;
    let mut duration_ms_acc = initial_duration_ms;
    let mut media_kind: Option<HlsMediaKind> = None;
    let mut next_local_index: u64 = 0;
    let mut last_remote_seq: Option<u64> = None;
    #[allow(unused_assignments)]
    let mut target_duration_ms: u64 = 4000; // overwritten from playlist on first iteration
    let mut segment_count: u64 = 0;

    // If resuming an existing temp dir, recover progress from the existing index.m3u8.
    let playlist_path = temp_dir.join("index.m3u8");
    if append && playlist_path.exists() {
        if let Ok(existing) = tokio::fs::read_to_string(&playlist_path).await {
            let (recovered_count, recovered_kind) = inspect_existing_local_playlist(&existing);
            next_local_index = recovered_count;
            segment_count = recovered_count;
            media_kind = recovered_kind;
        }
    }

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
        duration_ms_acc,
    )
    .await;

    loop {
        if signal.token.is_cancelled() {
            return Err(AppError::Cancelled);
        }

        let fetch = tokio::select! {
            res = fetch_media_playlist(&client, &url, headers.as_ref()) => res,
            _ = signal.token.cancelled() => return Err(AppError::Cancelled),
        };

        let (base_url, media) = match fetch {
            Ok(value) => value,
            Err(err) => {
                eprintln!("[live_recorder][hls] playlist fetch error: {err}");
                if wait_or_cancel(&signal, HLS_MIN_REFRESH_INTERVAL).await {
                    return Err(AppError::Cancelled);
                }
                continue;
            }
        };

        let playlist_target_ms = (media.target_duration as u64).max(1) * 1000;
        target_duration_ms = playlist_target_ms;

        // Establish media kind on first parse with EXT-X-MAP info.
        let has_map = media.segments.iter().any(|s| s.map.is_some());
        if media_kind.is_none() {
            let kind = if has_map {
                HlsMediaKind::Fmp4
            } else {
                HlsMediaKind::MpegTs
            };
            media_kind = Some(kind);
            {
                let mut map = live_records.lock().await;
                if let Some(task) = map.get_mut(&task_id) {
                    task.hls_media_kind = Some(kind);
                }
            }
        }
        let kind = media_kind.unwrap_or(HlsMediaKind::MpegTs);

        // Determine which segments are new.
        let mut new_segments: Vec<(u64, m3u8_rs::MediaSegment)> = Vec::new();
        for (offset, segment) in media.segments.iter().enumerate() {
            let remote_seq = media.media_sequence + offset as u64;
            if let Some(last) = last_remote_seq {
                if remote_seq <= last {
                    continue;
                }
            }
            new_segments.push((remote_seq, segment.clone()));
        }

        if media
            .segments
            .last()
            .is_some()
        {
            let last_remote = media.media_sequence + media.segments.len() as u64 - 1;
            last_remote_seq = Some(match last_remote_seq {
                Some(prev) => prev.max(last_remote),
                None => last_remote,
            });
        }

        // Process new segments in order. Track the init segment uri encountered so we only download it once.
        let mut current_init_local_name: Option<String> = match kind {
            HlsMediaKind::Fmp4 => existing_init_local_name(&temp_dir).await,
            HlsMediaKind::MpegTs => None,
        };
        let mut last_seen_init_uri: Option<String> = None;

        for (remote_seq, segment) in new_segments {
            if signal.token.is_cancelled() {
                return Err(AppError::Cancelled);
            }

            // Detect encrypted segments (we currently don't support live AES).
            if let Some(key) = segment.key.as_ref() {
                if !matches!(key.method, m3u8_rs::KeyMethod::None) {
                    return Err(AppError::M3u8Parse(
                        "暂不支持加密 (AES) HLS 直播录制".to_string(),
                    ));
                }
            }

            // Handle init segment (fMP4 only).
            if kind == HlsMediaKind::Fmp4 {
                if let Some(map) = segment.map.as_ref() {
                    let resolved_init = resolve_url(&base_url, &map.uri);
                    if last_seen_init_uri.as_deref() != Some(resolved_init.as_str())
                        || current_init_local_name.is_none()
                    {
                        let init_name = format!("init_{:06}.mp4", next_local_index);
                        let init_path = temp_dir.join(&init_name);
                        let bytes = tokio::select! {
                            res = fetch_bytes(&client, &resolved_init, headers.as_ref()) => res?,
                            _ = signal.token.cancelled() => return Err(AppError::Cancelled),
                        };
                        tokio::fs::write(&init_path, &bytes).await?;
                        total_bytes += bytes.len() as u64;
                        current_init_local_name = Some(init_name);
                        last_seen_init_uri = Some(resolved_init);
                    }
                }
            }

            // Download segment bytes (cancellation-aware).
            let resolved_seg = resolve_url(&base_url, &segment.uri);
            let bytes = tokio::select! {
                res = fetch_bytes(&client, &resolved_seg, headers.as_ref()) => match res {
                    Ok(b) => b,
                    Err(err) => {
                        eprintln!(
                            "[live_recorder][hls] segment {} fetch error: {}",
                            remote_seq, err
                        );
                        continue;
                    }
                },
                _ = signal.token.cancelled() => return Err(AppError::Cancelled),
            };

            let ext = match kind {
                HlsMediaKind::MpegTs => "ts",
                HlsMediaKind::Fmp4 => "m4s",
            };
            let local_name = format!("seg_{:08}.{}", next_local_index, ext);
            let local_path = temp_dir.join(&local_name);
            tokio::fs::write(&local_path, &bytes).await?;
            total_bytes += bytes.len() as u64;
            duration_ms_acc += (segment.duration * 1000.0) as u64;
            segment_count += 1;
            next_local_index += 1;

            // Append entry to local playlist.
            append_local_playlist_entry(
                &playlist_path,
                kind,
                target_duration_ms,
                &local_name,
                segment.duration,
                if segment_count == 1 {
                    current_init_local_name.as_deref()
                } else {
                    None
                },
            )
            .await?;

            // Throttled progress emit.
            if last_emit.elapsed() >= PROGRESS_EMIT_INTERVAL {
                let delta_bytes = total_bytes.saturating_sub(last_emit_bytes);
                let elapsed = last_emit.elapsed().as_secs_f64().max(0.001);
                let speed = (delta_bytes as f64 / elapsed) as u64;
                last_emit_bytes = total_bytes;
                last_emit = Instant::now();

                {
                    let mut map = live_records.lock().await;
                    if let Some(task) = map.get_mut(&task_id) {
                        task.segment_count = segment_count;
                    }
                }

                emit_live_progress(
                    &app_handle,
                    &live_records,
                    &task_id,
                    LiveRecordStatus::Recording,
                    total_bytes,
                    speed,
                    duration_ms_acc,
                )
                .await;
            }
        }

        // If playlist had ENDLIST tag, finish gracefully.
        if media.end_list {
            break;
        }

        // Sleep until next refresh, respect cancel.
        let refresh = Duration::from_millis((target_duration_ms / 2).max(800).min(6000));
        let refresh = refresh.clamp(HLS_MIN_REFRESH_INTERVAL, HLS_MAX_REFRESH_INTERVAL);
        if wait_or_cancel(&signal, refresh).await {
            return Err(AppError::Cancelled);
        }
    }

    // Sync final counters before finishing.
    {
        let mut map = live_records.lock().await;
        if let Some(task) = map.get_mut(&task_id) {
            task.total_bytes = total_bytes;
            task.duration_ms = duration_ms_acc;
            task.segment_count = segment_count;
        }
    }

    let _ = start_instant;
    Ok(LiveRecordOutcome::Finished)
}

async fn fetch_media_playlist(
    client: &reqwest::Client,
    url: &str,
    headers: &RequestHeaders,
) -> Result<(Url, m3u8_rs::MediaPlaylist), AppError> {
    fetch_media_playlist_inner(client, url, headers, 0).await
}

async fn fetch_media_playlist_inner(
    client: &reqwest::Client,
    url: &str,
    headers: &RequestHeaders,
    depth: u8,
) -> Result<(Url, m3u8_rs::MediaPlaylist), AppError> {
    if depth >= 4 {
        return Err(AppError::M3u8Parse(
            "Master playlist nesting too deep".to_string(),
        ));
    }
    let base_url = Url::parse(url).map_err(AppError::from)?;
    let mut request = client.get(url).timeout(HLS_PLAYLIST_FETCH_TIMEOUT);
    for (name, value) in headers.iter() {
        request = request.header(name, value);
    }
    let response = request.send().await?.error_for_status()?;
    let bytes = response.bytes().await?;
    let playlist = m3u8_rs::parse_playlist_res(&bytes).map_err(|_| {
        AppError::InvalidInput("链接内容不是有效的 M3U8 播放列表".to_string())
    })?;
    match playlist {
        m3u8_rs::Playlist::MediaPlaylist(media) => Ok((base_url, media)),
        m3u8_rs::Playlist::MasterPlaylist(master) => {
            let variant = master
                .variants
                .iter()
                .filter(|v| !v.is_i_frame)
                .filter(|v| !v.uri.trim().is_empty())
                .max_by_key(|v| v.bandwidth)
                .ok_or_else(|| AppError::M3u8Parse("Master playlist has no variants".to_string()))?;
            let next_url = resolve_url(&base_url, &variant.uri);
            Box::pin(fetch_media_playlist_inner(client, &next_url, headers, depth + 1)).await
        }
    }
}

async fn fetch_bytes(
    client: &reqwest::Client,
    url: &str,
    headers: &RequestHeaders,
) -> Result<bytes::Bytes, AppError> {
    let mut request = client.get(url);
    for (name, value) in headers.iter() {
        request = request.header(name, value);
    }
    let response = request.send().await?.error_for_status()?;
    Ok(response.bytes().await?)
}

fn resolve_url(base: &Url, relative: &str) -> String {
    if relative.starts_with("http://") || relative.starts_with("https://") {
        relative.to_string()
    } else {
        base.join(relative)
            .map(|u| u.to_string())
            .unwrap_or_else(|_| relative.to_string())
    }
}

async fn existing_init_local_name(temp_dir: &Path) -> Option<String> {
    let playlist = temp_dir.join("index.m3u8");
    if let Ok(text) = tokio::fs::read_to_string(&playlist).await {
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("#EXT-X-MAP:URI=\"") {
                if let Some(end) = rest.find('"') {
                    return Some(rest[..end].to_string());
                }
            }
        }
    }
    None
}

fn inspect_existing_local_playlist(text: &str) -> (u64, Option<HlsMediaKind>) {
    let mut count = 0u64;
    let mut has_map = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("#EXT-X-MAP:") {
            has_map = true;
        } else if !trimmed.starts_with('#') && !trimmed.is_empty() {
            count += 1;
        }
    }
    let kind = if count == 0 {
        None
    } else if has_map {
        Some(HlsMediaKind::Fmp4)
    } else {
        Some(HlsMediaKind::MpegTs)
    };
    (count, kind)
}

async fn append_local_playlist_entry(
    playlist_path: &Path,
    kind: HlsMediaKind,
    target_duration_ms: u64,
    local_segment_name: &str,
    duration_secs: f32,
    init_local_name: Option<&str>,
) -> Result<(), AppError> {
    let target_seconds = ((target_duration_ms + 999) / 1000).max(1);
    let need_header = !playlist_path.exists();
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(playlist_path)
        .await?;
    if need_header {
        let mut header = String::new();
        header.push_str("#EXTM3U\n");
        header.push_str("#EXT-X-VERSION:6\n");
        header.push_str(&format!("#EXT-X-TARGETDURATION:{}\n", target_seconds));
        header.push_str("#EXT-X-MEDIA-SEQUENCE:0\n");
        header.push_str("#EXT-X-PLAYLIST-TYPE:VOD\n");
        if kind == HlsMediaKind::Fmp4 {
            if let Some(name) = init_local_name {
                header.push_str(&format!("#EXT-X-MAP:URI=\"{}\"\n", name));
            }
        }
        file.write_all(header.as_bytes()).await?;
    }
    let entry = format!(
        "#EXTINF:{:.3},\n{}\n",
        duration_secs.max(0.0),
        local_segment_name
    );
    file.write_all(entry.as_bytes()).await?;
    file.flush().await?;
    Ok(())
}

async fn finalize_local_hls_playlist(playlist_path: &Path) -> Result<(), AppError> {
    let mut text = match tokio::fs::read_to_string(playlist_path).await {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };

    if text
        .lines()
        .any(|line| line.trim().eq_ignore_ascii_case("#EXT-X-ENDLIST"))
    {
        return Ok(());
    }

    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("#EXT-X-ENDLIST\n");
    tokio::fs::write(playlist_path, text).await?;
    Ok(())
}

/// Ensure `<filename>` is not already taken under `output_dir`; otherwise append `_N`.
pub fn pick_available_dir(output_dir: &Path, filename: &str) -> PathBuf {
    let candidate = output_dir.join(filename);
    if !candidate.exists() {
        return candidate;
    }
    let mut counter: u32 = 1;
    loop {
        let attempt = output_dir.join(format!("{}_{}", filename, counter));
        if !attempt.exists() {
            return attempt;
        }
        counter += 1;
        if counter > 9999 {
            return output_dir.join(format!("{}_{}", filename, Utc::now().timestamp_millis()));
        }
    }
}

/// Returns true if the wait was interrupted by cancellation.
async fn wait_or_cancel(signal: &LiveStopSignal, duration: Duration) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(duration) => false,
        _ = signal.token.cancelled() => true,
    }
}

async fn current_live_record_counters(
    live_records: &Arc<Mutex<HashMap<DownloadId, LiveRecordTask>>>,
    task_id: &str,
    fallback_total_bytes: u64,
    fallback_duration_ms: u64,
) -> (u64, u64) {
    let map = live_records.lock().await;
    map.get(task_id)
        .map(|task| (task.total_bytes, task.duration_ms))
        .unwrap_or((fallback_total_bytes, fallback_duration_ms))
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

pub(crate) fn emit_live_progress_from_task(app_handle: &AppHandle, task: &LiveRecordTask) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn unique_temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("m3u8quicker_{}_{}", name, uuid::Uuid::new_v4()))
    }

    #[tokio::test]
    async fn finalize_local_hls_playlist_appends_endlist_once() {
        let temp_root = unique_temp_path("live-endlist");
        fs::create_dir_all(&temp_root).expect("create temp root");
        let playlist_path = temp_root.join("index.m3u8");
        fs::write(
            &playlist_path,
            "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:4.000,\nseg_00000001.m4s\n",
        )
        .expect("write playlist");

        finalize_local_hls_playlist(&playlist_path)
            .await
            .expect("first finalize");
        finalize_local_hls_playlist(&playlist_path)
            .await
            .expect("second finalize");

        let text = fs::read_to_string(&playlist_path).expect("read playlist");
        assert_eq!(text.matches("#EXT-X-ENDLIST").count(), 1);
        assert!(text.ends_with("#EXT-X-ENDLIST\n"));
        let _ = fs::remove_dir_all(&temp_root);
    }
}
