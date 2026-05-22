use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{serve, Router};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, Notify};
use tokio_util::io::ReaderStream;

use crate::downloader;
use crate::error::AppError;
use crate::models::HlsMediaKind;
use crate::models::{
    DownloadId, DownloadStatus, DownloadTask, FileType, LiveProtocol, LiveRecordStatus,
    LiveRecordTask, PlaybackSourceKind,
};

pub const PLAYBACK_PRIORITY_WINDOW_SIZE: usize = 4;

#[derive(Debug, Clone)]
pub struct PlaybackServerState {
    pub base_url: String,
}

#[derive(Debug, Clone)]
pub struct PlaybackSession {
    pub task_id: DownloadId,
    pub session_token: String,
    pub window_label: String,
    pub playback_kind: PlaybackSourceKind,
    pub playback_path: String,
    pub is_live: bool,
    pub task_snapshot: DownloadTask,
    pub last_accessed_at: DateTime<Utc>,
    pub active_client_count: usize,
}

#[derive(Debug, Clone)]
pub struct LivePlaybackSession {
    pub session_token: String,
    pub window_label: String,
    pub playback_kind: PlaybackSourceKind,
    pub playback_path: String,
    pub is_live: bool,
    pub last_accessed_at: DateTime<Utc>,
}

#[derive(Debug)]
pub struct DownloadPriorityState {
    inner: Mutex<DownloadPriorityInner>,
    notify: Notify,
}

#[derive(Debug)]
struct DownloadPriorityInner {
    pending: VecDeque<usize>,
    in_progress: HashSet<usize>,
    high_priority_window: Vec<usize>,
}

#[derive(Clone)]
struct PlaybackHttpState {
    downloads: Arc<Mutex<HashMap<DownloadId, DownloadTask>>>,
    playback_sessions: Arc<Mutex<HashMap<DownloadId, PlaybackSession>>>,
    download_priorities: Arc<Mutex<HashMap<DownloadId, Arc<DownloadPriorityState>>>>,
    live_records: Arc<Mutex<HashMap<DownloadId, LiveRecordTask>>>,
    live_playback_sessions: Arc<Mutex<HashMap<DownloadId, LivePlaybackSession>>>,
}

#[derive(Debug)]
struct SessionLease {
    task_id: DownloadId,
    playback_sessions: Arc<Mutex<HashMap<DownloadId, PlaybackSession>>>,
}

#[derive(Debug)]
struct PlaybackHttpError {
    status: StatusCode,
    message: String,
}

#[derive(Debug, Deserialize)]
struct PlaybackTokenQuery {
    token: String,
}

impl DownloadPriorityState {
    pub fn new(
        total_segments: usize,
        completed_segment_indices: &[usize],
        failed_segment_indices: &[usize],
    ) -> Self {
        Self {
            inner: Mutex::new(DownloadPriorityInner {
                pending: build_pending_queue(
                    total_segments,
                    completed_segment_indices,
                    failed_segment_indices,
                ),
                in_progress: HashSet::new(),
                high_priority_window: Vec::new(),
            }),
            notify: Notify::new(),
        }
    }

    pub async fn reinitialize(
        &self,
        total_segments: usize,
        completed_segment_indices: &[usize],
        failed_segment_indices: &[usize],
    ) {
        let mut inner = self.inner.lock().await;
        inner.pending = build_pending_queue(
            total_segments,
            completed_segment_indices,
            failed_segment_indices,
        );
        let high_priority_window = inner.high_priority_window.clone();
        reorder_pending(&mut inner.pending, &high_priority_window);
        inner.in_progress.clear();
        self.notify.notify_waiters();
    }

    pub async fn take_next_segment(&self) -> Option<usize> {
        let mut inner = self.inner.lock().await;
        while let Some(segment_index) = inner.pending.pop_front() {
            if inner.in_progress.insert(segment_index) {
                return Some(segment_index);
            }
        }
        None
    }

    pub async fn mark_segment_completed(&self, segment_index: usize) {
        let mut inner = self.inner.lock().await;
        inner.in_progress.remove(&segment_index);
        self.notify.notify_waiters();
    }

    pub async fn mark_segment_skipped(&self, segment_index: usize) {
        let mut inner = self.inner.lock().await;
        inner.in_progress.remove(&segment_index);
        if let Some(position) = inner
            .pending
            .iter()
            .position(|value| *value == segment_index)
        {
            inner.pending.remove(position);
        }
        self.notify.notify_waiters();
    }

    pub async fn requeue_segment(&self, segment_index: usize) {
        let mut inner = self.inner.lock().await;
        inner.in_progress.remove(&segment_index);
        if inner.pending.contains(&segment_index) {
            self.notify.notify_waiters();
            return;
        }

        if inner.high_priority_window.contains(&segment_index) {
            inner.pending.push_front(segment_index);
        } else {
            inner.pending.push_back(segment_index);
        }
        let high_priority_window = inner.high_priority_window.clone();
        reorder_pending(&mut inner.pending, &high_priority_window);
        self.notify.notify_waiters();
    }

    pub async fn prioritize_window(&self, start_segment_index: usize, total_segments: usize) {
        let mut inner = self.inner.lock().await;
        inner.high_priority_window = (start_segment_index
            ..(start_segment_index + PLAYBACK_PRIORITY_WINDOW_SIZE).min(total_segments))
            .collect::<Vec<_>>();
        let high_priority_window = inner.high_priority_window.clone();
        reorder_pending(&mut inner.pending, &high_priority_window);
        self.notify.notify_waiters();
    }

    #[cfg(test)]
    pub async fn pending_snapshot(&self) -> Vec<usize> {
        let inner = self.inner.lock().await;
        inner.pending.iter().copied().collect()
    }
}

impl SessionLease {
    async fn finish(self) {
        let mut sessions = self.playback_sessions.lock().await;
        if let Some(session) = sessions.get_mut(&self.task_id) {
            session.active_client_count = session.active_client_count.saturating_sub(1);
            session.last_accessed_at = Utc::now();
        }
    }
}

impl IntoResponse for PlaybackHttpError {
    fn into_response(self) -> Response {
        with_playback_headers((self.status, self.message).into_response())
    }
}

impl PlaybackHttpError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        let message = message.into();
        playback_log(&format!("http error status={} message={}", status, message));
        Self { status, message }
    }
}

pub async fn start_playback_server(
    downloads: Arc<Mutex<HashMap<DownloadId, DownloadTask>>>,
    playback_sessions: Arc<Mutex<HashMap<DownloadId, PlaybackSession>>>,
    download_priorities: Arc<Mutex<HashMap<DownloadId, Arc<DownloadPriorityState>>>>,
    live_records: Arc<Mutex<HashMap<DownloadId, LiveRecordTask>>>,
    live_playback_sessions: Arc<Mutex<HashMap<DownloadId, LivePlaybackSession>>>,
) -> Result<PlaybackServerState, AppError> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let local_addr = listener.local_addr()?;
    let state = PlaybackHttpState {
        downloads,
        playback_sessions,
        download_priorities,
        live_records,
        live_playback_sessions,
    };

    playback_log(&format!(
        "starting local playback server at 127.0.0.1:{}",
        local_addr.port()
    ));
    let app = build_playback_router(state);

    tauri::async_runtime::spawn(async move {
        if let Err(error) = serve(listener, app).await {
            eprintln!("[m3u8quicker] playback server stopped: {}", error);
        }
    });

    Ok(PlaybackServerState {
        base_url: format!("http://127.0.0.1:{}", local_addr.port()),
    })
}

fn build_playback_router(state: PlaybackHttpState) -> Router {
    Router::new()
        .route("/playback/{task_id}/index.m3u8", get(serve_playlist))
        .route("/playback/{task_id}/file", get(serve_file))
        .route("/playback/{task_id}/stream", get(serve_download_ts_stream))
        .route(
            "/playback/{task_id}/init/{init_index}",
            get(serve_init_segment),
        )
        .route(
            "/playback/{task_id}/segments/{segment_index}",
            get(serve_segment),
        )
        .route("/live/{task_id}/index.m3u8", get(serve_live_playlist))
        .route("/live/{task_id}/seg/{name}", get(serve_live_segment))
        .route("/live/{task_id}/stream", get(serve_live_flv_stream))
        .route("/live/{task_id}/file", get(serve_live_flv_file))
        .with_state(state)
}

pub async fn ensure_download_priority_state(
    download_priorities: &Arc<Mutex<HashMap<DownloadId, Arc<DownloadPriorityState>>>>,
    task: &DownloadTask,
) -> Arc<DownloadPriorityState> {
    let existing = {
        let priorities = download_priorities.lock().await;
        priorities.get(&task.id).cloned()
    };

    if let Some(priority_state) = existing {
        playback_log(&format!(
            "reuse download priority state task_id={} total_segments={}",
            task.id, task.total_segments
        ));
        return priority_state;
    }

    let priority_state = Arc::new(DownloadPriorityState::new(
        task.total_segments,
        &task.completed_segment_indices,
        &task.failed_segment_indices,
    ));
    let mut priorities = download_priorities.lock().await;
    priorities.insert(task.id.clone(), priority_state.clone());
    playback_log(&format!(
        "create download priority state task_id={} total_segments={} completed={}",
        task.id,
        task.total_segments,
        task.completed_segment_indices.len()
    ));
    priority_state
}

pub async fn prepare_download_priority_state(
    download_priorities: &Arc<Mutex<HashMap<DownloadId, Arc<DownloadPriorityState>>>>,
    task_id: &str,
    total_segments: usize,
    completed_segment_indices: &[usize],
    failed_segment_indices: &[usize],
) -> Arc<DownloadPriorityState> {
    let existing = {
        let priorities = download_priorities.lock().await;
        priorities.get(task_id).cloned()
    };

    if let Some(priority_state) = existing {
        priority_state
            .reinitialize(
                total_segments,
                completed_segment_indices,
                failed_segment_indices,
            )
            .await;
        playback_log(&format!(
            "reset download priority state task_id={} total_segments={} completed={}",
            task_id,
            total_segments,
            completed_segment_indices.len()
        ));
        return priority_state;
    }

    let priority_state = Arc::new(DownloadPriorityState::new(
        total_segments,
        completed_segment_indices,
        failed_segment_indices,
    ));
    let mut priorities = download_priorities.lock().await;
    priorities.insert(task_id.to_string(), priority_state.clone());
    playback_log(&format!(
        "prepare new download priority state task_id={} total_segments={} completed={}",
        task_id,
        total_segments,
        completed_segment_indices.len()
    ));
    priority_state
}

pub async fn remove_download_priority_state(
    download_priorities: &Arc<Mutex<HashMap<DownloadId, Arc<DownloadPriorityState>>>>,
    task_id: &str,
) {
    let mut priorities = download_priorities.lock().await;
    let existed = priorities.remove(task_id).is_some();
    playback_log(&format!(
        "remove download priority state task_id={} existed={}",
        task_id, existed
    ));
}

pub async fn has_active_playback_session(
    playback_sessions: &Arc<Mutex<HashMap<DownloadId, PlaybackSession>>>,
    task_id: &str,
) -> bool {
    let sessions = playback_sessions.lock().await;
    sessions.contains_key(task_id)
}

pub async fn remove_playback_session(
    playback_sessions: &Arc<Mutex<HashMap<DownloadId, PlaybackSession>>>,
    task_id: &str,
) -> Option<PlaybackSession> {
    let mut sessions = playback_sessions.lock().await;
    let removed = sessions.remove(task_id);
    playback_log(&format!(
        "remove playback session task_id={} existed={}",
        task_id,
        removed.is_some()
    ));
    removed
}

pub async fn remove_live_playback_session(
    live_playback_sessions: &Arc<Mutex<HashMap<DownloadId, LivePlaybackSession>>>,
    task_id: &str,
) -> Option<LivePlaybackSession> {
    let mut sessions = live_playback_sessions.lock().await;
    let removed = sessions.remove(task_id);
    playback_log(&format!(
        "remove live playback session task_id={} existed={}",
        task_id,
        removed.is_some()
    ));
    removed
}

pub fn playback_window_label(task_id: &str) -> String {
    format!("player-{}", task_id)
}

pub fn task_id_from_window_label(label: &str) -> Option<&str> {
    label
        .strip_prefix("player-")
        .filter(|task_id| !task_id.is_empty())
}

pub fn playlist_path(task_id: &str) -> String {
    format!("/playback/{}/index.m3u8", task_id)
}

pub fn file_path(task_id: &str) -> String {
    format!("/playback/{}/file", task_id)
}

pub fn download_stream_path(task_id: &str) -> String {
    format!("/playback/{}/stream", task_id)
}

pub fn live_playback_window_label(task_id: &str) -> String {
    format!("live-player-{}", task_id)
}

pub fn task_id_from_live_window_label(label: &str) -> Option<&str> {
    label
        .strip_prefix("live-player-")
        .filter(|task_id| !task_id.is_empty())
}

pub fn live_playlist_path(task_id: &str) -> String {
    format!("/live/{}/index.m3u8", task_id)
}

pub fn live_flv_stream_path(task_id: &str) -> String {
    format!("/live/{}/stream", task_id)
}

pub fn live_flv_file_path(task_id: &str) -> String {
    format!("/live/{}/file", task_id)
}

pub fn task_can_open_playback(task: &DownloadTask) -> bool {
    if !task.playback_available {
        return false;
    }

    matches!(
        task.status,
        DownloadStatus::Downloading | DownloadStatus::Paused | DownloadStatus::Completed
    )
}

pub fn segment_index_for_position(segment_durations: &[f32], position_secs: f64) -> usize {
    if segment_durations.is_empty() {
        return 0;
    }

    let target = position_secs.max(0.0);
    let mut elapsed = 0.0f64;

    for (index, duration) in segment_durations.iter().enumerate() {
        let next = elapsed + (*duration).max(0.0) as f64;
        if target < next || index == segment_durations.len() - 1 {
            return index;
        }
        elapsed = next;
    }

    segment_durations.len().saturating_sub(1)
}

pub async fn prioritize_download_position(
    download_priorities: &Arc<Mutex<HashMap<DownloadId, Arc<DownloadPriorityState>>>>,
    task: &DownloadTask,
    position_secs: f64,
) -> Result<(), AppError> {
    if task.segment_durations.is_empty() || task.segment_durations.len() != task.total_segments {
        return Err(AppError::InvalidInput(
            "当前任务缺少可播放的切片时长信息".to_string(),
        ));
    }

    let priority_state = ensure_download_priority_state(download_priorities, task).await;
    let segment_index = segment_index_for_position(&task.segment_durations, position_secs);
    playback_log(&format!(
        "prioritize playback position task_id={} position_secs={:.3} segment_index={}",
        task.id, position_secs, segment_index
    ));
    priority_state
        .prioritize_window(segment_index, task.total_segments)
        .await;
    Ok(())
}

pub fn build_playlist(task: &DownloadTask, token: &str) -> Result<String, AppError> {
    if task.segment_durations.len() != task.total_segments {
        return Err(AppError::InvalidInput(
            "当前任务缺少完整的切片时长信息".to_string(),
        ));
    }
    if task.hls_media_kind == HlsMediaKind::Fmp4
        && task.segment_init_indices.len() != task.total_segments
    {
        return Err(AppError::InvalidInput(
            "当前任务缺少完整的 fMP4 初始化片段信息".to_string(),
        ));
    }

    let target_duration = task
        .segment_durations
        .iter()
        .fold(1u32, |max_duration, duration| {
            max_duration.max(duration.ceil().max(1.0) as u32)
        });

    let mut lines = Vec::with_capacity(task.total_segments * 2 + 6);
    lines.push("#EXTM3U".to_string());
    lines.push(match task.hls_media_kind {
        HlsMediaKind::MpegTs => "#EXT-X-VERSION:3".to_string(),
        HlsMediaKind::Fmp4 => "#EXT-X-VERSION:6".to_string(),
    });
    lines.push(format!("#EXT-X-TARGETDURATION:{}", target_duration));
    lines.push("#EXT-X-MEDIA-SEQUENCE:0".to_string());
    // Even while downloading, the app already knows the full segment list up front.
    // Treat the playback manifest as VOD so HLS clients start at the beginning
    // instead of jumping to the live edge of an EVENT playlist.
    lines.push("#EXT-X-PLAYLIST-TYPE:VOD".to_string());
    lines.push("#EXT-X-START:TIME-OFFSET=0,PRECISE=YES".to_string());

    let mut last_init_index = None;
    for (segment_index, duration) in task.segment_durations.iter().enumerate() {
        if task.hls_media_kind == HlsMediaKind::Fmp4 {
            let init_index = task
                .segment_init_indices
                .get(segment_index)
                .copied()
                .flatten()
                .ok_or_else(|| {
                    AppError::InvalidInput("当前 fMP4 切片缺少 EXT-X-MAP".to_string())
                })?;
            if Some(init_index) != last_init_index {
                lines.push(format!(
                    "#EXT-X-MAP:URI=\"init/{}?token={}\"",
                    init_index, token
                ));
                last_init_index = Some(init_index);
            }
        }
        lines.push(format!("#EXTINF:{:.3},", duration));
        lines.push(format!("segments/{}?token={}", segment_index, token));
    }

    lines.push("#EXT-X-ENDLIST".to_string());

    Ok(lines.join("\n"))
}

async fn serve_playlist(
    State(state): State<PlaybackHttpState>,
    Path(task_id): Path<String>,
    Query(query): Query<PlaybackTokenQuery>,
) -> Response {
    playback_log(&format!(
        "playlist request task_id={} token_suffix={}",
        task_id,
        token_suffix(&query.token)
    ));
    let (lease, task) = match acquire_session_task(&state, &task_id, &query.token).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };

    let response = match build_playlist(&task, &query.token) {
        Ok(playlist) => with_playback_headers(
            (
                [(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/vnd.apple.mpegurl"),
                )],
                playlist,
            )
                .into_response(),
        ),
        Err(error) => {
            PlaybackHttpError::new(StatusCode::BAD_REQUEST, error.to_string()).into_response()
        }
    };
    playback_log(&format!(
        "playlist served task_id={} status={:?} segments={}",
        task.id, task.status, task.total_segments
    ));

    lease.finish().await;
    response
}

async fn serve_file(
    State(state): State<PlaybackHttpState>,
    Path(task_id): Path<String>,
    Query(query): Query<PlaybackTokenQuery>,
    headers: HeaderMap,
) -> Response {
    playback_log(&format!(
        "file request task_id={} token_suffix={}",
        task_id,
        token_suffix(&query.token)
    ));
    let (lease, task) = match acquire_session_task(&state, &task_id, &query.token).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };

    let response = match build_file_response(&task, &headers).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    };
    playback_log(&format!(
        "file response task_id={} current_status={:?}",
        task.id, task.status
    ));

    lease.finish().await;
    response
}

async fn serve_init_segment(
    State(state): State<PlaybackHttpState>,
    Path((task_id, init_index)): Path<(String, usize)>,
    Query(query): Query<PlaybackTokenQuery>,
) -> Response {
    playback_log(&format!(
        "init segment request task_id={} init_index={} token_suffix={}",
        task_id,
        init_index,
        token_suffix(&query.token)
    ));
    let (lease, task) = match acquire_session_task(&state, &task_id, &query.token).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };

    if task.hls_media_kind != HlsMediaKind::Fmp4
        || !task
            .hls_init_segments
            .iter()
            .any(|init| init.index == init_index)
    {
        lease.finish().await;
        return PlaybackHttpError::new(StatusCode::NOT_FOUND, "初始化片段不存在").into_response();
    }

    let response =
        match read_or_wait_for_init_segment(&state, &task, &query.token, init_index).await {
            Ok(bytes) => with_playback_headers(
                (
                    [(header::CONTENT_TYPE, HeaderValue::from_static("video/mp4"))],
                    bytes,
                )
                    .into_response(),
            ),
            Err(error) => error.into_response(),
        };
    playback_log(&format!(
        "init segment response task_id={} init_index={} current_status={:?}",
        task.id, init_index, task.status
    ));

    lease.finish().await;
    response
}

async fn serve_segment(
    State(state): State<PlaybackHttpState>,
    Path((task_id, segment_index)): Path<(String, usize)>,
    Query(query): Query<PlaybackTokenQuery>,
) -> Response {
    playback_log(&format!(
        "segment request task_id={} segment_index={} token_suffix={}",
        task_id,
        segment_index,
        token_suffix(&query.token)
    ));
    let (lease, task) = match acquire_session_task(&state, &task_id, &query.token).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };

    if segment_index >= task.total_segments {
        lease.finish().await;
        return PlaybackHttpError::new(StatusCode::NOT_FOUND, "切片不存在").into_response();
    }

    let response = match read_or_wait_for_segment(&state, &task, &query.token, segment_index).await
    {
        Ok(bytes) => with_playback_headers(
            (
                [(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static(match task.hls_media_kind {
                        HlsMediaKind::MpegTs => "video/mp2t",
                        HlsMediaKind::Fmp4 => "video/iso.segment",
                    }),
                )],
                bytes,
            )
                .into_response(),
        ),
        Err(error) => error.into_response(),
    };
    playback_log(&format!(
        "segment response task_id={} segment_index={} current_status={:?}",
        task.id, segment_index, task.status
    ));

    lease.finish().await;
    response
}

struct DownloadTsStreamState {
    state: PlaybackHttpState,
    task: DownloadTask,
    token: String,
    next_index: usize,
}

/// Stream a download's MPEG-TS segments concatenated into a single continuous
/// TS byte stream, in playback order. Used for HEVC content played via
/// mpegts.js (which consumes a single stream rather than an HLS playlist).
/// Each segment is fetched through the same priority/wait path as the HLS
/// route, so it follows the download and waits for not-yet-downloaded
/// segments. The stream ends once every segment has been served or the
/// session is closed / the task is cancelled.
async fn serve_download_ts_stream(
    State(state): State<PlaybackHttpState>,
    Path(task_id): Path<String>,
    Query(query): Query<PlaybackTokenQuery>,
) -> Response {
    playback_log(&format!(
        "download ts stream request task_id={} token_suffix={}",
        task_id,
        token_suffix(&query.token)
    ));
    let (lease, task) = match acquire_session_task(&state, &task_id, &query.token).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    // The stream outlives this handler; per-segment waits re-validate the
    // session, so we release the lease immediately instead of holding it for
    // the whole playback.
    lease.finish().await;

    let init = DownloadTsStreamState {
        state: state.clone(),
        task,
        token: query.token.clone(),
        next_index: 0,
    };

    let stream = futures::stream::unfold(init, |mut st| async move {
        if st.next_index >= st.task.total_segments {
            return None;
        }
        let segment_index = st.next_index;
        match read_or_wait_for_segment(&st.state, &st.task, &st.token, segment_index).await {
            Ok(bytes) => {
                st.next_index += 1;
                Some((Ok::<Bytes, std::io::Error>(bytes), st))
            }
            Err(error) => {
                playback_log(&format!(
                    "download ts stream stopped task_id={} segment_index={} status={}",
                    st.task.id, segment_index, error.status
                ));
                None
            }
        }
    });

    let body = Body::from_stream(stream);
    let mut response = Response::new(body);
    {
        let response_headers = response.headers_mut();
        response_headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp2t"));
        response_headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("none"));
    }
    with_playback_headers(response)
}

async fn acquire_live_session(
    state: &PlaybackHttpState,
    task_id: &str,
    token: &str,
) -> Result<LiveRecordTask, PlaybackHttpError> {
    {
        let mut sessions = state.live_playback_sessions.lock().await;
        let Some(session) = sessions.get_mut(task_id) else {
            playback_log(&format!(
                "reject live playback request because session missing task_id={}",
                task_id
            ));
            return Err(PlaybackHttpError::new(
                StatusCode::NOT_FOUND,
                "播放会话不存在",
            ));
        };
        if session.session_token != token {
            playback_log(&format!(
                "reject live playback request because token invalid task_id={}",
                task_id
            ));
            return Err(PlaybackHttpError::new(
                StatusCode::FORBIDDEN,
                "播放会话令牌无效",
            ));
        }
        session.last_accessed_at = Utc::now();
    }

    let task = {
        let map = state.live_records.lock().await;
        map.get(task_id).cloned()
    };
    task.ok_or_else(|| PlaybackHttpError::new(StatusCode::NOT_FOUND, "直播任务不存在"))
}

/// HLS only: the directory currently holding `index.m3u8` + segments.
/// While recording it is the working `temp_dir`; once finished it is the
/// parent directory of the finalized `file_path`.
fn live_active_dir(task: &LiveRecordTask) -> Option<PathBuf> {
    match task.status {
        LiveRecordStatus::Recording | LiveRecordStatus::Paused => {
            task.temp_dir.as_ref().map(PathBuf::from)
        }
        _ => task
            .file_path
            .as_ref()
            .map(PathBuf::from)
            .and_then(|path| path.parent().map(|parent| parent.to_path_buf())),
    }
}

fn live_is_recording(task: &LiveRecordTask) -> bool {
    matches!(
        task.status,
        LiveRecordStatus::Recording | LiveRecordStatus::Paused
    )
}

/// Rewrite the on-disk local playlist for serving:
/// - segment + EXT-X-MAP URIs get the session token and a `seg/` prefix
/// - while recording it is served as a growing EVENT playlist (no ENDLIST) so
///   hls.js keeps reloading and follows new segments; once finished it is VOD
///   with ENDLIST so the whole recording is seekable.
fn rewrite_live_playlist(text: &str, token: &str, recording: bool) -> String {
    let mut body: Vec<String> = Vec::new();
    for raw in text.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed == "#EXTM3U" {
            continue;
        }
        if trimmed.starts_with("#EXT-X-PLAYLIST-TYPE:") {
            continue;
        }
        if trimmed.eq_ignore_ascii_case("#EXT-X-ENDLIST") {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("#EXT-X-MAP:URI=\"") {
            if let Some(end) = rest.find('"') {
                let name = &rest[..end];
                let tail = &rest[end + 1..];
                body.push(format!(
                    "#EXT-X-MAP:URI=\"seg/{}?token={}\"{}",
                    name, token, tail
                ));
                continue;
            }
        }
        if trimmed.starts_with('#') {
            body.push(trimmed.to_string());
            continue;
        }
        body.push(format!("seg/{}?token={}", trimmed, token));
    }

    let mut lines: Vec<String> = Vec::with_capacity(body.len() + 4);
    lines.push("#EXTM3U".to_string());
    if recording {
        lines.push("#EXT-X-PLAYLIST-TYPE:EVENT".to_string());
        lines.push("#EXT-X-START:TIME-OFFSET=0,PRECISE=YES".to_string());
    } else {
        lines.push("#EXT-X-PLAYLIST-TYPE:VOD".to_string());
    }
    lines.extend(body);
    if !recording {
        lines.push("#EXT-X-ENDLIST".to_string());
    }
    lines.join("\n")
}

fn is_valid_live_segment_name(name: &str) -> bool {
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    let valid = |prefix: &str, exts: &[&str]| {
        lower
            .strip_prefix(prefix)
            .and_then(|rest| rest.split_once('.'))
            .map(|(digits, ext)| {
                !digits.is_empty()
                    && digits.bytes().all(|byte| byte.is_ascii_digit())
                    && exts.contains(&ext)
            })
            .unwrap_or(false)
    };
    valid("seg_", &["ts", "m4s"]) || valid("init_", &["mp4"])
}

fn live_segment_content_type(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".ts") {
        "video/mp2t"
    } else if lower.ends_with(".m4s") {
        "video/iso.segment"
    } else if lower.ends_with(".mp4") {
        "video/mp4"
    } else {
        "application/octet-stream"
    }
}

async fn serve_live_playlist(
    State(state): State<PlaybackHttpState>,
    Path(task_id): Path<String>,
    Query(query): Query<PlaybackTokenQuery>,
) -> Response {
    playback_log(&format!(
        "live playlist request task_id={} token_suffix={}",
        task_id,
        token_suffix(&query.token)
    ));
    let task = match acquire_live_session(&state, &task_id, &query.token).await {
        Ok(task) => task,
        Err(error) => return error.into_response(),
    };
    if task.protocol != LiveProtocol::Hls {
        return PlaybackHttpError::new(StatusCode::CONFLICT, "当前直播任务不是 HLS")
            .into_response();
    }
    let Some(dir) = live_active_dir(&task) else {
        return PlaybackHttpError::new(StatusCode::NOT_FOUND, "直播录制目录不存在").into_response();
    };
    let playlist_path = dir.join("index.m3u8");
    let text = match tokio::fs::read_to_string(&playlist_path).await {
        Ok(text) => text,
        Err(error) => {
            return PlaybackHttpError::new(StatusCode::NOT_FOUND, error.to_string()).into_response()
        }
    };
    let playlist = rewrite_live_playlist(&text, &query.token, live_is_recording(&task));
    with_playback_headers(
        (
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/vnd.apple.mpegurl"),
            )],
            playlist,
        )
            .into_response(),
    )
}

async fn serve_live_segment(
    State(state): State<PlaybackHttpState>,
    Path((task_id, name)): Path<(String, String)>,
    Query(query): Query<PlaybackTokenQuery>,
) -> Response {
    let task = match acquire_live_session(&state, &task_id, &query.token).await {
        Ok(task) => task,
        Err(error) => return error.into_response(),
    };
    if !is_valid_live_segment_name(&name) {
        return PlaybackHttpError::new(StatusCode::BAD_REQUEST, "非法的分片名称").into_response();
    }
    let Some(dir) = live_active_dir(&task) else {
        return PlaybackHttpError::new(StatusCode::NOT_FOUND, "直播录制目录不存在").into_response();
    };
    let segment_path = dir.join(&name);
    match tokio::fs::read(&segment_path).await {
        Ok(bytes) => with_playback_headers(
            (
                [(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static(live_segment_content_type(&name)),
                )],
                bytes,
            )
                .into_response(),
        ),
        Err(error) => {
            PlaybackHttpError::new(StatusCode::NOT_FOUND, error.to_string()).into_response()
        }
    }
}

async fn serve_live_flv_file(
    State(state): State<PlaybackHttpState>,
    Path(task_id): Path<String>,
    Query(query): Query<PlaybackTokenQuery>,
    headers: HeaderMap,
) -> Response {
    let task = match acquire_live_session(&state, &task_id, &query.token).await {
        Ok(task) => task,
        Err(error) => return error.into_response(),
    };
    if task.protocol != LiveProtocol::Flv {
        return PlaybackHttpError::new(StatusCode::CONFLICT, "当前直播任务不是 FLV")
            .into_response();
    }
    let Some(path) = task.file_path.as_ref().map(PathBuf::from) else {
        return PlaybackHttpError::new(StatusCode::NOT_FOUND, "录制文件不存在").into_response();
    };
    match build_ranged_file_response(&path, "video/x-flv", &headers).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn live_session_token_matches(
    live_playback_sessions: &Arc<Mutex<HashMap<DownloadId, LivePlaybackSession>>>,
    task_id: &str,
    token: &str,
) -> bool {
    let sessions = live_playback_sessions.lock().await;
    sessions
        .get(task_id)
        .map(|session| session.session_token == token)
        .unwrap_or(false)
}

async fn live_record_finished(
    live_records: &Arc<Mutex<HashMap<DownloadId, LiveRecordTask>>>,
    task_id: &str,
) -> bool {
    let map = live_records.lock().await;
    match map.get(task_id) {
        Some(task) => matches!(
            task.status,
            LiveRecordStatus::Recorded
                | LiveRecordStatus::Failed(_)
                | LiveRecordStatus::Cancelled
        ),
        None => true,
    }
}

struct FlvTailState {
    file: File,
    task_id: DownloadId,
    token: String,
    live_records: Arc<Mutex<HashMap<DownloadId, LiveRecordTask>>>,
    live_playback_sessions: Arc<Mutex<HashMap<DownloadId, LivePlaybackSession>>>,
}

/// Stream an FLV file that is still being appended to during recording. Reads
/// what is present, then keeps polling the growing file until the recording
/// finishes (or the playback session is closed), at which point the stream ends.
async fn serve_live_flv_stream(
    State(state): State<PlaybackHttpState>,
    Path(task_id): Path<String>,
    Query(query): Query<PlaybackTokenQuery>,
) -> Response {
    playback_log(&format!(
        "live flv stream request task_id={} token_suffix={}",
        task_id,
        token_suffix(&query.token)
    ));
    let task = match acquire_live_session(&state, &task_id, &query.token).await {
        Ok(task) => task,
        Err(error) => return error.into_response(),
    };
    if task.protocol != LiveProtocol::Flv {
        return PlaybackHttpError::new(StatusCode::CONFLICT, "当前直播任务不是 FLV")
            .into_response();
    }
    let Some(path) = task.file_path.as_ref().map(PathBuf::from) else {
        return PlaybackHttpError::new(StatusCode::NOT_FOUND, "录制文件不存在").into_response();
    };
    let file = match File::open(&path).await {
        Ok(file) => file,
        Err(error) => {
            return PlaybackHttpError::new(StatusCode::NOT_FOUND, error.to_string()).into_response()
        }
    };

    let init = FlvTailState {
        file,
        task_id: task_id.clone(),
        token: query.token.clone(),
        live_records: state.live_records.clone(),
        live_playback_sessions: state.live_playback_sessions.clone(),
    };

    let stream = futures::stream::unfold(init, |mut st| async move {
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            match st.file.read(&mut buffer).await {
                Ok(0) => {
                    if !live_session_token_matches(
                        &st.live_playback_sessions,
                        &st.task_id,
                        &st.token,
                    )
                    .await
                    {
                        return None;
                    }
                    if live_record_finished(&st.live_records, &st.task_id).await {
                        // Recording is done — flush any final bytes, then end.
                        match st.file.read(&mut buffer).await {
                            Ok(n) if n > 0 => {
                                let chunk = Bytes::copy_from_slice(&buffer[..n]);
                                return Some((Ok::<Bytes, std::io::Error>(chunk), st));
                            }
                            _ => return None,
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
                Ok(n) => {
                    let chunk = Bytes::copy_from_slice(&buffer[..n]);
                    return Some((Ok::<Bytes, std::io::Error>(chunk), st));
                }
                Err(error) => {
                    return Some((Err(error), st));
                }
            }
        }
    });

    let body = Body::from_stream(stream);
    let mut response = Response::new(body);
    {
        let response_headers = response.headers_mut();
        response_headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("video/x-flv"));
        response_headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("none"));
    }
    with_playback_headers(response)
}

async fn build_ranged_file_response(
    path: &FsPath,
    content_type: &'static str,
    headers: &HeaderMap,
) -> Result<Response, PlaybackHttpError> {
    let mut file = File::open(path)
        .await
        .map_err(|error| PlaybackHttpError::new(StatusCode::NOT_FOUND, error.to_string()))?;
    let file_size = file
        .metadata()
        .await
        .map_err(|error| {
            PlaybackHttpError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
        })?
        .len();
    if file_size == 0 {
        return Err(PlaybackHttpError::new(
            StatusCode::NOT_FOUND,
            "录制文件为空",
        ));
    }

    let (start, end, status) = match parse_byte_range(headers, file_size)? {
        Some((start, end)) => (start, end, StatusCode::PARTIAL_CONTENT),
        None => (0, file_size - 1, StatusCode::OK),
    };
    let content_length = end - start + 1;

    file.seek(SeekFrom::Start(start)).await.map_err(|error| {
        PlaybackHttpError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
    })?;

    let stream = ReaderStream::new(file.take(content_length));
    let body = Body::from_stream(stream);
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let response_headers = response.headers_mut();
    response_headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response_headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response_headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&content_length.to_string()).map_err(|error| {
            PlaybackHttpError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
        })?,
    );
    if status == StatusCode::PARTIAL_CONTENT {
        response_headers.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {}-{}/{}", start, end, file_size)).map_err(
                |error| {
                    PlaybackHttpError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
                },
            )?,
        );
    }

    Ok(with_playback_headers(response))
}

async fn build_file_response(
    task: &DownloadTask,
    headers: &HeaderMap,
) -> Result<Response, PlaybackHttpError> {
    let path = playback_file_path_for_task(task)?;
    let mut file = File::open(&path)
        .await
        .map_err(|error| PlaybackHttpError::new(StatusCode::NOT_FOUND, error.to_string()))?;
    let file_size = file
        .metadata()
        .await
        .map_err(|error| {
            PlaybackHttpError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
        })?
        .len();
    if file_size == 0 {
        let message = if matches!(task.status, DownloadStatus::Completed) {
            "下载完成文件为空"
        } else {
            "当前任务尚未生成可播放数据"
        };
        return Err(PlaybackHttpError::new(StatusCode::NOT_FOUND, message));
    }

    let (start, end, status) = match parse_byte_range(headers, file_size)? {
        Some((start, end)) => (start, end, StatusCode::PARTIAL_CONTENT),
        None => (0, file_size - 1, StatusCode::OK),
    };
    let content_length = end - start + 1;

    file.seek(SeekFrom::Start(start)).await.map_err(|error| {
        PlaybackHttpError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
    })?;

    let stream = ReaderStream::new(file.take(content_length));
    let body = Body::from_stream(stream);
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let response_headers = response.headers_mut();
    response_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type_for_file_path(&task.filename)),
    );
    response_headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response_headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&content_length.to_string()).map_err(|error| {
            PlaybackHttpError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
        })?,
    );
    if status == StatusCode::PARTIAL_CONTENT {
        response_headers.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {}-{}/{}", start, end, file_size)).map_err(
                |error| {
                    PlaybackHttpError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
                },
            )?,
        );
    }

    Ok(with_playback_headers(response))
}

fn playback_file_path_for_task(task: &DownloadTask) -> Result<PathBuf, PlaybackHttpError> {
    match task.status {
        DownloadStatus::Completed => {
            let file_path = task.file_path.as_ref().ok_or_else(|| {
                PlaybackHttpError::new(StatusCode::NOT_FOUND, "下载完成文件不存在")
            })?;
            let path = PathBuf::from(file_path);
            if !path.is_file() {
                return Err(PlaybackHttpError::new(
                    StatusCode::NOT_FOUND,
                    "下载完成文件不存在",
                ));
            }
            Ok(path)
        }
        DownloadStatus::Downloading | DownloadStatus::Paused
            if task.file_type.supports_progressive_playback() =>
        {
            let partial_path = downloader::existing_mp4_partial_path(
                FsPath::new(&task.output_dir),
                &task.filename,
            )
            .ok_or_else(|| {
                PlaybackHttpError::new(StatusCode::CONFLICT, "当前任务尚未生成可播放文件")
            })?;

            if !partial_path.is_file() {
                return Err(PlaybackHttpError::new(
                    StatusCode::NOT_FOUND,
                    "当前任务尚未生成可播放文件",
                ));
            }

            Ok(partial_path)
        }
        DownloadStatus::Downloading | DownloadStatus::Paused => Err(PlaybackHttpError::new(
            StatusCode::CONFLICT,
            "当前格式暂不支持边下边播",
        )),
        _ => Err(PlaybackHttpError::new(
            StatusCode::CONFLICT,
            "当前任务尚未生成最终播放文件",
        )),
    }
}

async fn read_or_wait_for_segment(
    state: &PlaybackHttpState,
    task: &DownloadTask,
    token: &str,
    segment_index: usize,
) -> Result<Bytes, PlaybackHttpError> {
    let temp_dir = downloader::temp_dir_for_task(FsPath::new(&task.output_dir), &task.id);
    let segment_path = match task.hls_media_kind {
        HlsMediaKind::MpegTs => downloader::segment_file_path(&temp_dir, segment_index),
        HlsMediaKind::Fmp4 => downloader::fmp4_segment_file_path(&temp_dir, segment_index),
    };

    if let Ok(bytes) = tokio::fs::read(&segment_path).await {
        playback_log(&format!(
            "segment cache hit task_id={} segment_index={} path={}",
            task.id,
            segment_index,
            segment_path.display()
        ));
        return Ok(Bytes::from(bytes));
    }

    playback_log(&format!(
        "segment cache miss task_id={} segment_index={} path={}",
        task.id,
        segment_index,
        segment_path.display()
    ));

    if let Err(error) = prioritize_download_position(
        &state.download_priorities,
        task,
        total_duration_before(task, segment_index),
    )
    .await
    {
        return Err(PlaybackHttpError::new(
            StatusCode::BAD_REQUEST,
            error.to_string(),
        ));
    }

    let mut wait_round = 0usize;
    loop {
        if let Ok(bytes) = tokio::fs::read(&segment_path).await {
            playback_log(&format!(
                "segment became available task_id={} segment_index={} waited_rounds={}",
                task.id, segment_index, wait_round
            ));
            return Ok(Bytes::from(bytes));
        }

        {
            let sessions = state.playback_sessions.lock().await;
            let Some(session) = sessions.get(&task.id) else {
                playback_log(&format!(
                    "segment wait aborted because session missing task_id={} segment_index={}",
                    task.id, segment_index
                ));
                return Err(PlaybackHttpError::new(
                    StatusCode::NOT_FOUND,
                    "播放会话已关闭",
                ));
            };
            if session.session_token != token {
                playback_log(&format!(
                    "segment wait aborted because token mismatch task_id={} segment_index={}",
                    task.id, segment_index
                ));
                return Err(PlaybackHttpError::new(
                    StatusCode::FORBIDDEN,
                    "播放会话已失效",
                ));
            }
        }

        let task_state = {
            let downloads = state.downloads.lock().await;
            downloads.get(&task.id).cloned()
        };

        let Some(task_state) = task_state else {
            return Err(PlaybackHttpError::new(
                StatusCode::NOT_FOUND,
                "下载任务不存在",
            ));
        };

        match task_state.status {
            DownloadStatus::Cancelled => {
                playback_log(&format!(
                    "segment wait aborted because task cancelled task_id={} segment_index={}",
                    task.id, segment_index
                ));
                return Err(PlaybackHttpError::new(StatusCode::GONE, "下载任务已取消"));
            }
            DownloadStatus::Failed(message) => {
                playback_log(&format!(
                    "segment wait aborted because task failed task_id={} segment_index={} message={}",
                    task.id, segment_index, message
                ));
                return Err(PlaybackHttpError::new(StatusCode::CONFLICT, message));
            }
            DownloadStatus::Completed => {
                playback_log(&format!(
                    "segment wait aborted because task completed without segment task_id={} segment_index={}",
                    task.id, segment_index
                ));
                return Err(PlaybackHttpError::new(
                    StatusCode::NOT_FOUND,
                    "目标切片不可用",
                ));
            }
            _ => {}
        }

        if task_state
            .failed_segment_indices
            .contains(&(segment_index + 1))
        {
            playback_log(&format!(
                "segment wait aborted because segment skipped task_id={} segment_index={}",
                task.id, segment_index
            ));
            return Err(PlaybackHttpError::new(
                StatusCode::GONE,
                "目标切片多次下载失败，已跳过",
            ));
        }

        wait_round += 1;
        if wait_round == 1 || wait_round % 20 == 0 {
            playback_log(&format!(
                "waiting for segment task_id={} segment_index={} round={} task_status={:?}",
                task.id, segment_index, wait_round, task_state.status
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn read_or_wait_for_init_segment(
    state: &PlaybackHttpState,
    task: &DownloadTask,
    token: &str,
    init_index: usize,
) -> Result<Bytes, PlaybackHttpError> {
    let init_path = downloader::hls_init_segment_file_path(
        &downloader::temp_dir_for_task(FsPath::new(&task.output_dir), &task.id),
        init_index,
    );

    if let Ok(bytes) = tokio::fs::read(&init_path).await {
        playback_log(&format!(
            "init cache hit task_id={} init_index={} path={}",
            task.id,
            init_index,
            init_path.display()
        ));
        return Ok(Bytes::from(bytes));
    }

    playback_log(&format!(
        "init cache miss task_id={} init_index={} path={}",
        task.id,
        init_index,
        init_path.display()
    ));

    let mut wait_round = 0usize;
    loop {
        if let Ok(bytes) = tokio::fs::read(&init_path).await {
            playback_log(&format!(
                "init became available task_id={} init_index={} waited_rounds={}",
                task.id, init_index, wait_round
            ));
            return Ok(Bytes::from(bytes));
        }

        {
            let sessions = state.playback_sessions.lock().await;
            let Some(session) = sessions.get(&task.id) else {
                return Err(PlaybackHttpError::new(
                    StatusCode::NOT_FOUND,
                    "播放会话已关闭",
                ));
            };
            if session.session_token != token {
                return Err(PlaybackHttpError::new(
                    StatusCode::FORBIDDEN,
                    "播放会话已失效",
                ));
            }
        }

        let task_state = {
            let downloads = state.downloads.lock().await;
            downloads.get(&task.id).cloned()
        };

        let Some(task_state) = task_state else {
            return Err(PlaybackHttpError::new(
                StatusCode::NOT_FOUND,
                "下载任务不存在",
            ));
        };

        match task_state.status {
            DownloadStatus::Cancelled => {
                return Err(PlaybackHttpError::new(StatusCode::GONE, "下载任务已取消"));
            }
            DownloadStatus::Failed(message) => {
                return Err(PlaybackHttpError::new(StatusCode::CONFLICT, message));
            }
            DownloadStatus::Completed => {
                return Err(PlaybackHttpError::new(
                    StatusCode::NOT_FOUND,
                    "目标初始化片段不可用",
                ));
            }
            _ => {}
        }

        wait_round += 1;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn acquire_session_task(
    state: &PlaybackHttpState,
    task_id: &str,
    token: &str,
) -> Result<(SessionLease, DownloadTask), PlaybackHttpError> {
    let session_task = {
        let mut sessions = state.playback_sessions.lock().await;
        let Some(session) = sessions.get_mut(task_id) else {
            playback_log(&format!(
                "reject playback request because session missing task_id={}",
                task_id
            ));
            return Err(PlaybackHttpError::new(
                StatusCode::NOT_FOUND,
                "播放会话不存在",
            ));
        };

        if session.session_token != token {
            playback_log(&format!(
                "reject playback request because token invalid task_id={} provided={} expected={}",
                task_id,
                token_suffix(token),
                token_suffix(&session.session_token)
            ));
            return Err(PlaybackHttpError::new(
                StatusCode::FORBIDDEN,
                "播放会话令牌无效",
            ));
        }
        if session.task_id != task_id {
            playback_log(&format!(
                "reject playback request because task mismatch session_task_id={} request_task_id={}",
                session.task_id, task_id
            ));
            return Err(PlaybackHttpError::new(
                StatusCode::FORBIDDEN,
                "播放会话任务不匹配",
            ));
        }

        session.last_accessed_at = Utc::now();
        session.active_client_count += 1;
        playback_log(&format!(
            "lease playback session task_id={} active_clients={}",
            task_id, session.active_client_count
        ));
        session.task_snapshot.clone()
    };

    let task = {
        let downloads = state.downloads.lock().await;
        downloads
            .get(task_id)
            .cloned()
            .or(Some(session_task))
            .ok_or_else(|| PlaybackHttpError::new(StatusCode::NOT_FOUND, "下载任务不存在"))?
    };

    Ok((
        SessionLease {
            task_id: task_id.to_string(),
            playback_sessions: state.playback_sessions.clone(),
        },
        task,
    ))
}

fn build_pending_queue(
    total_segments: usize,
    completed_segment_indices: &[usize],
    failed_segment_indices: &[usize],
) -> VecDeque<usize> {
    let completed = completed_segment_indices
        .iter()
        .filter_map(|value| value.checked_sub(1))
        .collect::<HashSet<_>>();
    let failed = failed_segment_indices
        .iter()
        .filter_map(|value| value.checked_sub(1))
        .collect::<HashSet<_>>();

    (0..total_segments)
        .filter(|segment_index| {
            !completed.contains(segment_index) && !failed.contains(segment_index)
        })
        .collect::<VecDeque<_>>()
}

fn reorder_pending(pending: &mut VecDeque<usize>, high_priority_window: &[usize]) {
    if high_priority_window.is_empty() || pending.is_empty() {
        return;
    }

    let mut prioritized = VecDeque::new();
    for segment_index in high_priority_window {
        if let Some(position) = pending.iter().position(|value| value == segment_index) {
            if let Some(value) = pending.remove(position) {
                prioritized.push_back(value);
            }
        }
    }

    prioritized.append(pending);
    *pending = prioritized;
}

fn total_duration_before(task: &DownloadTask, segment_index: usize) -> f64 {
    task.segment_durations
        .iter()
        .take(segment_index)
        .map(|duration| *duration as f64)
        .sum()
}

fn parse_byte_range(
    headers: &HeaderMap,
    file_size: u64,
) -> Result<Option<(u64, u64)>, PlaybackHttpError> {
    let Some(range_header) = headers.get(header::RANGE) else {
        return Ok(None);
    };
    let range_header = range_header
        .to_str()
        .map_err(|error| PlaybackHttpError::new(StatusCode::BAD_REQUEST, error.to_string()))?;
    let Some(range_value) = range_header.strip_prefix("bytes=") else {
        return Err(PlaybackHttpError::new(
            StatusCode::RANGE_NOT_SATISFIABLE,
            "不支持的 Range 请求",
        ));
    };
    let Some((start_raw, end_raw)) = range_value.split_once('-') else {
        return Err(PlaybackHttpError::new(
            StatusCode::RANGE_NOT_SATISFIABLE,
            "无效的 Range 请求",
        ));
    };

    let parsed = if start_raw.is_empty() {
        let suffix_length = end_raw.parse::<u64>().map_err(|_| {
            PlaybackHttpError::new(StatusCode::RANGE_NOT_SATISFIABLE, "无效的 Range 请求")
        })?;
        if suffix_length == 0 {
            return Err(PlaybackHttpError::new(
                StatusCode::RANGE_NOT_SATISFIABLE,
                "无效的 Range 请求",
            ));
        }
        let start = file_size.saturating_sub(suffix_length);
        (start, file_size - 1)
    } else {
        let start = start_raw.parse::<u64>().map_err(|_| {
            PlaybackHttpError::new(StatusCode::RANGE_NOT_SATISFIABLE, "无效的 Range 请求")
        })?;
        let end = if end_raw.is_empty() {
            file_size - 1
        } else {
            end_raw.parse::<u64>().map_err(|_| {
                PlaybackHttpError::new(StatusCode::RANGE_NOT_SATISFIABLE, "无效的 Range 请求")
            })?
        };
        (start, end)
    };

    let (start, mut end) = parsed;
    if start >= file_size {
        return Err(PlaybackHttpError::new(
            StatusCode::RANGE_NOT_SATISFIABLE,
            "Range 超出文件大小",
        ));
    }
    end = end.min(file_size - 1);
    if end < start {
        return Err(PlaybackHttpError::new(
            StatusCode::RANGE_NOT_SATISFIABLE,
            "Range 起止位置无效",
        ));
    }

    Ok(Some((start, end)))
}

fn content_type_for_file_path(file_path: &str) -> &'static str {
    match FsPath::new(file_path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "mp4" => "video/mp4",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "wmv" => "video/x-ms-wmv",
        "flv" => "video/x-flv",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "rmvb" => "application/vnd.rn-realmedia-vbr",
        "ts" => "video/mp2t",
        _ => "application/octet-stream",
    }
}

const TS_PACKET_LEN: usize = 188;
/// MPEG-TS PMT stream_type value for HEVC/H.265 video.
const TS_STREAM_TYPE_HEVC: u8 = 0x24;
/// How many bytes from the front of a segment/file to inspect for codec info.
/// PAT/PMT appear within the first few KiB; 256 KiB is a generous margin.
const HEVC_PROBE_BYTES: usize = 256 * 1024;

/// Decide whether a download task's video should be played through mpegts.js
/// for H.265/HEVC compatibility. Only MPEG-TS content is eligible (mpegts.js
/// cannot consume fMP4 / MP4 / MKV …), so this returns `false` for any other
/// container and `true` only when an HEVC video elementary stream is detected.
pub async fn download_video_is_hevc(task: &DownloadTask) -> bool {
    let probe_path = match task.status {
        DownloadStatus::Completed => {
            let file_path = match task.file_path.as_deref() {
                Some(path) => PathBuf::from(path),
                None => return false,
            };
            let is_ts = file_path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("ts"))
                .unwrap_or(false);
            if !is_ts {
                return false;
            }
            file_path
        }
        DownloadStatus::Downloading | DownloadStatus::Paused => {
            if task.file_type != FileType::Hls || task.hls_media_kind != HlsMediaKind::MpegTs {
                return false;
            }
            match first_available_segment_path(task) {
                Some(path) => path,
                None => return false,
            }
        }
        _ => return false,
    };

    let head = read_file_head(&probe_path, HEVC_PROBE_BYTES).await;
    let is_hevc = matches!(ts_video_is_hevc(&head), Some(true));
    playback_log(&format!(
        "hevc probe task_id={} path={} bytes={} is_hevc={}",
        task.id,
        probe_path.display(),
        head.len(),
        is_hevc
    ));
    is_hevc
}

/// Locate an already-downloaded MPEG-TS segment to probe for codec info. Any
/// segment carries the PAT/PMT, so the lowest available index is fine.
fn first_available_segment_path(task: &DownloadTask) -> Option<PathBuf> {
    let temp_dir = downloader::temp_dir_for_task(FsPath::new(&task.output_dir), &task.id);

    let mut completed: Vec<usize> = task
        .completed_segment_indices
        .iter()
        .filter_map(|value| value.checked_sub(1))
        .collect();
    completed.sort_unstable();
    for segment_index in completed {
        let path = downloader::segment_file_path(&temp_dir, segment_index);
        if path.is_file() {
            return Some(path);
        }
    }

    let scan_limit = task.total_segments.min(64);
    for segment_index in 0..scan_limit {
        let path = downloader::segment_file_path(&temp_dir, segment_index);
        if path.is_file() {
            return Some(path);
        }
    }

    None
}

async fn read_file_head(path: &FsPath, max_bytes: usize) -> Vec<u8> {
    let mut file = match File::open(path).await {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };
    let mut buffer = Vec::with_capacity(max_bytes.min(64 * 1024));
    let mut chunk = vec![0u8; 64 * 1024];
    while buffer.len() < max_bytes {
        match file.read(&mut chunk).await {
            Ok(0) => break,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            Err(_) => break,
        }
    }
    buffer.truncate(max_bytes);
    buffer
}

/// Inspect an MPEG-TS byte slice and report whether its video elementary
/// stream is HEVC. `Some(true)` = HEVC present, `Some(false)` = a PMT was
/// parsed but no HEVC stream, `None` = the data could not be interpreted.
fn ts_video_is_hevc(data: &[u8]) -> Option<bool> {
    let base = ts_sync_offset(data)?;
    let mut pmt_pids: HashSet<u16> = HashSet::new();
    let mut parsed_pmt = false;

    let mut offset = base;
    while offset + TS_PACKET_LEN <= data.len() {
        let packet = &data[offset..offset + TS_PACKET_LEN];
        offset += TS_PACKET_LEN;
        if packet[0] != 0x47 {
            // Lost alignment; try to re-sync from the next byte.
            match ts_sync_offset(&data[offset - TS_PACKET_LEN + 1..]) {
                Some(next) => {
                    offset = offset - TS_PACKET_LEN + 1 + next;
                    continue;
                }
                None => break,
            }
        }

        let pusi = packet[1] & 0x40 != 0;
        let pid = (((packet[1] & 0x1F) as u16) << 8) | packet[2] as u16;
        let adaptation_field_control = (packet[3] >> 4) & 0x03;
        if adaptation_field_control == 0 || adaptation_field_control == 2 {
            continue; // no payload
        }
        let mut payload_start = 4;
        if adaptation_field_control == 3 {
            let adaptation_field_length = packet[4] as usize;
            payload_start = 5 + adaptation_field_length;
            if payload_start >= TS_PACKET_LEN {
                continue;
            }
        }
        let payload = &packet[payload_start..];

        if pid == 0x0000 {
            if let Some(section) = psi_section(payload, pusi) {
                collect_pat_pmt_pids(section, &mut pmt_pids);
            }
        } else if pmt_pids.contains(&pid) {
            if let Some(section) = psi_section(payload, pusi) {
                if let Some(is_hevc) = pmt_has_hevc(section) {
                    if is_hevc {
                        return Some(true);
                    }
                    parsed_pmt = true;
                }
            }
        }
    }

    if parsed_pmt {
        Some(false)
    } else {
        None
    }
}

/// Find the byte offset of the first MPEG-TS sync byte that lines up with the
/// 188-byte packet grid (verified across a few packets where possible).
fn ts_sync_offset(data: &[u8]) -> Option<usize> {
    let scan_limit = data.len().min(TS_PACKET_LEN);
    for start in 0..scan_limit {
        if data[start] != 0x47 {
            continue;
        }
        let mut aligned = true;
        let mut probe = start + TS_PACKET_LEN;
        for _ in 0..3 {
            if probe >= data.len() {
                break;
            }
            if data[probe] != 0x47 {
                aligned = false;
                break;
            }
            probe += TS_PACKET_LEN;
        }
        if aligned {
            return Some(start);
        }
    }
    None
}

/// Extract a PSI section from a TS packet payload, honoring the `pointer_field`
/// present when `payload_unit_start_indicator` is set. Only section-start
/// packets are handled (PAT/PMT almost always fit in a single packet).
fn psi_section(payload: &[u8], pusi: bool) -> Option<&[u8]> {
    if !pusi || payload.is_empty() {
        return None;
    }
    let pointer_field = payload[0] as usize;
    let start = 1 + pointer_field;
    payload.get(start..)
}

fn collect_pat_pmt_pids(section: &[u8], out: &mut HashSet<u16>) {
    if section.len() < 8 || section[0] != 0x00 {
        return;
    }
    let section_length = (((section[1] & 0x0F) as usize) << 8) | section[2] as usize;
    let total = (3 + section_length).min(section.len());
    if total < 12 {
        return;
    }
    let limit = total - 4; // exclude CRC32
    let mut index = 8;
    while index + 4 <= limit {
        let program_number = ((section[index] as u16) << 8) | section[index + 1] as u16;
        let pid = (((section[index + 2] & 0x1F) as u16) << 8) | section[index + 3] as u16;
        if program_number != 0 {
            out.insert(pid);
        }
        index += 4;
    }
}

/// Parse a PMT section and report whether any elementary stream is HEVC.
/// Returns `None` when the section cannot be parsed as a PMT.
fn pmt_has_hevc(section: &[u8]) -> Option<bool> {
    if section.len() < 12 || section[0] != 0x02 {
        return None;
    }
    let section_length = (((section[1] & 0x0F) as usize) << 8) | section[2] as usize;
    let total = (3 + section_length).min(section.len());
    if total < 16 {
        return None;
    }
    let program_info_length = (((section[10] & 0x0F) as usize) << 8) | section[11] as usize;
    let limit = total - 4; // exclude CRC32
    let mut index = 12 + program_info_length;
    let mut found_hevc = false;
    while index + 5 <= limit {
        let stream_type = section[index];
        let es_info_length =
            (((section[index + 3] & 0x0F) as usize) << 8) | section[index + 4] as usize;
        if stream_type == TS_STREAM_TYPE_HEVC {
            found_hevc = true;
        }
        index += 5 + es_info_length;
    }
    Some(found_hevc)
}

fn with_playback_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, HEAD, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, must-revalidate"),
    );
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

pub fn playback_log(message: &str) {
    eprintln!("[playback {}] {}", Utc::now().to_rfc3339(), message);
}

fn token_suffix(token: &str) -> &str {
    let start = token.len().saturating_sub(8);
    &token[start..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{DownloadTask, FileType};
    use std::fs;
    use uuid::Uuid;

    fn build_task(status: DownloadStatus) -> DownloadTask {
        DownloadTask {
            id: "task-1".to_string(),
            url: "https://example.com/video.m3u8".to_string(),
            source_kind: crate::models::DownloadSourceKind::Url,
            source_text: None,
            filename: "video".to_string(),
            file_type: FileType::Hls,
            hls_output_mode: crate::models::HlsOutputMode::SingleStream,
            hls_media_kind: HlsMediaKind::MpegTs,
            hls_selection: None,
            encryption_method: None,
            output_dir: "D:\\Downloads".to_string(),
            extra_headers: None,
            status,
            total_segments: 3,
            completed_segments: 1,
            completed_segment_indices: vec![1],
            failed_segment_indices: Vec::new(),
            segment_uris: vec![
                "https://example.com/0.ts".to_string(),
                "https://example.com/1.ts".to_string(),
                "https://example.com/2.ts".to_string(),
            ],
            segment_durations: vec![5.0, 7.5, 6.0],
            hls_init_segments: Vec::new(),
            segment_init_indices: Vec::new(),
            total_bytes: 1024,
            speed_bytes_per_sec: 0,
            created_at: Utc::now(),
            completed_at: None,
            updated_at: None,
            playback_available: true,
            file_path: None,
        }
    }

    fn unique_temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("m3u8quicker-playback-{}-{}", name, Uuid::new_v4()))
    }

    fn remove_temp_dir(dir: &FsPath) {
        let _ = fs::remove_dir_all(dir);
    }

    fn ts_psi_packet(pid: u16, section: &[u8]) -> [u8; TS_PACKET_LEN] {
        let mut packet = [0xFFu8; TS_PACKET_LEN];
        packet[0] = 0x47;
        packet[1] = 0x40 | ((pid >> 8) as u8 & 0x1F); // payload_unit_start_indicator set
        packet[2] = (pid & 0xFF) as u8;
        packet[3] = 0x10; // payload only, continuity_counter=0
        packet[4] = 0x00; // pointer_field
        packet[5..5 + section.len()].copy_from_slice(section);
        packet
    }

    fn pat_section(pmt_pid: u16) -> Vec<u8> {
        let mut section = vec![
            0x00, // table_id (PAT)
            0xB0, 0x0D, // section_syntax_indicator + section_length(13)
            0x00, 0x01, // transport_stream_id
            0xC1, // version / current_next
            0x00, // section_number
            0x00, // last_section_number
            0x00, 0x01, // program_number = 1
            0xE0 | ((pmt_pid >> 8) as u8 & 0x1F),
            (pmt_pid & 0xFF) as u8,
        ];
        section.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]); // CRC32 (ignored by parser)
        section
    }

    fn pmt_section(video_stream_type: u8) -> Vec<u8> {
        let mut section = vec![
            0x02, // table_id (PMT)
            0xB0, 0x17, // section_syntax_indicator + section_length(23)
            0x00, 0x01, // program_number
            0xC1, // version / current_next
            0x00, // section_number
            0x00, // last_section_number
            0xE1, 0x01, // reserved + PCR_PID
            0xF0, 0x00, // reserved + program_info_length(0)
            video_stream_type, 0xE1, 0x01, 0xF0, 0x00, // video ES, PID 0x0101
            0x0F, 0xE1, 0x02, 0xF0, 0x00, // AAC audio ES, PID 0x0102
        ];
        section.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]); // CRC32 (ignored by parser)
        section
    }

    fn ts_stream(video_stream_type: u8) -> Vec<u8> {
        let pmt_pid = 0x0100u16;
        let mut data = Vec::new();
        data.extend_from_slice(&ts_psi_packet(0x0000, &pat_section(pmt_pid)));
        data.extend_from_slice(&ts_psi_packet(pmt_pid, &pmt_section(video_stream_type)));
        data
    }

    #[test]
    fn detects_hevc_video_in_mpegts() {
        assert_eq!(ts_video_is_hevc(&ts_stream(0x24)), Some(true));
    }

    #[test]
    fn reports_non_hevc_video_in_mpegts() {
        // 0x1B = AVC/H.264
        assert_eq!(ts_video_is_hevc(&ts_stream(0x1B)), Some(false));
    }

    #[test]
    fn returns_none_for_non_ts_bytes() {
        assert_eq!(ts_video_is_hevc(b"not a transport stream at all"), None);
    }

    #[test]
    fn segment_index_for_position_maps_boundaries() {
        let durations = vec![5.0, 7.5, 6.0];

        assert_eq!(segment_index_for_position(&durations, 0.0), 0);
        assert_eq!(segment_index_for_position(&durations, 4.99), 0);
        assert_eq!(segment_index_for_position(&durations, 5.0), 1);
        assert_eq!(segment_index_for_position(&durations, 20.0), 2);
    }

    #[test]
    fn build_playlist_outputs_expected_lines() {
        let playlist = build_playlist(&build_task(DownloadStatus::Completed), "token-1").unwrap();

        assert!(playlist.contains("#EXT-X-TARGETDURATION:8"));
        assert!(playlist.contains("#EXT-X-PLAYLIST-TYPE:VOD"));
        assert!(playlist.contains("#EXT-X-START:TIME-OFFSET=0,PRECISE=YES"));
        assert!(playlist.contains("#EXTINF:5.000,"));
        assert!(playlist.contains("segments/1?token=token-1"));
        assert!(playlist.contains("#EXT-X-ENDLIST"));
    }

    #[test]
    fn build_playlist_outputs_fmp4_map_lines() {
        let mut task = build_task(DownloadStatus::Downloading);
        task.hls_media_kind = HlsMediaKind::Fmp4;
        task.hls_init_segments = vec![crate::models::HlsInitSegmentInfo {
            index: 0,
            uri: "https://example.com/init.mp4".to_string(),
            byte_range: None,
        }];
        task.segment_init_indices = vec![Some(0), Some(0), Some(0)];

        let playlist = build_playlist(&task, "token-fmp4").unwrap();

        assert!(playlist.contains("#EXT-X-VERSION:6"));
        assert!(playlist.contains("#EXT-X-MAP:URI=\"init/0?token=token-fmp4\""));
        assert_eq!(playlist.matches("#EXT-X-MAP").count(), 1);
        assert!(playlist.contains("segments/2?token=token-fmp4"));
    }

    #[test]
    fn build_playlist_for_paused_task_is_still_vod() {
        let playlist = build_playlist(&build_task(DownloadStatus::Paused), "token-2").unwrap();

        assert!(playlist.contains("#EXT-X-PLAYLIST-TYPE:VOD"));
        assert!(playlist.contains("#EXT-X-ENDLIST"));
        assert!(!playlist.contains("#EXT-X-PLAYLIST-TYPE:EVENT"));
    }

    #[tokio::test]
    async fn priority_window_reorders_pending_segments() {
        let state = DownloadPriorityState::new(6, &[1, 2], &[]);
        state.prioritize_window(4, 6).await;

        assert_eq!(state.pending_snapshot().await, vec![4, 5, 2, 3]);
    }

    #[test]
    fn build_playback_router_does_not_panic() {
        let state = PlaybackHttpState {
            downloads: Arc::new(Mutex::new(HashMap::new())),
            playback_sessions: Arc::new(Mutex::new(HashMap::new())),
            download_priorities: Arc::new(Mutex::new(HashMap::new())),
            live_records: Arc::new(Mutex::new(HashMap::new())),
            live_playback_sessions: Arc::new(Mutex::new(HashMap::new())),
        };

        let _router = build_playback_router(state);
    }

    #[test]
    fn playback_window_label_round_trips_task_id() {
        let task_id = "task-1";
        let label = playback_window_label(task_id);

        assert_eq!(task_id_from_window_label(&label), Some(task_id));
        assert_eq!(task_id_from_window_label("main"), None);
        assert_eq!(task_id_from_window_label("player-"), None);
    }

    #[test]
    fn live_segment_name_validation_rejects_traversal() {
        assert!(is_valid_live_segment_name("seg_00000001.ts"));
        assert!(is_valid_live_segment_name("seg_00000001.m4s"));
        assert!(is_valid_live_segment_name("init_000000.mp4"));
        assert!(!is_valid_live_segment_name("seg_1.mp4"));
        assert!(!is_valid_live_segment_name("init_0.ts"));
        assert!(!is_valid_live_segment_name("../index.m3u8"));
        assert!(!is_valid_live_segment_name("seg_/0.ts"));
        assert!(!is_valid_live_segment_name("seg_abc.ts"));
        assert!(!is_valid_live_segment_name("index.m3u8"));
    }

    #[test]
    fn rewrite_live_playlist_recording_is_event_without_endlist() {
        let source = "#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:4.000,\nseg_00000000.ts\n";
        let out = rewrite_live_playlist(source, "tok", true);

        assert!(out.contains("#EXT-X-PLAYLIST-TYPE:EVENT"));
        assert!(out.contains("#EXT-X-START:TIME-OFFSET=0,PRECISE=YES"));
        assert!(!out.contains("#EXT-X-PLAYLIST-TYPE:VOD"));
        assert!(!out.contains("#EXT-X-ENDLIST"));
        assert!(out.contains("seg/seg_00000000.ts?token=tok"));
        assert!(out.contains("#EXT-X-TARGETDURATION:4"));
    }

    #[test]
    fn rewrite_live_playlist_finished_is_vod_with_endlist_and_map() {
        let source = "#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MAP:URI=\"init_000000.mp4\"\n#EXTINF:4.000,\nseg_00000000.m4s\n#EXT-X-ENDLIST\n";
        let out = rewrite_live_playlist(source, "tok", false);

        assert!(out.contains("#EXT-X-PLAYLIST-TYPE:VOD"));
        assert!(out.trim_end().ends_with("#EXT-X-ENDLIST"));
        assert_eq!(out.matches("#EXT-X-ENDLIST").count(), 1);
        assert!(out.contains("#EXT-X-MAP:URI=\"seg/init_000000.mp4?token=tok\""));
        assert!(out.contains("seg/seg_00000000.m4s?token=tok"));
    }

    #[test]
    fn live_window_label_round_trips_task_id() {
        let label = live_playback_window_label("abc");
        assert_eq!(task_id_from_live_window_label(&label), Some("abc"));
        assert_eq!(task_id_from_live_window_label("player-abc"), None);
        assert_eq!(task_id_from_live_window_label("live-player-"), None);
    }

    #[test]
    fn playback_headers_include_cors() {
        let response = with_playback_headers(axum::http::Response::new(axum::body::Body::empty()));

        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|value| value.to_str().ok()),
            Some("*")
        );
    }

    #[test]
    fn playback_file_path_for_task_supports_in_progress_mp4_and_webm_only() {
        let temp_root = unique_temp_path("progressive-file");
        fs::create_dir_all(&temp_root).expect("create temp dir");

        let mut mp4_task = build_task(DownloadStatus::Downloading);
        mp4_task.file_type = FileType::Mp4;
        mp4_task.output_dir = temp_root.to_string_lossy().to_string();
        mp4_task.filename = "video.mp4".to_string();
        let mp4_partial = temp_root.join("video.mp4.partial");
        fs::write(&mp4_partial, b"partial").expect("write mp4 partial");

        let mut webm_task = build_task(DownloadStatus::Paused);
        webm_task.file_type = FileType::Webm;
        webm_task.output_dir = temp_root.to_string_lossy().to_string();
        webm_task.filename = "clip.webm".to_string();
        let webm_partial = temp_root.join("clip.webm.partial");
        fs::write(&webm_partial, b"partial").expect("write webm partial");

        let mut mkv_task = build_task(DownloadStatus::Downloading);
        mkv_task.file_type = FileType::Mkv;
        mkv_task.output_dir = temp_root.to_string_lossy().to_string();
        mkv_task.filename = "movie.mkv".to_string();

        assert_eq!(
            playback_file_path_for_task(&mp4_task).expect("mp4 partial"),
            mp4_partial
        );
        assert_eq!(
            playback_file_path_for_task(&webm_task).expect("webm partial"),
            webm_partial
        );
        assert!(playback_file_path_for_task(&mkv_task)
            .expect_err("mkv should fail")
            .message
            .contains("当前格式暂不支持边下边播"));

        remove_temp_dir(&temp_root);
    }

    #[tokio::test]
    async fn build_file_response_serves_partial_file_with_ranges() {
        let temp_root = unique_temp_path("partial-response");
        fs::create_dir_all(&temp_root).expect("create temp dir");

        let partial_path = temp_root.join("video.mp4.partial");
        fs::write(&partial_path, b"0123456789").expect("write partial");

        let mut task = build_task(DownloadStatus::Downloading);
        task.file_type = FileType::Mp4;
        task.output_dir = temp_root.to_string_lossy().to_string();
        task.filename = "video.mp4".to_string();

        let response = build_file_response(&task, &HeaderMap::new())
            .await
            .expect("build response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok()),
            Some("10")
        );

        let mut ranged_headers = HeaderMap::new();
        ranged_headers.insert(header::RANGE, HeaderValue::from_static("bytes=3-5"));
        let ranged_response = build_file_response(&task, &ranged_headers)
            .await
            .expect("build ranged response");
        assert_eq!(ranged_response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            ranged_response
                .headers()
                .get(header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok()),
            Some("bytes 3-5/10")
        );

        let mut invalid_headers = HeaderMap::new();
        invalid_headers.insert(header::RANGE, HeaderValue::from_static("bytes=20-30"));
        let error = build_file_response(&task, &invalid_headers)
            .await
            .expect_err("range should fail");
        assert_eq!(error.status, StatusCode::RANGE_NOT_SATISFIABLE);

        remove_temp_dir(&temp_root);
    }
}
