import { useEffect, useState } from "react";
import { Button, Layout, Modal, Popconfirm, Space, Tabs, Typography, message, theme } from "antd";
import { ChromeOutlined, ClearOutlined, FolderOpenOutlined } from "@ant-design/icons";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { Toolbar } from "./components/Toolbar";
import { DownloadList } from "./components/DownloadList";
import { NewDownloadModal } from "./components/NewDownloadModal";
import { SettingsModal } from "./components/SettingsModal";
import { ToolsModal, type ToolAction } from "./components/ToolsModal";
import { useDownloads } from "./hooks/useDownloads";
import {
  installChromeExtension,
  openChromeExtensionsPage,
  openFileLocation,
  openDownloadPlaybackSession,
} from "./services/api";
import type { ChromeExtensionInstallResult } from "./types";
import type { ThemeMode } from "./types/settings";

const { Header, Content } = Layout;

interface DownloadDraft {
  url: string;
  extraHeaders?: string;
  nonce: number;
}

interface AppProps {
  themeMode: ThemeMode;
  onThemeModeChange: (mode: ThemeMode) => void;
}

function App({ themeMode, onThemeModeChange }: AppProps) {
  const [modalOpen, setModalOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [toolModalOpen, setToolModalOpen] = useState(false);
  const [activeTool, setActiveTool] = useState<ToolAction | null>(null);
  const [downloadDraft, setDownloadDraft] = useState<DownloadDraft | null>(null);
  const [chromeInstallGuide, setChromeInstallGuide] =
    useState<ChromeExtensionInstallResult | null>(null);
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

  const handleOpenPlaybackWindow = async (id: string) => {
    try {
      const session = await openDownloadPlaybackSession(id);
      const existingWindow = await WebviewWindow.getByLabel(session.window_label);

      if (existingWindow) {
        await existingWindow.show();
        await existingWindow.setFocus();
        return;
      }

      const url = `/?${new URLSearchParams({
        view: "player",
        taskId: id,
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

  const handleInstallChromeExtension = async () => {
    try {
      const result = await installChromeExtension();
      setChromeInstallGuide(result);
    } catch (error) {
      console.error("Failed to open chrome extension installer", error);
      message.error(`打开安装引导失败: ${error}`);
    }
  };

  const handleOpenChromeExtensionsPage = async () => {
    try {
      const opened = await openChromeExtensionsPage();
      if (!opened) {
        message.warning("未找到 Chrome，请手动打开扩展页面");
      }
    } catch (error) {
      console.error("Failed to open chrome extensions page", error);
      message.error(`打开 Chrome 扩展页失败: ${error}`);
    }
  };

  const handleOpenChromeExtensionFolder = async () => {
    if (!chromeInstallGuide) return;

    try {
      await openFileLocation(chromeInstallGuide.extension_path);
      message.success("扩展目录已打开");
    } catch (error) {
      console.error("Failed to open chrome extension folder", error);
      message.error(`打开扩展目录失败: ${error}`);
    }
  };

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
          onPlay={(id) => {
            void handleOpenPlaybackWindow(id);
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
          onPlay={(id) => {
            void handleOpenPlaybackWindow(id);
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
              void handleInstallChromeExtension();
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
        title="安装 Chrome 扩展"
        open={Boolean(chromeInstallGuide)}
        onCancel={() => setChromeInstallGuide(null)}
        footer={null}
        width={680}
      >
        {chromeInstallGuide && (
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
                    background: token.colorPrimary,
                    color: token.colorWhite,
                    flex: "0 0 auto",
                  }}
                >
                  <ChromeOutlined style={{ fontSize: 20 }} />
                </div>
                <div>
                  <Typography.Title level={5} style={{ margin: 0 }}>
                    请按以下 3 步完成 Chrome 扩展安装
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
                      <Typography.Text strong>打开Chrome浏览器，在地址栏输入下面的地址并回车</Typography.Text>
                      <Typography.Paragraph
                        type="secondary"
                        style={{ margin: "6px 0 0" }}
                      >
                        打开后会进入 Chrome 的扩展管理页。
                      </Typography.Paragraph>
                      <div style={{ marginTop: 10 }}>
                        <Typography.Text
                          code
                          copyable={{ text: chromeInstallGuide.manual_url }}
                        >
                          {chromeInstallGuide.manual_url}
                        </Typography.Text>
                      </div>
                    </div>
                  </Space>
                  <Button
                    type="primary"
                    size="middle"
                    icon={<ChromeOutlined />}
                    aria-label="打开 Chrome 扩展页"
                    onClick={() => void handleOpenChromeExtensionsPage()}
                    style={{ height: 40, paddingInline: 18 }}
                  >
                    打开Chrome
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
                      开启后，Chrome 会显示用于加载本地扩展的按钮。
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
                      应用会先准备一个可直接选择的本地目录，你只需要打开并选中它。
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
                        onClick={() => void handleOpenChromeExtensionFolder()}
                        style={{
                          paddingInline: 0,
                          height: "auto",
                          whiteSpace: "normal",
                          textAlign: "left",
                        }}
                      >
                        {chromeInstallGuide.extension_path}
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
    return { url, extraHeaders };
  } catch (error) {
    console.debug("[m3u8quicker] failed to parse deep link", deepLink, error);
    return null;
  }
}

export default App;
