import { Button, Dropdown, Space, Typography, theme } from "antd";
import {
  ApartmentOutlined,
  ChromeOutlined,
  ControlOutlined,
  DeploymentUnitOutlined,
  FileSearchOutlined,
  GlobalOutlined,
  MergeCellsOutlined,
  DownOutlined,
  PlusOutlined,
  SettingOutlined,
  SwapOutlined,
  ToolOutlined,
  ThunderboltOutlined,
} from "@ant-design/icons";
import type { MenuProps } from "antd";
import type { ToolAction } from "./ToolsModal";
import { EdgeIcon } from "./EdgeIcon";
import { FirefoxIcon } from "./FirefoxIcon";

interface ToolbarProps {
  onNewDownload: () => void;
  onOpenTool: (tool: ToolAction) => void;
  onOpenSettings: () => void;
}

export function Toolbar({
  onNewDownload,
  onOpenTool,
  onOpenSettings,
}: ToolbarProps) {
  const { token } = theme.useToken();
  const toolItems: MenuProps["items"] = [
    {
      key: "merge-ts",
      label: "合并 ts",
      icon: <MergeCellsOutlined />,
    },
    {
      key: "ts-to-mp4",
      label: "ts 转 mp4",
      icon: <SwapOutlined />,
    },
    {
      key: "ffmpeg-tools",
      label: "FFmpeg",
      icon: <DeploymentUnitOutlined />,
      children: [
        {
          key: "analyze-media",
          label: "分析视频",
          icon: <FileSearchOutlined />,
        },
        {
          key: "format-convert",
          label: "格式转换",
          icon: <SwapOutlined />,
        },
        {
          key: "codec-convert",
          label: "编码转换",
          icon: <ControlOutlined />,
        },
        {
          key: "merge-video",
          label: "合并视频",
          icon: <MergeCellsOutlined />,
        },
        {
          key: "multi-track-hls-to-mp4",
          label: "多轨 HLS 转 mp4",
          icon: <ApartmentOutlined />,
        },
      ],
    },
    {
      key: "install-browser-extension",
      label: "安装浏览器扩展",
      icon: <GlobalOutlined />,
      children: [
        {
          key: "install-chrome-extension",
          label: "Chrome 扩展",
          icon: <ChromeOutlined />,
        },
        {
          key: "install-edge-extension",
          label: "Microsoft Edge 扩展",
          icon: <EdgeIcon />,
        },
        {
          key: "install-firefox-extension",
          label: "Firefox 扩展",
          icon: <FirefoxIcon />,
        },
      ],
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
        <ThunderboltOutlined style={{ fontSize: 24, color: "#1668dc" }} />
        <Typography.Title level={4} style={{ margin: 0, color: token.colorText }}>
          M3U8 Quicker
        </Typography.Title>
      </Space>
      <Space>
        <Button type="primary" icon={<PlusOutlined />} onClick={onNewDownload}>
          新建下载
        </Button>
        <Dropdown
          menu={{
            items: toolItems,
            onClick: ({ key }) => onOpenTool(key as ToolAction),
          }}
          trigger={["click"]}
        >
          <Button icon={<ToolOutlined />}>
            工具
            <DownOutlined style={{ fontSize: 12 }} />
          </Button>
        </Dropdown>
        <Button icon={<SettingOutlined />} onClick={onOpenSettings}>
          设置
        </Button>
      </Space>
    </div>
  );
}
