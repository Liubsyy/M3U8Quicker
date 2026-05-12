import { useEffect, useRef, useState } from "react";
import {
  Alert,
  Button,
  Modal,
  Progress,
  Space,
  Spin,
  Typography,
  message,
} from "antd";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  checkForUpdate,
  downloadUpdateInstaller,
  openFileLocation,
  openUpdateInstaller,
  openUrl,
} from "../services/api";
import type {
  UpdateDownloadProgress,
  UpdateInfo,
} from "../types/update";

type Phase =
  | "checking"
  | "no-update"
  | "has-update"
  | "no-asset"
  | "downloading"
  | "downloaded"
  | "error";

interface UpdateModalProps {
  open: boolean;
  onClose: () => void;
  onChecked?: (info: UpdateInfo) => void;
}

export function UpdateModal({ open, onClose, onChecked }: UpdateModalProps) {
  const [phase, setPhase] = useState<Phase>("checking");
  const [info, setInfo] = useState<UpdateInfo | null>(null);
  const [errorText, setErrorText] = useState("");
  const [progress, setProgress] = useState(0);
  const [downloadedBytes, setDownloadedBytes] = useState(0);
  const [totalBytes, setTotalBytes] = useState(0);
  const [installerPath, setInstallerPath] = useState("");
  const [openingInstaller, setOpeningInstaller] = useState(false);
  const unlistenRef = useRef<UnlistenFn | null>(null);

  useEffect(() => {
    if (!open) {
      return;
    }
    void runCheck();
    return () => {
      unlistenRef.current?.();
      unlistenRef.current = null;
    };
  }, [open]);

  const runCheck = async () => {
    setPhase("checking");
    setErrorText("");
    setInfo(null);
    setProgress(0);
    setDownloadedBytes(0);
    setTotalBytes(0);
    setInstallerPath("");

    try {
      const result = await checkForUpdate();
      setInfo(result);
      onChecked?.(result);
      if (!result.has_update) {
        setPhase("no-update");
      } else if (!result.asset) {
        setPhase("no-asset");
      } else {
        setPhase("has-update");
      }
    } catch (error) {
      setErrorText(formatError(error));
      setPhase("error");
    }
  };

  const handleDownload = async () => {
    if (!info?.asset) return;

    setPhase("downloading");
    setProgress(0);
    setDownloadedBytes(0);
    setTotalBytes(info.asset.size);

    try {
      const unlisten = await listen<UpdateDownloadProgress>(
        "update-download-progress",
        (event) => {
          const { downloaded_bytes, total_bytes, stage } = event.payload;
          if (stage === "downloading" || stage === "done") {
            setDownloadedBytes(downloaded_bytes);
            if (total_bytes > 0) {
              setTotalBytes(total_bytes);
              setProgress(
                Math.min(100, Math.round((downloaded_bytes / total_bytes) * 100))
              );
            }
          }
        }
      );
      unlistenRef.current = unlisten;

      const path = await downloadUpdateInstaller(info.asset);
      setInstallerPath(path);
      setProgress(100);
      setPhase("downloaded");
    } catch (error) {
      setErrorText(formatError(error));
      setPhase("error");
    } finally {
      unlistenRef.current?.();
      unlistenRef.current = null;
    }
  };

  const handleOpenReleasePage = async () => {
    const url = info?.release_url || "https://github.com/Liubsyy/M3U8Quicker/releases";
    try {
      await openUrl(url);
    } catch (error) {
      message.error(`打开发布页失败: ${formatError(error)}`);
    }
  };

  const handleOpenInstaller = async () => {
    if (!installerPath) return;
    setOpeningInstaller(true);
    try {
      await openUpdateInstaller(installerPath);
    } catch (error) {
      setOpeningInstaller(false);
      message.error(`打开安装包失败: ${formatError(error)}`);
    }
  };

  const handleOpenInstallerFolder = async () => {
    if (!installerPath) return;
    try {
      await openFileLocation(installerPath);
    } catch (error) {
      message.error(`打开目录失败: ${formatError(error)}`);
    }
  };

  const renderContent = () => {
    if (phase === "checking") {
      return (
        <Space direction="vertical" align="center" style={{ width: "100%", padding: "24px 0" }}>
          <Spin />
          <Typography.Text type="secondary">正在检查更新…</Typography.Text>
        </Space>
      );
    }

    if (phase === "no-update" && info) {
      return (
        <Space direction="vertical" size={12} style={{ width: "100%" }}>
          <Alert
            type="success"
            showIcon
            message="已经是最新版本"
            description={`当前版本 v${info.current_version}`}
          />
        </Space>
      );
    }

    if (phase === "error") {
      return (
        <Space direction="vertical" size={12} style={{ width: "100%" }}>
          <Alert type="error" showIcon message="检查更新失败" description={errorText} />
        </Space>
      );
    }

    if (phase === "no-asset" && info) {
      return (
        <Space direction="vertical" size={12} style={{ width: "100%" }}>
          <Alert
            type="warning"
            showIcon
            message={`发现新版本 v${info.latest_version}`}
            description="未找到与当前平台匹配的安装包，请前往发布页手动下载。"
          />
          {info.release_notes && (
            <ReleaseNotes notes={info.release_notes} />
          )}
        </Space>
      );
    }

    if (phase === "has-update" && info?.asset) {
      return (
        <Space direction="vertical" size={12} style={{ width: "100%" }}>
          <Alert
            type="info"
            showIcon
            message={`发现新版本 v${info.latest_version}`}
            description={`当前版本 v${info.current_version}`}
          />
          <div>
            <Typography.Text strong>安装包：</Typography.Text>
            <Typography.Text>{info.asset.name}</Typography.Text>
            <Typography.Text type="secondary" style={{ marginLeft: 8 }}>
              ({formatSize(info.asset.size)})
            </Typography.Text>
          </div>
          {info.release_notes && <ReleaseNotes notes={info.release_notes} />}
          <PlatformInstallTips />
        </Space>
      );
    }

    if (phase === "downloading" && info?.asset) {
      return (
        <Space direction="vertical" size={12} style={{ width: "100%" }}>
          <Typography.Text>正在下载 {info.asset.name}</Typography.Text>
          <Progress percent={progress} />
          <Typography.Text type="secondary">
            {formatSize(downloadedBytes)} / {formatSize(totalBytes || info.asset.size)}
          </Typography.Text>
        </Space>
      );
    }

    if (phase === "downloaded") {
      return (
        <Space direction="vertical" size={12} style={{ width: "100%" }}>
          <Alert
            type="success"
            showIcon
            message="下载完成"
            description={
              <Typography.Text style={{ wordBreak: "break-all" }}>
                {installerPath}
              </Typography.Text>
            }
          />
          <PlatformInstallTips />
        </Space>
      );
    }

    return null;
  };

  const renderFooter = () => {
    if (phase === "checking" || phase === "downloading") {
      return null;
    }

    if (phase === "no-update") {
      return [
        <Button key="close" type="primary" onClick={onClose}>
          关闭
        </Button>,
      ];
    }

    if (phase === "error") {
      return [
        <Button key="release" onClick={() => void handleOpenReleasePage()}>
          前往发布页
        </Button>,
        <Button key="retry" type="primary" onClick={() => void runCheck()}>
          重试
        </Button>,
      ];
    }

    if (phase === "no-asset") {
      return [
        <Button key="close" onClick={onClose}>
          关闭
        </Button>,
        <Button key="release" type="primary" onClick={() => void handleOpenReleasePage()}>
          前往发布页
        </Button>,
      ];
    }

    if (phase === "has-update") {
      return [
        <Button key="release" onClick={() => void handleOpenReleasePage()}>
          前往发布页
        </Button>,
        <Button key="download" type="primary" onClick={() => void handleDownload()}>
          下载并安装
        </Button>,
      ];
    }

    if (phase === "downloaded") {
      return [
        <Button key="folder" onClick={() => void handleOpenInstallerFolder()}>
          打开所在目录
        </Button>,
        <Button
          key="install"
          type="primary"
          loading={openingInstaller}
          onClick={() => void handleOpenInstaller()}
        >
          打开安装程序并退出
        </Button>,
      ];
    }

    return null;
  };

  const closable = phase !== "downloading";

  return (
    <Modal
      title="检查更新"
      open={open}
      onCancel={() => {
        if (closable) onClose();
      }}
      maskClosable={closable}
      closable={closable}
      footer={renderFooter()}
      width={560}
      destroyOnHidden
    >
      {renderContent()}
    </Modal>
  );
}

function ReleaseNotes({ notes }: { notes: string }) {
  return (
    <div>
      <Typography.Text strong>更新内容：</Typography.Text>
      <Typography.Paragraph
        style={{
          marginTop: 8,
          marginBottom: 0,
          maxHeight: 220,
          overflow: "auto",
          whiteSpace: "pre-wrap",
          fontSize: 13,
        }}
      >
        {notes}
      </Typography.Paragraph>
    </div>
  );
}

function PlatformInstallTips() {
  const platform = detectPlatform();
  const tip = (() => {
    switch (platform) {
      case "windows":
        return "将启动安装程序，本应用会自动关闭，安装完成后请重新启动。";
      case "macos":
        return "将打开 dmg 镜像，本应用会自动退出，请把新版应用拖到「应用程序」覆盖旧版本。";
      case "linux":
        return "本应用会自动退出，AppImage 可直接运行；.deb / .rpm 请使用包管理器安装。";
      default:
        return "本应用会在打开安装程序时自动退出，请按下载到的安装包类型继续安装。";
    }
  })();

  return <Alert type="info" showIcon message="安装提示" description={tip} />;
}

function detectPlatform(): "windows" | "macos" | "linux" | "unknown" {
  if (typeof navigator === "undefined") return "unknown";
  const ua = navigator.userAgent.toLowerCase();
  if (ua.includes("windows")) return "windows";
  if (ua.includes("mac os") || ua.includes("macintosh")) return "macos";
  if (ua.includes("linux")) return "linux";
  return "unknown";
}

function formatSize(bytes: number): string {
  if (!bytes || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  let value = bytes;
  let i = 0;
  while (value >= 1024 && i < units.length - 1) {
    value /= 1024;
    i += 1;
  }
  return `${value.toFixed(value >= 100 || i === 0 ? 0 : 1)} ${units[i]}`;
}

function formatError(error: unknown): string {
  if (!error) return "未知错误";
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return String(error);
}
