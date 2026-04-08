export type ThemeMode = "light" | "dark";

export const THEME_MODE_STORAGE_KEY = "m3u8quicker.themeMode";

export interface ProxySettings {
  enabled: boolean;
  url: string;
}

export interface AppSettings {
  default_download_dir: string | null;
  proxy: ProxySettings;
  download_concurrency: number;
  download_speed_limit_kbps: number;
  delete_ts_temp_dir_after_download: boolean;
  convert_to_mp4: boolean;
}
