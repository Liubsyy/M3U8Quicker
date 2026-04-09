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

export type DirectFileType =
  | "mp4"
  | "mkv"
  | "avi"
  | "wmv"
  | "flv"
  | "webm"
  | "mov"
  | "rmvb";

export type FileType = "hls" | DirectFileType;

export const DIRECT_FILE_TYPES: DirectFileType[] = [
  "mp4",
  "mkv",
  "avi",
  "wmv",
  "flv",
  "webm",
  "mov",
  "rmvb",
];

export const FILE_TYPE_OPTIONS: Array<{ value: FileType; label: string }> = [
  { value: "hls", label: "HLS" },
  ...DIRECT_FILE_TYPES.map((value) => ({
    value,
    label: value.toUpperCase(),
  })),
];

export interface DownloadTaskSummary {
  id: string;
  filename: string;
  file_type: FileType;
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

export type ResumeDownloadAction = "resume" | "confirm_restart";

export interface ResumeDownloadCheckResult {
  action: ResumeDownloadAction;
  downloaded_bytes: number;
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
  file_type?: FileType;
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

export function isDirectFileType(
  fileType: FileType | null | undefined
): fileType is DirectFileType {
  return fileType !== undefined && fileType !== null && fileType !== "hls";
}

export function parseFileType(value: string | null | undefined): FileType | undefined {
  if (!value) {
    return undefined;
  }

  const normalized = value.trim().toLowerCase();
  if (normalized === "hls") {
    return "hls";
  }

  return DIRECT_FILE_TYPES.find((fileType) => fileType === normalized);
}

export function getFileTypeLabel(fileType: FileType): string {
  return fileType === "hls" ? "HLS" : fileType.toUpperCase();
}

export function supportsProgressivePlayback(fileType: FileType): boolean {
  return fileType === "mp4" || fileType === "webm";
}

export function canOpenInProgressPlayback(task: Pick<DownloadTaskSummary, "file_type" | "status">): boolean {
  const isInProgress =
    task.status === "Downloading" || task.status === "Paused";

  if (!isInProgress) {
    return task.status === "Completed";
  }

  return task.file_type === "hls" || supportsProgressivePlayback(task.file_type);
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
  if (lower.endsWith(".ts")) {
    return sanitized.slice(0, -3);
  }

  for (const fileType of DIRECT_FILE_TYPES) {
    const suffix = `.${fileType}`;
    if (lower.endsWith(suffix)) {
      return sanitized.slice(0, -suffix.length);
    }
  }

  return sanitized;
}
