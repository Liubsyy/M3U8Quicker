import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import { isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AppLanguage } from "../types/settings";
import { getAppLanguage, setAppLanguage } from "../services/api";
import en from "./locales/en.json";
import zh from "./locales/zh-CN.json";
import zhTW from "./locales/zh-TW.json";
import ja from "./locales/ja.json";
import ko from "./locales/ko.json";
import es from "./locales/es.json";
import fr from "./locales/fr.json";
import de from "./locales/de.json";
import ptBR from "./locales/pt-BR.json";
import ru from "./locales/ru.json";
import { isAppLanguage, LANGUAGE_OPTIONS, resolveSystemLanguage } from "./languages";

export { useTranslation } from "react-i18next";
export { i18n };

await i18n.use(initReactI18next).init({
  resources: {
    en: { translation: en }, "zh-CN": { translation: zh }, "zh-TW": { translation: zhTW },
    ja: { translation: ja }, ko: { translation: ko }, es: { translation: es },
    fr: { translation: fr }, de: { translation: de }, "pt-BR": { translation: ptBR }, ru: { translation: ru },
  },
  lng: "en",
  fallbackLng: "en",
  supportedLngs: LANGUAGE_OPTIONS.map((option) => option.value),
  interpolation: { escapeValue: false },
});

// Read the current language at call time, including in long-lived event handlers.
export function t(key: keyof typeof en, values?: Record<string, unknown>): string {
  return i18n.t(key, values ?? {}) as string;
}

export function containsLocalizedMessage(text: string, key: keyof typeof en): boolean {
  return LANGUAGE_OPTIONS.some(({ value }) => text.includes(i18n.getFixedT(value)(key)));
}

export interface TranslatedMessage {
  key: keyof typeof en;
  values?: Record<string, unknown>;
}

export function translatedMessage(key: keyof typeof en, values?: Record<string, unknown>): TranslatedMessage {
  return { key, values };
}

export function renderMessage(message: TranslatedMessage | null): string | null {
  return message ? t(message.key, message.values) : null;
}

export function currentLanguage(): AppLanguage {
  return isAppLanguage(i18n.language) ? i18n.language : "en";
}

function applyLanguage(language: AppLanguage) {
  document.documentElement.lang = language;
  return i18n.changeLanguage(language);
}

export async function initializeLanguage(): Promise<() => void> {
  if (!isTauri()) {
    // Browser-only previews have no native settings or system-locale API.
    await applyLanguage(resolveSystemLanguage(navigator.language));
    return () => {};
  }
  let revision = 0;
  const unlisten = await listen<AppLanguage>("app-language-changed", ({ payload }) => {
    revision += 1;
    void applyLanguage(payload);
  });
  try {
    const language = await getAppLanguage();
    if (revision === 0) await applyLanguage(language);
    return unlisten;
  } catch (error) {
    unlisten();
    throw error;
  }
}

export async function changeAppLanguage(language: AppLanguage): Promise<void> {
  if (isTauri()) {
    const saved = await setAppLanguage(language);
    await applyLanguage(saved);
  } else {
    await applyLanguage(language);
  }
}
