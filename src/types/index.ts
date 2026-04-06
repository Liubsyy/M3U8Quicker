export type DownloadStatus =
  | "Pending"
  | "Downloading"
  | "Paused"
  | "Merging"
  | "Converting"
  | "Completed"
  | { Failed: string }
  | "Cancelled";

export type DownloadGroup = "active" | "history";

export interface DownloadTaskSummary {
  id: string;
  filename: string;
  encryption_method: string | null;
  output_dir: string;
  status: DownloadStatus;
  total_segments: number;
  completed_segments: number;
  failed_segment_count: number;
  total_bytes: number;
  speed_bytes_per_sec: number;
  created_at: string;
  completed_at: string | null;
  updated_at: string;
  file_path: string | null;
}

export interface DownloadTaskSegmentState {
  id: string;
  total_segments: number;
  completed_segment_indices: number[];
  failed_segment_indices: number[];
  updated_at: string;
}

export interface DownloadCounts {
  active_count: number;
  history_count: number;
}

export interface DownloadTaskPage {
  items: DownloadTaskSummary[];
  total: number;
  page: number;
  page_size: number;
}

export interface DownloadProgressEvent {
  id: string;
  status: DownloadStatus;
  group: DownloadGroup;
  completed_segments: number;
  total_segments: number;
  failed_segment_count: number;
  total_bytes: number;
  speed_bytes_per_sec: number;
  percentage: number;
  updated_at: string;
}

export interface CreateDownloadParams {
  url: string;
  filename?: string;
  output_dir?: string;
  extra_headers?: string;
}

export interface OpenPlaybackSessionResponse {
  window_label: string;
  playback_url: string;
  playback_kind: PlaybackSourceKind;
  session_token: string;
  filename: string;
  status: DownloadStatus;
}

export type PlaybackSourceKind = "hls" | "file";

export type ChromiumBrowser = "chrome" | "edge";

export interface ChromiumExtensionInstallResult {
  extension_path: string;
  manual_url: string;
}

export interface FirefoxExtensionInstallResult {
  extension_path: string;
  manual_url: string;
}

export function deriveFilenameFromUrl(url: string): string {
  try {
    const parsed = new URL(url.trim());
    const queryKeys = ["title", "name", "filename", "file", "videoTitle"];

    const rawName =
      queryKeys
        .map((key) => parsed.searchParams.get(key))
        .find((value) => value && value.trim()) ??
      parsed.pathname.split("/").filter(Boolean).at(-1) ??
      "";

    return normalizeDownloadFilename(rawName);
  } catch {
    return "";
  }
}

function normalizeDownloadFilename(name: string): string {
  const trimmed = name.trim();
  if (!trimmed) return "";

  const sanitized = Array.from(trimmed)
    .map((char) =>
      /[<>:"/\\|?*]/.test(char) || char.charCodeAt(0) <= 0x1f ? "_" : char
    )
    .join("")
    .replace(/^\.+|\.+$/g, "")
    .trim();

  if (!sanitized) return "";

  const lower = sanitized.toLowerCase();
  if (lower.endsWith(".m3u8")) {
    return sanitized.slice(0, -5);
  }
  if (lower.endsWith(".mp4")) {
    return sanitized.slice(0, -4);
  }
  if (lower.endsWith(".ts")) {
    return sanitized.slice(0, -3);
  }
  return sanitized;
}
