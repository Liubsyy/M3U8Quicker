import { useEffect, useState } from "react";
import { ConfigProvider, theme } from "antd";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import App from "./App";
import { PlaybackWindow } from "./components/PlaybackWindow";
import { PreviewWindow } from "./components/PreviewWindow";
import { useDisableDefaultContextMenu } from "./hooks/useDisableDefaultContextMenu";
import { darkTheme, lightTheme } from "./styles/theme";
import {
  DEFAULT_ZOOM,
  normalizeZoom,
  THEME_MODE_STORAGE_KEY,
  ZOOM_STORAGE_KEY,
  type ThemeMode,
} from "./types/settings";

function getInitialThemeMode(): ThemeMode {
  const saved = localStorage.getItem(THEME_MODE_STORAGE_KEY);
  return saved === "dark" ? "dark" : "light";
}

function getInitialZoom(): number {
  const saved = localStorage.getItem(ZOOM_STORAGE_KEY);
  if (saved === null) return DEFAULT_ZOOM;
  return normalizeZoom(Number.parseFloat(saved));
}

export function RootApp() {
  const [themeMode, setThemeMode] = useState<ThemeMode>(getInitialThemeMode);
  const [zoomFactor, setZoomFactor] = useState<number>(getInitialZoom);

  useDisableDefaultContextMenu();

  useEffect(() => {
    localStorage.setItem(THEME_MODE_STORAGE_KEY, themeMode);
    document.documentElement.dataset.themeMode = themeMode;
  }, [themeMode]);

  const themeConfig =
    themeMode === "light"
      ? { ...lightTheme, algorithm: theme.defaultAlgorithm }
      : { ...darkTheme, algorithm: theme.darkAlgorithm };

  const view = new URLSearchParams(window.location.search).get("view");
  const isMainWindow = view !== "player" && view !== "preview";

  useEffect(() => {
    localStorage.setItem(ZOOM_STORAGE_KEY, String(zoomFactor));
    if (isMainWindow) void getCurrentWebviewWindow().setZoom(zoomFactor);
  }, [zoomFactor, isMainWindow]);

  return (
    <ConfigProvider theme={themeConfig}>
      {view === "player" ? (
        <PlaybackWindow />
      ) : view === "preview" ? (
        <PreviewWindow />
      ) : (
        <App
          themeMode={themeMode}
          onThemeModeChange={setThemeMode}
          zoomFactor={zoomFactor}
          onZoomChange={setZoomFactor}
        />
      )}
    </ConfigProvider>
  );
}
