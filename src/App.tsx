import { useEffect, useState } from "react";
import { Button, Layout, Modal, Popconfirm, Space, Tabs, Typography, message, theme } from "antd";
import { ChromeOutlined, ClearOutlined, FolderOpenOutlined } from "@ant-design/icons";
import { EdgeIcon } from "./components/EdgeIcon";
import { FirefoxIcon } from "./components/FirefoxIcon";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { Toolbar } from "./components/Toolbar";
import { DownloadList } from "./components/DownloadList";
import { NewDownloadModal } from "./components/NewDownloadModal";
import { SettingsModal } from "./components/SettingsModal";
import { ToolsModal, type ToolAction } from "./components/ToolsModal";
import { useDownloads } from "./hooks/useDownloads";
import {
  installChromiumExtension,
  openChromiumExtensionsPage,
  installFirefoxExtension,
  openFirefoxAddonsPage,
  openFileLocation,
  openDownloadPlaybackSession,
} from "./services/api";
import type {
  ChromiumBrowser,
  ChromiumExtensionInstallResult,
  FirefoxExtensionInstallResult,
  DownloadTaskSummary,
} from "./types";
import { canOpenInProgressPlayback, isDirectFileType, parseFileType } from "./types";
import type { ThemeMode } from "./types/settings";

const { Header, Content } = Layout;

interface DownloadDraft {
  url: string;
  extraHeaders?: string;
  fileType?: import("./types").FileType;
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
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [toolModalOpen, setToolModalOpen] = useState(false);
  const [activeTool, setActiveTool] = useState<ToolAction | null>(null);
  const [downloadDraft, setDownloadDraft] = useState<DownloadDraft | null>(null);
  const [chromiumInstallGuide, setChromiumInstallGuide] =
    useState<ChromiumInstallGuideState | null>(null);
  const [firefoxInstallGuide, setFirefoxInstallGuide] =
    useState<FirefoxExtensionInstallResult | null>(null);
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
  const { token } = theme.useToken();

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;

    const openDraftFromDeepLink = (deepLink: string) => {
      const draft = parseDownloadDraft(deepLink);
      if (!draft) {
        return;
      }

      setDownloadDraft({
        ...draft,
        nonce: Date.now(),
      });
      setModalOpen(true);
    };

    const initDeepLink = async () => {
      try {
        const { getCurrent, onOpenUrl } = await import(
          "@tauri-apps/plugin-deep-link"
        );

        const initialUrls = await getCurrent();
        if (!disposed && initialUrls?.length) {
          initialUrls.forEach(openDraftFromDeepLink);
        }

        unlisten = await onOpenUrl((urls) => {
          urls.forEach(openDraftFromDeepLink);
        });
      } catch (error) {
        console.debug("[m3u8quicker] deep link unavailable", error);
      }
    };

    initDeepLink();

    return () => {
      disposed = true;
      unlisten?.();
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
          onOpenSettings={() => setSettingsOpen(true)}
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
        onSubmit={async (params) => {
          await addDownload(params);
          setModalOpen(false);
        }}
      />
      <SettingsModal
        open={settingsOpen}
        themeMode={themeMode}
        onClose={() => setSettingsOpen(false)}
        onThemeModeChange={onThemeModeChange}
      />
      <ToolsModal
        open={toolModalOpen}
        tool={activeTool}
        onClose={() => {
          setToolModalOpen(false);
          setActiveTool(null);
        }}
      />
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

export default App;
