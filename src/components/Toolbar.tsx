import { t, useTranslation } from "../i18n";
import { Badge, Button, Dropdown, Space, Tooltip, Typography, theme } from "antd";
import {
  ApartmentOutlined,
  ChromeOutlined,
  RetweetOutlined,
  DeploymentUnitOutlined,
  PlusSquareOutlined,
  FileSyncOutlined,
  FileSearchOutlined,
  GlobalOutlined,
  MergeCellsOutlined,
  DownOutlined,
  PictureOutlined,
  PlusOutlined,
  ScissorOutlined,
  SettingOutlined,
  SwapOutlined,
  ToolOutlined,
  ThunderboltOutlined,
  VideoCameraOutlined,
} from "@ant-design/icons";
import type { MenuProps } from "antd";
import type { ToolAction } from "./ToolsModal";
import { EdgeIcon } from "./EdgeIcon";
import { FirefoxIcon } from "./FirefoxIcon";

interface ToolbarProps {
  onNewDownload: () => void;
  onOpenBatchDownload: () => void;
  onOpenVideoPreview: () => void;
  onOpenLiveRecord: () => void;
  onOpenTool: (tool: ToolAction) => void;
  onOpenSettings: () => void;
  proxyEnabled: boolean;
  onOpenProxySettings: () => void;
  onProxyEnabledChange: (enabled: boolean) => void;
  updateAvailable?: boolean;
}

export function Toolbar({
  onNewDownload,
  onOpenBatchDownload,
  onOpenVideoPreview,
  onOpenLiveRecord,
  onOpenTool,
  onOpenSettings,
  proxyEnabled,
  onOpenProxySettings,
  onProxyEnabledChange,
  updateAvailable = false,
}: ToolbarProps) {
  useTranslation();
  const { token } = theme.useToken();
  const newDownloadItems: MenuProps["items"] = [
    {
      key: "batch-download",
      label: t("batchDownload"),
      icon: <PlusSquareOutlined />,
    },
    {
      key: "live-record",
      label: t("liveRecording"),
      icon: <VideoCameraOutlined />,
    },
    {
      key: "video-preview",
      label: t("videoThumbnails"),
      icon: <PictureOutlined />,
    },
  ];
  const toolItems: MenuProps["items"] = [
    {
      key: "merge-ts",
      label: t("mergeTs"),
      icon: <MergeCellsOutlined />,
    },
    {
      key: "ts-to-mp4",
      label: t("tsToMp4"),
      icon: <SwapOutlined />,
    },
    {
      key: "local-m3u8-to-mp4",
      label: t("localM3u8ToMp4"),
      icon: <FileSyncOutlined />,
    },
    {
      key: "ffmpeg-tools",
      label: "FFmpeg",
      icon: <DeploymentUnitOutlined />,
      children: [
        {
          key: "analyze-media",
          label: t("analyzeVideo"),
          icon: <FileSearchOutlined />,
        },
        {
          key: "format-convert",
          label: t("convertFormat"),
          icon: <SwapOutlined />,
        },
        {
          key: "codec-convert",
          label: t("transcode"),
          icon: <RetweetOutlined />,
        },
        {
          key: "merge-video",
          label: t("mergeVideos"),
          icon: <MergeCellsOutlined />,
        },
        {
          key: "clip-video",
          label: t("clipVideo"),
          icon: <ScissorOutlined />,
        },
        {
          key: "multi-track-hls-to-mp4",
          label: t("multiTrackHlsToMp4"),
          icon: <ApartmentOutlined />,
        },
      ],
    },
    {
      key: "install-browser-extension",
      label: t("installBrowserExtension"),
      icon: <GlobalOutlined />,
      children: [
        {
          key: "install-chrome-extension",
          label: t("chromeExtension"),
          icon: <ChromeOutlined />,
        },
        {
          key: "install-edge-extension",
          label: t("microsoftEdgeExtension"),
          icon: <EdgeIcon />,
        },
        {
          key: "install-firefox-extension",
          label: t("firefoxExtension"),
          icon: <FirefoxIcon />,
        },
      ],
    },
  ];
  const proxyItems: MenuProps["items"] = [
    {
      key: "enable-proxy",
      label: t("enableProxy2"),
      disabled: proxyEnabled,
    },
    {
      key: "disable-proxy",
      label: t("disableProxy"),
      disabled: !proxyEnabled,
    },
  ];

  return (
    <div
      style={{
        display: "flex",
        justifyContent: "space-between",
        alignItems: "center",
        width: "100%",
      }}
    >
      <Space>
        <Tooltip title={proxyEnabled ? t("proxyEnabled2") : t("proxyDisabled2")}>
          <Dropdown
            menu={{
              items: proxyItems,
              onClick: ({ key }) => onProxyEnabledChange(key === "enable-proxy"),
            }}
            trigger={["contextMenu"]}
          >
            <span
              role="button"
              tabIndex={0}
              aria-label={proxyEnabled ? t("proxyEnabledOpenNetworkSettings") : t("proxyDisabledOpenNetworkSettings")}
              onClick={onOpenProxySettings}
              onKeyDown={(event) => {
                if (event.key === "Enter" || event.key === " ") {
                  event.preventDefault();
                  onOpenProxySettings();
                }
              }}
              style={{ display: "inline-flex", cursor: "pointer" }}
            >
              <ThunderboltOutlined
                style={{
                  fontSize: 24,
                  color: proxyEnabled ? token.colorError : token.colorPrimary,
                }}
              />
            </span>
          </Dropdown>
        </Tooltip>
        <Typography.Title level={4} style={{ margin: 0, color: token.colorText }}>
          M3U8 Quicker
        </Typography.Title>
      </Space>
      <Space>
        <Dropdown
          menu={{
            items: newDownloadItems,
            onClick: ({ key }) => {
              if (key === "batch-download") {
                onOpenBatchDownload();
              } else if (key === "video-preview") {
                onOpenVideoPreview();
              } else if (key === "live-record") {
                onOpenLiveRecord();
              }
            },
          }}
          trigger={["click"]}
          placement="bottomLeft"
        >
          <Space.Compact className="toolbar-download-actions">
            <Button
              type="primary"
              icon={<PlusOutlined />}
              onClick={(e) => {
                e.stopPropagation();
                onNewDownload();
              }}
              className="toolbar-download-main-btn"
            >
              {t("newDownload")}</Button>
            <Button
              type="primary"
              aria-label={t("moreDownloadTypes")}
              className="toolbar-download-caret-btn"
            >
              <DownOutlined style={{ fontSize: 12 }} />
            </Button>
          </Space.Compact>
        </Dropdown>
        <Dropdown
          menu={{
            items: toolItems,
            onClick: ({ key }) => onOpenTool(key as ToolAction),
          }}
          trigger={["click"]}
        >
          <Button icon={<ToolOutlined />}>
            {t("tools")}<DownOutlined style={{ fontSize: 12 }} />
          </Button>
        </Dropdown>
        <Button
          icon={
            <Badge dot={updateAvailable} offset={[2, 0]}>
              <SettingOutlined />
            </Badge>
          }
          onClick={onOpenSettings}
        >
          {t("settings")}</Button>
      </Space>
    </div>
  );
}
