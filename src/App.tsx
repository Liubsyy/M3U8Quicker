import { t, useTranslation } from "./i18n";
import { useEffect, useState, type Dispatch, type SetStateAction } from "react";
import {
  Button,
  Layout,
  Modal,
  Popconfirm,
  Space,
  Tabs,
  Tag,
  Typography,
  message,
  theme,
} from "antd";
import {
  ChromeOutlined,
  ClearOutlined,
  FolderOpenOutlined,
} from "@ant-design/icons";
import { EdgeIcon } from "./components/EdgeIcon";
import { FirefoxIcon } from "./components/FirefoxIcon";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { Toolbar } from "./components/Toolbar";
import { DownloadList } from "./components/DownloadList";
import { NewDownloadModal } from "./components/NewDownloadModal";
import { NewLiveRecordModal } from "./components/NewLiveRecordModal";
import { BatchDownloadModal } from "./components/BatchDownloadModal";
import { VideoPreviewModal } from "./components/VideoPreviewModal";
import { SettingsModal } from "./components/SettingsModal";
import { ToolsModal, type ToolAction } from "./components/ToolsModal";
import { useDownloads } from "./hooks/useDownloads";
import { useLiveRecords } from "./hooks/useLiveRecords";
import {
  installChromiumExtension,
  openChromiumExtensionsPage,
  installFirefoxExtension,
  openFirefoxAddonsPage,
  openFileLocation,
  openDownloadPlaybackSession,
  openLivePlaybackSession,
  createPreviewSession,
  closePreviewSession,
  getAppSettings,
  setProxySettings,
  getFfmpegStatus,
  checkForUpdate,
  convertLiveRecordToMp4,
} from "./services/api";
import type {
  ChromiumBrowser,
  ChromiumExtensionInstallResult,
  DownloadStatus,
  FirefoxExtensionInstallResult,
  DownloadTaskSummary,
  LiveProgressEvent,
  LiveProtocol,
} from "./types";
import {
  canOpenInProgressPlayback,
  isDirectFileType,
  liveRecordToDownloadSummary,
  parseFileType,
} from "./types";
import type { ThemeMode } from "./types/settings";
import {
  DEFAULT_HISTORY_PAGE_SIZE,
  DEFAULT_ZOOM,
  normalizeZoom,
  UPDATE_NOTIFICATIONS_STORAGE_KEY,
  ZOOM_STEP,
} from "./types/settings";

const { Header, Content } = Layout;

interface DownloadDraft {
  url: string;
  extraHeaders?: string;
  fileType?: import("./types").FileType;
  nonce: number;
}

interface BatchDownloadDraft {
  rawInput: string;
  extraHeaders?: string;
  fileTypes?: Array<import("./types").FileType | undefined>;
  nonce: number;
}

interface AppProps {
  themeMode: ThemeMode;
  onThemeModeChange: (mode: ThemeMode) => void;
  zoomFactor: number;
  onZoomChange: Dispatch<SetStateAction<number>>;
}

interface ChromiumInstallGuideState {
  browser: ChromiumBrowser;
  guide: ChromiumExtensionInstallResult;
}

const CHROMIUM_BROWSER_META: Record<
  ChromiumBrowser,
  {
    title: string;
    name: string;
    shortName: string;
    openButtonText: string;
    accentColor: string;
  }
> = {
  chrome: {
    get title() { return t("installChromeExtension"); },
    name: "Chrome",
    shortName: "Chrome",
    get openButtonText() { return t("openChrome"); },
    accentColor: "#4285f4",
  },
  edge: {
    get title() { return t("installMicrosoftEdgeExtension"); },
    name: "Microsoft Edge",
    shortName: "Edge",
    get openButtonText() { return t("openEdge"); },
    accentColor: "#0f6cbd",
  },
};

function App({
  themeMode,
  onThemeModeChange,
  zoomFactor,
  onZoomChange,
}: AppProps) {
  useTranslation();
  const [modalOpen, setModalOpen] = useState(false);
  const [liveRecordModalOpen, setLiveRecordModalOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsInitialTab, setSettingsInitialTab] = useState<
    "general" | "network" | "download" | "ffmpeg"
  >("general");
  const [toolModalOpen, setToolModalOpen] = useState(false);
  const [activeTool, setActiveTool] = useState<ToolAction | null>(null);
  const [toolInputPath, setToolInputPath] = useState<string | undefined>();
  const [downloadDraft, setDownloadDraft] = useState<DownloadDraft | null>(null);
  const [batchDownloadDraft, setBatchDownloadDraft] = useState<BatchDownloadDraft | null>(null);
  const [liveRecordDraft, setLiveRecordDraft] = useState<{
    url: string;
    extraHeaders?: string;
    filename?: string;
    outputDir?: string;
    nonce: number;
  } | null>(null);
  const [batchDownloadModalOpen, setBatchDownloadModalOpen] = useState(false);
  const [videoPreviewModalOpen, setVideoPreviewModalOpen] = useState(false);
  const [updateAvailable, setUpdateAvailable] = useState(false);
  const [updateNotificationsEnabled, setUpdateNotificationsEnabled] = useState(
    () => localStorage.getItem(UPDATE_NOTIFICATIONS_STORAGE_KEY) !== "false"
  );
  const showUpdateDot = updateNotificationsEnabled && updateAvailable;
  const [proxyEnabled, setProxyEnabled] = useState(false);
  const [historyPageSize, setHistoryPageSize] = useState(DEFAULT_HISTORY_PAGE_SIZE);
  const [chromiumInstallGuide, setChromiumInstallGuide] =
    useState<ChromiumInstallGuideState | null>(null);
  const [firefoxInstallGuide, setFirefoxInstallGuide] =
    useState<FirefoxExtensionInstallResult | null>(null);
  const [liveStopTarget, setLiveStopTarget] = useState<{
    id: string;
    filename: string;
    filePath: string | null;
    protocol: LiveProtocol;
    isSplit: boolean;
  } | null>(null);
  const {
    counts,
    downloading,
    downloadingPage,
    downloadingPageSize,
    downloadingTotal,
    completed,
    completedPage,
    completedPageSize,
    completedTotal,
    addDownload,
    addDownloadsBatch,
    pause,
    resume,
    retryFailed,
    cancel,
    remove,
    clearCompleted,
    loadingActive,
    loadingHistory,
    refreshActive,
    refreshHistory,
    getSegmentState,
  } = useDownloads(historyPageSize);
  const {
    counts: liveCounts,
    recording: liveRecording,
    recordingPage: liveRecordingPage,
    recordingPageSize: liveRecordingPageSize,
    recordingTotal: liveRecordingTotal,
    recorded: liveRecorded,
    recordedPage: liveRecordedPage,
    recordedPageSize: liveRecordedPageSize,
    recordedTotal: liveRecordedTotal,
    addLiveRecord,
    pause: pauseLive,
    resume: resumeLive,
    stop: stopLive,
    cancel: cancelLive,
    remove: removeLive,
    clearCompleted: clearLiveCompleted,
    refreshActive: refreshLiveActive,
    refreshHistory: refreshLiveHistory,
    loadingActive: loadingLiveActive,
    loadingHistory: loadingLiveHistory,
  } = useLiveRecords(historyPageSize);
  const { token } = theme.useToken();

  useEffect(() => {
    localStorage.setItem(
      UPDATE_NOTIFICATIONS_STORAGE_KEY,
      String(updateNotificationsEnabled)
    );
  }, [updateNotificationsEnabled]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey)) return;
      if (e.key === "=" || e.key === "+") {
        e.preventDefault();
        onZoomChange((z) => normalizeZoom(z + ZOOM_STEP));
      } else if (e.key === "-" || e.key === "_") {
        e.preventDefault();
        onZoomChange((z) => normalizeZoom(z - ZOOM_STEP));
      } else if (e.key === "0") {
        e.preventDefault();
        onZoomChange(DEFAULT_ZOOM);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onZoomChange]);

  useEffect(() => {
    let cancelled = false;
    const timer = window.setTimeout(() => {
      void checkForUpdate()
        .then((info) => {
          if (!cancelled) {
            setUpdateAvailable(info.has_update);
          }
        })
        .catch(() => {
          // 启动后的静默检查不影响主流程。
        });
    }, 1200);

    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, []);

  useEffect(() => {
    let cancelled = false;

    void getAppSettings()
      .then((settings) => {
        if (!cancelled) {
          setHistoryPageSize(settings.history_page_size);
          setProxyEnabled(settings.proxy.enabled);
        }
      })
      .catch((error) => {
        console.error("Failed to load history page size", error);
      });

    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    const openDraftFromDeepLink = (deepLink: string) => {
      if (!shouldHandleDeepLink(deepLink)) {
        return;
      }

      const singleDraft = parseDownloadDraft(deepLink);
      if (singleDraft) {
        void bringMainWindowToFront();
        setDownloadDraft({
          ...singleDraft,
          nonce: Date.now(),
        });
        setModalOpen(true);
        return;
      }

      const liveDraft = parseNewLiveRecordDraft(deepLink);
      if (liveDraft) {
        void bringMainWindowToFront();
        setLiveRecordDraft({
          ...liveDraft,
          nonce: Date.now(),
        });
        setLiveRecordModalOpen(true);
        return;
      }

      const previewDraft = parsePreviewDraft(deepLink);
      if (previewDraft) {
        void openPreviewWindowFromDeepLink(
          previewDraft.url,
          previewDraft.extraHeaders,
          previewDraft.title,
          () => {
            setSettingsInitialTab("ffmpeg");
            setSettingsOpen(true);
          }
        );
        return;
      }

      const batchDraft = parseBatchDownloadDraft(deepLink);
      if (!batchDraft) {
        return;
      }

      void bringMainWindowToFront();
      setBatchDownloadDraft({
        ...batchDraft,
        nonce: Date.now(),
      });
      setBatchDownloadModalOpen(true);
    };

    deepLinkHandlers.add(openDraftFromDeepLink);
    void ensureDeepLinkInit();

    return () => {
      deepLinkHandlers.delete(openDraftFromDeepLink);
    };
  }, []);

  const handleOpenPlaybackWindow = async (task: DownloadTaskSummary) => {
    if (
      (task.status === "Downloading" || task.status === "Paused") &&
      isDirectFileType(task.file_type) &&
      !canOpenInProgressPlayback(task)
    ) {
      message.warning(t("thisFormatCannotBePlayedWhileDownloadingPleaseWaitFor"));
      return;
    }

    try {
      const session = await openDownloadPlaybackSession(task.id);
      const existingWindow = await WebviewWindow.getByLabel(session.window_label);

      if (existingWindow) {
        await existingWindow.show();
        await existingWindow.setFocus();
        return;
      }

      const url = `/?${new URLSearchParams({
        view: "player",
        taskId: task.id,
        playbackUrl: session.playback_url,
        playbackKind: session.playback_kind,
        isLive: session.is_live ? "1" : "0",
        sessionToken: session.session_token,
        filename: session.filename,
      }).toString()}`;

      const playerWindow = new WebviewWindow(session.window_label, {
        url,
        title: t("playing", { value0: session.filename }),
        width: 960,
        height: 640,
        minWidth: 720,
        minHeight: 420,
        resizable: true,
        center: true,
      });

      playerWindow.once("tauri://created", () => {
        void playerWindow.setFocus();
      });
      playerWindow.once("tauri://error", (event) => {
        console.error("Failed to create playback window", event);
        message.error(t("failedToOpenPlayerWindow"));
      });
    } catch (error) {
      console.error("Failed to open playback window", error);
      message.error(t("failedToOpenPlayer", { value0: error }));
    }
  };

  const handleOpenLivePlaybackWindow = async (task: DownloadTaskSummary) => {
    try {
      const session = await openLivePlaybackSession(task.id);
      const existingWindow = await WebviewWindow.getByLabel(session.window_label);

      if (existingWindow) {
        await existingWindow.show();
        await existingWindow.setFocus();
        return;
      }

      const url = `/?${new URLSearchParams({
        view: "player",
        scope: "live",
        taskId: task.id,
        playbackUrl: session.playback_url,
        playbackKind: session.playback_kind,
        isLive: session.is_live ? "1" : "0",
        sessionToken: session.session_token,
        filename: session.filename,
        status: typeof session.status === "object" ? "Failed" : session.status,
      }).toString()}`;

      const playerWindow = new WebviewWindow(session.window_label, {
        url,
        title: t("playing", { value0: session.filename }),
        width: 960,
        height: 640,
        minWidth: 720,
        minHeight: 420,
        resizable: true,
        center: true,
      });

      playerWindow.once("tauri://created", () => {
        void playerWindow.setFocus();
      });
      playerWindow.once("tauri://error", (event) => {
        console.error("Failed to create live playback window", event);
        message.error(t("failedToOpenPlayerWindow"));
      });
    } catch (error) {
      console.error("Failed to open live playback window", error);
      message.error(t("failedToOpenPlayer", { value0: error }));
    }
  };

  const requestStopLive = (id: string) => {
    const record = liveRecording.find((item) => item.id === id);
    if (!record) {
      void stopLive(id);
      return;
    }
    setLiveStopTarget({
      id,
      filename: record.filename,
      filePath: record.file_path,
      protocol: record.protocol,
      isSplit: Boolean(record.split) || (record.part_paths?.length ?? 0) > 0,
    });
  };

  const performStopLive = async (convertToMp4Flag: boolean) => {
    if (!liveStopTarget) return;
    const { id, filename } = liveStopTarget;
    setLiveStopTarget(null);

    const recordedPromise = convertToMp4Flag ? waitForLiveRecorded(id) : null;

    try {
      await stopLive(id);
    } catch {
      // stopLive already surfaces an error message
      return;
    }

    if (!convertToMp4Flag) return;

    const messageKey = `live-convert-${id}`;
    try {
      message.loading({
        key: messageKey,
        content: t("willConvertToMp4WhenRecordingFinishes", { value0: filename }),
        duration: 0,
      });
      await recordedPromise;
      // 分段任务由后端逐段转换（FLV 输出到任务目录的 mp4/ 子目录）。
      const finalPaths = await convertLiveRecordToMp4(id);
      message.success({
        key: messageKey,
        content:
          finalPaths.length === 1
            ? t("convertedToMp4", { value0: finalPaths[0] })
            : t("convertedSegmentsToMp4", { value0: finalPaths.length, value1: finalPaths[0]?.replace(/[^\\/]+$/, "") }),
      });
    } catch (err) {
      message.error({
        key: messageKey,
        content: t("failedToConvertToMp4", { value0: formatLiveStopError(err) }),
      });
    }
  };

  const handleInstallChromiumExtension = async (browser: ChromiumBrowser) => {
    try {
      const guide = await installChromiumExtension(browser);
      setChromiumInstallGuide({ browser, guide });
    } catch (error) {
      console.error("Failed to open chromium extension installer", error);
      message.error(t("failedToOpenInstallationGuide", { value0: error }));
    }
  };

  const handleOpenChromiumExtensionsPage = async (browser: ChromiumBrowser) => {
    const browserName = CHROMIUM_BROWSER_META[browser].name;

    try {
      const opened = await openChromiumExtensionsPage(browser);
      if (!opened) {
        message.warning(t("wasNotFoundPleaseOpenTheExtensionsPageManually", { value0: browserName }));
      }
    } catch (error) {
      console.error("Failed to open chromium extensions page", error);
      message.error(t("failedToOpenExtensionsPage", { value0: browserName, value1: error }));
    }
  };

  const handleOpenChromiumExtensionFolder = async () => {
    if (!chromiumInstallGuide) return;

    try {
      await openFileLocation(chromiumInstallGuide.guide.extension_path);
      message.success(t("extensionFolderOpened"));
    } catch (error) {
      console.error("Failed to open chromium extension folder", error);
      message.error(t("failedToOpenExtensionFolder", { value0: error }));
    }
  };

  const handleInstallFirefoxExtension = async () => {
    try {
      const result = await installFirefoxExtension();
      setFirefoxInstallGuide(result);
    } catch (error) {
      console.error("Failed to open firefox extension installer", error);
      message.error(t("failedToOpenInstallationGuide", { value0: error }));
    }
  };

  const handleOpenFirefoxAddonsPage = async () => {
    try {
      const opened = await openFirefoxAddonsPage();
      if (!opened) {
        message.warning(t("firefoxWasNotFoundPleaseOpenTheAddOnsPage"));
      }
    } catch (error) {
      console.error("Failed to open firefox addons page", error);
      message.error(t("failedToOpenFirefoxAddOnsPage", { value0: error }));
    }
  };

  const handleOpenFirefoxExtensionFolder = async () => {
    if (!firefoxInstallGuide) return;

    try {
      await openFileLocation(firefoxInstallGuide.extension_path);
      message.success(t("extensionFolderOpened"));
    } catch (error) {
      console.error("Failed to open firefox extension folder", error);
      message.error(t("failedToOpenExtensionFolder", { value0: error }));
    }
  };

  const handleProxyEnabledChange = async (enabled: boolean) => {
    try {
      const settings = await getAppSettings();
      if (settings.proxy.enabled !== enabled) {
        await setProxySettings({ ...settings.proxy, enabled });
      }
      setProxyEnabled(enabled);
      message.success(enabled ? t("proxyEnabled") : t("proxyDisabled"));
    } catch (error) {
      message.error(t("failedToToggleProxy", { value0: String(error) }));
      void getAppSettings()
        .then((settings) => setProxyEnabled(settings.proxy.enabled))
        .catch(() => {});
    }
  };

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let unlistenProxyError: UnlistenFn | undefined;
    let unlistenProxyChange: UnlistenFn | undefined;
    let cancelled = false;

    void listen<string>("tray-action", (event) => {
      const action = event.payload;
      switch (action) {
        case "new-download":
          setDownloadDraft(null);
          setModalOpen(true);
          break;
        case "live-record":
          setLiveRecordDraft(null);
          setLiveRecordModalOpen(true);
          break;
        case "open-video-preview":
          setVideoPreviewModalOpen(true);
          break;
        case "install-chrome-extension":
          void handleInstallChromiumExtension("chrome");
          break;
        case "install-edge-extension":
          void handleInstallChromiumExtension("edge");
          break;
        case "install-firefox-extension":
          void handleInstallFirefoxExtension();
          break;
        case "open-settings":
          setSettingsInitialTab("general");
          setSettingsOpen(true);
          break;
        case "open-proxy-settings":
          setSettingsInitialTab("network");
          setSettingsOpen(true);
          break;
        default:
          break;
      }
    })
      .then((fn) => {
        if (cancelled) {
          fn();
          return;
        }
        unlisten = fn;
      })
      .catch((error) => {
        console.error("[m3u8quicker] failed to subscribe tray-action", error);
      });

    void listen<string>("proxy-settings-error", (event) => {
      message.error(t("failedToSaveProxySettings", { value0: event.payload }));
    })
      .then((fn) => {
        if (cancelled) {
          fn();
          return;
        }
        unlistenProxyError = fn;
      })
      .catch((error) => {
        console.error(
          "[m3u8quicker] failed to subscribe proxy-settings-error",
          error
        );
      });

    void listen<boolean>("proxy-settings-changed", (event) => {
      setProxyEnabled(event.payload);
    })
      .then((fn) => {
        if (cancelled) {
          fn();
          return;
        }
        unlistenProxyChange = fn;
      })
      .catch((error) => {
        console.error(
          "[m3u8quicker] failed to subscribe proxy-settings-changed",
          error
        );
      });

    return () => {
      cancelled = true;
      unlisten?.();
      unlistenProxyError?.();
      unlistenProxyChange?.();
    };
  }, []);

  const chromiumBrowserMeta = CHROMIUM_BROWSER_META[chromiumInstallGuide?.browser ?? "chrome"];

  const liveRecordingItems = liveRecording.map(liveRecordToDownloadSummary);
  const liveRecordedItems = liveRecorded.map(liveRecordToDownloadSummary);

  const liveStatusTagOverride = (status: DownloadStatus) => {
    if (status === "Downloading") return <Tag color="processing">{t("recording")}</Tag>;
    if (status === "Paused") return <Tag color="warning">{t("paused")}</Tag>;
    if (status === "Completed") return <Tag color="success">{t("recorded")}</Tag>;
    if (status === "Cancelled") return <Tag color="default">{t("cancelled")}</Tag>;
    if (typeof status === "object" && "Failed" in status)
      return <Tag color="error">{t("failed")}</Tag>;
    return undefined;
  };

  const noopGetSegmentState = async () => ({
    id: "",
    total_segments: 0,
    completed_segment_indices: [],
    failed_segment_indices: [],
    updated_at: new Date().toISOString(),
  });

  const tabItems = [
    {
      key: "downloading",
      label: t("downloading", { value0: counts.active_count }),
      children: (
        <DownloadList
          downloads={downloading}
          total={downloadingTotal}
          currentPage={downloadingPage}
          pageSize={downloadingPageSize}
          onPageChange={(page) => {
            void refreshActive(page);
          }}
          getSegmentState={getSegmentState}
          onPause={pause}
          onResume={resume}
          onRetryFailed={retryFailed}
          onCancel={cancel}
          onRemove={remove}
          onPlay={(task) => {
            void handleOpenPlaybackWindow(task);
          }}
          loading={loadingActive}
          showActions={["play", "pause", "resume", "cancel", "open"]}
        />
      ),
    },
    {
      key: "completed",
      label: t("completed", { value0: counts.history_count }),
      children: (
        <DownloadList
          downloads={completed}
          total={completedTotal}
          currentPage={completedPage}
          pageSize={completedPageSize}
          onPageChange={(page) => {
            void refreshHistory(page);
          }}
          getSegmentState={getSegmentState}
          onPause={pause}
          onResume={resume}
          onRetryFailed={retryFailed}
          onCancel={cancel}
          onRemove={remove}
          onPlay={(task) => {
            void handleOpenPlaybackWindow(task);
          }}
          loading={loadingHistory}
          onAnalyze={(filePath) => {
            setToolInputPath(filePath);
            setActiveTool("analyze-media");
            setToolModalOpen(true);
          }}
          showActions={["play", "remove", "open"]}
          showSpeed={false}
          actionsHeaderExtra={
            <Popconfirm
              title={t("clearThisList")}
              description={t("onlyCompletedRecordsWillBeRemovedLocalFilesWillBe")}
              onConfirm={() => void clearCompleted()}
              okText={t("clearList")}
              cancelText={t("cancel")}
              disabled={counts.history_count === 0}
            >
              <Button
                type="text"
                size="small"
                danger
                icon={<ClearOutlined />}
                aria-label={t("clearList")}
                disabled={counts.history_count === 0}
              />
            </Popconfirm>
          }
        />
      ),
    },
    {
      key: "live-recording",
      label: t("liveRecordings", { value0: liveCounts.active_count }),
      children: (
        <DownloadList
          downloads={liveRecordingItems}
          total={liveRecordingTotal}
          currentPage={liveRecordingPage}
          pageSize={liveRecordingPageSize}
          onPageChange={(page) => {
            void refreshLiveActive(page);
          }}
          getSegmentState={noopGetSegmentState}
          onPause={pauseLive}
          onResume={resumeLive}
          onRetryFailed={() => undefined}
          onCancel={cancelLive}
          onRemove={removeLive}
          onStop={requestStopLive}
          onPlay={(task) => {
            void handleOpenLivePlaybackWindow(task);
          }}
          loading={loadingLiveActive}
          showActions={["play", "pause", "resume", "stop", "cancel", "open"]}
          statusTagOverride={liveStatusTagOverride}
          cancelLabels={{
            title: t("cancelRecording"),
            description: t("recordedFilesWillBePermanentlyDeleted"),
            okText: t("cancelAndDelete"),
            cancelText: t("continueRecording"),
          }}
        />
      ),
    },
    {
      key: "live-recorded",
      label: t("recorded2", { value0: liveCounts.history_count }),
      children: (
        <DownloadList
          downloads={liveRecordedItems}
          total={liveRecordedTotal}
          currentPage={liveRecordedPage}
          pageSize={liveRecordedPageSize}
          onPageChange={(page) => {
            void refreshLiveHistory(page);
          }}
          getSegmentState={noopGetSegmentState}
          onPause={() => undefined}
          onResume={() => undefined}
          onRetryFailed={() => undefined}
          onCancel={() => undefined}
          onRemove={removeLive}
          onPlay={(task) => {
            void handleOpenLivePlaybackWindow(task);
          }}
          loading={loadingLiveHistory}
          showActions={["play", "remove", "open"]}
          showSpeed={false}
          statusTagOverride={liveStatusTagOverride}
          actionsHeaderExtra={
            <Popconfirm
              title={t("clearThisList")}
              description={t("onlyCompletedRecordingRecordsWillBeRemovedLocalFilesWill")}
              onConfirm={() => void clearLiveCompleted()}
              okText={t("clearList")}
              cancelText={t("cancel")}
              disabled={liveCounts.history_count === 0}
            >
              <Button
                type="text"
                size="small"
                danger
                icon={<ClearOutlined />}
                aria-label={t("clearList")}
                disabled={liveCounts.history_count === 0}
              />
            </Popconfirm>
          }
        />
      ),
    },
  ];

  return (
    <Layout style={{ minHeight: "100vh", background: token.colorBgLayout }}>
      <Header
        style={{
          display: "flex",
          alignItems: "center",
          padding: "0 24px",
          background: token.colorBgContainer,
          borderBottom: `1px solid ${token.colorBorder}`,
        }}
      >
        <Toolbar
          onNewDownload={() => {
            setDownloadDraft(null);
            setModalOpen(true);
          }}
          onOpenBatchDownload={() => {
            setBatchDownloadDraft(null);
            setBatchDownloadModalOpen(true);
          }}
          onOpenVideoPreview={() => setVideoPreviewModalOpen(true)}
          onOpenLiveRecord={() => setLiveRecordModalOpen(true)}
          onOpenTool={(tool) => {
            if (tool === "install-chrome-extension") {
              void handleInstallChromiumExtension("chrome");
              return;
            }
            if (tool === "install-edge-extension") {
              void handleInstallChromiumExtension("edge");
              return;
            }
            if (tool === "install-firefox-extension") {
              void handleInstallFirefoxExtension();
              return;
            }
            setActiveTool(tool);
            setToolInputPath(undefined);
            setToolModalOpen(true);
          }}
          onOpenSettings={() => {
            setSettingsInitialTab("general");
            setSettingsOpen(true);
          }}
          proxyEnabled={proxyEnabled}
          onOpenProxySettings={() => {
            setSettingsInitialTab("network");
            setSettingsOpen(true);
          }}
          onProxyEnabledChange={(enabled) => {
            void handleProxyEnabledChange(enabled);
          }}
          updateAvailable={showUpdateDot}
        />
      </Header>
      <Content
        style={{
          padding: "16px 24px",
          background: token.colorBgLayout,
        }}
      >
        <Tabs items={tabItems} defaultActiveKey="downloading" />
      </Content>
      <NewDownloadModal
        open={modalOpen}
        initialUrl={downloadDraft?.url}
        initialExtraHeaders={downloadDraft?.extraHeaders}
        initialFileType={downloadDraft?.fileType}
        resetKey={downloadDraft?.nonce ?? 0}
        onClose={() => setModalOpen(false)}
        onOpenFfmpegSettings={() => {
          setSettingsInitialTab("ffmpeg");
          setSettingsOpen(true);
        }}
        onSwitchToLiveRecord={(draft) => {
          setModalOpen(false);
          setDownloadDraft(null);
          setLiveRecordDraft({ ...draft, nonce: Date.now() });
          setLiveRecordModalOpen(true);
        }}
        onSubmit={async (params) => {
          await addDownload(params);
          setModalOpen(false);
        }}
      />
      <SettingsModal
        open={settingsOpen}
        initialTab={settingsInitialTab}
        themeMode={themeMode}
        zoomFactor={zoomFactor}
        updateAvailable={showUpdateDot}
        updateNotificationsEnabled={updateNotificationsEnabled}
        onUpdateNotificationsChange={setUpdateNotificationsEnabled}
        historyPageSize={historyPageSize}
        onClose={() => {
          setSettingsOpen(false);
          setSettingsInitialTab("general");
        }}
        onThemeModeChange={onThemeModeChange}
        onZoomChange={onZoomChange}
        onHistoryPageSizeChange={setHistoryPageSize}
        onUpdateAvailabilityChange={setUpdateAvailable}
      />
      <ToolsModal
        open={toolModalOpen}
        tool={activeTool}
        initialInputPath={toolInputPath}
        onClose={() => {
          setToolModalOpen(false);
          setActiveTool(null);
          setToolInputPath(undefined);
        }}
      />
      <BatchDownloadModal
        open={batchDownloadModalOpen}
        initialRawInput={batchDownloadDraft?.rawInput}
        initialExtraHeaders={batchDownloadDraft?.extraHeaders}
        initialFileTypes={batchDownloadDraft?.fileTypes}
        resetKey={batchDownloadDraft?.nonce ?? 0}
        onClose={() => {
          setBatchDownloadModalOpen(false);
          setBatchDownloadDraft(null);
        }}
        onOpenFfmpegSettings={() => {
          setSettingsInitialTab("ffmpeg");
          setSettingsOpen(true);
        }}
        onSubmit={async (paramsList) => {
          return addDownloadsBatch(paramsList);
        }}
      />
      <VideoPreviewModal
        open={videoPreviewModalOpen}
        onClose={() => setVideoPreviewModalOpen(false)}
        onOpenFfmpegSettings={() => {
          setVideoPreviewModalOpen(false);
          setSettingsInitialTab("ffmpeg");
          setSettingsOpen(true);
        }}
      />
      <NewLiveRecordModal
        open={liveRecordModalOpen}
        onClose={() => {
          setLiveRecordModalOpen(false);
          setLiveRecordDraft(null);
        }}
        onSubmit={async (params) => {
          await addLiveRecord(params);
        }}
        onSwitchToDownload={(draft) => {
          setLiveRecordModalOpen(false);
          setLiveRecordDraft(null);
          setDownloadDraft({ ...draft, nonce: Date.now() });
          setModalOpen(true);
        }}
        initialUrl={liveRecordDraft?.url}
        initialExtraHeaders={liveRecordDraft?.extraHeaders}
        initialFilename={liveRecordDraft?.filename}
        initialOutputDir={liveRecordDraft?.outputDir}
        resetKey={liveRecordDraft?.nonce ?? 0}
      />
      <Modal
        title={t("stopRecording")}
        open={Boolean(liveStopTarget)}
        onCancel={() => setLiveStopTarget(null)}
        footer={
          <Space>
            <Button onClick={() => setLiveStopTarget(null)}>{t("cancel")}</Button>
            <Button onClick={() => void performStopLive(false)}>{t("stopOnly")}</Button>
            <Button type="primary" onClick={() => void performStopLive(true)}>
              {t("yesConvertToMp4")}</Button>
          </Space>
        }
      >
        <Typography.Paragraph style={{ marginBottom: 0 }}>
          {liveStopTarget?.protocol === "hls"
            ? t("mergeTheRecordedHlsSegmentsIntoMp4SegmentsAndLocal")
            : t("convertTheRecordedFlvToMp4ForPlaybackInBrowsers")}
        </Typography.Paragraph>
        {liveStopTarget?.isSplit ? (
          <Typography.Paragraph style={{ marginTop: 8, marginBottom: 0 }}>
            {t("splitRecordingIsEnabledEachSegmentWillBeConvertedInto")}</Typography.Paragraph>
        ) : null}
        {liveStopTarget?.filename ? (
          <Typography.Paragraph
            type="secondary"
            style={{ marginTop: 8, marginBottom: 0, fontSize: 12 }}
          >
            {liveStopTarget.protocol === "hls"
              ? t("recordingFolderM3u8SegmentsMp4Output", { value0: liveStopTarget.filename })
              : t("recordingFolderFlv", { value0: liveStopTarget.filename, value1: liveStopTarget.isSplit
                    ? t("part001FlvAndOtherSegments", { value0: liveStopTarget.filename })
                    : `${liveStopTarget.filename}.flv` })}
          </Typography.Paragraph>
        ) : null}
      </Modal>
      <Modal
        title={chromiumBrowserMeta.title}
        open={Boolean(chromiumInstallGuide)}
        onCancel={() => setChromiumInstallGuide(null)}
        footer={null}
        width={680}
      >
        {chromiumInstallGuide && (
          <div style={{ marginTop: 12, display: "grid", gap: 16 }}>
            <div
              style={{
                padding: "18px 20px",
                borderRadius: 16,
                border: `1px solid ${token.colorBorderSecondary}`,
                background: `linear-gradient(135deg, ${token.colorInfoBg} 0%, ${token.colorBgContainer} 100%)`,
              }}
            >
              <Space align="start" size={14}>
                <div
                  style={{
                    width: 40,
                    height: 40,
                    borderRadius: 12,
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "center",
                    background: chromiumBrowserMeta.accentColor,
                    color: token.colorWhite,
                    flex: "0 0 auto",
                  }}
                >
                  {chromiumInstallGuide.browser === "edge" ? (
                    <EdgeIcon style={{ fontSize: 20 }} />
                  ) : (
                    <ChromeOutlined style={{ fontSize: 20 }} />
                  )}
                </div>
                <div>
                  <Typography.Title level={5} style={{ margin: 0 }}>
                    {t("installBrowserSteps", { browser: chromiumBrowserMeta.name })}</Typography.Title>
                </div>
              </Space>
            </div>
            <div
              style={{
                display: "flex",
                flexDirection: "column",
                gap: 12,
              }}
            >
              <div
                style={{
                  padding: "16px 18px",
                  borderRadius: 14,
                  border: `1px solid ${token.colorBorderSecondary}`,
                  background: token.colorBgContainer,
                }}
              >
                <Space
                  align="start"
                  size={14}
                  style={{ width: "100%", justifyContent: "space-between" }}
                >
                  <Space align="start" size={12}>
                    <div
                      style={{
                        width: 28,
                        height: 28,
                        borderRadius: 999,
                        background: token.colorPrimaryBg,
                        color: token.colorPrimary,
                        display: "flex",
                        alignItems: "center",
                        justifyContent: "center",
                        fontWeight: 600,
                        flex: "0 0 auto",
                      }}
                    >
                      1
                    </div>
                    <div>
                      <Typography.Text strong>
                        {t("openBrowserAddress", { browser: chromiumBrowserMeta.name })}</Typography.Text>
                      <Typography.Paragraph
                        type="secondary"
                        style={{ margin: "6px 0 0" }}
                      >
                        {t("browserExtensionsPage", { browser: chromiumBrowserMeta.name })}</Typography.Paragraph>
                      <div style={{ marginTop: 10 }}>
                        <Typography.Text
                          code
                          copyable={{ text: chromiumInstallGuide.guide.manual_url }}
                        >
                          {chromiumInstallGuide.guide.manual_url}
                        </Typography.Text>
                      </div>
                    </div>
                  </Space>
                  <Button
                    type="primary"
                    size="middle"
                    icon={
                      chromiumInstallGuide.browser === "edge" ? (
                        <EdgeIcon />
                      ) : (
                        <ChromeOutlined />
                      )
                    }
                    aria-label={t("openExtensionsPage", { value0: chromiumBrowserMeta.name })}
                    onClick={() =>
                      void handleOpenChromiumExtensionsPage(chromiumInstallGuide.browser)
                    }
                    style={{
                      height: 40,
                      paddingInline: 18,
                      background: chromiumBrowserMeta.accentColor,
                      borderColor: chromiumBrowserMeta.accentColor,
                    }}
                  >
                    {chromiumBrowserMeta.openButtonText}
                  </Button>
                </Space>
              </div>
              <div
                style={{
                  padding: "16px 18px",
                  borderRadius: 14,
                  border: `1px solid ${token.colorBorderSecondary}`,
                  background: token.colorBgContainer,
                }}
              >
                <Space align="start" size={12}>
                  <div
                    style={{
                      width: 28,
                      height: 28,
                      borderRadius: 999,
                      background: token.colorPrimaryBg,
                      color: token.colorPrimary,
                      display: "flex",
                      alignItems: "center",
                      justifyContent: "center",
                      fontWeight: 600,
                      flex: "0 0 auto",
                    }}
                  >
                    2
                  </div>
                  <div>
                    <Typography.Text strong>{t("turnOnDeveloperModeInTheTopRightCorner")}</Typography.Text>
                    <Typography.Paragraph
                      type="secondary"
                      style={{ margin: "6px 0 0" }}
                    >
                      {t("theBrowserWillThenShowTheButtonForLoadingLocal")}</Typography.Paragraph>
                  </div>
                </Space>
              </div>
              <div
                style={{
                  padding: "16px 18px",
                  borderRadius: 14,
                  border: `1px solid ${token.colorBorderSecondary}`,
                  background: token.colorBgContainer,
                }}
              >
                <Space align="start" size={12}>
                  <div
                    style={{
                      width: 28,
                      height: 28,
                      borderRadius: 999,
                      background: token.colorPrimaryBg,
                      color: token.colorPrimary,
                      display: "flex",
                      alignItems: "center",
                      justifyContent: "center",
                      fontWeight: 600,
                      flex: "0 0 auto",
                    }}
                  >
                    3
                  </div>
                  <div style={{ minWidth: 0 }}>
                    <Typography.Text strong>
                      {t("clickLoadUnpackedAndSelectTheFolderShownBelow")}</Typography.Text>
                    <Typography.Paragraph
                      type="secondary"
                      style={{ margin: "6px 0 0" }}
                    >
                      {t("thisChromiumExtensionFolderWorksWithBothChromeAndMicrosoft")}</Typography.Paragraph>
                    <div
                      style={{
                        marginTop: 10,
                        padding: "10px 12px",
                        borderRadius: 10,
                        background: token.colorFillQuaternary,
                        border: `1px dashed ${token.colorBorder}`,
                      }}
                    >
                      <Button
                        type="link"
                        icon={<FolderOpenOutlined />}
                        onClick={() => void handleOpenChromiumExtensionFolder()}
                        style={{
                          paddingInline: 0,
                          height: "auto",
                          whiteSpace: "normal",
                          textAlign: "left",
                        }}
                      >
                        {chromiumInstallGuide.guide.extension_path}
                      </Button>
                    </div>
                  </div>
                </Space>
              </div>
            </div>
          </div>
        )}
      </Modal>
      <Modal
        title={t("installFirefoxExtension")}
        open={Boolean(firefoxInstallGuide)}
        onCancel={() => setFirefoxInstallGuide(null)}
        footer={null}
        width={680}
      >
        {firefoxInstallGuide && (
          <div style={{ marginTop: 12, display: "grid", gap: 16 }}>
            <div
              style={{
                padding: "18px 20px",
                borderRadius: 16,
                border: `1px solid ${token.colorBorderSecondary}`,
                background: `linear-gradient(135deg, ${token.colorInfoBg} 0%, ${token.colorBgContainer} 100%)`,
              }}
            >
              <Space align="start" size={14}>
                <div
                  style={{
                    width: 40,
                    height: 40,
                    borderRadius: 12,
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "center",
                    background: "#ff7139",
                    color: token.colorWhite,
                    flex: "0 0 auto",
                  }}
                >
                  <FirefoxIcon style={{ fontSize: 20 }} />
                </div>
                <div>
                  <Typography.Title level={5} style={{ margin: 0 }}>
                    {t("followThese3StepsToInstallTheFirefoxExtension")}</Typography.Title>
                </div>
              </Space>
            </div>
            <div
              style={{
                display: "flex",
                flexDirection: "column",
                gap: 12,
              }}
            >
              <div
                style={{
                  padding: "16px 18px",
                  borderRadius: 14,
                  border: `1px solid ${token.colorBorderSecondary}`,
                  background: token.colorBgContainer,
                }}
              >
                <Space
                  align="start"
                  size={14}
                  style={{ width: "100%", justifyContent: "space-between" }}
                >
                  <Space align="start" size={12}>
                    <div
                      style={{
                        width: 28,
                        height: 28,
                        borderRadius: 999,
                        background: token.colorPrimaryBg,
                        color: token.colorPrimary,
                        display: "flex",
                        alignItems: "center",
                        justifyContent: "center",
                        fontWeight: 600,
                        flex: "0 0 auto",
                      }}
                    >
                      1
                    </div>
                    <div>
                      <Typography.Text strong>{t("openFirefoxAndEnterTheFollowingAddressInTheAddress")}</Typography.Text>
                      <Typography.Paragraph
                        type="secondary"
                        style={{ margin: "6px 0 0" }}
                      >
                        {t("thisOpensFirefoxSTemporaryAddOnDebuggingPage")}</Typography.Paragraph>
                      <div style={{ marginTop: 10 }}>
                        <Typography.Text
                          code
                          copyable={{ text: firefoxInstallGuide.manual_url }}
                        >
                          {firefoxInstallGuide.manual_url}
                        </Typography.Text>
                      </div>
                    </div>
                  </Space>
                  <Button
                    type="primary"
                    size="middle"
                    icon={<FirefoxIcon />}
                    aria-label={t("openFirefoxAddOnsPage")}
                    onClick={() => void handleOpenFirefoxAddonsPage()}
                    style={{ height: 40, paddingInline: 18, background: "#ff7139", borderColor: "#ff7139" }}
                  >
                    {t("openFirefox")}</Button>
                </Space>
              </div>
              <div
                style={{
                  padding: "16px 18px",
                  borderRadius: 14,
                  border: `1px solid ${token.colorBorderSecondary}`,
                  background: token.colorBgContainer,
                }}
              >
                <Space align="start" size={12}>
                  <div
                    style={{
                      width: 28,
                      height: 28,
                      borderRadius: 999,
                      background: token.colorPrimaryBg,
                      color: token.colorPrimary,
                      display: "flex",
                      alignItems: "center",
                      justifyContent: "center",
                      fontWeight: 600,
                      flex: "0 0 auto",
                    }}
                  >
                    2
                  </div>
                  <div>
                    <Typography.Text strong>{t("clickLoadTemporaryAddOn")}</Typography.Text>
                    <Typography.Paragraph
                      type="secondary"
                      style={{ margin: "6px 0 0" }}
                    >
                      {t("findTheTemporaryExtensionsSectionAndClickLoadTemporaryAdd")}</Typography.Paragraph>
                  </div>
                </Space>
              </div>
              <div
                style={{
                  padding: "16px 18px",
                  borderRadius: 14,
                  border: `1px solid ${token.colorBorderSecondary}`,
                  background: token.colorBgContainer,
                }}
              >
                <Space align="start" size={12}>
                  <div
                    style={{
                      width: 28,
                      height: 28,
                      borderRadius: 999,
                      background: token.colorPrimaryBg,
                      color: token.colorPrimary,
                      display: "flex",
                      alignItems: "center",
                      justifyContent: "center",
                      fontWeight: 600,
                      flex: "0 0 auto",
                    }}
                  >
                    3
                  </div>
                  <div style={{ minWidth: 0 }}>
                    <Typography.Text strong>
                      {t("inTheFilePickerSelectManifestJsonInTheFolder")}</Typography.Text>
                    <Typography.Paragraph
                      type="secondary"
                      style={{ margin: "6px 0 0" }}
                    >
                      {t("firefoxRequiresSelectingTheManifestJsonFileInsideTheFolder")}</Typography.Paragraph>
                    <div
                      style={{
                        marginTop: 10,
                        padding: "10px 12px",
                        borderRadius: 10,
                        background: token.colorFillQuaternary,
                        border: `1px dashed ${token.colorBorder}`,
                      }}
                    >
                      <Button
                        type="link"
                        icon={<FolderOpenOutlined />}
                        onClick={() => void handleOpenFirefoxExtensionFolder()}
                        style={{
                          paddingInline: 0,
                          height: "auto",
                          whiteSpace: "normal",
                          textAlign: "left",
                        }}
                      >
                        {firefoxInstallGuide.extension_path}
                      </Button>
                    </div>
                  </div>
                </Space>
              </div>
            </div>
          </div>
        )}
      </Modal>
    </Layout>
  );
}

type DeepLinkHandler = (deepLink: string) => void;

const deepLinkHandlers = new Set<DeepLinkHandler>();
const recentlyHandledDeepLinks = new Map<string, number>();
const DEEP_LINK_DEDUP_WINDOW_MS = 1500;
let deepLinkInitPromise: Promise<void> | null = null;

function shouldHandleDeepLink(deepLink: string): boolean {
  const now = Date.now();
  for (const [key, ts] of recentlyHandledDeepLinks) {
    if (now - ts > DEEP_LINK_DEDUP_WINDOW_MS) {
      recentlyHandledDeepLinks.delete(key);
    }
  }
  const last = recentlyHandledDeepLinks.get(deepLink);
  if (last !== undefined && now - last < DEEP_LINK_DEDUP_WINDOW_MS) {
    return false;
  }
  recentlyHandledDeepLinks.set(deepLink, now);
  return true;
}

function dispatchDeepLink(deepLink: string): void {
  for (const handler of deepLinkHandlers) {
    handler(deepLink);
  }
}

async function bringMainWindowToFront(): Promise<void> {
  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    const win = getCurrentWindow();
    if (await win.isMinimized()) {
      await win.unminimize();
    }
    await win.show();
    await win.setFocus();
  } catch (error) {
    console.debug("[m3u8quicker] bring main window to front failed", error);
  }
}

function ensureDeepLinkInit(): Promise<void> {
  if (deepLinkInitPromise) {
    return deepLinkInitPromise;
  }
  deepLinkInitPromise = (async () => {
    try {
      const { getCurrent, onOpenUrl } = await import(
        "@tauri-apps/plugin-deep-link"
      );
      await onOpenUrl((urls) => {
        urls.forEach(dispatchDeepLink);
      });
      const initialUrls = await getCurrent();
      initialUrls?.forEach(dispatchDeepLink);
    } catch (error) {
      console.debug("[m3u8quicker] deep link unavailable", error);
    }
  })();
  return deepLinkInitPromise;
}

function parseDownloadDraft(deepLink: string): Omit<DownloadDraft, "nonce"> | null {
  try {
    const parsed = new URL(deepLink);
    const action = (parsed.hostname || parsed.pathname.replace(/^\/+/, "")).toLowerCase();
    if (action !== "new-task") {
      return null;
    }

    const url = (parsed.searchParams.get("url") || "").trim();
    if (!url) {
      return null;
    }

    const extraHeaders = parsed.searchParams.get("extra_headers")?.trim() || undefined;
    const rawFileType = parsed.searchParams.get("file_type");
    const fileType = parseFileType(rawFileType);
    return { url, extraHeaders, fileType };
  } catch (error) {
    console.debug("[m3u8quicker] failed to parse deep link", deepLink, error);
    return null;
  }
}

function parseNewLiveRecordDraft(
  deepLink: string
): { url: string; extraHeaders?: string } | null {
  try {
    const parsed = new URL(deepLink);
    const action = (parsed.hostname || parsed.pathname.replace(/^\/+/, "")).toLowerCase();
    if (action !== "new-live-record") {
      return null;
    }

    const url = (parsed.searchParams.get("url") || "").trim();
    if (!url) {
      return null;
    }

    const extraHeaders = parsed.searchParams.get("extra_headers")?.trim() || undefined;
    return { url, extraHeaders };
  } catch (error) {
    console.debug("[m3u8quicker] failed to parse live record deep link", deepLink, error);
    return null;
  }
}

function parsePreviewDraft(
  deepLink: string
): { url: string; extraHeaders?: string; title?: string } | null {
  try {
    const parsed = new URL(deepLink);
    const action = (parsed.hostname || parsed.pathname.replace(/^\/+/, "")).toLowerCase();
    if (action !== "preview") {
      return null;
    }

    const url = (parsed.searchParams.get("url") || "").trim();
    if (!url) {
      return null;
    }

    const extraHeaders = parsed.searchParams.get("extra_headers")?.trim() || undefined;
    const title = parsed.searchParams.get("title")?.trim() || undefined;
    return { url, extraHeaders, title };
  } catch (error) {
    console.debug("[m3u8quicker] failed to parse preview deep link", deepLink, error);
    return null;
  }
}

async function ensureFfmpegReadyForPreview(
  onOpenFfmpegSettings: () => void
): Promise<boolean> {
  try {
    const [settings, ffmpegStatus] = await Promise.all([
      getAppSettings(),
      getFfmpegStatus(),
    ]);
    if (settings.ffmpeg_enabled && ffmpegStatus.kind === "installed") {
      return true;
    }
  } catch {
    // fall through to prompt
  }

  return await new Promise<boolean>((resolve) => {
    Modal.confirm({
      title: t("ffmpegRequiredForPreviews"),
      content: (
        <Typography.Paragraph style={{ marginBottom: 0 }}>
          {t("videoPreviewsUseFfmpegToExtractFramesEnableAndConfigure")}</Typography.Paragraph>
      ),
      okText: t("goToSettings"),
      cancelText: t("cancel"),
      onOk: () => {
        onOpenFfmpegSettings();
        resolve(false);
      },
      onCancel: () => resolve(false),
    });
  });
}

async function openPreviewWindowFromDeepLink(
  url: string,
  extraHeaders: string | undefined,
  title: string | undefined,
  onOpenFfmpegSettings: () => void
): Promise<void> {
  if (!(await ensureFfmpegReadyForPreview(onOpenFfmpegSettings))) {
    return;
  }
  let token: string | null = null;
  try {
    const isInlineDashJson = url.trim().startsWith("{");
    const sessionUrl = isInlineDashJson ? "inline-dash-json" : url;
    const sourceKind = isInlineDashJson ? "inline_dash_json" : undefined;
    const sourceText = isInlineDashJson ? url : undefined;
    const session = await createPreviewSession(
      sessionUrl,
      extraHeaders,
      sourceKind,
      sourceText
    );
    token = session.token;
    const previewParams = new URLSearchParams({
      view: "preview",
      token: session.token,
    });
    if (title) {
      previewParams.set("title", title);
    }
    const previewUrl = `/?${previewParams.toString()}`;

    const previewWindow = new WebviewWindow(session.window_label, {
      url: previewUrl,
      title: title ? t("videoPreview", { value0: title }) : t("videoPreview2"),
      width: 960,
      height: 720,
      minWidth: 720,
      minHeight: 480,
      resizable: true,
      center: true,
    });

    previewWindow.once("tauri://created", () => {
      void previewWindow.setFocus();
    });
    previewWindow.once("tauri://error", (event) => {
      console.error("Failed to create preview window", event);
      if (token) {
        void closePreviewSession(token);
      }
      message.error(t("failedToOpenPreviewWindow"));
    });
  } catch (error) {
    if (token) {
      void closePreviewSession(token);
    }
    console.error("[m3u8quicker] failed to open preview window", error);
    message.error(t("failedToGeneratePreview", { value0: formatPreviewError(error) }));
  }
}

function waitForLiveRecorded(id: string, timeoutMs = 60000): Promise<void> {
  return new Promise((resolve, reject) => {
    let unlisten: UnlistenFn | undefined;
    let settled = false;

    const finish = (cb: () => void) => {
      if (settled) return;
      settled = true;
      unlisten?.();
      clearTimeout(timer);
      cb();
    };

    const timer = setTimeout(() => {
      finish(() => reject(new Error(t("timedOutWaitingForRecordingToFinish"))));
    }, timeoutMs);

    listen<LiveProgressEvent>("live-progress", (event) => {
      const payload = event.payload;
      if (payload.id !== id) return;
      if (payload.status === "Recorded") {
        finish(resolve);
        return;
      }
      if (
        payload.status === "Cancelled" ||
        (typeof payload.status === "object" && "Failed" in payload.status)
      ) {
        finish(() => reject(new Error(t("recordingDidNotFinishSuccessfullyAndCannotBeConverted"))));
      }
    })
      .then((fn) => {
        if (settled) {
          fn();
          return;
        }
        unlisten = fn;
      })
      .catch((error) => {
        finish(() => reject(error));
      });
  });
}

function formatLiveStopError(error: unknown): string {
  if (!error) return t("unknownError");
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return String(error);
}

function formatPreviewError(error: unknown): string {
  if (!error) return t("unknownError");
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return String(error);
}

function parseBatchDownloadDraft(
  deepLink: string
): Omit<BatchDownloadDraft, "nonce"> | null {
  try {
    const parsed = new URL(deepLink);
    const action = (parsed.hostname || parsed.pathname.replace(/^\/+/, "")).toLowerCase();
    if (action !== "batch-download") {
      return null;
    }

    const rawInput = (parsed.searchParams.get("items") || "").trim();
    if (!rawInput) {
      return null;
    }

    const extraHeaders = parsed.searchParams.get("extra_headers")?.trim() || undefined;
    const rawFileTypes = parsed.searchParams.get("file_types");
    const fileTypes = rawFileTypes
      ? rawFileTypes.split(/\r?\n/).map((value) => parseFileType(value))
      : undefined;
    return { rawInput, extraHeaders, fileTypes };
  } catch (error) {
    console.debug("[m3u8quicker] failed to parse batch deep link", deepLink, error);
    return null;
  }
}

export default App;
