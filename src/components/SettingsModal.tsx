import {
  useEffect,
  useRef,
  useState,
  type Dispatch,
  type ReactNode,
  type SetStateAction,
} from "react";
import {
  Badge,
  Button,
  Card,
  Input,
  InputNumber,
  Modal,
  Progress,
  Radio,
  Segmented,
  Select,
  Space,
  Switch,
  Tabs,
  Tag,
  Tooltip,
  Typography,
  message,
  theme,
} from "antd";
import {
  CheckCircleFilled,
  CloseCircleFilled,
  DashboardOutlined,
  GithubOutlined,
  ReloadOutlined,
  ThunderboltOutlined,
  ZoomInOutlined,
  ZoomOutOutlined,
} from "@ant-design/icons";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  downloadFfmpeg,
  getAppSettings,
  getDefaultDownloadDir,
  getFfmpegStatus,
  setCloseToTray,
  setDefaultDownloadDir,
  setDownloadConcurrency,
  setDownloadOutputSettings,
  setDownloadSpeedLimit,
  setFfmpegEnabled,
  setFfmpegPath,
  setHistoryPageSize,
  setLiveRecordSettings,
  setProxySettings,
  setTimeoutSettings,
  setUserAgent,
  openUrl,
} from "../services/api";
import {
  DEFAULT_HISTORY_PAGE_SIZE,
  DEFAULT_ZOOM,
  HISTORY_PAGE_SIZE_OPTIONS,
  MAX_ZOOM,
  MIN_ZOOM,
  normalizeZoom,
  ZOOM_STEP,
} from "../types/settings";
import type {
  FfmpegDownloadProgress,
  FfmpegStatus,
  ProxySettings,
  ThemeMode,
} from "../types/settings";
import { UpdateModal } from "./UpdateModal";

const MIN_DOWNLOAD_CONCURRENCY = 1;
const MAX_DOWNLOAD_CONCURRENCY = 64;
const DEFAULT_LIMITED_DOWNLOAD_SPEED_KBPS = 1024;

const SPEED_LIMIT_PRESETS: { label: string; value: number }[] = [
  { label: "512 KB/s", value: 512 },
  { label: "1 MB/s", value: 1024 },
  { label: "2 MB/s", value: 2048 },
  { label: "5 MB/s", value: 5120 },
  { label: "10 MB/s", value: 10240 },
];

const HISTORY_PAGE_SIZE_SELECT_OPTIONS = HISTORY_PAGE_SIZE_OPTIONS.map((value) => ({
  label: `${value}`,
  value,
}));

// 与后端 models.rs 的取值范围保持一致：仅约束下限（最小 1），无上限。
const MIN_METADATA_TIMEOUT_SECS = 1;
const MIN_SEGMENT_TIMEOUT_SECS = 1;
const MIN_MP4_TIMEOUT_SECS = 1;
const MIN_HLS_REFRESH_MIN_MS = 1;
const MIN_HLS_REFRESH_MAX_MS = 1;
const MIN_HLS_PLAYLIST_TIMEOUT_SECS = 1;
const MIN_LIVE_SEGMENT_TIMEOUT_SECS = 1;
const MIN_LIVE_RETRY_HLS_MS = 1;
const MIN_LIVE_RETRY_FLV_MS = 1;

function clampInt(value: number, min: number): number {
  return Math.max(min, Math.trunc(value));
}

function formatSpeedKbps(kbps: number | null): string {
  if (kbps === null || kbps <= 0) return "未设置";
  if (kbps >= 1024) {
    const mb = kbps / 1024;
    return `${Number.isInteger(mb) ? mb : mb.toFixed(1)} MB/s`;
  }
  return `${kbps} KB/s`;
}

type SpeedLimitMode = "unlimited" | "limited";

interface SettingsModalProps {
  open: boolean;
  initialTab?: "general" | "network" | "download" | "live" | "ffmpeg" | "about";
  themeMode: ThemeMode;
  zoomFactor: number;
  updateAvailable?: boolean;
  historyPageSize?: number;
  onClose: () => void;
  onThemeModeChange: (mode: ThemeMode) => void;
  onZoomChange: Dispatch<SetStateAction<number>>;
  onHistoryPageSizeChange?: (pageSize: number) => void;
  onUpdateAvailabilityChange?: (available: boolean) => void;
}

function SectionTitle({ children }: { children: ReactNode }) {
  return (
    <Typography.Text
      strong
      style={{ fontSize: 15, lineHeight: 1.5, letterSpacing: 0.2 }}
    >
      {children}
    </Typography.Text>
  );
}

export function SettingsModal({
  open,
  initialTab = "general",
  themeMode,
  zoomFactor,
  updateAvailable = false,
  historyPageSize = DEFAULT_HISTORY_PAGE_SIZE,
  onClose,
  onThemeModeChange,
  onZoomChange,
  onHistoryPageSizeChange,
  onUpdateAvailabilityChange,
}: SettingsModalProps) {
  const [activeTab, setActiveTab] = useState<
    "general" | "network" | "download" | "live" | "ffmpeg" | "about"
  >(initialTab);
  const [proxySettings, setProxySettingsState] = useState<ProxySettings | null>(
    null
  );
  const [downloadConcurrency, setDownloadConcurrencyState] = useState<
    number | null
  >(null);
  const [savedDownloadConcurrency, setSavedDownloadConcurrency] = useState<
    number | null
  >(null);
  const [speedLimitMode, setSpeedLimitMode] =
    useState<SpeedLimitMode>("unlimited");
  const [downloadSpeedLimitKbps, setDownloadSpeedLimitKbps] =
    useState<number | null>(DEFAULT_LIMITED_DOWNLOAD_SPEED_KBPS);
  const [savedDownloadSpeedLimitKbps, setSavedDownloadSpeedLimitKbps] =
    useState<number>(0);
  const [deleteTsTempDirAfterDownload, setDeleteTsTempDirAfterDownload] =
    useState(false);
  const [convertToMp4, setConvertToMp4] = useState(true);
  const [loading, setLoading] = useState(false);
  const [savingProxy, setSavingProxy] = useState(false);
  const [savingConcurrency, setSavingConcurrency] = useState(false);
  const [savingSpeedLimit, setSavingSpeedLimit] = useState(false);
  const [savingDownloadOutput, setSavingDownloadOutput] = useState(false);
  const [ffmpegStatus, setFfmpegStatus] = useState<FfmpegStatus | null>(null);
  const [ffmpegEnabled, setFfmpegEnabledState] = useState(true);
  const [ffmpegDownloading, setFfmpegDownloading] = useState(false);
  const [ffmpegDownloadProgress, setFfmpegDownloadProgress] = useState<number>(0);
  const [ffmpegCustomPath, setFfmpegCustomPath] = useState("");
  const [savingFfmpegEnabled, setSavingFfmpegEnabled] = useState(false);
  const [appVersion, setAppVersion] = useState("");
  const [updateModalOpen, setUpdateModalOpen] = useState(false);
  const [historyPageSizeValue, setHistoryPageSizeValue] =
    useState(historyPageSize);
  const [savingHistoryPageSize, setSavingHistoryPageSize] = useState(false);
  // 关闭窗口时的行为：true=最小化到系统托盘，false=退出应用
  const [closeToTray, setCloseToTrayState] = useState(true);
  const [savingCloseToTray, setSavingCloseToTray] = useState(false);
  // 默认下载目录
  const [defaultDownloadDir, setDefaultDownloadDirState] = useState("");
  // 默认 User-Agent
  const [userAgent, setUserAgentState] = useState("");
  const [savedUserAgent, setSavedUserAgent] = useState("");
  const [savingUserAgent, setSavingUserAgent] = useState(false);
  // HTTP 超时（秒）
  const [metadataTimeoutSecs, setMetadataTimeoutSecs] = useState<number | null>(
    null
  );
  const [segmentTimeoutSecs, setSegmentTimeoutSecs] = useState<number | null>(
    null
  );
  const [mp4TimeoutSecs, setMp4TimeoutSecs] = useState<number | null>(null);
  const [savingTimeouts, setSavingTimeouts] = useState(false);
  // 录播设置
  const [hlsRefreshMinMs, setHlsRefreshMinMs] = useState<number | null>(null);
  const [hlsRefreshMaxMs, setHlsRefreshMaxMs] = useState<number | null>(null);
  const [hlsPlaylistTimeoutSecs, setHlsPlaylistTimeoutSecs] = useState<
    number | null
  >(null);
  const [liveSegmentTimeoutSecs, setLiveSegmentTimeoutSecs] = useState<
    number | null
  >(null);
  const [liveRetryHlsMs, setLiveRetryHlsMs] = useState<number | null>(null);
  const [liveRetryFlvMs, setLiveRetryFlvMs] = useState<number | null>(null);
  const [savingLiveSettings, setSavingLiveSettings] = useState(false);
  const ffmpegUnlistenRef = useRef<UnlistenFn | null>(null);
  const { token } = theme.useToken();

  useEffect(() => {
    if (!open) return;

    setActiveTab(initialTab);

    getVersion().then(setAppVersion);

    setLoading(true);
    getAppSettings()
      .then((settings) => {
        setProxySettingsState(settings.proxy);
        setDownloadConcurrencyState(settings.download_concurrency);
        setSavedDownloadConcurrency(settings.download_concurrency);
        setSavedDownloadSpeedLimitKbps(settings.download_speed_limit_kbps);
        setSpeedLimitMode(
          settings.download_speed_limit_kbps > 0 ? "limited" : "unlimited"
        );
        setDownloadSpeedLimitKbps(
          settings.download_speed_limit_kbps > 0
            ? settings.download_speed_limit_kbps
            : DEFAULT_LIMITED_DOWNLOAD_SPEED_KBPS
        );
        setDeleteTsTempDirAfterDownload(
          settings.delete_ts_temp_dir_after_download
        );
        setConvertToMp4(settings.convert_to_mp4);
        setFfmpegEnabledState(settings.ffmpeg_enabled);
        setFfmpegCustomPath(settings.ffmpeg_path ?? "");
        setUserAgentState(settings.user_agent);
        setSavedUserAgent(settings.user_agent);
        setMetadataTimeoutSecs(settings.metadata_timeout_secs);
        setSegmentTimeoutSecs(settings.segment_timeout_secs);
        setMp4TimeoutSecs(settings.mp4_timeout_secs);
        setHlsRefreshMinMs(settings.hls_refresh_min_ms);
        setHlsRefreshMaxMs(settings.hls_refresh_max_ms);
        setHlsPlaylistTimeoutSecs(settings.hls_playlist_timeout_secs);
        setLiveSegmentTimeoutSecs(settings.live_segment_timeout_secs);
        setLiveRetryHlsMs(settings.live_retry_hls_ms);
        setLiveRetryFlvMs(settings.live_retry_flv_ms);
        setHistoryPageSizeValue(settings.history_page_size);
        onHistoryPageSizeChange?.(settings.history_page_size);
        setCloseToTrayState(settings.close_to_tray);
      })
      .catch((error) => {
        message.error(`读取设置失败：${formatSettingsError(error)}`);
      })
      .finally(() => setLoading(false));

    getDefaultDownloadDir().then(setDefaultDownloadDirState).catch(() => {});
    getFfmpegStatus().then(setFfmpegStatus).catch(() => {});
  }, [initialTab, onHistoryPageSizeChange, open]);

  useEffect(() => {
    setHistoryPageSizeValue(historyPageSize);
  }, [historyPageSize]);

  useEffect(() => {
    return () => {
      ffmpegUnlistenRef.current?.();
    };
  }, []);

  const updateProxy = async (nextProxy: ProxySettings) => {
    setProxySettingsState(nextProxy);
    setSavingProxy(true);

    try {
      await setProxySettings(nextProxy);
      message.success("代理设置已保存");
    } catch (error) {
      message.error(`保存代理设置失败：${formatSettingsError(error)}`);
      const settings = await getAppSettings();
      setProxySettingsState(settings.proxy);
    } finally {
      setSavingProxy(false);
    }
  };

  const saveDownloadConcurrencyValue = async (value: number) => {
    const normalizedValue = Math.max(
      MIN_DOWNLOAD_CONCURRENCY,
      Math.min(MAX_DOWNLOAD_CONCURRENCY, Math.trunc(value))
    );

    if (savedDownloadConcurrency === normalizedValue) {
      setDownloadConcurrencyState(normalizedValue);
      return;
    }

    setDownloadConcurrencyState(normalizedValue);
    setSavingConcurrency(true);

    try {
      await setDownloadConcurrency(normalizedValue);
      setSavedDownloadConcurrency(normalizedValue);
      message.success("下载并发数量已保存");
    } catch (error) {
      message.error(`保存下载并发数量失败：${formatSettingsError(error)}`);
      const settings = await getAppSettings();
      setDownloadConcurrencyState(settings.download_concurrency);
      setSavedDownloadConcurrency(settings.download_concurrency);
    } finally {
      setSavingConcurrency(false);
    }
  };

  const saveDownloadSpeedLimitValue = async (value: number | null) => {
    const normalizedValue = Math.max(
      1,
      Math.trunc(value ?? DEFAULT_LIMITED_DOWNLOAD_SPEED_KBPS)
    );

    if (
      speedLimitMode === "limited" &&
      savedDownloadSpeedLimitKbps === normalizedValue
    ) {
      setDownloadSpeedLimitKbps(normalizedValue);
      return;
    }

    setSpeedLimitMode("limited");
    setDownloadSpeedLimitKbps(normalizedValue);
    setSavingSpeedLimit(true);

    try {
      await setDownloadSpeedLimit(normalizedValue);
      setSavedDownloadSpeedLimitKbps(normalizedValue);
      message.success("下载限速已保存");
    } catch (error) {
      message.error(`保存下载限速失败：${formatSettingsError(error)}`);
      const settings = await getAppSettings();
      setSavedDownloadSpeedLimitKbps(settings.download_speed_limit_kbps);
      setSpeedLimitMode(
        settings.download_speed_limit_kbps > 0 ? "limited" : "unlimited"
      );
      setDownloadSpeedLimitKbps(
        settings.download_speed_limit_kbps > 0
          ? settings.download_speed_limit_kbps
          : DEFAULT_LIMITED_DOWNLOAD_SPEED_KBPS
      );
    } finally {
      setSavingSpeedLimit(false);
    }
  };

  const updateSpeedLimitMode = async (nextMode: SpeedLimitMode) => {
    if (nextMode === speedLimitMode) return;

    if (nextMode === "unlimited") {
      setSpeedLimitMode("unlimited");
      setSavingSpeedLimit(true);
      try {
        await setDownloadSpeedLimit(0);
        setSavedDownloadSpeedLimitKbps(0);
        message.success("下载限速已关闭");
      } catch (error) {
        message.error(`保存下载限速失败：${formatSettingsError(error)}`);
        const settings = await getAppSettings();
        setSavedDownloadSpeedLimitKbps(settings.download_speed_limit_kbps);
        setSpeedLimitMode(
          settings.download_speed_limit_kbps > 0 ? "limited" : "unlimited"
        );
        setDownloadSpeedLimitKbps(
          settings.download_speed_limit_kbps > 0
            ? settings.download_speed_limit_kbps
            : DEFAULT_LIMITED_DOWNLOAD_SPEED_KBPS
        );
      } finally {
        setSavingSpeedLimit(false);
      }
      return;
    }

    await saveDownloadSpeedLimitValue(
      downloadSpeedLimitKbps !== null && downloadSpeedLimitKbps > 0
        ? downloadSpeedLimitKbps
        : DEFAULT_LIMITED_DOWNLOAD_SPEED_KBPS
    );
  };

  const updateDownloadOutputSettings = async (
    nextDeleteTsTempDirAfterDownload: boolean,
    nextConvertToMp4: boolean
  ) => {
    setDeleteTsTempDirAfterDownload(nextDeleteTsTempDirAfterDownload);
    setConvertToMp4(nextConvertToMp4);
    setSavingDownloadOutput(true);

    try {
      await setDownloadOutputSettings(
        nextDeleteTsTempDirAfterDownload,
        nextConvertToMp4
      );
      message.success("下载完成行为已保存");
    } catch (error) {
      message.error(`保存下载完成行为失败：${formatSettingsError(error)}`);
      const settings = await getAppSettings();
      setDeleteTsTempDirAfterDownload(
        settings.delete_ts_temp_dir_after_download
      );
      setConvertToMp4(settings.convert_to_mp4);
    } finally {
      setSavingDownloadOutput(false);
    }
  };

  const handleSelectDefaultDownloadDir = async () => {
    const selected = await openDialog({ multiple: false, directory: true });
    if (!selected) return;
    const selectedPath = selected as string;
    try {
      await setDefaultDownloadDir(selectedPath);
      setDefaultDownloadDirState(selectedPath);
      message.success("默认下载目录已保存");
    } catch (error) {
      message.error(`保存默认下载目录失败：${formatSettingsError(error)}`);
    }
  };

  const saveUserAgentValue = async () => {
    const normalized = userAgent.trim();
    if (normalized === savedUserAgent) return;
    setSavingUserAgent(true);
    try {
      await setUserAgent(normalized);
      const settings = await getAppSettings();
      setUserAgentState(settings.user_agent);
      setSavedUserAgent(settings.user_agent);
      message.success("User-Agent 已保存");
    } catch (error) {
      message.error(`保存 User-Agent 失败：${formatSettingsError(error)}`);
      setUserAgentState(savedUserAgent);
    } finally {
      setSavingUserAgent(false);
    }
  };

  const saveHistoryPageSizeValue = async (nextPageSize: number) => {
    setHistoryPageSizeValue(nextPageSize);
    setSavingHistoryPageSize(true);
    try {
      await setHistoryPageSize(nextPageSize);
      onHistoryPageSizeChange?.(nextPageSize);
      message.success("每页展示已保存");
    } catch (error) {
      message.error(`保存每页展示失败：${formatSettingsError(error)}`);
      const settings = await getAppSettings();
      setHistoryPageSizeValue(settings.history_page_size);
      onHistoryPageSizeChange?.(settings.history_page_size);
    } finally {
      setSavingHistoryPageSize(false);
    }
  };

  const saveCloseToTrayValue = async (next: boolean) => {
    if (next === closeToTray) return;
    const previous = closeToTray;
    setCloseToTrayState(next);
    setSavingCloseToTray(true);
    try {
      await setCloseToTray(next);
    } catch (error) {
      message.error(`保存关闭窗口行为失败：${formatSettingsError(error)}`);
      setCloseToTrayState(previous);
    } finally {
      setSavingCloseToTray(false);
    }
  };

  const saveTimeoutSettingsValues = async () => {
    const metadata = clampInt(
      metadataTimeoutSecs ?? MIN_METADATA_TIMEOUT_SECS,
      MIN_METADATA_TIMEOUT_SECS
    );
    const segment = clampInt(
      segmentTimeoutSecs ?? MIN_SEGMENT_TIMEOUT_SECS,
      MIN_SEGMENT_TIMEOUT_SECS
    );
    const mp4 = clampInt(
      mp4TimeoutSecs ?? MIN_MP4_TIMEOUT_SECS,
      MIN_MP4_TIMEOUT_SECS
    );

    setMetadataTimeoutSecs(metadata);
    setSegmentTimeoutSecs(segment);
    setMp4TimeoutSecs(mp4);
    setSavingTimeouts(true);
    try {
      await setTimeoutSettings(metadata, segment, mp4);
      message.success("请求超时已保存");
    } catch (error) {
      message.error(`保存请求超时失败：${formatSettingsError(error)}`);
      const settings = await getAppSettings();
      setMetadataTimeoutSecs(settings.metadata_timeout_secs);
      setSegmentTimeoutSecs(settings.segment_timeout_secs);
      setMp4TimeoutSecs(settings.mp4_timeout_secs);
    } finally {
      setSavingTimeouts(false);
    }
  };

  const saveLiveRecordSettingsValues = async () => {
    const refreshMin = clampInt(
      hlsRefreshMinMs ?? MIN_HLS_REFRESH_MIN_MS,
      MIN_HLS_REFRESH_MIN_MS
    );
    let refreshMax = clampInt(
      hlsRefreshMaxMs ?? MIN_HLS_REFRESH_MAX_MS,
      MIN_HLS_REFRESH_MAX_MS
    );
    if (refreshMax < refreshMin) refreshMax = refreshMin;
    const playlistTimeout = clampInt(
      hlsPlaylistTimeoutSecs ?? MIN_HLS_PLAYLIST_TIMEOUT_SECS,
      MIN_HLS_PLAYLIST_TIMEOUT_SECS
    );
    const segmentTimeout = clampInt(
      liveSegmentTimeoutSecs ?? MIN_LIVE_SEGMENT_TIMEOUT_SECS,
      MIN_LIVE_SEGMENT_TIMEOUT_SECS
    );
    const retryHls = clampInt(
      liveRetryHlsMs ?? MIN_LIVE_RETRY_HLS_MS,
      MIN_LIVE_RETRY_HLS_MS
    );
    const retryFlv = clampInt(
      liveRetryFlvMs ?? MIN_LIVE_RETRY_FLV_MS,
      MIN_LIVE_RETRY_FLV_MS
    );

    setHlsRefreshMinMs(refreshMin);
    setHlsRefreshMaxMs(refreshMax);
    setHlsPlaylistTimeoutSecs(playlistTimeout);
    setLiveSegmentTimeoutSecs(segmentTimeout);
    setLiveRetryHlsMs(retryHls);
    setLiveRetryFlvMs(retryFlv);
    setSavingLiveSettings(true);
    try {
      await setLiveRecordSettings(
        refreshMin,
        refreshMax,
        playlistTimeout,
        segmentTimeout,
        retryHls,
        retryFlv
      );
      message.success("录播设置已保存");
    } catch (error) {
      message.error(`保存录播设置失败：${formatSettingsError(error)}`);
      const settings = await getAppSettings();
      setHlsRefreshMinMs(settings.hls_refresh_min_ms);
      setHlsRefreshMaxMs(settings.hls_refresh_max_ms);
      setHlsPlaylistTimeoutSecs(settings.hls_playlist_timeout_secs);
      setLiveSegmentTimeoutSecs(settings.live_segment_timeout_secs);
      setLiveRetryHlsMs(settings.live_retry_hls_ms);
      setLiveRetryFlvMs(settings.live_retry_flv_ms);
    } finally {
      setSavingLiveSettings(false);
    }
  };

  const handleDownloadFfmpeg = async () => {
    setFfmpegDownloading(true);
    setFfmpegDownloadProgress(0);

    const unlisten = await listen<FfmpegDownloadProgress>(
      "ffmpeg-download-progress",
      (event) => {
        const { total_bytes, downloaded_bytes, stage } = event.payload;
        if (stage === "downloading" && total_bytes > 0) {
          setFfmpegDownloadProgress(
            Math.round((downloaded_bytes / total_bytes) * 90)
          );
        } else if (stage === "unpacking") {
          setFfmpegDownloadProgress(95);
        } else if (stage === "done") {
          setFfmpegDownloadProgress(100);
        }
      }
    );
    ffmpegUnlistenRef.current = unlisten;

    try {
      await downloadFfmpeg();
      message.success("FFmpeg 下载完成");
      const status = await getFfmpegStatus();
      setFfmpegStatus(status);
    } catch (error) {
      message.error(`FFmpeg 下载失败：${String(error)}`);
    } finally {
      setFfmpegDownloading(false);
      setFfmpegDownloadProgress(0);
      unlisten();
      ffmpegUnlistenRef.current = null;
    }
  };

  const handleSetFfmpegCustomPath = async () => {
    const selected = await openDialog({
      multiple: false,
      filters: [{ name: "FFmpeg", extensions: ["exe", "*"] }],
    });
    if (!selected) return;

    const filePath = typeof selected === "string" ? selected : selected;
    setFfmpegCustomPath(filePath);
    try {
      const status = await setFfmpegPath(filePath);
      setFfmpegStatus(status);
      if (status.kind === "installed") {
        message.success("FFmpeg 路径已保存");
      } else {
        message.warning("所选文件不是有效的 FFmpeg");
      }
    } catch (error) {
      message.error(`设置 FFmpeg 路径失败：${String(error)}`);
    }
  };

  const handleResetFfmpegPath = async () => {
    setFfmpegCustomPath("");
    try {
      const status = await setFfmpegPath(null);
      setFfmpegStatus(status);
      message.success("已重置为自动检测");
    } catch (error) {
      message.error(`重置 FFmpeg 路径失败：${String(error)}`);
    }
  };

  const handleSetFfmpegEnabled = async (enabled: boolean) => {
    setFfmpegEnabledState(enabled);
    setSavingFfmpegEnabled(true);
    try {
      await setFfmpegEnabled(enabled);
      message.success(enabled ? "FFmpeg 已开启" : "FFmpeg 已关闭");
    } catch (error) {
      message.error(`保存 FFmpeg 开关失败：${String(error)}`);
      const settings = await getAppSettings();
      setFfmpegEnabledState(settings.ffmpeg_enabled);
    } finally {
      setSavingFfmpegEnabled(false);
    }
  };

  const handleConfirm = async () => {
    if (
      downloadConcurrency !== null &&
      downloadConcurrency !== savedDownloadConcurrency
    ) {
      await saveDownloadConcurrencyValue(downloadConcurrency);
    }
    if (
      speedLimitMode === "limited" &&
      downloadSpeedLimitKbps !== savedDownloadSpeedLimitKbps
    ) {
      await saveDownloadSpeedLimitValue(downloadSpeedLimitKbps);
    }

    onClose();
  };

  const settingsTabItems = [
    {
      key: "general",
      label: "常规",
      children: (
        <Space direction="vertical" size={18} style={{ width: "100%" }}>
          <SectionTitle>主题</SectionTitle>
          <Radio.Group
            value={themeMode}
            onChange={(event) => onThemeModeChange(event.target.value)}
          >
            <Space size={20}>
              <Radio value="light">
                {themeMode === "light" ? "浅色（当前）" : "浅色"}
              </Radio>
              <Radio value="dark">
                {themeMode === "dark" ? "深色（当前）" : "深色"}
              </Radio>
            </Space>
          </Radio.Group>
          <Space direction="vertical" size={8}>
            <SectionTitle>界面缩放</SectionTitle>
            <Space size={12} align="center">
              <div
                style={{
                  display: "inline-flex",
                  alignItems: "center",
                  height: 36,
                  borderRadius: token.borderRadiusLG,
                  border: `1px solid ${token.colorBorder}`,
                  background: token.colorFillTertiary,
                  overflow: "hidden",
                }}
              >
                <Tooltip title="缩小">
                  <Button
                    type="text"
                    icon={<ZoomOutOutlined />}
                    disabled={zoomFactor <= MIN_ZOOM}
                    onClick={() =>
                      onZoomChange((z) => normalizeZoom(z - ZOOM_STEP))
                    }
                    style={{ height: 36, width: 40, borderRadius: 0 }}
                  />
                </Tooltip>
                <Typography.Text
                  style={{
                    minWidth: 56,
                    textAlign: "center",
                    fontWeight: 600,
                    fontVariantNumeric: "tabular-nums",
                    userSelect: "none",
                    borderLeft: `1px solid ${token.colorBorderSecondary}`,
                    borderRight: `1px solid ${token.colorBorderSecondary}`,
                    lineHeight: "36px",
                    padding: "0 4px",
                  }}
                >
                  {Math.round(zoomFactor * 100)}%
                </Typography.Text>
                <Tooltip title="放大">
                  <Button
                    type="text"
                    icon={<ZoomInOutlined />}
                    disabled={zoomFactor >= MAX_ZOOM}
                    onClick={() =>
                      onZoomChange((z) => normalizeZoom(z + ZOOM_STEP))
                    }
                    style={{ height: 36, width: 40, borderRadius: 0 }}
                  />
                </Tooltip>
              </div>
              <Button
                type="text"
                icon={<ReloadOutlined />}
                disabled={zoomFactor === DEFAULT_ZOOM}
                onClick={() => onZoomChange(DEFAULT_ZOOM)}
                style={{ color: token.colorTextSecondary }}
              >
                恢复默认
              </Button>
            </Space>
          </Space>
          <Space direction="vertical" size={8}>
            <SectionTitle>列表展示</SectionTitle>
            <Space size={8} align="center">
              <Typography.Text>每页展示</Typography.Text>
              <Select
                value={historyPageSizeValue}
                options={HISTORY_PAGE_SIZE_SELECT_OPTIONS}
                style={{ width: 88 }}
                disabled={loading || savingHistoryPageSize}
                onChange={(value) => void saveHistoryPageSizeValue(value)}
              />
              <Typography.Text>条</Typography.Text>
            </Space>
          </Space>
          <Space direction="vertical" size={8}>
            <SectionTitle>关闭窗口时</SectionTitle>
            <Radio.Group
              value={closeToTray ? "tray" : "exit"}
              disabled={loading || savingCloseToTray}
              onChange={(event) =>
                void saveCloseToTrayValue(event.target.value === "tray")
              }
            >
              <Space size={20}>
                <Radio value="tray">最小化到系统托盘</Radio>
                <Radio value="exit">退出应用</Radio>
              </Space>
            </Radio.Group>
          </Space>
        </Space>
      ),
    },
    {
      key: "network",
      label: "网络",
      children: (
        <Space direction="vertical" size={18} style={{ width: "100%" }}>
          <Space direction="vertical" size={8} style={{ width: "100%" }}>
            <SectionTitle>代理设置</SectionTitle>
            <Space style={{ width: "100%", justifyContent: "space-between" }}>
              <Typography.Text>启用代理</Typography.Text>
              <Switch
                checked={proxySettings?.enabled ?? false}
                loading={loading || savingProxy}
                onChange={(checked) =>
                  proxySettings &&
                  void updateProxy({ ...proxySettings, enabled: checked })
                }
              />
            </Space>
            <Input
              value={proxySettings?.url ?? ""}
              placeholder="请输入代理地址"
              disabled={!proxySettings || loading || savingProxy}
              onBlur={(event) => {
                if (!proxySettings) return;
                const nextUrl = event.target.value.trim();
                if (nextUrl === proxySettings.url) return;
                void updateProxy({ ...proxySettings, url: nextUrl });
              }}
              onChange={(event) =>
                proxySettings &&
                setProxySettingsState({
                  ...proxySettings,
                  url: event.target.value,
                })
              }
            />
          </Space>

          <Space direction="vertical" size={8} style={{ width: "100%" }}>
            <SectionTitle>默认 User-Agent</SectionTitle>
            <Input
              value={userAgent}
              placeholder="留空则使用默认 User-Agent"
              disabled={loading || savingUserAgent}
              onChange={(event) => setUserAgentState(event.target.value)}
              onBlur={() => void saveUserAgentValue()}
            />
            <Typography.Text type="secondary" style={{ fontSize: 12 }}>
              新建任务时若在「附加 Header」中填写 User-Agent，将优先生效并覆盖此默认值。
            </Typography.Text>
          </Space>
        </Space>
      ),
    },
    {
      key: "download",
      label: "下载设置",
      children: (
        <Space direction="vertical" size={18} style={{ width: "100%" }}>
          <Space direction="vertical" size={8} style={{ width: "100%" }}>
            <SectionTitle>默认下载目录</SectionTitle>
            <Space.Compact style={{ width: "calc(100% - 24px)" }}>
              <Input value={defaultDownloadDir} readOnly placeholder="尚未设置" />
              <Button onClick={() => void handleSelectDefaultDownloadDir()}>
                选择
              </Button>
            </Space.Compact>
          </Space>
          <Space direction="vertical" size={8} style={{ width: "100%" }}>
            <SectionTitle>下载并发数量</SectionTitle>
            <InputNumber
              min={MIN_DOWNLOAD_CONCURRENCY}
              max={MAX_DOWNLOAD_CONCURRENCY}
              precision={0}
              value={downloadConcurrency ?? undefined}
              style={{ width: 180 }}
              disabled={loading || savingConcurrency}
              placeholder="请输入下载并发数量"
              onChange={(value) =>
                setDownloadConcurrencyState(
                  typeof value === "number" ? value : null
                )
              }
              onBlur={() => {
                if (downloadConcurrency === null) {
                  setDownloadConcurrencyState(savedDownloadConcurrency);
                  return;
                }
                void saveDownloadConcurrencyValue(downloadConcurrency);
              }}
            />
          </Space>
          <Space direction="vertical" size={8} style={{ width: "100%" }}>
            <div
              style={{
                display: "flex",
                alignItems: "center",
                justifyContent: "space-between",
              }}
            >
              <SectionTitle>下载限速</SectionTitle>
              <Tag
                bordered={false}
                color={speedLimitMode === "unlimited" ? "success" : "processing"}
                style={{ marginInlineEnd: 0, fontWeight: 500 }}
              >
                {speedLimitMode === "unlimited"
                  ? "全速下载"
                  : `≤ ${formatSpeedKbps(downloadSpeedLimitKbps)}`}
              </Tag>
            </div>
            <div
              style={{
                padding: "14px 16px",
                borderRadius: 12,
                border: `1px solid ${token.colorBorderSecondary}`,
                background: token.colorFillQuaternary,
                display: "flex",
                flexDirection: "column",
                gap: 12,
              }}
            >
              <Segmented
                block
                value={speedLimitMode}
                disabled={loading || savingSpeedLimit}
                onChange={(value) =>
                  void updateSpeedLimitMode(value as SpeedLimitMode)
                }
                options={[
                  {
                    label: (
                      <Space size={6}>
                        <ThunderboltOutlined />
                        不限速
                      </Space>
                    ),
                    value: "unlimited",
                  },
                  {
                    label: (
                      <Space size={6}>
                        <DashboardOutlined />
                        限速
                      </Space>
                    ),
                    value: "limited",
                  },
                ]}
              />
              {speedLimitMode === "limited" ? (
                <Space direction="vertical" size={10} style={{ width: "100%" }}>
                  <InputNumber
                    min={1}
                    precision={0}
                    addonAfter="KB/s"
                    value={downloadSpeedLimitKbps ?? undefined}
                    style={{ width: "100%" }}
                    disabled={loading || savingSpeedLimit}
                    placeholder="请输入下载限速"
                    onChange={(value) =>
                      setDownloadSpeedLimitKbps(
                        typeof value === "number" ? value : null
                      )
                    }
                    onBlur={() => {
                      void saveDownloadSpeedLimitValue(downloadSpeedLimitKbps);
                    }}
                  />
                  <Space size={6} wrap>
                    <Typography.Text
                      type="secondary"
                      style={{ fontSize: 12, marginRight: 2 }}
                    >
                      快捷
                    </Typography.Text>
                    {SPEED_LIMIT_PRESETS.map((preset) => {
                      const active = downloadSpeedLimitKbps === preset.value;
                      return (
                        <Button
                          key={preset.value}
                          size="small"
                          type={active ? "primary" : "default"}
                          disabled={loading || savingSpeedLimit}
                          onClick={() => {
                            setDownloadSpeedLimitKbps(preset.value);
                            void saveDownloadSpeedLimitValue(preset.value);
                          }}
                        >
                          {preset.label}
                        </Button>
                      );
                    })}
                  </Space>
                </Space>
              ) : (
                <Typography.Text
                  type="secondary"
                  style={{ fontSize: 12, lineHeight: 1.6 }}
                >
                  当前不限制下载速度，将以最大可用带宽下载。
                </Typography.Text>
              )}
            </div>
          </Space>
          <Space direction="vertical" size={8} style={{ width: "100%" }}>
            <SectionTitle>下载完成后</SectionTitle>
            <Space size={24}>
              <Space size={12}>
                <Typography.Text>删除临时文件夹</Typography.Text>
                <Switch
                  checked={deleteTsTempDirAfterDownload}
                  loading={loading || savingDownloadOutput}
                  onChange={(checked) =>
                    void updateDownloadOutputSettings(checked, convertToMp4)
                  }
                />
              </Space>
              <Space size={12}>
                <Typography.Text>合并mp4</Typography.Text>
                <Switch
                  checked={convertToMp4}
                  loading={loading || savingDownloadOutput}
                  onChange={(checked) =>
                    void updateDownloadOutputSettings(
                      deleteTsTempDirAfterDownload,
                      checked
                    )
                  }
                />
              </Space>
            </Space>
          </Space>
          <Space direction="vertical" size={8} style={{ width: "100%" }}>
            <SectionTitle>请求超时</SectionTitle>
            <div
              style={{
                display: "flex",
                alignItems: "center",
                justifyContent: "space-between",
                gap: 12,
              }}
            >
              <Typography.Text>解析超时</Typography.Text>
              <InputNumber
                min={MIN_METADATA_TIMEOUT_SECS}                precision={0}
                addonAfter="秒"
                style={{ width: 160 }}
                value={metadataTimeoutSecs ?? undefined}
                disabled={loading || savingTimeouts}
                onChange={(value) =>
                  setMetadataTimeoutSecs(
                    typeof value === "number" ? value : null
                  )
                }
                onBlur={() => void saveTimeoutSettingsValues()}
              />
            </div>
            <div
              style={{
                display: "flex",
                alignItems: "center",
                justifyContent: "space-between",
                gap: 12,
              }}
            >
              <Typography.Text>分片下载超时</Typography.Text>
              <InputNumber
                min={MIN_SEGMENT_TIMEOUT_SECS}                precision={0}
                addonAfter="秒"
                style={{ width: 160 }}
                value={segmentTimeoutSecs ?? undefined}
                disabled={loading || savingTimeouts}
                onChange={(value) =>
                  setSegmentTimeoutSecs(typeof value === "number" ? value : null)
                }
                onBlur={() => void saveTimeoutSettingsValues()}
              />
            </div>
            <div
              style={{
                display: "flex",
                alignItems: "center",
                justifyContent: "space-between",
                gap: 12,
              }}
            >
              <Typography.Text>MP4 直链超时</Typography.Text>
              <InputNumber
                min={MIN_MP4_TIMEOUT_SECS}                precision={0}
                addonAfter="秒"
                style={{ width: 160 }}
                value={mp4TimeoutSecs ?? undefined}
                disabled={loading || savingTimeouts}
                onChange={(value) =>
                  setMp4TimeoutSecs(typeof value === "number" ? value : null)
                }
                onBlur={() => void saveTimeoutSettingsValues()}
              />
            </div>
          </Space>
        </Space>
      ),
    },
    {
      key: "live",
      label: "录播设置",
      children: (
        <Space direction="vertical" size={18} style={{ width: "100%" }}>
          <Space direction="vertical" size={8} style={{ width: "100%" }}>
            <SectionTitle>HLS 刷新间隔</SectionTitle>
            <Typography.Text type="secondary" style={{ fontSize: 12 }}>
              录制 HLS 直播时轮询新分片的频率范围，越小越实时但请求更频繁。
            </Typography.Text>
            <div
              style={{
                display: "flex",
                alignItems: "center",
                justifyContent: "space-between",
                gap: 12,
              }}
            >
              <Typography.Text>最小间隔</Typography.Text>
              <InputNumber
                min={MIN_HLS_REFRESH_MIN_MS}                precision={0}
                addonAfter="毫秒"
                style={{ width: 170 }}
                value={hlsRefreshMinMs ?? undefined}
                disabled={loading || savingLiveSettings}
                onChange={(value) =>
                  setHlsRefreshMinMs(typeof value === "number" ? value : null)
                }
                onBlur={() => void saveLiveRecordSettingsValues()}
              />
            </div>
            <div
              style={{
                display: "flex",
                alignItems: "center",
                justifyContent: "space-between",
                gap: 12,
              }}
            >
              <Typography.Text>最大间隔</Typography.Text>
              <InputNumber
                min={MIN_HLS_REFRESH_MAX_MS}                precision={0}
                addonAfter="毫秒"
                style={{ width: 170 }}
                value={hlsRefreshMaxMs ?? undefined}
                disabled={loading || savingLiveSettings}
                onChange={(value) =>
                  setHlsRefreshMaxMs(typeof value === "number" ? value : null)
                }
                onBlur={() => void saveLiveRecordSettingsValues()}
              />
            </div>
          </Space>

          <Space direction="vertical" size={8} style={{ width: "100%" }}>
            <SectionTitle>请求超时</SectionTitle>
            <div
              style={{
                display: "flex",
                alignItems: "center",
                justifyContent: "space-between",
                gap: 12,
              }}
            >
              <Typography.Text>拉取 playlist 超时</Typography.Text>
              <InputNumber
                min={MIN_HLS_PLAYLIST_TIMEOUT_SECS}                precision={0}
                addonAfter="秒"
                style={{ width: 170 }}
                value={hlsPlaylistTimeoutSecs ?? undefined}
                disabled={loading || savingLiveSettings}
                onChange={(value) =>
                  setHlsPlaylistTimeoutSecs(
                    typeof value === "number" ? value : null
                  )
                }
                onBlur={() => void saveLiveRecordSettingsValues()}
              />
            </div>
            <div
              style={{
                display: "flex",
                alignItems: "center",
                justifyContent: "space-between",
                gap: 12,
              }}
            >
              <Typography.Text>分片 / 流下载超时</Typography.Text>
              <InputNumber
                min={MIN_LIVE_SEGMENT_TIMEOUT_SECS}                precision={0}
                addonAfter="秒"
                style={{ width: 170 }}
                value={liveSegmentTimeoutSecs ?? undefined}
                disabled={loading || savingLiveSettings}
                onChange={(value) =>
                  setLiveSegmentTimeoutSecs(
                    typeof value === "number" ? value : null
                  )
                }
                onBlur={() => void saveLiveRecordSettingsValues()}
              />
            </div>
          </Space>

          <Space direction="vertical" size={8} style={{ width: "100%" }}>
            <SectionTitle>断线重连间隔</SectionTitle>
            <Typography.Text type="secondary" style={{ fontSize: 12 }}>
              录制中断流后等待多久再重连。
            </Typography.Text>
            <div
              style={{
                display: "flex",
                alignItems: "center",
                justifyContent: "space-between",
                gap: 12,
              }}
            >
              <Typography.Text>HLS 重连</Typography.Text>
              <InputNumber
                min={MIN_LIVE_RETRY_HLS_MS}                precision={0}
                addonAfter="毫秒"
                style={{ width: 170 }}
                value={liveRetryHlsMs ?? undefined}
                disabled={loading || savingLiveSettings}
                onChange={(value) =>
                  setLiveRetryHlsMs(typeof value === "number" ? value : null)
                }
                onBlur={() => void saveLiveRecordSettingsValues()}
              />
            </div>
            <div
              style={{
                display: "flex",
                alignItems: "center",
                justifyContent: "space-between",
                gap: 12,
              }}
            >
              <Typography.Text>FLV 重连</Typography.Text>
              <InputNumber
                min={MIN_LIVE_RETRY_FLV_MS}                precision={0}
                addonAfter="毫秒"
                style={{ width: 170 }}
                value={liveRetryFlvMs ?? undefined}
                disabled={loading || savingLiveSettings}
                onChange={(value) =>
                  setLiveRetryFlvMs(typeof value === "number" ? value : null)
                }
                onBlur={() => void saveLiveRecordSettingsValues()}
              />
            </div>
          </Space>
        </Space>
      ),
    },
    {
      key: "ffmpeg",
      label: "FFmpeg",
      children: (
        <Space direction="vertical" size={12} style={{ width: "100%" }}>
          <Typography.Paragraph type="secondary" style={{ marginBottom: 0 }}>
            FFmpeg 是一个专业的音视频处理工具，部分转码和合成功能会依赖它，
            如果你想获得更佳的体验，请无脑下载 FFmpeg。
          </Typography.Paragraph>

          <Space style={{ width: "100%", justifyContent: "space-between" }}>
            <SectionTitle>开启 FFmpeg</SectionTitle>
            <Switch
              checked={ffmpegEnabled}
              loading={loading || savingFfmpegEnabled}
              onChange={(checked) => {
                void handleSetFfmpegEnabled(checked);
              }}
            />
          </Space>

          <Card
            size="small"
            title={
              <Space>
                <DashboardOutlined />
                <span>环境检测</span>
              </Space>
            }
            styles={{ body: { padding: "10px 16px" } }}
          >
            {ffmpegStatus ? (
              (() => {
                const ffmpegInfo =
                  ffmpegStatus.kind === "installed"
                    ? {
                        path: ffmpegStatus.path,
                        version: ffmpegStatus.version,
                      }
                    : ffmpegStatus.ffmpeg;
                const ffprobeInfo =
                  ffmpegStatus.kind === "installed"
                    ? ffmpegStatus.ffprobe
                    : ffmpegStatus.ffprobe;

                const renderStatusRow = (
                  name: string,
                  info: { path: string; version: string } | null
                ) => (
                  <div style={{ marginBottom: name === "ffmpeg" ? 12 : 0 }}>
                    <div
                      style={{
                        display: "flex",
                        justifyContent: "space-between",
                        alignItems: "center",
                        marginBottom: 4,
                      }}
                    >
                      <Typography.Text strong>{name}</Typography.Text>
                      {info ? (
                        <Tag
                          color="success"
                          icon={<CheckCircleFilled />}
                          style={{ marginInlineEnd: 0 }}
                        >
                          已就绪 (v{info.version})
                        </Tag>
                      ) : (
                        <Tag
                          color="error"
                          icon={<CloseCircleFilled />}
                          style={{ marginInlineEnd: 0 }}
                        >
                          未找到
                        </Tag>
                      )}
                    </div>
                    {info && (
                      <Typography.Text
                        type="secondary"
                        style={{
                          display: "block",
                          fontSize: 12,
                          wordBreak: "break-all",
                          padding: "4px 8px",
                          background: token.colorFillQuaternary,
                          borderRadius: token.borderRadiusSM,
                        }}
                      >
                        {info.path}
                      </Typography.Text>
                    )}
                  </div>
                );

                return (
                  <div>
                    {renderStatusRow("ffmpeg", ffmpegInfo)}
                    {renderStatusRow("ffprobe", ffprobeInfo)}
                  </div>
                );
              })()
            ) : (
              <div style={{ textAlign: "center", padding: "8px 0" }}>
                <Badge status="processing" text="正在检测环境..." />
              </div>
            )}
          </Card>

          {ffmpegEnabled && ffmpegStatus?.kind !== "installed" && (
            <Space direction="vertical" size={8} style={{ width: "100%" }}>
              <SectionTitle>自动下载</SectionTitle>
              {ffmpegDownloading && (
                <Progress percent={ffmpegDownloadProgress} size="small" />
              )}
              <Button
                type="primary"
                loading={ffmpegDownloading}
                onClick={() => void handleDownloadFfmpeg()}
              >
                一键下载
              </Button>
            </Space>
          )}

          <Space direction="vertical" size={8} style={{ width: "100%" }}>
            <SectionTitle>自定义路径</SectionTitle>
            <Typography.Text type="secondary" style={{ fontSize: 12 }}>
              请选择 ffmpeg 可执行文件本身（非所在目录），同目录下需存在 ffprobe。
            </Typography.Text>
            <Space size={8}>
              <Button
                disabled={!ffmpegEnabled}
                onClick={() => void handleSetFfmpegCustomPath()}
              >
                选择文件
              </Button>
              {ffmpegCustomPath && (
                <Button
                  disabled={!ffmpegEnabled}
                  onClick={() => void handleResetFfmpegPath()}
                >
                  重置
                </Button>
              )}
            </Space>
            {ffmpegCustomPath && (
              <Typography.Text
                type="secondary"
                style={{ fontSize: 12, wordBreak: "break-all" }}
              >
                {ffmpegCustomPath}
              </Typography.Text>
            )}
          </Space>
        </Space>
      ),
    },
    {
      key: "about",
      label: (
        <Badge dot={updateAvailable} offset={[6, 2]}>
          关于
        </Badge>
      ),
      children: (
        <div
          style={{
            padding: "16px 18px",
            borderRadius: 14,
            border: `1px solid ${token.colorBorderSecondary}`,
            background: `linear-gradient(135deg, ${token.colorInfoBg} 0%, ${token.colorBgContainer} 100%)`,
          }}
        >
          <div
            style={{
              display: "flex",
              alignItems: "center",
              justifyContent: "space-between",
              gap: 12,
              flexWrap: "wrap",
            }}
          >
            <Space size={12} align="center">
              <div
                style={{
                  width: 40,
                  height: 40,
                  borderRadius: 12,
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                  background: token.colorPrimary,
                  color: token.colorWhite,
                  flex: "0 0 auto",
                }}
              >
                <ThunderboltOutlined style={{ fontSize: 20 }} />
              </div>
              <div style={{ display: "flex", flexDirection: "column", gap: 2 }}>
                <Typography.Text strong style={{ fontSize: 15 }}>
                  M3U8 Quicker
                </Typography.Text>
                <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                  <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                    v{appVersion || "-"}
                  </Typography.Text>
                  <a
                    href="#"
                    onClick={(e) => {
                      e.preventDefault();
                      openUrl("https://github.com/Liubsyy/M3U8Quicker");
                    }}
                    style={{ display: "inline-flex", alignItems: "center", gap: 4, fontSize: 12 }}
                  >
                    <GithubOutlined />
                    Liubsyy
                  </a>
                </div>
              </div>
            </Space>
            <Button
              size="small"
              icon={
                <Badge dot={updateAvailable} offset={[2, 0]}>
                  <ReloadOutlined />
                </Badge>
              }
              onClick={() => setUpdateModalOpen(true)}
            >
              检查更新
            </Button>
          </div>
        </div>
      ),
    },
  ];

  return (
    <>
      <Modal
        title="设置"
        open={open}
        onCancel={onClose}
        onOk={() => void handleConfirm()}
        okText="确定"
        cancelButtonProps={{ style: { display: "none" } }}
        width={680}
        confirmLoading={
          loading ||
          savingProxy ||
          savingConcurrency ||
          savingSpeedLimit ||
          savingDownloadOutput ||
          savingFfmpegEnabled ||
          savingUserAgent ||
          savingTimeouts ||
          savingLiveSettings ||
          savingCloseToTray
        }
      >
        <Tabs
          className="settings-modal-tabs"
          tabPosition="left"
          activeKey={activeTab}
          onChange={(key) =>
            setActiveTab(
              key as
                | "general"
                | "network"
                | "download"
                | "live"
                | "ffmpeg"
                | "about"
            )
          }
          items={settingsTabItems}
        />
      </Modal>
      <UpdateModal
        open={updateModalOpen}
        onClose={() => setUpdateModalOpen(false)}
        onChecked={(info) => onUpdateAvailabilityChange?.(info.has_update)}
      />
    </>
  );
}

function formatSettingsError(error: unknown) {
  const text = String(error ?? "").trim();
  if (!text) {
    return "未知错误";
  }

  const normalized = text
    .replace(
      /^(Invalid input|M3U8 parse error|Network error|IO error|URL parse error|Decryption error|Conversion error|Failed to create HTTP client):\s*/i,
      ""
    )
    .replace(/^builder error:\s*/i, "")
    .trim();

  if (!normalized) {
    return "未知错误";
  }

  if (/^代理地址不能为空$/i.test(normalized)) {
    return normalized;
  }

  if (/^代理地址无效[:：]\s*/i.test(normalized)) {
    const detail = normalized.replace(/^代理地址无效[:：]\s*/i, "").trim();
    return formatProxyAddressDetail(detail);
  }

  return formatProxyAddressDetail(normalized);
}

function formatProxyAddressDetail(detail: string) {
  const normalizedDetail = detail
    .replace(/^(builder error:\s*)+/i, "")
    .trim();

  if (!normalizedDetail) {
    return "请输入有效的地址";
  }

  if (/builder error/i.test(normalizedDetail)) {
    return "代理地址端口无效";
  }

  if (/^relative url without a base$/i.test(normalizedDetail)) {
    return "请输入完整的代理地址，例如 http://127.0.0.1:7890";
  }

  if (/unknown proxy scheme/i.test(normalizedDetail)) {
    return "代理协议不受支持，请使用 http://、https:// 或 socks5://";
  }

  if (/empty host/i.test(normalizedDetail)) {
    return "代理地址缺少主机名";
  }

  if (/invalid port number/i.test(normalizedDetail)) {
    return "代理地址端口无效";
  }

  if (/failed to create http client/i.test(normalizedDetail)) {
    return "代理地址端口无效";
  }

  return normalizedDetail;
}
