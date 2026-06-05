export type ThemeMode = "light" | "dark";

export const THEME_MODE_STORAGE_KEY = "m3u8quicker.themeMode";
export const DEFAULT_HISTORY_PAGE_SIZE = 50;
export const HISTORY_PAGE_SIZE_OPTIONS = [10, 20, 50, 100, 200] as const;

export const ZOOM_STORAGE_KEY = "m3u8quicker.zoom";
export const DEFAULT_ZOOM = 1;
export const MIN_ZOOM = 0.5; // 50%
export const MAX_ZOOM = 2; // 200%
export const ZOOM_STEP = 0.1;

export function normalizeZoom(value: number): number {
  if (!Number.isFinite(value)) return DEFAULT_ZOOM;
  const rounded = Math.round(value * 100) / 100; // 消除浮点累积误差
  return Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, rounded));
}

export interface ProxySettings {
  enabled: boolean;
  url: string;
}

export interface AppSettings {
  default_download_dir: string | null;
  proxy: ProxySettings;
  download_concurrency: number;
  download_speed_limit_kbps: number;
  preview_columns: number;
  preview_count: number;
  preview_thumbnail_width: number;
  preview_jpeg_quality: number;
  delete_ts_temp_dir_after_download: boolean;
  convert_to_mp4: boolean;
  ffmpeg_enabled: boolean;
  ffmpeg_path: string | null;
  user_agent: string;
  metadata_timeout_secs: number;
  segment_timeout_secs: number;
  mp4_timeout_secs: number;
  hls_refresh_min_ms: number;
  hls_refresh_max_ms: number;
  hls_playlist_timeout_secs: number;
  live_segment_timeout_secs: number;
  live_retry_hls_ms: number;
  live_retry_flv_ms: number;
  history_page_size: number;
  close_to_tray: boolean;
}

export interface FfprobeInfo {
  path: string;
  version: string;
}

export interface FfmpegBinaryInfo {
  path: string;
  version: string;
}

export type FfmpegStatus =
  | {
      kind: "not_installed";
      ffmpeg: FfmpegBinaryInfo | null;
      ffprobe: FfprobeInfo | null;
    }
  | {
      kind: "installed";
      path: string;
      version: string;
      ffprobe: FfprobeInfo;
    };

export interface FfmpegDownloadProgress {
  downloaded_bytes: number;
  total_bytes: number;
  stage: "downloading" | "unpacking" | "done";
}
