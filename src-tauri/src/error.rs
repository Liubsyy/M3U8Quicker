use serde::Serialize;

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

    #[error("Invalid input: {0}")]
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
        .map(|value| value.as_str())
        .unwrap_or("unknown url");

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

    error.to_string()
}

impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        serializer.serialize_str(self.to_string().as_ref())
    }
}
