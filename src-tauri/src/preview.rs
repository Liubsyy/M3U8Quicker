use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use futures::stream::{self, StreamExt};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::error::AppError;
use crate::ffmpeg;
use crate::state::AppState;

const PREVIEW_WINDOW_LABEL_PREFIX: &str = "preview-";
pub const MIN_THUMBNAIL_COUNT: usize = 9;
pub const MAX_THUMBNAIL_COUNT: usize = 99;
pub const MIN_THUMBNAIL_WIDTH: u32 = 320;
pub const MAX_THUMBNAIL_WIDTH: u32 = 1920;
pub const MIN_JPEG_QUALITY: u8 = 2;
pub const MAX_JPEG_QUALITY: u8 = 10;

#[derive(Debug)]
pub struct PreviewSession {
    pub url: String,
    pub extra_headers: Option<String>,
    pub cache_dir: PathBuf,
    pub duration_secs: Mutex<Option<f64>>,
    pub operation_lock: RwLock<()>,
    pub cancel_token: Mutex<CancellationToken>,
    pub cancelled_runs: Mutex<HashSet<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PreviewThumbnail {
    pub index: usize,
    pub time_secs: f64,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PreviewThumbnailEvent {
    pub token: String,
    pub count: usize,
    pub target_width: u32,
    pub jpeg_quality: u8,
    pub run_id: String,
    pub thumbnail: PreviewThumbnail,
}

pub async fn create_session(
    app_handle: &AppHandle,
    state: &AppState,
    url: String,
    extra_headers: Option<String>,
) -> Result<String, AppError> {
    let token = Uuid::new_v4().to_string();
    let cache_dir = preview_root_dir(app_handle)?.join(&token);
    tokio::fs::create_dir_all(&cache_dir).await?;

    let session = Arc::new(PreviewSession {
        url,
        extra_headers,
        cache_dir,
        duration_secs: Mutex::new(None),
        operation_lock: RwLock::new(()),
        cancel_token: Mutex::new(CancellationToken::new()),
        cancelled_runs: Mutex::new(HashSet::new()),
    });

    let mut sessions = state.preview_sessions.lock().await;
    sessions.insert(token.clone(), session);
    Ok(token)
}

pub async fn extract_thumbnails(
    app_handle: &AppHandle,
    state: &AppState,
    token: &str,
    count: usize,
    target_width: u32,
    jpeg_quality: u8,
    run_id: &str,
    force_refresh: bool,
) -> Result<Vec<PreviewThumbnail>, AppError> {
    if !(MIN_THUMBNAIL_COUNT..=MAX_THUMBNAIL_COUNT).contains(&count) {
        return Err(AppError::InvalidInput(format!(
            "缩略图数量必须在 {}~{} 之间",
            MIN_THUMBNAIL_COUNT, MAX_THUMBNAIL_COUNT
        )));
    }
    if !(MIN_THUMBNAIL_WIDTH..=MAX_THUMBNAIL_WIDTH).contains(&target_width) {
        return Err(AppError::InvalidInput(format!(
            "预览图宽度必须在 {}~{} 之间",
            MIN_THUMBNAIL_WIDTH, MAX_THUMBNAIL_WIDTH
        )));
    }
    if !(MIN_JPEG_QUALITY..=MAX_JPEG_QUALITY).contains(&jpeg_quality) {
        return Err(AppError::InvalidInput(format!(
            "图片质量参数必须在 {}~{} 之间",
            MIN_JPEG_QUALITY, MAX_JPEG_QUALITY
        )));
    }

    let session = {
        let sessions = state.preview_sessions.lock().await;
        sessions
            .get(token)
            .cloned()
            .ok_or_else(|| AppError::InvalidInput("预览会话不存在或已关闭".to_string()))?
    };
    let _operation_guard = session.operation_lock.write().await;
    {
        let mut cancelled_runs = session.cancelled_runs.lock().await;
        if cancelled_runs.remove(run_id) {
            return Err(AppError::InvalidInput("预览已取消".to_string()));
        }
    }

    let ffmpeg_path = ffmpeg::resolve_ffmpeg_path(app_handle)
        .await
        .ok_or_else(|| AppError::InvalidInput("请先在设置中开启并配置 FFmpeg".to_string()))?;
    let cancel_token = {
        let mut guard = session.cancel_token.lock().await;
        if guard.is_cancelled() {
            *guard = CancellationToken::new();
        }
        guard.child_token()
    };
    let proxy_url = {
        let proxy = state.proxy_settings.lock().await;
        if proxy.enabled && !proxy.url.trim().is_empty() {
            Some(proxy.url.trim().to_string())
        } else {
            None
        }
    };

    let duration_secs = {
        let mut guard = session.duration_secs.lock().await;
        if let Some(value) = *guard {
            value
        } else {
            let value = ffmpeg::probe_media_duration_secs_cancellable(
                &ffmpeg_path,
                &session.url,
                session.extra_headers.as_deref(),
                proxy_url.as_deref(),
                &cancel_token,
            )
            .await?;
            if !(value.is_finite() && value > 0.0) {
                return Err(AppError::Conversion(
                    "无法识别视频时长，无法生成预览".to_string(),
                ));
            }
            *guard = Some(value);
            value
        }
    };

    let thumbnail_dir =
        thumbnail_dir_for_options(&session.cache_dir, count, target_width, jpeg_quality);
    if force_refresh {
        match tokio::fs::remove_dir_all(&thumbnail_dir).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    tokio::fs::create_dir_all(&thumbnail_dir).await?;
    let preview_concurrency = (*state.max_concurrent_segments.lock().await).max(1);

    let results = stream::iter(0..count)
        .map(|index| {
            let app_handle = app_handle.clone();
            let cancel_token = cancel_token.clone();
            let ffmpeg_path = ffmpeg_path.clone();
            let session = Arc::clone(&session);
            let token = token.to_string();
            let run_id = run_id.to_string();
            let proxy_url = proxy_url.clone();

            async move {
                if cancel_token.is_cancelled() {
                    return Err(AppError::InvalidInput("预览已取消".to_string()));
                }

                let time = duration_secs * (index as f64 + 0.5) / (count as f64);
                let output_path = thumbnail_path_for_options(
                    &session.cache_dir,
                    count,
                    target_width,
                    jpeg_quality,
                    index,
                );
                if !tokio::fs::try_exists(&output_path).await.unwrap_or(false) {
                    ffmpeg::extract_thumbnail_jpeg_cancellable(
                        &ffmpeg_path,
                        &session.url,
                        session.extra_headers.as_deref(),
                        proxy_url.as_deref(),
                        time,
                        &output_path,
                        target_width,
                        jpeg_quality,
                        &cancel_token,
                    )
                    .await?;
                }

                let thumbnail = PreviewThumbnail {
                    index,
                    time_secs: time,
                    path: output_path.to_string_lossy().into_owned(),
                };
                let _ = app_handle.emit(
                    "preview-thumbnail",
                    PreviewThumbnailEvent {
                        token,
                        count,
                        target_width,
                        jpeg_quality,
                        run_id,
                        thumbnail: thumbnail.clone(),
                    },
                );
                Ok(thumbnail)
            }
        })
        .buffer_unordered(preview_concurrency)
        .collect::<Vec<Result<PreviewThumbnail, AppError>>>()
        .await;

    let collected = results.into_iter().collect::<Result<Vec<_>, _>>();
    {
        let mut cancelled_runs = session.cancelled_runs.lock().await;
        cancelled_runs.remove(run_id);
    }
    let mut results = collected?;
    results.sort_by_key(|thumbnail| thumbnail.index);

    Ok(results)
}

pub async fn cancel_extraction(state: &AppState, token: &str, run_id: &str) -> Result<(), AppError> {
    let session = {
        let sessions = state.preview_sessions.lock().await;
        sessions
            .get(token)
            .cloned()
            .ok_or_else(|| AppError::InvalidInput("预览会话不存在或已关闭".to_string()))?
    };

    {
        let mut cancelled_runs = session.cancelled_runs.lock().await;
        cancelled_runs.insert(run_id.to_string());
    }
    {
        let guard = session.cancel_token.lock().await;
        guard.cancel();
    }

    Ok(())
}

pub async fn close_session(state: &AppState, token: &str) {
    let session = {
        let mut sessions = state.preview_sessions.lock().await;
        sessions.remove(token)
    };
    if let Some(session) = session {
        {
            let guard = session.cancel_token.lock().await;
            guard.cancel();
        }
        let _operation_guard = session.operation_lock.write().await;
        let _ = tokio::fs::remove_dir_all(&session.cache_dir).await;
    }
}

pub fn window_label(token: &str) -> String {
    format!("{}{}", PREVIEW_WINDOW_LABEL_PREFIX, token)
}

pub fn token_from_window_label(label: &str) -> Option<&str> {
    label
        .strip_prefix(PREVIEW_WINDOW_LABEL_PREFIX)
        .filter(|token| !token.is_empty())
}

fn preview_root_dir(app_handle: &AppHandle) -> Result<PathBuf, AppError> {
    let cache_dir = app_handle
        .path()
        .app_cache_dir()
        .map_err(|e| AppError::Internal(format!("无法获取应用缓存目录: {}", e)))?;
    Ok(cache_dir.join("preview"))
}

fn thumbnail_path_for_options(
    cache_dir: &std::path::Path,
    count: usize,
    target_width: u32,
    jpeg_quality: u8,
    index: usize,
) -> PathBuf {
    thumbnail_dir_for_options(cache_dir, count, target_width, jpeg_quality)
        .join(format!("{:03}.jpg", index))
}

fn thumbnail_dir_for_options(
    cache_dir: &std::path::Path,
    count: usize,
    target_width: u32,
    jpeg_quality: u8,
) -> PathBuf {
    cache_dir.join(format!(
        "count_{:03}_w_{:04}_q_{:02}",
        count, target_width, jpeg_quality
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_window_label_round_trips_token() {
        let token = "12345678-90ab-cdef-1234-567890abcdef";
        let label = window_label(token);

        assert_eq!(token_from_window_label(&label), Some(token));
        assert_eq!(token_from_window_label("main"), None);
        assert_eq!(token_from_window_label("preview-"), None);
    }

    #[test]
    fn thumbnail_path_includes_requested_count() {
        let cache_dir = PathBuf::from("preview-session");

        assert_eq!(
            thumbnail_path_for_options(&cache_dir, 9, 320, 4, 0),
            cache_dir.join("count_009_w_0320_q_04").join("000.jpg")
        );
        assert_eq!(
            thumbnail_path_for_options(&cache_dir, 18, 1280, 2, 0),
            cache_dir.join("count_018_w_1280_q_02").join("000.jpg")
        );
    }
}
