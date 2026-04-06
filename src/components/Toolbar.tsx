import { Button, Dropdown, Space, Typography, theme } from "antd";
import {
  ChromeOutlined,
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
      key: "install-chrome-extension",
      label: "安装 Chrome 扩展",
      icon: <ChromeOutlined />,
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
