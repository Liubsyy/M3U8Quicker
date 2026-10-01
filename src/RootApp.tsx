import { useEffect, useMemo, useState } from "react";
import { ConfigProvider, theme } from "antd";
import enUS from "antd/locale/en_US";
import zhCN from "antd/locale/zh_CN";
import zhTW from "antd/locale/zh_TW";
import jaJP from "antd/locale/ja_JP";
import koKR from "antd/locale/ko_KR";
import esES from "antd/locale/es_ES";
import frFR from "antd/locale/fr_FR";
import deDE from "antd/locale/de_DE";
import ptBR from "antd/locale/pt_BR";
import ruRU from "antd/locale/ru_RU";
import { currentLanguage, initializeLanguage, useTranslation } from "./i18n";
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
  useTranslation();
  const [languageReady, setLanguageReady] = useState(false);
  const locale = { "zh-CN": zhCN, "zh-TW": zhTW, en: enUS, ja: jaJP, ko: koKR, es: esES, fr: frFR, de: deDE, "pt-BR": ptBR, ru: ruRU }[currentLanguage()];
  const [themeMode, setThemeMode] = useState<ThemeMode>(getInitialThemeMode);
  const [zoomFactor, setZoomFactor] = useState<number>(getInitialZoom);

  useDisableDefaultContextMenu();

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void initializeLanguage().then((cleanup) => {
      if (disposed) cleanup();
      else { unlisten = cleanup; setLanguageReady(true); }
    }).catch((error) => {
      console.error("Failed to initialize language", error);
      if (!disposed) setLanguageReady(true);
    });
    return () => { disposed = true; unlisten?.(); };
  }, []);

  useEffect(() => {
    localStorage.setItem(THEME_MODE_STORAGE_KEY, themeMode);
    document.documentElement.dataset.themeMode = themeMode;
  }, [themeMode]);

  const themeConfig = useMemo(() =>
    themeMode === "light"
      ? { ...lightTheme, algorithm: theme.defaultAlgorithm }
      : { ...darkTheme, algorithm: theme.darkAlgorithm }, [themeMode]);

  useEffect(() => {
    ConfigProvider.config({
      holderRender: (children) => <ConfigProvider locale={locale} theme={themeConfig}>{children}</ConfigProvider>,
    });
  }, [locale, themeConfig]);

  const view = new URLSearchParams(window.location.search).get("view");
  const isMainWindow = view !== "player" && view !== "preview";

  useEffect(() => {
    localStorage.setItem(ZOOM_STORAGE_KEY, String(zoomFactor));
    if (isMainWindow) void getCurrentWebviewWindow().setZoom(zoomFactor);
  }, [zoomFactor, isMainWindow]);

  if (!languageReady) return null;

  return (
    <ConfigProvider theme={themeConfig} locale={locale}>
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
