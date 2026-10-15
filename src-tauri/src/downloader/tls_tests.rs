use super::*;
use cbc::cipher::BlockEncryptMut;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::PrivatePkcs8KeyDer;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;

const AES_KEY: &[u8; 16] = b"0123456789abcdef";
const PAYLOAD: &[u8] = b"local HTTPS video segment";

pub(crate) struct HttpsFixture {
    port: u16,
    ca: Vec<u8>,
    stopped: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl HttpsFixture {
    pub(crate) fn expired() -> Self {
        Self::new(true)
    }

    fn new(expired: bool) -> Self {
        // Generate short-lived certificates relative to now, so the valid control
        // never ages out or exceeds platform certificate lifetime limits.
        let now = std::time::SystemTime::now();
        let day = Duration::from_secs(24 * 60 * 60);
        let mut ca_params = CertificateParams::default();
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "M3U8Quicker test CA");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        ca_params.not_before = (now - day * 30).into();
        ca_params.not_after = (now + day * 30).into();
        let ca_key = KeyPair::generate().unwrap();
        let ca = ca_params.self_signed(&ca_key).unwrap().der().to_vec();
        let issuer = Issuer::from_params(&ca_params, &ca_key);
        let mut params = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
        params
            .distinguished_name
            .push(DnType::CommonName, "localhost");
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.not_before = (now - day * 3).into();
        params.not_after = if expired {
            now - day * 2
        } else {
            now + day * 7
        }
        .into();
        let key = KeyPair::generate().unwrap();
        let certificate = params.signed_by(&key, &issuer).unwrap();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.der().clone()],
            PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
        )
        .unwrap();
        let config = Arc::new(config);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stopped = Arc::new(AtomicBool::new(false));
        let server_stopped = stopped.clone();
        let worker = std::thread::spawn(move || {
            while !server_stopped.load(Ordering::Relaxed) {
                let (socket, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("HTTPS fixture accept failed: {error}"),
                };
                // On macOS accepted sockets inherit the listener's nonblocking mode.
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let connection = rustls::ServerConnection::new(config.clone()).unwrap();
                let mut stream = rustls::StreamOwned::new(connection, socket);
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") && request.len() < 16 * 1024 {
                    if stream.read_exact(&mut byte).is_err() {
                        break;
                    }
                    request.push(byte[0]);
                }
                if !request.ends_with(b"\r\n\r\n") {
                    continue;
                }
                let request = String::from_utf8(request).unwrap();
                let path = request.split_whitespace().nth(1).unwrap();
                let body = match path {
                    "/index.m3u8" => b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x00000000000000000000000000000000\n#EXTINF:1,\nsegment.ts\n#EXT-X-ENDLIST\n".to_vec(),
                    "/key.bin" => AES_KEY.to_vec(),
                    "/user-agent" => request.lines().find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("user-agent").then(|| value.trim().as_bytes().to_vec())
                    }).unwrap_or_default(),
                    "/segment.ts" => {
                        let mut buffer = vec![0; PAYLOAD.len() + 16];
                        buffer[..PAYLOAD.len()].copy_from_slice(PAYLOAD);
                        cbc::Encryptor::<Aes128>::new(AES_KEY.into(), (&[0; 16]).into())
                            .encrypt_padded_mut::<Pkcs7>(&mut buffer, PAYLOAD.len()).unwrap().to_vec()
                    }
                    _ => PAYLOAD.to_vec(),
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                // Clients may close after inspecting the TLS connection.
                let _ = stream
                    .write_all(response.as_bytes())
                    .and_then(|_| stream.write_all(&body));
            }
        });
        Self {
            port,
            ca,
            stopped,
            worker: Some(worker),
        }
    }

    pub(crate) fn url(&self, path: &str) -> String {
        format!("https://localhost:{}{path}", self.port)
    }
}

impl Drop for HttpsFixture {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}

fn trusted_client(server: &HttpsFixture, ignore_errors: bool) -> reqwest::Client {
    http_client_builder(None, default_user_agent(), ignore_errors)
        .unwrap()
        .add_root_certificate(reqwest::Certificate::from_der(&server.ca).unwrap())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

fn assert_certificate_error(error: reqwest::Error, url: &str) {
    assert_eq!(
        AppError::from(error).to_string(),
        format!(
            "Network error: {}",
            crate::localized!("httpsCertificateValidationFailed", url)
        ),
    );
}

#[tokio::test]
async fn https_certificate_validation_rejects_expired_but_accepts_valid_certificate() {
    let valid = HttpsFixture::new(false);
    let expired = HttpsFixture::expired();
    let strict = trusted_client(&valid, false);
    assert_eq!(
        strict
            .get(valid.url("/video.mp4"))
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .as_ref(),
        PAYLOAD
    );
    let error = trusted_client(&expired, false)
        .get(expired.url("/video.mp4"))
        .send()
        .await
        .unwrap_err();
    assert!(
        !error.is_timeout(),
        "must reject the certificate, not time out: {error}"
    );
    assert_certificate_error(error, &expired.url("/video.mp4"));

    assert_eq!(
        trusted_client(&expired, true)
            .get(expired.url("/video.mp4"))
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .as_ref(),
        PAYLOAD
    );
    assert!(
        trusted_client(&expired, false)
            .get(expired.url("/video.mp4"))
            .send()
            .await
            .is_err(),
        "turning the option off must restore validation"
    );
}

#[tokio::test]
async fn https_certificate_validation_covers_untrusted_ca_and_hostname_mismatch() {
    let server = HttpsFixture::new(false);
    let strict = build_http_client(None, default_user_agent(), false).unwrap();
    let untrusted_url = server.url("/video.mp4");
    let error = strict.get(&untrusted_url).send().await.unwrap_err();
    assert_certificate_error(error, &untrusted_url);
    let mismatched_url = format!("https://127.0.0.1:{}/video.mp4", server.port);
    let error = trusted_client(&server, false)
        .get(&mismatched_url)
        .send()
        .await
        .unwrap_err();
    assert_certificate_error(error, &mismatched_url);
    let permissive = build_http_client(None, default_user_agent(), true).unwrap();
    assert_eq!(
        permissive
            .get(&mismatched_url)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .as_ref(),
        PAYLOAD
    );
}

#[tokio::test]
async fn ignoring_https_certificate_errors_downloads_playlist_key_and_segment() {
    let server = HttpsFixture::expired();
    let client = build_http_client(None, default_user_agent(), true).unwrap();
    let headers = Arc::new(RequestHeaders::new());
    let plan = prepare_hls_download(&client, &server.url("/index.m3u8"), &headers, None)
        .await
        .unwrap();
    let PreparedHlsDownload::Single(plan) = plan else {
        panic!("expected single stream")
    };
    assert_eq!(plan.segments.len(), 1);
    let segment = &plan.segments[0];
    assert_eq!(segment.encryption.as_ref().unwrap().key_bytes, AES_KEY);
    let path = std::env::temp_dir().join(format!("m3u8quicker-tls-{}.ts", uuid::Uuid::new_v4()));
    download_segment(
        Arc::new(RwLock::new(client)),
        Arc::new(DownloadRateLimiter::new(0)),
        headers,
        &segment.uri,
        &path,
        segment.byte_range.as_ref(),
        segment.encryption.as_ref(),
        segment.sequence_number,
        &CancellationToken::new(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(tokio::fs::read(&path).await.unwrap(), PAYLOAD);
    tokio::fs::remove_file(path).await.unwrap();
}
