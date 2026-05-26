import { invoke } from "@tauri-apps/api/core";
import type {
  ChromiumBrowser,
  ChromiumExtensionInstallResult,
  FirefoxExtensionInstallResult,
  CreateDownloadParams,
  CreateLiveRecordParams,
  DownloadCounts,
  DownloadGroup,
  DownloadSourceKind,
  DownloadTaskPage,
  DownloadTaskSegmentState,
  DownloadTaskSummary,
  InspectHlsTracksParams,
  InspectDashTracksParams,
  InspectHlsTracksResult,
  LiveGroup,
  LiveRecordCounts,
  LiveRecordPage,
  LiveRecordSummary,
  OpenPlaybackSessionResponse,
  ResumeDownloadCheckResult,
  MediaAnalysisResult,
} from "../types";
import type { AppSettings, FfmpegStatus, ProxySettings } from "../types/settings";
import type { UpdateAsset, UpdateInfo } from "../types/update";

export async function createDownload(
  params: CreateDownloadParams
): Promise<DownloadTaskSummary> {
  return invoke<DownloadTaskSummary>("create_download", { params });
}

export async function inspectHlsTracks(
  params: InspectHlsTracksParams
): Promise<InspectHlsTracksResult> {
  return invoke<InspectHlsTracksResult>("inspect_hls_tracks", { params });
}

export async function inspectDashTracks(
  params: InspectDashTracksParams
): Promise<InspectHlsTracksResult> {
  return invoke<InspectHlsTracksResult>("inspect_dash_tracks", { params });
}

export async function cancelDownload(id: string): Promise<void> {
  return invoke("cancel_download", { id });
}

export async function pauseDownload(id: string): Promise<void> {
  return invoke("pause_download", { id });
}

export async function checkResumeDownload(
  id: string
): Promise<ResumeDownloadCheckResult> {
  return invoke<ResumeDownloadCheckResult>("check_resume_download", { id });
}

export async function resumeDownload(
  id: string,
  restartConfirmed = false
): Promise<DownloadTaskSummary> {
  return invoke<DownloadTaskSummary>("resume_download", {
    id,
    restartConfirmed,
  });
}

export async function retryFailedSegments(id: string): Promise<DownloadTaskSummary> {
  return invoke<DownloadTaskSummary>("retry_failed_segments", { id });
}

export async function getDownloadCounts(): Promise<DownloadCounts> {
  return invoke<DownloadCounts>("get_download_counts");
}

export async function getDownloadsPage(
  group: DownloadGroup,
  page: number,
  pageSize: number
): Promise<DownloadTaskPage> {
  return invoke<DownloadTaskPage>("get_downloads_page", {
    group,
    page,
    pageSize,
  });
}

export async function getDownloadSegmentState(
  id: string
): Promise<DownloadTaskSegmentState> {
  return invoke<DownloadTaskSegmentState>("get_download_segment_state", { id });
}

export async function getDownloadSummary(id: string): Promise<DownloadTaskSummary> {
  return invoke<DownloadTaskSummary>("get_download_summary", { id });
}

export async function removeDownload(
  id: string,
  deleteFile: boolean
): Promise<void> {
  return invoke("remove_download", { id, deleteFile });
}

export async function clearHistoryDownloads(): Promise<void> {
  return invoke("clear_history_downloads");
}

export async function getDefaultDownloadDir(): Promise<string> {
  return invoke<string>("get_default_download_dir");
}

export async function setDefaultDownloadDir(path: string): Promise<void> {
  return invoke("set_default_download_dir", { path });
}

export async function openFileLocation(path: string): Promise<void> {
  return invoke("open_file_location", { path });
}

export async function installChromiumExtension(
  browser: ChromiumBrowser
): Promise<ChromiumExtensionInstallResult> {
  return invoke<ChromiumExtensionInstallResult>("install_chromium_extension", {
    browser,
  });
}

export async function openChromiumExtensionsPage(
  browser: ChromiumBrowser
): Promise<boolean> {
  return invoke<boolean>("open_chromium_extensions_page", { browser });
}

export async function installFirefoxExtension(): Promise<FirefoxExtensionInstallResult> {
  return invoke<FirefoxExtensionInstallResult>("install_firefox_extension");
}

export async function openFirefoxAddonsPage(): Promise<boolean> {
  return invoke<boolean>("open_firefox_addons_page");
}

export async function getAppSettings(): Promise<AppSettings> {
  return invoke<AppSettings>("get_app_settings");
}

export async function setProxySettings(proxy: ProxySettings): Promise<void> {
  return invoke("set_proxy_settings", { proxy });
}

export async function setUserAgent(userAgent: string): Promise<void> {
  return invoke("set_user_agent", { userAgent });
}

export async function setTimeoutSettings(
  metadataTimeoutSecs: number,
  segmentTimeoutSecs: number,
  mp4TimeoutSecs: number
): Promise<void> {
  return invoke("set_timeout_settings", {
    metadataTimeoutSecs,
    segmentTimeoutSecs,
    mp4TimeoutSecs,
  });
}

export async function setLiveRecordSettings(
  hlsRefreshMinMs: number,
  hlsRefreshMaxMs: number,
  hlsPlaylistTimeoutSecs: number,
  liveSegmentTimeoutSecs: number,
  liveRetryHlsMs: number,
  liveRetryFlvMs: number
): Promise<void> {
  return invoke("set_live_record_settings", {
    hlsRefreshMinMs,
    hlsRefreshMaxMs,
    hlsPlaylistTimeoutSecs,
    liveSegmentTimeoutSecs,
    liveRetryHlsMs,
    liveRetryFlvMs,
  });
}

export async function setDownloadConcurrency(
  downloadConcurrency: number
): Promise<void> {
  return invoke("set_download_concurrency", { downloadConcurrency });
}

export async function setDownloadSpeedLimit(
  downloadSpeedLimitKbps: number
): Promise<void> {
  return invoke("set_download_speed_limit", { downloadSpeedLimitKbps });
}

export async function setHistoryPageSize(pageSize: number): Promise<void> {
  return invoke("set_history_page_size", { pageSize });
}

export async function setPreviewColumns(previewColumns: number): Promise<void> {
  return invoke("set_preview_columns", { previewColumns });
}

export async function setPreviewCount(previewCount: number): Promise<void> {
  return invoke("set_preview_count", { previewCount });
}

export async function setPreviewThumbnailSettings(
  previewThumbnailWidth: number,
  previewJpegQuality: number
): Promise<void> {
  return invoke("set_preview_thumbnail_settings", {
    previewThumbnailWidth,
    previewJpegQuality,
  });
}

export async function setDownloadOutputSettings(
  deleteTsTempDirAfterDownload: boolean,
  convertToMp4: boolean
): Promise<void> {
  return invoke("set_download_output_settings", {
    deleteTsTempDirAfterDownload,
    convertToMp4,
  });
}

export async function openDownloadPlaybackSession(
  id: string
): Promise<OpenPlaybackSessionResponse> {
  return invoke<OpenPlaybackSessionResponse>("open_download_playback_session", {
    id,
  });
}

export async function prioritizeDownloadPlaybackPosition(
  id: string,
  positionSecs: number
): Promise<void> {
  return invoke("prioritize_download_playback_position", {
    id,
    positionSecs,
  });
}

export async function closeDownloadPlaybackSession(
  id: string,
  sessionToken: string
): Promise<void> {
  return invoke("close_download_playback_session", {
    id,
    sessionToken,
  });
}

export async function openLivePlaybackSession(
  id: string
): Promise<OpenPlaybackSessionResponse> {
  return invoke<OpenPlaybackSessionResponse>("open_live_playback_session", {
    id,
  });
}

export async function closeLivePlaybackSession(
  id: string,
  sessionToken: string
): Promise<void> {
  return invoke("close_live_playback_session", {
    id,
    sessionToken,
  });
}

export async function mergeTsFiles(
  inputDir: string,
  outputPath: string
): Promise<string> {
  return invoke<string>("merge_ts_files", { inputDir, outputPath });
}

export async function convertTsToMp4File(
  inputPath: string,
  outputPath: string
): Promise<string> {
  return invoke<string>("convert_ts_to_mp4_file", { inputPath, outputPath });
}

export async function convertLocalM3u8ToMp4File(
  inputPath: string,
  outputPath: string
): Promise<string> {
  return invoke<string>("convert_local_m3u8_to_mp4_file", { inputPath, outputPath });
}

export async function convertMediaFile(
  inputPath: string,
  outputPath: string,
  targetFormat: string,
  convertMode: string
): Promise<string> {
  return invoke<string>("convert_media_file", {
    inputPath,
    outputPath,
    targetFormat,
    convertMode,
  });
}

export async function analyzeMediaFile(
  inputPath: string
): Promise<MediaAnalysisResult> {
  return invoke<MediaAnalysisResult>("analyze_media_file", { inputPath });
}

export async function clipVideoFile(
  inputPath: string,
  outputPath: string,
  startSeconds: number,
  endSeconds: number,
  clipMode: "fast" | "precise"
): Promise<string> {
  return invoke<string>("clip_video_file", {
    inputPath,
    outputPath,
    startSeconds,
    endSeconds,
    clipMode,
  });
}

export async function transcodeMediaFile(
  inputPath: string,
  outputPath: string,
  outputFormat: string,
  videoCodec: string,
  audioCodec: string
): Promise<string> {
  return invoke<string>("transcode_media_file", {
    inputPath,
    outputPath,
    outputFormat,
    videoCodec,
    audioCodec,
  });
}

export async function mergeVideoFiles(
  inputPaths: string[],
  outputPath: string,
  mergeMode: string
): Promise<string> {
  return invoke<string>("merge_video_files", {
    inputPaths,
    outputPath,
    mergeMode,
  });
}

export async function convertMultiTrackHlsToMp4Dir(
  inputDir: string,
  outputPath: string
): Promise<string> {
  return invoke<string>("convert_multi_track_hls_to_mp4_dir", {
    inputDir,
    outputPath,
  });
}

export async function getFfmpegStatus(): Promise<FfmpegStatus> {
  return invoke<FfmpegStatus>("get_ffmpeg_status");
}

export async function downloadFfmpeg(): Promise<string> {
  return invoke<string>("download_ffmpeg");
}

export async function setFfmpegPath(
  path: string | null
): Promise<FfmpegStatus> {
  return invoke<FfmpegStatus>("set_ffmpeg_path", { path });
}

export async function setFfmpegEnabled(enabled: boolean): Promise<void> {
  return invoke("set_ffmpeg_enabled", { enabled });
}

export async function openUrl(url: string): Promise<void> {
  return invoke("open_url", { url });
}

export interface PreviewThumbnail {
  index: number;
  time_secs: number;
  path: string;
}

export async function createPreviewSession(
  url: string,
  extraHeaders?: string,
  sourceKind?: DownloadSourceKind,
  sourceText?: string
): Promise<{ token: string; window_label: string }> {
  return invoke<{ token: string; window_label: string }>("create_preview_session", {
    url,
    extraHeaders: extraHeaders ?? null,
    sourceKind: sourceKind ?? null,
    sourceText: sourceText ?? null,
  });
}

export async function extractPreviewThumbnails(
  token: string,
  count: number,
  targetWidth: number,
  jpegQuality: number,
  runId: string,
  forceRefresh = false
): Promise<PreviewThumbnail[]> {
  return invoke<PreviewThumbnail[]>("extract_preview_thumbnails", {
    token,
    count,
    targetWidth,
    jpegQuality,
    runId,
    forceRefresh,
  });
}

export async function cancelPreviewThumbnails(
  token: string,
  runId: string
): Promise<void> {
  return invoke("cancel_preview_thumbnails", { token, runId });
}

export async function closePreviewSession(token: string): Promise<void> {
  return invoke("close_preview_session", { token });
}

// ===================== Live recording =====================

export async function createLiveRecord(
  params: CreateLiveRecordParams
): Promise<LiveRecordSummary> {
  return invoke<LiveRecordSummary>("create_live_record", { params });
}

export async function pauseLiveRecord(id: string): Promise<void> {
  return invoke("pause_live_record", { id });
}

export async function resumeLiveRecord(id: string): Promise<LiveRecordSummary> {
  return invoke<LiveRecordSummary>("resume_live_record", { id });
}

export async function stopLiveRecord(id: string): Promise<void> {
  return invoke("stop_live_record", { id });
}

export async function cancelLiveRecord(id: string): Promise<void> {
  return invoke("cancel_live_record", { id });
}

export async function removeLiveRecord(
  id: string,
  deleteFile: boolean
): Promise<void> {
  return invoke("remove_live_record", { id, deleteFile });
}

export async function clearLiveHistory(): Promise<void> {
  return invoke("clear_live_history");
}

export async function getLiveRecordCounts(): Promise<LiveRecordCounts> {
  return invoke<LiveRecordCounts>("get_live_record_counts");
}

export async function getLiveRecordsPage(
  group: LiveGroup,
  page: number,
  pageSize: number
): Promise<LiveRecordPage> {
  return invoke<LiveRecordPage>("get_live_records_page", {
    group,
    page,
    pageSize,
  });
}

export async function convertLiveHlsToMp4(id: string): Promise<string> {
  return invoke<string>("convert_live_hls_to_mp4", { id });
}

export async function checkForUpdate(): Promise<UpdateInfo> {
  return invoke<UpdateInfo>("check_for_update");
}

export async function downloadUpdateInstaller(
  asset: UpdateAsset
): Promise<string> {
  return invoke<string>("download_update_installer", { asset });
}

export async function openUpdateInstaller(path: string): Promise<void> {
  return invoke("open_update_installer", { path });
}
