//! Opt-in Direct downloads. The legacy single-connection downloader is untouched.
use super::*;
use serde::Serialize;
use tokio::io::AsyncReadExt;

pub const CHUNK_SIZE: u64 = 2 * 1024 * 1024;
const MAX_ATTEMPTS: u32 = 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    version: u32,
    pub total: u64,
    pub chunk_size: u64,
    validator: Option<String>,
    pub filename: String,
}

#[derive(Default, Serialize, Deserialize)]
struct BlockState {
    attempts: u32,
    error: Option<String>,
}

impl Manifest {
    pub fn count(&self) -> usize { self.total.div_ceil(self.chunk_size) as usize }
    pub fn length(&self, index: usize) -> u64 {
        self.chunk_size.min(self.total - index as u64 * self.chunk_size)
    }
}

pub fn directory(task: &DownloadTask) -> PathBuf {
    temp_dir_for_task(Path::new(&task.output_dir), &task.id).join("direct")
}

pub fn block_path(dir: &Path, index: usize) -> PathBuf { dir.join(format!("{index:08}.part")) }
fn state_path(dir: &Path, index: usize) -> PathBuf { dir.join(format!("{index:08}.json")) }

// Keep a recoverable replacement if the process exits between removing and renaming
// on Windows. Writers are serialized per file; readers never observe partial JSON.
async fn write_record<T: Serialize>(path: &Path, value: &T) -> Result<(), AppError> {
    let pending = path.with_extension("pending");
    let bytes = serde_json::to_vec(value).map_err(|e| AppError::Internal(e.to_string()))?;
    let mut file = tokio::fs::File::create(&pending).await?;
    file.write_all(&bytes).await?;
    file.sync_all().await?;
    drop(file);
    if path.exists() { tokio::fs::remove_file(path).await?; }
    tokio::fs::rename(pending, path).await?;
    Ok(())
}

async fn read_record<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, AppError> {
    // A complete pending record is newer than the old file, even if replacement
    // has not started yet. Ignore an interrupted (invalid) pending write.
    if let Ok(bytes) = tokio::fs::read(path.with_extension("pending")).await {
        if let Ok(value) = serde_json::from_slice(&bytes) { return Ok(Some(value)); }
    }
    match tokio::fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some)
            .map_err(|e| AppError::InvalidInput(format!("分块记录损坏：{e}"))),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub async fn manifest(task: &DownloadTask) -> Result<Option<Manifest>, AppError> {
    let value: Option<Manifest> = read_record(&directory(task).join("manifest.json")).await?;
    if let Some(m) = &value {
        if m.version != 1 || m.total == 0 || m.chunk_size != CHUNK_SIZE
            || Path::new(&m.filename).file_name().and_then(|s| s.to_str()) != Some(&m.filename)
        {
            return Err(AppError::InvalidInput("分块记录无效，请重新创建下载任务".into()));
        }
    }
    Ok(value)
}

fn content_range(response: &reqwest::Response) -> Option<(u64, u64, u64)> {
    parse_content_range(response.headers().get(header::CONTENT_RANGE)?.to_str().ok()?)
}

fn parse_content_range(value: &str) -> Option<(u64, u64, u64)> {
    let (range, total) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let (start, end, total) = (start.parse().ok()?, end.parse().ok()?, total.parse().ok()?);
    if start <= end && end < total { Some((start, end, total)) } else { None }
}

fn validator(response: &reqwest::Response) -> Option<String> {
    response.headers().get(header::ETAG).and_then(|v| v.to_str().ok())
        .filter(|v| !v.starts_with("W/"))
        .or_else(|| response.headers().get(header::LAST_MODIFIED).and_then(|v| v.to_str().ok()))
        .map(str::to_owned)
}

async fn probe(client: &reqwest::Client, task: &DownloadTask, headers: &RequestHeaders)
    -> Result<Option<(u64, Option<String>)>, AppError>
{
    let response = build_request_with_headers(client, &task.url, headers)
        .header(header::ACCEPT_ENCODING, "identity")
        .header(header::RANGE, "bytes=0-0").timeout(metadata_timeout()).send().await?;
    if response.status() == StatusCode::RANGE_NOT_SATISFIABLE { return Ok(None); }
    let response = response.error_for_status()?;
    if response.status() != StatusCode::PARTIAL_CONTENT { return Ok(None); }
    let Some((0, 0, total)) = content_range(&response) else { return Ok(None); };
    if !identity_encoded(&response) { return Ok(None); }
    let identity = validator(&response);
    // Read only the promised byte, without buffering a misbehaving server's file.
    let mut stream = response.bytes_stream();
    let mut received = 0;
    while let Some(bytes) = stream.next().await {
        received += bytes?.len();
        if received > 1 { return Ok(None); }
    }
    Ok((received == 1).then_some((total, identity)))
}

fn identity_encoded(response: &reqwest::Response) -> bool {
    response.headers().get(header::CONTENT_ENCODING)
        .is_none_or(|v| v.as_bytes().eq_ignore_ascii_case(b"identity"))
}

pub async fn check_resume(client: &reqwest::Client, task: &DownloadTask, headers: &RequestHeaders)
    -> Result<Mp4ResumeCheck, AppError>
{
    let Some(m) = manifest(task).await? else {
        return match task.direct_info.as_ref() {
            Some(info) if !info.parallel => check_mp4_resume(client, &task.url, headers, Path::new(&task.output_dir), &task.filename).await,
            Some(_) => Err(AppError::InvalidInput("分块记录已丢失，请重新创建下载任务".into())),
            // Paused during the initial probe: no files belong to this task yet.
            // Do not accidentally resume another task with the same filename.
            None => Ok(Mp4ResumeCheck::Ready { downloaded_bytes: 0 }),
        };
    };
    let (complete, _, bytes) = inspect_blocks(&directory(task), &m).await?;
    if complete.len() == m.count() || completed_output(task, &m).await?.is_some() {
        return Ok(Mp4ResumeCheck::Ready { downloaded_bytes: m.total });
    }
    let remote = probe(client, task, headers).await?;
    if remote.is_some_and(|(size, identity)| size == m.total && m.validator.is_some() && identity == m.validator) {
        Ok(Mp4ResumeCheck::Ready { downloaded_bytes: bytes })
    } else {
        Ok(Mp4ResumeCheck::RequiresRestartConfirmation { downloaded_bytes: bytes })
    }
}

async fn inspect_blocks(dir: &Path, m: &Manifest) -> Result<(Vec<usize>, Vec<usize>, u64), AppError> {
    let mut complete = Vec::new();
    let mut failed = Vec::new();
    let mut bytes = 0;
    for index in 0..m.count() {
        let size = file_len_if_exists(&block_path(dir, index)).await?;
        if size > m.length(index) {
            tokio::fs::remove_file(block_path(dir, index)).await?;
            failed.push(index + 1);
            continue;
        }
        bytes += size;
        if size == m.length(index) { complete.push(index + 1); }
        else if read_record::<BlockState>(&state_path(dir, index)).await?.is_some_and(|s| s.error.is_some()) {
            failed.push(index + 1);
        }
    }
    Ok((complete, failed, bytes))
}

pub async fn restore(task: &mut DownloadTask) -> Result<(), AppError> {
    if !task.direct_parallel { return Ok(()); }
    if let Some(m) = manifest(task).await? {
        if let Some(path) = completed_output(task, &m).await? {
            task.status = DownloadStatus::Completed;
            task.file_path = Some(path.to_string_lossy().into_owned());
            task.filename = path.file_name().unwrap().to_string_lossy().into_owned();
            task.completed_at = Some(Utc::now());
        }
        let (complete, failed, bytes) = inspect_blocks(&directory(task), &m).await?;
        task.direct_info = Some(DirectDownloadInfo { parallel: true, total_bytes: m.total });
        task.total_segments = m.count();
        task.completed_segments = complete.len();
        task.completed_segment_indices = complete;
        task.failed_segment_indices = failed;
        task.total_bytes = bytes;
    }
    Ok(())
}

async fn completed_output(task: &DownloadTask, m: &Manifest) -> Result<Option<PathBuf>, AppError> {
    let name: Option<String> = read_record(&directory(task).join("completed.json")).await?;
    if let Some(name) = name {
        if Path::new(&name).file_name().and_then(|s| s.to_str()) != Some(&name) {
            return Err(AppError::InvalidInput("下载完成记录无效".into()));
        }
        let path = Path::new(&task.output_dir).join(name);
        if file_len_if_exists(&path).await? == m.total { return Ok(Some(path)); }
    }
    Ok(None)
}

pub async fn cleanup(task: &DownloadTask) -> Result<(), AppError> {
    if let Some(m) = manifest(task).await? {
        let partial = mp4_partial_path_for_output_path(&Path::new(&task.output_dir).join(m.filename));
        if partial.exists() { tokio::fs::remove_file(partial).await?; }
    }
    let dir = directory(task);
    if dir.exists() { tokio::fs::remove_dir_all(dir).await?; }
    Ok(())
}

pub async fn errors(task: &DownloadTask) -> Result<HashMap<usize, String>, AppError> {
    let mut errors = HashMap::new();
    for index in &task.failed_segment_indices {
        if let Some(index0) = index.checked_sub(1) {
            if let Some(error) = read_record::<BlockState>(&state_path(&directory(task), index0)).await?.and_then(|s| s.error) {
                errors.insert(*index, error);
            }
        }
    }
    Ok(errors)
}

enum BlockError { Retry(AppError), RangeIgnored, ResourceChanged }
impl From<AppError> for BlockError { fn from(e: AppError) -> Self { Self::Retry(e) } }
impl From<std::io::Error> for BlockError { fn from(e: std::io::Error) -> Self { Self::Retry(e.into()) } }
impl From<reqwest::Error> for BlockError { fn from(e: reqwest::Error) -> Self { Self::Retry(e.into()) } }

async fn download_block(
    client: &reqwest::Client, url: &str, headers: &RequestHeaders, dir: &Path,
    m: &Manifest, index: usize, limiter: &DownloadRateLimiter,
    received: &AtomicU64, cancel: &CancellationToken,
) -> Result<(), BlockError> {
    let path = block_path(dir, index);
    let have = file_len_if_exists(&path).await?;
    let length = m.length(index);
    if have == length { return Ok(()); }
    let start = index as u64 * m.chunk_size + have;
    let end = index as u64 * m.chunk_size + length - 1;
    let mut request = build_request_with_headers(client, url, headers)
        .header(header::ACCEPT_ENCODING, "identity")
        .header(header::RANGE, format!("bytes={start}-{end}"))
        .timeout(segment_timeout());
    if let Some(identity) = &m.validator { request = request.header(header::IF_RANGE, identity); }
    let response = tokio::select! {
        _ = cancel.cancelled() => return Err(BlockError::Retry(AppError::Cancelled)),
        response = request.send() => response?,
    };
    if response.status() == StatusCode::OK { return Err(BlockError::RangeIgnored); }
    let response = response.error_for_status()?;
    if response.status() != StatusCode::PARTIAL_CONTENT
        || content_range(&response) != Some((start, end, m.total))
        || !identity_encoded(&response)
        || (m.validator.is_some() && validator(&response).is_some() && validator(&response) != m.validator)
    { return Err(BlockError::ResourceChanged); }
    let mut file = tokio::fs::OpenOptions::new().create(true).append(true).open(path).await?;
    let mut stream = response.bytes_stream();
    let mut written = have;
    while let Some(bytes) = tokio::select! {
        _ = cancel.cancelled() => {
            file.sync_all().await?;
            return Err(BlockError::Retry(AppError::Cancelled));
        }
        bytes = stream.next() => bytes,
    } {
        let bytes = bytes?;
        if written + bytes.len() as u64 > length { return Err(BlockError::ResourceChanged); }
        limiter.wait_for_bytes(bytes.len(), cancel).await?;
        file.write_all(&bytes).await?;
        // Playback reads only flushed bytes; a crash can resume from the actual length.
        file.flush().await?;
        written += bytes.len() as u64;
        received.fetch_add(bytes.len() as u64, Ordering::Relaxed);
    }
    file.sync_all().await?;
    if written != length {
        return Err(BlockError::Retry(AppError::Network("分块响应提前结束".into())));
    }
    Ok(())
}

async fn block_worker(
    client: reqwest::Client, url: String, headers: Arc<RequestHeaders>, dir: PathBuf,
    m: Arc<Manifest>, index: usize, limiter: Arc<DownloadRateLimiter>,
    received: Arc<AtomicU64>, cancel: CancellationToken,
) -> (usize, Result<(), BlockError>) {
    let result = async {
        for attempt in 1..=MAX_ATTEMPTS {
            let result = download_block(&client, &url, &headers, &dir, &m, index, &limiter, &received, &cancel).await;
            match result {
                Ok(()) => {
                    write_record(&state_path(&dir, index), &BlockState::default()).await?;
                    return Ok(());
                }
                Err(BlockError::Retry(AppError::Cancelled)) => return Err(BlockError::Retry(AppError::Cancelled)),
                Err(BlockError::Retry(error)) => {
                    write_record(&state_path(&dir, index), &BlockState { attempts: attempt, error: Some(error.to_string()) }).await?;
                    if attempt == MAX_ATTEMPTS { return Err(BlockError::Retry(error)); }
                    tokio::select! {
                        _ = cancel.cancelled() => return Err(BlockError::Retry(AppError::Cancelled)),
                        _ = tokio::time::sleep(Duration::from_secs(1 << attempt)) => {}
                    }
                }
                Err(error) => {
                    // A malformed response must not leave a full-length block
                    // which could be mistaken for a completed block on restart.
                    let _ = tokio::fs::remove_file(block_path(&dir, index)).await;
                    write_record(&state_path(&dir, index), &BlockState {
                        attempts: attempt, error: Some("服务器返回的分块范围或资源不一致".into()),
                    }).await?;
                    return Err(error);
                }
            }
        }
        unreachable!()
    }.await;
    (index, result)
}

async fn publish<R: tauri::Runtime>(
    app: &AppHandle<R>, downloads: &Arc<Mutex<HashMap<DownloadId, DownloadTask>>>,
    id: &str, m: &Manifest, complete: &BTreeSet<usize>, failed: &BTreeSet<usize>,
    bytes: u64, speed: u64,
) {
    let mut tasks = downloads.lock().await;
    if let Some(task) = tasks.get_mut(id) {
        task.direct_info = Some(DirectDownloadInfo { parallel: true, total_bytes: m.total });
        task.total_segments = m.count();
        task.completed_segments = complete.len();
        task.completed_segment_indices = complete.iter().copied().collect();
        task.failed_segment_indices = failed.iter().copied().collect();
        task.total_bytes = bytes;
        task.speed_bytes_per_sec = speed;
        task.touch();
        let _ = app.emit("download-progress", DownloadProgressEvent {
            direct_info: task.direct_info.clone(), id: id.to_string(), status: task.status.clone(),
            group: download_group_for_status(&task.status), completed_segments: complete.len(),
            total_segments: m.count(), failed_segment_count: failed.len(), total_bytes: bytes,
            speed_bytes_per_sec: speed, percentage: bytes as f64 / m.total as f64 * 100.0,
            updated_at: task.last_updated_at().to_rfc3339(),
        });
    }
}

fn unsupported_ranges() -> AppError {
    AppError::InvalidInput(crate::i18n::tr("directParallelNotSupported").to_string())
}

async fn initialize_manifest(task: &DownloadTask, total: u64, identity: Option<String>) -> Result<Manifest, AppError> {
    let dir = directory(task);
    tokio::fs::create_dir_all(&dir).await?;
    // Reserve a collision-free output name without touching another task's files.
    let output = loop {
        let (output, partial) = resolve_available_mp4_output_paths(Path::new(&task.output_dir), &task.filename);
        match tokio::fs::OpenOptions::new().create_new(true).write(true).open(partial).await {
            Ok(_) => break output,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    };
    let m = Manifest { version: 1, total, chunk_size: CHUNK_SIZE, validator: identity,
        filename: output.file_name().unwrap().to_string_lossy().into_owned() };
    if let Err(error) = write_record(&dir.join("manifest.json"), &m).await {
        let _ = tokio::fs::remove_file(mp4_partial_path_for_output_path(&output)).await;
        return Err(error);
    }
    Ok(m)
}

/// Run before publishing a new task: both network and initial disk failures are
/// returned to the creation dialog, and the worker reuses this checked manifest.
pub async fn prepare_new(client: &reqwest::Client, task: &DownloadTask, headers: &RequestHeaders)
    -> Result<DirectDownloadInfo, AppError>
{
    let (total, identity) = probe(client, task, headers).await?.ok_or_else(unsupported_ranges)?;
    if let Err(error) = initialize_manifest(task, total, identity).await {
        let _ = tokio::fs::remove_dir_all(directory(task)).await;
        return Err(error);
    }
    Ok(DirectDownloadInfo { parallel: true, total_bytes: total })
}

/// None is only for resuming a legacy task already saved in single-connection mode.
pub async fn run<R: tauri::Runtime>(
    app: &AppHandle<R>, downloads: Arc<Mutex<HashMap<DownloadId, DownloadTask>>>,
    client: Arc<RwLock<reqwest::Client>>, limiter: Arc<DownloadRateLimiter>, task: &DownloadTask,
    headers: Arc<RequestHeaders>, concurrency: Arc<Mutex<usize>>,
    priorities: Arc<Mutex<HashMap<DownloadId, Arc<playback::DownloadPriorityState>>>>,
    restart: bool, cancel: CancellationToken, prechecked: bool,
) -> Result<Option<DownloadRunOutcome>, AppError> {
    let old = manifest(task).await?;
    if old.is_none() && task.direct_info.as_ref().is_some_and(|info| info.parallel) {
        return Err(AppError::InvalidInput("分块记录已丢失，请重新创建下载任务".into()));
    }
    if let Some(m) = &old {
        if let Some(path) = completed_output(task, m).await? {
            return Ok(Some(DownloadRunOutcome::Completed(path)));
        }
    }
    if old.is_none() && task.direct_info.as_ref().is_some_and(|info| !info.parallel) { return Ok(None); }
    let http = client.read().await.clone();
    let locally_complete = if let Some(m) = &old {
        inspect_blocks(&directory(task), m).await?.0.len() == m.count()
    } else { false };
    let remote = if locally_complete || (prechecked && old.is_some()) {
        old.as_ref().map(|m| (m.total, m.validator.clone()))
    } else {
        tokio::select! {
            _ = cancel.cancelled() => return Err(AppError::Cancelled),
            result = probe(&http, task, &headers) => result?,
        }
    };
    let (total, identity) = remote.ok_or_else(unsupported_ranges)?;
    let old = if let Some(m) = old {
        let unchanged = locally_complete || prechecked
            || (total == m.total && m.validator.is_some() && identity == m.validator);
        if !unchanged {
            if !restart { return Err(AppError::InvalidInput("远端文件或分片支持已变化，请确认重新下载".into())); }
            cleanup(task).await?;
            None
        } else { Some(m) }
    } else { None };
    let dir = directory(task);
    let m = Arc::new(if let Some(m) = old { m } else {
        initialize_manifest(task, total, identity).await?
    });
    let (complete, _, bytes) = inspect_blocks(&dir, &m).await?;
    let mut complete: BTreeSet<usize> = complete.into_iter().collect();
    let mut failed = BTreeSet::new();
    let priority = playback::prepare_download_priority_state(&priorities, &task.id, m.count(), &complete.iter().copied().collect::<Vec<_>>(), &[]).await;
    // First bytes and tail metadata (e.g. MP4 moov) are scheduled before the body.
    priority.prioritize_segment(m.count() - 1).await;
    priority.prioritize_segment(0).await;
    let received = Arc::new(AtomicU64::new(bytes));
    let workers_cancel = cancel.child_token();
    let mut workers = tokio::task::JoinSet::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(250));
    let mut reported_at = Instant::now();
    let mut reported_bytes = bytes;
    let mut fatal = None;
    publish(app, &downloads, &task.id, &m, &complete, &failed, bytes, 0).await;
    loop {
        let limit = normalize_download_concurrency(*concurrency.lock().await);
        while workers.len() < limit && !cancel.is_cancelled() {
            let Some(index) = priority.take_next_segment().await else { break; };
            workers.spawn(block_worker(http.clone(), task.url.clone(), headers.clone(), dir.clone(), m.clone(), index,
                limiter.clone(), received.clone(), workers_cancel.clone()));
        }
        if workers.is_empty() { break; }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => { fatal = Some(AppError::Cancelled); break; }
            result = workers.join_next() => {
                match result {
                    Some(Ok((index, result))) => {
                        priority.mark_segment_completed(index).await;
                        match result {
                            Ok(()) => { complete.insert(index + 1); }
                            Err(BlockError::RangeIgnored) => { fatal = Some(unsupported_ranges()); break; }
                            Err(BlockError::ResourceChanged) => {
                                fatal = Some(AppError::InvalidInput("分块响应与原文件不一致，请重新创建下载任务".into())); break;
                            }
                            Err(BlockError::Retry(AppError::Cancelled)) => { fatal = Some(AppError::Cancelled); break; }
                            Err(BlockError::Retry(_)) => { failed.insert(index + 1); }
                        }
                        publish(app, &downloads, &task.id, &m, &complete, &failed, received.load(Ordering::Relaxed), 0).await;
                    }
                    Some(Err(error)) => { fatal = Some(AppError::Internal(error.to_string())); break; }
                    None => {}
                }
            }
            _ = ticker.tick() => {
                if reported_at.elapsed() >= Duration::from_secs(1) {
                    let bytes = received.load(Ordering::Relaxed);
                    let speed = (bytes.saturating_sub(reported_bytes) as f64 / reported_at.elapsed().as_secs_f64()) as u64;
                    publish(app, &downloads, &task.id, &m, &complete, &failed, bytes, speed).await;
                    reported_bytes = bytes;
                    reported_at = Instant::now();
                }
            }
        }
    }
    workers_cancel.cancel();
    // Wait for all file handles to close before cleanup or assembly, including on pause.
    while workers.join_next().await.is_some() {}
    if cancel.is_cancelled() { fatal = Some(AppError::Cancelled); }
    let (disk_complete, disk_failed, bytes) = inspect_blocks(&dir, &m).await?;
    complete = disk_complete.into_iter().collect();
    failed.extend(disk_failed);
    failed.retain(|index| !complete.contains(index));
    publish(app, &downloads, &task.id, &m, &complete, &failed, bytes, 0).await;
    if let Some(error) = fatal { return Err(error); }
    if complete.len() != m.count() {
        let message = failed.iter().next().map(|index| state_path(&dir, index - 1));
        let error = if let Some(path) = message {
            read_record::<BlockState>(&path).await?.and_then(|s| s.error).unwrap_or_default()
        } else { String::new() };
        return Err(AppError::Network(format!("{} 个分块下载失败，可重试失败分片。{}", failed.len(), error)));
    }
    if let Some(task) = downloads.lock().await.get_mut(&task.id) {
        if task.status == DownloadStatus::Downloading { task.status = DownloadStatus::Merging; }
    }
    publish(app, &downloads, &task.id, &m, &complete, &failed, bytes, 0).await;
    let result = assemble(task, &dir, &m, &cancel).await?;
    Ok(Some(DownloadRunOutcome::Completed(result)))
}

async fn assemble(task: &DownloadTask, dir: &Path, m: &Manifest, cancel: &CancellationToken) -> Result<PathBuf, AppError> {
    let output = Path::new(&task.output_dir).join(&m.filename);
    let partial = mp4_partial_path_for_output_path(&output);
    let mut file = tokio::fs::File::create(&partial).await?;
    for index in 0..m.count() {
        let mut block = tokio::fs::File::open(block_path(dir, index)).await?;
        if block.metadata().await?.len() != m.length(index) {
            return Err(AppError::InvalidInput("分块长度不匹配，请重试下载".into()));
        }
        let mut buffer = vec![0; 256 * 1024];
        loop {
            if cancel.is_cancelled() {
                file.sync_all().await?;
                return Err(AppError::Cancelled);
            }
            let read = block.read(&mut buffer).await?;
            if read == 0 { break; }
            file.write_all(&buffer[..read]).await?;
        }
    }
    file.sync_all().await?;
    drop(file);
    // Never replace an unrelated file created while the download was running.
    let final_path = if output.exists() {
        resolve_available_output_path(Path::new(&task.output_dir), &m.filename)
    } else { output };
    publish_file(&partial, &final_path).await?;
    write_record(&dir.join("completed.json"), &final_path.file_name().unwrap().to_string_lossy()).await?;
    let _ = tokio::fs::remove_file(partial).await;
    // Keep blocks until the worker persists Completed, so active playback readers
    // can cross the finalization boundary safely. The caller then cleans them up.
    Ok(final_path)
}

async fn publish_file(partial: &Path, output: &Path) -> Result<(), std::io::Error> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        // MoveFileW supports NTFS as well as FAT/exFAT and fails if output exists.
        // std::fs::rename uses replacement semantics on Windows.
        let source: Vec<u16> = partial.as_os_str().encode_wide().chain(Some(0)).collect();
        let target: Vec<u16> = output.as_os_str().encode_wide().chain(Some(0)).collect();
        tokio::task::spawn_blocking(move || {
            #[link(name = "kernel32")]
            extern "system" { fn MoveFileW(source: *const u16, target: *const u16) -> i32; }
            // SAFETY: both buffers are owned, NUL-terminated UTF-16 paths and
            // remain alive throughout this synchronous call.
            if unsafe { MoveFileW(source.as_ptr(), target.as_ptr()) } == 0 {
                Err(std::io::Error::last_os_error())
            } else { Ok(()) }
        }).await.map_err(std::io::Error::other)?
    }
    #[cfg(not(windows))]
    {
        match tokio::fs::hard_link(partial, output).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => Err(error),
            Err(_) => {
                // Filesystems without hard links still get create-only output.
                let mut file = tokio::fs::OpenOptions::new().write(true).create_new(true).open(output).await?;
                let result = async {
                    let mut source = tokio::fs::File::open(partial).await?;
                    tokio::io::copy(&mut source, &mut file).await?;
                    file.sync_all().await
                }.await;
                drop(file);
                if result.is_err() { let _ = tokio::fs::remove_file(output).await; }
                result
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    pub(super) fn task(dir: &Path, url: &str) -> DownloadTask {
        serde_json::from_value(serde_json::json!({
            "id": "direct-test", "url": url, "filename": "video.webm", "file_type": "webm",
            "direct_parallel": true, "output_dir": dir, "status": "Downloading",
            "total_segments": 0, "completed_segments": 0, "total_bytes": 0,
            "speed_bytes_per_sec": 0, "created_at": Utc::now(), "completed_at": null, "file_path": null
        })).unwrap()
    }

    struct Fixture {
        root: PathBuf,
        url: String,
        payload: Arc<Vec<u8>>,
        requests: Arc<Mutex<Vec<(u64, u64)>>>,
        peak: Arc<AtomicUsize>,
        mode: Arc<AtomicUsize>,
        server: tokio::task::JoinHandle<()>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.server.abort();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    impl Fixture {
        async fn new(size: usize, mode: usize) -> Self {
            let root = std::env::temp_dir().join(format!("direct-test-{}", uuid::Uuid::new_v4()));
            tokio::fs::create_dir_all(&root).await.unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/video.webm", listener.local_addr().unwrap());
            let payload = Arc::new((0..size).map(|i| (i % 251) as u8).collect::<Vec<_>>());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let peak = Arc::new(AtomicUsize::new(0));
            let active = Arc::new(AtomicUsize::new(0));
            let mode = Arc::new(AtomicUsize::new(mode));
            let interrupted = Arc::new(AtomicUsize::new(0));
            let server = {
                let (payload, requests, peak, mode) = (payload.clone(), requests.clone(), peak.clone(), mode.clone());
                tokio::spawn(async move {
                    while let Ok((mut socket, _)) = listener.accept().await {
                        let (payload, requests, peak, active, mode, interrupted) = (payload.clone(), requests.clone(), peak.clone(), active.clone(), mode.clone(), interrupted.clone());
                        tokio::spawn(async move {
                            let mut raw = Vec::new();
                            while !raw.ends_with(b"\r\n\r\n") {
                                let Ok(byte) = socket.read_u8().await else { return; };
                                raw.push(byte);
                                if raw.len() > 16000 { return; }
                            }
                            let request = String::from_utf8(raw).unwrap().to_ascii_lowercase();
                            let range = request.lines().find_map(|line| line.strip_prefix("range: bytes=")).unwrap();
                            let (start, end) = range.split_once('-').unwrap();
                            let (start, end) = (start.parse::<u64>().unwrap(), end.parse::<u64>().unwrap());
                            requests.lock().await.push((start, end));
                            let probe = start == 0 && end == 0;
                            let mode = mode.load(Ordering::Relaxed);
                            if mode == 1 || (mode == 2 && !probe) {
                                let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\nx").await;
                                return;
                            }
                            if (mode == 3 && !probe) || mode == 7 {
                                let _ = socket.write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                                return;
                            }
                            let identity = if mode == 5 { "new" } else { "test" };
                            let count = active.fetch_add(1, Ordering::Relaxed) + 1;
                            peak.fetch_max(count, Ordering::Relaxed);
                            let response_start = if (mode == 4 && !probe) || mode == 8 { start + 1 } else { start };
                            let response = format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {response_start}-{end}/{}\r\nContent-Length: {}\r\nETag: \"{identity}\"\r\nConnection: close\r\n\r\n", payload.len(), end - start + 1);
                            let _ = socket.write_all(response.as_bytes()).await;
                            let truncate = mode == 6 && !probe && interrupted.fetch_add(1, Ordering::Relaxed) == 0;
                            let end = if truncate { (start + 16383).min(end) } else { end };
                            for bytes in payload[start as usize..=end as usize].chunks(16 * 1024) {
                                if socket.write_all(bytes).await.is_err() { break; }
                                if !probe { tokio::time::sleep(Duration::from_millis(2)).await; }
                            }
                            let _ = socket.shutdown().await;
                            active.fetch_sub(1, Ordering::Relaxed);
                        });
                    }
                })
            };
            Self { root, url, payload, requests, peak, mode, server }
        }
    }

    async fn run_test(f: &Fixture, task: &DownloadTask, cancel: CancellationToken) -> Result<Option<DownloadRunOutcome>, AppError> {
        run_test_prechecked(f, task, cancel, false).await
    }

    async fn run_test_prechecked(_f: &Fixture, task: &DownloadTask, cancel: CancellationToken, prechecked: bool) -> Result<Option<DownloadRunOutcome>, AppError> {
        let app = tauri::test::mock_app();
        let downloads = Arc::new(Mutex::new(HashMap::from([(task.id.clone(), task.clone())])));
        tokio::time::timeout(Duration::from_secs(30), run(app.handle(), downloads,
            Arc::new(RwLock::new(reqwest::Client::builder().no_proxy().build().unwrap())),
            Arc::new(DownloadRateLimiter::new(0)), task, Arc::new(RequestHeaders::new()),
            Arc::new(Mutex::new(2)), Arc::new(Mutex::new(HashMap::new())), false, cancel, prechecked,
        )).await.expect("download timeout")
    }

    #[test]
    fn parses_only_valid_content_ranges() {
        assert_eq!(parse_content_range("bytes 0-0/50"), Some((0, 0, 50)));
        for value in ["bytes 1-0/50", "bytes 0-50/50", "bytes 0-0/*", "bytes */50", "garbage"] {
            assert_eq!(parse_content_range(value), None);
        }
    }

    #[test]
    fn legacy_tasks_and_new_requests_default_to_single_connection() {
        let mut value = serde_json::to_value(task(Path::new("."), "http://localhost/video.mp4")).unwrap();
        value.as_object_mut().unwrap().remove("direct_parallel");
        value.as_object_mut().unwrap().remove("direct_info");
        let legacy: DownloadTask = serde_json::from_value(value).unwrap();
        assert!(!legacy.direct_parallel);
        assert!(legacy.direct_info.is_none());
        let params: CreateDownloadParams = serde_json::from_value(serde_json::json!({"url":"http://localhost/video.mp4"})).unwrap();
        assert!(!params.direct_parallel);
    }

    #[tokio::test]
    async fn parallel_download_matches_source_and_respects_limit() {
        let f = Fixture::new(CHUNK_SIZE as usize * 2 + 1000, 0).await;
        let task = task(&f.root, &f.url);
        tokio::fs::write(f.root.join("video.webm"), b"existing").await.unwrap();
        let result = run_test(&f, &task, CancellationToken::new()).await.unwrap();
        let Some(DownloadRunOutcome::Completed(path)) = result else { panic!("not completed") };
        assert_eq!(tokio::fs::read(path).await.unwrap(), *f.payload);
        assert_eq!(tokio::fs::read(f.root.join("video.webm")).await.unwrap(), b"existing");
        assert_eq!(f.peak.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn rejects_unsupported_ranges_without_switching_to_single_connection() {
        for mode in [1, 2] {
            let f = Fixture::new(128 * 1024, mode).await;
            assert!(run_test(&f, &task(&f.root, &f.url), CancellationToken::new()).await.is_err());
            assert!(!f.root.join("video.webm").exists());
        }
    }

    #[tokio::test]
    async fn preflight_rejects_unsupported_or_unavailable_urls_before_creating_files() {
        for mode in [1, 7, 8] {
            let f = Fixture::new(8192, mode).await;
            let task = task(&f.root, &f.url);
            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            assert!(prepare_new(&client, &task, &RequestHeaders::new()).await.is_err());
            assert!(!directory(&task).exists());
            assert!(!f.root.join("video.webm.partial").exists());
        }
    }

    #[tokio::test]
    async fn preflight_checks_output_path_and_worker_reuses_successful_probe() {
        let f = Fixture::new(8192, 0).await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut task = task(&f.root, &f.url);
        let invalid_dir = f.root.join("not-a-directory");
        tokio::fs::write(&invalid_dir, b"keep").await.unwrap();
        task.output_dir = invalid_dir.to_string_lossy().into_owned();
        assert!(prepare_new(&client, &task, &RequestHeaders::new()).await.is_err());
        assert_eq!(tokio::fs::read(invalid_dir).await.unwrap(), b"keep");
        task.output_dir = f.root.to_string_lossy().into_owned();
        f.requests.lock().await.clear();
        task.direct_info = Some(prepare_new(&client, &task, &RequestHeaders::new()).await.unwrap());
        assert!(manifest(&task).await.unwrap().is_some());
        let result = run_test_prechecked(&f, &task, CancellationToken::new(), true).await.unwrap();
        let Some(DownloadRunOutcome::Completed(path)) = result else { panic!("not completed") };
        assert_eq!(tokio::fs::read(path).await.unwrap(), *f.payload);
        assert_eq!(*f.requests.lock().await, vec![(0, 0), (0, f.payload.len() as u64 - 1)]);
    }

    #[tokio::test]
    async fn paused_probe_does_not_resume_or_delete_another_tasks_partial() {
        let f = Fixture::new(8192, 0).await;
        let task = task(&f.root, &f.url);
        let partial = f.root.join("video.webm.partial");
        tokio::fs::write(&partial, b"another download").await.unwrap();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        assert!(matches!(check_resume(&client, &task, &RequestHeaders::new()).await.unwrap(), Mp4ResumeCheck::Ready { downloaded_bytes: 0 }));
        assert!(f.requests.lock().await.is_empty());
        cleanup(&task).await.unwrap();
        assert_eq!(tokio::fs::read(partial).await.unwrap(), b"another download");
    }

    #[tokio::test]
    async fn finished_blocks_can_be_merged_offline_after_restart() {
        let f = Fixture::new(8192, 0).await;
        let task = task(&f.root, "http://127.0.0.1:1/expired-url");
        let dir = directory(&task);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let m = Manifest { version: 1, total: f.payload.len() as u64, chunk_size: CHUNK_SIZE,
            validator: None, filename: task.filename.clone() };
        write_record(&dir.join("manifest.json"), &m).await.unwrap();
        tokio::fs::write(block_path(&dir, 0), f.payload.as_slice()).await.unwrap();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        assert!(matches!(check_resume(&client, &task, &RequestHeaders::new()).await.unwrap(), Mp4ResumeCheck::Ready { downloaded_bytes: 8192 }));
        let result = run_test(&f, &task, CancellationToken::new()).await.unwrap();
        let Some(DownloadRunOutcome::Completed(path)) = result else { panic!("not completed") };
        assert_eq!(tokio::fs::read(path).await.unwrap(), *f.payload);
        assert!(f.requests.lock().await.is_empty());
    }

    #[tokio::test]
    async fn interrupted_block_retries_from_its_actual_length() {
        let f = Fixture::new(128 * 1024, 6).await;
        let result = run_test(&f, &task(&f.root, &f.url), CancellationToken::new()).await.unwrap();
        let Some(DownloadRunOutcome::Completed(path)) = result else { panic!("not completed") };
        assert_eq!(tokio::fs::read(path).await.unwrap(), *f.payload);
        assert!(f.requests.lock().await.iter().any(|(start, _)| *start == 16384));
    }

    #[tokio::test]
    async fn restart_recovers_partial_complete_and_failed_blocks() {
        let f = Fixture::new(CHUNK_SIZE as usize + 128 * 1024, 0).await;
        let mut task = task(&f.root, &f.url);
        let dir = directory(&task);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let m = Manifest { version: 1, total: f.payload.len() as u64, chunk_size: CHUNK_SIZE,
            validator: Some("\"test\"".into()), filename: task.filename.clone() };
        write_record(&dir.join("manifest.json"), &m).await.unwrap();
        tokio::fs::write(block_path(&dir, 0), &f.payload[..CHUNK_SIZE as usize]).await.unwrap();
        tokio::fs::write(block_path(&dir, 1), &f.payload[CHUNK_SIZE as usize..CHUNK_SIZE as usize + 4000]).await.unwrap();
        write_record(&state_path(&dir, 1), &BlockState { attempts: 4, error: Some("connection reset".into()) }).await.unwrap();
        // Simulate loading a persisted task which predates all progress events.
        task = serde_json::from_slice(&serde_json::to_vec(&task).unwrap()).unwrap();
        restore(&mut task).await.unwrap();
        assert_eq!(task.completed_segment_indices, vec![1]);
        assert_eq!(task.failed_segment_indices, vec![2]);
        assert_eq!(task.total_bytes, CHUNK_SIZE + 4000);
        let result = run_test(&f, &task, CancellationToken::new()).await.unwrap();
        let Some(DownloadRunOutcome::Completed(path)) = result else { panic!("not completed") };
        assert_eq!(tokio::fs::read(path).await.unwrap(), *f.payload);
        assert_eq!(*f.requests.lock().await, vec![(0, 0), (CHUNK_SIZE + 4000, f.payload.len() as u64 - 1)]);
    }

    #[tokio::test]
    async fn failures_are_persisted_and_manual_retry_recovers() {
        let f = Fixture::new(8192, 3).await;
        let mut task = task(&f.root, &f.url);
        assert!(run_test(&f, &task, CancellationToken::new()).await.is_err());
        restore(&mut task).await.unwrap();
        assert_eq!(task.failed_segment_indices, vec![1]);
        assert_eq!(read_record::<BlockState>(&state_path(&directory(&task), 0)).await.unwrap().unwrap().attempts, 4);
        f.mode.store(0, Ordering::Relaxed);
        assert!(matches!(run_test(&f, &task, CancellationToken::new()).await.unwrap(), Some(DownloadRunOutcome::Completed(_))));
    }

    #[tokio::test]
    async fn pause_closes_workers_and_resume_uses_partial_bytes() {
        let f = Fixture::new(CHUNK_SIZE as usize + 128 * 1024, 0).await;
        let mut task = task(&f.root, &f.url);
        let cancel = CancellationToken::new();
        let download = run_test(&f, &task, cancel.clone());
        let stop = async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            cancel.cancel();
        };
        let (result, _) = tokio::join!(download, stop);
        assert!(matches!(result, Err(AppError::Cancelled)));
        restore(&mut task).await.unwrap();
        assert!(task.total_bytes > 0 && task.total_bytes < f.payload.len() as u64);
        assert!(task.failed_segment_indices.is_empty());
        let result = run_test(&f, &task, CancellationToken::new()).await.unwrap();
        let Some(DownloadRunOutcome::Completed(path)) = result else { panic!("not completed") };
        assert_eq!(tokio::fs::read(path).await.unwrap(), *f.payload);
    }

    #[tokio::test]
    async fn rejects_wrong_ranges_and_changed_resources() {
        let f = Fixture::new(8192, 4).await;
        let task = task(&f.root, &f.url);
        assert!(run_test(&f, &task, CancellationToken::new()).await.is_err());
        assert!(!f.root.join("video.webm").exists());
        assert!(!block_path(&directory(&task), 0).exists());
        f.mode.store(5, Ordering::Relaxed);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        assert!(matches!(check_resume(&client, &task, &RequestHeaders::new()).await.unwrap(), Mp4ResumeCheck::RequiresRestartConfirmation { .. }));
        assert!(run_test(&f, &task, CancellationToken::new()).await.is_err());
    }
}
