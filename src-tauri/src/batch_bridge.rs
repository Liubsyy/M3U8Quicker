//! Large browser batches travel over loopback HTTP; the deep link carries only credentials.
use std::{collections::HashMap, sync::Arc, time::{Duration, Instant}};

use axum::{body::Bytes, extract::{DefaultBodyLimit, Path, Query, State}, http::{header, HeaderMap, StatusCode}, routing::{get, post}, Router};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{net::TcpListener, sync::Mutex, task::JoinHandle};

const PORTS: std::ops::RangeInclusive<u16> = 49320..=49329;
const MAX_BODY: usize = 16 * 1024 * 1024;
const SESSION_TTL: Duration = Duration::from_secs(300);
type Sessions = Arc<Mutex<HashMap<String, Session>>>;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BatchItem {
    pub url: String,
    pub title: String,
    pub file_type: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BatchPayload {
    pub items: Vec<BatchItem>,
    pub extra_headers: String,
}

struct Session {
    secret: String,
    expires: Instant,
    payload: Option<BatchPayload>,
    digest: Option<Vec<u8>>,
    delivered: bool,
}

#[derive(Default)]
pub struct BatchBridge {
    sessions: Sessions,
    server: Mutex<Option<JoinHandle<()>>>,
}

fn valid_hex(value: &str, len: usize) -> bool {
    value.len() == len && value.bytes().all(|b| b.is_ascii_hexdigit())
}

async fn bind_available(ports: impl IntoIterator<Item = u16>) -> std::io::Result<TcpListener> {
    let mut last_error = std::io::Error::new(std::io::ErrorKind::AddrInUse, "No batch transfer port available");
    for port in ports {
        match TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await {
            Ok(listener) => return Ok(listener),
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

impl BatchBridge {
    async fn prepare(&self, id: &str, secret: &str) -> Result<(), String> {
        if !valid_hex(id, 32) || !valid_hex(secret, 64) {
            return Err("Invalid batch transfer credentials".into());
        }
        let mut server = self.server.lock().await;
        if server.as_ref().is_none_or(|task| task.is_finished()) {
            let listener = bind_available(PORTS).await.map_err(|e| e.to_string())?;
            let sessions = self.sessions.clone();
            *server = Some(tokio::spawn(async move {
                let router = batch_router(sessions.clone());
                let cleanup = async move {
                    loop {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        sessions.lock().await.retain(|_, session| session.expires > Instant::now());
                    }
                };
                tokio::select! {
                    result = axum::serve(listener, router) => {
                        if let Err(error) = result { eprintln!("Batch transfer server: {error}"); }
                    }
                    _ = cleanup => {}
                }
            }));
        }
        let mut sessions = self.sessions.lock().await;
        sessions.retain(|_, session| session.expires > Instant::now());
        if let Some(session) = sessions.get(id) {
            if session.secret != secret { return Err("Batch transfer credentials mismatch".into()); }
            return Ok(());
        }
        if sessions.len() >= 16 { return Err("Too many pending batch transfers".into()); }
        sessions.insert(id.to_string(), Session {
            secret: secret.to_string(), expires: Instant::now() + SESSION_TTL,
            payload: None, digest: None, delivered: false,
        });
        Ok(())
    }

    async fn receive(&self, id: &str, secret: &str) -> Result<Option<BatchPayload>, String> {
        self.prepare(id, secret).await?;
        let deadline = Instant::now() + Duration::from_secs(100);
        loop {
            {
                let mut sessions = self.sessions.lock().await;
                let session = sessions.get_mut(id).ok_or("Batch transfer expired")?;
                if session.delivered { return Ok(None); }
                if let Some(payload) = session.payload.take() {
                    session.delivered = true;
                    return Ok(Some(payload));
                }
            }
            if Instant::now() >= deadline { return Err("Batch transfer timed out".into()); }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

#[tauri::command]
pub async fn receive_browser_batch(
    bridge: tauri::State<'_, BatchBridge>, id: String, secret: String,
) -> Result<Option<BatchPayload>, String> {
    bridge.receive(&id, &secret).await
}

fn batch_router(sessions: Sessions) -> Router {
    Router::new()
        .route("/batch/v1/{id}/probe", get(probe))
        .route("/batch/v1/{id}", post(upload))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(sessions)
}

#[derive(Deserialize)]
struct ProbeQuery { challenge: String }

// The secret is never sent to unverified candidate ports. Prove knowledge of it first.
async fn probe(
    State(sessions): State<Sessions>, Path(id): Path<String>, Query(query): Query<ProbeQuery>,
) -> Result<([(header::HeaderName, &'static str); 2], String), StatusCode> {
    if !valid_hex(&query.challenge, 32) { return Err(StatusCode::BAD_REQUEST); }
    let sessions = sessions.lock().await;
    let session = sessions.get(&id).filter(|s| s.expires > Instant::now()).ok_or(StatusCode::NOT_FOUND)?;
    let status = if session.delivered { "delivered" } else { "ready" };
    let mut mac = Hmac::<Sha256>::new_from_slice(session.secret.as_bytes()).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    mac.update(format!("m3u8quicker-batch-v1:{id}:{}:{status}", query.challenge).as_bytes());
    let proof = hex::encode(mac.finalize().into_bytes());
    Ok(([(header::CONTENT_TYPE, "application/json"), (header::CACHE_CONTROL, "no-store")],
        serde_json::json!({"proof": proof, "status": status}).to_string()))
}

async fn upload(
    State(sessions): State<Sessions>, Path(id): Path<String>, headers: HeaderMap, body: Bytes,
) -> Result<StatusCode, StatusCode> {
    let mut sessions = sessions.lock().await;
    let session = sessions.get_mut(&id).filter(|s| s.expires > Instant::now()).ok_or(StatusCode::NOT_FOUND)?;
    let authorization = headers.get(header::AUTHORIZATION).and_then(|value| value.to_str().ok());
    if authorization != Some(format!("Bearer {}", session.secret).as_str()) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let digest = Sha256::digest(&body).to_vec();
    if let Some(previous) = &session.digest {
        if previous != &digest { return Err(StatusCode::CONFLICT); }
        return Ok(if session.delivered { StatusCode::OK } else { StatusCode::ACCEPTED });
    }
    let payload: BatchPayload = serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    if payload.items.is_empty() || payload.items.len() > 10000 || payload.items.iter().any(|item| item.url.trim().is_empty()) {
        return Err(StatusCode::BAD_REQUEST);
    }
    session.payload = Some(payload);
    session.digest = Some(digest);
    Ok(StatusCode::ACCEPTED)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0123456789abcdef0123456789abcdef";
    const SECRET: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[tokio::test]
    async fn occupied_port_falls_back_and_all_occupied_returns_error() {
        let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = occupied.local_addr().unwrap().port();
        let available = bind_available([port, 0]).await.unwrap();
        assert_ne!(available.local_addr().unwrap().port(), port);
        assert!(bind_available([port]).await.is_err());
    }

    #[tokio::test]
    async fn large_batch_authenticated_delivery_and_retry() {
        let bridge = Arc::new(BatchBridge::default());
        bridge.prepare(ID, SECRET).await.unwrap();
        assert!(bridge.prepare(ID, &"f".repeat(64)).await.is_err());
        let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(3)).build().unwrap();
        let challenge = "fedcba9876543210fedcba9876543210";
        let mut base = None;
        for port in PORTS {
            let candidate = format!("http://127.0.0.1:{port}/batch/v1/{ID}");
            if let Ok(response) = client.get(format!("{candidate}/probe?challenge={challenge}")).send().await {
                if response.status().is_success() {
                    let value: serde_json::Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
                    let mut mac = Hmac::<Sha256>::new_from_slice(SECRET.as_bytes()).unwrap();
                    mac.update(format!("m3u8quicker-batch-v1:{ID}:{challenge}:ready").as_bytes());
                    assert_eq!(value["proof"], hex::encode(mac.finalize().into_bytes()));
                    base = Some(candidate);
                    break;
                }
            }
        }
        let base = base.expect("discover authenticated server");
        let payload = BatchPayload {
            items: (0..500).map(|i| BatchItem {
                url: format!("https://example.com/{i}.m3u8?token={}", "a".repeat(400)),
                title: format!("中文视频 {i}"), file_type: "hls".into(),
            }).collect(),
            extra_headers: "referer:https://example.com\nuser-agent:test".into(),
        };
        let body = serde_json::to_vec(&payload).unwrap();
        assert!(body.len() > 32767);
        assert_eq!(client.post(&base).body(body.clone()).send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
        assert_eq!(client.post(&base).bearer_auth(SECRET).body("{}").send().await.unwrap().status(), StatusCode::BAD_REQUEST);
        assert_eq!(client.post(&base).bearer_auth(SECRET).body(body.clone()).send().await.unwrap().status(), StatusCode::ACCEPTED);
        // Retrying before the webview consumes the payload must not replace or duplicate it.
        assert_eq!(client.post(&base).bearer_auth(SECRET).body(body.clone()).send().await.unwrap().status(), StatusCode::ACCEPTED);
        let received = bridge.receive(ID, SECRET).await.unwrap().unwrap();
        assert_eq!(serde_json::to_vec(&received).unwrap(), body);
        assert_eq!(client.post(&base).bearer_auth(SECRET).body(body).send().await.unwrap().status(), StatusCode::OK);
        assert!(bridge.receive(ID, SECRET).await.unwrap().is_none());
        assert_eq!(client.post(&base).bearer_auth(SECRET).body("{}").send().await.unwrap().status(), StatusCode::CONFLICT);
        let response = client.get(format!("{base}/probe?challenge={challenge}")).send().await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(value["status"], "delivered");
        assert_eq!(client.post(&base).bearer_auth(SECRET).body(vec![b' '; MAX_BODY + 1]).send().await.unwrap().status(), StatusCode::PAYLOAD_TOO_LARGE);
        bridge.sessions.lock().await.get_mut(ID).unwrap().expires = Instant::now() - Duration::from_secs(1);
        assert_eq!(client.get(format!("{base}/probe?challenge={challenge}")).send().await.unwrap().status(), StatusCode::NOT_FOUND);
        bridge.server.lock().await.take().unwrap().abort();
    }
}
