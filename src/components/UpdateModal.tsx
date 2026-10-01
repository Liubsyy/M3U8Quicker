import { t, useTranslation } from "../i18n";
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
  useTranslation();
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
      message.error(t("failedToOpenReleasePage", { value0: formatError(error) }));
    }
  };

  const handleOpenInstaller = async () => {
    if (!installerPath) return;
    setOpeningInstaller(true);
    try {
      await openUpdateInstaller(installerPath);
    } catch (error) {
      setOpeningInstaller(false);
      message.error(t("failedToOpenInstaller", { value0: formatError(error) }));
    }
  };

  const handleOpenInstallerFolder = async () => {
    if (!installerPath) return;
    try {
      await openFileLocation(installerPath);
    } catch (error) {
      message.error(t("failedToOpenFolder", { value0: formatError(error) }));
    }
  };

  const renderContent = () => {
    if (phase === "checking") {
      return (
        <Space direction="vertical" align="center" style={{ width: "100%", padding: "24px 0" }}>
          <Spin />
          <Typography.Text type="secondary">{t("checkingForUpdates")}</Typography.Text>
        </Space>
      );
    }

    if (phase === "no-update" && info) {
      return (
        <Space direction="vertical" size={12} style={{ width: "100%" }}>
          <Alert
            type="success"
            showIcon
            message={t("youAreUpToDate")}
            description={t("currentVersionV", { value0: info.current_version })}
          />
        </Space>
      );
    }

    if (phase === "error") {
      return (
        <Space direction="vertical" size={12} style={{ width: "100%" }}>
          <Alert type="error" showIcon message={t("failedToCheckForUpdates")} description={errorText} />
        </Space>
      );
    }

    if (phase === "no-asset" && info) {
      return (
        <Space direction="vertical" size={12} style={{ width: "100%" }}>
          <Alert
            type="warning"
            showIcon
            message={t("newVersionAvailableV", { value0: info.latest_version })}
            description={t("noInstallerFoundForThisPlatformDownloadManuallyFromThe")}
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
            message={t("newVersionAvailableV", { value0: info.latest_version })}
            description={t("currentVersionV", { value0: info.current_version })}
          />
          <div>
            <Typography.Text strong>{t("installer")}</Typography.Text>
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
          <Typography.Text>{t("downloadingInstaller", { name: info.asset.name })}</Typography.Text>
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
            message={t("downloadComplete")}
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
          {t("close")}</Button>,
      ];
    }

    if (phase === "error") {
      return [
        <Button key="release" onClick={() => void handleOpenReleasePage()}>
          {t("openReleasePage")}</Button>,
        <Button key="retry" type="primary" onClick={() => void runCheck()}>
          {t("retry")}</Button>,
      ];
    }

    if (phase === "no-asset") {
      return [
        <Button key="close" onClick={onClose}>
          {t("close")}</Button>,
        <Button key="release" type="primary" onClick={() => void handleOpenReleasePage()}>
          {t("openReleasePage")}</Button>,
      ];
    }

    if (phase === "has-update") {
      return [
        <Button key="release" onClick={() => void handleOpenReleasePage()}>
          {t("openReleasePage")}</Button>,
        <Button key="download" type="primary" onClick={() => void handleDownload()}>
          {t("downloadAndInstall")}</Button>,
      ];
    }

    if (phase === "downloaded") {
      return [
        <Button key="folder" onClick={() => void handleOpenInstallerFolder()}>
          {t("openContainingFolder")}</Button>,
        <Button
          key="install"
          type="primary"
          loading={openingInstaller}
          onClick={() => void handleOpenInstaller()}
        >
          {t("openInstallerAndQuit")}</Button>,
      ];
    }

    return null;
  };

  const closable = phase !== "downloading";

  return (
    <Modal
      title={t("checkForUpdates")}
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
  useTranslation();
  return (
    <div>
      <Typography.Text strong>{t("whatSNew")}</Typography.Text>
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
  useTranslation();
  const platform = detectPlatform();
  const tip = (() => {
    switch (platform) {
      case "windows":
        return t("theInstallerWillLaunchAndThisAppWillCloseAutomatically");
      case "macos":
        return t("theDmgImageWillOpenAndThisAppWillQuit");
      case "linux":
        return t("thisAppWillQuitAutomaticallyRunAnAppimageDirectlyInstall");
      default:
        return t("thisAppWillQuitWhenTheInstallerOpensFollowThe");
    }
  })();

  return <Alert type="info" showIcon message={t("installationInstructions")} description={tip} />;
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
  if (!error) return t("unknownError");
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return String(error);
}
