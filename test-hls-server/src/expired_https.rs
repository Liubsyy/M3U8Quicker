use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use axum::extract::{Path as AxumPath, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_rustls::rustls::{self, pki_types::PrivatePkcs8KeyDer};
use tokio_rustls::{server::TlsStream, TlsAcceptor};

use crate::{AppError, Command};

const PLAYLIST: &str = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:2.000,\nsegment.ts\n#EXT-X-ENDLIST\n";

pub(crate) struct ExpiredTlsListener {
    listener: TcpListener,
    acceptor: TlsAcceptor,
}

impl axum::serve::Listener for ExpiredTlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let (socket, address) = match self.listener.accept().await {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("[expired-https] accept: {error}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            match tokio::time::timeout(Duration::from_secs(5), self.acceptor.accept(socket)).await {
                Ok(Ok(stream)) => return (stream, address),
                Ok(Err(error)) => {
                    eprintln!("[expired-https] 握手失败（客户端校验证书时属于预期结果）: {error}")
                }
                Err(_) => eprintln!("[expired-https] TLS 握手超时"),
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

fn expired_tls_config() -> Result<(Arc<rustls::ServerConfig>, String), Box<dyn std::error::Error>> {
    let now = SystemTime::now();
    let day = Duration::from_secs(24 * 60 * 60);
    let mut root = CertificateParams::default();
    root.distinguished_name
        .push(DnType::CommonName, "Test HLS Server local CA");
    root.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    root.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    root.not_before = (now - day * 30).into();
    root.not_after = (now + day * 30).into();
    let root_key = KeyPair::generate()?;
    let ca = root.self_signed(&root_key)?;
    let issuer = Issuer::from_params(&root, &root_key);

    let mut leaf = CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()])?;
    leaf.distinguished_name
        .push(DnType::CommonName, "localhost expired test");
    leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    // Always expired, independently of the day the test server is started.
    leaf.not_before = (now - day * 3).into();
    leaf.not_after = (now - day * 2).into();
    let key = KeyPair::generate()?;
    let certificate = leaf.signed_by(&key, &issuer)?;
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(
        vec![certificate.der().clone(), ca.der().clone()],
        PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
    )?;
    Ok((Arc::new(config), ca.pem()))
}

pub(crate) async fn bind(
    temp_dir: &Path,
) -> Result<ExpiredTlsListener, Box<dyn std::error::Error>> {
    let (config, ca_pem) = expired_tls_config()?;
    let listener = TcpListener::bind("127.0.0.1:7879").await?;
    let ca_path = temp_dir.join("expired-https-ca.pem");
    tokio::fs::write(&ca_path, ca_pem).await?;
    println!("过期证书 HTTPS 监听: https://localhost:7879（仅本机）");
    println!(
        "测试 CA（每次启动重新生成，无需导入系统）: {}",
        ca_path.display()
    );
    Ok(ExpiredTlsListener {
        listener,
        acceptor: TlsAcceptor::from(config),
    })
}

#[derive(Clone)]
struct SampleState {
    directory: PathBuf,
    generation_lock: Arc<Mutex<()>>,
}

pub(crate) fn sample_routes(data_dir: PathBuf) -> Router {
    Router::new()
        .route("/tls-test", get(index))
        .route("/tls-test/{file}", get(sample_file))
        .with_state(SampleState {
            directory: data_dir.join("tls-test"),
            generation_lock: Arc::new(Mutex::new(())),
        })
}

async fn index() -> Html<&'static str> {
    Html(
        r#"<!doctype html><html lang="zh-CN"><head><meta charset="utf-8">
    <title>HTTPS 过期证书测试</title>
    <style>body{font-family:system-ui;max-width:900px;margin:40px auto;padding:20px;line-height:1.8}input{width:100%;box-sizing:border-box;padding:12px}code{background:#eee;padding:3px}</style></head>
    <body><h1>HTTPS 过期证书测试</h1>
    <p>内置 2 秒彩条视频，首次下载时通过 FFmpeg 自动生成，无需上传文件。播放列表与分片均走 HTTPS。</p>
    <p>复制下面地址到 M3U8 Quicker；点输入框可选中全文。</p>
    <input aria-label="过期证书 HLS 地址" readonly value="https://localhost:7879/tls-test/index.m3u8" onclick="this.select()">
    <ol><li>关闭「设置 → 网络 → 忽略 HTTPS 证书错误」，新建下载任务，应连接失败。</li>
    <li>开启开关，重新创建任务，应成功下载并能播放彩条视频。</li>
    <li>再次关闭开关，新建任务，应重新失败。每次使用新任务，避免已有分片影响结果。</li></ol>
    <p>HTTP 对照地址：<a href="/tls-test/index.m3u8">http://127.0.0.1:7878/tls-test/index.m3u8</a></p>
    <p>证书由本地测试 CA 签发，同时已过期；关闭校验时可以连接。浏览器直接打开 HTTPS 地址会拦截，使用本 HTTP 页面复制地址即可，无需在浏览器中放行或导入证书。</p>
    <p>7879 上也可访问已有的 HLS、DASH、MP4 和直播资源。TLS 证书每次启动重新生成，有效期在启动前两天结束。</p>
    <p><a href="/">返回首页</a></p></body></html>"#,
    )
}

async fn sample_file(
    State(state): State<SampleState>,
    AxumPath(file): AxumPath<String>,
) -> Result<Response, AppError> {
    if file != "index.m3u8" && file != "segment.ts" {
        return Err(AppError {
            status: StatusCode::NOT_FOUND,
            message: "测试文件不存在".to_string(),
        });
    }
    let _guard = state.generation_lock.lock().await;
    let segment_path = state.directory.join("segment.ts");
    if !segment_path.is_file() {
        tokio::fs::create_dir_all(&state.directory)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        let partial = state.directory.join("segment.tmp.ts");
        let result = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x180:rate=25",
                "-t",
                "2",
                "-c:v",
                "libx264",
                "-threads",
                "1",
                "-pix_fmt",
                "yuv420p",
                "-f",
                "mpegts",
            ])
            .arg(&partial)
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|error| {
                AppError::internal(format!("生成测试视频失败，请确认已安装 ffmpeg: {error}"))
            })?;
        if !result.status.success() {
            return Err(AppError::internal(format!(
                "生成测试视频失败: {}",
                String::from_utf8_lossy(&result.stderr)
            )));
        }
        tokio::fs::rename(partial, &segment_path)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    drop(_guard);
    println!("[expired-https sample] GET /tls-test/{file}");
    if file == "index.m3u8" {
        Ok((
            [
                (header::CONTENT_TYPE, "application/vnd.apple.mpegurl"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            PLAYLIST,
        )
            .into_response())
    } else {
        let bytes = tokio::fs::read(segment_path)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        Ok((
            [
                (header::CONTENT_TYPE, "video/mp2t"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            bytes,
        )
            .into_response())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::{pem::PemObject, CertificateDer, ServerName};

    #[tokio::test]
    async fn certificate_is_rejected_as_expired_even_when_ca_is_trusted() {
        let (config, ca_pem) = expired_tls_config().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let acceptor = TlsAcceptor::from(config);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let _ = acceptor.accept(stream).await;
        });
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from_pem_slice(ca_pem.as_bytes()).unwrap())
            .unwrap();
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let result = connector
            .connect(
                ServerName::try_from("localhost").unwrap(),
                TcpStream::connect(address).await.unwrap(),
            )
            .await;
        let error = result.unwrap_err();
        assert!(
            format!("{error:?}").contains("Expired"),
            "expected expiry error, got: {error:?}"
        );
        server.await.unwrap();
    }

    #[test]
    fn sample_playlist_keeps_segment_on_the_same_https_origin() {
        assert!(PLAYLIST.contains("\nsegment.ts\n"));
        assert!(!PLAYLIST.contains("http://"));
        assert!(PLAYLIST.ends_with("#EXT-X-ENDLIST\n"));
    }

    #[test]
    fn generated_resource_urls_keep_the_https_scheme() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(header::HOST, "localhost:7879".parse().unwrap());
        headers.insert("x-forwarded-proto", "https".parse().unwrap());
        assert_eq!(crate::request_base_url(&headers), "https://localhost:7879");
        headers.remove("x-forwarded-proto");
        headers.insert(header::HOST, "127.0.0.1:7878".parse().unwrap());
        assert_eq!(crate::request_base_url(&headers), "http://127.0.0.1:7878");
    }
}
