use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aes::{Aes128, Aes192, Aes256};
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockDecryptMut, KeyIvInit};
use chrono::Utc;
use futures::StreamExt;
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use reqwest::{header, StatusCode};
use serde::Deserialize;
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
const VIDEO_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);
const MP4_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60 * 60);

pub enum DownloadRunOutcome {
    Completed(PathBuf),
    Incomplete,
}

enum SegmentDownloadOutcome {
    Downloaded(u64),
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mp4ResumeCheck {
    Ready { downloaded_bytes: u64 },
    RequiresRestartConfirmation { downloaded_bytes: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mp4ResumeResponseMode {
    Append,
    RestartRequired,
    Unexpected,
}

#[derive(Debug, Clone)]
pub enum PreparedHlsDownload {
    Single(PreparedSingleHlsDownload),
    Bundle(PreparedBundleHlsDownload),
}

#[derive(Debug, Clone)]
pub struct PreparedSingleHlsDownload {
    pub media_kind: HlsMediaKind,
    pub init_segments: Vec<PreparedHlsInitSegment>,
    pub segments: Vec<SegmentInfo>,
    pub selection: Option<HlsTrackSelection>,
}

#[derive(Debug, Clone)]
pub struct PreparedHlsInitSegment {
    pub info: HlsInitSegmentInfo,
    pub encryption: Option<EncryptionInfo>,
}

impl PreparedHlsInitSegment {
    pub fn output_path(&self, temp_dir: &Path) -> PathBuf {
        hls_init_segment_file_path(temp_dir, self.info.index)
    }
}

#[derive(Debug, Clone)]
pub struct PreparedBundleHlsDownload {
    pub selection: HlsTrackSelection,
    pub playlist_files: Vec<BundlePlaylistFile>,
    pub entries: Vec<BundleDownloadEntry>,
}

impl PreparedBundleHlsDownload {
    pub fn total_units(&self) -> usize {
        self.entries.len()
    }

    pub fn source_uris(&self) -> Vec<String> {
        self.entries.iter().map(|entry| entry.uri.clone()).collect()
    }

    pub fn durations(&self) -> Vec<f32> {
        self.entries.iter().map(|entry| entry.duration).collect()
    }

    pub fn encryption_method(&self) -> Option<String> {
        self.entries
            .iter()
            .find_map(|entry| entry.encryption.as_ref())
            .map(|encryption| encryption.method.clone())
    }
}

#[derive(Debug, Clone)]
pub struct BundlePlaylistFile {
    pub relative_path: PathBuf,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct BundleDownloadEntry {
    pub index: usize,
    pub uri: String,
    pub duration: f32,
    pub sequence_number: u64,
    pub byte_range: Option<ByteRangeSpec>,
    pub encryption: Option<EncryptionInfo>,
    pub relative_path: PathBuf,
}

impl BundleDownloadEntry {
    fn output_path(&self, bundle_dir: &Path) -> PathBuf {
        bundle_dir.join(&self.relative_path)
    }
}

#[derive(Debug, Clone)]
struct FetchedPlaylist {
    base_url: Url,
    playlist: m3u8_rs::Playlist,
}

#[derive(Debug, Clone)]
struct MasterVideoTrack {
    option: HlsTrackOption,
    uri: String,
}

#[derive(Debug, Clone)]
struct MasterAlternativeTrack {
    option: HlsTrackOption,
    uri: String,
}

#[derive(Debug, Clone)]
struct MasterTrackCatalog {
    inspection: InspectHlsTracksResult,
    videos: Vec<MasterVideoTrack>,
    audios: Vec<MasterAlternativeTrack>,
    subtitles: Vec<MasterAlternativeTrack>,
}

#[derive(Debug, Clone)]
struct ParsedEncryptionState {
    method: String,
    key_uri: String,
    iv: Option<String>,
}

#[derive(Debug, Clone)]
struct BundleMapState {
    local_file_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BundleMapCacheKey {
    uri: String,
    byte_range: Option<ByteRangeSpec>,
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

pub async fn inspect_hls_tracks(
    client: &reqwest::Client,
    m3u8_url: &str,
    headers: &RequestHeaders,
) -> Result<InspectHlsTracksResult, AppError> {
    let fetched = fetch_hls_playlist(client, m3u8_url, headers).await?;

    match fetched.playlist {
        m3u8_rs::Playlist::MediaPlaylist(_) => Ok(InspectHlsTracksResult {
            kind: HlsPlaylistKind::Media,
            requires_selection: false,
            video_tracks: Vec::new(),
            audio_tracks: Vec::new(),
            subtitle_tracks: Vec::new(),
            default_selection: HlsTrackSelection::default(),
        }),
        m3u8_rs::Playlist::MasterPlaylist(master) => {
            Ok(build_master_track_catalog(&fetched.base_url, &master)?.inspection)
        }
    }
}

pub async fn prepare_hls_download(
    client: &reqwest::Client,
    m3u8_url: &str,
    headers: &RequestHeaders,
    selection: Option<&HlsTrackSelection>,
) -> Result<PreparedHlsDownload, AppError> {
    let fetched = fetch_hls_playlist(client, m3u8_url, headers).await?;

    match fetched.playlist {
        m3u8_rs::Playlist::MediaPlaylist(media) => {
            let mut plan = parse_media_playlist_plan(&fetched.base_url, &media)?;
            validate_fmp4_init_encryption(&plan.init_segments)?;
            fetch_prepared_init_encryption_keys(client, &mut plan.init_segments, headers).await?;
            fetch_encryption_keys(client, &mut plan.segments, headers).await?;

            Ok(PreparedHlsDownload::Single(PreparedSingleHlsDownload {
                media_kind: plan.media_kind,
                init_segments: plan.init_segments,
                segments: plan.segments,
                selection: None,
            }))
        }
        m3u8_rs::Playlist::MasterPlaylist(master) => {
            let catalog = build_master_track_catalog(&fetched.base_url, &master)?;
            let default_selection = catalog.inspection.default_selection.clone();
            let requested_selection = selection.cloned().unwrap_or_default();
            let selected_video_id = requested_selection
                .video_id
                .clone()
                .or(default_selection.video_id.clone())
                .ok_or_else(|| AppError::M3u8Parse("No variants found".into()))?;
            let selected_video = catalog
                .videos
                .iter()
                .find(|track| track.option.id == selected_video_id)
                .cloned()
                .ok_or_else(|| {
                    AppError::InvalidInput("所选视频轨道不存在，请重新解析后再下载".to_string())
                })?;

            let available_audios = tracks_for_group(
                &catalog.audios,
                selected_video.option.audio_group_id.as_deref(),
            );
            let available_subtitles = tracks_for_group(
                &catalog.subtitles,
                selected_video.option.subtitle_group_id.as_deref(),
            );
            let selected_audio = resolve_selected_alternative_track(
                &available_audios,
                requested_selection.audio_id.as_deref(),
                default_audio_track_id(&available_audios).as_deref(),
                "音频",
            )?;
            let selected_subtitle = resolve_selected_optional_track(
                &available_subtitles,
                requested_selection.subtitle_id.as_deref(),
                "字幕",
            )?;

            let resolved_selection = HlsTrackSelection {
                video_id: Some(selected_video.option.id.clone()),
                audio_id: selected_audio.as_ref().map(|track| track.option.id.clone()),
                subtitle_id: selected_subtitle
                    .as_ref()
                    .map(|track| track.option.id.clone()),
            };

            if selected_audio.is_none() && selected_subtitle.is_none() {
                let video_playlist =
                    fetch_media_playlist_following_variants(client, &selected_video.uri, headers)
                        .await?;
                let mut plan =
                    parse_media_playlist_plan(&video_playlist.base_url, &video_playlist.playlist)?;
                validate_fmp4_init_encryption(&plan.init_segments)?;
                fetch_prepared_init_encryption_keys(client, &mut plan.init_segments, headers)
                    .await?;
                fetch_encryption_keys(client, &mut plan.segments, headers).await?;

                return Ok(PreparedHlsDownload::Single(PreparedSingleHlsDownload {
                    media_kind: plan.media_kind,
                    init_segments: plan.init_segments,
                    segments: plan.segments,
                    selection: Some(resolved_selection),
                }));
            }

            let video_playlist =
                fetch_media_playlist_following_variants(client, &selected_video.uri, headers)
                    .await?;
            let mut plan = build_bundle_track_plan(&video_playlist, "video")?;

            if let Some(selected_audio) = selected_audio {
                let audio_playlist =
                    fetch_media_playlist_following_variants(client, &selected_audio.uri, headers)
                        .await?;
                plan.extend(build_bundle_track_plan(&audio_playlist, "audio")?);
            }

            if let Some(selected_subtitle) = selected_subtitle {
                let subtitle_playlist = fetch_media_playlist_following_variants(
                    client,
                    &selected_subtitle.uri,
                    headers,
                )
                .await?;
                plan.extend(build_bundle_track_plan(&subtitle_playlist, "subtitle")?);
            }

            fetch_bundle_encryption_keys(client, &mut plan.entries, headers).await?;

            Ok(PreparedHlsDownload::Bundle(PreparedBundleHlsDownload {
                selection: resolved_selection,
                playlist_files: plan.playlist_files,
                entries: plan.entries,
            }))
        }
    }
}

pub async fn inspect_dash_tracks(
    client: &reqwest::Client,
    source: &str,
    headers: &RequestHeaders,
    source_kind: DownloadSourceKind,
) -> Result<InspectHlsTracksResult, AppError> {
    let manifest = fetch_or_parse_dash_manifest(client, source, headers, source_kind).await?;
    Ok(build_dash_inspection(&manifest))
}

pub async fn prepare_dash_download(
    client: &reqwest::Client,
    source: &str,
    headers: &RequestHeaders,
    source_kind: DownloadSourceKind,
    selection: Option<&HlsTrackSelection>,
) -> Result<PreparedBundleHlsDownload, AppError> {
    let manifest = fetch_or_parse_dash_manifest(client, source, headers, source_kind).await?;
    build_dash_bundle_download(&manifest, selection)
}

async fn fetch_or_parse_dash_manifest(
    client: &reqwest::Client,
    source: &str,
    headers: &RequestHeaders,
    source_kind: DownloadSourceKind,
) -> Result<DashManifest, AppError> {
    match source_kind {
        DownloadSourceKind::Url => fetch_dash_manifest(client, source, headers).await,
        DownloadSourceKind::InlineDashJson => parse_dash_json_manifest(source),
    }
}

async fn fetch_dash_manifest(
    client: &reqwest::Client,
    url: &str,
    headers: &RequestHeaders,
) -> Result<DashManifest, AppError> {
    let base_url = Url::parse(url)?;
    let response = build_request_with_headers(client, url, headers)
        .timeout(M3U8_METADATA_TIMEOUT)
        .send()
        .await?
        .error_for_status()?;
    let body = response.text().await?;
    parse_dash_mpd_manifest(&body, &base_url)
}

#[derive(Debug, Clone)]
struct DashManifest {
    video_tracks: Vec<DashTrack>,
    audio_tracks: Vec<DashTrack>,
    default_selection: HlsTrackSelection,
}

#[derive(Debug, Clone)]
struct DashTrack {
    option: HlsTrackOption,
    init: Option<DashResource>,
    segments: Vec<DashSegment>,
}

#[derive(Debug, Clone)]
struct DashResource {
    uri: String,
    byte_range: Option<ByteRangeSpec>,
}

#[derive(Debug, Clone)]
struct DashSegment {
    uri: String,
    duration: f32,
    sequence_number: u64,
    byte_range: Option<ByteRangeSpec>,
}

#[derive(Debug, Clone, Default)]
struct DashSegmentTemplate {
    timescale: u64,
    initialization: Option<String>,
    media: Option<String>,
    start_number: u64,
    duration: Option<u64>,
    timeline: Vec<DashTimelineItem>,
}

#[derive(Debug, Clone)]
struct DashTimelineItem {
    start_time: Option<u64>,
    duration: u64,
    repeat: i64,
}

#[derive(Debug, Clone, Default)]
struct DashAdaptationBuild {
    content_type: Option<String>,
    mime_type: Option<String>,
    lang: Option<String>,
    base_url: Option<String>,
    segment_template: Option<DashSegmentTemplate>,
    representations: Vec<DashRepresentationBuild>,
}

#[derive(Debug, Clone, Default)]
struct DashRepresentationBuild {
    id: String,
    bandwidth: Option<u64>,
    width: Option<u64>,
    height: Option<u64>,
    codecs: Option<String>,
    base_url: Option<String>,
    segment_template: Option<DashSegmentTemplate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DashTemplateTarget {
    Adaptation,
    Representation,
}

#[derive(Debug, Deserialize)]
struct DashJsonManifest {
    format: String,
    title: Option<String>,
    base_url: String,
    tracks: DashJsonTracks,
    #[serde(default)]
    default_selection: HlsTrackSelection,
}

#[derive(Debug, Deserialize)]
struct DashJsonTracks {
    #[serde(default)]
    video: Vec<DashJsonTrack>,
    #[serde(default)]
    audio: Vec<DashJsonTrack>,
}

#[derive(Debug, Deserialize)]
struct DashJsonTrack {
    id: String,
    label: Option<String>,
    bandwidth: Option<u64>,
    resolution: Option<String>,
    codecs: Option<String>,
    language: Option<String>,
    #[serde(default)]
    init: Option<String>,
    #[serde(default)]
    byte_range: Option<String>,
    segments: Vec<DashJsonSegment>,
}

#[derive(Debug, Deserialize)]
struct DashJsonSegment {
    uri: String,
    duration: f32,
    #[serde(default)]
    byte_range: Option<String>,
}

fn build_dash_inspection(manifest: &DashManifest) -> InspectHlsTracksResult {
    InspectHlsTracksResult {
        kind: HlsPlaylistKind::Master,
        requires_selection: manifest.video_tracks.len() > 1 || manifest.audio_tracks.len() > 1,
        video_tracks: manifest
            .video_tracks
            .iter()
            .map(|track| track.option.clone())
            .collect(),
        audio_tracks: manifest
            .audio_tracks
            .iter()
            .map(|track| track.option.clone())
            .collect(),
        subtitle_tracks: Vec::new(),
        default_selection: manifest.default_selection.clone(),
    }
}

fn build_dash_bundle_download(
    manifest: &DashManifest,
    selection: Option<&HlsTrackSelection>,
) -> Result<PreparedBundleHlsDownload, AppError> {
    let inspection = build_dash_inspection(manifest);
    let requested = selection.cloned().unwrap_or_default();
    let video_id = requested
        .video_id
        .or(inspection.default_selection.video_id.clone())
        .or_else(|| inspection.video_tracks.first().map(|track| track.id.clone()))
        .ok_or_else(|| AppError::M3u8Parse("DASH manifest missing video track".to_string()))?;
    let selected_video = manifest
        .video_tracks
        .iter()
        .find(|track| track.option.id == video_id)
        .ok_or_else(|| AppError::InvalidInput("所选 DASH 视频轨道不存在，请重新解析后再下载".to_string()))?;

    let audio_id = requested
        .audio_id
        .or(inspection.default_selection.audio_id.clone())
        .or_else(|| manifest.audio_tracks.first().map(|track| track.option.id.clone()));
    let selected_audio = audio_id
        .as_deref()
        .and_then(|id| manifest.audio_tracks.iter().find(|track| track.option.id == id));

    let resolved_selection = HlsTrackSelection {
        video_id: Some(selected_video.option.id.clone()),
        audio_id: selected_audio.map(|track| track.option.id.clone()),
        subtitle_id: None,
    };

    let mut plan = build_dash_track_bundle_plan(selected_video, "video")?;
    if let Some(selected_audio) = selected_audio {
        plan.extend(build_dash_track_bundle_plan(selected_audio, "audio")?);
    }

    Ok(PreparedBundleHlsDownload {
        selection: resolved_selection,
        playlist_files: plan.playlist_files,
        entries: plan.entries,
    })
}

fn build_dash_track_bundle_plan(
    track: &DashTrack,
    subdir: &str,
) -> Result<BundleTrackPlanBuild, AppError> {
    let mut entries = Vec::with_capacity(track.segments.len() + 1);
    let init_file_name = if let Some(init) = &track.init {
        let name = format!("init_000001.{}", infer_file_extension(&init.uri, "mp4"));
        entries.push(BundleDownloadEntry {
            index: 0,
            uri: init.uri.clone(),
            duration: 0.0,
            sequence_number: 0,
            byte_range: init.byte_range.clone(),
            encryption: None,
            relative_path: PathBuf::from(subdir).join(&name),
        });
        Some(name)
    } else {
        None
    };

    let mut local_segments = Vec::with_capacity(track.segments.len());
    for (index, segment) in track.segments.iter().enumerate() {
        let local_segment_name = format!(
            "seg_{:06}.{}",
            index + 1,
            infer_file_extension(&segment.uri, "m4s")
        );
        entries.push(BundleDownloadEntry {
            index: entries.len(),
            uri: segment.uri.clone(),
            duration: segment.duration,
            sequence_number: segment.sequence_number,
            byte_range: segment.byte_range.clone(),
            encryption: None,
            relative_path: PathBuf::from(subdir).join(&local_segment_name),
        });

        let mut local_segment = m3u8_rs::MediaSegment {
            uri: local_segment_name,
            duration: segment.duration,
            title: None,
            map: None,
            ..Default::default()
        };
        if index == 0 {
            if let Some(init_name) = &init_file_name {
                local_segment.map = Some(m3u8_rs::Map {
                    uri: init_name.clone(),
                    ..Default::default()
                });
            }
        }
        local_segments.push(local_segment);
    }

    let target_duration = local_segments.iter().fold(1u64, |max_duration, segment| {
        max_duration.max(segment.duration.ceil().max(1.0) as u64)
    });
    let local_playlist = m3u8_rs::MediaPlaylist {
        version: Some(6),
        target_duration,
        media_sequence: 0,
        segments: local_segments,
        discontinuity_sequence: 0,
        end_list: true,
        playlist_type: Some(m3u8_rs::MediaPlaylistType::Vod),
        i_frames_only: false,
        start: None,
        independent_segments: true,
        unknown_tags: Vec::new(),
    };

    Ok(BundleTrackPlanBuild {
        playlist_files: vec![BundlePlaylistFile {
            relative_path: PathBuf::from(subdir).join("index.m3u8"),
            content: media_playlist_to_string(&local_playlist)?,
        }],
        entries,
    })
}

fn parse_dash_json_manifest(raw: &str) -> Result<DashManifest, AppError> {
    let parsed: DashJsonManifest = serde_json::from_str(raw)
        .map_err(|error| AppError::M3u8Parse(format!("DASH JSON 解析失败: {}", error)))?;
    if parsed.format != "m3u8quicker-dash-v1" {
        return Err(AppError::M3u8Parse(
            "DASH JSON format 必须为 m3u8quicker-dash-v1".to_string(),
        ));
    }
    let base_url = Url::parse(&parsed.base_url)?;
    let audio_group_id = (!parsed.tracks.audio.is_empty()).then_some("dash-audio".to_string());
    let video_tracks = parsed
        .tracks
        .video
        .iter()
        .enumerate()
        .map(|(index, track)| {
            dash_json_track_to_track(
                &base_url,
                track,
                HlsTrackType::Video,
                index,
                audio_group_id.clone(),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let audio_tracks = parsed
        .tracks
        .audio
        .iter()
        .enumerate()
        .map(|(index, track)| dash_json_track_to_track(&base_url, track, HlsTrackType::Audio, index, None))
        .collect::<Result<Vec<_>, _>>()?;
    if video_tracks.is_empty() {
        return Err(AppError::M3u8Parse("DASH JSON 缺少 video 轨道".to_string()));
    }

    let default_selection = HlsTrackSelection {
        video_id: parsed
            .default_selection
            .video_id
            .or_else(|| video_tracks.first().map(|track| track.option.id.clone())),
        audio_id: parsed
            .default_selection
            .audio_id
            .or_else(|| audio_tracks.first().map(|track| track.option.id.clone())),
        subtitle_id: None,
    };

    let _ = parsed.title;
    Ok(DashManifest {
        video_tracks,
        audio_tracks,
        default_selection,
    })
}

/// Build a self-contained HLS media playlist (as a string) from inline DASH JSON,
/// using the default video track only. The playlist references absolute remote
/// URLs, so it can be fed to ffmpeg/ffprobe (e.g. for preview/probe purposes)
/// without needing any local segment files.
pub fn build_dash_preview_playlist_from_json(raw_json: &str) -> Result<String, AppError> {
    let manifest = parse_dash_json_manifest(raw_json)?;
    build_dash_preview_playlist_content(&manifest)
}

/// If the inline DASH JSON's default video track is a single self-contained
/// segment (no init, no byte range), return that URL so callers can hand it
/// directly to ffmpeg without synthesizing a local HLS playlist. Returning
/// the bare URL means ffmpeg sees an HTTPS input and `-headers` propagation
/// works the same as with normal HLS/MP4 sources. Returns `Ok(None)` when
/// the manifest is too complex (multiple segments, fMP4 init, byte ranges).
pub fn inline_dash_preview_direct_url(raw_json: &str) -> Result<Option<String>, AppError> {
    let manifest = parse_dash_json_manifest(raw_json)?;
    let video_id = manifest
        .default_selection
        .video_id
        .as_deref()
        .or_else(|| {
            manifest
                .video_tracks
                .first()
                .map(|track| track.option.id.as_str())
        });
    let Some(video_id) = video_id else {
        return Ok(None);
    };
    let Some(track) = manifest
        .video_tracks
        .iter()
        .find(|track| track.option.id == video_id)
    else {
        return Ok(None);
    };

    if track.init.is_some() {
        return Ok(None);
    }
    if track.segments.len() != 1 {
        return Ok(None);
    }
    let segment = &track.segments[0];
    if segment.byte_range.is_some() {
        return Ok(None);
    }
    Ok(Some(segment.uri.clone()))
}

fn build_dash_preview_playlist_content(manifest: &DashManifest) -> Result<String, AppError> {
    let video_id = manifest
        .default_selection
        .video_id
        .as_deref()
        .or_else(|| {
            manifest
                .video_tracks
                .first()
                .map(|track| track.option.id.as_str())
        })
        .ok_or_else(|| AppError::M3u8Parse("DASH manifest 缺少视频轨道".to_string()))?;
    let track = manifest
        .video_tracks
        .iter()
        .find(|track| track.option.id == video_id)
        .ok_or_else(|| AppError::M3u8Parse("DASH manifest 缺少视频轨道".to_string()))?;

    let map = track.init.as_ref().map(|init| m3u8_rs::Map {
        uri: init.uri.clone(),
        byte_range: init.byte_range.as_ref().map(byte_range_spec_to_m3u8),
        ..Default::default()
    });

    let segments = track
        .segments
        .iter()
        .enumerate()
        .map(|(index, segment)| m3u8_rs::MediaSegment {
            uri: segment.uri.clone(),
            duration: segment.duration,
            title: None,
            byte_range: segment.byte_range.as_ref().map(byte_range_spec_to_m3u8),
            map: if index == 0 { map.clone() } else { None },
            ..Default::default()
        })
        .collect::<Vec<_>>();

    let target_duration = segments.iter().fold(1u64, |max_duration, segment| {
        max_duration.max(segment.duration.ceil().max(1.0) as u64)
    });

    let playlist = m3u8_rs::MediaPlaylist {
        version: Some(7),
        target_duration,
        media_sequence: 0,
        segments,
        discontinuity_sequence: 0,
        end_list: true,
        playlist_type: Some(m3u8_rs::MediaPlaylistType::Vod),
        i_frames_only: false,
        start: None,
        independent_segments: true,
        unknown_tags: Vec::new(),
    };

    media_playlist_to_string(&playlist)
}

fn byte_range_spec_to_m3u8(spec: &ByteRangeSpec) -> m3u8_rs::ByteRange {
    m3u8_rs::ByteRange {
        length: spec.length,
        offset: spec.offset,
    }
}

fn dash_json_track_to_track(
    base_url: &Url,
    track: &DashJsonTrack,
    track_type: HlsTrackType,
    index: usize,
    audio_group_id: Option<String>,
) -> Result<DashTrack, AppError> {
    let init = match track.init.as_deref() {
        Some(init_url) => Some(DashResource {
            uri: resolve_url(base_url, init_url),
            byte_range: track
                .byte_range
                .as_deref()
                .map(parse_dash_byte_range)
                .transpose()?,
        }),
        None => None,
    };
    let segments = track
        .segments
        .iter()
        .enumerate()
        .map(|(segment_index, segment)| {
            Ok(DashSegment {
                uri: resolve_url(base_url, &segment.uri),
                duration: segment.duration,
                sequence_number: segment_index as u64 + 1,
                byte_range: segment
                    .byte_range
                    .as_deref()
                    .map(parse_dash_byte_range)
                    .transpose()?,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    if segments.is_empty() {
        return Err(AppError::M3u8Parse(format!(
            "DASH JSON 轨道 {} 缺少 segments",
            track.id
        )));
    }

    let group_id = (track_type == HlsTrackType::Audio).then_some("dash-audio".to_string());
    Ok(DashTrack {
        option: HlsTrackOption {
            id: track.id.clone(),
            track_type,
            label: track.label.clone().unwrap_or_else(|| {
                if let Some(resolution) = &track.resolution {
                    resolution.clone()
                } else {
                    format!("轨道 {}", index + 1)
                }
            }),
            name: track.label.clone(),
            language: track.language.clone(),
            group_id,
            audio_group_id,
            subtitle_group_id: None,
            bandwidth: track.bandwidth,
            resolution: track.resolution.clone(),
            codecs: track.codecs.clone(),
            is_default: index == 0,
            is_autoselect: index == 0,
            is_forced: false,
        },
        init,
        segments,
    })
}

fn parse_dash_mpd_manifest(raw: &str, manifest_url: &Url) -> Result<DashManifest, AppError> {
    let mut reader = Reader::from_str(raw);
    reader.config_mut().trim_text(true);

    let mut manifest_base = manifest_url.clone();
    let mut media_presentation_duration = None;
    let mut adaptations = Vec::<DashAdaptationBuild>::new();
    let mut current_adaptation: Option<DashAdaptationBuild> = None;
    let mut current_representation: Option<DashRepresentationBuild> = None;
    let mut editing_template: Option<(DashTemplateTarget, DashSegmentTemplate)> = None;
    let mut collecting_base_url = false;
    let mut base_url_text = String::new();
    let mut tag_stack: Vec<Vec<u8>> = Vec::new();

    loop {
        match reader
            .read_event()
            .map_err(|error| AppError::M3u8Parse(format!("DASH MPD XML 解析失败: {}", error)))?
        {
            Event::Start(event) => {
                let name = event.name().as_ref().to_vec();
                if name.as_slice() == b"MPD" {
                    if attr_value(&reader, &event, b"type")?
                        .as_deref()
                        .is_some_and(|value| value.eq_ignore_ascii_case("dynamic"))
                    {
                        return Err(AppError::M3u8Parse("暂不支持 dynamic/live DASH".to_string()));
                    }
                    media_presentation_duration =
                        attr_value(&reader, &event, b"mediaPresentationDuration")?
                            .and_then(|value| parse_iso8601_duration_seconds(&value));
                } else if name.as_slice() == b"ContentProtection" {
                    return Err(AppError::M3u8Parse("暂不支持 DRM/encrypted DASH".to_string()));
                } else if name.as_slice() == b"AdaptationSet" {
                    current_adaptation = Some(DashAdaptationBuild {
                        content_type: attr_value(&reader, &event, b"contentType")?,
                        mime_type: attr_value(&reader, &event, b"mimeType")?,
                        lang: attr_value(&reader, &event, b"lang")?,
                        ..Default::default()
                    });
                } else if name.as_slice() == b"Representation" {
                    current_representation = Some(DashRepresentationBuild {
                        id: attr_value(&reader, &event, b"id")?.unwrap_or_default(),
                        bandwidth: attr_value(&reader, &event, b"bandwidth")?
                            .and_then(|value| value.parse().ok()),
                        width: attr_value(&reader, &event, b"width")?.and_then(|value| value.parse().ok()),
                        height: attr_value(&reader, &event, b"height")?.and_then(|value| value.parse().ok()),
                        codecs: attr_value(&reader, &event, b"codecs")?,
                        ..Default::default()
                    });
                } else if name.as_slice() == b"SegmentTemplate" {
                    let target = if current_representation.is_some() {
                        DashTemplateTarget::Representation
                    } else {
                        DashTemplateTarget::Adaptation
                    };
                    editing_template = Some((target, parse_dash_segment_template_attrs(&reader, &event)?));
                } else if name.as_slice() == b"S" {
                    if let Some((_, template)) = editing_template.as_mut() {
                        template.timeline.push(parse_dash_timeline_item(&reader, &event)?);
                    }
                } else if name.as_slice() == b"BaseURL" {
                    collecting_base_url = true;
                    base_url_text.clear();
                } else if matches!(
                    name.as_slice(),
                    b"SegmentBase" | b"SegmentList" | b"SegmentURL"
                ) {
                    return Err(AppError::M3u8Parse(
                        "暂不支持 SegmentBase/SegmentList DASH".to_string(),
                    ));
                }
                tag_stack.push(name);
            }
            Event::Empty(event) => {
                let name = event.name().as_ref().to_vec();
                if name.as_slice() == b"ContentProtection" {
                    return Err(AppError::M3u8Parse("暂不支持 DRM/encrypted DASH".to_string()));
                } else if name.as_slice() == b"SegmentTemplate" {
                    let target = if current_representation.is_some() {
                        DashTemplateTarget::Representation
                    } else {
                        DashTemplateTarget::Adaptation
                    };
                    assign_dash_template(
                        target,
                        parse_dash_segment_template_attrs(&reader, &event)?,
                        current_adaptation.as_mut(),
                        current_representation.as_mut(),
                    );
                } else if name.as_slice() == b"S" {
                    if let Some((_, template)) = editing_template.as_mut() {
                        template.timeline.push(parse_dash_timeline_item(&reader, &event)?);
                    }
                } else if name.as_slice() == b"Representation" {
                    if let Some(adaptation) = current_adaptation.as_mut() {
                        let mut representation = DashRepresentationBuild {
                            id: attr_value(&reader, &event, b"id")?.unwrap_or_default(),
                            bandwidth: attr_value(&reader, &event, b"bandwidth")?
                                .and_then(|value| value.parse().ok()),
                            width: attr_value(&reader, &event, b"width")?
                                .and_then(|value| value.parse().ok()),
                            height: attr_value(&reader, &event, b"height")?
                                .and_then(|value| value.parse().ok()),
                            codecs: attr_value(&reader, &event, b"codecs")?,
                            ..Default::default()
                        };
                        if representation.id.trim().is_empty() {
                            representation.id =
                                format!("rep-{}", adaptation.representations.len() + 1);
                        }
                        adaptation.representations.push(representation);
                    }
                } else if matches!(
                    name.as_slice(),
                    b"SegmentBase" | b"SegmentList" | b"SegmentURL"
                ) {
                    return Err(AppError::M3u8Parse(
                        "暂不支持 SegmentBase/SegmentList DASH".to_string(),
                    ));
                }
            }
            Event::Text(event) => {
                if collecting_base_url {
                    base_url_text.push_str(&String::from_utf8_lossy(event.as_ref()));
                }
            }
            Event::End(event) => {
                let name = event.name().as_ref().to_vec();
                if name.as_slice() == b"BaseURL" {
                    let value = base_url_text.trim().to_string();
                    if !value.is_empty() {
                        if let Some(representation) = current_representation.as_mut() {
                            representation.base_url = Some(value);
                        } else if let Some(adaptation) = current_adaptation.as_mut() {
                            adaptation.base_url = Some(value);
                        } else {
                            manifest_base = manifest_base.join(&value)?;
                        }
                    }
                    collecting_base_url = false;
                    base_url_text.clear();
                } else if name.as_slice() == b"SegmentTemplate" {
                    if let Some((target, template)) = editing_template.take() {
                        assign_dash_template(
                            target,
                            template,
                            current_adaptation.as_mut(),
                            current_representation.as_mut(),
                        );
                    }
                } else if name.as_slice() == b"Representation" {
                    if let (Some(adaptation), Some(mut representation)) =
                        (current_adaptation.as_mut(), current_representation.take())
                    {
                        if representation.id.trim().is_empty() {
                            representation.id = format!("rep-{}", adaptation.representations.len() + 1);
                        }
                        adaptation.representations.push(representation);
                    }
                } else if name.as_slice() == b"AdaptationSet" {
                    if let Some(adaptation) = current_adaptation.take() {
                        adaptations.push(adaptation);
                    }
                }
                let _ = tag_stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
    }

    dash_adaptations_to_manifest(&manifest_base, &adaptations, media_presentation_duration)
}

fn attr_value(
    reader: &Reader<&[u8]>,
    event: &BytesStart<'_>,
    key: &[u8],
) -> Result<Option<String>, AppError> {
    for attr in event.attributes().with_checks(false) {
        let attr =
            attr.map_err(|error| AppError::M3u8Parse(format!("DASH MPD 属性解析失败: {}", error)))?;
        if attr.key.as_ref() == key {
            return attr
                .decode_and_unescape_value(reader.decoder())
                .map(|value| Some(value.into_owned()))
                .map_err(|error| AppError::M3u8Parse(format!("DASH MPD 属性解析失败: {}", error)));
        }
    }
    Ok(None)
}

fn parse_dash_segment_template_attrs(
    reader: &Reader<&[u8]>,
    event: &BytesStart<'_>,
) -> Result<DashSegmentTemplate, AppError> {
    Ok(DashSegmentTemplate {
        timescale: attr_value(reader, event, b"timescale")?
            .and_then(|value| value.parse().ok())
            .unwrap_or(1),
        initialization: attr_value(reader, event, b"initialization")?,
        media: attr_value(reader, event, b"media")?,
        start_number: attr_value(reader, event, b"startNumber")?
            .and_then(|value| value.parse().ok())
            .unwrap_or(1),
        duration: attr_value(reader, event, b"duration")?.and_then(|value| value.parse().ok()),
        timeline: Vec::new(),
    })
}

fn parse_dash_timeline_item(
    reader: &Reader<&[u8]>,
    event: &BytesStart<'_>,
) -> Result<DashTimelineItem, AppError> {
    let duration = attr_value(reader, event, b"d")?
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| AppError::M3u8Parse("DASH SegmentTimeline S 缺少 d".to_string()))?;
    Ok(DashTimelineItem {
        start_time: attr_value(reader, event, b"t")?.and_then(|value| value.parse().ok()),
        duration,
        repeat: attr_value(reader, event, b"r")?
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
    })
}

fn assign_dash_template(
    target: DashTemplateTarget,
    template: DashSegmentTemplate,
    adaptation: Option<&mut DashAdaptationBuild>,
    representation: Option<&mut DashRepresentationBuild>,
) {
    match target {
        DashTemplateTarget::Adaptation => {
            if let Some(adaptation) = adaptation {
                adaptation.segment_template = Some(template);
            }
        }
        DashTemplateTarget::Representation => {
            if let Some(representation) = representation {
                representation.segment_template = Some(template);
            }
        }
    }
}

fn dash_adaptations_to_manifest(
    manifest_base: &Url,
    adaptations: &[DashAdaptationBuild],
    media_presentation_duration: Option<f64>,
) -> Result<DashManifest, AppError> {
    let has_audio = adaptations.iter().any(|adaptation| dash_adaptation_kind(adaptation) == Some(HlsTrackType::Audio));
    let audio_group_id = has_audio.then_some("dash-audio".to_string());
    let mut video_tracks = Vec::new();
    let mut audio_tracks = Vec::new();

    for adaptation in adaptations {
        let Some(track_type) = dash_adaptation_kind(adaptation) else {
            continue;
        };
        let base = adaptation
            .base_url
            .as_deref()
            .map(|value| manifest_base.join(value))
            .transpose()?
            .unwrap_or_else(|| manifest_base.clone());

        for representation in &adaptation.representations {
            let template = representation
                .segment_template
                .as_ref()
                .or(adaptation.segment_template.as_ref())
                .ok_or_else(|| AppError::M3u8Parse("DASH Representation 缺少 SegmentTemplate".to_string()))?;
            let rep_base = representation
                .base_url
                .as_deref()
                .map(|value| base.join(value))
                .transpose()?
                .unwrap_or_else(|| base.clone());
            let track = build_dash_track_from_template(
                &rep_base,
                adaptation,
                representation,
                template,
                track_type,
                media_presentation_duration,
                match track_type {
                    HlsTrackType::Video => video_tracks.len(),
                    HlsTrackType::Audio => audio_tracks.len(),
                    HlsTrackType::Subtitle => 0,
                },
                audio_group_id.clone(),
            )?;
            match track_type {
                HlsTrackType::Video => video_tracks.push(track),
                HlsTrackType::Audio => audio_tracks.push(track),
                HlsTrackType::Subtitle => {}
            }
        }
    }

    if video_tracks.is_empty() {
        return Err(AppError::M3u8Parse("DASH MPD 未找到可下载的视频轨道".to_string()));
    }

    let default_selection = HlsTrackSelection {
        video_id: video_tracks.first().map(|track| track.option.id.clone()),
        audio_id: audio_tracks.first().map(|track| track.option.id.clone()),
        subtitle_id: None,
    };

    Ok(DashManifest {
        video_tracks,
        audio_tracks,
        default_selection,
    })
}

fn dash_adaptation_kind(adaptation: &DashAdaptationBuild) -> Option<HlsTrackType> {
    if adaptation
        .content_type
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case("video"))
        || adaptation
            .mime_type
            .as_deref()
            .is_some_and(|value| value.starts_with("video/"))
    {
        Some(HlsTrackType::Video)
    } else if adaptation
        .content_type
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case("audio"))
        || adaptation
            .mime_type
            .as_deref()
            .is_some_and(|value| value.starts_with("audio/"))
    {
        Some(HlsTrackType::Audio)
    } else {
        None
    }
}

fn build_dash_track_from_template(
    base_url: &Url,
    adaptation: &DashAdaptationBuild,
    representation: &DashRepresentationBuild,
    template: &DashSegmentTemplate,
    track_type: HlsTrackType,
    media_presentation_duration: Option<f64>,
    index: usize,
    audio_group_id: Option<String>,
) -> Result<DashTrack, AppError> {
    let initialization = template
        .initialization
        .as_deref()
        .ok_or_else(|| AppError::M3u8Parse("DASH SegmentTemplate 缺少 initialization".to_string()))?;
    let media = template
        .media
        .as_deref()
        .ok_or_else(|| AppError::M3u8Parse("DASH SegmentTemplate 缺少 media".to_string()))?;
    let init_uri = resolve_url(
        base_url,
        &apply_dash_template(initialization, representation, template.start_number, None)?,
    );
    let segments = expand_dash_segments(media, representation, template, media_presentation_duration)?
        .into_iter()
        .map(|(number, duration)| {
            Ok(DashSegment {
                uri: resolve_url(
                    base_url,
                    &apply_dash_template(media, representation, number, None)?,
                ),
                duration,
                sequence_number: number,
                byte_range: None,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    if segments.is_empty() {
        return Err(AppError::M3u8Parse("DASH 轨道没有可展开的分片".to_string()));
    }

    let resolution = match (representation.width, representation.height) {
        (Some(width), Some(height)) => Some(format!("{}x{}", width, height)),
        _ => None,
    };
    let label = if let Some(resolution) = &resolution {
        match representation.bandwidth {
            Some(bandwidth) => format!("{} · {} kbps", resolution, bandwidth / 1000),
            None => resolution.clone(),
        }
    } else if let Some(language) = &adaptation.lang {
        language.clone()
    } else {
        format!("轨道 {}", index + 1)
    };

    Ok(DashTrack {
        option: HlsTrackOption {
            id: representation.id.clone(),
            track_type,
            label: label.clone(),
            name: Some(label),
            language: adaptation.lang.clone(),
            group_id: (track_type == HlsTrackType::Audio).then_some("dash-audio".to_string()),
            audio_group_id: (track_type == HlsTrackType::Video)
                .then(|| audio_group_id)
                .flatten(),
            subtitle_group_id: None,
            bandwidth: representation.bandwidth,
            resolution,
            codecs: representation.codecs.clone(),
            is_default: index == 0,
            is_autoselect: index == 0,
            is_forced: false,
        },
        init: Some(DashResource {
            uri: init_uri,
            byte_range: None,
        }),
        segments,
    })
}

fn expand_dash_segments(
    media_template: &str,
    representation: &DashRepresentationBuild,
    template: &DashSegmentTemplate,
    media_presentation_duration: Option<f64>,
) -> Result<Vec<(u64, f32)>, AppError> {
    let timescale = template.timescale.max(1);
    let mut number = template.start_number;
    let mut segments = Vec::new();
    if !template.timeline.is_empty() {
        for item in &template.timeline {
            if item.repeat < 0 {
                return Err(AppError::M3u8Parse(
                    "暂不支持 DASH SegmentTimeline 负数 repeat".to_string(),
                ));
            }
            let _ = item.start_time;
            for _ in 0..=item.repeat {
                segments.push((number, item.duration as f32 / timescale as f32));
                number = number.saturating_add(1);
            }
        }
        return Ok(segments);
    }

    let Some(duration) = template.duration else {
        return Err(AppError::M3u8Parse(
            "DASH SegmentTemplate 缺少 SegmentTimeline 或 duration".to_string(),
        ));
    };
    let Some(total_duration) = media_presentation_duration else {
        return Err(AppError::M3u8Parse(
            "DASH MPD 缺少 mediaPresentationDuration，无法展开 duration 模板".to_string(),
        ));
    };
    let segment_duration = duration as f64 / timescale as f64;
    let count = (total_duration / segment_duration).ceil().max(1.0) as u64;
    for _ in 0..count {
        let _ = apply_dash_template(media_template, representation, number, None)?;
        segments.push((number, segment_duration as f32));
        number = number.saturating_add(1);
    }
    Ok(segments)
}

fn apply_dash_template(
    template: &str,
    representation: &DashRepresentationBuild,
    number: u64,
    time: Option<u64>,
) -> Result<String, AppError> {
    let mut result = String::new();
    let mut chars = template.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '$' {
            result.push(ch);
            continue;
        }
        if matches!(chars.peek(), Some('$')) {
            chars.next();
            result.push('$');
            continue;
        }
        let mut token = String::new();
        while let Some(next) = chars.next() {
            if next == '$' {
                break;
            }
            token.push(next);
        }
        let (name, format_width) = parse_dash_template_token(&token);
        let replacement = match name {
            "RepresentationID" => representation.id.clone(),
            "Bandwidth" => representation.bandwidth.unwrap_or(0).to_string(),
            "Number" => format_dash_template_number(number, format_width),
            "Time" => format_dash_template_number(time.unwrap_or(0), format_width),
            _ => {
                return Err(AppError::M3u8Parse(format!(
                    "暂不支持 DASH 模板变量 ${}$",
                    token
                )))
            }
        };
        result.push_str(&replacement);
    }
    Ok(result)
}

fn parse_dash_template_token(token: &str) -> (&str, Option<usize>) {
    if let Some((name, format)) = token.split_once('%') {
        let width = format
            .trim_end_matches('d')
            .trim_start_matches('0')
            .parse::<usize>()
            .ok();
        (name, width)
    } else {
        (token, None)
    }
}

fn format_dash_template_number(value: u64, width: Option<usize>) -> String {
    match width {
        Some(width) => format!("{:0width$}", value, width = width),
        None => value.to_string(),
    }
}

fn parse_iso8601_duration_seconds(value: &str) -> Option<f64> {
    let mut raw = value.strip_prefix("PT")?;
    let mut total = 0.0;
    while !raw.is_empty() {
        let number_len = raw
            .find(|ch: char| !ch.is_ascii_digit() && ch != '.')
            .unwrap_or(raw.len());
        if number_len == 0 || number_len >= raw.len() {
            return None;
        }
        let number: f64 = raw[..number_len].parse().ok()?;
        let unit = raw.as_bytes()[number_len] as char;
        total += match unit {
            'H' => number * 3600.0,
            'M' => number * 60.0,
            'S' => number,
            _ => return None,
        };
        raw = &raw[number_len + 1..];
    }
    Some(total)
}

fn parse_dash_byte_range(value: &str) -> Result<ByteRangeSpec, AppError> {
    let (start, end) = value
        .split_once('-')
        .ok_or_else(|| AppError::M3u8Parse(format!("无效 DASH byte_range: {}", value)))?;
    let start = start
        .trim()
        .parse::<u64>()
        .map_err(|_| AppError::M3u8Parse(format!("无效 DASH byte_range: {}", value)))?;
    if end.trim().is_empty() {
        return Ok(ByteRangeSpec {
            length: 0,
            offset: Some(start),
        });
    }
    let end = end
        .trim()
        .parse::<u64>()
        .map_err(|_| AppError::M3u8Parse(format!("无效 DASH byte_range: {}", value)))?;
    if end < start {
        return Err(AppError::M3u8Parse(format!("无效 DASH byte_range: {}", value)));
    }
    Ok(ByteRangeSpec {
        length: end - start + 1,
        offset: Some(start),
    })
}

async fn fetch_hls_playlist(
    client: &reqwest::Client,
    url: &str,
    headers: &RequestHeaders,
) -> Result<FetchedPlaylist, AppError> {
    let base_url = Url::parse(url)?;
    let response = build_request_with_headers(client, url, headers)
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

    let playlist = m3u8_rs::parse_playlist_res(&bytes).map_err(|_| {
        AppError::InvalidInput("链接内容不是有效的 M3U8 播放列表，请检查地址是否正确".to_string())
    })?;

    Ok(FetchedPlaylist { base_url, playlist })
}

async fn fetch_media_playlist_following_variants(
    client: &reqwest::Client,
    url: &str,
    headers: &RequestHeaders,
) -> Result<FetchedResolvedMediaPlaylist, AppError> {
    let fetched = fetch_hls_playlist(client, url, headers).await?;

    match fetched.playlist {
        m3u8_rs::Playlist::MediaPlaylist(playlist) => Ok(FetchedResolvedMediaPlaylist {
            base_url: fetched.base_url,
            playlist,
        }),
        m3u8_rs::Playlist::MasterPlaylist(master) => {
            let variant = master
                .variants
                .iter()
                .filter(|variant| !variant.is_i_frame)
                .max_by_key(|variant| variant.bandwidth)
                .ok_or_else(|| AppError::M3u8Parse("No variants found".into()))?;
            let variant_url = resolve_url(&fetched.base_url, &variant.uri);
            Box::pin(fetch_media_playlist_following_variants(
                client,
                &variant_url,
                headers,
            ))
            .await
        }
    }
}

#[derive(Debug, Clone)]
struct FetchedResolvedMediaPlaylist {
    base_url: Url,
    playlist: m3u8_rs::MediaPlaylist,
}

fn build_master_track_catalog(
    base_url: &Url,
    master: &m3u8_rs::MasterPlaylist,
) -> Result<MasterTrackCatalog, AppError> {
    let mut videos = master
        .variants
        .iter()
        .filter(|variant| !variant.is_i_frame)
        .filter(|variant| !variant.uri.trim().is_empty())
        .map(|variant| MasterVideoTrack {
            uri: resolve_url(base_url, &variant.uri),
            option: HlsTrackOption {
                id: build_video_track_id(
                    &resolve_url(base_url, &variant.uri),
                    variant.bandwidth,
                    variant.resolution.as_ref(),
                    variant.codecs.as_deref(),
                ),
                track_type: HlsTrackType::Video,
                label: build_video_track_label(variant),
                name: None,
                language: None,
                group_id: None,
                audio_group_id: variant.audio.clone(),
                subtitle_group_id: variant.subtitles.clone(),
                bandwidth: Some(variant.bandwidth),
                resolution: variant.resolution.as_ref().map(ToString::to_string),
                codecs: variant.codecs.clone(),
                is_default: false,
                is_autoselect: false,
                is_forced: false,
            },
        })
        .collect::<Vec<_>>();
    if videos.is_empty() {
        return Err(AppError::M3u8Parse("No variants found".into()));
    }
    videos.sort_by(|a, b| {
        b.option
            .bandwidth
            .cmp(&a.option.bandwidth)
            .then_with(|| a.option.label.cmp(&b.option.label))
    });

    let audios = master
        .alternatives
        .iter()
        .filter_map(|media| build_alternative_track_option(base_url, media, HlsTrackType::Audio))
        .collect::<Vec<_>>();
    let subtitles = master
        .alternatives
        .iter()
        .filter_map(|media| build_alternative_track_option(base_url, media, HlsTrackType::Subtitle))
        .collect::<Vec<_>>();

    let default_video = videos
        .first()
        .cloned()
        .ok_or_else(|| AppError::M3u8Parse("No variants found".into()))?;
    let default_audio = default_audio_track_id(&tracks_for_group(
        &audios,
        default_video.option.audio_group_id.as_deref(),
    ));
    let inspection = InspectHlsTracksResult {
        kind: HlsPlaylistKind::Master,
        requires_selection: videos.len() > 1 || audios.len() > 1 || !subtitles.is_empty(),
        video_tracks: videos.iter().map(|track| track.option.clone()).collect(),
        audio_tracks: audios.iter().map(|track| track.option.clone()).collect(),
        subtitle_tracks: subtitles.iter().map(|track| track.option.clone()).collect(),
        default_selection: HlsTrackSelection {
            video_id: Some(default_video.option.id.clone()),
            audio_id: default_audio,
            subtitle_id: None,
        },
    };

    Ok(MasterTrackCatalog {
        inspection,
        videos,
        audios,
        subtitles,
    })
}

fn build_alternative_track_option(
    base_url: &Url,
    media: &m3u8_rs::AlternativeMedia,
    requested_type: HlsTrackType,
) -> Option<MasterAlternativeTrack> {
    let matches_type = match requested_type {
        HlsTrackType::Audio => media.media_type == m3u8_rs::AlternativeMediaType::Audio,
        HlsTrackType::Subtitle => media.media_type == m3u8_rs::AlternativeMediaType::Subtitles,
        HlsTrackType::Video => false,
    };
    if !matches_type {
        return None;
    }

    let uri = media.uri.as_ref()?;
    let resolved_uri = resolve_url(base_url, uri);
    let id = build_alternative_track_id(
        requested_type,
        &media.group_id,
        &media.name,
        media.language.as_deref(),
        &resolved_uri,
    );

    Some(MasterAlternativeTrack {
        uri: resolved_uri,
        option: HlsTrackOption {
            id,
            track_type: requested_type,
            label: build_alternative_track_label(media),
            name: Some(media.name.clone()),
            language: media.language.clone(),
            group_id: Some(media.group_id.clone()),
            audio_group_id: None,
            subtitle_group_id: None,
            bandwidth: None,
            resolution: None,
            codecs: None,
            is_default: media.default,
            is_autoselect: media.autoselect,
            is_forced: media.forced,
        },
    })
}

fn build_video_track_id(
    uri: &str,
    bandwidth: u64,
    resolution: Option<&m3u8_rs::Resolution>,
    codecs: Option<&str>,
) -> String {
    let resolution = resolution.map(ToString::to_string).unwrap_or_default();
    let codecs = codecs.unwrap_or_default();
    format!(
        "video:{}|{}|{}|{}",
        comparable_uri_path(uri),
        bandwidth,
        resolution,
        codecs
    )
}

fn build_alternative_track_id(
    track_type: HlsTrackType,
    group_id: &str,
    name: &str,
    language: Option<&str>,
    uri: &str,
) -> String {
    let track_type = match track_type {
        HlsTrackType::Audio => "audio",
        HlsTrackType::Subtitle => "subtitle",
        HlsTrackType::Video => "video",
    };
    format!(
        "{}:{}|{}|{}|{}",
        track_type,
        group_id,
        name,
        language.unwrap_or_default(),
        comparable_uri_path(uri)
    )
}

fn build_video_track_label(variant: &m3u8_rs::VariantStream) -> String {
    let mut parts = Vec::new();
    if let Some(resolution) = variant.resolution.as_ref() {
        parts.push(resolution.to_string());
    }
    parts.push(format!("{:.0} kbps", variant.bandwidth as f64 / 1000.0));
    if let Some(codecs) = variant.codecs.as_ref() {
        parts.push(codecs.clone());
    }
    parts.join(" | ")
}

fn build_alternative_track_label(media: &m3u8_rs::AlternativeMedia) -> String {
    let mut parts = vec![media.name.clone()];
    if let Some(language) = media.language.as_ref() {
        parts.push(language.clone());
    }

    let mut flags = Vec::new();
    if media.default {
        flags.push("默认");
    }
    if media.autoselect {
        flags.push("自动");
    }
    if media.forced {
        flags.push("强制");
    }
    if !flags.is_empty() {
        parts.push(flags.join("/"));
    }

    parts.join(" | ")
}

fn default_audio_track_id(tracks: &[MasterAlternativeTrack]) -> Option<String> {
    tracks
        .iter()
        .find(|track| track.option.is_default)
        .or_else(|| tracks.iter().find(|track| track.option.is_autoselect))
        .or_else(|| tracks.first())
        .map(|track| track.option.id.clone())
}

fn tracks_for_group(
    tracks: &[MasterAlternativeTrack],
    group_id: Option<&str>,
) -> Vec<MasterAlternativeTrack> {
    let Some(group_id) = group_id else {
        return Vec::new();
    };

    tracks
        .iter()
        .filter(|track| track.option.group_id.as_deref() == Some(group_id))
        .cloned()
        .collect()
}

fn resolve_selected_alternative_track(
    available_tracks: &[MasterAlternativeTrack],
    selected_id: Option<&str>,
    default_id: Option<&str>,
    track_name: &str,
) -> Result<Option<MasterAlternativeTrack>, AppError> {
    if available_tracks.is_empty() {
        if selected_id.is_some() {
            return Err(AppError::InvalidInput(format!(
                "所选{}轨道已不存在，请重新解析后再下载",
                track_name
            )));
        }
        return Ok(None);
    }

    let target_id = selected_id.or(default_id).or_else(|| {
        available_tracks
            .first()
            .map(|track| track.option.id.as_str())
    });

    let Some(target_id) = target_id else {
        return Ok(None);
    };

    available_tracks
        .iter()
        .find(|track| track.option.id == target_id)
        .cloned()
        .map(Some)
        .ok_or_else(|| {
            AppError::InvalidInput(format!(
                "所选{}轨道已不存在，请重新解析后再下载",
                track_name
            ))
        })
}

fn resolve_selected_optional_track(
    available_tracks: &[MasterAlternativeTrack],
    selected_id: Option<&str>,
    track_name: &str,
) -> Result<Option<MasterAlternativeTrack>, AppError> {
    let Some(selected_id) = selected_id else {
        return Ok(None);
    };

    available_tracks
        .iter()
        .find(|track| track.option.id == selected_id)
        .cloned()
        .map(Some)
        .ok_or_else(|| {
            AppError::InvalidInput(format!(
                "所选{}轨道已不存在，请重新解析后再下载",
                track_name
            ))
        })
}

#[derive(Debug, Clone)]
struct ParsedMediaPlaylistPlan {
    media_kind: HlsMediaKind,
    init_segments: Vec<PreparedHlsInitSegment>,
    segments: Vec<SegmentInfo>,
}

fn parse_media_playlist_plan(
    base_url: &Url,
    playlist: &m3u8_rs::MediaPlaylist,
) -> Result<ParsedMediaPlaylistPlan, AppError> {
    let media_sequence = playlist.media_sequence;
    let mut current_key: Option<ParsedEncryptionState> = None;
    let mut current_init_segment_index: Option<usize> = None;
    let mut init_cache = HashMap::<BundleMapCacheKey, usize>::new();
    let mut init_segments = Vec::new();
    let mut previous_map_byte_range: Option<(String, u64)> = None;
    let mut previous_media_byte_range: Option<(String, u64)> = None;
    let mut segments = Vec::with_capacity(playlist.segments.len());

    for (index, segment) in playlist.segments.iter().enumerate() {
        update_encryption_state(&mut current_key, base_url, segment.key.as_ref())?;
        let encryption = current_key.as_ref().map(to_encryption_info);
        if let Some(map) = segment.map.as_ref() {
            let resolved_map_uri = resolve_url(base_url, &map.uri);
            let byte_range = resolve_explicit_byte_range(
                &resolved_map_uri,
                map.byte_range.as_ref(),
                &mut previous_map_byte_range,
            );
            let cache_key = BundleMapCacheKey {
                uri: resolved_map_uri.clone(),
                byte_range: byte_range.clone(),
            };
            let init_index = if let Some(existing) = init_cache.get(&cache_key) {
                *existing
            } else {
                let init_index = init_segments.len();
                init_segments.push(PreparedHlsInitSegment {
                    info: HlsInitSegmentInfo {
                        index: init_index,
                        uri: resolved_map_uri,
                        byte_range,
                    },
                    encryption: encryption.clone(),
                });
                init_cache.insert(cache_key, init_index);
                init_index
            };
            current_init_segment_index = Some(init_index);
        }
        let resolved_uri = resolve_url(base_url, &segment.uri);

        segments.push(SegmentInfo {
            index,
            uri: resolved_uri.clone(),
            duration: segment.duration,
            sequence_number: media_sequence + index as u64,
            byte_range: resolve_explicit_byte_range(
                &resolved_uri,
                segment.byte_range.as_ref(),
                &mut previous_media_byte_range,
            ),
            init_segment_index: current_init_segment_index,
            encryption,
        });
    }

    Ok(ParsedMediaPlaylistPlan {
        media_kind: if init_segments.is_empty() {
            HlsMediaKind::MpegTs
        } else {
            HlsMediaKind::Fmp4
        },
        init_segments,
        segments,
    })
}

#[cfg(test)]
fn parse_media_playlist_segments(
    base_url: &Url,
    playlist: &m3u8_rs::MediaPlaylist,
) -> Result<Vec<SegmentInfo>, AppError> {
    Ok(parse_media_playlist_plan(base_url, playlist)?.segments)
}

#[derive(Debug, Clone)]
struct BundleTrackPlanBuild {
    playlist_files: Vec<BundlePlaylistFile>,
    entries: Vec<BundleDownloadEntry>,
}

impl BundleTrackPlanBuild {
    fn extend(&mut self, other: BundleTrackPlanBuild) {
        let next_index = self.entries.len();
        self.playlist_files.extend(other.playlist_files);
        self.entries.extend(
            other
                .entries
                .into_iter()
                .enumerate()
                .map(|(offset, mut entry)| {
                    entry.index = next_index + offset;
                    entry
                }),
        );
    }
}

fn build_bundle_track_plan(
    fetched: &FetchedResolvedMediaPlaylist,
    subdir: &str,
) -> Result<BundleTrackPlanBuild, AppError> {
    let mut current_key: Option<ParsedEncryptionState> = None;
    let mut current_map: Option<BundleMapState> = None;
    let mut map_cache = HashMap::<BundleMapCacheKey, BundleMapState>::new();
    let mut last_emitted_map: Option<String> = None;
    let mut previous_map_byte_range: Option<(String, u64)> = None;
    let mut previous_media_byte_range: Option<(String, u64)> = None;
    let mut map_counter = 0usize;
    let mut entries = Vec::new();
    let mut local_segments = Vec::with_capacity(fetched.playlist.segments.len());

    for (segment_index, segment) in fetched.playlist.segments.iter().enumerate() {
        update_encryption_state(&mut current_key, &fetched.base_url, segment.key.as_ref())?;
        let encryption = current_key.as_ref().map(to_encryption_info);
        let sequence_number = fetched.playlist.media_sequence + segment_index as u64;

        if let Some(map) = segment.map.as_ref() {
            let resolved_map_uri = resolve_url(&fetched.base_url, &map.uri);
            let byte_range = resolve_explicit_byte_range(
                &resolved_map_uri,
                map.byte_range.as_ref(),
                &mut previous_map_byte_range,
            );
            let cache_key = BundleMapCacheKey {
                uri: resolved_map_uri.clone(),
                byte_range: byte_range.clone(),
            };
            let map_state = if let Some(existing) = map_cache.get(&cache_key) {
                existing.clone()
            } else {
                map_counter += 1;
                let local_file_name = format!(
                    "init_{:06}.{}",
                    map_counter,
                    infer_file_extension(&resolved_map_uri, "bin")
                );
                let created = BundleMapState {
                    local_file_name: local_file_name.clone(),
                };
                entries.push(BundleDownloadEntry {
                    index: entries.len(),
                    uri: resolved_map_uri,
                    duration: 0.0,
                    sequence_number,
                    byte_range,
                    encryption: encryption.clone(),
                    relative_path: PathBuf::from(subdir).join(local_file_name),
                });
                map_cache.insert(cache_key, created.clone());
                created
            };
            current_map = Some(map_state);
        }

        let local_segment_name = format!(
            "seg_{:06}.{}",
            segment_index + 1,
            infer_file_extension(&segment.uri, "bin")
        );
        let resolved_segment_uri = resolve_url(&fetched.base_url, &segment.uri);
        entries.push(BundleDownloadEntry {
            index: entries.len(),
            uri: resolved_segment_uri.clone(),
            duration: segment.duration,
            sequence_number,
            byte_range: resolve_explicit_byte_range(
                &resolved_segment_uri,
                segment.byte_range.as_ref(),
                &mut previous_media_byte_range,
            ),
            encryption,
            relative_path: PathBuf::from(subdir).join(&local_segment_name),
        });

        let map_uri = current_map.as_ref().map(|map| map.local_file_name.clone());
        let mut local_segment = m3u8_rs::MediaSegment {
            uri: local_segment_name,
            duration: segment.duration,
            title: segment.title.clone(),
            map: None,
            ..Default::default()
        };
        if let Some(map_uri) = map_uri {
            if last_emitted_map.as_deref() != Some(map_uri.as_str()) {
                local_segment.map = Some(m3u8_rs::Map {
                    uri: map_uri.clone(),
                    ..Default::default()
                });
                last_emitted_map = Some(map_uri);
            }
        } else {
            last_emitted_map = None;
        }
        local_segments.push(local_segment);
    }

    let target_duration = local_segments.iter().fold(1u64, |max_duration, segment| {
        max_duration.max(segment.duration.ceil().max(1.0) as u64)
    });
    let local_playlist = m3u8_rs::MediaPlaylist {
        version: Some(6),
        target_duration,
        media_sequence: 0,
        segments: local_segments,
        discontinuity_sequence: 0,
        end_list: true,
        playlist_type: Some(m3u8_rs::MediaPlaylistType::Vod),
        i_frames_only: false,
        start: None,
        independent_segments: fetched.playlist.independent_segments,
        unknown_tags: Vec::new(),
    };

    Ok(BundleTrackPlanBuild {
        playlist_files: vec![BundlePlaylistFile {
            relative_path: PathBuf::from(subdir).join("index.m3u8"),
            content: media_playlist_to_string(&local_playlist)?,
        }],
        entries,
    })
}

fn media_playlist_to_string(playlist: &m3u8_rs::MediaPlaylist) -> Result<String, AppError> {
    let mut bytes = Vec::new();
    playlist
        .write_to(&mut bytes)
        .map_err(|error| AppError::Internal(error.to_string()))?;
    String::from_utf8(bytes).map_err(|error| AppError::Internal(error.to_string()))
}

fn update_encryption_state(
    current_key: &mut Option<ParsedEncryptionState>,
    base_url: &Url,
    key: Option<&m3u8_rs::Key>,
) -> Result<(), AppError> {
    let Some(key) = key else {
        return Ok(());
    };

    if key.method == m3u8_rs::KeyMethod::None {
        *current_key = None;
    } else if is_aes_cbc_method(&key.method) {
        let method_name = key.method.to_string();
        let key_uri = key
            .uri
            .as_ref()
            .ok_or_else(|| AppError::M3u8Parse(format!("{} key missing URI", method_name)))?;
        *current_key = Some(ParsedEncryptionState {
            method: method_name,
            key_uri: resolve_url(base_url, key_uri),
            iv: key.iv.clone(),
        });
    } else {
        return Err(AppError::M3u8Parse(format!(
            "Unsupported encryption method: {:?}",
            key.method
        )));
    }

    Ok(())
}

fn to_encryption_info(state: &ParsedEncryptionState) -> EncryptionInfo {
    EncryptionInfo {
        method: state.method.clone(),
        key_uri: state.key_uri.clone(),
        iv: state.iv.clone(),
        key_bytes: Vec::new(),
    }
}

fn is_aes_cbc_method(method: &m3u8_rs::KeyMethod) -> bool {
    matches!(method, m3u8_rs::KeyMethod::AES128)
        || matches!(method, m3u8_rs::KeyMethod::Other(s) if s == "AES-192" || s == "AES-256")
}

fn resolve_explicit_byte_range(
    uri: &str,
    byte_range: Option<&m3u8_rs::ByteRange>,
    previous_state: &mut Option<(String, u64)>,
) -> Option<ByteRangeSpec> {
    let Some(byte_range) = byte_range else {
        *previous_state = None;
        return None;
    };

    let offset = byte_range.offset.unwrap_or_else(|| {
        previous_state
            .as_ref()
            .filter(|(previous_uri, _)| previous_uri == uri)
            .map(|(_, next_offset)| *next_offset)
            .unwrap_or(0)
    });

    *previous_state = Some((uri.to_string(), offset.saturating_add(byte_range.length)));

    Some(ByteRangeSpec {
        length: byte_range.length,
        offset: Some(offset),
    })
}

fn comparable_uri_path(uri: &str) -> String {
    if let Ok(parsed) = url::Url::parse(uri) {
        parsed.path().to_string()
    } else {
        uri.split('?').next().unwrap_or(uri).to_string()
    }
}

fn infer_file_extension(uri: &str, fallback: &str) -> String {
    let path = if let Ok(parsed) = url::Url::parse(uri) {
        parsed.path().to_string()
    } else {
        uri.split(['?', '#']).next().unwrap_or(uri).to_string()
    };

    Path::new(&path)
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback)
        .to_string()
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

fn validate_fmp4_init_encryption(init_segments: &[PreparedHlsInitSegment]) -> Result<(), AppError> {
    for init in init_segments {
        if init
            .encryption
            .as_ref()
            .is_some_and(|encryption| encryption.iv.is_none())
        {
            return Err(AppError::Decryption(
                "加密的 fMP4 EXT-X-MAP 必须提供显式 IV".to_string(),
            ));
        }
    }

    Ok(())
}

pub async fn fetch_prepared_init_encryption_keys(
    client: &reqwest::Client,
    init_segments: &mut [PreparedHlsInitSegment],
    headers: &RequestHeaders,
) -> Result<(), AppError> {
    let mut key_cache: HashMap<String, Vec<u8>> = HashMap::new();

    for init in init_segments.iter_mut() {
        if let Some(ref mut enc) = init.encryption {
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

pub async fn fetch_bundle_encryption_keys(
    client: &reqwest::Client,
    entries: &mut [BundleDownloadEntry],
    headers: &RequestHeaders,
) -> Result<(), AppError> {
    let mut key_cache: HashMap<String, Vec<u8>> = HashMap::new();

    for entry in entries.iter_mut() {
        if let Some(ref mut enc) = entry.encryption {
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

pub fn hls_init_segment_file_path(temp_dir: &Path, init_index: usize) -> PathBuf {
    temp_dir.join(format!("init_{:06}.mp4", init_index))
}

pub fn fmp4_segment_file_path(temp_dir: &Path, segment_index: usize) -> PathBuf {
    temp_dir.join(format!("seg_{:06}.m4s", segment_index))
}

fn segment_file_path_for_kind(
    temp_dir: &Path,
    media_kind: HlsMediaKind,
    segment_index: usize,
) -> PathBuf {
    match media_kind {
        HlsMediaKind::MpegTs => segment_file_path(temp_dir, segment_index),
        HlsMediaKind::Fmp4 => fmp4_segment_file_path(temp_dir, segment_index),
    }
}

fn partial_segment_file_path_for_kind(
    temp_dir: &Path,
    media_kind: HlsMediaKind,
    segment_index: usize,
) -> PathBuf {
    part_path_for_downloaded_file(&segment_file_path_for_kind(
        temp_dir,
        media_kind,
        segment_index,
    ))
}

fn part_path_for_downloaded_file(path: &Path) -> PathBuf {
    let Some(file_name) = path.file_name() else {
        return path.with_extension("part");
    };

    let mut partial_name = OsString::from(file_name);
    partial_name.push(".part");
    path.with_file_name(partial_name)
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

fn mp4_partial_path_for_output_path(mp4_path: &Path) -> PathBuf {
    let Some(file_name) = mp4_path.file_name() else {
        return mp4_path.with_extension("partial");
    };

    let mut partial_name = OsString::from(file_name);
    partial_name.push(".partial");
    mp4_path.with_file_name(partial_name)
}

fn normalize_mp4_output_filename(filename: &str) -> String {
    let (_, extension) = split_filename_and_extension(filename);
    if extension.is_some() {
        filename.to_string()
    } else {
        ensure_extension(filename, "mp4")
    }
}

fn find_existing_mp4_partial_path(output_dir: &Path, filename: &str) -> Option<PathBuf> {
    let (base_name, extension) = split_filename_and_extension(filename);
    let initial = output_dir.join(build_filename(&base_name, extension.as_deref()));
    let mut candidate = initial;
    let mut index = 1usize;

    loop {
        let partial = mp4_partial_path_for_output_path(&candidate);
        if partial.exists() {
            return Some(partial);
        }
        if !candidate.exists() {
            return None;
        }

        candidate = output_dir.join(build_indexed_filename(
            &base_name,
            index,
            extension.as_deref(),
        ));
        index += 1;
    }
}

fn mp4_output_path_from_partial_path(partial_path: &Path) -> PathBuf {
    let Some(file_name) = partial_path.file_name().and_then(|value| value.to_str()) else {
        return partial_path.to_path_buf();
    };
    let Some(output_name) = file_name.strip_suffix(".partial") else {
        return partial_path.to_path_buf();
    };
    partial_path
        .parent()
        .map(|parent| parent.join(output_name))
        .unwrap_or_else(|| PathBuf::from(output_name))
}

fn resolve_available_mp4_output_paths(output_dir: &Path, filename: &str) -> (PathBuf, PathBuf) {
    let mp4_filename = normalize_mp4_output_filename(filename);
    let (base_name, extension) = split_filename_and_extension(&mp4_filename);
    let mut candidate = output_dir.join(build_filename(&base_name, extension.as_deref()));
    let mut index = 1usize;

    loop {
        let partial_path = mp4_partial_path_for_output_path(&candidate);
        if !candidate.exists() && !partial_path.exists() {
            return (candidate, partial_path);
        }

        candidate = output_dir.join(build_indexed_filename(
            &base_name,
            index,
            extension.as_deref(),
        ));
        index += 1;
    }
}

fn resolve_mp4_output_paths(
    output_dir: &Path,
    filename: &str,
    prefer_existing_partial: bool,
) -> (PathBuf, PathBuf) {
    let mp4_filename = normalize_mp4_output_filename(filename);

    if prefer_existing_partial {
        if let Some(partial_path) = find_existing_mp4_partial_path(output_dir, &mp4_filename) {
            return (
                mp4_output_path_from_partial_path(&partial_path),
                partial_path,
            );
        }
    }

    resolve_available_mp4_output_paths(output_dir, &mp4_filename)
}

fn resolve_existing_mp4_partial_paths(output_dir: &Path, filename: &str) -> (PathBuf, PathBuf) {
    let mp4_filename = normalize_mp4_output_filename(filename);

    if let Some(partial_path) = find_existing_mp4_partial_path(output_dir, &mp4_filename) {
        return (
            mp4_output_path_from_partial_path(&partial_path),
            partial_path,
        );
    }

    resolve_available_mp4_output_paths(output_dir, &mp4_filename)
}

pub fn existing_mp4_partial_path(output_dir: &Path, filename: &str) -> Option<PathBuf> {
    let (_, partial_path) = resolve_existing_mp4_partial_paths(output_dir, filename);
    if partial_path.exists() {
        Some(partial_path)
    } else {
        None
    }
}

async fn file_len_if_exists(path: &Path) -> Result<u64, AppError> {
    match tokio::fs::metadata(path).await {
        Ok(metadata) if metadata.is_file() => Ok(metadata.len()),
        Ok(_) => Ok(0),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}

pub async fn cleanup_mp4_partial_file(output_dir: &Path, filename: &str) -> Result<(), AppError> {
    let (_, partial_path) = resolve_existing_mp4_partial_paths(output_dir, filename);
    match tokio::fs::remove_file(&partial_path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
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
        tasks
            .get(task_id)
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
    segment_indices.lock().await.iter().copied().collect()
}

async fn restore_download_state(
    temp_dir: &Path,
    media_kind: HlsMediaKind,
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
        let completed_path = segment_file_path_for_kind(temp_dir, media_kind, segment.index);
        let partial_path = partial_segment_file_path_for_kind(temp_dir, media_kind, segment.index);
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

    Ok((
        completed_segment_indices,
        failed_segment_indices,
        total_bytes,
    ))
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

async fn restore_fmp4_init_download_state(
    temp_dir: &Path,
    init_segments: &[PreparedHlsInitSegment],
) -> Result<u64, AppError> {
    let mut total_bytes = 0u64;
    for init in init_segments {
        let completed_path = init.output_path(temp_dir);
        let partial_path = part_path_for_downloaded_file(&completed_path);
        if partial_path.exists() {
            let _ = tokio::fs::remove_file(&partial_path).await;
        }
        if completed_path.exists() {
            total_bytes += tokio::fs::metadata(&completed_path).await?.len();
        }
    }

    Ok(total_bytes)
}

async fn download_missing_fmp4_init_segments(
    client: Arc<RwLock<reqwest::Client>>,
    rate_limiter: Arc<DownloadRateLimiter>,
    headers: Arc<RequestHeaders>,
    temp_dir: &Path,
    init_segments: &[PreparedHlsInitSegment],
    cancel: &CancellationToken,
) -> Result<u64, AppError> {
    let mut downloaded_bytes = 0u64;
    for init in init_segments {
        let output_path = init.output_path(temp_dir);
        if output_path.exists() {
            continue;
        }

        let outcome = download_segment_with_retry(
            client.clone(),
            rate_limiter.clone(),
            headers.clone(),
            &init.info.uri,
            &output_path,
            init.info.byte_range.as_ref(),
            init.encryption.as_ref(),
            0,
            3,
            cancel,
            None,
        )
        .await?;

        match outcome {
            SegmentDownloadOutcome::Downloaded(file_size) => downloaded_bytes += file_size,
            SegmentDownloadOutcome::Skipped => {
                return Err(AppError::Network(format!(
                    "fMP4 初始化片段下载失败：{}",
                    init.info.uri
                )));
            }
        }
    }

    Ok(downloaded_bytes)
}

pub async fn run_download(
    app_handle: AppHandle,
    downloads: Arc<Mutex<HashMap<DownloadId, DownloadTask>>>,
    client: Arc<RwLock<reqwest::Client>>,
    rate_limiter: Arc<DownloadRateLimiter>,
    task_id: DownloadId,
    media_kind: HlsMediaKind,
    init_segments: Vec<PreparedHlsInitSegment>,
    segments: Vec<SegmentInfo>,
    headers: Arc<RequestHeaders>,
    output_dir: PathBuf,
    filename: String,
    delete_ts_temp_dir_after_download: bool,
    playback_sessions: Arc<Mutex<HashMap<DownloadId, playback::PlaybackSession>>>,
    download_priorities: Arc<Mutex<HashMap<DownloadId, Arc<playback::DownloadPriorityState>>>>,
    convert_to_mp4: bool,
    ffmpeg_path: Option<PathBuf>,
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
    let (restored_completed_segment_indices, restored_failed_segment_indices, restored_total_bytes) =
        restore_download_state(
            &temp_dir,
            media_kind,
            &segments,
            &existing_completed_segment_indices,
            &existing_failed_segment_indices,
        )
        .await?;
    let restored_init_bytes = if media_kind == HlsMediaKind::Fmp4 {
        restore_fmp4_init_download_state(&temp_dir, &init_segments).await?
    } else {
        0
    };
    let downloaded_init_bytes = if media_kind == HlsMediaKind::Fmp4 {
        download_missing_fmp4_init_segments(
            client.clone(),
            rate_limiter.clone(),
            headers.clone(),
            &temp_dir,
            &init_segments,
            &cancel_token,
        )
        .await?
    } else {
        0
    };
    if media_kind == HlsMediaKind::Fmp4 {
        let init_infos = init_segments
            .iter()
            .map(|init| init.info.clone())
            .collect::<Vec<_>>();
        write_fmp4_local_playlist(&temp_dir, &init_infos, &segments).await?;
    }

    let initial_total_bytes = restored_total_bytes
        .saturating_add(restored_init_bytes)
        .saturating_add(downloaded_init_bytes);
    let semaphore = Arc::new(Semaphore::new(MAX_DOWNLOAD_CONCURRENCY));
    let completed = Arc::new(AtomicUsize::new(restored_completed_segment_indices.len()));
    let total_bytes = Arc::new(AtomicU64::new(initial_total_bytes));
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
        let restored_total_bytes = initial_total_bytes;

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
            media_kind,
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

    let final_path = if media_kind == HlsMediaKind::Fmp4 {
        if convert_to_mp4 {
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
            let init_infos = init_segments
                .iter()
                .map(|init| init.info.clone())
                .collect::<Vec<_>>();
            merge_fmp4_segments(&temp_dir, &init_infos, segments.as_ref(), &mp4_path).await?;
            mp4_path
        } else {
            temp_dir.clone()
        }
    } else {
        // Merge segments into .ts file
        let ts_filename = ensure_extension(&filename, "ts");
        let ts_path = resolve_available_output_path(&output_dir, &ts_filename);
        merge_segments(&temp_dir, total, &ts_path).await?;

        if convert_to_mp4 {
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

            match convert_ts_to_mp4_file(
                &ts_path,
                &mp4_path,
                true,
                ffmpeg_path.is_some(),
                ffmpeg_path.as_deref(),
            )
            .await
            {
                Ok(()) => mp4_path,
                Err(_) => ts_path,
            }
        } else {
            ts_path
        }
    };

    if media_kind != HlsMediaKind::Fmp4
        && delete_ts_temp_dir_after_download
        && !playback::has_active_playback_session(&playback_sessions, &task_id).await
    {
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    }
    if media_kind == HlsMediaKind::Fmp4
        && convert_to_mp4
        && delete_ts_temp_dir_after_download
        && !playback::has_active_playback_session(&playback_sessions, &task_id).await
    {
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    }

    Ok(DownloadRunOutcome::Completed(final_path))
}

async fn write_bundle_playlist_files(
    bundle_dir: &Path,
    playlist_files: &[BundlePlaylistFile],
) -> Result<(), AppError> {
    for playlist_file in playlist_files {
        let path = bundle_dir.join(&playlist_file.relative_path);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(path, playlist_file.content.as_bytes()).await?;
    }

    Ok(())
}

async fn restore_bundle_download_state(
    bundle_dir: &Path,
    entries: &[BundleDownloadEntry],
    recorded_completed_segment_indices: &[usize],
    recorded_failed_segment_indices: &[usize],
) -> Result<(BTreeSet<usize>, BTreeSet<usize>, u64), AppError> {
    let mut completed_segment_indices = BTreeSet::new();
    let mut failed_segment_indices = recorded_failed_segment_indices
        .iter()
        .copied()
        .filter(|value| *value > 0 && *value <= entries.len())
        .collect::<BTreeSet<_>>();
    let mut total_bytes = 0u64;
    let recorded: BTreeSet<usize> = recorded_completed_segment_indices.iter().copied().collect();

    for entry in entries {
        let completed_path = entry.output_path(bundle_dir);
        let partial_path = part_path_for_downloaded_file(&completed_path);
        if partial_path.exists() {
            let _ = tokio::fs::remove_file(&partial_path).await;
        }

        if completed_path.exists() {
            if !recorded.is_empty() && !recorded.contains(&(entry.index + 1)) {
                // Trust on-disk files even if the persisted record is stale.
            }
            total_bytes += tokio::fs::metadata(&completed_path).await?.len();
            completed_segment_indices.insert(entry.index + 1);
            failed_segment_indices.remove(&(entry.index + 1));
        }
    }

    Ok((
        completed_segment_indices,
        failed_segment_indices,
        total_bytes,
    ))
}

pub async fn run_hls_bundle_download(
    app_handle: AppHandle,
    downloads: Arc<Mutex<HashMap<DownloadId, DownloadTask>>>,
    client: Arc<RwLock<reqwest::Client>>,
    rate_limiter: Arc<DownloadRateLimiter>,
    task_id: DownloadId,
    output_dir: PathBuf,
    filename: String,
    bundle_dir: PathBuf,
    playlist_files: Vec<BundlePlaylistFile>,
    entries: Vec<BundleDownloadEntry>,
    headers: Arc<RequestHeaders>,
    convert_to_mp4: bool,
    ffmpeg_path: Option<PathBuf>,
    cancel_token: CancellationToken,
    max_concurrent: Arc<Mutex<usize>>,
) -> Result<DownloadRunOutcome, AppError> {
    let total = entries.len();
    tokio::fs::create_dir_all(&bundle_dir).await?;
    write_bundle_playlist_files(&bundle_dir, &playlist_files).await?;

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
    let (restored_completed_segment_indices, restored_failed_segment_indices, restored_total_bytes) =
        restore_bundle_download_state(
            &bundle_dir,
            &entries,
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
    let pending_indices = entries
        .iter()
        .filter(|entry| !restored_completed_segments.contains(&(entry.index + 1)))
        .map(|entry| entry.index)
        .collect::<Vec<_>>();
    let next_pending = Arc::new(AtomicUsize::new(0));
    let entries = Arc::new(entries);
    let pending_indices = Arc::new(pending_indices);
    let worker_count = MAX_DOWNLOAD_CONCURRENCY.min(total.max(1));
    let mut worker_handles = Vec::with_capacity(worker_count);

    for _ in 0..worker_count {
        worker_handles.push(tokio::spawn(bundle_download_worker_loop(
            semaphore.clone(),
            client.clone(),
            rate_limiter.clone(),
            headers.clone(),
            bundle_dir.clone(),
            entries.clone(),
            pending_indices.clone(),
            next_pending.clone(),
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
                        "Bundle download worker task join error: {}",
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
        let _ = tokio::fs::remove_dir_all(&bundle_dir).await;
        return Err(error);
    }

    if cancel_token.is_cancelled() {
        let status = current_task_status(&downloads, &task_id).await;
        if !matches!(status, Some(DownloadStatus::Paused)) {
            let _ = tokio::fs::remove_dir_all(&bundle_dir).await;
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

    if convert_to_mp4 {
        if let Some(ffmpeg_path) = ffmpeg_path {
            let mp4_filename = ensure_extension(&filename, "mp4");
            let mp4_path = resolve_available_output_path(&output_dir, &mp4_filename);
            let video_playlist = bundle_dir.join("video").join("index.m3u8");
            let audio_playlist = bundle_dir.join("audio").join("index.m3u8");
            let subtitle_playlist = bundle_dir.join("subtitle").join("index.m3u8");
            let audio_playlist = audio_playlist.is_file().then_some(audio_playlist);
            let subtitle_playlist = subtitle_playlist.is_file().then_some(subtitle_playlist);

            if video_playlist.is_file() {
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
                        total_bytes: total_bytes.load(Ordering::Relaxed),
                        speed_bytes_per_sec: 0,
                        updated_at: Utc::now().to_rfc3339(),
                    },
                )
                .await;

                if crate::ffmpeg::convert_multi_track_hls_to_mp4(
                    &ffmpeg_path,
                    &video_playlist,
                    audio_playlist.as_deref(),
                    subtitle_playlist.as_deref(),
                    &mp4_path,
                )
                .await
                .is_ok()
                {
                    return Ok(DownloadRunOutcome::Completed(mp4_path));
                }
            }
        }
    }

    Ok(DownloadRunOutcome::Completed(bundle_dir))
}

async fn bundle_download_worker_loop(
    semaphore: Arc<Semaphore>,
    client: Arc<RwLock<reqwest::Client>>,
    rate_limiter: Arc<DownloadRateLimiter>,
    headers: Arc<RequestHeaders>,
    bundle_dir: PathBuf,
    entries: Arc<Vec<BundleDownloadEntry>>,
    pending_indices: Arc<Vec<usize>>,
    next_pending: Arc<AtomicUsize>,
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

        let pending_position = next_pending.fetch_add(1, Ordering::Relaxed);
        let Some(entry_index) = pending_indices.get(pending_position).copied() else {
            return Ok(());
        };

        let permit = tokio::select! {
            _ = cancel.cancelled() => return Err(AppError::Cancelled),
            permit = semaphore.acquire() => permit
                .map_err(|_| AppError::Internal("下载并发控制已关闭".to_string()))?,
        };

        let entry = match entries.get(entry_index).cloned() {
            Some(entry) => entry,
            None => {
                drop(permit);
                return Err(AppError::Internal(format!(
                    "Missing bundle entry metadata for index {}",
                    entry_index
                )));
            }
        };
        let output_path = entry.output_path(&bundle_dir);
        if let Some(parent) = output_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let outcome = download_segment_with_retry(
            client.clone(),
            rate_limiter.clone(),
            headers.clone(),
            &entry.uri,
            &output_path,
            entry.byte_range.as_ref(),
            entry.encryption.as_ref(),
            entry.sequence_number,
            3,
            &cancel,
            Some(total_bytes.clone()),
        )
        .await;

        drop(permit);

        match outcome {
            Ok(SegmentDownloadOutcome::Downloaded(_file_size)) => {
                completed_segment_indices
                    .lock()
                    .await
                    .insert(entry.index + 1);
                failed_segment_indices
                    .lock()
                    .await
                    .remove(&(entry.index + 1));
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
                failed_segment_indices.lock().await.insert(entry.index + 1);
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
            Err(AppError::Cancelled) => return Err(AppError::Cancelled),
            Err(error) => return Err(error),
        }
    }
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
    media_kind: HlsMediaKind,
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

        let segment_path = segment_file_path_for_kind(&temp_dir, media_kind, segment.index);
        let outcome = download_segment_with_retry(
            client.clone(),
            rate_limiter.clone(),
            headers.clone(),
            &segment.uri,
            &segment_path,
            segment.byte_range.as_ref(),
            segment.encryption.as_ref(),
            segment.sequence_number,
            3,
            &cancel,
            Some(total_bytes.clone()),
        )
        .await;

        drop(permit);

        match outcome {
            Ok(SegmentDownloadOutcome::Downloaded(_file_size)) => {
                priority_state.mark_segment_completed(segment.index).await;
                completed_segment_indices
                    .lock()
                    .await
                    .insert(segment.index + 1);
                failed_segment_indices
                    .lock()
                    .await
                    .remove(&(segment.index + 1));
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
                failed_segment_indices
                    .lock()
                    .await
                    .insert(segment.index + 1);
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

fn atomic_saturating_sub(atom: &AtomicU64, n: u64) {
    if n == 0 {
        return;
    }
    let mut current = atom.load(Ordering::Relaxed);
    loop {
        let new = current.saturating_sub(n);
        match atom.compare_exchange_weak(current, new, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(actual) => current = actual,
        }
    }
}

struct SegmentByteCounter {
    sink: Option<Arc<AtomicU64>>,
    pending: u64,
    committed: bool,
}

impl SegmentByteCounter {
    fn new(sink: Option<Arc<AtomicU64>>) -> Self {
        Self {
            sink,
            pending: 0,
            committed: false,
        }
    }

    fn add(&mut self, bytes: u64) {
        if bytes == 0 {
            return;
        }
        if let Some(sink) = &self.sink {
            sink.fetch_add(bytes, Ordering::Relaxed);
        }
        self.pending = self.pending.saturating_add(bytes);
    }

    fn commit(&mut self, final_size: u64) {
        if let Some(sink) = &self.sink {
            if final_size > self.pending {
                sink.fetch_add(final_size - self.pending, Ordering::Relaxed);
            } else if self.pending > final_size {
                atomic_saturating_sub(sink, self.pending - final_size);
            }
        }
        self.pending = final_size;
        self.committed = true;
    }
}

impl Drop for SegmentByteCounter {
    fn drop(&mut self) {
        if !self.committed {
            if let Some(sink) = &self.sink {
                atomic_saturating_sub(sink, self.pending);
            }
        }
    }
}

async fn download_segment_with_retry(
    client: Arc<RwLock<reqwest::Client>>,
    rate_limiter: Arc<DownloadRateLimiter>,
    headers: Arc<RequestHeaders>,
    url: &str,
    path: &Path,
    byte_range: Option<&ByteRangeSpec>,
    encryption: Option<&EncryptionInfo>,
    sequence_number: u64,
    max_retries: u32,
    cancel: &CancellationToken,
    progress_sink: Option<Arc<AtomicU64>>,
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
            byte_range,
            encryption,
            sequence_number,
            cancel,
            progress_sink.clone(),
        )
        .await
        {
            Ok(file_size) => {
                return Ok(SegmentDownloadOutcome::Downloaded(file_size));
            }
            Err(e) => {
                if matches!(e, AppError::Cancelled) {
                    return Err(e);
                }
                attempts += 1;
                if attempts >= max_retries {
                    let _ = tokio::fs::remove_file(path).await;
                    let _ = tokio::fs::remove_file(part_path_for_downloaded_file(path)).await;
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
    byte_range: Option<&ByteRangeSpec>,
    encryption: Option<&EncryptionInfo>,
    sequence_number: u64,
    cancel: &CancellationToken,
    progress_sink: Option<Arc<AtomicU64>>,
) -> Result<u64, AppError> {
    let part_path = part_path_for_downloaded_file(path);
    if part_path.exists() {
        let _ = tokio::fs::remove_file(&part_path).await;
    }
    let mut byte_counter = SegmentByteCounter::new(progress_sink);

    let active_client = client.read().await.clone();
    let mut request = build_request_with_headers(&active_client, url, headers.as_ref());
    if let Some(byte_range) = byte_range {
        let range_value = match byte_range.offset {
            Some(offset) if byte_range.length == 0 => format!("bytes={}-", offset),
            Some(offset) if byte_range.length > 0 => {
                format!(
                    "bytes={}-{}",
                    offset,
                    offset + byte_range.length.saturating_sub(1)
                )
            }
            Some(offset) => format!("bytes={}-{}", offset, offset),
            None if byte_range.length > 0 => format!("bytes=0-{}", byte_range.length - 1),
            None => "bytes=0-0".to_string(),
        };
        request = request.header(header::RANGE, range_value);
    }

    let response = request.send().await?.error_for_status()?;

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
        byte_counter.add(chunk.len() as u64);
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

    let final_size = tokio::fs::metadata(path).await?.len();
    byte_counter.commit(final_size);
    Ok(final_size)
}

// --- Merge Segments ---

async fn merge_segments(temp_dir: &Path, total: usize, output_path: &Path) -> Result<(), AppError> {
    let segment_paths = (0..total)
        .map(|index| segment_file_path(temp_dir, index))
        .collect::<Vec<_>>();
    merge_files(&segment_paths, output_path).await
}

async fn merge_fmp4_segments(
    temp_dir: &Path,
    init_segments: &[HlsInitSegmentInfo],
    segments: &[SegmentInfo],
    output_path: &Path,
) -> Result<(), AppError> {
    let mut output_file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output_path)
        .await?;
    let mut last_init_index = None;

    for segment in segments {
        if segment.init_segment_index != last_init_index {
            if let Some(init_index) = segment.init_segment_index {
                let init = init_segments
                    .iter()
                    .find(|init| init.index == init_index)
                    .ok_or_else(|| {
                        AppError::Internal(format!(
                            "Missing fMP4 init segment metadata for index {}",
                            init_index
                        ))
                    })?;
                let init_bytes =
                    tokio::fs::read(hls_init_segment_file_path(temp_dir, init.index)).await?;
                output_file.write_all(&init_bytes).await?;
            }
            last_init_index = segment.init_segment_index;
        }

        let segment_bytes =
            tokio::fs::read(fmp4_segment_file_path(temp_dir, segment.index)).await?;
        output_file.write_all(&segment_bytes).await?;
    }

    output_file.flush().await?;
    Ok(())
}

pub async fn write_fmp4_local_playlist(
    temp_dir: &Path,
    init_segments: &[HlsInitSegmentInfo],
    segments: &[SegmentInfo],
) -> Result<(), AppError> {
    let target_duration = segments.iter().fold(1u64, |max_duration, segment| {
        max_duration.max(segment.duration.ceil().max(1.0) as u64)
    });
    let mut lines = Vec::with_capacity(segments.len() * 3 + 8);
    lines.push("#EXTM3U".to_string());
    lines.push("#EXT-X-VERSION:6".to_string());
    lines.push(format!("#EXT-X-TARGETDURATION:{}", target_duration));
    lines.push("#EXT-X-MEDIA-SEQUENCE:0".to_string());
    lines.push("#EXT-X-PLAYLIST-TYPE:VOD".to_string());

    let mut last_init_index = None;
    for segment in segments {
        if segment.init_segment_index != last_init_index {
            if let Some(init_index) = segment.init_segment_index {
                let init = init_segments
                    .iter()
                    .find(|init| init.index == init_index)
                    .ok_or_else(|| {
                        AppError::Internal(format!(
                            "Missing fMP4 init segment metadata for index {}",
                            init_index
                        ))
                    })?;
                lines.push(format!(
                    "#EXT-X-MAP:URI=\"{}\"",
                    hls_init_segment_file_path(Path::new(""), init.index)
                        .to_string_lossy()
                        .replace('\\', "/")
                ));
            }
            last_init_index = segment.init_segment_index;
        }
        lines.push(format!("#EXTINF:{:.3},", segment.duration));
        lines.push(
            fmp4_segment_file_path(Path::new(""), segment.index)
                .to_string_lossy()
                .replace('\\', "/"),
        );
    }

    lines.push("#EXT-X-ENDLIST".to_string());
    tokio::fs::write(temp_dir.join("index.m3u8"), lines.join("\n")).await?;
    Ok(())
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

// --- Local M3U8 to MP4 ---

fn resolve_local_m3u8_uri(base_dir: &Path, uri: &str) -> Result<PathBuf, AppError> {
    let trimmed = uri.trim();
    if trimmed.is_empty() {
        return Err(AppError::M3u8Parse("m3u8 中存在空 URI".to_string()));
    }

    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Err(AppError::InvalidInput("本地转换不支持网络 URI".to_string()));
    }

    let cleaned: &str = trimmed.split(['?', '#']).next().unwrap_or(trimmed);

    let candidate = if let Some(rest) = cleaned
        .strip_prefix("file://")
        .or_else(|| cleaned.strip_prefix("FILE://"))
    {
        // Strip optional leading slash on Windows-style file:///C:/... URIs.
        let rest = if cfg!(windows) {
            rest.strip_prefix('/').unwrap_or(rest)
        } else {
            rest
        };
        PathBuf::from(rest)
    } else {
        let path = Path::new(cleaned);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            base_dir.join(path)
        }
    };

    let resolved = std::fs::canonicalize(&candidate).unwrap_or(candidate);
    if !resolved.is_file() {
        return Err(AppError::InvalidInput(format!(
            "找不到本地文件：{}",
            resolved.display()
        )));
    }

    Ok(resolved)
}

async fn read_local_m3u8_bytes(
    path: &Path,
    byte_range: Option<&ByteRangeSpec>,
) -> Result<Vec<u8>, AppError> {
    let bytes = tokio::fs::read(path).await?;
    let Some(byte_range) = byte_range else {
        return Ok(bytes);
    };

    let offset = byte_range.offset.unwrap_or(0) as usize;
    let length = byte_range.length as usize;
    let end = offset.saturating_add(length);
    if offset > bytes.len() || end > bytes.len() {
        return Err(AppError::InvalidInput(format!(
            "字节范围超出文件大小：{}",
            path.display()
        )));
    }

    Ok(bytes[offset..end].to_vec())
}

pub async fn convert_local_m3u8_to_mp4_file(
    m3u8_path: &Path,
    mp4_path: &Path,
    ffmpeg_enabled: bool,
    ffmpeg_path: Option<&Path>,
) -> Result<(), AppError> {
    let bytes = tokio::fs::read(m3u8_path).await?;
    let playlist = m3u8_rs::parse_playlist_res(&bytes)
        .map_err(|_| AppError::InvalidInput("所选文件不是有效的 M3U8 播放列表".to_string()))?;

    let media = match playlist {
        m3u8_rs::Playlist::MediaPlaylist(media) => media,
        m3u8_rs::Playlist::MasterPlaylist(_) => {
            return Err(AppError::InvalidInput(
                "不支持主播放列表，请指向包含分片的 m3u8 文件".to_string(),
            ));
        }
    };

    let base_dir = m3u8_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let media_sequence = media.media_sequence;
    let mut current_enc: Option<EncryptionInfo> = None;
    let mut key_cache: HashMap<PathBuf, Vec<u8>> = HashMap::new();
    let is_fmp4 = media.segments.iter().any(|segment| segment.map.is_some());

    let tmp_media_path = {
        let stem = mp4_path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("output");
        let parent = mp4_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let extension = if is_fmp4 { "mp4" } else { "ts" };
        parent.join(format!("{}.m3u8quicker.{}", stem, extension))
    };

    let result = async {
        let mut tmp_file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_media_path)
            .await?;
        let mut previous_map_byte_range: Option<(String, u64)> = None;
        let mut previous_media_byte_range: Option<(String, u64)> = None;
        let mut current_map_key: Option<(PathBuf, Option<ByteRangeSpec>)> = None;

        for (index, segment) in media.segments.iter().enumerate() {
            if !is_fmp4 && segment.byte_range.is_some() {
                return Err(AppError::InvalidInput(
                    "暂不支持包含 EXT-X-BYTERANGE 的播放列表".to_string(),
                ));
            }

            if let Some(key) = segment.key.as_ref() {
                if key.method == m3u8_rs::KeyMethod::None {
                    current_enc = None;
                } else if is_aes_cbc_method(&key.method) {
                    let method_name = key.method.to_string();
                    let key_uri = key.uri.as_ref().ok_or_else(|| {
                        AppError::M3u8Parse(format!("{} key 缺少 URI", method_name))
                    })?;
                    let key_path = resolve_local_m3u8_uri(&base_dir, key_uri)?;
                    let key_bytes = if let Some(cached) = key_cache.get(&key_path) {
                        cached.clone()
                    } else {
                        let bytes = tokio::fs::read(&key_path).await?;
                        if !matches!(bytes.len(), 16 | 24 | 32) {
                            return Err(AppError::Decryption(format!(
                                "AES key 长度非法：{} 字节",
                                bytes.len()
                            )));
                        }
                        key_cache.insert(key_path.clone(), bytes.clone());
                        bytes
                    };
                    let method = match key_bytes.len() {
                        16 => "AES-128",
                        24 => "AES-192",
                        32 => "AES-256",
                        _ => "AES-128",
                    }
                    .to_string();
                    current_enc = Some(EncryptionInfo {
                        method,
                        key_uri: key_uri.clone(),
                        iv: key.iv.clone(),
                        key_bytes,
                    });
                } else {
                    return Err(AppError::M3u8Parse(format!(
                        "不支持的加密方式：{:?}",
                        key.method
                    )));
                }
            }

            if let Some(map) = segment.map.as_ref() {
                let map_path = resolve_local_m3u8_uri(&base_dir, &map.uri)?;
                let map_uri_key = map_path.to_string_lossy().to_string();
                let map_byte_range = resolve_explicit_byte_range(
                    &map_uri_key,
                    map.byte_range.as_ref(),
                    &mut previous_map_byte_range,
                );
                let map_key = (map_path.clone(), map_byte_range.clone());
                if current_map_key.as_ref() != Some(&map_key) {
                    if current_enc.as_ref().is_some_and(|enc| enc.iv.is_none()) {
                        return Err(AppError::Decryption(
                            "加密的 fMP4 EXT-X-MAP 必须提供显式 IV".to_string(),
                        ));
                    }
                    let raw = read_local_m3u8_bytes(&map_path, map_byte_range.as_ref()).await?;
                    let plain = if let Some(ref enc) = current_enc {
                        let iv = compute_iv(enc, 0);
                        decrypt_aes_cbc(&raw, &enc.key_bytes, &iv)?
                    } else {
                        raw
                    };
                    tmp_file.write_all(&plain).await?;
                    current_map_key = Some(map_key);
                }
            } else if is_fmp4 && current_map_key.is_none() {
                return Err(AppError::InvalidInput(
                    "fMP4 播放列表缺少 EXT-X-MAP".to_string(),
                ));
            }

            let segment_path = resolve_local_m3u8_uri(&base_dir, &segment.uri)?;
            let segment_uri_key = segment_path.to_string_lossy().to_string();
            let media_byte_range = resolve_explicit_byte_range(
                &segment_uri_key,
                segment.byte_range.as_ref(),
                &mut previous_media_byte_range,
            );
            let raw = read_local_m3u8_bytes(&segment_path, media_byte_range.as_ref()).await?;
            let sequence_number = media_sequence + index as u64;

            let plain = if let Some(ref enc) = current_enc {
                let iv = compute_iv(enc, sequence_number);
                decrypt_aes_cbc(&raw, &enc.key_bytes, &iv)?
            } else {
                raw
            };

            tmp_file.write_all(&plain).await?;
        }

        tmp_file.flush().await?;
        drop(tmp_file);

        if is_fmp4 {
            tokio::fs::rename(&tmp_media_path, mp4_path).await?;
            Ok(())
        } else {
            convert_ts_to_mp4_file(&tmp_media_path, mp4_path, true, ffmpeg_enabled, ffmpeg_path)
                .await
        }
    }
    .await;

    if result.is_err() {
        let _ = tokio::fs::remove_file(&tmp_media_path).await;
    }

    result
}

fn mp4_resume_response_mode(status: StatusCode) -> Mp4ResumeResponseMode {
    match status {
        StatusCode::PARTIAL_CONTENT => Mp4ResumeResponseMode::Append,
        StatusCode::OK | StatusCode::RANGE_NOT_SATISFIABLE => {
            Mp4ResumeResponseMode::RestartRequired
        }
        _ => Mp4ResumeResponseMode::Unexpected,
    }
}

fn should_keep_mp4_partial_on_cancel(status: Option<&DownloadStatus>) -> bool {
    matches!(status, Some(DownloadStatus::Paused))
}

async fn send_mp4_download_request(
    client: &reqwest::Client,
    url: &str,
    headers: &RequestHeaders,
    resume_from: Option<u64>,
) -> Result<reqwest::Response, AppError> {
    let mut request =
        build_request_with_headers(client, url, headers).timeout(MP4_DOWNLOAD_TIMEOUT);
    if let Some(offset) = resume_from.filter(|offset| *offset > 0) {
        request = request.header(header::RANGE, format!("bytes={}-", offset));
    }

    Ok(request.send().await?)
}

pub async fn check_mp4_resume(
    client: &reqwest::Client,
    url: &str,
    headers: &RequestHeaders,
    output_dir: &Path,
    filename: &str,
) -> Result<Mp4ResumeCheck, AppError> {
    let (_, partial_path) = resolve_mp4_output_paths(output_dir, filename, true);
    let downloaded_bytes = file_len_if_exists(&partial_path).await?;
    if downloaded_bytes == 0 {
        return Ok(Mp4ResumeCheck::Ready { downloaded_bytes });
    }

    let response = send_mp4_download_request(client, url, headers, Some(downloaded_bytes)).await?;
    match mp4_resume_response_mode(response.status()) {
        Mp4ResumeResponseMode::Append => Ok(Mp4ResumeCheck::Ready { downloaded_bytes }),
        Mp4ResumeResponseMode::RestartRequired => {
            Ok(Mp4ResumeCheck::RequiresRestartConfirmation { downloaded_bytes })
        }
        Mp4ResumeResponseMode::Unexpected => {
            response.error_for_status()?;
            Ok(Mp4ResumeCheck::RequiresRestartConfirmation { downloaded_bytes })
        }
    }
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
    resume_existing_partial: bool,
    restart_confirmed: bool,
    cancel_token: CancellationToken,
) -> Result<DownloadRunOutcome, AppError> {
    let (mp4_path, partial_path) =
        resolve_mp4_output_paths(&output_dir, &filename, resume_existing_partial);
    let client = client.read().await.clone();
    let existing_bytes = file_len_if_exists(&partial_path).await?;
    let mut downloaded = 0u64;
    let mut append = false;
    let response = if existing_bytes > 0 {
        let response =
            send_mp4_download_request(&client, &url, &headers, Some(existing_bytes)).await?;

        match mp4_resume_response_mode(response.status()) {
            Mp4ResumeResponseMode::Append => {
                downloaded = existing_bytes;
                append = true;
                response.error_for_status()?
            }
            Mp4ResumeResponseMode::RestartRequired => {
                if !restart_confirmed {
                    return Err(AppError::InvalidInput(
                        "服务器不支持断点续传，请确认后从头下载".to_string(),
                    ));
                }

                let _ = tokio::fs::remove_file(&partial_path).await;
                if response.status() == StatusCode::OK {
                    response.error_for_status()?
                } else {
                    send_mp4_download_request(&client, &url, &headers, None)
                        .await?
                        .error_for_status()?
                }
            }
            Mp4ResumeResponseMode::Unexpected => response.error_for_status()?,
        }
    } else {
        send_mp4_download_request(&client, &url, &headers, None)
            .await?
            .error_for_status()?
    };

    let content_length = response.content_length().unwrap_or(0);
    let expected_total_bytes = if content_length > 0 {
        downloaded.saturating_add(content_length)
    } else {
        0
    };
    let mut stream = response.bytes_stream();
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .append(append)
        .truncate(!append)
        .open(&partial_path)
        .await?;

    let mut last_report = Instant::now();
    let mut last_report_bytes = downloaded;

    emit_mp4_progress(
        &app_handle,
        &downloads,
        &task_id,
        downloaded,
        expected_total_bytes,
        0,
    )
    .await;

    while let Some(chunk) = tokio::select! {
        chunk = stream.next() => chunk,
        _ = cancel_token.cancelled() => {
            file.flush().await?;
            drop(file);
            let status = current_task_status(&downloads, &task_id).await;
            if !should_keep_mp4_partial_on_cancel(status.as_ref()) {
                let _ = tokio::fs::remove_file(&partial_path).await;
            }
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

            emit_mp4_progress(
                &app_handle,
                &downloads,
                &task_id,
                downloaded,
                expected_total_bytes,
                speed,
            )
            .await;
        }
    }

    file.flush().await?;
    drop(file);
    tokio::fs::rename(&partial_path, &mp4_path).await?;

    Ok(DownloadRunOutcome::Completed(mp4_path))
}

async fn emit_mp4_progress(
    app_handle: &AppHandle,
    downloads: &Arc<Mutex<HashMap<DownloadId, DownloadTask>>>,
    task_id: &str,
    downloaded: u64,
    expected_total_bytes: u64,
    speed_bytes_per_sec: u64,
) {
    let total_segments = if expected_total_bytes > 0 { 100 } else { 0 };
    let completed_segments = if expected_total_bytes > 0 {
        ((downloaded as f64 / expected_total_bytes as f64) * 100.0).min(100.0) as usize
    } else {
        0
    };

    emit_progress(
        app_handle,
        downloads,
        RuntimeProgressSnapshot {
            id: task_id.to_string(),
            status: DownloadStatus::Downloading,
            completed_segments,
            total_segments,
            completed_segment_indices: Vec::new(),
            failed_segment_indices: Vec::new(),
            total_bytes: downloaded,
            speed_bytes_per_sec,
            updated_at: Utc::now().to_rfc3339(),
        },
    )
    .await;
}

pub async fn convert_ts_to_mp4_file(
    ts_path: &Path,
    mp4_path: &Path,
    delete_source: bool,
    ffmpeg_enabled: bool,
    ffmpeg_path: Option<&Path>,
) -> Result<(), AppError> {
    let ts_path = ts_path.to_path_buf();
    if ffmpeg_enabled {
        if let Some(ffmpeg) = ffmpeg_path {
            match crate::ffmpeg::convert_ts_to_mp4(ffmpeg, &ts_path, mp4_path).await {
                Ok(()) => {}
                Err(ffmpeg_err) => {
                    let _ = tokio::fs::remove_file(mp4_path).await;
                    remux_ts_to_mp4_with_rust(&ts_path, mp4_path)
                        .await
                        .map_err(|remux_err| {
                            AppError::Conversion(format!(
                                "FFmpeg: {}; Rust remux: {}",
                                ffmpeg_err, remux_err
                            ))
                        })?;
                }
            }
        } else {
            remux_ts_to_mp4_with_rust(&ts_path, mp4_path).await?;
        }
    } else {
        remux_ts_to_mp4_with_rust(&ts_path, mp4_path).await?;
    }

    if delete_source {
        let _ = tokio::fs::remove_file(ts_path).await;
    }

    Ok(())
}

async fn remux_ts_to_mp4_with_rust(ts_path: &Path, mp4_path: &Path) -> Result<(), AppError> {
    let blocking_ts_path = ts_path.to_path_buf();
    let blocking_mp4_path = mp4_path.to_path_buf();

    tokio::task::spawn_blocking(move || {
        crate::remux::remux_ts_to_mp4_file(&blocking_ts_path, &blocking_mp4_path)
    })
    .await
    .map_err(|e| AppError::Conversion(format!("Task join error: {}", e)))?
    .map_err(|e| AppError::Conversion(format!("TS to MP4 conversion failed: {}", e)))
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
    use std::fs;
    use uuid::Uuid;

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
            0x3c, 0x4d, 0x7e, 0x23, 0xed, 0xf7, 0x84, 0x18, 0xa3, 0xb4, 0xbe, 0xc4, 0x30, 0xdf,
            0x2b, 0x61,
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

    #[test]
    fn resolve_mp4_output_paths_reuses_existing_indexed_partial() {
        let temp_root = unique_temp_path("mp4-partial-reuse");
        fs::create_dir_all(&temp_root).expect("create temp dir");
        let existing_final_path = temp_root.join("video.mp4");
        let partial_path = temp_root.join("video (1).mp4.partial");
        fs::write(&existing_final_path, b"existing").expect("write existing file");
        fs::write(&partial_path, b"partial").expect("write partial file");

        let (resolved_final_path, resolved_partial_path) =
            resolve_mp4_output_paths(&temp_root, "video", true);

        assert_eq!(resolved_final_path, temp_root.join("video (1).mp4"));
        assert_eq!(resolved_partial_path, partial_path);
        remove_temp_dir(&temp_root);
    }

    #[test]
    fn resolve_mp4_output_paths_avoids_old_partial_for_new_downloads() {
        let temp_root = unique_temp_path("mp4-partial-new-download");
        fs::create_dir_all(&temp_root).expect("create temp dir");
        fs::write(temp_root.join("video.mp4.partial"), b"partial").expect("write partial file");

        let (resolved_final_path, resolved_partial_path) =
            resolve_mp4_output_paths(&temp_root, "video", false);

        assert_eq!(resolved_final_path, temp_root.join("video (1).mp4"));
        assert_eq!(
            resolved_partial_path,
            temp_root.join("video (1).mp4.partial")
        );
        remove_temp_dir(&temp_root);
    }

    #[test]
    fn resolve_mp4_output_paths_preserves_non_mp4_extension() {
        let temp_root = unique_temp_path("direct-file-partial-reuse");
        fs::create_dir_all(&temp_root).expect("create temp dir");
        let partial_path = temp_root.join("video.mkv.partial");
        fs::write(&partial_path, b"partial").expect("write partial file");

        let (resolved_final_path, resolved_partial_path) =
            resolve_mp4_output_paths(&temp_root, "video.mkv", true);

        assert_eq!(resolved_final_path, temp_root.join("video.mkv"));
        assert_eq!(resolved_partial_path, partial_path);
        remove_temp_dir(&temp_root);
    }

    #[test]
    fn mp4_resume_response_mode_appends_on_partial_content() {
        assert_eq!(
            mp4_resume_response_mode(StatusCode::PARTIAL_CONTENT),
            Mp4ResumeResponseMode::Append
        );
    }

    #[test]
    fn mp4_resume_response_mode_requires_restart_on_ok_with_partial() {
        assert_eq!(
            mp4_resume_response_mode(StatusCode::OK),
            Mp4ResumeResponseMode::RestartRequired
        );
    }

    #[test]
    fn mp4_cancel_only_keeps_partial_for_paused_tasks() {
        assert!(should_keep_mp4_partial_on_cancel(Some(
            &DownloadStatus::Paused
        )));
        assert!(!should_keep_mp4_partial_on_cancel(Some(
            &DownloadStatus::Cancelled
        )));
        assert!(!should_keep_mp4_partial_on_cancel(None));
    }

    #[tokio::test]
    async fn cleanup_mp4_partial_file_removes_existing_partial() {
        let temp_root = unique_temp_path("mp4-partial-cleanup");
        fs::create_dir_all(&temp_root).expect("create temp dir");
        let partial_path = temp_root.join("video.mp4.partial");
        fs::write(&partial_path, b"partial").expect("write partial file");

        cleanup_mp4_partial_file(&temp_root, "video")
            .await
            .expect("cleanup partial file");

        assert!(!partial_path.exists());
        remove_temp_dir(&temp_root);
    }

    #[tokio::test]
    async fn cleanup_mp4_partial_file_removes_existing_non_mp4_partial() {
        let temp_root = unique_temp_path("direct-file-partial-cleanup");
        fs::create_dir_all(&temp_root).expect("create temp dir");
        let partial_path = temp_root.join("video.webm.partial");
        fs::write(&partial_path, b"partial").expect("write partial file");

        cleanup_mp4_partial_file(&temp_root, "video.webm")
            .await
            .expect("cleanup partial file");

        assert!(!partial_path.exists());
        remove_temp_dir(&temp_root);
    }

    #[test]
    fn build_master_track_catalog_prefers_highest_video_and_default_audio() {
        let base_url = Url::parse("https://example.com/master.m3u8").expect("base url");
        let playlist = r#"#EXTM3U
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="audio",LANGUAGE="ja",NAME="Japanese",DEFAULT=NO,AUTOSELECT=YES,URI="audio/ja.m3u8"
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="audio",LANGUAGE="en",NAME="English",DEFAULT=YES,AUTOSELECT=YES,URI="audio/en.m3u8"
#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID="subs",LANGUAGE="en",NAME="English",DEFAULT=YES,AUTOSELECT=YES,FORCED=NO,URI="subs/en.m3u8"
#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360,AUDIO="audio",SUBTITLES="subs",CODECS="avc1.4d401e,mp4a.40.2"
low/index.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=1600000,RESOLUTION=1280x720,AUDIO="audio",SUBTITLES="subs",CODECS="avc1.4d401f,mp4a.40.2"
high/index.m3u8
"#;

        let parsed = m3u8_rs::parse_playlist_res(playlist.as_bytes()).expect("parse master");
        let m3u8_rs::Playlist::MasterPlaylist(master) = parsed else {
            panic!("expected master playlist");
        };
        let catalog = build_master_track_catalog(&base_url, &master).expect("catalog");

        assert!(catalog.inspection.requires_selection);
        assert_eq!(catalog.inspection.video_tracks.len(), 2);
        assert_eq!(catalog.inspection.audio_tracks.len(), 2);
        assert_eq!(catalog.inspection.subtitle_tracks.len(), 1);
        assert_eq!(
            catalog.inspection.default_selection.video_id,
            Some(catalog.inspection.video_tracks[0].id.clone())
        );
        assert_eq!(
            catalog.inspection.default_selection.audio_id,
            Some(catalog.inspection.audio_tracks[1].id.clone())
        );
        assert_eq!(catalog.inspection.default_selection.subtitle_id, None);
        assert_eq!(
            catalog.inspection.video_tracks[0].resolution.as_deref(),
            Some("1280x720")
        );
    }

    #[test]
    fn build_bundle_track_plan_writes_local_map_and_segments() {
        let base_url = Url::parse("https://example.com/video/index.m3u8").expect("base url");
        let playlist = r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MAP:URI="init.mp4"
#EXTINF:4.000,
seg-1.m4s
#EXTINF:4.000,
seg-2.m4s
#EXT-X-ENDLIST
"#;

        let parsed = m3u8_rs::parse_playlist_res(playlist.as_bytes()).expect("parse media");
        let m3u8_rs::Playlist::MediaPlaylist(media) = parsed else {
            panic!("expected media playlist");
        };
        let plan = build_bundle_track_plan(
            &FetchedResolvedMediaPlaylist {
                base_url,
                playlist: media,
            },
            "video",
        )
        .expect("bundle plan");

        assert_eq!(plan.entries.len(), 3);
        assert_eq!(plan.entries[0].duration, 0.0);
        assert_eq!(
            plan.entries[0].relative_path,
            PathBuf::from("video").join("init_000001.mp4")
        );
        assert_eq!(
            plan.entries[1].relative_path,
            PathBuf::from("video").join("seg_000001.m4s")
        );
        assert!(plan.playlist_files[0]
            .content
            .contains("#EXT-X-MAP:URI=\"init_000001.mp4\""));
        assert!(plan.playlist_files[0].content.contains("seg_000001.m4s"));
        assert!(plan.playlist_files[0].content.contains("seg_000002.m4s"));
    }

    #[test]
    fn parse_media_playlist_plan_detects_fmp4_init_segments() {
        let base_url = Url::parse("https://example.com/video/index.m3u8").expect("base url");
        let playlist = r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MAP:URI="init.mp4"
#EXTINF:4.000,
seg-1.m4s
#EXTINF:4.000,
seg-2.m4s
#EXT-X-ENDLIST
"#;

        let parsed = m3u8_rs::parse_playlist_res(playlist.as_bytes()).expect("parse media");
        let m3u8_rs::Playlist::MediaPlaylist(media) = parsed else {
            panic!("expected media playlist");
        };
        let plan = parse_media_playlist_plan(&base_url, &media).expect("plan");

        assert_eq!(plan.media_kind, HlsMediaKind::Fmp4);
        assert_eq!(plan.init_segments.len(), 1);
        assert_eq!(
            plan.init_segments[0].info.uri,
            "https://example.com/video/init.mp4"
        );
        assert_eq!(plan.segments[0].init_segment_index, Some(0));
        assert_eq!(plan.segments[1].init_segment_index, Some(0));
    }

    #[test]
    fn validate_fmp4_init_encryption_requires_explicit_iv() {
        let init_segments = vec![PreparedHlsInitSegment {
            info: HlsInitSegmentInfo {
                index: 0,
                uri: "https://example.com/init.mp4".to_string(),
                byte_range: None,
            },
            encryption: Some(EncryptionInfo {
                method: "AES-128".to_string(),
                key_uri: "https://example.com/key.bin".to_string(),
                iv: None,
                key_bytes: Vec::new(),
            }),
        }];

        assert!(validate_fmp4_init_encryption(&init_segments).is_err());
    }

    #[tokio::test]
    async fn convert_local_m3u8_to_mp4_file_supports_fmp4_maps() {
        let temp_root = unique_temp_path("local-fmp4-convert");
        fs::create_dir_all(&temp_root).expect("create temp root");
        fs::write(temp_root.join("init.mp4"), b"init").expect("write init");
        fs::write(temp_root.join("seg-1.m4s"), b"seg1").expect("write seg1");
        fs::write(temp_root.join("seg-2.m4s"), b"seg2").expect("write seg2");
        let playlist = r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MAP:URI="init.mp4"
#EXTINF:4.000,
seg-1.m4s
#EXTINF:4.000,
seg-2.m4s
#EXT-X-ENDLIST
"#;
        let playlist_path = temp_root.join("index.m3u8");
        let output_path = temp_root.join("output.mp4");
        fs::write(&playlist_path, playlist).expect("write playlist");

        convert_local_m3u8_to_mp4_file(&playlist_path, &output_path, false, None)
            .await
            .expect("convert local fmp4");

        assert_eq!(
            fs::read(&output_path).expect("read output"),
            b"initseg1seg2"
        );
        remove_temp_dir(&temp_root);
    }

    #[tokio::test]
    async fn merge_fmp4_segments_writes_init_before_fragments() {
        let temp_root = unique_temp_path("merge-fmp4");
        fs::create_dir_all(&temp_root).expect("create temp root");
        fs::write(hls_init_segment_file_path(&temp_root, 0), b"init").expect("write init");
        fs::write(fmp4_segment_file_path(&temp_root, 0), b"seg0").expect("write seg0");
        fs::write(fmp4_segment_file_path(&temp_root, 1), b"seg1").expect("write seg1");
        let output_path = temp_root.join("merged.mp4");
        let init_segments = vec![HlsInitSegmentInfo {
            index: 0,
            uri: "https://example.com/init.mp4".to_string(),
            byte_range: None,
        }];
        let segments = vec![
            SegmentInfo {
                index: 0,
                uri: "https://example.com/seg0.m4s".to_string(),
                duration: 4.0,
                sequence_number: 0,
                byte_range: None,
                init_segment_index: Some(0),
                encryption: None,
            },
            SegmentInfo {
                index: 1,
                uri: "https://example.com/seg1.m4s".to_string(),
                duration: 4.0,
                sequence_number: 1,
                byte_range: None,
                init_segment_index: Some(0),
                encryption: None,
            },
        ];

        merge_fmp4_segments(&temp_root, &init_segments, &segments, &output_path)
            .await
            .expect("merge fmp4");

        assert_eq!(
            fs::read(&output_path).expect("read output"),
            b"initseg0seg1"
        );
        remove_temp_dir(&temp_root);
    }

    #[test]
    fn parse_media_playlist_segments_expands_implicit_byte_ranges() {
        let base_url = Url::parse("https://example.com/video/index.m3u8").expect("base url");
        let playlist = r#"#EXTM3U
#EXT-X-TARGETDURATION:4
#EXTINF:4.000,
#EXT-X-BYTERANGE:100@10
seg.ts
#EXTINF:4.000,
#EXT-X-BYTERANGE:50
seg.ts
#EXT-X-ENDLIST
"#;

        let parsed = m3u8_rs::parse_playlist_res(playlist.as_bytes()).expect("parse media");
        let m3u8_rs::Playlist::MediaPlaylist(media) = parsed else {
            panic!("expected media playlist");
        };
        let segments = parse_media_playlist_segments(&base_url, &media).expect("segments");

        assert_eq!(
            segments[0].byte_range,
            Some(ByteRangeSpec {
                length: 100,
                offset: Some(10),
            })
        );
        assert_eq!(
            segments[1].byte_range,
            Some(ByteRangeSpec {
                length: 50,
                offset: Some(110),
            })
        );
    }

    #[test]
    fn infer_file_extension_ignores_query_on_relative_uri() {
        assert_eq!(
            infer_file_extension("subtitles/en.vtt?segment=28&duration=30", "bin"),
            "vtt"
        );
        assert_eq!(infer_file_extension("media/seg.m4s#frag", "bin"), "m4s");
    }

    #[test]
    fn parse_dash_json_manifest_reads_tracks() {
        let raw = r#"{
          "format": "m3u8quicker-dash-v1",
          "base_url": "https://example.com/dash/",
          "tracks": {
            "video": [{
              "id": "v1",
              "label": "1080p",
              "bandwidth": 3000000,
              "resolution": "1920x1080",
              "codecs": "avc1.640028",
              "init": "video/init.mp4",
              "segments": [{ "uri": "video/seg-1.m4s", "duration": 6.0 }]
            }],
            "audio": [{
              "id": "a1",
              "label": "audio",
              "language": "und",
              "codecs": "mp4a.40.2",
              "init": "audio/init.mp4",
              "segments": [{ "uri": "audio/seg-1.m4s", "duration": 6.0 }]
            }]
          },
          "default_selection": { "video_id": "v1", "audio_id": "a1" }
        }"#;

        let manifest = parse_dash_json_manifest(raw).expect("dash json");

        assert_eq!(
            manifest.video_tracks[0].init.as_ref().expect("init").uri,
            "https://example.com/dash/video/init.mp4"
        );
        assert_eq!(
            manifest.video_tracks[0].segments[0].uri,
            "https://example.com/dash/video/seg-1.m4s"
        );
        assert_eq!(manifest.audio_tracks[0].option.group_id.as_deref(), Some("dash-audio"));
        assert_eq!(
            manifest.video_tracks[0].option.audio_group_id.as_deref(),
            Some("dash-audio")
        );
    }

    #[test]
    fn build_dash_preview_playlist_from_json_emits_video_only_remote_playlist() {
        let raw = r#"{
          "format": "m3u8quicker-dash-v1",
          "base_url": "https://example.com/dash/",
          "tracks": {
            "video": [
              {
                "id": "v1",
                "label": "1080p",
                "init": "video/init.mp4",
                "segments": [
                  { "uri": "video/seg-1.m4s", "duration": 6.0 },
                  { "uri": "video/seg-2.m4s", "duration": 6.0 }
                ]
              }
            ],
            "audio": [
              {
                "id": "a1",
                "init": "audio/init.mp4",
                "segments": [
                  { "uri": "audio/seg-1.m4s", "duration": 6.0 }
                ]
              }
            ]
          },
          "default_selection": { "video_id": "v1", "audio_id": "a1" }
        }"#;

        let playlist = build_dash_preview_playlist_from_json(raw).expect("playlist");

        assert!(playlist.starts_with("#EXTM3U"));
        assert!(playlist.contains("https://example.com/dash/video/seg-1.m4s"));
        assert!(playlist.contains("https://example.com/dash/video/seg-2.m4s"));
        assert!(playlist.contains("EXT-X-MAP"));
        assert!(playlist.contains("https://example.com/dash/video/init.mp4"));
        assert!(playlist.contains("EXT-X-ENDLIST"));
        // The audio track must not bleed into the preview playlist.
        assert!(!playlist.contains("audio/seg-1.m4s"));
        assert!(!playlist.contains("audio/init.mp4"));
    }

    #[test]
    fn build_dash_preview_playlist_from_json_preserves_byte_ranges() {
        let raw = r#"{
          "format": "m3u8quicker-dash-v1",
          "base_url": "https://example.com/dash/",
          "tracks": {
            "video": [
              {
                "id": "v1",
                "init": "media.mp4",
                "byte_range": "0-999",
                "segments": [
                  { "uri": "media.mp4", "duration": 4.0, "byte_range": "1000-4999" },
                  { "uri": "media.mp4", "duration": 4.0, "byte_range": "5000-8999" }
                ]
              }
            ],
            "audio": []
          }
        }"#;

        let playlist = build_dash_preview_playlist_from_json(raw).expect("playlist");

        assert!(playlist.contains("EXT-X-BYTERANGE"));
        assert!(playlist.contains("EXT-X-MAP"));
    }

    #[test]
    fn parse_dash_mpd_manifest_expands_segment_template() {
        let base_url = Url::parse("https://example.com/dash/manifest.mpd").expect("url");
        let mpd = r#"<?xml version="1.0"?>
<MPD type="static" mediaPresentationDuration="PT12S">
  <Period>
    <AdaptationSet contentType="video" mimeType="video/mp4">
      <SegmentTemplate timescale="1" initialization="init-$RepresentationID$.m4s" media="chunk-$RepresentationID$-$Number%05d$.m4s" startNumber="1">
        <SegmentTimeline>
          <S d="6" r="1" />
        </SegmentTimeline>
      </SegmentTemplate>
      <Representation id="0" bandwidth="2500000" width="1920" height="1080" codecs="avc1.640028" />
    </AdaptationSet>
  </Period>
</MPD>"#;

        let manifest = parse_dash_mpd_manifest(mpd, &base_url).expect("mpd");

        assert_eq!(manifest.video_tracks.len(), 1);
        assert_eq!(
            manifest.video_tracks[0].init.as_ref().expect("init").uri,
            "https://example.com/dash/init-0.m4s"
        );
        assert_eq!(
            manifest.video_tracks[0].segments[0].uri,
            "https://example.com/dash/chunk-0-00001.m4s"
        );
        assert_eq!(
            manifest.video_tracks[0].segments[1].uri,
            "https://example.com/dash/chunk-0-00002.m4s"
        );
    }

    fn unique_temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("m3u8quicker-{}-{}", name, Uuid::new_v4()))
    }

    fn remove_temp_dir(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }
}
