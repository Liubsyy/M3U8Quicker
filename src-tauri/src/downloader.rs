use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aes::{Aes128, Aes192, Aes256};
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockDecryptMut, KeyIvInit};
use chrono::Utc;
use futures::StreamExt;
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, Notify, OwnedSemaphorePermit, RwLock, Semaphore, TryAcquireError};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::error::AppError;
use crate::models::*;
use crate::persistence;
use crate::playback;

type Aes128CbcDec = cbc::Decryptor<Aes128>;
type Aes192CbcDec = cbc::Decryptor<Aes192>;
type Aes256CbcDec = cbc::Decryptor<Aes256>;

const M3U8_METADATA_TIMEOUT: Duration = Duration::from_secs(5);
const VIDEO_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);

pub enum DownloadRunOutcome {
    Completed(PathBuf),
    Incomplete,
}

enum SegmentDownloadOutcome {
    Downloaded(u64),
    Skipped,
}

#[derive(Debug, Clone)]
struct RuntimeProgressSnapshot {
    id: DownloadId,
    status: DownloadStatus,
    completed_segments: usize,
    total_segments: usize,
    completed_segment_indices: Vec<usize>,
    failed_segment_indices: Vec<usize>,
    total_bytes: u64,
    speed_bytes_per_sec: u64,
    updated_at: String,
}

#[derive(Debug)]
struct PersistThrottleState {
    last_saved_at: Instant,
    last_failed_segment_count: usize,
}

#[derive(Debug)]
struct DownloadRateLimitState {
    limit_kbps: u64,
    next_available_at: Instant,
}

#[derive(Debug)]
pub struct DownloadRateLimiter {
    state: Mutex<DownloadRateLimitState>,
    notify: Notify,
}

impl DownloadRateLimiter {
    pub fn new(limit_kbps: u64) -> Self {
        Self {
            state: Mutex::new(DownloadRateLimitState {
                limit_kbps,
                next_available_at: Instant::now(),
            }),
            notify: Notify::new(),
        }
    }

    pub async fn set_limit_kbps(&self, limit_kbps: u64) {
        let mut state = self.state.lock().await;
        state.limit_kbps = limit_kbps;
        state.next_available_at = Instant::now();
        self.notify.notify_waiters();
    }

    pub async fn limit_kbps(&self) -> u64 {
        self.state.lock().await.limit_kbps
    }

    pub async fn wait_for_bytes(
        &self,
        byte_count: usize,
        cancel: &CancellationToken,
    ) -> Result<(), AppError> {
        loop {
            let notified = self.notify.notified();
            let wait_duration = {
                let mut state = self.state.lock().await;
                reserve_rate_limit_delay(&mut state, byte_count, Instant::now())
            };

            if wait_duration.is_zero() {
                return Ok(());
            }

            tokio::select! {
                _ = cancel.cancelled() => return Err(AppError::Cancelled),
                _ = tokio::time::sleep(wait_duration) => return Ok(()),
                _ = notified => {}
            }
        }
    }
}

fn reserve_rate_limit_delay(
    state: &mut DownloadRateLimitState,
    byte_count: usize,
    now: Instant,
) -> Duration {
    if state.limit_kbps == 0 || byte_count == 0 {
        state.next_available_at = now;
        return Duration::ZERO;
    }

    let bytes_per_second = state.limit_kbps.saturating_mul(1024);
    if bytes_per_second == 0 {
        state.next_available_at = now;
        return Duration::ZERO;
    }

    let transfer_nanos =
        (byte_count as u128).saturating_mul(1_000_000_000u128) / bytes_per_second as u128;
    let transfer_duration = Duration::from_nanos(transfer_nanos.min(u64::MAX as u128) as u64);
    let start_at = state.next_available_at.max(now);
    let ready_at = start_at + transfer_duration;
    state.next_available_at = ready_at;
    ready_at.saturating_duration_since(now)
}

pub fn build_http_client(proxy_url: Option<&str>) -> Result<reqwest::Client, AppError> {
    let mut builder = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) M3U8Quicker/0.1")
        .timeout(VIDEO_DOWNLOAD_TIMEOUT);

    if let Some(url) = proxy_url.filter(|value| !value.trim().is_empty()) {
        let proxy = reqwest::Proxy::all(url.trim())
            .map_err(|e| AppError::InvalidInput(format!("代理地址无效: {}", e)))?;
        builder = builder.proxy(proxy);
    } else {
        // Force direct connections when the app-level proxy is disabled.
        // This prevents reqwest from inheriting OS/system proxy settings.
        builder = builder.no_proxy();
    }

    builder
        .build()
        .map_err(|e| AppError::Internal(format!("Failed to create HTTP client: {}", e)))
}

fn build_request(client: &reqwest::Client, url: &str) -> reqwest::RequestBuilder {
    client.get(url)
}

fn build_request_with_headers(
    client: &reqwest::Client,
    url: &str,
    headers: &RequestHeaders,
) -> reqwest::RequestBuilder {
    let mut request = build_request(client, url);

    for (name, value) in headers {
        request = request.header(name, value);
    }

    request
}

// --- M3U8 Parsing ---

pub fn resolve_m3u8<'a>(
    client: &'a reqwest::Client,
    m3u8_url: &'a str,
    headers: &'a RequestHeaders,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Vec<SegmentInfo>, AppError>> + Send + 'a>,
> {
    let url = m3u8_url.to_string();
    Box::pin(async move {
        let base_url = Url::parse(&url)?;
        let response = build_request_with_headers(client, &url, headers)
            .timeout(M3U8_METADATA_TIMEOUT)
            .send()
            .await?
            .error_for_status()?;
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_string());
        let bytes = response.bytes().await?;

        if looks_like_html_response(&bytes, content_type.as_deref()) {
            return Err(AppError::InvalidInput(
                "链接内容不是有效的 M3U8 播放列表，请检查地址是否正确".to_string(),
            ));
        }

        match m3u8_rs::parse_playlist_res(&bytes) {
            Ok(m3u8_rs::Playlist::MediaPlaylist(pl)) => {
                let media_sequence = pl.media_sequence;
                let mut current_key: Option<(String, String, Option<String>)> = None;
                let mut segments = Vec::new();

                for (i, seg) in pl.segments.iter().enumerate() {
                    // Track key inheritance
                    if let Some(ref key) = seg.key {
                        match key.method {
                            m3u8_rs::KeyMethod::AES128 => {
                                let key_uri = key.uri.as_ref().ok_or_else(|| {
                                    AppError::M3u8Parse("AES-128 key missing URI".into())
                                })?;
                                let resolved_uri = resolve_url(&base_url, key_uri);
                                current_key =
                                    Some(("AES-128".to_string(), resolved_uri, key.iv.clone()));
                            }
                            m3u8_rs::KeyMethod::None => {
                                current_key = None;
                            }
                            _ => {
                                return Err(AppError::M3u8Parse(format!(
                                    "Unsupported encryption method: {:?}",
                                    key.method
                                )));
                            }
                        }
                    }

                    let encryption = current_key
                        .as_ref()
                        .map(|(method, uri, iv)| EncryptionInfo {
                            method: method.clone(),
                            key_uri: uri.clone(),
                            iv: iv.clone(),
                            key_bytes: Vec::new(),
                        });

                    segments.push(SegmentInfo {
                        index: i,
                        uri: resolve_url(&base_url, &seg.uri),
                        duration: seg.duration,
                        sequence_number: media_sequence + i as u64,
                        encryption,
                    });
                }
                Ok(segments)
            }
            Ok(m3u8_rs::Playlist::MasterPlaylist(pl)) => {
                let variant = pl
                    .variants
                    .iter()
                    .max_by_key(|v| v.bandwidth)
                    .ok_or_else(|| AppError::M3u8Parse("No variants found".into()))?;
                let variant_url = resolve_url(&base_url, &variant.uri);
                resolve_m3u8(client, &variant_url, headers).await
            }
            Err(_) => Err(AppError::InvalidInput(
                "链接内容不是有效的 M3U8 播放列表，请检查地址是否正确".to_string(),
            )),
        }
    })
}

fn looks_like_html_response(bytes: &[u8], content_type: Option<&str>) -> bool {
    if let Some(content_type) = content_type {
        let lower = content_type.to_ascii_lowercase();
        if lower.contains("text/html") || lower.contains("application/xhtml+xml") {
            return true;
        }
    }

    let prefix = String::from_utf8_lossy(&bytes[..bytes.len().min(256)])
        .trim_start()
        .to_ascii_lowercase();

    prefix.starts_with("<!doctype html")
        || prefix.starts_with("<html")
        || prefix.starts_with("<head")
        || prefix.starts_with("<body")
        || prefix.starts_with("<script")
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

// --- Encryption Key Fetching ---

pub async fn fetch_encryption_keys(
    client: &reqwest::Client,
    segments: &mut [SegmentInfo],
    headers: &RequestHeaders,
) -> Result<(), AppError> {
    let mut key_cache: HashMap<String, Vec<u8>> = HashMap::new();

    for seg in segments.iter_mut() {
        if let Some(ref mut enc) = seg.encryption {
            if !key_cache.contains_key(&enc.key_uri) {
                let resp = build_request_with_headers(client, &enc.key_uri, headers)
                    .timeout(M3U8_METADATA_TIMEOUT)
                    .send()
                    .await?
                    .error_for_status()?;
                let bytes = resp.bytes().await?;
                if !matches!(bytes.len(), 16 | 24 | 32) {
                    return Err(AppError::Decryption(format!(
                        "AES key must be 16, 24, or 32 bytes, got {}",
                        bytes.len()
                    )));
                }
                key_cache.insert(enc.key_uri.clone(), bytes.to_vec());
            }
            enc.key_bytes = key_cache[&enc.key_uri].clone();
            enc.method = match enc.key_bytes.len() {
                16 => "AES-128",
                24 => "AES-192",
                32 => "AES-256",
                _ => enc.method.as_str(),
            }
            .to_string();
        }
    }
    Ok(())
}

// --- AES-CBC Decryption ---

fn decrypt_aes128(data: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Result<Vec<u8>, AppError> {
    let mut buf = data.to_vec();
    let decrypted = Aes128CbcDec::new(key.into(), iv.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|e| AppError::Decryption(format!("AES decryption failed: {}", e)))?;
    Ok(decrypted.to_vec())
}

fn decrypt_aes192(data: &[u8], key: &[u8; 24], iv: &[u8; 16]) -> Result<Vec<u8>, AppError> {
    let mut buf = data.to_vec();
    let decrypted = Aes192CbcDec::new(key.into(), iv.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|e| AppError::Decryption(format!("AES decryption failed: {}", e)))?;
    Ok(decrypted.to_vec())
}

fn decrypt_aes256(data: &[u8], key: &[u8; 32], iv: &[u8; 16]) -> Result<Vec<u8>, AppError> {
    let mut buf = data.to_vec();
    let decrypted = Aes256CbcDec::new(key.into(), iv.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|e| AppError::Decryption(format!("AES decryption failed: {}", e)))?;
    Ok(decrypted.to_vec())
}

fn decrypt_aes_cbc(data: &[u8], key: &[u8], iv: &[u8; 16]) -> Result<Vec<u8>, AppError> {
    match key.len() {
        16 => {
            let key: [u8; 16] = key
                .try_into()
                .map_err(|_| AppError::Decryption("Invalid AES-128 key length".into()))?;
            decrypt_aes128(data, &key, iv)
        }
        24 => {
            let key: [u8; 24] = key
                .try_into()
                .map_err(|_| AppError::Decryption("Invalid AES-192 key length".into()))?;
            decrypt_aes192(data, &key, iv)
        }
        32 => {
            let key: [u8; 32] = key
                .try_into()
                .map_err(|_| AppError::Decryption("Invalid AES-256 key length".into()))?;
            decrypt_aes256(data, &key, iv)
        }
        other => Err(AppError::Decryption(format!(
            "Unsupported AES key length: {}",
            other
        ))),
    }
}

fn compute_iv(enc: &EncryptionInfo, sequence_number: u64) -> [u8; 16] {
    if let Some(ref iv_hex) = enc.iv {
        let hex_str = iv_hex.trim_start_matches("0x").trim_start_matches("0X");
        if let Ok(bytes) = hex::decode(hex_str) {
            let mut iv = [0u8; 16];
            let start = 16usize.saturating_sub(bytes.len());
            let len = bytes.len().min(16);
            iv[start..start + len].copy_from_slice(&bytes[..len]);
            return iv;
        }
    }
    // Default: use sequence number as big-endian 128-bit integer
    let mut iv = [0u8; 16];
    iv[8..16].copy_from_slice(&sequence_number.to_be_bytes());
    iv
}

// --- Download Engine ---

pub fn temp_dir_for_task(output_dir: &Path, task_id: &str) -> PathBuf {
    output_dir.join(format!("m3u8quicker_temp_{}", &task_id[..8]))
}

pub fn segment_file_path(temp_dir: &Path, segment_index: usize) -> PathBuf {
    temp_dir.join(format!("seg_{:06}.ts", segment_index))
}

fn partial_segment_file_path(temp_dir: &Path, segment_index: usize) -> PathBuf {
    temp_dir.join(format!("seg_{:06}.ts.part", segment_index))
}

fn split_filename_and_extension(filename: &str) -> (String, Option<String>) {
    let path = Path::new(filename);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or(filename)
        .to_string();
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string());

    (stem, extension)
}

fn build_filename(base_name: &str, extension: Option<&str>) -> String {
    match extension {
        Some(extension) => format!("{}.{}", base_name, extension),
        None => base_name.to_string(),
    }
}

fn build_indexed_filename(base_name: &str, index: usize, extension: Option<&str>) -> String {
    match extension {
        Some(extension) => format!("{} ({}).{}", base_name, index, extension),
        None => format!("{} ({})", base_name, index),
    }
}

fn ensure_extension(filename: &str, extension: &str) -> String {
    let expected_suffix = format!(".{}", extension);
    if filename.to_ascii_lowercase().ends_with(&expected_suffix) {
        filename.to_string()
    } else {
        format!("{}.{}", filename, extension)
    }
}

fn resolve_available_output_path(output_dir: &Path, filename: &str) -> PathBuf {
    let (base_name, extension) = split_filename_and_extension(filename);
    let initial = output_dir.join(build_filename(&base_name, extension.as_deref()));
    if !initial.exists() {
        return initial;
    }

    let mut index = 1usize;
    loop {
        let candidate = output_dir.join(build_indexed_filename(
            &base_name,
            index,
            extension.as_deref(),
        ));
        if !candidate.exists() {
            return candidate;
        }
        index += 1;
    }
}

pub fn resolve_available_file_path(output_path: &Path) -> PathBuf {
    if !output_path.exists() {
        return output_path.to_path_buf();
    }

    let Some(file_name) = output_path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
    else {
        return output_path.to_path_buf();
    };

    let (base_name, extension) = split_filename_and_extension(file_name);
    let parent = output_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new(""));

    let mut index = 1usize;
    loop {
        let candidate = parent.join(build_indexed_filename(
            &base_name,
            index,
            extension.as_deref(),
        ));
        if !candidate.exists() {
            return candidate;
        }
        index += 1;
    }
}

fn calculate_percentage(completed_segments: usize, total_segments: usize) -> f64 {
    if total_segments == 0 {
        0.0
    } else {
        (completed_segments as f64 / total_segments as f64) * 100.0
    }
}

fn snapshot_to_event(snapshot: &RuntimeProgressSnapshot) -> DownloadProgressEvent {
    DownloadProgressEvent {
        id: snapshot.id.clone(),
        status: snapshot.status.clone(),
        group: download_group_for_status(&snapshot.status),
        completed_segments: snapshot.completed_segments,
        total_segments: snapshot.total_segments,
        failed_segment_count: snapshot.failed_segment_indices.len(),
        total_bytes: snapshot.total_bytes,
        speed_bytes_per_sec: snapshot.speed_bytes_per_sec,
        percentage: calculate_percentage(snapshot.completed_segments, snapshot.total_segments),
        updated_at: snapshot.updated_at.clone(),
    }
}

async fn sync_task_progress(
    downloads: &Arc<Mutex<HashMap<DownloadId, DownloadTask>>>,
    snapshot: &mut RuntimeProgressSnapshot,
) {
    let mut tasks = downloads.lock().await;
    if let Some(task) = tasks.get_mut(&snapshot.id) {
        if matches!(snapshot.status, DownloadStatus::Downloading)
            && matches!(
                task.status,
                DownloadStatus::Paused | DownloadStatus::Cancelled
            )
        {
            snapshot.status = task.status.clone();
            snapshot.speed_bytes_per_sec = 0;
            task.completed_segments = snapshot.completed_segments;
            task.total_segments = snapshot.total_segments;
            task.completed_segment_indices = snapshot.completed_segment_indices.clone();
            task.failed_segment_indices = snapshot.failed_segment_indices.clone();
            task.total_bytes = snapshot.total_bytes;
            task.speed_bytes_per_sec = 0;
            return;
        }

        task.status = snapshot.status.clone();
        task.completed_segments = snapshot.completed_segments;
        task.total_segments = snapshot.total_segments;
        task.completed_segment_indices = snapshot.completed_segment_indices.clone();
        task.failed_segment_indices = snapshot.failed_segment_indices.clone();
        task.total_bytes = snapshot.total_bytes;
        task.speed_bytes_per_sec = snapshot.speed_bytes_per_sec;
        task.touch();
    }
}

async fn emit_progress(
    app_handle: &AppHandle,
    downloads: &Arc<Mutex<HashMap<DownloadId, DownloadTask>>>,
    mut snapshot: RuntimeProgressSnapshot,
) {
    sync_task_progress(downloads, &mut snapshot).await;
    let _ = app_handle.emit("download-progress", &snapshot_to_event(&snapshot));
}

async fn maybe_persist_task_progress(
    app_handle: &AppHandle,
    downloads: &Arc<Mutex<HashMap<DownloadId, DownloadTask>>>,
    task_id: &str,
    throttle: &Arc<Mutex<PersistThrottleState>>,
    force: bool,
) {
    let failed_segment_count = {
        let tasks = downloads.lock().await;
        tasks.get(task_id)
            .map(|task| task.failed_segment_indices.len())
            .unwrap_or_default()
    };

    let should_save = {
        let mut throttle = throttle.lock().await;
        if force
            || failed_segment_count != throttle.last_failed_segment_count
            || throttle.last_saved_at.elapsed() >= Duration::from_secs(5)
        {
            throttle.last_saved_at = Instant::now();
            throttle.last_failed_segment_count = failed_segment_count;
            true
        } else {
            false
        }
    };

    if !should_save {
        return;
    }

    let task = {
        let tasks = downloads.lock().await;
        tasks.get(task_id).cloned()
    };

    if let Some(task) = task {
        let _ = persistence::save_task(app_handle, &task).await;
    }
}

async fn snapshot_segments(segment_indices: &Arc<Mutex<BTreeSet<usize>>>) -> Vec<usize> {
    segment_indices
        .lock()
        .await
        .iter()
        .copied()
        .collect()
}

async fn restore_download_state(
    temp_dir: &Path,
    segments: &[SegmentInfo],
    recorded_completed_segment_indices: &[usize],
    recorded_failed_segment_indices: &[usize],
) -> Result<(BTreeSet<usize>, BTreeSet<usize>, u64), AppError> {
    let mut completed_segment_indices = BTreeSet::new();
    let mut failed_segment_indices = recorded_failed_segment_indices
        .iter()
        .copied()
        .filter(|value| *value > 0 && *value <= segments.len())
        .collect::<BTreeSet<_>>();
    let mut total_bytes = 0u64;
    let recorded: BTreeSet<usize> = recorded_completed_segment_indices.iter().copied().collect();

    for segment in segments {
        let completed_path = segment_file_path(temp_dir, segment.index);
        let partial_path = partial_segment_file_path(temp_dir, segment.index);
        if partial_path.exists() {
            let _ = tokio::fs::remove_file(&partial_path).await;
        }

        if completed_path.exists() {
            if !recorded.is_empty() && !recorded.contains(&(segment.index + 1)) {
                // Trust on-disk completed segments even if the persisted record is stale.
            }
            total_bytes += tokio::fs::metadata(&completed_path).await?.len();
            completed_segment_indices.insert(segment.index + 1);
            failed_segment_indices.remove(&(segment.index + 1));
        }
    }

    Ok((completed_segment_indices, failed_segment_indices, total_bytes))
}

async fn current_task_status(
    downloads: &Arc<Mutex<HashMap<DownloadId, DownloadTask>>>,
    task_id: &str,
) -> Option<DownloadStatus> {
    let tasks = downloads.lock().await;
    tasks.get(task_id).map(|task| task.status.clone())
}

pub async fn cleanup_temp_dir(output_dir: &Path, task_id: &str) -> Result<(), AppError> {
    let temp_dir = temp_dir_for_task(output_dir, task_id);
    if temp_dir.exists() {
        tokio::fs::remove_dir_all(temp_dir).await?;
    }
    Ok(())
}

pub async fn run_download(
    app_handle: AppHandle,
    downloads: Arc<Mutex<HashMap<DownloadId, DownloadTask>>>,
    client: Arc<RwLock<reqwest::Client>>,
    rate_limiter: Arc<DownloadRateLimiter>,
    task_id: DownloadId,
    segments: Vec<SegmentInfo>,
    headers: Arc<RequestHeaders>,
    output_dir: PathBuf,
    filename: String,
    delete_ts_temp_dir_after_download: bool,
    playback_sessions: Arc<Mutex<HashMap<DownloadId, playback::PlaybackSession>>>,
    download_priorities: Arc<Mutex<HashMap<DownloadId, Arc<playback::DownloadPriorityState>>>>,
    convert_to_mp4: bool,
    cancel_token: CancellationToken,
    max_concurrent: Arc<Mutex<usize>>,
) -> Result<DownloadRunOutcome, AppError> {
    let total = segments.len();
    let temp_dir = temp_dir_for_task(&output_dir, &task_id);
    tokio::fs::create_dir_all(&temp_dir).await?;

    let (existing_completed_segment_indices, existing_failed_segment_indices) = {
        let tasks = downloads.lock().await;
        tasks
            .get(&task_id)
            .map(|task| {
                (
                    task.completed_segment_indices.clone(),
                    task.failed_segment_indices.clone(),
                )
            })
            .unwrap_or_default()
    };
    let (
        restored_completed_segment_indices,
        restored_failed_segment_indices,
        restored_total_bytes,
    ) = restore_download_state(
        &temp_dir,
        &segments,
        &existing_completed_segment_indices,
        &existing_failed_segment_indices,
    )
    .await?;

    let semaphore = Arc::new(Semaphore::new(MAX_DOWNLOAD_CONCURRENCY));
    let completed = Arc::new(AtomicUsize::new(restored_completed_segment_indices.len()));
    let total_bytes = Arc::new(AtomicU64::new(restored_total_bytes));
    let speed_bytes_per_sec = Arc::new(AtomicU64::new(0));
    let completed_segment_indices = Arc::new(Mutex::new(restored_completed_segment_indices));
    let failed_segment_indices = Arc::new(Mutex::new(restored_failed_segment_indices));
    let speed_report_cancel = CancellationToken::new();
    let concurrency_limit_cancel = CancellationToken::new();
    let persist_throttle = Arc::new(Mutex::new(PersistThrottleState {
        last_saved_at: Instant::now(),
        last_failed_segment_count: existing_failed_segment_indices.len(),
    }));
    let initial_concurrency = normalize_download_concurrency(*max_concurrent.lock().await);
    let mut held_permits = Vec::with_capacity(MAX_DOWNLOAD_CONCURRENCY - initial_concurrency);
    rebalance_concurrency_permits(&semaphore, &mut held_permits, initial_concurrency)?;

    emit_progress(
        &app_handle,
        &downloads,
        RuntimeProgressSnapshot {
            id: task_id.clone(),
            status: DownloadStatus::Downloading,
            completed_segments: completed.load(Ordering::Relaxed),
            total_segments: total,
            completed_segment_indices: snapshot_segments(&completed_segment_indices).await,
            failed_segment_indices: snapshot_segments(&failed_segment_indices).await,
            total_bytes: total_bytes.load(Ordering::Relaxed),
            speed_bytes_per_sec: 0,
            updated_at: Utc::now().to_rfc3339(),
        },
    )
    .await;

    let speed_reporter = {
        let app_handle = app_handle.clone();
        let downloads = downloads.clone();
        let task_id = task_id.clone();
        let completed = completed.clone();
        let total_bytes = total_bytes.clone();
        let speed_bytes_per_sec = speed_bytes_per_sec.clone();
        let completed_segment_indices = completed_segment_indices.clone();
        let failed_segment_indices = failed_segment_indices.clone();
        let speed_report_cancel = speed_report_cancel.clone();
        let restored_total_bytes = restored_total_bytes;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut last_bytes = restored_total_bytes;

            interval.tick().await;

            loop {
                tokio::select! {
                    _ = speed_report_cancel.cancelled() => break,
                    _ = interval.tick() => {
                        let downloaded_bytes = total_bytes.load(Ordering::Relaxed);
                        let speed = downloaded_bytes.saturating_sub(last_bytes);
                        last_bytes = downloaded_bytes;
                        speed_bytes_per_sec.store(speed, Ordering::Relaxed);

                        let done = completed.load(Ordering::Relaxed);
                        let completed_segments_list = snapshot_segments(&completed_segment_indices).await;
                        let failed_segments_list = snapshot_segments(&failed_segment_indices).await;
                        emit_progress(
                            &app_handle,
                            &downloads,
                            RuntimeProgressSnapshot {
                                id: task_id.clone(),
                                status: DownloadStatus::Downloading,
                                completed_segments: done,
                                total_segments: total,
                                completed_segment_indices: completed_segments_list,
                                failed_segment_indices: failed_segments_list,
                                total_bytes: downloaded_bytes,
                                speed_bytes_per_sec: speed,
                                updated_at: Utc::now().to_rfc3339(),
                            },
                        )
                        .await;
                    }
                }
            }
        })
    };

    let concurrency_limiter = {
        let semaphore = semaphore.clone();
        let max_concurrent = max_concurrent.clone();
        let concurrency_limit_cancel = concurrency_limit_cancel.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(250));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut held_permits = held_permits;

            interval.tick().await;

            loop {
                tokio::select! {
                    _ = concurrency_limit_cancel.cancelled() => break,
                    _ = interval.tick() => {
                        let target_concurrency =
                            normalize_download_concurrency(*max_concurrent.lock().await);
                        if rebalance_concurrency_permits(
                            &semaphore,
                            &mut held_permits,
                            target_concurrency,
                        )
                        .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        })
    };

    let restored_completed_segments = snapshot_segments(&completed_segment_indices).await;
    let restored_failed_segments = snapshot_segments(&failed_segment_indices).await;
    let priority_state = playback::prepare_download_priority_state(
        &download_priorities,
        &task_id,
        total,
        &restored_completed_segments,
        &restored_failed_segments,
    )
    .await;
    let segments = Arc::new(segments);
    let worker_count = MAX_DOWNLOAD_CONCURRENCY.min(total.max(1));
    let mut worker_handles = Vec::with_capacity(worker_count);

    for _ in 0..worker_count {
        worker_handles.push(tokio::spawn(download_worker_loop(
            semaphore.clone(),
            priority_state.clone(),
            client.clone(),
            rate_limiter.clone(),
            headers.clone(),
            temp_dir.clone(),
            segments.clone(),
            completed.clone(),
            total_bytes.clone(),
            speed_bytes_per_sec.clone(),
            completed_segment_indices.clone(),
            failed_segment_indices.clone(),
            downloads.clone(),
            app_handle.clone(),
            task_id.clone(),
            cancel_token.clone(),
            total,
            persist_throttle.clone(),
        )));
    }

    let mut first_error = None;
    for handle in worker_handles {
        match handle.await {
            Ok(Ok(())) | Ok(Err(AppError::Cancelled)) => {}
            Ok(Err(error)) => {
                if first_error.is_none() {
                    cancel_token.cancel();
                    first_error = Some(error);
                }
            }
            Err(error) => {
                if first_error.is_none() {
                    cancel_token.cancel();
                    first_error = Some(AppError::Internal(format!(
                        "Download worker task join error: {}",
                        error
                    )));
                }
            }
        }
    }

    speed_report_cancel.cancel();
    concurrency_limit_cancel.cancel();
    let _ = speed_reporter.await;
    let _ = concurrency_limiter.await;

    if let Some(error) = first_error {
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
        return Err(error);
    }

    if cancel_token.is_cancelled() {
        let status = current_task_status(&downloads, &task_id).await;
        if !matches!(status, Some(DownloadStatus::Paused)) {
            let _ = tokio::fs::remove_dir_all(&temp_dir).await;
        }
        return Err(AppError::Cancelled);
    }

    let completed_segments_list = snapshot_segments(&completed_segment_indices).await;
    let failed_segments_list = snapshot_segments(&failed_segment_indices).await;
    if !failed_segments_list.is_empty() {
        speed_bytes_per_sec.store(0, Ordering::Relaxed);
        emit_progress(
            &app_handle,
            &downloads,
            RuntimeProgressSnapshot {
                id: task_id.clone(),
                status: DownloadStatus::Downloading,
                completed_segments: completed.load(Ordering::Relaxed),
                total_segments: total,
                completed_segment_indices: completed_segments_list,
                failed_segment_indices: failed_segments_list,
                total_bytes: total_bytes.load(Ordering::Relaxed),
                speed_bytes_per_sec: 0,
                updated_at: Utc::now().to_rfc3339(),
            },
        )
        .await;
        return Ok(DownloadRunOutcome::Incomplete);
    }

    // Emit merging status
    let downloaded_bytes = total_bytes.load(Ordering::Relaxed);
    speed_bytes_per_sec.store(0, Ordering::Relaxed);
    emit_progress(
        &app_handle,
        &downloads,
        RuntimeProgressSnapshot {
            id: task_id.clone(),
            status: DownloadStatus::Merging,
            completed_segments: total,
            total_segments: total,
            completed_segment_indices: completed_segments_list.clone(),
            failed_segment_indices: Vec::new(),
            total_bytes: downloaded_bytes,
            speed_bytes_per_sec: 0,
            updated_at: Utc::now().to_rfc3339(),
        },
    )
    .await;

    // Merge segments into .ts file
    let ts_filename = ensure_extension(&filename, "ts");
    let ts_path = resolve_available_output_path(&output_dir, &ts_filename);
    merge_segments(&temp_dir, total, &ts_path).await?;

    let final_path = if convert_to_mp4 {
        emit_progress(
            &app_handle,
            &downloads,
            RuntimeProgressSnapshot {
                id: task_id.clone(),
                status: DownloadStatus::Converting,
                completed_segments: total,
                total_segments: total,
                completed_segment_indices: completed_segments_list,
                failed_segment_indices: Vec::new(),
                total_bytes: downloaded_bytes,
                speed_bytes_per_sec: 0,
                updated_at: Utc::now().to_rfc3339(),
            },
        )
        .await;

        let mp4_filename = ensure_extension(&filename, "mp4");
        let mp4_path = resolve_available_output_path(&output_dir, &mp4_filename);

        match convert_ts_to_mp4_file(&ts_path, &mp4_path, true).await {
            Ok(()) => mp4_path,
            Err(_) => ts_path,
        }
    } else {
        ts_path
    };

    if delete_ts_temp_dir_after_download
        && !playback::has_active_playback_session(&playback_sessions, &task_id).await
    {
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    }

    Ok(DownloadRunOutcome::Completed(final_path))
}

fn rebalance_concurrency_permits(
    semaphore: &Arc<Semaphore>,
    held_permits: &mut Vec<OwnedSemaphorePermit>,
    target_concurrency: usize,
) -> Result<(), AppError> {
    let permits_to_hold = MAX_DOWNLOAD_CONCURRENCY.saturating_sub(target_concurrency);

    while held_permits.len() > permits_to_hold {
        held_permits.pop();
    }

    while held_permits.len() < permits_to_hold {
        match semaphore.clone().try_acquire_owned() {
            Ok(permit) => held_permits.push(permit),
            Err(TryAcquireError::NoPermits) => break,
            Err(TryAcquireError::Closed) => {
                return Err(AppError::Internal("下载并发控制已关闭".to_string()));
            }
        }
    }

    Ok(())
}

async fn download_worker_loop(
    semaphore: Arc<Semaphore>,
    priority_state: Arc<playback::DownloadPriorityState>,
    client: Arc<RwLock<reqwest::Client>>,
    rate_limiter: Arc<DownloadRateLimiter>,
    headers: Arc<RequestHeaders>,
    temp_dir: PathBuf,
    segments: Arc<Vec<SegmentInfo>>,
    completed: Arc<AtomicUsize>,
    total_bytes: Arc<AtomicU64>,
    speed_bytes_per_sec: Arc<AtomicU64>,
    completed_segment_indices: Arc<Mutex<BTreeSet<usize>>>,
    failed_segment_indices: Arc<Mutex<BTreeSet<usize>>>,
    downloads: Arc<Mutex<HashMap<DownloadId, DownloadTask>>>,
    app_handle: AppHandle,
    task_id: DownloadId,
    cancel: CancellationToken,
    total_segments: usize,
    persist_throttle: Arc<Mutex<PersistThrottleState>>,
) -> Result<(), AppError> {
    loop {
        if cancel.is_cancelled() {
            return Err(AppError::Cancelled);
        }

        let Some(segment_index) = priority_state.take_next_segment().await else {
            return Ok(());
        };

        let permit = tokio::select! {
            _ = cancel.cancelled() => {
                priority_state.requeue_segment(segment_index).await;
                return Err(AppError::Cancelled);
            }
            permit = semaphore.acquire() => permit
                .map_err(|_| AppError::Internal("下载并发控制已关闭".to_string()))?,
        };

        let segment = match segments.get(segment_index).cloned() {
            Some(segment) => segment,
            None => {
                drop(permit);
                priority_state.requeue_segment(segment_index).await;
                return Err(AppError::Internal(format!(
                    "Missing segment metadata for index {}",
                    segment_index
                )));
            }
        };

        let segment_path = segment_file_path(&temp_dir, segment.index);
        let outcome = download_segment_with_retry(
            client.clone(),
            rate_limiter.clone(),
            headers.clone(),
            &segment.uri,
            &segment_path,
            segment.encryption.as_ref(),
            segment.sequence_number,
            3,
            &cancel,
        )
        .await;

        drop(permit);

        match outcome {
            Ok(SegmentDownloadOutcome::Downloaded(file_size)) => {
                priority_state.mark_segment_completed(segment.index).await;
                total_bytes.fetch_add(file_size, Ordering::Relaxed);
                completed_segment_indices.lock().await.insert(segment.index + 1);
                failed_segment_indices.lock().await.remove(&(segment.index + 1));
                let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                let downloaded_bytes = total_bytes.load(Ordering::Relaxed);
                let speed = speed_bytes_per_sec.load(Ordering::Relaxed);
                let completed_segments_list = snapshot_segments(&completed_segment_indices).await;
                let failed_segments_list = snapshot_segments(&failed_segment_indices).await;

                emit_progress(
                    &app_handle,
                    &downloads,
                    RuntimeProgressSnapshot {
                        id: task_id.clone(),
                        status: DownloadStatus::Downloading,
                        completed_segments: done,
                        total_segments,
                        completed_segment_indices: completed_segments_list,
                        failed_segment_indices: failed_segments_list,
                        total_bytes: downloaded_bytes,
                        speed_bytes_per_sec: speed,
                        updated_at: Utc::now().to_rfc3339(),
                    },
                )
                .await;
                maybe_persist_task_progress(
                    &app_handle,
                    &downloads,
                    &task_id,
                    &persist_throttle,
                    false,
                )
                .await;
            }
            Ok(SegmentDownloadOutcome::Skipped) => {
                priority_state.mark_segment_skipped(segment.index).await;
                failed_segment_indices.lock().await.insert(segment.index + 1);
                let done = completed.load(Ordering::Relaxed);
                let downloaded_bytes = total_bytes.load(Ordering::Relaxed);
                let completed_segments_list = snapshot_segments(&completed_segment_indices).await;
                let failed_segments_list = snapshot_segments(&failed_segment_indices).await;

                emit_progress(
                    &app_handle,
                    &downloads,
                    RuntimeProgressSnapshot {
                        id: task_id.clone(),
                        status: DownloadStatus::Downloading,
                        completed_segments: done,
                        total_segments,
                        completed_segment_indices: completed_segments_list,
                        failed_segment_indices: failed_segments_list,
                        total_bytes: downloaded_bytes,
                        speed_bytes_per_sec: 0,
                        updated_at: Utc::now().to_rfc3339(),
                    },
                )
                .await;
                maybe_persist_task_progress(
                    &app_handle,
                    &downloads,
                    &task_id,
                    &persist_throttle,
                    true,
                )
                .await;
            }
            Err(AppError::Cancelled) => {
                priority_state.requeue_segment(segment.index).await;
                return Err(AppError::Cancelled);
            }
            Err(error) => {
                priority_state.requeue_segment(segment.index).await;
                return Err(error);
            }
        }
    }
}

// --- Segment Download ---

async fn download_segment_with_retry(
    client: Arc<RwLock<reqwest::Client>>,
    rate_limiter: Arc<DownloadRateLimiter>,
    headers: Arc<RequestHeaders>,
    url: &str,
    path: &Path,
    encryption: Option<&EncryptionInfo>,
    sequence_number: u64,
    max_retries: u32,
    cancel: &CancellationToken,
) -> Result<SegmentDownloadOutcome, AppError> {
    let mut attempts = 0;
    loop {
        if cancel.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        match download_segment(
            client.clone(),
            rate_limiter.clone(),
            headers.clone(),
            url,
            path,
            encryption,
            sequence_number,
            cancel,
        )
        .await
        {
            Ok(()) => {
                let file_size = tokio::fs::metadata(path).await?.len();
                return Ok(SegmentDownloadOutcome::Downloaded(file_size));
            }
            Err(e) => {
                if matches!(e, AppError::Cancelled) {
                    return Err(e);
                }
                attempts += 1;
                if attempts >= max_retries {
                    let _ = tokio::fs::remove_file(path).await;
                    let _ = tokio::fs::remove_file(path.with_extension("ts.part")).await;
                    eprintln!(
                        "[m3u8quicker] skip segment after {} failed attempts url={} error={}",
                        attempts, url, e
                    );
                    return Ok(SegmentDownloadOutcome::Skipped);
                }
                tokio::time::sleep(std::time::Duration::from_millis(500 * attempts as u64)).await;
            }
        }
    }
}

async fn download_segment(
    client: Arc<RwLock<reqwest::Client>>,
    rate_limiter: Arc<DownloadRateLimiter>,
    headers: Arc<RequestHeaders>,
    url: &str,
    path: &Path,
    encryption: Option<&EncryptionInfo>,
    sequence_number: u64,
    cancel: &CancellationToken,
) -> Result<(), AppError> {
    let part_path = path.with_extension("ts.part");
    if part_path.exists() {
        let _ = tokio::fs::remove_file(&part_path).await;
    }

    let active_client = client.read().await.clone();
    let response = build_request_with_headers(&active_client, url, headers.as_ref())
        .send()
        .await?
        .error_for_status()?;

    let mut stream = response.bytes_stream();
    let mut output = tokio::fs::File::create(&part_path).await?;

    while let Some(chunk) = stream.next().await {
        if cancel.is_cancelled() {
            output.flush().await?;
            return Err(AppError::Cancelled);
        }

        let chunk = chunk?;
        rate_limiter.wait_for_bytes(chunk.len(), cancel).await?;
        output.write_all(&chunk).await?;
    }
    output.flush().await?;
    drop(output);

    if cancel.is_cancelled() {
        return Err(AppError::Cancelled);
    }

    if let Some(enc) = encryption {
        let encrypted_bytes = tokio::fs::read(&part_path).await?;
        let iv = compute_iv(enc, sequence_number);
        let final_bytes = decrypt_aes_cbc(&encrypted_bytes, &enc.key_bytes, &iv)?;
        tokio::fs::write(path, &final_bytes).await?;
        let _ = tokio::fs::remove_file(&part_path).await;
    } else {
        tokio::fs::rename(&part_path, path).await?;
    }

    Ok(())
}

// --- Merge Segments ---

async fn merge_segments(temp_dir: &Path, total: usize, output_path: &Path) -> Result<(), AppError> {
    let segment_paths = (0..total)
        .map(|index| segment_file_path(temp_dir, index))
        .collect::<Vec<_>>();
    merge_files(&segment_paths, output_path).await
}

// --- TS to MP4 Conversion ---

pub async fn merge_ts_files_in_dir(input_dir: &Path, output_path: &Path) -> Result<(), AppError> {
    let mut entries = tokio::fs::read_dir(input_dir).await?;
    let mut files = Vec::new();

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        let is_ts = path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.eq_ignore_ascii_case("ts"))
            .unwrap_or(false);

        if is_ts && path.is_file() {
            files.push(path);
        }
    }

    files.sort_by(|a, b| {
        let a_name = a
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        let b_name = b
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        a_name.cmp(b_name)
    });

    if files.is_empty() {
        return Err(AppError::InvalidInput(
            "所选目录中未找到可合并的 ts 文件".to_string(),
        ));
    }

    merge_files(&files, output_path).await
}

pub async fn run_mp4_download(
    app_handle: AppHandle,
    downloads: Arc<Mutex<HashMap<DownloadId, DownloadTask>>>,
    client: Arc<RwLock<reqwest::Client>>,
    rate_limiter: Arc<DownloadRateLimiter>,
    task_id: DownloadId,
    url: String,
    headers: Arc<RequestHeaders>,
    output_dir: PathBuf,
    filename: String,
    cancel_token: CancellationToken,
) -> Result<DownloadRunOutcome, AppError> {
    let mp4_filename = ensure_extension(&filename, "mp4");
    let mp4_path = resolve_available_output_path(&output_dir, &mp4_filename);
    let partial_path = mp4_path.with_extension("mp4.partial");

    let client = client.read().await.clone();
    let response = build_request_with_headers(&client, &url, &headers)
        .send()
        .await?
        .error_for_status()?;

    let content_length = response.content_length().unwrap_or(0);
    let mut stream = response.bytes_stream();
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&partial_path)
        .await?;

    let mut downloaded: u64 = 0;
    let mut last_report = Instant::now();
    let mut last_report_bytes: u64 = 0;

    while let Some(chunk) = tokio::select! {
        chunk = stream.next() => chunk,
        _ = cancel_token.cancelled() => {
            let _ = tokio::fs::remove_file(&partial_path).await;
            return Err(AppError::Cancelled);
        }
    } {
        let chunk = chunk.map_err(|e| AppError::Network(e.to_string()))?;
        rate_limiter
            .wait_for_bytes(chunk.len(), &cancel_token)
            .await?;
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;

        if last_report.elapsed() >= Duration::from_secs(1) {
            let speed = downloaded.saturating_sub(last_report_bytes);
            last_report_bytes = downloaded;
            last_report = Instant::now();

            let total_segments = if content_length > 0 { 100 } else { 0 };
            let completed_segments = if content_length > 0 {
                ((downloaded as f64 / content_length as f64) * 100.0).min(100.0) as usize
            } else {
                0
            };

            emit_progress(
                &app_handle,
                &downloads,
                RuntimeProgressSnapshot {
                    id: task_id.clone(),
                    status: DownloadStatus::Downloading,
                    completed_segments,
                    total_segments,
                    completed_segment_indices: Vec::new(),
                    failed_segment_indices: Vec::new(),
                    total_bytes: downloaded,
                    speed_bytes_per_sec: speed,
                    updated_at: Utc::now().to_rfc3339(),
                },
            )
            .await;
        }
    }

    file.flush().await?;
    drop(file);
    tokio::fs::rename(&partial_path, &mp4_path).await?;

    Ok(DownloadRunOutcome::Completed(mp4_path))
}

pub async fn convert_ts_to_mp4_file(
    ts_path: &Path,
    mp4_path: &Path,
    delete_source: bool,
) -> Result<(), AppError> {
    let ts_path = ts_path.to_path_buf();
    let blocking_ts_path = ts_path.clone();
    let blocking_mp4_path = mp4_path.to_path_buf();

    tokio::task::spawn_blocking(move || {
        crate::remux::remux_ts_to_mp4_file(&blocking_ts_path, &blocking_mp4_path)
    })
    .await
    .map_err(|e| AppError::Conversion(format!("Task join error: {}", e)))?
    .map_err(|e| AppError::Conversion(format!("TS to MP4 conversion failed: {}", e)))?;

    if delete_source {
        let _ = tokio::fs::remove_file(ts_path).await;
    }

    Ok(())
}

async fn merge_files(files: &[PathBuf], output_path: &Path) -> Result<(), AppError> {
    let mut output_file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output_path)
        .await?;
    for file in files {
        let data = tokio::fs::read(file).await?;
        output_file.write_all(&data).await?;
    }
    output_file.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cbc::cipher::BlockEncryptMut;

    type Aes128CbcEnc = cbc::Encryptor<Aes128>;
    type Aes192CbcEnc = cbc::Encryptor<Aes192>;
    type Aes256CbcEnc = cbc::Encryptor<Aes256>;

    fn encrypt_aes_cbc_for_test(data: &[u8], key: &[u8], iv: &[u8; 16]) -> Vec<u8> {
        let mut buf = vec![0u8; data.len() + 16];
        buf[..data.len()].copy_from_slice(data);

        match key.len() {
            16 => {
                let key: [u8; 16] = key.try_into().expect("valid AES-128 key");
                Aes128CbcEnc::new((&key).into(), iv.into())
                    .encrypt_padded_mut::<Pkcs7>(&mut buf, data.len())
                    .expect("AES-128 encrypt")
                    .to_vec()
            }
            24 => {
                let key: [u8; 24] = key.try_into().expect("valid AES-192 key");
                Aes192CbcEnc::new((&key).into(), iv.into())
                    .encrypt_padded_mut::<Pkcs7>(&mut buf, data.len())
                    .expect("AES-192 encrypt")
                    .to_vec()
            }
            32 => {
                let key: [u8; 32] = key.try_into().expect("valid AES-256 key");
                Aes256CbcEnc::new((&key).into(), iv.into())
                    .encrypt_padded_mut::<Pkcs7>(&mut buf, data.len())
                    .expect("AES-256 encrypt")
                    .to_vec()
            }
            other => panic!("unexpected key length {}", other),
        }
    }

    #[test]
    fn decrypt_aes_cbc_supports_128_192_and_256_bit_keys() {
        let iv = [
            0x3c, 0x4d, 0x7e, 0x23, 0xed, 0xf7, 0x84, 0x18, 0xa3, 0xb4, 0xbe, 0xc4, 0x30,
            0xdf, 0x2b, 0x61,
        ];
        let plaintext = b"m3u8quicker AES CBC compatibility";
        let key_sizes = [16usize, 24, 32];

        for key_size in key_sizes {
            let key = (0..key_size).map(|index| index as u8).collect::<Vec<_>>();
            let ciphertext = encrypt_aes_cbc_for_test(plaintext, &key, &iv);
            let decrypted = decrypt_aes_cbc(&ciphertext, &key, &iv).expect("decrypt succeeds");
            assert_eq!(decrypted, plaintext);
        }
    }

    #[test]
    fn decrypt_aes_cbc_rejects_unsupported_key_lengths() {
        let iv = [0u8; 16];
        let err = decrypt_aes_cbc(b"ciphertext", &[1, 2, 3], &iv).expect_err("must fail");
        assert!(err.to_string().contains("Unsupported AES key length"));
    }

    #[test]
    fn reserve_rate_limit_delay_allows_unlimited_downloads() {
        let now = Instant::now();
        let mut state = DownloadRateLimitState {
            limit_kbps: 0,
            next_available_at: now,
        };

        let delay = reserve_rate_limit_delay(&mut state, 1024 * 1024, now);

        assert_eq!(delay, Duration::ZERO);
    }

    #[test]
    fn reserve_rate_limit_delay_delays_limited_chunks() {
        let now = Instant::now();
        let mut state = DownloadRateLimitState {
            limit_kbps: 1024,
            next_available_at: now,
        };

        let first_delay = reserve_rate_limit_delay(&mut state, 1024, now);
        let second_delay = reserve_rate_limit_delay(&mut state, 1024, now);

        assert!(first_delay > Duration::ZERO);
        assert!(second_delay > first_delay);
    }
}
