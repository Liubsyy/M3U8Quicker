use std::collections::{HashMap, VecDeque};
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use futures::StreamExt;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::error::AppError;
use crate::models::{
    live_group_for_status, DownloadId, HlsMediaKind, LiveProgressEvent, LiveProtocol,
    LiveRecordStatus, LiveRecordTask, RequestHeaders, DEFAULT_HLS_PLAYLIST_TIMEOUT_SECS,
    DEFAULT_HLS_REFRESH_MAX_MS, DEFAULT_HLS_REFRESH_MIN_MS, DEFAULT_LIVE_RETRY_FLV_MS,
    DEFAULT_LIVE_RETRY_HLS_MS, DEFAULT_LIVE_SEGMENT_TIMEOUT_SECS,
};
use crate::persistence;

const FLV_HEADER_SKIP_LEN: usize = 13; // 9 bytes FLV signature + 4 bytes PreviousTagSize0
const FLV_TAG_HEADER_LEN: usize = 11;
const FLV_TAG_PREVIOUS_SIZE_LEN: usize = 4;
const FLV_DEDUPE_MAX_TAGS: usize = 2048;
const FLV_DEDUPE_MAX_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(500);
// 运行时可配置的录播节奏参数。由设置加载/变更时通过 `set_live_settings` 更新，
// 全部为逐请求/逐循环读取，无需重建 HTTP 客户端。
static HLS_REFRESH_MIN_MS: AtomicU64 = AtomicU64::new(DEFAULT_HLS_REFRESH_MIN_MS);
static HLS_REFRESH_MAX_MS: AtomicU64 = AtomicU64::new(DEFAULT_HLS_REFRESH_MAX_MS);
static HLS_PLAYLIST_TIMEOUT_SECS: AtomicU64 = AtomicU64::new(DEFAULT_HLS_PLAYLIST_TIMEOUT_SECS);
static LIVE_SEGMENT_TIMEOUT_SECS: AtomicU64 = AtomicU64::new(DEFAULT_LIVE_SEGMENT_TIMEOUT_SECS);
static LIVE_RETRY_HLS_MS: AtomicU64 = AtomicU64::new(DEFAULT_LIVE_RETRY_HLS_MS);
static LIVE_RETRY_FLV_MS: AtomicU64 = AtomicU64::new(DEFAULT_LIVE_RETRY_FLV_MS);

fn hls_min_refresh() -> Duration {
    Duration::from_millis(HLS_REFRESH_MIN_MS.load(Ordering::Relaxed))
}

fn hls_max_refresh() -> Duration {
    Duration::from_millis(HLS_REFRESH_MAX_MS.load(Ordering::Relaxed))
}

fn hls_playlist_timeout() -> Duration {
    Duration::from_secs(HLS_PLAYLIST_TIMEOUT_SECS.load(Ordering::Relaxed))
}

fn live_segment_timeout() -> Duration {
    Duration::from_secs(LIVE_SEGMENT_TIMEOUT_SECS.load(Ordering::Relaxed))
}

fn live_retry_hls() -> Duration {
    Duration::from_millis(LIVE_RETRY_HLS_MS.load(Ordering::Relaxed))
}

fn flv_live_retry() -> Duration {
    Duration::from_millis(LIVE_RETRY_FLV_MS.load(Ordering::Relaxed))
}

/// 更新可配置的录播节奏参数。
pub fn set_live_settings(
    hls_refresh_min_ms: u64,
    hls_refresh_max_ms: u64,
    hls_playlist_timeout_secs: u64,
    live_segment_timeout_secs: u64,
    live_retry_hls_ms: u64,
    live_retry_flv_ms: u64,
) {
    HLS_REFRESH_MIN_MS.store(hls_refresh_min_ms, Ordering::Relaxed);
    HLS_REFRESH_MAX_MS.store(hls_refresh_max_ms, Ordering::Relaxed);
    HLS_PLAYLIST_TIMEOUT_SECS.store(hls_playlist_timeout_secs, Ordering::Relaxed);
    LIVE_SEGMENT_TIMEOUT_SECS.store(live_segment_timeout_secs, Ordering::Relaxed);
    LIVE_RETRY_HLS_MS.store(live_retry_hls_ms, Ordering::Relaxed);
    LIVE_RETRY_FLV_MS.store(live_retry_flv_ms, Ordering::Relaxed);
}

/// 当前生效的录播参数 (refresh_min_ms, refresh_max_ms, playlist_timeout_secs,
/// segment_timeout_secs, retry_hls_ms, retry_flv_ms)，供 get_app_settings 回读。
pub fn live_settings_snapshot() -> (u64, u64, u64, u64, u64, u64) {
    (
        HLS_REFRESH_MIN_MS.load(Ordering::Relaxed),
        HLS_REFRESH_MAX_MS.load(Ordering::Relaxed),
        HLS_PLAYLIST_TIMEOUT_SECS.load(Ordering::Relaxed),
        LIVE_SEGMENT_TIMEOUT_SECS.load(Ordering::Relaxed),
        LIVE_RETRY_HLS_MS.load(Ordering::Relaxed),
        LIVE_RETRY_FLV_MS.load(Ordering::Relaxed),
    )
}
const FLV_SPEED_WINDOW: Duration = Duration::from_secs(3);

/// Sliding-window throughput tracker. Survives across FLV reconnect attempts so
/// brief disconnects do not collapse the displayed download speed to 0.
#[derive(Debug)]
struct SpeedTracker {
    window: Duration,
    samples: VecDeque<(Instant, u64)>,
}

impl SpeedTracker {
    fn new(window: Duration) -> Self {
        Self {
            window,
            samples: VecDeque::with_capacity(16),
        }
    }

    fn record(&mut self, total_bytes: u64) {
        let now = Instant::now();
        let bytes = match self.samples.back() {
            Some(&(_, last)) if total_bytes < last => last,
            _ => total_bytes,
        };
        self.samples.push_back((now, bytes));
        let cutoff = now.checked_sub(self.window);
        if let Some(cutoff) = cutoff {
            while self.samples.len() > 2 {
                match self.samples.front() {
                    Some(&(t, _)) if t < cutoff => {
                        self.samples.pop_front();
                    }
                    _ => break,
                }
            }
        }
    }

    fn speed_bps(&self) -> u64 {
        if self.samples.len() < 2 {
            return 0;
        }
        let (t0, b0) = *self.samples.front().unwrap();
        let (t1, b1) = *self.samples.back().unwrap();
        let dt = t1.saturating_duration_since(t0).as_secs_f64();
        if dt <= 0.0 {
            return 0;
        }
        let db = b1.saturating_sub(b0) as f64;
        (db / dt) as u64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FlvTagFingerprint {
    tag_type: u8,
    data_size: u32,
    timestamp: u32,
    stream_id: u32,
    payload_hash_128: u128,
}

#[derive(Debug, Clone)]
struct FlvTag {
    bytes: Vec<u8>,
    fingerprint: FlvTagFingerprint,
}

#[derive(Default)]
struct FlvTagParser {
    buffer: Vec<u8>,
}

impl FlvTagParser {
    fn push(&mut self, bytes: &[u8]) -> Vec<FlvTag> {
        self.buffer.extend_from_slice(bytes);
        let mut tags = Vec::new();

        loop {
            let Some(tag_len) = next_flv_tag_len(&self.buffer) else {
                break;
            };
            let tag_bytes = self.buffer.drain(..tag_len).collect::<Vec<_>>();
            if let Some(fingerprint) = flv_tag_fingerprint(&tag_bytes) {
                tags.push(FlvTag {
                    bytes: tag_bytes,
                    fingerprint,
                });
            }
        }

        tags
    }
}

#[derive(Default)]
struct FlvDedupeWindow {
    tags: VecDeque<FlvTagFingerprint>,
    total_payload_bytes: usize,
}

impl FlvDedupeWindow {
    fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }

    fn push(&mut self, fingerprint: FlvTagFingerprint) {
        self.total_payload_bytes = self
            .total_payload_bytes
            .saturating_add(fingerprint.data_size as usize);
        self.tags.push_back(fingerprint);

        while self.tags.len() > FLV_DEDUPE_MAX_TAGS
            || self.total_payload_bytes > FLV_DEDUPE_MAX_PAYLOAD_BYTES
        {
            let Some(removed) = self.tags.pop_front() else {
                break;
            };
            self.total_payload_bytes = self
                .total_payload_bytes
                .saturating_sub(removed.data_size as usize);
        }
    }

    fn fingerprints(&self) -> Vec<FlvTagFingerprint> {
        self.tags.iter().copied().collect()
    }
}

struct FlvBoundaryDedupe {
    history: Vec<FlvTagFingerprint>,
    pending: Vec<FlvTag>,
    candidates: Vec<usize>,
    best_complete: usize,
    preamble_len: usize,
    resolved: bool,
}

impl FlvBoundaryDedupe {
    fn new(window: &FlvDedupeWindow) -> Self {
        let history = window.fingerprints();
        let candidates = (1..=history.len()).collect::<Vec<_>>();
        let resolved = history.is_empty();

        Self {
            history,
            pending: Vec::new(),
            candidates,
            best_complete: 0,
            preamble_len: 0,
            resolved,
        }
    }

    fn process_tag(&mut self, tag: FlvTag) -> Vec<FlvTag> {
        if self.resolved {
            return vec![tag];
        }

        if self.pending.len() == self.preamble_len && is_flv_reconnect_preamble_tag(&tag) {
            self.pending.push(tag);
            self.preamble_len += 1;
            return Vec::new();
        }

        let fingerprint = tag.fingerprint;
        self.pending.push(tag);

        let matched_len = self.pending.len().saturating_sub(self.preamble_len);
        let history_len = self.history.len();
        let history = &self.history;
        self.candidates.retain(|&candidate_len| {
            matched_len <= candidate_len
                && history[history_len - candidate_len + matched_len - 1] == fingerprint
        });

        for &candidate_len in &self.candidates {
            if candidate_len == matched_len {
                self.best_complete = self.best_complete.max(candidate_len);
            }
        }

        if self.candidates.is_empty() {
            return self.resolve(self.best_complete);
        }

        if self
            .candidates
            .iter()
            .all(|&candidate_len| candidate_len == matched_len)
        {
            return self.resolve(self.best_complete.max(matched_len));
        }

        Vec::new()
    }

    fn resolve(&mut self, skip_count: usize) -> Vec<FlvTag> {
        self.resolved = true;
        let mut pending = std::mem::take(&mut self.pending);
        let skip_count = if skip_count > 0 {
            self.preamble_len.saturating_add(skip_count)
        } else {
            0
        }
        .min(pending.len());
        pending.drain(..skip_count);
        pending
    }
}

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

/// Pick an available file stem under `output_dir`; otherwise append `_N`.
pub fn pick_available_file_stem(
    output_dir: &Path,
    filename: &str,
    extension: &str,
    is_reserved: impl Fn(&Path) -> bool,
) -> String {
    let candidate_path =
        |candidate_name: &str| output_dir.join(format!("{}.{}", candidate_name, extension));

    let initial = candidate_path(filename);
    if !initial.exists() && !is_reserved(&initial) {
        return filename.to_string();
    }

    let mut counter: u32 = 1;
    loop {
        let candidate_name = format!("{}_{}", filename, counter);
        let candidate = candidate_path(&candidate_name);
        if !candidate.exists() && !is_reserved(&candidate) {
            return candidate_name;
        }
        counter += 1;
        if counter > 9999 {
            return format!("{}_{}", filename, Utc::now().timestamp_millis());
        }
    }
}

/// Build the working directory path for an HLS live task. Segments + local index.m3u8 are
/// written here during recording and moved to a permanent location on Stop.
pub fn live_hls_temp_dir(task: &LiveRecordTask) -> PathBuf {
    Path::new(&task.output_dir).join(format!("live_{}", task.id))
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
    let mut speed_tracker = SpeedTracker::new(FLV_SPEED_WINDOW);
    if protocol == LiveProtocol::Flv {
        speed_tracker.record(initial_bytes);
    }
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
                    &mut speed_tracker,
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
                // Hold the most recently computed speed across the reconnect
                // attempt — do not add new tracker samples here, so the value
                // does not decay while the connection is down.
                let retry_speed = if protocol == LiveProtocol::Flv {
                    speed_tracker.speed_bps()
                } else {
                    0
                };
                emit_live_progress(
                    &app_handle,
                    &live_records,
                    &task_id,
                    LiveRecordStatus::Recording,
                    total_bytes,
                    retry_speed,
                    duration_ms,
                )
                .await;
                append_next = true;
                if wait_or_cancel(&signal, live_record_retry_interval(protocol)).await {
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
                            let final_dir = pick_available_dir(
                                &PathBuf::from(&task.output_dir),
                                &task.filename,
                            );
                            match tokio::fs::rename(&temp_dir, &final_dir).await {
                                Ok(()) => {
                                    let final_playlist = final_dir.join("index.m3u8");
                                    task.file_path =
                                        Some(final_playlist.to_string_lossy().to_string());
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
                                        task.file_path =
                                            Some(playlist.to_string_lossy().to_string());
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

fn next_flv_tag_len(buffer: &[u8]) -> Option<usize> {
    if buffer.len() < FLV_TAG_HEADER_LEN {
        return None;
    }

    let data_size = read_u24_be(&buffer[1..4]) as usize;
    let tag_len = FLV_TAG_HEADER_LEN + data_size + FLV_TAG_PREVIOUS_SIZE_LEN;
    (buffer.len() >= tag_len).then_some(tag_len)
}

fn take_flv_connection_header_prefix<'a>(
    payload: &'a [u8],
    header_remaining: &mut usize,
) -> (&'a [u8], &'a [u8]) {
    let consumed = (*header_remaining).min(payload.len());
    *header_remaining -= consumed;
    (&payload[..consumed], &payload[consumed..])
}

fn is_flv_reconnect_preamble_tag(tag: &FlvTag) -> bool {
    match tag.fingerprint.tag_type {
        18 => true,
        8 => flv_tag_payload(&tag.bytes)
            .map(is_aac_sequence_header)
            .unwrap_or(false),
        9 => flv_tag_payload(&tag.bytes)
            .map(is_avc_sequence_header)
            .unwrap_or(false),
        _ => false,
    }
}

fn flv_tag_payload(tag: &[u8]) -> Option<&[u8]> {
    if tag.len() < FLV_TAG_HEADER_LEN + FLV_TAG_PREVIOUS_SIZE_LEN {
        return None;
    }
    let data_size = read_u24_be(&tag[1..4]) as usize;
    let payload_start = FLV_TAG_HEADER_LEN;
    let payload_end = payload_start + data_size;
    (tag.len() >= payload_end + FLV_TAG_PREVIOUS_SIZE_LEN).then_some(&tag[payload_start..payload_end])
}

fn is_aac_sequence_header(payload: &[u8]) -> bool {
    payload.len() >= 2 && (payload[0] >> 4) == 10 && payload[1] == 0
}

fn is_avc_sequence_header(payload: &[u8]) -> bool {
    payload.len() >= 2 && (payload[0] & 0x0f) == 7 && payload[1] == 0
}

fn flv_tag_fingerprint(tag: &[u8]) -> Option<FlvTagFingerprint> {
    if tag.len() < FLV_TAG_HEADER_LEN + FLV_TAG_PREVIOUS_SIZE_LEN {
        return None;
    }

    let data_size = read_u24_be(&tag[1..4]) as usize;
    let expected_len = FLV_TAG_HEADER_LEN + data_size + FLV_TAG_PREVIOUS_SIZE_LEN;
    if tag.len() != expected_len {
        return None;
    }

    let payload_start = FLV_TAG_HEADER_LEN;
    let payload_end = payload_start + data_size;
    let timestamp = ((tag[7] as u32) << 24)
        | ((tag[4] as u32) << 16)
        | ((tag[5] as u32) << 8)
        | tag[6] as u32;

    Some(FlvTagFingerprint {
        tag_type: tag[0],
        data_size: data_size as u32,
        timestamp,
        stream_id: read_u24_be(&tag[8..11]),
        payload_hash_128: stable_payload_hash_128(&tag[payload_start..payload_end]),
    })
}

fn read_u24_be(bytes: &[u8]) -> u32 {
    ((bytes[0] as u32) << 16) | ((bytes[1] as u32) << 8) | bytes[2] as u32
}

fn read_u32_be(bytes: &[u8]) -> u32 {
    ((bytes[0] as u32) << 24)
        | ((bytes[1] as u32) << 16)
        | ((bytes[2] as u32) << 8)
        | bytes[3] as u32
}

fn stable_payload_hash_128(payload: &[u8]) -> u128 {
    let forward = fnv1a64(payload.iter().copied(), 0xcbf2_9ce4_8422_2325);
    let backward = fnv1a64(payload.iter().rev().copied(), 0x8422_2325_cbf2_9ce4);
    ((forward as u128) << 64) | backward as u128
}

fn fnv1a64(bytes: impl IntoIterator<Item = u8>, seed: u64) -> u64 {
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = seed;
    for byte in bytes {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

async fn load_flv_dedupe_window_from_file(path: &Path) -> Result<FlvDedupeWindow, AppError> {
    let metadata = tokio::fs::metadata(path).await?;
    if !metadata.is_file() || metadata.len() <= FLV_HEADER_SKIP_LEN as u64 {
        return Ok(FlvDedupeWindow::default());
    }

    let read_len = metadata
        .len()
        .min(FLV_DEDUPE_MAX_PAYLOAD_BYTES as u64) as usize;
    let start_offset = metadata.len().saturating_sub(read_len as u64);
    let mut file = tokio::fs::File::open(path).await?;
    file.seek(SeekFrom::Start(start_offset)).await?;

    let mut buffer = Vec::with_capacity(read_len);
    file.read_to_end(&mut buffer).await?;

    Ok(build_flv_dedupe_window_from_tail(&buffer, start_offset))
}

fn build_flv_dedupe_window_from_tail(buffer: &[u8], absolute_offset: u64) -> FlvDedupeWindow {
    let mut cursor = buffer.len();
    let mut reversed = Vec::new();
    let mut payload_bytes = 0usize;

    while cursor >= FLV_TAG_PREVIOUS_SIZE_LEN && reversed.len() < FLV_DEDUPE_MAX_TAGS {
        if absolute_offset + (cursor as u64) <= FLV_HEADER_SKIP_LEN as u64 {
            break;
        }

        let previous_size_start = cursor - FLV_TAG_PREVIOUS_SIZE_LEN;
        let previous_size = read_u32_be(&buffer[previous_size_start..cursor]) as usize;
        if previous_size < FLV_TAG_HEADER_LEN {
            break;
        }

        let tag_len = previous_size + FLV_TAG_PREVIOUS_SIZE_LEN;
        if tag_len > cursor {
            break;
        }

        let tag_start = cursor - tag_len;
        if absolute_offset + (tag_start as u64) < FLV_HEADER_SKIP_LEN as u64 {
            break;
        }

        let Some(fingerprint) = flv_tag_fingerprint(&buffer[tag_start..cursor]) else {
            break;
        };
        payload_bytes = payload_bytes.saturating_add(fingerprint.data_size as usize);
        reversed.push(fingerprint);
        cursor = tag_start;

        if payload_bytes >= FLV_DEDUPE_MAX_PAYLOAD_BYTES {
            break;
        }
    }

    let mut window = FlvDedupeWindow::default();
    for fingerprint in reversed.into_iter().rev() {
        window.push(fingerprint);
    }
    window
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
    _initial_bytes: u64,
    initial_duration_ms: u64,
    append: bool,
    speed_tracker: &mut SpeedTracker,
) -> Result<LiveRecordOutcome, AppError> {
    if let Some(parent) = output_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let existing_len = tokio::fs::metadata(&output_path)
        .await
        .ok()
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let append_to_existing = append && existing_len >= FLV_HEADER_SKIP_LEN as u64;
    let mut total_bytes = if append_to_existing { existing_len } else { 0 };
    let mut dedupe_window = if append_to_existing {
        match load_flv_dedupe_window_from_file(&output_path).await {
            Ok(window) => window,
            Err(err) => {
                eprintln!(
                    "[live_recorder][flv] failed to bootstrap dedupe window from {}: {}",
                    output_path.display(),
                    err
                );
                FlvDedupeWindow::default()
            }
        }
    } else {
        FlvDedupeWindow::default()
    };
    let mut boundary_dedupe = if append_to_existing && !dedupe_window.is_empty() {
        Some(FlvBoundaryDedupe::new(&dedupe_window))
    } else {
        None
    };

    let mut request = client.get(&url).timeout(live_segment_timeout());
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
        .append(append_to_existing)
        .truncate(!append_to_existing)
        .open(&output_path)
        .await?;

    let mut header_remaining = FLV_HEADER_SKIP_LEN;
    let mut pending_header = Vec::with_capacity(FLV_HEADER_SKIP_LEN);
    let mut parser = FlvTagParser::default();

    let start_instant = Instant::now();
    let mut last_emit = Instant::now();
    // Note: do not push a same-byte sample here. We want the displayed speed
    // to stay frozen at the pre-disconnect value until real chunks arrive
    // again. `speed_tracker.record(total_bytes)` happens inside the chunk
    // loop below when bytes actually flow.

    emit_live_progress(
        &app_handle,
        &live_records,
        &task_id,
        LiveRecordStatus::Recording,
        total_bytes,
        speed_tracker.speed_bps(),
        initial_duration_ms,
    )
    .await;

    loop {
        let next = tokio::select! {
            item = stream.next() => item,
            _ = signal.token.cancelled() => {
                file.flush().await?;
                let duration_ms = initial_duration_ms + start_instant.elapsed().as_millis() as u64;
                update_live_record_counters(&live_records, &task_id, total_bytes, duration_ms).await;
                drop(file);
                return Err(AppError::Cancelled);
            }
        };

        let chunk = match next {
            Some(Ok(chunk)) => chunk,
            Some(Err(err)) => {
                file.flush().await?;
                let duration_ms = initial_duration_ms + start_instant.elapsed().as_millis() as u64;
                update_live_record_counters(&live_records, &task_id, total_bytes, duration_ms)
                    .await;
                // Capture bytes received during this short-lived connection
                // so rapid reconnects still accumulate samples in the window.
                speed_tracker.record(total_bytes);
                drop(file);
                return Err(AppError::Network(err.to_string()));
            }
            None => {
                file.flush().await?;
                let duration_ms = initial_duration_ms + start_instant.elapsed().as_millis() as u64;
                update_live_record_counters(&live_records, &task_id, total_bytes, duration_ms)
                    .await;
                speed_tracker.record(total_bytes);
                drop(file);
                return Err(AppError::Network(
                    "HTTP-FLV stream ended before user stopped recording".to_string(),
                ));
            }
        };

        let mut payload = &chunk[..];
        if header_remaining > 0 {
            let (header_part, rest) =
                take_flv_connection_header_prefix(payload, &mut header_remaining);
            if !append_to_existing {
                pending_header.extend_from_slice(header_part);
                if pending_header.len() == FLV_HEADER_SKIP_LEN {
                    file.write_all(&pending_header).await?;
                    total_bytes += pending_header.len() as u64;
                    pending_header.clear();
                }
            }
            payload = rest;
            if payload.is_empty() {
                continue;
            }
        }

        for tag in parser.push(payload) {
            let writable_tags = if let Some(dedupe) = boundary_dedupe.as_mut() {
                dedupe.process_tag(tag)
            } else {
                vec![tag]
            };

            for tag in writable_tags {
                file.write_all(&tag.bytes).await?;
                total_bytes += tag.bytes.len() as u64;
                dedupe_window.push(tag.fingerprint);
            }
        }

        if last_emit.elapsed() >= PROGRESS_EMIT_INTERVAL {
            speed_tracker.record(total_bytes);
            let speed = speed_tracker.speed_bps();
            last_emit = Instant::now();

            let duration_ms = initial_duration_ms + start_instant.elapsed().as_millis() as u64;

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
                if wait_or_cancel(&signal, hls_min_refresh()).await {
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

        if media.segments.last().is_some() {
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
        let refresh = refresh.clamp(hls_min_refresh(), hls_max_refresh());
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
    let mut request = client.get(url).timeout(hls_playlist_timeout());
    for (name, value) in headers.iter() {
        request = request.header(name, value);
    }
    let response = request.send().await?.error_for_status()?;
    let bytes = response.bytes().await?;
    let playlist = m3u8_rs::parse_playlist_res(&bytes)
        .map_err(|_| AppError::InvalidInput("链接内容不是有效的 M3U8 播放列表".to_string()))?;
    match playlist {
        m3u8_rs::Playlist::MediaPlaylist(media) => Ok((base_url, media)),
        m3u8_rs::Playlist::MasterPlaylist(master) => {
            let variant = master
                .variants
                .iter()
                .filter(|v| !v.is_i_frame)
                .filter(|v| !v.uri.trim().is_empty())
                .max_by_key(|v| v.bandwidth)
                .ok_or_else(|| {
                    AppError::M3u8Parse("Master playlist has no variants".to_string())
                })?;
            let next_url = resolve_url(&base_url, &variant.uri);
            Box::pin(fetch_media_playlist_inner(
                client,
                &next_url,
                headers,
                depth + 1,
            ))
            .await
        }
    }
}

async fn fetch_bytes(
    client: &reqwest::Client,
    url: &str,
    headers: &RequestHeaders,
) -> Result<bytes::Bytes, AppError> {
    let mut request = client.get(url).timeout(live_segment_timeout());
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

fn live_record_retry_interval(protocol: LiveProtocol) -> Duration {
    match protocol {
        LiveProtocol::Flv => flv_live_retry(),
        LiveProtocol::Hls => live_retry_hls(),
    }
}

async fn update_live_record_counters(
    live_records: &Arc<Mutex<HashMap<DownloadId, LiveRecordTask>>>,
    task_id: &str,
    total_bytes: u64,
    duration_ms: u64,
) {
    let mut map = live_records.lock().await;
    if let Some(task) = map.get_mut(task_id) {
        task.total_bytes = total_bytes;
        task.duration_ms = duration_ms;
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
    let updated_at = task.updated_at.unwrap_or_else(Utc::now).to_rfc3339();
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

    fn flv_file_header() -> Vec<u8> {
        b"FLV\x01\x05\0\0\0\x09\0\0\0\0".to_vec()
    }

    fn make_flv_tag(tag_type: u8, timestamp: u32, payload: &[u8]) -> Vec<u8> {
        assert!(payload.len() <= 0xFF_FF_FF);

        let data_size = payload.len();
        let mut tag = Vec::with_capacity(FLV_TAG_HEADER_LEN + data_size + 4);
        tag.push(tag_type);
        tag.extend_from_slice(&[
            ((data_size >> 16) & 0xFF) as u8,
            ((data_size >> 8) & 0xFF) as u8,
            (data_size & 0xFF) as u8,
        ]);
        tag.extend_from_slice(&[
            ((timestamp >> 16) & 0xFF) as u8,
            ((timestamp >> 8) & 0xFF) as u8,
            (timestamp & 0xFF) as u8,
            ((timestamp >> 24) & 0xFF) as u8,
        ]);
        tag.extend_from_slice(&[0, 0, 0]);
        tag.extend_from_slice(payload);
        tag.extend_from_slice(&((FLV_TAG_HEADER_LEN + data_size) as u32).to_be_bytes());
        tag
    }

    fn test_flv_tag(index: usize) -> Vec<u8> {
        make_flv_tag(
            9,
            (index as u32).saturating_mul(40),
            &[(index & 0xFF) as u8, ((index >> 8) & 0xFF) as u8],
        )
    }

    fn flv_metadata_tag() -> Vec<u8> {
        make_flv_tag(18, 0, b"metadata")
    }

    fn flv_aac_sequence_header(timestamp: u32) -> Vec<u8> {
        make_flv_tag(8, timestamp, &[0xaf, 0x00, 0x12, 0x10])
    }

    fn flv_avc_sequence_header(timestamp: u32) -> Vec<u8> {
        make_flv_tag(9, timestamp, &[0x17, 0x00, 0x00, 0x00, 0x00, 0x64, 0x00])
    }

    fn parsed_flv_tag(bytes: &[u8]) -> FlvTag {
        FlvTag {
            bytes: bytes.to_vec(),
            fingerprint: flv_tag_fingerprint(bytes).expect("valid FLV tag"),
        }
    }

    fn flv_window_from_tags(tags: &[Vec<u8>]) -> FlvDedupeWindow {
        let mut window = FlvDedupeWindow::default();
        for tag in tags {
            window.push(flv_tag_fingerprint(tag).expect("valid FLV tag"));
        }
        window
    }

    #[test]
    fn flv_tag_parser_reassembles_split_chunks() {
        let tag_a = test_flv_tag(1);
        let tag_b = test_flv_tag(2);
        let mut parser = FlvTagParser::default();

        assert!(parser.push(&tag_a[..5]).is_empty());

        let mut rest = tag_a[5..].to_vec();
        rest.extend_from_slice(&tag_b);
        let parsed = parser.push(&rest);

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].bytes, tag_a);
        assert_eq!(parsed[1].bytes, tag_b);
    }

    #[test]
    fn flv_header_skip_handles_split_reconnect_header() {
        let header = flv_file_header();
        let tag = test_flv_tag(1);
        let mut remaining = FLV_HEADER_SKIP_LEN;

        let (header_part, rest) =
            take_flv_connection_header_prefix(&header[..5], &mut remaining);
        assert_eq!(header_part, &header[..5]);
        assert!(rest.is_empty());
        assert_eq!(remaining, FLV_HEADER_SKIP_LEN - 5);

        let mut second_chunk = header[5..].to_vec();
        second_chunk.extend_from_slice(&tag);
        let (header_part, rest) =
            take_flv_connection_header_prefix(&second_chunk, &mut remaining);
        assert_eq!(header_part, &header[5..]);
        assert_eq!(rest, &tag);
        assert_eq!(remaining, 0);
    }

    #[test]
    fn flv_boundary_dedupe_skips_overlapping_reconnect_tags() {
        let tags = (1..=4).map(test_flv_tag).collect::<Vec<_>>();
        let window = flv_window_from_tags(&tags[..3]);
        let mut dedupe = FlvBoundaryDedupe::new(&window);

        assert!(dedupe.process_tag(parsed_flv_tag(&tags[1])).is_empty());
        assert!(dedupe.process_tag(parsed_flv_tag(&tags[2])).is_empty());
        let written = dedupe.process_tag(parsed_flv_tag(&tags[3]));

        assert_eq!(written.len(), 1);
        assert_eq!(written[0].bytes, tags[3]);
    }

    #[test]
    fn flv_boundary_dedupe_skips_douyu_sized_reconnect_overlap() {
        let duplicate_count = 314;
        let tags = (0..=duplicate_count)
            .map(test_flv_tag)
            .collect::<Vec<_>>();
        let window = flv_window_from_tags(&tags[..duplicate_count]);
        let mut dedupe = FlvBoundaryDedupe::new(&window);

        for tag in tags.iter().take(duplicate_count) {
            assert!(dedupe.process_tag(parsed_flv_tag(tag)).is_empty());
        }
        let written = dedupe.process_tag(parsed_flv_tag(&tags[duplicate_count]));

        assert_eq!(written.len(), 1);
        assert_eq!(written[0].bytes, tags[duplicate_count]);
    }

    #[test]
    fn flv_boundary_dedupe_skips_reconnect_preamble_before_duplicate_media() {
        let duplicate_tags = (10..20).map(test_flv_tag).collect::<Vec<_>>();
        let next_tag = test_flv_tag(20);
        let preamble = [
            flv_metadata_tag(),
            flv_aac_sequence_header(400),
            flv_avc_sequence_header(400),
        ];
        let window = flv_window_from_tags(&duplicate_tags);
        let mut dedupe = FlvBoundaryDedupe::new(&window);

        for tag in &preamble {
            assert!(dedupe.process_tag(parsed_flv_tag(tag)).is_empty());
        }
        for tag in &duplicate_tags {
            assert!(dedupe.process_tag(parsed_flv_tag(tag)).is_empty());
        }
        let written = dedupe.process_tag(parsed_flv_tag(&next_tag));

        assert_eq!(written.len(), 1);
        assert_eq!(written[0].bytes, next_tag);
    }

    #[test]
    fn flv_boundary_dedupe_writes_preamble_when_media_does_not_overlap() {
        let history_tags = (1..4).map(test_flv_tag).collect::<Vec<_>>();
        let new_tag = test_flv_tag(10);
        let preamble = [
            flv_metadata_tag(),
            flv_aac_sequence_header(400),
            flv_avc_sequence_header(400),
        ];
        let window = flv_window_from_tags(&history_tags);
        let mut dedupe = FlvBoundaryDedupe::new(&window);

        for tag in &preamble {
            assert!(dedupe.process_tag(parsed_flv_tag(tag)).is_empty());
        }
        let written = dedupe.process_tag(parsed_flv_tag(&new_tag));

        assert_eq!(written.len(), preamble.len() + 1);
        for (actual, expected) in written.iter().zip(preamble.iter()) {
            assert_eq!(&actual.bytes, expected);
        }
        assert_eq!(written.last().expect("new tag").bytes, new_tag);
    }

    #[test]
    fn flv_boundary_dedupe_writes_all_tags_without_overlap() {
        let tags = (1..=4).map(test_flv_tag).collect::<Vec<_>>();
        let window = flv_window_from_tags(&tags[..3]);
        let mut dedupe = FlvBoundaryDedupe::new(&window);

        let written = dedupe.process_tag(parsed_flv_tag(&tags[3]));

        assert_eq!(written.len(), 1);
        assert_eq!(written[0].bytes, tags[3]);
    }

    #[test]
    fn flv_dedupe_window_keeps_at_most_configured_tag_count() {
        let mut window = FlvDedupeWindow::default();
        let tags = (0..FLV_DEDUPE_MAX_TAGS + 2)
            .map(test_flv_tag)
            .collect::<Vec<_>>();

        for tag in &tags {
            window.push(flv_tag_fingerprint(tag).expect("valid FLV tag"));
        }

        assert_eq!(window.tags.len(), FLV_DEDUPE_MAX_TAGS);
        assert_eq!(
            window.tags.front().copied(),
            flv_tag_fingerprint(&tags[2])
        );
        assert_eq!(
            window.tags.back().copied(),
            flv_tag_fingerprint(tags.last().expect("last tag"))
        );
    }

    #[test]
    fn flv_tail_bootstrap_keeps_last_configured_tags() {
        let tags = (0..FLV_DEDUPE_MAX_TAGS + 2)
            .map(test_flv_tag)
            .collect::<Vec<_>>();
        let mut bytes = flv_file_header();
        for tag in &tags {
            bytes.extend_from_slice(tag);
        }

        let window = build_flv_dedupe_window_from_tail(&bytes, 0);

        assert_eq!(window.tags.len(), FLV_DEDUPE_MAX_TAGS);
        assert_eq!(
            window.tags.front().copied(),
            flv_tag_fingerprint(&tags[2])
        );
        assert_eq!(
            window.tags.back().copied(),
            flv_tag_fingerprint(tags.last().expect("last tag"))
        );
    }

    #[test]
    fn live_record_retry_interval_is_shorter_for_flv() {
        assert_eq!(
            live_record_retry_interval(LiveProtocol::Flv),
            flv_live_retry()
        );
        assert_eq!(
            live_record_retry_interval(LiveProtocol::Hls),
            live_retry_hls()
        );
    }

    #[test]
    fn speed_tracker_returns_zero_with_single_sample() {
        let mut tracker = SpeedTracker::new(Duration::from_secs(3));
        tracker.record(1_000);
        assert_eq!(tracker.speed_bps(), 0);
    }

    #[test]
    fn speed_tracker_clamps_monotonic_regression() {
        let mut tracker = SpeedTracker::new(Duration::from_secs(3));
        tracker.record(1_000);
        std::thread::sleep(Duration::from_millis(20));
        // Regress; tracker should clamp to the previous max so speed never
        // becomes negative / underflows.
        tracker.record(500);
        std::thread::sleep(Duration::from_millis(20));
        tracker.record(2_000);
        assert!(tracker.speed_bps() > 0);
    }

    #[test]
    fn speed_tracker_computes_average_over_window() {
        let mut tracker = SpeedTracker::new(Duration::from_secs(3));
        tracker.record(0);
        std::thread::sleep(Duration::from_millis(500));
        tracker.record(500_000);
        let speed = tracker.speed_bps();
        // Expect roughly 1_000_000 B/s; allow wide tolerance for CI jitter.
        assert!(
            speed > 500_000 && speed < 2_000_000,
            "speed out of expected range: {speed}"
        );
    }

    #[test]
    fn speed_tracker_holds_value_when_no_new_samples() {
        let mut tracker = SpeedTracker::new(Duration::from_millis(300));
        tracker.record(0);
        std::thread::sleep(Duration::from_millis(50));
        tracker.record(500_000);
        let frozen = tracker.speed_bps();
        assert!(frozen > 0);
        // Simulate the reconnect-wait period: no new samples pushed. The
        // tracker should keep returning the same speed, not decay to 0.
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(tracker.speed_bps(), frozen);
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
