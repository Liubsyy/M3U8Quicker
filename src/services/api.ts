import { invoke } from "@tauri-apps/api/core";
import type {
  ChromiumBrowser,
  ChromiumExtensionInstallResult,
  FirefoxExtensionInstallResult,
  CreateDownloadParams,
  DownloadCounts,
  DownloadGroup,
  DownloadTaskPage,
  DownloadTaskSegmentState,
  DownloadTaskSummary,
  OpenPlaybackSessionResponse,
} from "../types";
import type { AppSettings, ProxySettings } from "../types/settings";

export async function createDownload(
  params: CreateDownloadParams
): Promise<DownloadTaskSummary> {
  return invoke<DownloadTaskSummary>("create_download", { params });
}

export async function cancelDownload(id: string): Promise<void> {
  return invoke("cancel_download", { id });
}

export async function pauseDownload(id: string): Promise<void> {
  return invoke("pause_download", { id });
}

export async function resumeDownload(id: string): Promise<DownloadTaskSummary> {
  return invoke<DownloadTaskSummary>("resume_download", { id });
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
