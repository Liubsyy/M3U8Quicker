import type { ThemeConfig } from "antd";

export const lightTheme: ThemeConfig = {
  token: {
    colorPrimary: "#1668dc",
    colorBgContainer: "#ffffff",
    colorBgElevated: "#ffffff",
    colorBgLayout: "#f5f7fb",
    colorText: "#1f1f1f",
    colorTextSecondary: "#595959",
    colorBorder: "#d9d9d9",
    borderRadius: 6,
    colorSuccess: "#52c41a",
    colorError: "#ff4d4f",
    colorWarning: "#faad14",
    fontFamily:
      "-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'PingFang SC', 'Microsoft YaHei', sans-serif",
  },
  components: {
    Layout: {
      bodyBg: "#f5f7fb",
      headerBg: "#ffffff",
      siderBg: "#ffffff",
    },
    Table: {
      headerBg: "#f7f9fc",
      rowHoverBg: "#f2f6ff",
    },
    Modal: {
      contentBg: "#ffffff",
      headerBg: "#ffffff",
    },
    Tabs: {
      inkBarColor: "#1668dc",
    },
  },
};

export const darkTheme: ThemeConfig = {
  token: {
    colorPrimary: "#4096ff",
    colorInfo: "#4096ff",
    colorBgContainer: "#161b22",
    colorBgElevated: "#1f242c",
    colorBgLayout: "#0d1117",
    colorText: "#e6edf3",
    colorTextSecondary: "#9da7b3",
    colorTextTertiary: "#7d8590",
    colorBorder: "#30363d",
    colorBorderSecondary: "#21262d",
    borderRadius: 6,
    colorSuccess: "#3fb950",
    colorError: "#f85149",
    colorWarning: "#d29922",
    fontFamily:
      "-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'PingFang SC', 'Microsoft YaHei', sans-serif",
  },
  components: {
    Layout: {
      bodyBg: "#0d1117",
      headerBg: "#161b22",
      siderBg: "#161b22",
    },
    Table: {
      headerBg: "#161b22",
      rowHoverBg: "#1f242c",
    },
    Modal: {
      contentBg: "#1f242c",
      headerBg: "#1f242c",
    },
    Tabs: {
      inkBarColor: "#4096ff",
    },
    Button: {
      defaultBg: "#21262d",
      defaultBorderColor: "#30363d",
      defaultHoverBg: "#2a313a",
      defaultHoverBorderColor: "#3d444d",
    },
    Card: {
      colorBgContainer: "#161b22",
    },
    Tooltip: {
      colorBgSpotlight: "#2a313a",
    },
  },
};
