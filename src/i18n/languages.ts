import type { AppLanguage } from "../types/settings";

export const LANGUAGE_OPTIONS: { value: AppLanguage; label: string }[] = [
  { value: "zh-CN", label: "简体中文" },
  { value: "zh-TW", label: "繁體中文" },
  { value: "en", label: "English" },
  { value: "ja", label: "日本語" },
  { value: "ko", label: "한국어" },
  { value: "es", label: "Español" },
  { value: "fr", label: "Français" },
  { value: "de", label: "Deutsch" },
  { value: "pt-BR", label: "Português (Brasil)" },
  { value: "ru", label: "Русский" },
];

export function isAppLanguage(value: string): value is AppLanguage {
  return LANGUAGE_OPTIONS.some((option) => option.value === value);
}

export function resolveSystemLanguage(locale?: string | null): AppLanguage {
  const parts = (locale ?? "").trim().toLowerCase().replaceAll("_", "-").split(/[.@]/)[0].split("-");
  if (parts[0] === "zh") {
    if (parts.includes("hant")) return "zh-TW";
    if (parts.includes("hans")) return "zh-CN";
    return parts.some((part) => ["tw", "hk", "mo"].includes(part)) ? "zh-TW" : "zh-CN";
  }
  if (parts[0] === "pt") return "pt-BR";
  return isAppLanguage(parts[0]) ? parts[0] : "en";
}
