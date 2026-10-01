use std::sync::atomic::{AtomicU8, Ordering};
use serde::{Deserialize, Deserializer, Serialize};
use tauri::{AppHandle, Emitter};
use crate::{error::AppError, persistence};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum AppLanguage {
    #[serde(rename = "en")]
    English,
    #[serde(rename = "zh-CN")]
    Chinese,
    #[serde(rename = "zh-TW")]
    TraditionalChinese,
    #[serde(rename = "ja")]
    Japanese,
    #[serde(rename = "ko")]
    Korean,
    #[serde(rename = "es")]
    Spanish,
    #[serde(rename = "fr")]
    French,
    #[serde(rename = "de")]
    German,
    #[serde(rename = "pt-BR")]
    Portuguese,
    #[serde(rename = "ru")]
    Russian,
}

impl AppLanguage {
    pub fn code(self) -> &'static str {
        match self {
            Self::English => "en", Self::Chinese => "zh-CN", Self::TraditionalChinese => "zh-TW",
            Self::Japanese => "ja", Self::Korean => "ko", Self::Spanish => "es",
            Self::French => "fr", Self::German => "de", Self::Portuguese => "pt-BR", Self::Russian => "ru",
        }
    }
}

static LANGUAGE: AtomicU8 = AtomicU8::new(0);
static CHANGE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub fn current_language() -> AppLanguage {
    match LANGUAGE.load(Ordering::Relaxed) {
        1 => AppLanguage::Chinese, 2 => AppLanguage::TraditionalChinese,
        3 => AppLanguage::Japanese, 4 => AppLanguage::Korean, 5 => AppLanguage::Spanish,
        6 => AppLanguage::French, 7 => AppLanguage::German, 8 => AppLanguage::Portuguese,
        9 => AppLanguage::Russian, _ => AppLanguage::English,
    }
}

fn set_current(language: AppLanguage) {
    LANGUAGE.store(language as u8, Ordering::Relaxed);
}

pub fn resolve_system_language(locale: Option<&str>) -> AppLanguage {
    let locale = locale.unwrap_or_default().trim().replace('_', "-").to_ascii_lowercase();
    let locale = locale.split(['.', '@']).next().unwrap_or_default();
    let parts: Vec<&str> = locale.split('-').collect();
    match parts[0] {
        "zh" => {
            if parts.contains(&"hant") { AppLanguage::TraditionalChinese }
            else if parts.contains(&"hans") { AppLanguage::Chinese }
            else if parts.iter().any(|part| matches!(*part, "tw" | "hk" | "mo")) { AppLanguage::TraditionalChinese }
            else { AppLanguage::Chinese }
        }
        "ja" => AppLanguage::Japanese, "ko" => AppLanguage::Korean,
        "es" => AppLanguage::Spanish, "fr" => AppLanguage::French,
        "de" => AppLanguage::German, "pt" => AppLanguage::Portuguese,
        "ru" => AppLanguage::Russian, _ => AppLanguage::English,
    }
}

// Missing/null values mean first launch; malformed stored values fall back to English.
pub fn deserialize_saved_language<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<AppLanguage>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::Null => None,
        value => Some(serde_json::from_value(value).unwrap_or(AppLanguage::English)),
    })
}

pub async fn initialize(app: &AppHandle) {
    let saved = persistence::load_settings(app).await.language;
    let language = saved.unwrap_or_else(|| resolve_system_language(sys_locale::get_locale().as_deref()));
    set_current(language);
    if saved.is_none() {
        if let Err(error) = persistence::try_update_settings(app, |settings| settings.language = Some(language)).await {
            eprintln!("Failed to save initial language: {error}");
        }
    }
}

fn resources() -> &'static serde_json::Value {
    static RESOURCES: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    RESOURCES.get_or_init(|| serde_json::from_str(include_str!("locales.json")).expect("valid embedded translations"))
}

pub fn tr(key: &'static str) -> &'static str {
    let entry = &resources()[key];
    let language = current_language().code();
    entry[language].as_str().or_else(|| entry["en"].as_str()).unwrap_or(key)
}

pub fn format_message(key: &'static str, args: &[&dyn std::fmt::Display]) -> String {
    let mut parts = tr(key).split("{}");
    let mut result = parts.next().unwrap_or_default().to_string();
    for (part, value) in parts.zip(args) {
        use std::fmt::Write;
        let _ = write!(result, "{value}{part}");
    }
    result
}

#[macro_export]
macro_rules! localized {
    ($key:literal $(, $value:expr)* $(,)?) => {
        $crate::i18n::format_message($key, &[$(&$value as &dyn std::fmt::Display),*])
    };
}

#[tauri::command]
pub fn get_app_language() -> AppLanguage { current_language() }

#[tauri::command]
pub async fn set_app_language(app_handle: AppHandle, language: AppLanguage) -> Result<AppLanguage, AppError> {
    let _guard = CHANGE_LOCK.lock().await;
    persistence::try_update_settings(&app_handle, |settings| settings.language = Some(language)).await?;
    set_current(language);
    crate::set_tray_language(&app_handle);
    let _ = app_handle.emit("app-language-changed", language);
    Ok(language)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::AppSettings;

    #[test]
    fn matches_system_language_and_falls_back() {
        for locale in ["zh", "zh-CN", "zh-Hans-TW", "zh_SG.UTF-8"] {
            assert_eq!(resolve_system_language(Some(locale)), AppLanguage::Chinese);
        }
        for locale in [None, Some(""), Some("en-US"), Some("id-ID"), Some("invalid")] {
            assert_eq!(resolve_system_language(locale), AppLanguage::English);
        }
        for (locale, expected) in [
            ("ZH_tw", AppLanguage::TraditionalChinese), ("zh-Hant-HK", AppLanguage::TraditionalChinese),
            ("zh-MO", AppLanguage::TraditionalChinese), ("ja-JP", AppLanguage::Japanese),
            ("ko_KR.UTF-8", AppLanguage::Korean), ("es-MX", AppLanguage::Spanish),
            ("fr-CA", AppLanguage::French), ("de-AT", AppLanguage::German),
            ("pt-PT", AppLanguage::Portuguese), ("pt_BR", AppLanguage::Portuguese),
            ("ru-RU", AppLanguage::Russian),
        ] { assert_eq!(resolve_system_language(Some(locale)), expected); }
    }

    #[test]
    fn old_and_invalid_settings_preserve_other_values() {
        let old: AppSettings = serde_json::from_str(r#"{"download_concurrency":7}"#).unwrap();
        assert_eq!(old.language, None);
        for invalid in [r#""unsupported""#, "42", "{}"] {
            let settings: AppSettings = serde_json::from_str(&format!(r#"{{"language":{invalid},"download_concurrency":7}}"#)).unwrap();
            assert_eq!(settings.language, Some(AppLanguage::English));
            assert_eq!(settings.download_concurrency, 7);
        }
        let saved: AppSettings = serde_json::from_str(r#"{"language":"zh-CN"}"#).unwrap();
        assert_eq!(saved.language.unwrap_or_else(|| resolve_system_language(Some("en-US"))), AppLanguage::Chinese);
    }

    #[test]
    fn all_languages_round_trip_without_changing_saved_preferences() {
        for code in ["en", "zh-CN", "zh-TW", "ja", "ko", "es", "fr", "de", "pt-BR", "ru"] {
            let language: AppLanguage = serde_json::from_value(serde_json::json!(code)).unwrap();
            assert_eq!(language.code(), code);
            assert_eq!(serde_json::to_value(language).unwrap(), serde_json::json!(code));
            let settings: AppSettings = serde_json::from_value(serde_json::json!({"language": code})).unwrap();
            assert_eq!(settings.language, Some(language));
        }
    }

    #[test]
    fn formatting_preserves_braces_in_user_values() {
        let filename = "video{}.mp4";
        assert_eq!(format_message("localFileNotFound", &[&filename]), "Local file not found: video{}.mp4");
    }
}
