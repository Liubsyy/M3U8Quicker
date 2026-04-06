import { useEffect, useState } from "react";
import { ConfigProvider, theme } from "antd";
import App from "./App";
import { PlaybackWindow } from "./components/PlaybackWindow";
import { useDisableDefaultContextMenu } from "./hooks/useDisableDefaultContextMenu";
import { darkTheme, lightTheme } from "./styles/theme";
import {
  THEME_MODE_STORAGE_KEY,
  type ThemeMode,
} from "./types/settings";

function getInitialThemeMode(): ThemeMode {
  const saved = localStorage.getItem(THEME_MODE_STORAGE_KEY);
  return saved === "dark" ? "dark" : "light";
}

export function RootApp() {
  const [themeMode, setThemeMode] = useState<ThemeMode>(getInitialThemeMode);

  useDisableDefaultContextMenu();

  useEffect(() => {
    localStorage.setItem(THEME_MODE_STORAGE_KEY, themeMode);
    document.documentElement.dataset.themeMode = themeMode;
  }, [themeMode]);

  const themeConfig =
    themeMode === "light"
      ? { ...lightTheme, algorithm: theme.defaultAlgorithm }
      : { ...darkTheme, algorithm: theme.darkAlgorithm };

  return (
    <ConfigProvider theme={themeConfig}>
      {new URLSearchParams(window.location.search).get("view") === "player" ? (
        <PlaybackWindow />
      ) : (
        <App themeMode={themeMode} onThemeModeChange={setThemeMode} />
      )}
    </ConfigProvider>
  );
}
