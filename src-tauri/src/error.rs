use std::borrow::Cow;
use std::error::Error;

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

    if error.url().is_some_and(|url| url.scheme() == "https")
        && has_certificate_validation_error(error)
    {
        return crate::localized!("httpsCertificateValidationFailed", url);
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

fn has_certificate_validation_error(error: &reqwest::Error) -> bool {
    // Only inspect the actual TLS error, never request URLs or HTTP response text.
    let mut source = error.source();
    while let Some(cause) = source {
        if let Some(tls_error) = cause.downcast_ref::<native_tls::Error>() {
            return is_certificate_validation_failure(
                &tls_error.to_string(),
                &format!("{tls_error:?}"),
            );
        }
        source = cause.source();
    }
    false
}

fn is_certificate_validation_failure(message: &str, debug: &str) -> bool {
    // Secure Transport and Schannel expose OS status codes in Debug even when
    // their human-readable error messages are localized by the operating system.
    let code = debug
        .split_once("code: ")
        .and_then(|(_, value)| value.split([',', '}']).next()?.trim().parse::<i64>().ok());
    if matches!(
        code,
        Some(
            // macOS: chain, hostname, expiry, revocation, trust and lifetime errors.
            -25318 | -67602 | -67818 | -67819 | -67820 | -67843 | -67901
            | -9807 | -9808 | -9812 | -9813 | -9814 | -9815 | -9843
            // Windows: CERT_E_EXPIRED / UNTRUSTEDROOT / CHAINING / REVOKED /
            // CN_NO_MATCH and SEC_E_CERT_EXPIRED / UNTRUSTED_ROOT / WRONG_PRINCIPAL.
            | -2146762495 | -2146762487 | -2146762486 | -2146762484 | -2146762481
            | -2146893016 | -2146893019 | -2146893022
        )
    ) {
        return true;
    }

    // OpenSSL reports certificate validation separately from TLS protocol errors.
    let message = message.to_ascii_lowercase();
    [
        "certificate verify failed",
        "certificate_verify_failed",
        "certificate verification failed",
        "certificate has expired",
        "certificate is not yet valid",
        "certificate was not trusted",
        "self-signed certificate",
        "self signed certificate",
        "unable to get local issuer certificate",
        "hostname mismatch",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
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
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn certificate_errors_are_recognized_across_native_tls_backends() {
        for message in [
            "error:0A000086:SSL routines:tls_post_process_server_certificate:certificate verify failed (certificate has expired)",
            "self-signed certificate in certificate chain",
            "unable to get local issuer certificate",
            "hostname mismatch",
        ] {
            assert!(is_certificate_validation_failure(message, ""), "{message}");
        }
        // OS status codes work independently of the language of system messages.
        for code in [
            -67818,
            -67843,
            -67602,
            -9814,
            -2146762495,
            -2146762487,
            -2146762481,
            -2146893016,
        ] {
            assert!(is_certificate_validation_failure(
                "localized OS message",
                &format!("Error {{ code: {code}, message: localized }}")
            ));
        }
        for message in [
            "connection refused",
            "unexpected EOF",
            "wrong version number",
            "TLS protocol version unsupported",
            "tlsv13 alert certificate required",
        ] {
            assert!(
                !is_certificate_validation_failure(message, "Error { code: -9806 }"),
                "{message}"
            );
        }
    }

    #[tokio::test]
    async fn connection_failure_keeps_existing_message_even_with_certificate_words_in_url() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "https://{}/certificate_verify_failed",
            listener.local_addr().unwrap()
        );
        drop(listener);
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let error = client.get(&url).send().await.unwrap_err();
        assert_eq!(
            format_reqwest_error(&error),
            format!("failed to connect to {url}")
        );
    }

    #[tokio::test]
    async fn http_status_failure_keeps_existing_message() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/certificate_verify_failed",
            listener.local_addr().unwrap()
        );
        let worker = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 1024];
            stream.read(&mut buffer).await.unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        });
        let error = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(&url)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap_err();
        assert_eq!(format_reqwest_error(&error), format!("HTTP 403 for {url}"));
        worker.await.unwrap();
    }

    #[tokio::test]
    async fn https_timeout_keeps_existing_message() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("https://{}/video.m3u8", listener.local_addr().unwrap());
        let worker = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let error = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(200))
            .build()
            .unwrap()
            .get(&url)
            .send()
            .await
            .unwrap_err();
        worker.abort();
        assert_eq!(
            format_reqwest_error(&error),
            format!("request timed out for {url}")
        );
    }

    #[tokio::test]
    async fn tls_protocol_failure_keeps_existing_connection_message() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("https://{}/video.m3u8", listener.local_addr().unwrap());
        let worker = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            // Speak plain HTTP to a TLS client: this is a protocol error, not a bad certificate.
            let mut buffer = [0; 4096];
            stream.read(&mut buffer).await.unwrap();
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await;
        });
        let error = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap()
            .get(&url)
            .send()
            .await
            .unwrap_err();
        assert_eq!(
            format_reqwest_error(&error),
            format!("failed to connect to {url}")
        );
        worker.await.unwrap();
    }

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
