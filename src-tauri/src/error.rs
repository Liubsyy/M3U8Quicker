use std::borrow::Cow;

use serde::Serialize;

const MAX_DISPLAY_URL_CHARS: usize = 160;
const DISPLAY_URL_PREFIX_CHARS: usize = 96;
const DISPLAY_URL_SUFFIX_CHARS: usize = 48;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Network error: {0}")]
    Network(String),

    #[error("M3U8 parse error: {0}")]
    M3u8Parse(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("URL parse error: {0}")]
    UrlParse(#[from] url::ParseError),

    #[error("{0}")]
    InvalidInput(String),

    #[error("Decryption error: {0}")]
    Decryption(String),

    #[error("Conversion error: {0}")]
    Conversion(String),

    #[error("Download cancelled")]
    Cancelled,

    #[error("{0}")]
    Internal(String),
}

impl From<reqwest::Error> for AppError {
    fn from(error: reqwest::Error) -> Self {
        Self::Network(format_reqwest_error(&error))
    }
}

fn format_reqwest_error(error: &reqwest::Error) -> String {
    let url = error
        .url()
        .map(|value| shorten_url_for_error(value.as_str()))
        .unwrap_or(Cow::Borrowed("unknown url"));

    if let Some(status) = error.status() {
        return format!("HTTP {} for {}", status.as_u16(), url);
    }

    if error.is_timeout() {
        return format!("request timed out for {}", url);
    }

    if error.is_connect() {
        return format!("failed to connect to {}", url);
    }

    if error.is_request() {
        return format!("request could not be built for {}", url);
    }

    if error.is_body() {
        return format!("failed while reading response body from {}", url);
    }

    if error.is_decode() {
        return format!("failed to decode response from {}", url);
    }

    if let Some(raw_url) = error.url().map(|value| value.as_str()) {
        return error.to_string().replace(raw_url, url.as_ref());
    }

    error.to_string()
}

fn shorten_url_for_error(url: &str) -> Cow<'_, str> {
    let char_count = url.chars().count();
    if char_count <= MAX_DISPLAY_URL_CHARS {
        return Cow::Borrowed(url);
    }

    let prefix: String = url.chars().take(DISPLAY_URL_PREFIX_CHARS).collect();
    let suffix_start = char_count.saturating_sub(DISPLAY_URL_SUFFIX_CHARS);
    let suffix: String = url.chars().skip(suffix_start).collect();

    Cow::Owned(format!("{prefix}...{suffix}"))
}

impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        serializer.serialize_str(self.to_string().as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shorten_url_for_error_keeps_short_urls_intact() {
        let url = "https://example.com/video/playlist.m3u8";

        assert_eq!(shorten_url_for_error(url).as_ref(), url);
    }

    #[test]
    fn shorten_url_for_error_truncates_long_urls_in_the_middle() {
        let url = format!(
            "https://example.com/video/playlist.m3u8?token={}&expires=123456",
            "a".repeat(240)
        );
        let shortened = shorten_url_for_error(&url);
        let expected_suffix: String = url
            .chars()
            .skip(url.chars().count() - DISPLAY_URL_SUFFIX_CHARS)
            .collect();

        assert!(shortened.len() < url.len());
        assert!(shortened.contains("..."));
        assert!(shortened.starts_with("https://example.com/video/playlist.m3u8"));
        assert!(shortened.ends_with(&expected_suffix));
    }
}
