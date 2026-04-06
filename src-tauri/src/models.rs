use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub type DownloadId = String;
pub type RequestHeaders = HashMap<String, String>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DownloadStatus {
    Pending,
    Downloading,
    Paused,
    Merging,
    Converting,
    Completed,
    Failed(String),
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadTask {
    pub id: DownloadId,
    pub url: String,
    pub filename: String,
    #[serde(default)]
    pub encryption_method: Option<String>,
    pub output_dir: String,
    #[serde(default)]
    pub extra_headers: Option<String>,
    pub status: DownloadStatus,
    pub total_segments: usize,
    pub completed_segments: usize,
    #[serde(default)]
    pub completed_segment_indices: Vec<usize>,
    #[serde(default)]
    pub failed_segment_indices: Vec<usize>,
    #[serde(default)]
    pub segment_uris: Vec<String>,
    #[serde(default)]
    pub segment_durations: Vec<f32>,
    pub total_bytes: u64,
    pub speed_bytes_per_sec: u64,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
    pub file_path: Option<String>,
}

impl DownloadTask {
    pub fn touch(&mut self) -> DateTime<Utc> {
        let now = Utc::now();
        self.updated_at = Some(now);
        now
    }

    pub fn last_updated_at(&self) -> DateTime<Utc> {
        self.updated_at
            .clone()
            .or_else(|| self.completed_at.clone())
            .unwrap_or(self.created_at)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DownloadProgressEvent {
    pub id: DownloadId,
    pub status: DownloadStatus,
    pub group: DownloadGroup,
    pub completed_segments: usize,
    pub total_segments: usize,
    pub failed_segment_count: usize,
    pub total_bytes: u64,
    pub speed_bytes_per_sec: u64,
    pub percentage: f64,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateDownloadParams {
    pub url: String,
    pub filename: Option<String>,
    pub output_dir: Option<String>,
    pub extra_headers: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxySettings {
    pub enabled: bool,
    pub url: String,
}

pub const DEFAULT_DOWNLOAD_CONCURRENCY: usize = 8;
pub const MIN_DOWNLOAD_CONCURRENCY: usize = 1;
pub const MAX_DOWNLOAD_CONCURRENCY: usize = 64;

pub fn normalize_download_concurrency(value: usize) -> usize {
    value.clamp(MIN_DOWNLOAD_CONCURRENCY, MAX_DOWNLOAD_CONCURRENCY)
}

impl Default for ProxySettings {
    fn default() -> Self {
        let default_url = if cfg!(target_os = "macos") {
            "http://127.0.0.1:7890"
        } else {
            "http://127.0.0.1:10808"
        };

        Self {
            enabled: false,
            url: default_url.to_string(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub default_download_dir: Option<String>,
    pub proxy: ProxySettings,
    pub download_concurrency: usize,
    pub delete_ts_temp_dir_after_download: bool,
    pub convert_to_mp4: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            default_download_dir: None,
            proxy: ProxySettings::default(),
            download_concurrency: DEFAULT_DOWNLOAD_CONCURRENCY,
            delete_ts_temp_dir_after_download: true,
            convert_to_mp4: true,
        }
    }
}

impl AppSettings {
    pub fn sanitize(&mut self) {
        self.download_concurrency = normalize_download_concurrency(self.download_concurrency);
    }
}

#[derive(Debug, Clone)]
pub struct EncryptionInfo {
    pub method: String,
    pub key_uri: String,
    pub iv: Option<String>,
    pub key_bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct SegmentInfo {
    pub index: usize,
    pub uri: String,
    pub duration: f32,
    pub sequence_number: u64,
    pub encryption: Option<EncryptionInfo>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DownloadGroup {
    Active,
    History,
}

pub fn download_group_for_status(status: &DownloadStatus) -> DownloadGroup {
    match status {
        DownloadStatus::Pending
        | DownloadStatus::Downloading
        | DownloadStatus::Paused
        | DownloadStatus::Merging
        | DownloadStatus::Converting => DownloadGroup::Active,
        DownloadStatus::Completed | DownloadStatus::Failed(_) | DownloadStatus::Cancelled => {
            DownloadGroup::History
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadTaskSummary {
    pub id: DownloadId,
    pub filename: String,
    pub encryption_method: Option<String>,
    pub output_dir: String,
    pub status: DownloadStatus,
    pub total_segments: usize,
    pub completed_segments: usize,
    pub failed_segment_count: usize,
    pub total_bytes: u64,
    pub speed_bytes_per_sec: u64,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub updated_at: String,
    pub file_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadTaskSegmentState {
    pub id: DownloadId,
    pub total_segments: usize,
    pub completed_segment_indices: Vec<usize>,
    pub failed_segment_indices: Vec<usize>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadCounts {
    pub active_count: usize,
    pub history_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadTaskPage {
    pub items: Vec<DownloadTaskSummary>,
    pub total: usize,
    pub page: usize,
    pub page_size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackSourceKind {
    Hls,
    File,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChromiumBrowser {
    Chrome,
    Edge,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenPlaybackSessionResponse {
    pub window_label: String,
    pub playback_url: String,
    pub playback_kind: PlaybackSourceKind,
    pub session_token: String,
    pub filename: String,
    pub status: DownloadStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChromiumExtensionInstallResult {
    pub extension_path: String,
    pub manual_url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FirefoxExtensionInstallResult {
    pub extension_path: String,
    pub manual_url: String,
}
