use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub type DownloadId = String;
pub type RequestHeaders = HashMap<String, String>;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileType {
    Hls,
    Mp4,
    Mkv,
    Avi,
    Wmv,
    Flv,
    Webm,
    Mov,
    Rmvb,
}

impl Default for FileType {
    fn default() -> Self {
        FileType::Hls
    }
}

impl FileType {
    pub fn is_direct_download(self) -> bool {
        !matches!(self, FileType::Hls)
    }

    pub fn supports_progressive_playback(self) -> bool {
        matches!(self, FileType::Mp4 | FileType::Webm)
    }

    pub fn default_extension(self) -> Option<&'static str> {
        match self {
            FileType::Hls => None,
            FileType::Mp4 => Some("mp4"),
            FileType::Mkv => Some("mkv"),
            FileType::Avi => Some("avi"),
            FileType::Wmv => Some("wmv"),
            FileType::Flv => Some("flv"),
            FileType::Webm => Some("webm"),
            FileType::Mov => Some("mov"),
            FileType::Rmvb => Some("rmvb"),
        }
    }
}

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
    pub file_type: FileType,
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
    #[serde(default)]
    pub file_type: FileType,
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
pub const DEFAULT_DOWNLOAD_SPEED_LIMIT_KBPS: u64 = 0;

pub fn normalize_download_concurrency(value: usize) -> usize {
    value.clamp(MIN_DOWNLOAD_CONCURRENCY, MAX_DOWNLOAD_CONCURRENCY)
}

pub fn normalize_download_speed_limit_kbps(value: u64) -> u64 {
    value
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
    pub download_speed_limit_kbps: u64,
    pub delete_ts_temp_dir_after_download: bool,
    pub convert_to_mp4: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            default_download_dir: None,
            proxy: ProxySettings::default(),
            download_concurrency: DEFAULT_DOWNLOAD_CONCURRENCY,
            download_speed_limit_kbps: DEFAULT_DOWNLOAD_SPEED_LIMIT_KBPS,
            delete_ts_temp_dir_after_download: true,
            convert_to_mp4: true,
        }
    }
}

impl AppSettings {
    pub fn sanitize(&mut self) {
        self.download_concurrency = normalize_download_concurrency(self.download_concurrency);
        self.download_speed_limit_kbps =
            normalize_download_speed_limit_kbps(self.download_speed_limit_kbps);
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
    #[serde(default)]
    pub file_type: FileType,
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
pub enum ResumeDownloadAction {
    Resume,
    ConfirmRestart,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeDownloadCheckResult {
    pub action: ResumeDownloadAction,
    pub downloaded_bytes: u64,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_settings_defaults_download_speed_limit_to_unlimited() {
        let settings: AppSettings = serde_json::from_str(
            r#"{
                "default_download_dir": null,
                "proxy": {"enabled": false, "url": "http://127.0.0.1:10808"},
                "download_concurrency": 8,
                "delete_ts_temp_dir_after_download": true,
                "convert_to_mp4": true
            }"#,
        )
        .expect("settings deserialize");

        assert_eq!(
            settings.download_speed_limit_kbps,
            DEFAULT_DOWNLOAD_SPEED_LIMIT_KBPS
        );
    }

    #[test]
    fn app_settings_keeps_positive_download_speed_limit() {
        let mut settings = AppSettings {
            download_speed_limit_kbps: 1024,
            ..AppSettings::default()
        };

        settings.sanitize();

        assert_eq!(settings.download_speed_limit_kbps, 1024);
    }

    #[test]
    fn file_type_direct_download_variants_report_extensions() {
        let cases = [
            (FileType::Mp4, Some("mp4")),
            (FileType::Mkv, Some("mkv")),
            (FileType::Avi, Some("avi")),
            (FileType::Wmv, Some("wmv")),
            (FileType::Flv, Some("flv")),
            (FileType::Webm, Some("webm")),
            (FileType::Mov, Some("mov")),
            (FileType::Rmvb, Some("rmvb")),
        ];

        for (file_type, expected_extension) in cases {
            assert!(file_type.is_direct_download());
            assert_eq!(file_type.default_extension(), expected_extension);
        }

        assert!(!FileType::Hls.is_direct_download());
        assert_eq!(FileType::Hls.default_extension(), None);
    }

    #[test]
    fn file_type_progressive_playback_is_limited_to_mp4_and_webm() {
        assert!(FileType::Mp4.supports_progressive_playback());
        assert!(FileType::Webm.supports_progressive_playback());

        for file_type in [
            FileType::Hls,
            FileType::Mkv,
            FileType::Avi,
            FileType::Wmv,
            FileType::Flv,
            FileType::Mov,
            FileType::Rmvb,
        ] {
            assert!(!file_type.supports_progressive_playback());
        }
    }
}
