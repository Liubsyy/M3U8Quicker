use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

use crate::downloader;
use crate::models::{
    DownloadId, DownloadTask, ProxySettings, DEFAULT_DOWNLOAD_CONCURRENCY,
    DEFAULT_DOWNLOAD_SPEED_LIMIT_KBPS, DEFAULT_PREVIEW_COLUMNS, DEFAULT_PREVIEW_COUNT,
    DEFAULT_PREVIEW_JPEG_QUALITY, DEFAULT_PREVIEW_THUMBNAIL_WIDTH,
};
use crate::playback::{DownloadPriorityState, PlaybackServerState, PlaybackSession};
use crate::preview::PreviewSession;

pub struct AppState {
    pub downloads: Arc<Mutex<HashMap<DownloadId, DownloadTask>>>,
    pub download_store_lock: Arc<Mutex<()>>,
    pub cancel_tokens: Arc<Mutex<HashMap<DownloadId, CancellationToken>>>,
    pub http_client: Arc<RwLock<reqwest::Client>>,
    pub default_download_dir: Arc<Mutex<String>>,
    pub proxy_settings: Arc<Mutex<ProxySettings>>,
    pub max_concurrent_segments: Arc<Mutex<usize>>,
    pub download_rate_limiter: Arc<downloader::DownloadRateLimiter>,
    pub preview_columns: Arc<Mutex<usize>>,
    pub preview_count: Arc<Mutex<usize>>,
    pub preview_thumbnail_width: Arc<Mutex<u32>>,
    pub preview_jpeg_quality: Arc<Mutex<u8>>,
    pub delete_ts_temp_dir_after_download: Arc<Mutex<bool>>,
    pub convert_to_mp4: Arc<Mutex<bool>>,
    pub ffmpeg_enabled: Arc<Mutex<bool>>,
    pub playback_server: Arc<RwLock<Option<PlaybackServerState>>>,
    pub playback_sessions: Arc<Mutex<HashMap<DownloadId, PlaybackSession>>>,
    pub download_priorities: Arc<Mutex<HashMap<DownloadId, Arc<DownloadPriorityState>>>>,
    pub ffmpeg_path: Arc<Mutex<Option<String>>>,
    pub preview_sessions: Arc<Mutex<HashMap<String, Arc<PreviewSession>>>>,
}

impl AppState {
    pub fn new(download_dir: String) -> Self {
        let client = downloader::build_http_client(None).expect("Failed to create HTTP client");

        Self {
            downloads: Arc::new(Mutex::new(HashMap::new())),
            download_store_lock: Arc::new(Mutex::new(())),
            cancel_tokens: Arc::new(Mutex::new(HashMap::new())),
            http_client: Arc::new(RwLock::new(client)),
            default_download_dir: Arc::new(Mutex::new(download_dir)),
            proxy_settings: Arc::new(Mutex::new(ProxySettings::default())),
            max_concurrent_segments: Arc::new(Mutex::new(DEFAULT_DOWNLOAD_CONCURRENCY)),
            download_rate_limiter: Arc::new(downloader::DownloadRateLimiter::new(
                DEFAULT_DOWNLOAD_SPEED_LIMIT_KBPS,
            )),
            preview_columns: Arc::new(Mutex::new(DEFAULT_PREVIEW_COLUMNS)),
            preview_count: Arc::new(Mutex::new(DEFAULT_PREVIEW_COUNT)),
            preview_thumbnail_width: Arc::new(Mutex::new(DEFAULT_PREVIEW_THUMBNAIL_WIDTH)),
            preview_jpeg_quality: Arc::new(Mutex::new(DEFAULT_PREVIEW_JPEG_QUALITY)),
            delete_ts_temp_dir_after_download: Arc::new(Mutex::new(true)),
            convert_to_mp4: Arc::new(Mutex::new(true)),
            ffmpeg_enabled: Arc::new(Mutex::new(true)),
            playback_server: Arc::new(RwLock::new(None)),
            playback_sessions: Arc::new(Mutex::new(HashMap::new())),
            download_priorities: Arc::new(Mutex::new(HashMap::new())),
            ffmpeg_path: Arc::new(Mutex::new(None)),
            preview_sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}
