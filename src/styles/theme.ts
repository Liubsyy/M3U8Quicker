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
    colorPrimary: "#1668dc",
    colorBgContainer: "#1f1f1f",
    colorBgElevated: "#2a2a2a",
    colorBgLayout: "#141414",
    colorText: "#e8e8e8",
    colorTextSecondary: "#a0a0a0",
    colorBorder: "#333333",
    borderRadius: 6,
    colorSuccess: "#52c41a",
    colorError: "#ff4d4f",
    colorWarning: "#faad14",
    fontFamily:
      "-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'PingFang SC', 'Microsoft YaHei', sans-serif",
  },
  components: {
    Layout: {
      bodyBg: "#141414",
      headerBg: "#1a1a2e",
      siderBg: "#1a1a2e",
    },
    Table: {
      headerBg: "#1f1f1f",
      rowHoverBg: "#2a2a2e",
    },
    Modal: {
      contentBg: "#1f1f1f",
      headerBg: "#1f1f1f",
    },
    Tabs: {
      inkBarColor: "#1668dc",
    },
  },
};
