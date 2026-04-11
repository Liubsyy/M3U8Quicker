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

export type DownloadMode = "hls" | "direct";

export type HlsOutputMode = "single_stream" | "multi_track_bundle";

export type HlsPlaylistKind = "media" | "master";

export type HlsTrackType = "video" | "audio" | "subtitle";

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
  hls_output_mode: HlsOutputMode;
  hls_selection: HlsTrackSelection | null;
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
  playback_available: boolean;
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
  download_mode?: DownloadMode;
  file_type?: FileType;
  hls_selection?: HlsTrackSelection;
}

export interface HlsTrackSelection {
  video_id?: string;
  audio_id?: string;
  subtitle_id?: string;
}

export interface HlsTrackOption {
  id: string;
  track_type: HlsTrackType;
  label: string;
  name: string | null;
  language: string | null;
  group_id: string | null;
  audio_group_id: string | null;
  subtitle_group_id: string | null;
  bandwidth: number | null;
  resolution: string | null;
  codecs: string | null;
  is_default: boolean;
  is_autoselect: boolean;
  is_forced: boolean;
}

export interface InspectHlsTracksParams {
  url: string;
  extra_headers?: string;
}

export interface InspectHlsTracksResult {
  kind: HlsPlaylistKind;
  requires_selection: boolean;
  video_tracks: HlsTrackOption[];
  audio_tracks: HlsTrackOption[];
  subtitle_tracks: HlsTrackOption[];
  default_selection: HlsTrackSelection;
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

export function inferDirectFileTypeFromUrl(
  url: string | null | undefined
): DirectFileType | undefined {
  if (!url) {
    return undefined;
  }

  const candidates: string[] = [];
  const rawUrl = url.trim();

  try {
    const parsed = new URL(rawUrl);
    candidates.push(parsed.pathname);

    for (const key of ["filename", "file", "name", "title", "videoTitle"]) {
      const value = parsed.searchParams.get(key);
      if (value) {
        candidates.push(value);
      }
    }
  } catch {
    candidates.push(rawUrl);
  }

  candidates.push(rawUrl);

  for (const candidate of candidates) {
    const match = candidate.match(/\.(mp4|mkv|avi|wmv|flv|webm|mov|rmvb)(?:$|[?#])/i);
    const fileType = parseFileType(match?.[1]);
    if (fileType && isDirectFileType(fileType)) {
      return fileType;
    }
  }

  return undefined;
}

export function getFileTypeLabel(fileType: FileType): string {
  return fileType === "hls" ? "HLS" : fileType.toUpperCase();
}

export function supportsProgressivePlayback(fileType: FileType): boolean {
  return fileType === "mp4" || fileType === "webm";
}

export function canOpenInProgressPlayback(
  task: Pick<DownloadTaskSummary, "file_type" | "status" | "playback_available">
): boolean {
  if (!task.playback_available) {
    return false;
  }

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
