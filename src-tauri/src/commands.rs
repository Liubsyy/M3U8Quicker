use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use chrono::Utc;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::downloader;
use crate::error::AppError;
use crate::models::*;
use crate::persistence;
use crate::playback;
use crate::state::AppState;

const CHROME_EXTENSIONS_URL: &str = "chrome://extensions/";
const EDGE_EXTENSIONS_URL: &str = "edge://extensions/";
const FIREFOX_ADDONS_URL: &str = "about:debugging#/runtime/this-firefox";
const NORMALIZED_DOWNLOAD_EXTENSIONS: &[&str] = &[
    "m3u8", "mp4", "mkv", "avi", "wmv", "flv", "webm", "mov", "rmvb", "ts",
];
const DIRECT_DOWNLOAD_EXTENSIONS: &[(&str, FileType)] = &[
    ("mp4", FileType::Mp4),
    ("mkv", FileType::Mkv),
    ("avi", FileType::Avi),
    ("wmv", FileType::Wmv),
    ("flv", FileType::Flv),
    ("webm", FileType::Webm),
    ("mov", FileType::Mov),
    ("rmvb", FileType::Rmvb),
];

#[tauri::command]
pub async fn inspect_hls_tracks(
    state: State<'_, AppState>,
    params: InspectHlsTracksParams,
) -> Result<InspectHlsTracksResult, AppError> {
    let client = state.http_client.read().await.clone();
    let request_headers = parse_request_headers(params.extra_headers.as_deref())?;
    downloader::inspect_hls_tracks(&client, &params.url, &request_headers).await
}

#[tauri::command]
pub async fn create_download(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    params: CreateDownloadParams,
) -> Result<DownloadTaskSummary, AppError> {
    let id = Uuid::new_v4().to_string();
    let client = state.http_client.read().await.clone();
    let request_headers = parse_request_headers(params.extra_headers.as_deref())?;
    let file_type = resolve_create_download_file_type(&params)?;

    let output_dir =
        if let Some(output_dir) = params.output_dir.filter(|dir| !dir.trim().is_empty()) {
            let output_dir = output_dir.trim().to_string();
            let mut default_download_dir = state.default_download_dir.lock().await;
            if *default_download_dir != output_dir {
                *default_download_dir = output_dir.clone();
                persistence::update_settings(&app_handle, |settings| {
                    settings.default_download_dir = Some(output_dir.clone());
                })
                .await;
            }
            output_dir
        } else {
            state.default_download_dir.lock().await.clone()
        };
    let filename = normalize_download_filename(
        params
            .filename
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| derive_filename_from_url(&params.url)),
    );
    let filename = if file_type.is_direct_download() {
        normalize_direct_download_filename(filename, file_type)
    } else {
        filename
    };

    // Ensure output directory exists
    tokio::fs::create_dir_all(&output_dir).await?;

    let created_at = Utc::now();

    if file_type.is_direct_download() {
        let task = DownloadTask {
            id: id.clone(),
            url: params.url.clone(),
            filename: filename.clone(),
            file_type,
            hls_output_mode: HlsOutputMode::SingleStream,
            hls_selection: None,
            encryption_method: None,
            output_dir: output_dir.clone(),
            extra_headers: params.extra_headers.clone(),
            status: DownloadStatus::Downloading,
            total_segments: 0,
            completed_segments: 0,
            completed_segment_indices: Vec::new(),
            failed_segment_indices: Vec::new(),
            segment_uris: Vec::new(),
            segment_durations: Vec::new(),
            total_bytes: 0,
            speed_bytes_per_sec: 0,
            created_at,
            completed_at: None,
            updated_at: Some(created_at),
            playback_available: true,
            file_path: None,
        };

        {
            let mut downloads = state.downloads.lock().await;
            downloads.insert(id.clone(), task.clone());
        }
        persistence::save_task(&app_handle, &task).await?;

        start_mp4_download_worker(
            app_handle.clone(),
            state.downloads.clone(),
            state.cancel_tokens.clone(),
            state.http_client.clone(),
            task.clone(),
            request_headers,
            false,
            false,
        )
        .await;

        Ok(persistence::task_to_summary(&task))
    } else {
        match downloader::prepare_hls_download(
            &client,
            &params.url,
            &request_headers,
            params.hls_selection.as_ref(),
        )
        .await?
        {
            downloader::PreparedHlsDownload::Single(prepared) => {
                let task = DownloadTask {
                    id: id.clone(),
                    url: params.url.clone(),
                    filename: filename.clone(),
                    file_type: FileType::Hls,
                    hls_output_mode: HlsOutputMode::SingleStream,
                    hls_selection: prepared.selection.clone(),
                    encryption_method: detect_encryption_method(&prepared.segments),
                    output_dir: output_dir.clone(),
                    extra_headers: params.extra_headers.clone(),
                    status: DownloadStatus::Downloading,
                    total_segments: prepared.segments.len(),
                    completed_segments: 0,
                    completed_segment_indices: Vec::new(),
                    failed_segment_indices: Vec::new(),
                    segment_uris: segment_uris(&prepared.segments),
                    segment_durations: segment_durations(&prepared.segments),
                    total_bytes: 0,
                    speed_bytes_per_sec: 0,
                    created_at,
                    completed_at: None,
                    updated_at: Some(created_at),
                    playback_available: true,
                    file_path: None,
                };

                {
                    let mut downloads = state.downloads.lock().await;
                    downloads.insert(id.clone(), task.clone());
                }
                persistence::save_task(&app_handle, &task).await?;

                start_download_worker(
                    app_handle.clone(),
                    state.downloads.clone(),
                    state.cancel_tokens.clone(),
                    state.playback_sessions.clone(),
                    state.download_priorities.clone(),
                    state.http_client.clone(),
                    task.clone(),
                    prepared.segments,
                    request_headers,
                    state.max_concurrent_segments.clone(),
                )
                .await;

                Ok(persistence::task_to_summary(&task))
            }
            downloader::PreparedHlsDownload::Bundle(prepared) => {
                let bundle_dir = resolve_bundle_output_dir(Path::new(&output_dir), &filename)
                    .to_string_lossy()
                    .to_string();
                let task = DownloadTask {
                    id: id.clone(),
                    url: params.url.clone(),
                    filename: filename.clone(),
                    file_type: FileType::Hls,
                    hls_output_mode: HlsOutputMode::MultiTrackBundle,
                    hls_selection: Some(prepared.selection.clone()),
                    encryption_method: prepared.encryption_method(),
                    output_dir: output_dir.clone(),
                    extra_headers: params.extra_headers.clone(),
                    status: DownloadStatus::Downloading,
                    total_segments: prepared.total_units(),
                    completed_segments: 0,
                    completed_segment_indices: Vec::new(),
                    failed_segment_indices: Vec::new(),
                    segment_uris: prepared.source_uris(),
                    segment_durations: prepared.durations(),
                    total_bytes: 0,
                    speed_bytes_per_sec: 0,
                    created_at,
                    completed_at: None,
                    updated_at: Some(created_at),
                    playback_available: false,
                    file_path: Some(bundle_dir.clone()),
                };

                {
                    let mut downloads = state.downloads.lock().await;
                    downloads.insert(id.clone(), task.clone());
                }
                persistence::save_task(&app_handle, &task).await?;

                start_hls_bundle_download_worker(
                    app_handle.clone(),
                    state.downloads.clone(),
                    state.cancel_tokens.clone(),
                    state.http_client.clone(),
                    task.clone(),
                    PathBuf::from(bundle_dir),
                    prepared,
                    request_headers,
                    state.max_concurrent_segments.clone(),
                )
                .await;

                Ok(persistence::task_to_summary(&task))
            }
        }
    }
}

#[tauri::command]
pub async fn pause_download(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), AppError> {
    let token = {
        let mut tokens = state.cancel_tokens.lock().await;
        tokens.remove(&id)
    };

    let Some(token) = token else {
        return Err(AppError::InvalidInput(format!(
            "Download {} not found or already finished",
            id
        )));
    };

    let task = {
        let mut downloads = state.downloads.lock().await;
        let task = downloads
            .get_mut(&id)
            .ok_or_else(|| AppError::InvalidInput(format!("Download {} not found", id)))?;

        if task.status != DownloadStatus::Downloading {
            return Err(AppError::InvalidInput(
                "只有下载中的任务可以暂停".to_string(),
            ));
        }

        task.status = DownloadStatus::Paused;
        task.speed_bytes_per_sec = 0;
        task.touch();
        task.clone()
    };

    token.cancel();
    persistence::save_task(&app_handle, &task).await?;
    let progress = task_to_progress(&task);
    let _ = app_handle.emit("download-progress", &progress);
    Ok(())
}

#[tauri::command]
pub async fn resume_download(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    id: String,
    restart_confirmed: Option<bool>,
) -> Result<DownloadTaskSummary, AppError> {
    let task = get_or_load_task(&app_handle, &state, &id).await?;

    if task.status != DownloadStatus::Paused {
        return Err(AppError::InvalidInput(
            "只有已暂停的任务可以继续".to_string(),
        ));
    }

    {
        let tokens = state.cancel_tokens.lock().await;
        if tokens.contains_key(&id) {
            return Err(AppError::InvalidInput("任务已在运行中".to_string()));
        }
    }

    let request_headers = parse_request_headers(task.extra_headers.as_deref())?;

    if task.file_type.is_direct_download() {
        let client = state.http_client.read().await.clone();
        let resume_check = downloader::check_mp4_resume(
            &client,
            &task.url,
            &request_headers,
            &PathBuf::from(&task.output_dir),
            &task.filename,
        )
        .await?;
        let should_restart_mp4 = matches!(
            resume_check,
            downloader::Mp4ResumeCheck::RequiresRestartConfirmation { .. }
        ) && restart_confirmed.unwrap_or(false);
        if matches!(
            resume_check,
            downloader::Mp4ResumeCheck::RequiresRestartConfirmation { .. }
        ) && !restart_confirmed.unwrap_or(false)
        {
            return Err(AppError::InvalidInput(
                "服务器不支持断点续传，请确认后从头下载".to_string(),
            ));
        }

        let updated_task = {
            let mut downloads = state.downloads.lock().await;
            downloads.entry(id.clone()).or_insert_with(|| task.clone());
            let task = downloads
                .get_mut(&id)
                .ok_or_else(|| AppError::InvalidInput(format!("Download {} not found", id)))?;
            task.status = DownloadStatus::Downloading;
            task.speed_bytes_per_sec = 0;
            task.completed_at = None;
            task.file_path = None;
            if should_restart_mp4 {
                task.total_bytes = 0;
                task.completed_segments = 0;
            }
            task.touch();
            task.clone()
        };

        persistence::save_task(&app_handle, &updated_task).await?;
        let progress = task_to_progress(&updated_task);
        let _ = app_handle.emit("download-progress", &progress);

        start_mp4_download_worker(
            app_handle.clone(),
            state.downloads.clone(),
            state.cancel_tokens.clone(),
            state.http_client.clone(),
            updated_task.clone(),
            request_headers,
            true,
            restart_confirmed.unwrap_or(false),
        )
        .await;

        return Ok(persistence::task_to_summary(&updated_task));
    }

    let client = state.http_client.read().await.clone();
    match downloader::prepare_hls_download(
        &client,
        &task.url,
        &request_headers,
        task.hls_selection.as_ref(),
    )
    .await?
    {
        downloader::PreparedHlsDownload::Single(prepared) => {
            if task.hls_output_mode != HlsOutputMode::SingleStream {
                return Err(AppError::InvalidInput(
                    "检测到远端轨道结构已变化，请重新创建下载任务".to_string(),
                ));
            }
            validate_segment_layout(&task, &prepared.segments)?;

            let updated_task = {
                let mut downloads = state.downloads.lock().await;
                downloads.entry(id.clone()).or_insert_with(|| task.clone());
                let task = downloads
                    .get_mut(&id)
                    .ok_or_else(|| AppError::InvalidInput(format!("Download {} not found", id)))?;
                task.status = DownloadStatus::Downloading;
                task.speed_bytes_per_sec = 0;
                task.completed_at = None;
                task.file_path = None;
                task.total_segments = prepared.segments.len();
                task.segment_uris = segment_uris(&prepared.segments);
                task.segment_durations = segment_durations(&prepared.segments);
                task.encryption_method = detect_encryption_method(&prepared.segments);
                task.hls_selection = prepared.selection.clone();
                task.hls_output_mode = HlsOutputMode::SingleStream;
                task.playback_available = true;
                task.touch();
                task.clone()
            };

            persistence::save_task(&app_handle, &updated_task).await?;
            let progress = task_to_progress(&updated_task);
            let _ = app_handle.emit("download-progress", &progress);

            start_download_worker(
                app_handle.clone(),
                state.downloads.clone(),
                state.cancel_tokens.clone(),
                state.playback_sessions.clone(),
                state.download_priorities.clone(),
                state.http_client.clone(),
                updated_task.clone(),
                prepared.segments,
                request_headers,
                state.max_concurrent_segments.clone(),
            )
            .await;

            Ok(persistence::task_to_summary(&updated_task))
        }
        downloader::PreparedHlsDownload::Bundle(prepared) => {
            if task.hls_output_mode != HlsOutputMode::MultiTrackBundle {
                return Err(AppError::InvalidInput(
                    "检测到远端轨道结构已变化，请重新创建下载任务".to_string(),
                ));
            }
            validate_bundle_layout(&task, &prepared.source_uris())?;
            let bundle_dir = task
                .file_path
                .clone()
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    resolve_bundle_output_dir(Path::new(&task.output_dir), &task.filename)
                });

            let updated_task = {
                let mut downloads = state.downloads.lock().await;
                downloads.entry(id.clone()).or_insert_with(|| task.clone());
                let task = downloads
                    .get_mut(&id)
                    .ok_or_else(|| AppError::InvalidInput(format!("Download {} not found", id)))?;
                task.status = DownloadStatus::Downloading;
                task.speed_bytes_per_sec = 0;
                task.completed_at = None;
                task.file_path = Some(bundle_dir.to_string_lossy().to_string());
                task.total_segments = prepared.total_units();
                task.segment_uris = prepared.source_uris();
                task.segment_durations = prepared.durations();
                task.encryption_method = prepared.encryption_method();
                task.hls_selection = Some(prepared.selection.clone());
                task.hls_output_mode = HlsOutputMode::MultiTrackBundle;
                task.playback_available = false;
                task.touch();
                task.clone()
            };

            persistence::save_task(&app_handle, &updated_task).await?;
            let progress = task_to_progress(&updated_task);
            let _ = app_handle.emit("download-progress", &progress);

            start_hls_bundle_download_worker(
                app_handle.clone(),
                state.downloads.clone(),
                state.cancel_tokens.clone(),
                state.http_client.clone(),
                updated_task.clone(),
                bundle_dir,
                prepared,
                request_headers,
                state.max_concurrent_segments.clone(),
            )
            .await;

            Ok(persistence::task_to_summary(&updated_task))
        }
    }
}

#[tauri::command]
pub async fn check_resume_download(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<ResumeDownloadCheckResult, AppError> {
    let task = get_or_load_task(&app_handle, &state, &id).await?;

    if task.status != DownloadStatus::Paused {
        return Err(AppError::InvalidInput(
            "只有已暂停的任务可以继续".to_string(),
        ));
    }

    if !task.file_type.is_direct_download() {
        return Ok(ResumeDownloadCheckResult {
            action: ResumeDownloadAction::Resume,
            downloaded_bytes: task.total_bytes,
        });
    }

    let client = state.http_client.read().await.clone();
    let request_headers = parse_request_headers(task.extra_headers.as_deref())?;
    let check = downloader::check_mp4_resume(
        &client,
        &task.url,
        &request_headers,
        &PathBuf::from(&task.output_dir),
        &task.filename,
    )
    .await?;

    Ok(match check {
        downloader::Mp4ResumeCheck::Ready { downloaded_bytes } => ResumeDownloadCheckResult {
            action: ResumeDownloadAction::Resume,
            downloaded_bytes,
        },
        downloader::Mp4ResumeCheck::RequiresRestartConfirmation { downloaded_bytes } => {
            ResumeDownloadCheckResult {
                action: ResumeDownloadAction::ConfirmRestart,
                downloaded_bytes,
            }
        }
    })
}

#[tauri::command]
pub async fn retry_failed_segments(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<DownloadTaskSummary, AppError> {
    let task = get_or_load_task(&app_handle, &state, &id).await?;

    if task.failed_segment_indices.is_empty() {
        return Err(AppError::InvalidInput(
            "当前任务没有可重试的失败分片".to_string(),
        ));
    }

    if task.status != DownloadStatus::Downloading && task.status != DownloadStatus::Paused {
        return Err(AppError::InvalidInput(
            "只有下载中或已暂停的任务可以重试失败分片".to_string(),
        ));
    }

    {
        let mut tokens = state.cancel_tokens.lock().await;
        tokens.remove(&id);
    }

    let client = state.http_client.read().await.clone();
    let request_headers = parse_request_headers(task.extra_headers.as_deref())?;
    match downloader::prepare_hls_download(
        &client,
        &task.url,
        &request_headers,
        task.hls_selection.as_ref(),
    )
    .await?
    {
        downloader::PreparedHlsDownload::Single(prepared) => {
            if task.hls_output_mode != HlsOutputMode::SingleStream {
                return Err(AppError::InvalidInput(
                    "检测到远端轨道结构已变化，请重新创建下载任务".to_string(),
                ));
            }
            validate_segment_layout(&task, &prepared.segments)?;

            let updated_task = {
                let mut downloads = state.downloads.lock().await;
                downloads.entry(id.clone()).or_insert_with(|| task.clone());
                let task = downloads
                    .get_mut(&id)
                    .ok_or_else(|| AppError::InvalidInput(format!("Download {} not found", id)))?;
                task.status = DownloadStatus::Downloading;
                task.speed_bytes_per_sec = 0;
                task.completed_at = None;
                task.file_path = None;
                task.failed_segment_indices.clear();
                task.total_segments = prepared.segments.len();
                task.segment_uris = segment_uris(&prepared.segments);
                task.segment_durations = segment_durations(&prepared.segments);
                task.encryption_method = detect_encryption_method(&prepared.segments);
                task.hls_selection = prepared.selection.clone();
                task.hls_output_mode = HlsOutputMode::SingleStream;
                task.playback_available = true;
                task.touch();
                task.clone()
            };

            persistence::save_task(&app_handle, &updated_task).await?;
            let progress = task_to_progress(&updated_task);
            let _ = app_handle.emit("download-progress", &progress);

            start_download_worker(
                app_handle.clone(),
                state.downloads.clone(),
                state.cancel_tokens.clone(),
                state.playback_sessions.clone(),
                state.download_priorities.clone(),
                state.http_client.clone(),
                updated_task.clone(),
                prepared.segments,
                request_headers,
                state.max_concurrent_segments.clone(),
            )
            .await;

            Ok(persistence::task_to_summary(&updated_task))
        }
        downloader::PreparedHlsDownload::Bundle(prepared) => {
            if task.hls_output_mode != HlsOutputMode::MultiTrackBundle {
                return Err(AppError::InvalidInput(
                    "检测到远端轨道结构已变化，请重新创建下载任务".to_string(),
                ));
            }
            validate_bundle_layout(&task, &prepared.source_uris())?;
            let bundle_dir = task
                .file_path
                .clone()
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    resolve_bundle_output_dir(Path::new(&task.output_dir), &task.filename)
                });

            let updated_task = {
                let mut downloads = state.downloads.lock().await;
                downloads.entry(id.clone()).or_insert_with(|| task.clone());
                let task = downloads
                    .get_mut(&id)
                    .ok_or_else(|| AppError::InvalidInput(format!("Download {} not found", id)))?;
                task.status = DownloadStatus::Downloading;
                task.speed_bytes_per_sec = 0;
                task.completed_at = None;
                task.file_path = Some(bundle_dir.to_string_lossy().to_string());
                task.failed_segment_indices.clear();
                task.total_segments = prepared.total_units();
                task.segment_uris = prepared.source_uris();
                task.segment_durations = prepared.durations();
                task.encryption_method = prepared.encryption_method();
                task.hls_selection = Some(prepared.selection.clone());
                task.hls_output_mode = HlsOutputMode::MultiTrackBundle;
                task.playback_available = false;
                task.touch();
                task.clone()
            };

            persistence::save_task(&app_handle, &updated_task).await?;
            let progress = task_to_progress(&updated_task);
            let _ = app_handle.emit("download-progress", &progress);

            start_hls_bundle_download_worker(
                app_handle.clone(),
                state.downloads.clone(),
                state.cancel_tokens.clone(),
                state.http_client.clone(),
                updated_task.clone(),
                bundle_dir,
                prepared,
                request_headers,
                state.max_concurrent_segments.clone(),
            )
            .await;

            Ok(persistence::task_to_summary(&updated_task))
        }
    }
}

#[tauri::command]
pub async fn cancel_download(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), AppError> {
    let task = {
        let mut downloads = state.downloads.lock().await;
        let task = downloads
            .get_mut(&id)
            .ok_or_else(|| AppError::InvalidInput(format!("Download {} not found", id)))?;
        if task.status != DownloadStatus::Downloading && task.status != DownloadStatus::Paused {
            return Err(AppError::InvalidInput(
                "只有下载中或已暂停的任务可以取消".to_string(),
            ));
        }
        task.status = DownloadStatus::Cancelled;
        task.speed_bytes_per_sec = 0;
        task.touch();
        task.clone()
    };

    let token = {
        let mut tokens = state.cancel_tokens.lock().await;
        tokens.remove(&id)
    };

    if let Some(token) = token {
        token.cancel();
    } else if task.file_type.is_direct_download() {
        downloader::cleanup_mp4_partial_file(&PathBuf::from(&task.output_dir), &task.filename)
            .await?;
    } else {
        downloader::cleanup_temp_dir(&PathBuf::from(&task.output_dir), &task.id).await?;
    }

    playback::remove_download_priority_state(&state.download_priorities, &id).await;

    persistence::save_task(&app_handle, &task).await?;
    {
        let mut downloads = state.downloads.lock().await;
        downloads.remove(&id);
    }
    let _ = app_handle.emit("download-progress", &task_to_progress(&task));
    Ok(())
}

#[tauri::command]
pub async fn get_download_counts(
    app_handle: AppHandle,
    state: State<'_, AppState>,
) -> Result<DownloadCounts, AppError> {
    let active_count = state.downloads.lock().await.len();
    let mut counts = persistence::get_download_counts(&app_handle).await?;
    counts.active_count = active_count;
    Ok(counts)
}

#[tauri::command]
pub async fn get_downloads_page(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    group: DownloadGroup,
    page: usize,
    page_size: usize,
) -> Result<DownloadTaskPage, AppError> {
    if group == DownloadGroup::Active {
        return Ok(get_active_downloads_page(&state, page, page_size).await);
    }

    persistence::get_downloads_page(&app_handle, group, page, page_size).await
}

#[tauri::command]
pub async fn get_download_segment_state(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<DownloadTaskSegmentState, AppError> {
    if let Some(task) = {
        let downloads = state.downloads.lock().await;
        downloads.get(&id).cloned()
    } {
        return Ok(persistence::task_to_segment_state(&task));
    }

    persistence::load_download_segment_state(&app_handle, &id)
        .await?
        .ok_or_else(|| AppError::InvalidInput(format!("Download {} not found", id)))
}

#[tauri::command]
pub async fn get_download_summary(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<DownloadTaskSummary, AppError> {
    if let Some(task) = {
        let downloads = state.downloads.lock().await;
        downloads.get(&id).cloned()
    } {
        return Ok(persistence::task_to_summary(&task));
    }

    persistence::load_download_summary(&app_handle, &id)
        .await?
        .ok_or_else(|| AppError::InvalidInput(format!("Download {} not found", id)))
}

#[tauri::command]
pub async fn remove_download(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    id: String,
    delete_file: bool,
) -> Result<(), AppError> {
    {
        let mut tokens = state.cancel_tokens.lock().await;
        if let Some(token) = tokens.remove(&id) {
            token.cancel();
        }
    }

    let removed_task = {
        let mut downloads = state.downloads.lock().await;
        downloads.remove(&id)
    };

    let removed_task = if removed_task.is_some() {
        removed_task
    } else {
        persistence::load_download_task(&app_handle, &id).await?
    };

    if let Some(task) = removed_task {
        if let Some(session) =
            playback::remove_playback_session(&state.playback_sessions, &task.id).await
        {
            if let Some(window) = app_handle.get_webview_window(&session.window_label) {
                let _ = window.close();
            }
        }
        playback::remove_download_priority_state(&state.download_priorities, &task.id).await;
        if task.file_type.is_direct_download() {
            let _ = downloader::cleanup_mp4_partial_file(
                &PathBuf::from(&task.output_dir),
                &task.filename,
            )
            .await;
        } else if task.hls_output_mode == HlsOutputMode::SingleStream {
            let _ = downloader::cleanup_temp_dir(&PathBuf::from(&task.output_dir), &task.id).await;
        }
        if delete_file {
            if let Some(path) = &task.file_path {
                let file_path = PathBuf::from(path);
                if file_path.is_dir() {
                    let _ = tokio::fs::remove_dir_all(file_path).await;
                } else {
                    let _ = tokio::fs::remove_file(file_path).await;
                }
            }
        }
    }

    let _ = persistence::delete_task(&app_handle, &id).await?;

    Ok(())
}

#[tauri::command]
pub async fn clear_history_downloads(
    app_handle: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    let removed = persistence::clear_history_downloads(&app_handle).await?;

    for item in removed {
        if let Some(session) =
            playback::remove_playback_session(&state.playback_sessions, &item.id).await
        {
            if let Some(window) = app_handle.get_webview_window(&session.window_label) {
                let _ = window.close();
            }
        }
        playback::remove_download_priority_state(&state.download_priorities, &item.id).await;
    }

    Ok(())
}

#[tauri::command]
pub async fn get_default_download_dir(state: State<'_, AppState>) -> Result<String, AppError> {
    Ok(state.default_download_dir.lock().await.clone())
}

#[tauri::command]
pub async fn set_default_download_dir(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<(), AppError> {
    let path = path.trim();
    if path.is_empty() {
        return Err(AppError::InvalidInput("下载目录不能为空".to_string()));
    }

    {
        let mut default_download_dir = state.default_download_dir.lock().await;
        *default_download_dir = path.to_string();
    }

    persistence::update_settings(&app_handle, |settings| {
        settings.default_download_dir = Some(path.to_string());
    })
    .await;

    Ok(())
}

#[tauri::command]
pub async fn get_app_settings(state: State<'_, AppState>) -> Result<AppSettings, AppError> {
    Ok(AppSettings {
        default_download_dir: Some(state.default_download_dir.lock().await.clone()),
        proxy: state.proxy_settings.lock().await.clone(),
        download_concurrency: *state.max_concurrent_segments.lock().await,
        download_speed_limit_kbps: state.download_rate_limiter.limit_kbps().await,
        delete_ts_temp_dir_after_download: *state.delete_ts_temp_dir_after_download.lock().await,
        convert_to_mp4: *state.convert_to_mp4.lock().await,
        ffmpeg_enabled: *state.ffmpeg_enabled.lock().await,
        ffmpeg_path: state.ffmpeg_path.lock().await.clone(),
    })
}

#[tauri::command]
pub async fn set_proxy_settings(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    proxy: ProxySettings,
) -> Result<(), AppError> {
    let proxy_url = proxy.url.trim();
    if proxy.enabled && proxy_url.is_empty() {
        return Err(AppError::InvalidInput("代理地址不能为空".to_string()));
    }

    if proxy.enabled {
        let _ = downloader::build_http_client(Some(proxy_url))?;
    }

    let next_client = if proxy.enabled {
        downloader::build_http_client(Some(proxy_url))?
    } else {
        downloader::build_http_client(None)?
    };

    {
        let mut current_proxy = state.proxy_settings.lock().await;
        *current_proxy = ProxySettings {
            enabled: proxy.enabled,
            url: if proxy_url.is_empty() {
                current_proxy.url.clone()
            } else {
                proxy_url.to_string()
            },
        };
    }
    {
        let mut current_client = state.http_client.write().await;
        *current_client = next_client;
    }

    let saved_proxy = state.proxy_settings.lock().await.clone();
    persistence::update_settings(&app_handle, |settings| {
        settings.proxy = saved_proxy;
    })
    .await;

    Ok(())
}

#[tauri::command]
pub async fn set_download_concurrency(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    download_concurrency: usize,
) -> Result<(), AppError> {
    if !(MIN_DOWNLOAD_CONCURRENCY..=MAX_DOWNLOAD_CONCURRENCY).contains(&download_concurrency) {
        return Err(AppError::InvalidInput(format!(
            "下载并发数量必须在 {} 到 {} 之间",
            MIN_DOWNLOAD_CONCURRENCY, MAX_DOWNLOAD_CONCURRENCY
        )));
    }

    {
        let mut max_concurrent_segments = state.max_concurrent_segments.lock().await;
        *max_concurrent_segments = download_concurrency;
    }

    persistence::update_settings(&app_handle, |settings| {
        settings.download_concurrency = download_concurrency;
    })
    .await;

    Ok(())
}

#[tauri::command]
pub async fn set_download_speed_limit(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    download_speed_limit_kbps: u64,
) -> Result<(), AppError> {
    let normalized_limit = normalize_download_speed_limit_kbps(download_speed_limit_kbps);

    state
        .download_rate_limiter
        .set_limit_kbps(normalized_limit)
        .await;

    persistence::update_settings(&app_handle, |settings| {
        settings.download_speed_limit_kbps = normalized_limit;
    })
    .await;

    Ok(())
}

#[tauri::command]
pub async fn set_download_output_settings(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    delete_ts_temp_dir_after_download: bool,
    convert_to_mp4: bool,
) -> Result<(), AppError> {
    {
        let mut delete_temp = state.delete_ts_temp_dir_after_download.lock().await;
        *delete_temp = delete_ts_temp_dir_after_download;
    }
    {
        let mut convert = state.convert_to_mp4.lock().await;
        *convert = convert_to_mp4;
    }

    persistence::update_settings(&app_handle, |settings| {
        settings.delete_ts_temp_dir_after_download = delete_ts_temp_dir_after_download;
        settings.convert_to_mp4 = convert_to_mp4;
    })
    .await;

    Ok(())
}

#[tauri::command]
pub async fn open_download_playback_session(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<OpenPlaybackSessionResponse, AppError> {
    playback::playback_log(&format!("open playback session requested task_id={}", id));
    let task = get_or_load_task(&app_handle, &state, &id).await?;

    if !task.playback_available {
        return Err(AppError::InvalidInput("多轨下载暂不支持播放".to_string()));
    }

    if !playback::task_can_open_playback(&task) {
        return Err(AppError::InvalidInput(
            "只有下载中、已暂停或已完成的任务可以打开播放器".to_string(),
        ));
    }

    let task = if matches!(
        task.status,
        DownloadStatus::Downloading | DownloadStatus::Paused
    ) && task.file_type == FileType::Hls
    {
        ensure_task_playback_ready(&app_handle, &state, &id).await?
    } else {
        task
    };
    let (playback_kind, playback_path) = playback_target_for_task(&task)?;

    if playback_kind == PlaybackSourceKind::Hls {
        playback::ensure_download_priority_state(&state.download_priorities, &task).await;
    }

    let playback_server = state
        .playback_server
        .read()
        .await
        .clone()
        .ok_or_else(|| AppError::Internal("播放服务尚未初始化".to_string()))?;

    if let Some(session) = {
        let sessions = state.playback_sessions.lock().await;
        sessions.get(&id).cloned()
    } {
        playback::playback_log(&format!(
            "reuse playback session task_id={} window_label={} token_suffix={} mode={:?}",
            id,
            session.window_label,
            &session.session_token[session.session_token.len().saturating_sub(8)..],
            session.playback_kind
        ));
        return Ok(OpenPlaybackSessionResponse {
            window_label: session.window_label,
            playback_url: format!(
                "{}{}?token={}",
                playback_server.base_url, session.playback_path, session.session_token
            ),
            playback_kind: session.playback_kind,
            session_token: session.session_token,
            filename: task.filename,
            status: task.status,
        });
    }

    let session_token = Uuid::new_v4().to_string();
    let window_label = playback::playback_window_label(&task.id);
    let session = playback::PlaybackSession {
        task_id: task.id.clone(),
        session_token: session_token.clone(),
        window_label: window_label.clone(),
        playback_kind: playback_kind.clone(),
        playback_path: playback_path.clone(),
        task_snapshot: task.clone(),
        last_accessed_at: Utc::now(),
        active_client_count: 0,
    };

    {
        let mut sessions = state.playback_sessions.lock().await;
        sessions.insert(task.id.clone(), session);
    }
    playback::playback_log(&format!(
        "create playback session task_id={} window_label={} token_suffix={} mode={:?} playback_path={}",
        task.id,
        window_label,
        &session_token[session_token.len().saturating_sub(8)..],
        playback_kind,
        playback_path
    ));

    Ok(OpenPlaybackSessionResponse {
        window_label,
        playback_url: format!(
            "{}{}?token={}",
            playback_server.base_url, playback_path, session_token
        ),
        playback_kind,
        session_token,
        filename: task.filename,
        status: task.status,
    })
}

#[tauri::command]
pub async fn prioritize_download_playback_position(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    id: String,
    position_secs: f64,
) -> Result<(), AppError> {
    playback::playback_log(&format!(
        "frontend requested prioritize task_id={} position_secs={:.3}",
        id, position_secs
    ));
    let task = get_or_load_task(&app_handle, &state, &id).await?;

    match task.status {
        DownloadStatus::Downloading | DownloadStatus::Paused => {
            if task.file_type.is_direct_download() {
                Ok(())
            } else {
                playback::prioritize_download_position(
                    &state.download_priorities,
                    &task,
                    position_secs,
                )
                .await
            }
        }
        DownloadStatus::Completed | DownloadStatus::Merging | DownloadStatus::Converting => Ok(()),
        DownloadStatus::Cancelled => Err(AppError::InvalidInput("任务已取消".to_string())),
        DownloadStatus::Failed(message) => Err(AppError::InvalidInput(message)),
        DownloadStatus::Pending => Ok(()),
    }
}

#[tauri::command]
pub async fn close_download_playback_session(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    id: String,
    session_token: String,
) -> Result<(), AppError> {
    playback::playback_log(&format!(
        "close playback session requested task_id={} token_suffix={}",
        id,
        &session_token[session_token.len().saturating_sub(8)..]
    ));
    {
        let mut sessions = state.playback_sessions.lock().await;
        let Some(session) = sessions.get(&id) else {
            playback::playback_log(&format!(
                "close playback session skipped because session missing task_id={}",
                id
            ));
            return Ok(());
        };

        if session.session_token != session_token {
            playback::playback_log(&format!(
                "close playback session skipped because token mismatch task_id={}",
                id
            ));
            return Ok(());
        }

        sessions.remove(&id);
    }
    playback::playback_log(&format!("playback session closed task_id={}", id));

    maybe_cleanup_completed_temp_dir(&app_handle, &state, &id).await;
    Ok(())
}

#[tauri::command]
pub async fn open_file_location(path: String) -> Result<(), AppError> {
    let p = PathBuf::from(&path);
    let target = if p.is_dir() {
        p
    } else {
        p.parent().map(|parent| parent.to_path_buf()).unwrap_or(p)
    };

    if target.exists() {
        open::that(target).map_err(|e| AppError::Internal(e.to_string()))?;
    } else {
        return Err(AppError::InvalidInput("目录不存在".to_string()));
    }
    Ok(())
}

#[tauri::command]
pub async fn open_url(url: String) -> Result<(), AppError> {
    open::that(&url).map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(())
}

#[tauri::command]
pub async fn install_chromium_extension(
    app_handle: AppHandle,
    browser: ChromiumBrowser,
) -> Result<ChromiumExtensionInstallResult, AppError> {
    let extension_path = prepare_chrome_extension_install_dir(&app_handle).await?;

    Ok(ChromiumExtensionInstallResult {
        extension_path: normalize_display_path(&extension_path),
        manual_url: chromium_extensions_url(browser).to_string(),
    })
}

#[tauri::command]
pub async fn open_chromium_extensions_page(browser: ChromiumBrowser) -> Result<bool, AppError> {
    Ok(try_open_chromium_extensions_page(browser))
}

#[tauri::command]
pub async fn install_firefox_extension(
    app_handle: AppHandle,
) -> Result<FirefoxExtensionInstallResult, AppError> {
    let extension_path = prepare_firefox_extension_install_dir(&app_handle).await?;

    Ok(FirefoxExtensionInstallResult {
        extension_path: normalize_display_path(&extension_path),
        manual_url: FIREFOX_ADDONS_URL.to_string(),
    })
}

#[tauri::command]
pub async fn open_firefox_addons_page() -> Result<bool, AppError> {
    Ok(try_open_firefox_addons_page())
}

#[tauri::command]
pub async fn merge_ts_files(input_dir: String, output_path: String) -> Result<String, AppError> {
    let input_dir = PathBuf::from(input_dir.trim());
    let output_path = PathBuf::from(output_path.trim());

    if input_dir.as_os_str().is_empty() || !input_dir.is_dir() {
        return Err(AppError::InvalidInput("请选择有效的 ts 目录".to_string()));
    }
    if output_path.as_os_str().is_empty() {
        return Err(AppError::InvalidInput("输出文件不能为空".to_string()));
    }

    if let Some(parent) = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent).await?;
    }

    let resolved_output_path = downloader::resolve_available_file_path(&output_path);
    downloader::merge_ts_files_in_dir(&input_dir, &resolved_output_path).await?;
    Ok(resolved_output_path.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn convert_ts_to_mp4_file(
    app_handle: AppHandle,
    input_path: String,
    output_path: String,
) -> Result<String, AppError> {
    let input_path = PathBuf::from(input_path.trim());
    let output_path = PathBuf::from(output_path.trim());

    if input_path.as_os_str().is_empty() || !input_path.is_file() {
        return Err(AppError::InvalidInput("请选择有效的 ts 文件".to_string()));
    }
    if output_path.as_os_str().is_empty() {
        return Err(AppError::InvalidInput("输出文件不能为空".to_string()));
    }

    if let Some(parent) = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent).await?;
    }

    let ffmpeg_path = crate::ffmpeg::resolve_ffmpeg_path(&app_handle).await;
    let ffmpeg_enabled = *app_handle.state::<AppState>().ffmpeg_enabled.lock().await;
    let resolved_output_path = downloader::resolve_available_file_path(&output_path);
    downloader::convert_ts_to_mp4_file(
        &input_path,
        &resolved_output_path,
        false,
        ffmpeg_enabled,
        ffmpeg_path.as_deref(),
    )
    .await?;
    Ok(resolved_output_path.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn convert_local_m3u8_to_mp4_file(
    app_handle: AppHandle,
    input_path: String,
    output_path: String,
) -> Result<String, AppError> {
    let input_path = PathBuf::from(input_path.trim());
    let output_path = PathBuf::from(output_path.trim());

    if input_path.as_os_str().is_empty() || !input_path.is_file() {
        return Err(AppError::InvalidInput("请选择有效的 m3u8 文件".to_string()));
    }
    if output_path.as_os_str().is_empty() {
        return Err(AppError::InvalidInput("输出文件不能为空".to_string()));
    }

    if let Some(parent) = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent).await?;
    }

    let ffmpeg_path = crate::ffmpeg::resolve_ffmpeg_path(&app_handle).await;
    let ffmpeg_enabled = *app_handle.state::<AppState>().ffmpeg_enabled.lock().await;
    let resolved_output_path = downloader::resolve_available_file_path(&output_path);
    downloader::convert_local_m3u8_to_mp4_file(
        &input_path,
        &resolved_output_path,
        ffmpeg_enabled,
        ffmpeg_path.as_deref(),
    )
    .await?;
    Ok(resolved_output_path.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn convert_media_file(
    app_handle: AppHandle,
    input_path: String,
    output_path: String,
    target_format: String,
    convert_mode: String,
) -> Result<String, AppError> {
    let input_path = PathBuf::from(input_path.trim());
    let output_path = PathBuf::from(output_path.trim());
    let target_format = target_format.trim().to_lowercase();
    let convert_mode = convert_mode.trim().to_lowercase();

    if input_path.as_os_str().is_empty() || !input_path.is_file() {
        return Err(AppError::InvalidInput("请选择有效的媒体文件".to_string()));
    }
    if output_path.as_os_str().is_empty() {
        return Err(AppError::InvalidInput("输出文件不能为空".to_string()));
    }
    if target_format.is_empty() {
        return Err(AppError::InvalidInput("请选择目标格式".to_string()));
    }
    if convert_mode.is_empty() {
        return Err(AppError::InvalidInput("请选择转换模式".to_string()));
    }

    let ffmpeg_enabled = *app_handle.state::<AppState>().ffmpeg_enabled.lock().await;
    if !ffmpeg_enabled {
        return Err(AppError::InvalidInput(
            "FFmpeg 开关未开启，请先在设置 -> FFmpeg 中开启".to_string(),
        ));
    }
    let ffmpeg_path = crate::ffmpeg::resolve_ffmpeg_path(&app_handle)
        .await
        .ok_or_else(|| {
            AppError::InvalidInput(
                "未检测到可用的 FFmpeg，请先在设置 -> FFmpeg 中配置或下载 FFmpeg".to_string(),
            )
        })?;

    if let Some(parent) = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent).await?;
    }

    let resolved_output_path = downloader::resolve_available_file_path(&output_path);
    crate::ffmpeg::convert_media_file(
        &ffmpeg_path,
        &input_path,
        &resolved_output_path,
        &target_format,
        &convert_mode,
    )
    .await?;
    Ok(resolved_output_path.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn analyze_media_file(
    app_handle: AppHandle,
    input_path: String,
) -> Result<crate::ffmpeg::MediaAnalysisResult, AppError> {
    let input_path = PathBuf::from(input_path.trim());

    if input_path.as_os_str().is_empty() || !input_path.is_file() {
        return Err(AppError::InvalidInput("请选择有效的媒体文件".to_string()));
    }

    let ffmpeg_enabled = *app_handle.state::<AppState>().ffmpeg_enabled.lock().await;
    if !ffmpeg_enabled {
        return Err(AppError::InvalidInput(
            "FFmpeg 开关未开启，请先在设置 -> FFmpeg 中开启".to_string(),
        ));
    }
    let ffmpeg_path = crate::ffmpeg::resolve_ffmpeg_path(&app_handle)
        .await
        .ok_or_else(|| {
            AppError::InvalidInput(
                "未检测到可用的 FFmpeg，请先在设置 -> FFmpeg 中配置或下载 FFmpeg".to_string(),
            )
        })?;

    crate::ffmpeg::analyze_media_file(&ffmpeg_path, &input_path).await
}

#[tauri::command]
pub async fn transcode_media_file(
    app_handle: AppHandle,
    input_path: String,
    output_path: String,
    output_format: String,
    video_codec: String,
    audio_codec: String,
) -> Result<String, AppError> {
    let input_path = PathBuf::from(input_path.trim());
    let output_path = PathBuf::from(output_path.trim());
    let output_format = output_format.trim().to_lowercase();
    let video_codec = video_codec.trim().to_lowercase();
    let audio_codec = audio_codec.trim().to_lowercase();

    if input_path.as_os_str().is_empty() || !input_path.is_file() {
        return Err(AppError::InvalidInput("请选择有效的媒体文件".to_string()));
    }
    if output_path.as_os_str().is_empty() {
        return Err(AppError::InvalidInput("输出文件不能为空".to_string()));
    }
    if output_format.is_empty() {
        return Err(AppError::InvalidInput("请选择输出格式".to_string()));
    }
    if video_codec.is_empty() {
        return Err(AppError::InvalidInput("请选择视频编码".to_string()));
    }
    if audio_codec.is_empty() {
        return Err(AppError::InvalidInput("请选择音频编码".to_string()));
    }

    let ffmpeg_enabled = *app_handle.state::<AppState>().ffmpeg_enabled.lock().await;
    if !ffmpeg_enabled {
        return Err(AppError::InvalidInput(
            "FFmpeg 开关未开启，请先在设置 -> FFmpeg 中开启".to_string(),
        ));
    }
    let ffmpeg_path = crate::ffmpeg::resolve_ffmpeg_path(&app_handle)
        .await
        .ok_or_else(|| {
            AppError::InvalidInput(
                "未检测到可用的 FFmpeg，请先在设置 -> FFmpeg 中配置或下载 FFmpeg".to_string(),
            )
        })?;

    if let Some(parent) = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent).await?;
    }

    let resolved_output_path = downloader::resolve_available_file_path(&output_path);
    crate::ffmpeg::transcode_media_file(
        &ffmpeg_path,
        &input_path,
        &resolved_output_path,
        &output_format,
        &video_codec,
        &audio_codec,
    )
    .await?;
    Ok(resolved_output_path.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn merge_video_files(
    app_handle: AppHandle,
    input_paths: Vec<String>,
    output_path: String,
    merge_mode: String,
) -> Result<String, AppError> {
    if input_paths.len() < 2 {
        return Err(AppError::InvalidInput("请至少选择两个视频文件".to_string()));
    }

    let mut resolved_inputs = Vec::with_capacity(input_paths.len());
    for input_path in input_paths {
        let path = PathBuf::from(input_path.trim());
        if path.as_os_str().is_empty() || !path.is_file() {
            return Err(AppError::InvalidInput("请选择有效的视频文件".to_string()));
        }
        resolved_inputs.push(path);
    }

    let output_path = PathBuf::from(output_path.trim());
    let merge_mode = merge_mode.trim().to_lowercase();
    if output_path.as_os_str().is_empty() {
        return Err(AppError::InvalidInput("输出文件不能为空".to_string()));
    }
    if !matches!(merge_mode.as_str(), "fast" | "compatible") {
        return Err(AppError::InvalidInput("请选择有效的合并模式".to_string()));
    }

    let ffmpeg_enabled = *app_handle.state::<AppState>().ffmpeg_enabled.lock().await;
    if !ffmpeg_enabled {
        return Err(AppError::InvalidInput(
            "FFmpeg 开关未开启，请先在设置 -> FFmpeg 中开启".to_string(),
        ));
    }
    let ffmpeg_path = crate::ffmpeg::resolve_ffmpeg_path(&app_handle)
        .await
        .ok_or_else(|| {
            AppError::InvalidInput(
                "未检测到可用的 FFmpeg，请先在设置 -> FFmpeg 中配置或下载 FFmpeg".to_string(),
            )
        })?;

    if let Some(parent) = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent).await?;
    }

    let resolved_output_path = downloader::resolve_available_file_path(&output_path);
    crate::ffmpeg::merge_video_files(
        &ffmpeg_path,
        &resolved_inputs,
        &resolved_output_path,
        &merge_mode,
    )
    .await?;
    Ok(resolved_output_path.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn convert_multi_track_hls_to_mp4_dir(
    app_handle: AppHandle,
    input_dir: String,
    output_path: String,
) -> Result<String, AppError> {
    let input_dir = PathBuf::from(input_dir.trim());
    let output_path = PathBuf::from(output_path.trim());

    if input_dir.as_os_str().is_empty() || !input_dir.is_dir() {
        return Err(AppError::InvalidInput(
            "请选择有效的多轨 HLS 目录".to_string(),
        ));
    }
    if output_path.as_os_str().is_empty() {
        return Err(AppError::InvalidInput("输出文件不能为空".to_string()));
    }

    let bundle = resolve_local_hls_bundle_paths(&input_dir)?;
    let ffmpeg_enabled = *app_handle.state::<AppState>().ffmpeg_enabled.lock().await;
    if !ffmpeg_enabled {
        return Err(AppError::InvalidInput(
            "FFmpeg 开关未开启，请先在设置 -> FFmpeg 中开启".to_string(),
        ));
    }
    let ffmpeg_path = crate::ffmpeg::resolve_ffmpeg_path(&app_handle)
        .await
        .ok_or_else(|| {
            AppError::InvalidInput(
                "未检测到可用的 FFmpeg，请先在设置 -> FFmpeg 中配置或下载 FFmpeg".to_string(),
            )
        })?;

    if let Some(parent) = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent).await?;
    }

    let resolved_output_path = downloader::resolve_available_file_path(&output_path);
    crate::ffmpeg::convert_multi_track_hls_to_mp4(
        &ffmpeg_path,
        &bundle.video_playlist,
        bundle.audio_playlist.as_deref(),
        bundle.subtitle_playlist.as_deref(),
        &resolved_output_path,
    )
    .await?;
    Ok(resolved_output_path.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn get_ffmpeg_status(
    app_handle: AppHandle,
) -> Result<crate::ffmpeg::FfmpegStatus, AppError> {
    Ok(crate::ffmpeg::detect_ffmpeg(&app_handle).await)
}

#[tauri::command]
pub async fn download_ffmpeg(app_handle: AppHandle) -> Result<String, AppError> {
    let path = crate::ffmpeg::download_ffmpeg(app_handle).await?;
    Ok(path.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn set_ffmpeg_path(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    path: Option<String>,
) -> Result<crate::ffmpeg::FfmpegStatus, AppError> {
    {
        let mut ffmpeg_path = state.ffmpeg_path.lock().await;
        *ffmpeg_path = path.clone();
    }
    persistence::update_settings(&app_handle, |settings| {
        settings.ffmpeg_path = path;
    })
    .await;
    Ok(crate::ffmpeg::detect_ffmpeg(&app_handle).await)
}

#[tauri::command]
pub async fn set_ffmpeg_enabled(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<(), AppError> {
    {
        let mut ffmpeg_enabled = state.ffmpeg_enabled.lock().await;
        *ffmpeg_enabled = enabled;
    }
    persistence::update_settings(&app_handle, |settings| {
        settings.ffmpeg_enabled = enabled;
    })
    .await;
    Ok(())
}

async fn ensure_task_playback_ready(
    app_handle: &AppHandle,
    state: &State<'_, AppState>,
    id: &str,
) -> Result<DownloadTask, AppError> {
    playback::playback_log(&format!("ensure task playback ready task_id={}", id));
    let task = get_or_load_task(app_handle, state, id).await?;

    if task.segment_durations.len() == task.total_segments && !task.segment_durations.is_empty() {
        playback::playback_log(&format!(
            "task already has playback metadata task_id={} segments={}",
            id, task.total_segments
        ));
        return Ok(task);
    }

    let client = state.http_client.read().await.clone();
    let request_headers = parse_request_headers(task.extra_headers.as_deref())?;
    let segments = match downloader::prepare_hls_download(
        &client,
        &task.url,
        &request_headers,
        task.hls_selection.as_ref(),
    )
    .await?
    {
        downloader::PreparedHlsDownload::Single(prepared) => prepared.segments,
        downloader::PreparedHlsDownload::Bundle(_) => {
            return Err(AppError::InvalidInput("多轨下载暂不支持播放".to_string()))
        }
    };
    validate_segment_layout(&task, &segments)?;
    playback::playback_log(&format!(
        "reloaded playback metadata task_id={} segments={}",
        id,
        segments.len()
    ));

    let refreshed_task = {
        let mut downloads = state.downloads.lock().await;
        downloads
            .entry(id.to_string())
            .or_insert_with(|| task.clone());
        let task = downloads
            .get_mut(id)
            .ok_or_else(|| AppError::InvalidInput(format!("Download {} not found", id)))?;
        task.segment_uris = segment_uris(&segments);
        task.segment_durations = segment_durations(&segments);
        task.encryption_method = detect_encryption_method(&segments);
        task.touch();
        task.clone()
    };

    persistence::save_task(app_handle, &refreshed_task).await?;
    Ok(refreshed_task)
}

fn playback_target_for_task(task: &DownloadTask) -> Result<(PlaybackSourceKind, String), AppError> {
    if !task.playback_available {
        return Err(AppError::InvalidInput("多轨下载暂不支持播放".to_string()));
    }

    match task.status {
        DownloadStatus::Completed => {
            let file_path = task
                .file_path
                .as_ref()
                .ok_or_else(|| AppError::InvalidInput("下载完成文件不存在".to_string()))?;
            if !std::path::Path::new(file_path).is_file() {
                return Err(AppError::InvalidInput("下载完成文件不存在".to_string()));
            }
            Ok((PlaybackSourceKind::File, playback::file_path(&task.id)))
        }
        DownloadStatus::Downloading | DownloadStatus::Paused if task.file_type == FileType::Hls => {
            Ok((PlaybackSourceKind::Hls, playback::playlist_path(&task.id)))
        }
        DownloadStatus::Downloading | DownloadStatus::Paused
            if task.file_type.supports_progressive_playback() =>
        {
            Ok((PlaybackSourceKind::File, playback::file_path(&task.id)))
        }
        DownloadStatus::Downloading | DownloadStatus::Paused => Err(AppError::InvalidInput(
            "当前格式暂不支持边下边播，请等待下载完成后再播放".to_string(),
        )),
        _ => Err(AppError::InvalidInput("当前任务状态不支持播放".to_string())),
    }
}

async fn maybe_cleanup_completed_temp_dir(
    app_handle: &AppHandle,
    state: &State<'_, AppState>,
    id: &str,
) {
    if playback::has_active_playback_session(&state.playback_sessions, id).await {
        playback::playback_log(&format!(
            "skip temp cleanup because playback session active task_id={}",
            id
        ));
        return;
    }

    let delete_temp = *state.delete_ts_temp_dir_after_download.lock().await;
    if !delete_temp {
        playback::playback_log(&format!(
            "skip temp cleanup because delete setting disabled task_id={}",
            id
        ));
        return;
    }

    let task = {
        let downloads = state.downloads.lock().await;
        downloads.get(id).cloned()
    };

    let task = if let Some(task) = task {
        Some(task)
    } else {
        persistence::load_download_task(app_handle, id)
            .await
            .ok()
            .flatten()
    };

    let Some(task) = task else {
        return;
    };

    if matches!(task.status, DownloadStatus::Completed) {
        playback::playback_log(&format!("cleanup completed temp dir task_id={}", id));
        let _ = downloader::cleanup_temp_dir(&PathBuf::from(&task.output_dir), &task.id).await;
    }
}

fn segment_uris(segments: &[SegmentInfo]) -> Vec<String> {
    segments.iter().map(|segment| segment.uri.clone()).collect()
}

fn segment_durations(segments: &[SegmentInfo]) -> Vec<f32> {
    segments.iter().map(|segment| segment.duration).collect()
}

fn detect_encryption_method(segments: &[SegmentInfo]) -> Option<String> {
    segments
        .iter()
        .find_map(|segment| segment.encryption.as_ref())
        .map(|encryption| encryption.method.clone())
}

fn comparable_segment_path(uri: &str) -> String {
    if let Ok(parsed) = url::Url::parse(uri) {
        parsed.path().to_string()
    } else {
        uri.split('?').next().unwrap_or(uri).to_string()
    }
}

fn validate_segment_layout(task: &DownloadTask, segments: &[SegmentInfo]) -> Result<(), AppError> {
    let current_uris = segment_uris(segments);
    validate_uri_layout(task, &current_uris)
}

fn validate_bundle_layout(task: &DownloadTask, current_uris: &[String]) -> Result<(), AppError> {
    validate_uri_layout(task, current_uris)
}

fn validate_uri_layout(task: &DownloadTask, current_uris: &[String]) -> Result<(), AppError> {
    let current_uris = current_uris
        .iter()
        .map(|uri| comparable_segment_path(uri))
        .collect::<Vec<_>>();
    let stored_uris = task
        .segment_uris
        .iter()
        .map(|uri| comparable_segment_path(uri))
        .collect::<Vec<_>>();

    if !stored_uris.is_empty() && stored_uris != current_uris {
        return Err(AppError::InvalidInput(
            "检测到远端分片结构已变化，请重新创建下载任务".to_string(),
        ));
    }
    Ok(())
}

fn resolve_bundle_output_dir(output_dir: &Path, filename: &str) -> PathBuf {
    let candidate = output_dir.join(format!("{}_tracks", filename));
    downloader::resolve_available_file_path(&candidate)
}

#[derive(Debug)]
struct LocalHlsBundlePaths {
    video_playlist: PathBuf,
    audio_playlist: Option<PathBuf>,
    subtitle_playlist: Option<PathBuf>,
}

fn resolve_local_hls_bundle_paths(input_dir: &Path) -> Result<LocalHlsBundlePaths, AppError> {
    let video_playlist = required_track_playlist_path(input_dir, "video", "视频")?;
    let audio_playlist = optional_track_playlist_path(input_dir, "audio", "音频")?;
    let subtitle_playlist = optional_track_playlist_path(input_dir, "subtitle", "字幕")?;

    if audio_playlist.is_none() && subtitle_playlist.is_none() {
        return Err(AppError::InvalidInput(
            "所选目录不是有效的多轨 HLS 目录，至少需要音频或字幕轨道".to_string(),
        ));
    }

    Ok(LocalHlsBundlePaths {
        video_playlist,
        audio_playlist,
        subtitle_playlist,
    })
}

fn required_track_playlist_path(
    input_dir: &Path,
    track_dir_name: &str,
    track_label: &str,
) -> Result<PathBuf, AppError> {
    let track_dir = input_dir.join(track_dir_name);
    let playlist_path = track_dir.join("index.m3u8");

    if !track_dir.is_dir() || !playlist_path.is_file() {
        return Err(AppError::InvalidInput(format!(
            "所选目录缺少 {} 轨道的 index.m3u8",
            track_label
        )));
    }

    Ok(playlist_path)
}

fn optional_track_playlist_path(
    input_dir: &Path,
    track_dir_name: &str,
    track_label: &str,
) -> Result<Option<PathBuf>, AppError> {
    let track_dir = input_dir.join(track_dir_name);
    let playlist_path = track_dir.join("index.m3u8");

    if !track_dir.exists() {
        return Ok(None);
    }
    if !track_dir.is_dir() || !playlist_path.is_file() {
        return Err(AppError::InvalidInput(format!(
            "所选目录中的 {} 轨道缺少 index.m3u8",
            track_label
        )));
    }

    Ok(Some(playlist_path))
}

fn task_to_progress(task: &DownloadTask) -> DownloadProgressEvent {
    DownloadProgressEvent {
        id: task.id.clone(),
        status: task.status.clone(),
        group: download_group_for_status(&task.status),
        completed_segments: task.completed_segments,
        total_segments: task.total_segments,
        failed_segment_count: task.failed_segment_indices.len(),
        total_bytes: task.total_bytes,
        speed_bytes_per_sec: task.speed_bytes_per_sec,
        percentage: if matches!(task.status, DownloadStatus::Completed) {
            100.0
        } else if task.total_segments > 0 {
            (task.completed_segments as f64 / task.total_segments as f64) * 100.0
        } else {
            0.0
        },
        updated_at: task.last_updated_at().to_rfc3339(),
    }
}

async fn get_or_load_task(
    app_handle: &AppHandle,
    state: &State<'_, AppState>,
    id: &str,
) -> Result<DownloadTask, AppError> {
    if let Some(task) = {
        let downloads = state.downloads.lock().await;
        downloads.get(id).cloned()
    } {
        return Ok(task);
    }

    persistence::load_download_task(app_handle, id)
        .await?
        .ok_or_else(|| AppError::InvalidInput(format!("Download {} not found", id)))
}

async fn get_active_downloads_page(
    state: &State<'_, AppState>,
    page: usize,
    page_size: usize,
) -> DownloadTaskPage {
    let downloads = state.downloads.lock().await;
    let mut items = downloads
        .values()
        .map(persistence::task_to_summary)
        .collect::<Vec<_>>();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    let safe_page = page.max(1);
    let safe_page_size = page_size.max(1);
    let total = items.len();
    let start = safe_page.saturating_sub(1) * safe_page_size;
    let paged_items = if start >= total {
        Vec::new()
    } else {
        items
            .into_iter()
            .skip(start)
            .take(safe_page_size)
            .collect::<Vec<_>>()
    };

    DownloadTaskPage {
        items: paged_items,
        total,
        page: safe_page,
        page_size: safe_page_size,
    }
}

async fn start_mp4_download_worker(
    app_handle: AppHandle,
    state_downloads: Arc<Mutex<HashMap<DownloadId, DownloadTask>>>,
    state_cancel_tokens: Arc<Mutex<HashMap<DownloadId, CancellationToken>>>,
    client: Arc<RwLock<reqwest::Client>>,
    task: DownloadTask,
    request_headers: RequestHeaders,
    resume_existing_partial: bool,
    restart_confirmed: bool,
) {
    let task_id = task.id.clone();
    let output_dir_path = PathBuf::from(&task.output_dir);
    let filename = task.filename.clone();
    let url = task.url.clone();
    let cancel_token = CancellationToken::new();
    let rate_limiter = app_handle.state::<AppState>().download_rate_limiter.clone();

    {
        let mut tokens = state_cancel_tokens.lock().await;
        tokens.insert(task_id.clone(), cancel_token.clone());
    }

    tokio::spawn(async move {
        let result = downloader::run_mp4_download(
            app_handle.clone(),
            state_downloads.clone(),
            client,
            rate_limiter,
            task_id.clone(),
            url,
            Arc::new(request_headers),
            output_dir_path,
            filename,
            resume_existing_partial,
            restart_confirmed,
            cancel_token.clone(),
        )
        .await;

        let mut should_save = false;
        let mut progress_to_emit = None;
        let mut remove_from_runtime = false;

        {
            let mut downloads = state_downloads.lock().await;
            if let Some(task) = downloads.get_mut(&task_id) {
                match result {
                    Ok(downloader::DownloadRunOutcome::Completed(final_path)) => {
                        let completed_at = Utc::now();
                        let final_size = final_path.metadata().map(|metadata| metadata.len()).ok();
                        task.status = DownloadStatus::Completed;
                        task.completed_at = Some(completed_at);
                        task.updated_at = Some(completed_at);
                        task.speed_bytes_per_sec = 0;
                        task.file_path = Some(final_path.to_string_lossy().to_string());
                        if let Some(final_size) = final_size {
                            task.total_bytes = final_size;
                            task.completed_segments = task.total_segments;
                        }
                        if let Some(name) = final_path.file_name() {
                            task.filename = name.to_string_lossy().to_string();
                        }
                        progress_to_emit = Some(task_to_progress(task));
                        should_save = true;
                        remove_from_runtime = true;
                    }
                    Ok(downloader::DownloadRunOutcome::Incomplete) => {
                        task.speed_bytes_per_sec = 0;
                        task.touch();
                        progress_to_emit = Some(task_to_progress(task));
                        should_save = true;
                    }
                    Err(AppError::Cancelled) => {
                        task.speed_bytes_per_sec = 0;
                        if task.status == DownloadStatus::Paused
                            || task.status == DownloadStatus::Cancelled
                        {
                            task.touch();
                            progress_to_emit = Some(task_to_progress(task));
                            should_save = true;
                        } else {
                            task.status = DownloadStatus::Cancelled;
                            task.touch();
                            progress_to_emit = Some(task_to_progress(task));
                            should_save = true;
                            remove_from_runtime = true;
                        }
                    }
                    Err(error) => {
                        if task.status != DownloadStatus::Cancelled {
                            task.status = DownloadStatus::Failed(error.to_string());
                        }
                        task.speed_bytes_per_sec = 0;
                        task.touch();
                        progress_to_emit = Some(task_to_progress(task));
                        should_save = true;
                        remove_from_runtime = true;
                    }
                }
            }
        }

        if should_save {
            let task = {
                let downloads = state_downloads.lock().await;
                downloads.get(&task_id).cloned()
            };

            if let Some(task) = task {
                let _ = persistence::save_task(&app_handle, &task).await;
            }
        }
        if remove_from_runtime {
            let mut downloads = state_downloads.lock().await;
            downloads.remove(&task_id);
        }

        if let Some(progress) = progress_to_emit {
            let _ = app_handle.emit("download-progress", &progress);
        }

        let final_status = {
            let downloads = state_downloads.lock().await;
            downloads.get(&task_id).map(|task| task.status.clone())
        };

        if !matches!(final_status, Some(DownloadStatus::Downloading)) {
            let mut tokens = state_cancel_tokens.lock().await;
            tokens.remove(&task_id);
        }
    });
}

async fn start_download_worker(
    app_handle: AppHandle,
    state_downloads: Arc<Mutex<HashMap<DownloadId, DownloadTask>>>,
    state_cancel_tokens: Arc<Mutex<HashMap<DownloadId, CancellationToken>>>,
    state_playback_sessions: Arc<Mutex<HashMap<DownloadId, playback::PlaybackSession>>>,
    state_download_priorities: Arc<
        Mutex<HashMap<DownloadId, Arc<playback::DownloadPriorityState>>>,
    >,
    client: Arc<RwLock<reqwest::Client>>,
    task: DownloadTask,
    segments: Vec<SegmentInfo>,
    request_headers: RequestHeaders,
    max_concurrent: Arc<Mutex<usize>>,
) {
    let task_id = task.id.clone();
    let output_dir_path = PathBuf::from(&task.output_dir);
    let filename = task.filename.clone();
    let cancel_token = CancellationToken::new();
    let delete_ts_temp_dir_after_download = *app_handle
        .state::<AppState>()
        .delete_ts_temp_dir_after_download
        .lock()
        .await;
    let convert_to_mp4 = *app_handle.state::<AppState>().convert_to_mp4.lock().await;
    let ffmpeg_enabled = *app_handle.state::<AppState>().ffmpeg_enabled.lock().await;
    let ffmpeg_path = if ffmpeg_enabled {
        crate::ffmpeg::resolve_ffmpeg_path(&app_handle).await
    } else {
        None
    };
    let rate_limiter = app_handle.state::<AppState>().download_rate_limiter.clone();

    {
        let mut tokens = state_cancel_tokens.lock().await;
        tokens.insert(task_id.clone(), cancel_token.clone());
    }

    tokio::spawn(async move {
        let result = downloader::run_download(
            app_handle.clone(),
            state_downloads.clone(),
            client,
            rate_limiter,
            task_id.clone(),
            segments,
            Arc::new(request_headers),
            output_dir_path.clone(),
            filename,
            delete_ts_temp_dir_after_download,
            state_playback_sessions.clone(),
            state_download_priorities.clone(),
            convert_to_mp4,
            ffmpeg_path,
            cancel_token,
            max_concurrent,
        )
        .await;

        let mut should_save = false;
        let mut progress_to_emit = None;
        let mut remove_from_runtime = false;

        {
            let mut downloads = state_downloads.lock().await;
            if let Some(task) = downloads.get_mut(&task_id) {
                match result {
                    Ok(downloader::DownloadRunOutcome::Completed(final_path)) => {
                        let completed_at = Utc::now();
                        let final_size = final_path.metadata().map(|metadata| metadata.len()).ok();
                        task.status = DownloadStatus::Completed;
                        task.completed_at = Some(completed_at);
                        task.updated_at = Some(completed_at);
                        task.completed_segments = task.total_segments;
                        task.speed_bytes_per_sec = 0;
                        task.file_path = Some(final_path.to_string_lossy().to_string());
                        if let Some(final_size) = final_size {
                            task.total_bytes = final_size;
                        }
                        if let Some(name) = final_path.file_name() {
                            task.filename = name.to_string_lossy().to_string();
                        }
                        progress_to_emit = Some(task_to_progress(task));
                        should_save = true;
                        remove_from_runtime = true;
                    }
                    Ok(downloader::DownloadRunOutcome::Incomplete) => {
                        task.speed_bytes_per_sec = 0;
                        task.touch();
                        progress_to_emit = Some(task_to_progress(task));
                        should_save = true;
                    }
                    Err(AppError::Cancelled) => {
                        task.speed_bytes_per_sec = 0;
                        if task.status == DownloadStatus::Paused
                            || task.status == DownloadStatus::Cancelled
                        {
                            task.touch();
                            progress_to_emit = Some(task_to_progress(task));
                            should_save = true;
                        } else {
                            task.status = DownloadStatus::Cancelled;
                            task.touch();
                            progress_to_emit = Some(task_to_progress(task));
                            should_save = true;
                            remove_from_runtime = true;
                        }
                    }
                    Err(error) => {
                        if task.status != DownloadStatus::Cancelled {
                            task.status = DownloadStatus::Failed(error.to_string());
                        }
                        task.speed_bytes_per_sec = 0;
                        task.touch();
                        progress_to_emit = Some(task_to_progress(task));
                        should_save = true;
                        remove_from_runtime = true;
                    }
                }
            }
        }

        if should_save {
            let task = {
                let downloads = state_downloads.lock().await;
                downloads.get(&task_id).cloned()
            };

            if let Some(task) = task {
                let _ = persistence::save_task(&app_handle, &task).await;
            }
        }
        if remove_from_runtime {
            let mut downloads = state_downloads.lock().await;
            downloads.remove(&task_id);
        }
        if let Some(progress) = progress_to_emit {
            let _ = app_handle.emit("download-progress", &progress);
        }

        let final_status = {
            let downloads = state_downloads.lock().await;
            downloads.get(&task_id).map(|task| task.status.clone())
        };

        if !matches!(final_status, Some(DownloadStatus::Downloading)) {
            let mut tokens = state_cancel_tokens.lock().await;
            tokens.remove(&task_id);
        }

        match final_status {
            Some(DownloadStatus::Paused) => {}
            Some(DownloadStatus::Downloading) | Some(DownloadStatus::Pending) => {}
            Some(_) | None => {
                playback::remove_download_priority_state(&state_download_priorities, &task_id)
                    .await;
            }
        }

        if matches!(final_status, Some(DownloadStatus::Completed)) {
            let app_state = app_handle.state::<AppState>();
            maybe_cleanup_completed_temp_dir(&app_handle, &app_state, &task_id).await;
        }
    });
}

async fn start_hls_bundle_download_worker(
    app_handle: AppHandle,
    state_downloads: Arc<Mutex<HashMap<DownloadId, DownloadTask>>>,
    state_cancel_tokens: Arc<Mutex<HashMap<DownloadId, CancellationToken>>>,
    client: Arc<RwLock<reqwest::Client>>,
    task: DownloadTask,
    bundle_dir: PathBuf,
    prepared: downloader::PreparedBundleHlsDownload,
    request_headers: RequestHeaders,
    max_concurrent: Arc<Mutex<usize>>,
) {
    let task_id = task.id.clone();
    let cancel_token = CancellationToken::new();
    let rate_limiter = app_handle.state::<AppState>().download_rate_limiter.clone();
    let output_dir_path = PathBuf::from(&task.output_dir);
    let filename = task.filename.clone();
    let convert_to_mp4 = *app_handle.state::<AppState>().convert_to_mp4.lock().await;
    let ffmpeg_enabled = *app_handle.state::<AppState>().ffmpeg_enabled.lock().await;
    let ffmpeg_path = if ffmpeg_enabled {
        crate::ffmpeg::resolve_ffmpeg_path(&app_handle).await
    } else {
        None
    };

    {
        let mut tokens = state_cancel_tokens.lock().await;
        tokens.insert(task_id.clone(), cancel_token.clone());
    }

    tokio::spawn(async move {
        let result = downloader::run_hls_bundle_download(
            app_handle.clone(),
            state_downloads.clone(),
            client,
            rate_limiter,
            task_id.clone(),
            output_dir_path,
            filename,
            bundle_dir.clone(),
            prepared.playlist_files,
            prepared.entries,
            Arc::new(request_headers),
            convert_to_mp4,
            ffmpeg_path,
            cancel_token,
            max_concurrent,
        )
        .await;

        let mut should_save = false;
        let mut progress_to_emit = None;
        let mut remove_from_runtime = false;

        {
            let mut downloads = state_downloads.lock().await;
            if let Some(task) = downloads.get_mut(&task_id) {
                match result {
                    Ok(downloader::DownloadRunOutcome::Completed(final_path)) => {
                        let completed_at = Utc::now();
                        let final_size = final_path.metadata().map(|metadata| metadata.len()).ok();
                        task.status = DownloadStatus::Completed;
                        task.completed_at = Some(completed_at);
                        task.updated_at = Some(completed_at);
                        task.completed_segments = task.total_segments;
                        task.speed_bytes_per_sec = 0;
                        task.file_path = Some(final_path.to_string_lossy().to_string());
                        task.playback_available = final_path.is_file();
                        if let Some(final_size) = final_size {
                            task.total_bytes = final_size;
                        }
                        if let Some(name) = final_path.file_name() {
                            task.filename = name.to_string_lossy().to_string();
                        }
                        progress_to_emit = Some(task_to_progress(task));
                        should_save = true;
                        remove_from_runtime = true;
                    }
                    Ok(downloader::DownloadRunOutcome::Incomplete) => {
                        task.speed_bytes_per_sec = 0;
                        task.touch();
                        progress_to_emit = Some(task_to_progress(task));
                        should_save = true;
                    }
                    Err(AppError::Cancelled) => {
                        task.speed_bytes_per_sec = 0;
                        if task.status == DownloadStatus::Paused
                            || task.status == DownloadStatus::Cancelled
                        {
                            task.touch();
                            progress_to_emit = Some(task_to_progress(task));
                            should_save = true;
                        } else {
                            task.status = DownloadStatus::Cancelled;
                            task.touch();
                            progress_to_emit = Some(task_to_progress(task));
                            should_save = true;
                            remove_from_runtime = true;
                        }
                    }
                    Err(error) => {
                        if task.status != DownloadStatus::Cancelled {
                            task.status = DownloadStatus::Failed(error.to_string());
                        }
                        task.speed_bytes_per_sec = 0;
                        task.touch();
                        progress_to_emit = Some(task_to_progress(task));
                        should_save = true;
                        remove_from_runtime = true;
                    }
                }
            }
        }

        if should_save {
            let task = {
                let downloads = state_downloads.lock().await;
                downloads.get(&task_id).cloned()
            };

            if let Some(task) = task {
                let _ = persistence::save_task(&app_handle, &task).await;
            }
        }
        if remove_from_runtime {
            let mut downloads = state_downloads.lock().await;
            downloads.remove(&task_id);
        }
        if let Some(progress) = progress_to_emit {
            let _ = app_handle.emit("download-progress", &progress);
        }

        let final_status = {
            let downloads = state_downloads.lock().await;
            downloads.get(&task_id).map(|task| task.status.clone())
        };

        if !matches!(final_status, Some(DownloadStatus::Downloading)) {
            let mut tokens = state_cancel_tokens.lock().await;
            tokens.remove(&task_id);
        }
    });
}

fn derive_filename_from_url(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| {
            let query_name = ["title", "name", "filename", "file", "videoTitle"]
                .into_iter()
                .find_map(|key| {
                    u.query_pairs()
                        .find(|(k, _)| k.eq_ignore_ascii_case(key))
                        .map(|(_, v)| v.into_owned())
                });

            query_name.or_else(|| {
                u.path_segments()
                    .and_then(|segs| segs.last().map(|s| s.to_string()))
            })
        })
        .map(normalize_download_filename)
        .unwrap_or_else(|| "download".to_string())
}

fn resolve_create_download_file_type(params: &CreateDownloadParams) -> Result<FileType, AppError> {
    if let Some(download_mode) = params.download_mode {
        return match download_mode {
            DownloadMode::Hls => Ok(FileType::Hls),
            DownloadMode::Direct => infer_direct_file_type_from_url(&params.url)
                .or_else(|| params.file_type.filter(|file_type| file_type.is_direct_download()))
                .ok_or_else(|| {
                    AppError::InvalidInput(
                        "无法从地址推断 Direct 文件类型，请使用包含 mp4/mkv/avi/wmv/flv/webm/mov/rmvb 后缀的链接"
                            .to_string(),
                    )
                }),
        };
    }

    if let Some(file_type) = params.file_type {
        if file_type.is_direct_download() {
            return Ok(infer_direct_file_type_from_url(&params.url).unwrap_or(file_type));
        }

        return Ok(FileType::Hls);
    }

    Ok(infer_direct_file_type_from_url(&params.url).unwrap_or(FileType::Hls))
}

fn infer_direct_file_type_from_url(url: &str) -> Option<FileType> {
    let trimmed = url.trim();

    if let Ok(parsed) = url::Url::parse(trimmed) {
        if let Some(file_type) = infer_direct_file_type_from_candidate(parsed.path()) {
            return Some(file_type);
        }

        for key in ["filename", "file", "name", "title", "videoTitle"] {
            if let Some((_, value)) = parsed
                .query_pairs()
                .find(|(query_key, _)| query_key.eq_ignore_ascii_case(key))
            {
                if let Some(file_type) = infer_direct_file_type_from_candidate(&value) {
                    return Some(file_type);
                }
            }
        }
    }

    infer_direct_file_type_from_candidate(trimmed)
}

fn infer_direct_file_type_from_candidate(candidate: &str) -> Option<FileType> {
    let lower = candidate.to_ascii_lowercase();

    for (extension, file_type) in DIRECT_DOWNLOAD_EXTENSIONS {
        let suffix = format!(".{}", extension);
        if lower.ends_with(&suffix)
            || lower.contains(&format!(".{}?", extension))
            || lower.contains(&format!(".{}#", extension))
        {
            return Some(*file_type);
        }
    }

    None
}

fn normalize_direct_download_filename(name: String, file_type: FileType) -> String {
    let expected_extension = file_type.default_extension().unwrap_or("mp4");
    let stem = strip_known_download_extension(&name).unwrap_or(&name);
    ensure_download_extension(stem, expected_extension)
}

fn ensure_download_extension(filename: &str, extension: &str) -> String {
    let expected_suffix = format!(".{}", extension);
    if filename.to_ascii_lowercase().ends_with(&expected_suffix) {
        filename.to_string()
    } else {
        format!("{}.{}", filename, extension)
    }
}

fn parse_request_headers(raw: Option<&str>) -> Result<RequestHeaders, AppError> {
    let mut headers = RequestHeaders::new();

    for (index, line) in raw.unwrap_or_default().lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let Some((name, value)) = trimmed.split_once(':') else {
            return Err(AppError::InvalidInput(format!(
                "附加 Header 第 {} 行格式无效，请使用 name:value",
                index + 1
            )));
        };

        let name = name.trim();
        let value = value.trim();

        if name.is_empty() || value.is_empty() {
            return Err(AppError::InvalidInput(format!(
                "附加 Header 第 {} 行格式无效，请使用 name:value",
                index + 1
            )));
        }

        headers.insert(name.to_string(), value.to_string());
    }

    Ok(headers)
}

fn normalize_download_filename(name: String) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return "download".to_string();
    }

    let sanitized = trimmed
        .chars()
        .map(|ch| match ch {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect::<String>()
        .trim()
        .trim_matches('.')
        .to_string();

    let fallback = if sanitized.is_empty() {
        "download".to_string()
    } else {
        sanitized
    };

    strip_known_download_extension(&fallback)
        .unwrap_or(&fallback)
        .to_string()
}

fn strip_known_download_extension(name: &str) -> Option<&str> {
    let lower = name.to_ascii_lowercase();

    for extension in NORMALIZED_DOWNLOAD_EXTENSIONS {
        let suffix = format!(".{}", extension);
        if lower.ends_with(&suffix) {
            return Some(&name[..name.len() - suffix.len()]);
        }
    }

    None
}

fn resolve_chrome_extension_dir(app_handle: &AppHandle) -> Result<PathBuf, AppError> {
    resolve_chrome_extension_dir_from_candidates(chrome_extension_dir_candidates(app_handle))
}

#[cfg(target_os = "macos")]
async fn prepare_chrome_extension_install_dir(app_handle: &AppHandle) -> Result<PathBuf, AppError> {
    let source_dir = resolve_chrome_extension_dir(app_handle)?;
    let target_dir = chrome_extension_install_target_dir()?;

    let source_resolved = std::fs::canonicalize(&source_dir).unwrap_or(source_dir.clone());
    let target_resolved = std::fs::canonicalize(&target_dir).unwrap_or(target_dir.clone());

    if source_resolved == target_resolved {
        return Ok(normalize_path_for_platform(target_resolved));
    }

    if target_dir.exists() {
        if target_dir.is_dir() {
            std::fs::remove_dir_all(&target_dir)?;
        } else {
            std::fs::remove_file(&target_dir)?;
        }
    }

    copy_dir_recursive(&source_dir, &target_dir)?;
    let copied_dir = std::fs::canonicalize(&target_dir).unwrap_or(target_dir);
    Ok(normalize_path_for_platform(copied_dir))
}

#[cfg(not(target_os = "macos"))]
async fn prepare_chrome_extension_install_dir(app_handle: &AppHandle) -> Result<PathBuf, AppError> {
    resolve_chrome_extension_dir(app_handle)
}

#[cfg(target_os = "macos")]
fn chrome_extension_install_target_dir() -> Result<PathBuf, AppError> {
    let download_dir = dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join("Downloads")))
        .ok_or_else(|| AppError::Internal("获取下载目录失败".to_string()))?;

    Ok(download_dir
        .join("M3U8 Quicker Extension")
        .join("chrome-extension"))
}

#[allow(dead_code)]
fn copy_dir_recursive(source: &Path, target: &Path) -> Result<(), AppError> {
    if !source.is_dir() {
        return Err(AppError::InvalidInput(
            "Chrome / Edge 扩展目录不存在".to_string(),
        ));
    }

    std::fs::create_dir_all(target)?;

    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            copy_dir_recursive(&source_path, &target_path)?;
        } else if file_type.is_file() {
            if let Some(parent) = target_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&source_path, &target_path)?;
        }
    }

    Ok(())
}

fn chrome_extension_dir_candidates(app_handle: &AppHandle) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let workspace_candidate = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("browser-extension")
        .join("chrome");

    if cfg!(debug_assertions) {
        candidates.push(workspace_candidate.clone());
    }

    if let Ok(resource_dir) = app_handle.path().resource_dir() {
        candidates.push(resource_dir.join("chrome-extension"));
    }

    if !cfg!(debug_assertions) {
        candidates.push(workspace_candidate);
    }

    candidates
}

fn resolve_chrome_extension_dir_from_candidates<I>(candidates: I) -> Result<PathBuf, AppError>
where
    I: IntoIterator<Item = PathBuf>,
{
    for candidate in candidates {
        if candidate.join("manifest.json").is_file() {
            let resolved = std::fs::canonicalize(&candidate).unwrap_or(candidate);
            return Ok(normalize_path_for_platform(resolved));
        }
    }

    Err(AppError::InvalidInput(
        "未找到内置 Chrome / Edge 扩展目录".to_string(),
    ))
}

#[cfg(target_os = "windows")]
fn normalize_path_for_platform(path: PathBuf) -> PathBuf {
    let display = path.to_string_lossy().to_string();
    PathBuf::from(display.strip_prefix(r"\\?\").unwrap_or(&display))
}

#[cfg(not(target_os = "windows"))]
fn normalize_path_for_platform(path: PathBuf) -> PathBuf {
    path
}

#[cfg(target_os = "windows")]
fn normalize_display_path(path: &Path) -> String {
    let display = path.to_string_lossy().to_string();
    display
        .strip_prefix(r"\\?\")
        .unwrap_or(&display)
        .to_string()
}

#[cfg(not(target_os = "windows"))]
fn normalize_display_path(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn resolve_firefox_extension_dir(app_handle: &AppHandle) -> Result<PathBuf, AppError> {
    resolve_firefox_extension_dir_from_candidates(firefox_extension_dir_candidates(app_handle))
}

#[cfg(target_os = "macos")]
async fn prepare_firefox_extension_install_dir(
    app_handle: &AppHandle,
) -> Result<PathBuf, AppError> {
    let source_dir = resolve_firefox_extension_dir(app_handle)?;
    let target_dir = firefox_extension_install_target_dir()?;

    let source_resolved = std::fs::canonicalize(&source_dir).unwrap_or(source_dir.clone());
    let target_resolved = std::fs::canonicalize(&target_dir).unwrap_or(target_dir.clone());

    if source_resolved == target_resolved {
        return Ok(normalize_path_for_platform(target_resolved));
    }

    if target_dir.exists() {
        if target_dir.is_dir() {
            std::fs::remove_dir_all(&target_dir)?;
        } else {
            std::fs::remove_file(&target_dir)?;
        }
    }

    copy_dir_recursive(&source_dir, &target_dir)?;
    let copied_dir = std::fs::canonicalize(&target_dir).unwrap_or(target_dir);
    Ok(normalize_path_for_platform(copied_dir))
}

#[cfg(not(target_os = "macos"))]
async fn prepare_firefox_extension_install_dir(
    app_handle: &AppHandle,
) -> Result<PathBuf, AppError> {
    resolve_firefox_extension_dir(app_handle)
}

#[cfg(target_os = "macos")]
fn firefox_extension_install_target_dir() -> Result<PathBuf, AppError> {
    let download_dir = dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join("Downloads")))
        .ok_or_else(|| AppError::Internal("获取下载���录失败".to_string()))?;

    Ok(download_dir
        .join("M3U8 Quicker Extension")
        .join("firefox-extension"))
}

fn firefox_extension_dir_candidates(app_handle: &AppHandle) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let workspace_candidate = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("browser-extension")
        .join("firefox");

    if cfg!(debug_assertions) {
        candidates.push(workspace_candidate.clone());
    }

    if let Ok(resource_dir) = app_handle.path().resource_dir() {
        candidates.push(resource_dir.join("firefox-extension"));
    }

    if !cfg!(debug_assertions) {
        candidates.push(workspace_candidate);
    }

    candidates
}

fn resolve_firefox_extension_dir_from_candidates<I>(candidates: I) -> Result<PathBuf, AppError>
where
    I: IntoIterator<Item = PathBuf>,
{
    for candidate in candidates {
        if candidate.join("manifest.json").is_file() {
            let resolved = std::fs::canonicalize(&candidate).unwrap_or(candidate);
            return Ok(normalize_path_for_platform(resolved));
        }
    }

    Err(AppError::InvalidInput(
        "未找到内置 Firefox 扩展��录".to_string(),
    ))
}

fn chromium_extensions_url(browser: ChromiumBrowser) -> &'static str {
    match browser {
        ChromiumBrowser::Chrome => CHROME_EXTENSIONS_URL,
        ChromiumBrowser::Edge => EDGE_EXTENSIONS_URL,
    }
}

fn try_open_chromium_extensions_page(browser: ChromiumBrowser) -> bool {
    chromium_command_candidates(browser)
        .iter()
        .any(|command| open_chromium_extensions_page_with_command(command, browser))
}

fn open_chromium_extensions_page_with_command(command: &OsStr, browser: ChromiumBrowser) -> bool {
    Command::new(command)
        .arg(chromium_extensions_url(browser))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

fn chromium_command_candidates(browser: ChromiumBrowser) -> Vec<OsString> {
    #[cfg(target_os = "windows")]
    {
        return build_windows_chromium_command_candidates(
            browser,
            std::env::var_os("ProgramFiles"),
            std::env::var_os("ProgramFiles(x86)"),
            std::env::var_os("LocalAppData"),
        );
    }

    #[cfg(target_os = "macos")]
    {
        return build_macos_chromium_command_candidates(browser);
    }

    #[cfg(target_os = "linux")]
    {
        return build_linux_chromium_command_candidates(browser);
    }

    #[allow(unreachable_code)]
    Vec::new()
}

#[cfg(target_os = "windows")]
fn build_windows_chromium_command_candidates(
    browser: ChromiumBrowser,
    program_files: Option<OsString>,
    program_files_x86: Option<OsString>,
    local_app_data: Option<OsString>,
) -> Vec<OsString> {
    let suffix = match browser {
        ChromiumBrowser::Chrome => Path::new("Google")
            .join("Chrome")
            .join("Application")
            .join("chrome.exe"),
        ChromiumBrowser::Edge => Path::new("Microsoft")
            .join("Edge")
            .join("Application")
            .join("msedge.exe"),
    };
    let mut candidates = Vec::new();

    for base in [program_files, program_files_x86, local_app_data]
        .into_iter()
        .flatten()
    {
        let candidate = PathBuf::from(base).join(&suffix);
        if !candidates
            .iter()
            .any(|existing| existing == candidate.as_os_str())
        {
            candidates.push(candidate.into_os_string());
        }
    }

    candidates
}

#[cfg(target_os = "macos")]
fn build_macos_chromium_command_candidates(browser: ChromiumBrowser) -> Vec<OsString> {
    match browser {
        ChromiumBrowser::Chrome => vec![OsString::from(
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        )],
        ChromiumBrowser::Edge => vec![OsString::from(
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        )],
    }
}

#[cfg(target_os = "linux")]
fn build_linux_chromium_command_candidates(browser: ChromiumBrowser) -> Vec<OsString> {
    match browser {
        ChromiumBrowser::Chrome => vec![
            OsString::from("google-chrome"),
            OsString::from("google-chrome-stable"),
        ],
        ChromiumBrowser::Edge => vec![
            OsString::from("microsoft-edge"),
            OsString::from("microsoft-edge-stable"),
        ],
    }
}

fn try_open_firefox_addons_page() -> bool {
    firefox_command_candidates()
        .iter()
        .any(|command| open_firefox_addons_page_with_command(command))
}

fn open_firefox_addons_page_with_command(command: &OsStr) -> bool {
    Command::new(command)
        .arg(FIREFOX_ADDONS_URL)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

fn firefox_command_candidates() -> Vec<OsString> {
    #[cfg(target_os = "windows")]
    {
        return build_windows_firefox_command_candidates(
            std::env::var_os("ProgramFiles"),
            std::env::var_os("ProgramFiles(x86)"),
        );
    }

    #[cfg(target_os = "macos")]
    {
        return build_macos_firefox_command_candidates();
    }

    #[cfg(target_os = "linux")]
    {
        return build_linux_firefox_command_candidates();
    }

    #[allow(unreachable_code)]
    Vec::new()
}

#[cfg(target_os = "windows")]
fn build_windows_firefox_command_candidates(
    program_files: Option<OsString>,
    program_files_x86: Option<OsString>,
) -> Vec<OsString> {
    let suffix = Path::new("Mozilla Firefox").join("firefox.exe");
    let mut candidates = Vec::new();

    for base in [program_files, program_files_x86].into_iter().flatten() {
        let candidate = PathBuf::from(base).join(&suffix);
        if !candidates
            .iter()
            .any(|existing| existing == candidate.as_os_str())
        {
            candidates.push(candidate.into_os_string());
        }
    }

    candidates
}

#[cfg(target_os = "macos")]
fn build_macos_firefox_command_candidates() -> Vec<OsString> {
    vec![OsString::from(
        "/Applications/Firefox.app/Contents/MacOS/firefox",
    )]
}

#[cfg(target_os = "linux")]
fn build_linux_firefox_command_candidates() -> Vec<OsString> {
    vec![OsString::from("firefox")]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn build_task(segment_uris: Vec<&str>) -> DownloadTask {
        let total_segments = segment_uris.len();
        DownloadTask {
            id: "task-id".to_string(),
            url: "https://example.com/video.m3u8".to_string(),
            filename: "video".to_string(),
            file_type: FileType::Hls,
            hls_output_mode: HlsOutputMode::SingleStream,
            hls_selection: None,
            encryption_method: None,
            output_dir: "D:\\Download".to_string(),
            extra_headers: None,
            status: DownloadStatus::Paused,
            total_segments,
            completed_segments: 1,
            completed_segment_indices: vec![1],
            failed_segment_indices: Vec::new(),
            segment_uris: segment_uris.into_iter().map(str::to_string).collect(),
            segment_durations: vec![5.0; total_segments],
            total_bytes: 1024,
            speed_bytes_per_sec: 0,
            created_at: Utc::now(),
            completed_at: None,
            updated_at: None,
            playback_available: true,
            file_path: None,
        }
    }

    fn build_segments(segment_uris: Vec<&str>) -> Vec<SegmentInfo> {
        segment_uris
            .into_iter()
            .enumerate()
            .map(|(index, uri)| SegmentInfo {
                index,
                uri: uri.to_string(),
                duration: 5.0,
                sequence_number: index as u64,
                byte_range: None,
                encryption: None,
            })
            .collect()
    }

    #[test]
    fn normalize_download_filename_strips_supported_direct_extensions() {
        assert_eq!(
            normalize_download_filename("movie.mp4".to_string()),
            "movie"
        );
        assert_eq!(
            normalize_download_filename("movie.mkv".to_string()),
            "movie"
        );
        assert_eq!(
            normalize_download_filename("movie.webm".to_string()),
            "movie"
        );
        assert_eq!(
            normalize_download_filename("movie.rmvb".to_string()),
            "movie"
        );
    }

    #[test]
    fn normalize_direct_download_filename_uses_selected_extension() {
        assert_eq!(
            normalize_direct_download_filename("movie".to_string(), FileType::Mp4),
            "movie.mp4"
        );
        assert_eq!(
            normalize_direct_download_filename("movie.mp4".to_string(), FileType::Mkv),
            "movie.mkv"
        );
        assert_eq!(
            normalize_direct_download_filename("movie.rmvb".to_string(), FileType::Webm),
            "movie.webm"
        );
    }

    #[test]
    fn resolve_create_download_file_type_infers_direct_type_from_url() {
        let params = CreateDownloadParams {
            url: "https://example.com/media/movie.webm?token=abc".to_string(),
            filename: None,
            output_dir: None,
            extra_headers: None,
            download_mode: Some(DownloadMode::Direct),
            file_type: None,
            hls_selection: None,
        };

        assert_eq!(
            resolve_create_download_file_type(&params).expect("file type"),
            FileType::Webm
        );
    }

    #[test]
    fn resolve_create_download_file_type_keeps_hls_mode_even_for_direct_url() {
        let params = CreateDownloadParams {
            url: "https://example.com/media/movie.mp4".to_string(),
            filename: None,
            output_dir: None,
            extra_headers: None,
            download_mode: Some(DownloadMode::Hls),
            file_type: None,
            hls_selection: None,
        };

        assert_eq!(
            resolve_create_download_file_type(&params).expect("file type"),
            FileType::Hls
        );
    }

    #[test]
    fn resolve_create_download_file_type_updates_legacy_direct_type_from_url() {
        let params = CreateDownloadParams {
            url: "https://example.com/media/movie.mkv".to_string(),
            filename: None,
            output_dir: None,
            extra_headers: None,
            download_mode: None,
            file_type: Some(FileType::Mp4),
            hls_selection: None,
        };

        assert_eq!(
            resolve_create_download_file_type(&params).expect("file type"),
            FileType::Mkv
        );
    }

    #[test]
    fn resolve_create_download_file_type_rejects_unknown_direct_url_extension() {
        let params = CreateDownloadParams {
            url: "https://example.com/media/download".to_string(),
            filename: None,
            output_dir: None,
            extra_headers: None,
            download_mode: Some(DownloadMode::Direct),
            file_type: None,
            hls_selection: None,
        };

        assert!(resolve_create_download_file_type(&params).is_err());
    }

    #[test]
    fn playback_target_for_task_uses_file_route_for_in_progress_mp4_and_webm() {
        let mut mp4_task = build_task(Vec::new());
        mp4_task.status = DownloadStatus::Downloading;
        mp4_task.file_type = FileType::Mp4;
        let (mp4_kind, mp4_path) = playback_target_for_task(&mp4_task).expect("mp4 playback path");
        assert_eq!(mp4_kind, PlaybackSourceKind::File);
        assert_eq!(mp4_path, playback::file_path(&mp4_task.id));

        let mut webm_task = build_task(Vec::new());
        webm_task.status = DownloadStatus::Paused;
        webm_task.file_type = FileType::Webm;
        let (webm_kind, webm_path) =
            playback_target_for_task(&webm_task).expect("webm playback path");
        assert_eq!(webm_kind, PlaybackSourceKind::File);
        assert_eq!(webm_path, playback::file_path(&webm_task.id));
    }

    #[test]
    fn playback_target_for_task_rejects_in_progress_unsupported_direct_formats() {
        let mut task = build_task(Vec::new());
        task.status = DownloadStatus::Downloading;
        task.file_type = FileType::Mkv;

        let error = playback_target_for_task(&task).expect_err("unsupported format should fail");

        assert!(error
            .to_string()
            .contains("当前格式暂不支持边下边播，请等待下载完成后再播放"));
    }

    #[test]
    fn playback_target_for_task_rejects_multi_track_downloads() {
        let mut task = build_task(Vec::new());
        task.playback_available = false;
        task.hls_output_mode = HlsOutputMode::MultiTrackBundle;
        task.file_path = Some("D:\\Download\\video_tracks".to_string());

        let error = playback_target_for_task(&task).expect_err("multi-track playback should fail");

        assert!(error.to_string().contains("多轨下载暂不支持播放"));
    }

    #[test]
    fn validate_segment_layout_allows_host_and_query_rotation() {
        let task = build_task(vec![
            "https://cdn-a.example.com/videos/seg_000.ts?auth=old",
            "https://cdn-a.example.com/videos/seg_001.ts?auth=old",
        ]);
        let segments = build_segments(vec![
            "https://cdn-b.example.com/videos/seg_000.ts?auth=new",
            "https://cdn-b.example.com/videos/seg_001.ts?auth=new",
        ]);

        assert!(validate_segment_layout(&task, &segments).is_ok());
    }

    #[test]
    fn validate_segment_layout_rejects_real_path_changes() {
        let task = build_task(vec![
            "https://cdn-a.example.com/videos/seg_000.ts?auth=old",
            "https://cdn-a.example.com/videos/seg_001.ts?auth=old",
        ]);
        let segments = build_segments(vec![
            "https://cdn-b.example.com/videos/seg_000.ts?auth=new",
            "https://cdn-b.example.com/videos/seg_999.ts?auth=new",
        ]);

        assert!(matches!(
            validate_segment_layout(&task, &segments),
            Err(AppError::InvalidInput(_))
        ));
    }

    #[test]
    fn resolve_chrome_extension_dir_prefers_first_valid_candidate() {
        let temp_root = unique_temp_path("chrome-extension-priority");
        let bundled_dir = temp_root.join("resources").join("chrome-extension");
        let dev_dir = temp_root.join("workspace").join("chrome-extension");
        create_manifest(&bundled_dir);
        create_manifest(&dev_dir);

        let resolved =
            resolve_chrome_extension_dir_from_candidates(vec![bundled_dir.clone(), dev_dir])
                .expect("expected chrome extension dir");

        let expected =
            normalize_path_for_platform(std::fs::canonicalize(&bundled_dir).unwrap_or(bundled_dir));
        assert_eq!(resolved, expected);
        remove_temp_dir(&temp_root);
    }

    #[test]
    fn resolve_chrome_extension_dir_falls_back_to_next_valid_candidate() {
        let temp_root = unique_temp_path("chrome-extension-fallback");
        let missing_dir = temp_root.join("resources").join("chrome-extension");
        let dev_dir = temp_root.join("workspace").join("chrome-extension");
        create_manifest(&dev_dir);

        let resolved =
            resolve_chrome_extension_dir_from_candidates(vec![missing_dir, dev_dir.clone()])
                .expect("expected fallback chrome extension dir");

        let expected =
            normalize_path_for_platform(std::fs::canonicalize(&dev_dir).unwrap_or(dev_dir));
        assert_eq!(resolved, expected);
        remove_temp_dir(&temp_root);
    }

    #[test]
    fn copy_dir_recursive_copies_nested_extension_files() {
        let temp_root = unique_temp_path("chrome-extension-copy");
        let source_dir = temp_root.join("source");
        let nested_dir = source_dir.join("assets");
        let target_dir = temp_root.join("target");

        create_manifest(&source_dir);
        fs::create_dir_all(&nested_dir).expect("create nested dir");
        fs::write(nested_dir.join("icon.png"), "icon-bytes").expect("write nested file");

        copy_dir_recursive(&source_dir, &target_dir).expect("copy extension directory");

        assert!(target_dir.join("manifest.json").is_file());
        assert!(target_dir.join("assets").join("icon.png").is_file());
        assert_eq!(
            fs::read_to_string(target_dir.join("assets").join("icon.png"))
                .expect("read copied nested file"),
            "icon-bytes"
        );

        remove_temp_dir(&temp_root);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn build_windows_chromium_command_candidates_keeps_expected_priority_for_chrome() {
        let candidates = build_windows_chromium_command_candidates(
            ChromiumBrowser::Chrome,
            Some(OsString::from(r"C:\Program Files")),
            Some(OsString::from(r"C:\Program Files (x86)")),
            Some(OsString::from(r"C:\Users\Test\AppData\Local")),
        );

        assert_eq!(
            candidates,
            vec![
                OsString::from(r"C:\Program Files\Google\Chrome\Application\chrome.exe"),
                OsString::from(r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe"),
                OsString::from(r"C:\Users\Test\AppData\Local\Google\Chrome\Application\chrome.exe"),
            ]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn build_linux_chromium_command_candidates_keeps_expected_priority_for_chrome() {
        assert_eq!(
            build_linux_chromium_command_candidates(ChromiumBrowser::Chrome),
            vec![
                OsString::from("google-chrome"),
                OsString::from("google-chrome-stable"),
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn build_macos_chromium_command_candidates_keeps_expected_priority_for_chrome() {
        assert_eq!(
            build_macos_chromium_command_candidates(ChromiumBrowser::Chrome),
            vec![OsString::from(
                "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            )]
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn build_windows_chromium_command_candidates_keeps_expected_priority_for_edge() {
        let candidates = build_windows_chromium_command_candidates(
            ChromiumBrowser::Edge,
            Some(OsString::from(r"C:\Program Files")),
            Some(OsString::from(r"C:\Program Files (x86)")),
            Some(OsString::from(r"C:\Users\Test\AppData\Local")),
        );

        assert_eq!(
            candidates,
            vec![
                OsString::from(r"C:\Program Files\Microsoft\Edge\Application\msedge.exe"),
                OsString::from(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"),
                OsString::from(
                    r"C:\Users\Test\AppData\Local\Microsoft\Edge\Application\msedge.exe"
                ),
            ]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn build_linux_chromium_command_candidates_keeps_expected_priority_for_edge() {
        assert_eq!(
            build_linux_chromium_command_candidates(ChromiumBrowser::Edge),
            vec![
                OsString::from("microsoft-edge"),
                OsString::from("microsoft-edge-stable"),
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn build_macos_chromium_command_candidates_keeps_expected_priority_for_edge() {
        assert_eq!(
            build_macos_chromium_command_candidates(ChromiumBrowser::Edge),
            vec![OsString::from(
                "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            )]
        );
    }

    fn unique_temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("m3u8quicker-{}-{}", name, Uuid::new_v4()))
    }

    fn create_manifest(dir: &Path) {
        fs::create_dir_all(dir).expect("create test dir");
        fs::write(dir.join("manifest.json"), "{}").expect("write manifest");
    }

    fn create_bundle_playlist(dir: &Path, track_dir_name: &str) {
        let track_dir = dir.join(track_dir_name);
        fs::create_dir_all(&track_dir).expect("create track dir");
        fs::write(track_dir.join("index.m3u8"), "#EXTM3U\n").expect("write playlist");
    }

    fn remove_temp_dir(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn resolve_local_hls_bundle_paths_accepts_video_audio_subtitle_bundle() {
        let temp_root = unique_temp_path("multi-track-bundle-valid");
        create_bundle_playlist(&temp_root, "video");
        create_bundle_playlist(&temp_root, "audio");
        create_bundle_playlist(&temp_root, "subtitle");

        let bundle = resolve_local_hls_bundle_paths(&temp_root).expect("valid bundle");

        assert!(bundle
            .video_playlist
            .ends_with(Path::new("video").join("index.m3u8")));
        assert!(bundle.audio_playlist.is_some());
        assert!(bundle.subtitle_playlist.is_some());
        remove_temp_dir(&temp_root);
    }

    #[test]
    fn resolve_local_hls_bundle_paths_rejects_missing_video_playlist() {
        let temp_root = unique_temp_path("multi-track-bundle-missing-video");
        create_bundle_playlist(&temp_root, "audio");

        let error =
            resolve_local_hls_bundle_paths(&temp_root).expect_err("bundle without video must fail");

        assert!(error.to_string().contains("视频"));
        remove_temp_dir(&temp_root);
    }

    #[test]
    fn resolve_local_hls_bundle_paths_rejects_single_video_only_bundle() {
        let temp_root = unique_temp_path("multi-track-bundle-video-only");
        create_bundle_playlist(&temp_root, "video");

        let error =
            resolve_local_hls_bundle_paths(&temp_root).expect_err("video-only bundle must fail");

        assert!(error.to_string().contains("至少需要音频或字幕轨道"));
        remove_temp_dir(&temp_root);
    }
}
