import { useEffect, useState } from "react";
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
  createPreviewSession,
  closePreviewSession,
  getAppSettings,
  getFfmpegStatus,
  checkForUpdate,
  convertMediaFile,
  convertLiveHlsToMp4,
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
  nonce: number;
}

interface AppProps {
  themeMode: ThemeMode;
  onThemeModeChange: (mode: ThemeMode) => void;
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
    title: "安装 Chrome 扩展",
    name: "Chrome",
    shortName: "Chrome",
    openButtonText: "打开Chrome",
    accentColor: "#4285f4",
  },
  edge: {
    title: "安装 Microsoft Edge 扩展",
    name: "Microsoft Edge",
    shortName: "Edge",
    openButtonText: "打开Edge",
    accentColor: "#0f6cbd",
  },
};

function App({ themeMode, onThemeModeChange }: AppProps) {
  const [modalOpen, setModalOpen] = useState(false);
  const [liveRecordModalOpen, setLiveRecordModalOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsInitialTab, setSettingsInitialTab] = useState<
    "general" | "download" | "ffmpeg"
  >("general");
  const [toolModalOpen, setToolModalOpen] = useState(false);
  const [activeTool, setActiveTool] = useState<ToolAction | null>(null);
  const [downloadDraft, setDownloadDraft] = useState<DownloadDraft | null>(null);
  const [batchDownloadDraft, setBatchDownloadDraft] = useState<BatchDownloadDraft | null>(null);
  const [liveRecordDraft, setLiveRecordDraft] = useState<{
    url: string;
    extraHeaders?: string;
    nonce: number;
  } | null>(null);
  const [batchDownloadModalOpen, setBatchDownloadModalOpen] = useState(false);
  const [videoPreviewModalOpen, setVideoPreviewModalOpen] = useState(false);
  const [updateAvailable, setUpdateAvailable] = useState(false);
  const [chromiumInstallGuide, setChromiumInstallGuide] =
    useState<ChromiumInstallGuideState | null>(null);
  const [firefoxInstallGuide, setFirefoxInstallGuide] =
    useState<FirefoxExtensionInstallResult | null>(null);
  const [liveStopTarget, setLiveStopTarget] = useState<{
    id: string;
    filename: string;
    filePath: string | null;
    protocol: LiveProtocol;
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
  } = useDownloads();
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
  } = useLiveRecords();
  const { token } = theme.useToken();

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
      message.warning("当前格式暂不支持边下边播，请等待下载完成后再播放");
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
        sessionToken: session.session_token,
        filename: session.filename,
      }).toString()}`;

      const playerWindow = new WebviewWindow(session.window_label, {
        url,
        title: `播放中 - ${session.filename}`,
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
        message.error("打开播放器窗口失败");
      });
    } catch (error) {
      console.error("Failed to open playback window", error);
      message.error(`打开播放器失败: ${error}`);
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
    });
  };

  const performStopLive = async (convertToMp4Flag: boolean) => {
    if (!liveStopTarget) return;
    const { id, filename, filePath, protocol } = liveStopTarget;
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
        content: `录制完成后将转换 ${filename} 为 MP4...`,
        duration: 0,
      });
      await recordedPromise;
      let finalPath: string;
      if (protocol === "hls") {
        finalPath = await convertLiveHlsToMp4(id);
      } else {
        if (!filePath) {
          message.warning({
            key: messageKey,
            content: "未找到已录制文件，无法转换为 MP4",
          });
          return;
        }
        const outputPath = deriveMp4PathFromFlv(filePath);
        finalPath = await convertMediaFile(filePath, outputPath, "mp4", "quick");
      }
      message.success({
        key: messageKey,
        content: `已转换为 MP4：${finalPath}`,
      });
    } catch (err) {
      message.error({
        key: messageKey,
        content: `转换为 MP4 失败: ${formatLiveStopError(err)}`,
      });
    }
  };

  const handleInstallChromiumExtension = async (browser: ChromiumBrowser) => {
    try {
      const guide = await installChromiumExtension(browser);
      setChromiumInstallGuide({ browser, guide });
    } catch (error) {
      console.error("Failed to open chromium extension installer", error);
      message.error(`打开安装引导失败: ${error}`);
    }
  };

  const handleOpenChromiumExtensionsPage = async (browser: ChromiumBrowser) => {
    const browserName = CHROMIUM_BROWSER_META[browser].name;

    try {
      const opened = await openChromiumExtensionsPage(browser);
      if (!opened) {
        message.warning(`未找到 ${browserName}，请手动打开扩展页面`);
      }
    } catch (error) {
      console.error("Failed to open chromium extensions page", error);
      message.error(`打开 ${browserName} 扩展页失败: ${error}`);
    }
  };

  const handleOpenChromiumExtensionFolder = async () => {
    if (!chromiumInstallGuide) return;

    try {
      await openFileLocation(chromiumInstallGuide.guide.extension_path);
      message.success("扩展目录已打开");
    } catch (error) {
      console.error("Failed to open chromium extension folder", error);
      message.error(`打开扩展目录失败: ${error}`);
    }
  };

  const handleInstallFirefoxExtension = async () => {
    try {
      const result = await installFirefoxExtension();
      setFirefoxInstallGuide(result);
    } catch (error) {
      console.error("Failed to open firefox extension installer", error);
      message.error(`打开安装引导失败: ${error}`);
    }
  };

  const handleOpenFirefoxAddonsPage = async () => {
    try {
      const opened = await openFirefoxAddonsPage();
      if (!opened) {
        message.warning("未找到 Firefox，请手动打开附加组件页面");
      }
    } catch (error) {
      console.error("Failed to open firefox addons page", error);
      message.error(`打开 Firefox 附加组件页失败: ${error}`);
    }
  };

  const handleOpenFirefoxExtensionFolder = async () => {
    if (!firefoxInstallGuide) return;

    try {
      await openFileLocation(firefoxInstallGuide.extension_path);
      message.success("扩展目录已打开");
    } catch (error) {
      console.error("Failed to open firefox extension folder", error);
      message.error(`打开扩展目录失败: ${error}`);
    }
  };

  const chromiumBrowserMeta = CHROMIUM_BROWSER_META[chromiumInstallGuide?.browser ?? "chrome"];

  const liveRecordingItems = liveRecording.map(liveRecordToDownloadSummary);
  const liveRecordedItems = liveRecorded.map(liveRecordToDownloadSummary);

  const liveStatusTagOverride = (status: DownloadStatus) => {
    if (status === "Downloading") return <Tag color="processing">录制中</Tag>;
    if (status === "Paused") return <Tag color="warning">已暂停</Tag>;
    if (status === "Completed") return <Tag color="success">已录制</Tag>;
    if (status === "Cancelled") return <Tag color="default">已取消</Tag>;
    if (typeof status === "object" && "Failed" in status)
      return <Tag color="error">失败</Tag>;
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
      label: `下载中 (${counts.active_count})`,
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
      label: `已完成 (${counts.history_count})`,
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
          showActions={["play", "remove", "open"]}
          showSpeed={false}
          actionsHeaderExtra={
            <Popconfirm
              title="确认清空列表?"
              description="只删除已完成列表记录，不删除本地文件。"
              onConfirm={() => void clearCompleted()}
              okText="清空列表"
              cancelText="取消"
              disabled={counts.history_count === 0}
            >
              <Button
                type="text"
                size="small"
                danger
                icon={<ClearOutlined />}
                aria-label="清空列表"
                disabled={counts.history_count === 0}
              />
            </Popconfirm>
          }
        />
      ),
    },
    {
      key: "live-recording",
      label: `直播录制中 (${liveCounts.active_count})`,
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
          loading={loadingLiveActive}
          showActions={["pause", "resume", "stop", "cancel", "open"]}
          statusTagOverride={liveStatusTagOverride}
          cancelLabels={{
            title: "确认取消录制?",
            description: "取消后会删除已录制的文件，无法恢复。",
            okText: "取消并删除",
            cancelText: "继续录制",
          }}
        />
      ),
    },
    {
      key: "live-recorded",
      label: `录制完成 (${liveCounts.history_count})`,
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
          loading={loadingLiveHistory}
          showActions={["remove", "open"]}
          showSpeed={false}
          statusTagOverride={liveStatusTagOverride}
          actionsHeaderExtra={
            <Popconfirm
              title="确认清空列表?"
              description="只删除录制完成列表记录，不删除本地文件。"
              onConfirm={() => void clearLiveCompleted()}
              okText="清空列表"
              cancelText="取消"
              disabled={liveCounts.history_count === 0}
            >
              <Button
                type="text"
                size="small"
                danger
                icon={<ClearOutlined />}
                aria-label="清空列表"
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
            setToolModalOpen(true);
          }}
          onOpenSettings={() => {
            setSettingsInitialTab("general");
            setSettingsOpen(true);
          }}
          updateAvailable={updateAvailable}
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
        onSubmit={async (params) => {
          await addDownload(params);
          setModalOpen(false);
        }}
      />
      <SettingsModal
        open={settingsOpen}
        initialTab={settingsInitialTab}
        themeMode={themeMode}
        updateAvailable={updateAvailable}
        onClose={() => {
          setSettingsOpen(false);
          setSettingsInitialTab("general");
        }}
        onThemeModeChange={onThemeModeChange}
        onUpdateAvailabilityChange={setUpdateAvailable}
      />
      <ToolsModal
        open={toolModalOpen}
        tool={activeTool}
        onClose={() => {
          setToolModalOpen(false);
          setActiveTool(null);
        }}
      />
      <BatchDownloadModal
        open={batchDownloadModalOpen}
        initialRawInput={batchDownloadDraft?.rawInput}
        initialExtraHeaders={batchDownloadDraft?.extraHeaders}
        resetKey={batchDownloadDraft?.nonce ?? 0}
        onClose={() => {
          setBatchDownloadModalOpen(false);
          setBatchDownloadDraft(null);
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
        initialUrl={liveRecordDraft?.url}
        initialExtraHeaders={liveRecordDraft?.extraHeaders}
        resetKey={liveRecordDraft?.nonce ?? 0}
      />
      <Modal
        title="停止录制"
        open={Boolean(liveStopTarget)}
        onCancel={() => setLiveStopTarget(null)}
        footer={
          <Space>
            <Button onClick={() => setLiveStopTarget(null)}>取消</Button>
            <Button onClick={() => void performStopLive(false)}>仅停止</Button>
            <Button type="primary" onClick={() => void performStopLive(true)}>
              是，转成 MP4
            </Button>
          </Space>
        }
      >
        <Typography.Paragraph style={{ marginBottom: 0 }}>
          {liveStopTarget?.protocol === "hls"
            ? "是否将录制好的 HLS 分片合并为 MP4？保留的分片 + 本地 m3u8 会留在录制目录里，可以直接重新合并。"
            : "是否将录制好的 FLV 转成 MP4？转换后浏览器、微信等场景都能直接播放，原 FLV 文件保留。"}
        </Typography.Paragraph>
        {liveStopTarget?.filename ? (
          <Typography.Paragraph
            type="secondary"
            style={{ marginTop: 8, marginBottom: 0, fontSize: 12 }}
          >
            {liveStopTarget.protocol === "hls"
              ? `录制目录：${liveStopTarget.filename}/`
              : `文件：${liveStopTarget.filename}.flv`}
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
                    请按以下 3 步完成 {chromiumBrowserMeta.name} 扩展安装
                  </Typography.Title>
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
                        打开 {chromiumBrowserMeta.name} 浏览器，在地址栏输入下面的地址并回车
                      </Typography.Text>
                      <Typography.Paragraph
                        type="secondary"
                        style={{ margin: "6px 0 0" }}
                      >
                        打开后会进入 {chromiumBrowserMeta.name} 的扩展管理页。
                      </Typography.Paragraph>
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
                    aria-label={`打开 ${chromiumBrowserMeta.name} 扩展页`}
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
                    <Typography.Text strong>打开右上角“开发者模式”开关</Typography.Text>
                    <Typography.Paragraph
                      type="secondary"
                      style={{ margin: "6px 0 0" }}
                    >
                      开启后，浏览器会显示用于加载本地扩展的按钮。
                    </Typography.Paragraph>
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
                      点击“加载未打包的扩展程序”，然后选择下面展示的目录
                    </Typography.Text>
                    <Typography.Paragraph
                      type="secondary"
                      style={{ margin: "6px 0 0" }}
                    >
                      这是 Chromium 通用扩展目录，Chrome 和 Microsoft Edge 都可以直接使用。
                    </Typography.Paragraph>
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
        title="安装 Firefox 扩展"
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
                    请按以下 3 步完成 Firefox 扩展安装
                  </Typography.Title>
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
                      <Typography.Text strong>打开 Firefox 浏览器，在地址栏输入下面的地址并回车</Typography.Text>
                      <Typography.Paragraph
                        type="secondary"
                        style={{ margin: "6px 0 0" }}
                      >
                        打开后会进入 Firefox 的临时附加组件调试页。
                      </Typography.Paragraph>
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
                    aria-label="打开 Firefox 附加组件页"
                    onClick={() => void handleOpenFirefoxAddonsPage()}
                    style={{ height: 40, paddingInline: 18, background: "#ff7139", borderColor: "#ff7139" }}
                  >
                    打开Firefox
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
                    <Typography.Text strong>点击"加载临时附加组件..."按钮</Typography.Text>
                    <Typography.Paragraph
                      type="secondary"
                      style={{ margin: "6px 0 0" }}
                    >
                      在页面中找到"临时扩展"区域，点击"加载临时附加组件..."。
                    </Typography.Paragraph>
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
                      在弹出的文件选择器中，选择下面目录中的 manifest.json 文件
                    </Typography.Text>
                    <Typography.Paragraph
                      type="secondary"
                      style={{ margin: "6px 0 0" }}
                    >
                      与 Chrome 不同，Firefox 需要选择目录中的 manifest.json 文件而非目录本身。
                    </Typography.Paragraph>
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
): { url: string; extraHeaders?: string } | null {
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
    return { url, extraHeaders };
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
      title: "预览需要 FFmpeg",
      content: (
        <Typography.Paragraph style={{ marginBottom: 0 }}>
          视频预览需要 FFmpeg 抽帧，请先在设置中开启并配置 FFmpeg。
        </Typography.Paragraph>
      ),
      okText: "前往设置",
      cancelText: "取消",
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
    const previewUrl = `/?${new URLSearchParams({
      view: "preview",
      token: session.token,
    }).toString()}`;

    const previewWindow = new WebviewWindow(session.window_label, {
      url: previewUrl,
      title: "视频预览",
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
      message.error("打开预览窗口失败");
    });
  } catch (error) {
    if (token) {
      void closePreviewSession(token);
    }
    console.error("[m3u8quicker] failed to open preview window", error);
    message.error(`生成预览失败: ${formatPreviewError(error)}`);
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
      finish(() => reject(new Error("等待录制完成超时")));
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
        finish(() => reject(new Error("录制未正常结束，无法转换")));
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

function deriveMp4PathFromFlv(flvPath: string): string {
  if (/\.flv$/i.test(flvPath)) {
    return flvPath.replace(/\.flv$/i, ".mp4");
  }
  return `${flvPath}.mp4`;
}

function formatLiveStopError(error: unknown): string {
  if (!error) return "未知错误";
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return String(error);
}

function formatPreviewError(error: unknown): string {
  if (!error) return "未知错误";
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
    return { rawInput, extraHeaders };
  } catch (error) {
    console.debug("[m3u8quicker] failed to parse batch deep link", deepLink, error);
    return null;
  }
}

export default App;
