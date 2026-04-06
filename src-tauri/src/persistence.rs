use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tauri::Manager;

use crate::error::AppError;
use crate::models::{
    download_group_for_status, AppSettings, DownloadCounts, DownloadGroup, DownloadTask,
    DownloadTaskPage, DownloadTaskSegmentState, DownloadTaskSummary,
};
use crate::state::AppState;

const DOWNLOAD_STORE_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DownloadStoreMeta {
    version: u32,
}

impl Default for DownloadStoreMeta {
    fn default() -> Self {
        Self {
            version: DOWNLOAD_STORE_VERSION,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct DownloadTaskDetail {
    url: String,
    extra_headers: Option<String>,
    #[serde(default)]
    completed_segment_indices: Vec<usize>,
    #[serde(default)]
    failed_segment_indices: Vec<usize>,
    #[serde(default)]
    segment_uris: Vec<String>,
    #[serde(default)]
    segment_durations: Vec<f32>,
}

fn app_data_dir(app_handle: &tauri::AppHandle) -> PathBuf {
    app_handle
        .path()
        .app_data_dir()
        .expect("Failed to get app data dir")
}

fn legacy_downloads_file(app_handle: &tauri::AppHandle) -> PathBuf {
    app_data_dir(app_handle).join("downloads.json")
}

fn legacy_downloads_backup_file(app_handle: &tauri::AppHandle) -> PathBuf {
    app_data_dir(app_handle).join("downloads.json.bak")
}

fn store_root_dir(app_handle: &tauri::AppHandle) -> PathBuf {
    app_data_dir(app_handle).join("downloads")
}

fn meta_file(app_handle: &tauri::AppHandle) -> PathBuf {
    store_root_dir(app_handle).join("meta.json")
}

fn index_dir(app_handle: &tauri::AppHandle) -> PathBuf {
    store_root_dir(app_handle).join("index")
}

fn tasks_dir(app_handle: &tauri::AppHandle) -> PathBuf {
    store_root_dir(app_handle).join("tasks")
}

fn index_file(app_handle: &tauri::AppHandle, group: DownloadGroup) -> PathBuf {
    let name = match group {
        DownloadGroup::Active => "active.json",
        DownloadGroup::History => "history.json",
    };
    index_dir(app_handle).join(name)
}

fn task_file(app_handle: &tauri::AppHandle, id: &str) -> PathBuf {
    tasks_dir(app_handle).join(format!("{}.json", id))
}

async fn read_json_or_default<T>(path: &Path) -> Result<T, AppError>
where
    T: DeserializeOwned + Default,
{
    if !path.exists() {
        return Ok(T::default());
    }

    let data = tokio::fs::read_to_string(path)
        .await
        .map_err(|error| AppError::Internal(error.to_string()))?;
    if data.trim().is_empty() {
        return Ok(T::default());
    }

    serde_json::from_str(&data).map_err(|error| AppError::Internal(error.to_string()))
}

async fn write_json_atomic<T>(path: &Path, value: &T) -> Result<(), AppError>
where
    T: Serialize,
{
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| AppError::Internal(error.to_string()))?;
    }

    let json =
        serde_json::to_vec_pretty(value).map_err(|error| AppError::Internal(error.to_string()))?;
    let temp_path = path.with_extension("tmp");
    tokio::fs::write(&temp_path, json)
        .await
        .map_err(|error| AppError::Internal(error.to_string()))?;
    if path.exists() {
        let _ = tokio::fs::remove_file(path).await;
    }
    tokio::fs::rename(&temp_path, path)
        .await
        .map_err(|error| AppError::Internal(error.to_string()))
}

async fn remove_file_if_exists(path: &Path) -> Result<(), AppError> {
    if path.exists() {
        tokio::fs::remove_file(path)
            .await
            .map_err(|error| AppError::Internal(error.to_string()))?;
    }
    Ok(())
}

fn parse_datetime(value: &str) -> Result<DateTime<Utc>, AppError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| AppError::Internal(error.to_string()))
}

fn sort_summaries_desc(items: &mut [DownloadTaskSummary]) {
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
}

fn task_to_detail(task: &DownloadTask) -> DownloadTaskDetail {
    DownloadTaskDetail {
        url: task.url.clone(),
        extra_headers: task.extra_headers.clone(),
        completed_segment_indices: task.completed_segment_indices.clone(),
        failed_segment_indices: task.failed_segment_indices.clone(),
        segment_uris: task.segment_uris.clone(),
        segment_durations: task.segment_durations.clone(),
    }
}

pub fn task_to_summary(task: &DownloadTask) -> DownloadTaskSummary {
    DownloadTaskSummary {
        id: task.id.clone(),
        filename: task.filename.clone(),
        encryption_method: task.encryption_method.clone(),
        output_dir: task.output_dir.clone(),
        status: task.status.clone(),
        total_segments: task.total_segments,
        completed_segments: task.completed_segments,
        failed_segment_count: task.failed_segment_indices.len(),
        total_bytes: task.total_bytes,
        speed_bytes_per_sec: task.speed_bytes_per_sec,
        created_at: task.created_at.to_rfc3339(),
        completed_at: task.completed_at.map(|value| value.to_rfc3339()),
        updated_at: task.last_updated_at().to_rfc3339(),
        file_path: task.file_path.clone(),
    }
}

pub fn task_to_segment_state(task: &DownloadTask) -> DownloadTaskSegmentState {
    DownloadTaskSegmentState {
        id: task.id.clone(),
        total_segments: task.total_segments,
        completed_segment_indices: task.completed_segment_indices.clone(),
        failed_segment_indices: task.failed_segment_indices.clone(),
        updated_at: task.last_updated_at().to_rfc3339(),
    }
}

fn task_from_parts(
    summary: DownloadTaskSummary,
    detail: DownloadTaskDetail,
) -> Result<DownloadTask, AppError> {
    Ok(DownloadTask {
        id: summary.id,
        url: detail.url,
        filename: summary.filename,
        encryption_method: summary.encryption_method,
        output_dir: summary.output_dir,
        extra_headers: detail.extra_headers,
        status: summary.status,
        total_segments: summary.total_segments,
        completed_segments: summary.completed_segments,
        completed_segment_indices: detail.completed_segment_indices,
        failed_segment_indices: detail.failed_segment_indices,
        segment_uris: detail.segment_uris,
        segment_durations: detail.segment_durations,
        total_bytes: summary.total_bytes,
        speed_bytes_per_sec: summary.speed_bytes_per_sec,
        created_at: parse_datetime(&summary.created_at)?,
        completed_at: summary
            .completed_at
            .as_deref()
            .map(parse_datetime)
            .transpose()?,
        updated_at: Some(parse_datetime(&summary.updated_at)?),
        file_path: summary.file_path,
    })
}

async fn read_index_locked(
    app_handle: &tauri::AppHandle,
    group: DownloadGroup,
) -> Result<Vec<DownloadTaskSummary>, AppError> {
    read_json_or_default(&index_file(app_handle, group)).await
}

async fn write_index_locked(
    app_handle: &tauri::AppHandle,
    group: DownloadGroup,
    items: &[DownloadTaskSummary],
) -> Result<(), AppError> {
    write_json_atomic(&index_file(app_handle, group), &items).await
}

async fn ensure_store_ready_locked(app_handle: &tauri::AppHandle) -> Result<(), AppError> {
    tokio::fs::create_dir_all(index_dir(app_handle))
        .await
        .map_err(|error| AppError::Internal(error.to_string()))?;
    tokio::fs::create_dir_all(tasks_dir(app_handle))
        .await
        .map_err(|error| AppError::Internal(error.to_string()))?;

    let meta_path = meta_file(app_handle);
    let meta = read_json_or_default::<DownloadStoreMeta>(&meta_path).await?;
    if meta.version != DOWNLOAD_STORE_VERSION || !meta_path.exists() {
        write_json_atomic(&meta_path, &DownloadStoreMeta::default()).await?;
    }

    for group in [DownloadGroup::Active, DownloadGroup::History] {
        let path = index_file(app_handle, group);
        if !path.exists() {
            write_json_atomic(&path, &Vec::<DownloadTaskSummary>::new()).await?;
        }
    }

    Ok(())
}

pub async fn migrate_legacy_downloads(app_handle: &tauri::AppHandle) -> Result<(), AppError> {
    let store_lock = app_handle.state::<AppState>().download_store_lock.clone();
    let _guard = store_lock.lock().await;

    ensure_store_ready_locked(app_handle).await?;

    let legacy_path = legacy_downloads_file(app_handle);
    if !legacy_path.exists() {
        return Ok(());
    }

    let data = tokio::fs::read_to_string(&legacy_path)
        .await
        .map_err(|error| AppError::Internal(error.to_string()))?;
    let tasks: Vec<DownloadTask> = serde_json::from_str(&data).unwrap_or_default();

    let mut active = Vec::new();
    let mut history = Vec::new();
    for task in tasks {
        write_json_atomic(&task_file(app_handle, &task.id), &task_to_detail(&task)).await?;
        let summary = task_to_summary(&task);
        match download_group_for_status(&summary.status) {
            DownloadGroup::Active => active.push(summary),
            DownloadGroup::History => history.push(summary),
        }
    }

    sort_summaries_desc(&mut active);
    sort_summaries_desc(&mut history);
    write_index_locked(app_handle, DownloadGroup::Active, &active).await?;
    write_index_locked(app_handle, DownloadGroup::History, &history).await?;

    let backup_path = legacy_downloads_backup_file(app_handle);
    if backup_path.exists() {
        let _ = tokio::fs::remove_file(&backup_path).await;
    }
    tokio::fs::copy(&legacy_path, &backup_path)
        .await
        .map_err(|error| AppError::Internal(error.to_string()))?;
    tokio::fs::remove_file(&legacy_path)
        .await
        .map_err(|error| AppError::Internal(error.to_string()))?;

    Ok(())
}

pub async fn load_active_downloads(
    app_handle: &tauri::AppHandle,
) -> Result<Vec<DownloadTask>, AppError> {
    let store_lock = app_handle.state::<AppState>().download_store_lock.clone();
    let _guard = store_lock.lock().await;

    ensure_store_ready_locked(app_handle).await?;

    let active = read_index_locked(app_handle, DownloadGroup::Active).await?;
    let mut tasks = Vec::with_capacity(active.len());
    for summary in active {
        let detail_path = task_file(app_handle, &summary.id);
        if !detail_path.exists() {
            continue;
        }
        let detail = read_json_or_default::<DownloadTaskDetail>(&detail_path).await?;
        tasks.push(task_from_parts(summary, detail)?);
    }

    Ok(tasks)
}

pub async fn load_download_summaries(
    app_handle: &tauri::AppHandle,
    group: DownloadGroup,
) -> Result<Vec<DownloadTaskSummary>, AppError> {
    let store_lock = app_handle.state::<AppState>().download_store_lock.clone();
    let _guard = store_lock.lock().await;

    ensure_store_ready_locked(app_handle).await?;
    read_index_locked(app_handle, group).await
}

pub async fn load_download_summary(
    app_handle: &tauri::AppHandle,
    id: &str,
) -> Result<Option<DownloadTaskSummary>, AppError> {
    let store_lock = app_handle.state::<AppState>().download_store_lock.clone();
    let _guard = store_lock.lock().await;

    ensure_store_ready_locked(app_handle).await?;
    for group in [DownloadGroup::Active, DownloadGroup::History] {
        let items = read_index_locked(app_handle, group).await?;
        if let Some(item) = items.into_iter().find(|item| item.id == id) {
            return Ok(Some(item));
        }
    }

    Ok(None)
}

pub async fn load_download_task(
    app_handle: &tauri::AppHandle,
    id: &str,
) -> Result<Option<DownloadTask>, AppError> {
    let store_lock = app_handle.state::<AppState>().download_store_lock.clone();
    let _guard = store_lock.lock().await;

    ensure_store_ready_locked(app_handle).await?;
    let summary = {
        let mut found = None;
        for group in [DownloadGroup::Active, DownloadGroup::History] {
            let items = read_index_locked(app_handle, group).await?;
            if let Some(item) = items.into_iter().find(|item| item.id == id) {
                found = Some(item);
                break;
            }
        }
        found
    };

    let Some(summary) = summary else {
        return Ok(None);
    };

    let detail_path = task_file(app_handle, id);
    if !detail_path.exists() {
        return Ok(None);
    }
    let detail = read_json_or_default::<DownloadTaskDetail>(&detail_path).await?;
    Ok(Some(task_from_parts(summary, detail)?))
}

pub async fn load_download_segment_state(
    app_handle: &tauri::AppHandle,
    id: &str,
) -> Result<Option<DownloadTaskSegmentState>, AppError> {
    Ok(load_download_task(app_handle, id)
        .await?
        .map(|task| task_to_segment_state(&task)))
}

pub async fn get_download_counts(
    app_handle: &tauri::AppHandle,
) -> Result<DownloadCounts, AppError> {
    let history = load_download_summaries(app_handle, DownloadGroup::History).await?;
    Ok(DownloadCounts {
        active_count: 0,
        history_count: history.len(),
    })
}

pub async fn get_downloads_page(
    app_handle: &tauri::AppHandle,
    group: DownloadGroup,
    page: usize,
    page_size: usize,
) -> Result<DownloadTaskPage, AppError> {
    let items = load_download_summaries(app_handle, group).await?;
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

    Ok(DownloadTaskPage {
        items: paged_items,
        total,
        page: safe_page,
        page_size: safe_page_size,
    })
}

pub async fn save_task(
    app_handle: &tauri::AppHandle,
    task: &DownloadTask,
) -> Result<DownloadTaskSummary, AppError> {
    let store_lock = app_handle.state::<AppState>().download_store_lock.clone();
    let _guard = store_lock.lock().await;

    ensure_store_ready_locked(app_handle).await?;

    let detail = task_to_detail(task);
    let summary = task_to_summary(task);
    let target_group = download_group_for_status(&summary.status);
    let mut active = read_index_locked(app_handle, DownloadGroup::Active).await?;
    let mut history = read_index_locked(app_handle, DownloadGroup::History).await?;

    active.retain(|item| item.id != summary.id);
    history.retain(|item| item.id != summary.id);
    match target_group {
        DownloadGroup::Active => active.push(summary.clone()),
        DownloadGroup::History => history.push(summary.clone()),
    }

    sort_summaries_desc(&mut active);
    sort_summaries_desc(&mut history);
    write_json_atomic(&task_file(app_handle, &summary.id), &detail).await?;
    write_index_locked(app_handle, DownloadGroup::Active, &active).await?;
    write_index_locked(app_handle, DownloadGroup::History, &history).await?;

    Ok(summary)
}

pub async fn delete_task(
    app_handle: &tauri::AppHandle,
    id: &str,
) -> Result<Option<DownloadTaskSummary>, AppError> {
    let store_lock = app_handle.state::<AppState>().download_store_lock.clone();
    let _guard = store_lock.lock().await;

    ensure_store_ready_locked(app_handle).await?;

    let mut removed = None;
    let mut active = read_index_locked(app_handle, DownloadGroup::Active).await?;
    let mut history = read_index_locked(app_handle, DownloadGroup::History).await?;
    if let Some(index) = active.iter().position(|item| item.id == id) {
        removed = Some(active.remove(index));
    }
    if let Some(index) = history.iter().position(|item| item.id == id) {
        removed = Some(history.remove(index));
    }

    write_index_locked(app_handle, DownloadGroup::Active, &active).await?;
    write_index_locked(app_handle, DownloadGroup::History, &history).await?;
    remove_file_if_exists(&task_file(app_handle, id)).await?;

    Ok(removed)
}

pub async fn clear_history_downloads(
    app_handle: &tauri::AppHandle,
) -> Result<Vec<DownloadTaskSummary>, AppError> {
    let store_lock = app_handle.state::<AppState>().download_store_lock.clone();
    let _guard = store_lock.lock().await;

    ensure_store_ready_locked(app_handle).await?;

    let history = read_index_locked(app_handle, DownloadGroup::History).await?;
    for item in &history {
        remove_file_if_exists(&task_file(app_handle, &item.id)).await?;
    }
    write_index_locked(app_handle, DownloadGroup::History, &[]).await?;

    Ok(history)
}

fn get_settings_file(app_handle: &tauri::AppHandle) -> PathBuf {
    app_data_dir(app_handle).join("settings.json")
}

pub async fn load_settings(app_handle: &tauri::AppHandle) -> AppSettings {
    let path = get_settings_file(app_handle);
    if !path.exists() {
        return AppSettings::default();
    }
    let data = tokio::fs::read_to_string(&path).await.unwrap_or_default();
    let mut settings: AppSettings = serde_json::from_str(&data).unwrap_or_default();
    settings.sanitize();
    settings
}

pub async fn save_settings(app_handle: &tauri::AppHandle, settings: &AppSettings) {
    let path = get_settings_file(app_handle);
    if let Some(parent) = path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Ok(json) = serde_json::to_string_pretty(settings) {
        let _ = tokio::fs::write(&path, json).await;
    }
}

pub async fn update_settings<F>(app_handle: &tauri::AppHandle, update: F)
where
    F: FnOnce(&mut AppSettings),
{
    let mut settings = load_settings(app_handle).await;
    update(&mut settings);
    save_settings(app_handle, &settings).await;
}
