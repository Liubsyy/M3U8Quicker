use std::cmp::min;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Multipart, Path as AxumPath, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use chrono::Local;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    root_dir: PathBuf,
    data_dir: PathBuf,
    temp_dir: PathBuf,
}

const MAX_UPLOAD_BYTES: usize = 10 * 1024 * 1024 * 1024;
const MAX_DOWNLOAD_BYTES_PER_SECOND: usize = 1024 * 1024;
const THROTTLE_CHUNK_BYTES: usize = 16 * 1024;

#[derive(Debug, Deserialize)]
struct PathGenerateForm {
    input_path: String,
    playlist_name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct JobMeta {
    id: String,
    playlist_name: String,
    source_name: String,
    created_at: String,
}

#[derive(Debug)]
struct JobSummary {
    meta: JobMeta,
    segment_count: usize,
    playlist_path: String,
}

#[derive(Debug)]
struct AppError {
    status: StatusCode,
    message: String,
}

impl AppError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><title>测试服务器错误</title>\
             <style>body{{font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;padding:32px;line-height:1.6}}\
             .box{{max-width:840px;margin:0 auto;border:1px solid #e5e7eb;border-radius:16px;padding:24px;background:#fff}}\
             code{{background:#f3f4f6;padding:2px 6px;border-radius:6px}}a{{color:#2563eb}}</style></head>\
             <body><div class=\"box\"><h1>操作失败</h1><p>{}</p><p><a href=\"/\">返回首页</a></p></div></body></html>",
            escape_html(&self.message)
        );

        (self.status, Html(body)).into_response()
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let data_dir = root_dir.join("data");
    let temp_dir = root_dir.join("tmp");

    tokio::fs::create_dir_all(&data_dir).await?;
    tokio::fs::create_dir_all(&temp_dir).await?;

    let state = AppState {
        root_dir,
        data_dir,
        temp_dir,
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/healthz", get(healthz))
        .route(
            "/generate/upload",
            post(generate_from_upload).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)),
        )
        .route("/generate/path", post(generate_from_path))
        .route("/hls/{job_id}/{*file}", get(serve_hls_file))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], 7878));
    println!("Test HLS server listening at http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn healthz() -> &'static str {
    "ok"
}

async fn index(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Html<String>, AppError> {
    let jobs = load_jobs(&state).await?;
    let ffmpeg_ready = ffmpeg_available().await;
    let base_url = request_base_url(&headers);

    Ok(Html(render_index_page(
        &jobs,
        ffmpeg_ready,
        &base_url,
        &state.root_dir,
    )))
}

async fn generate_from_upload(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Redirect, AppError> {
    ensure_ffmpeg_available().await?;

    let upload_id = Uuid::new_v4().to_string();
    let upload_dir = state.temp_dir.join(&upload_id);
    tokio::fs::create_dir_all(&upload_dir)
        .await
        .map_err(|e| AppError::internal(format!("创建上传临时目录失败: {}", e)))?;

    let mut video_path: Option<PathBuf> = None;
    let mut source_name = String::new();
    let mut playlist_name: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("解析上传内容失败: {}", e)))?
    {
        let field_name = field.name().unwrap_or_default().to_string();

        if field_name == "playlist_name" {
            let text = field
                .text()
                .await
                .map_err(|e| AppError::bad_request(format!("读取表单字段失败: {}", e)))?;
            if !text.trim().is_empty() {
                playlist_name = Some(text);
            }
            continue;
        }

        if field_name != "video" {
            continue;
        }

        let original_name = field
            .file_name()
            .map(str::to_string)
            .unwrap_or_else(|| "upload.mp4".to_string());
        source_name = original_name.clone();
        let safe_name = sanitize_filename(&original_name, "upload.mp4");
        let file_path = upload_dir.join(safe_name);

        let mut file = tokio::fs::File::create(&file_path)
            .await
            .map_err(|e| AppError::internal(format!("创建上传文件失败: {}", e)))?;
        let mut field = field;

        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|e| AppError::bad_request(format!("读取上传文件失败: {}", e)))?
        {
            file.write_all(&chunk)
                .await
                .map_err(|e| AppError::internal(format!("写入上传文件失败: {}", e)))?;
        }

        video_path = Some(file_path);
    }

    let video_path =
        video_path.ok_or_else(|| AppError::bad_request("请先选择一个视频文件再上传"))?;
    let source_name = if source_name.is_empty() {
        video_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("upload.mp4")
            .to_string()
    } else {
        source_name
    };

    let result = create_hls_job(
        &state,
        &video_path,
        playlist_name,
        source_name,
    )
    .await;

    let _ = tokio::fs::remove_dir_all(&upload_dir).await;
    result?;
    Ok(Redirect::to("/"))
}

async fn generate_from_path(
    State(state): State<AppState>,
    Form(form): Form<PathGenerateForm>,
) -> Result<Redirect, AppError> {
    ensure_ffmpeg_available().await?;

    let input_path = PathBuf::from(form.input_path.trim());
    if form.input_path.trim().is_empty() {
        return Err(AppError::bad_request("请输入本地视频文件路径"));
    }

    let metadata = tokio::fs::metadata(&input_path)
        .await
        .map_err(|e| AppError::bad_request(format!("读取输入文件失败: {}", e)))?;
    if !metadata.is_file() {
        return Err(AppError::bad_request("输入路径不是一个有效视频文件"));
    }

    let source_name = input_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("video")
        .to_string();

    create_hls_job(&state, &input_path, form.playlist_name, source_name).await?;
    Ok(Redirect::to("/"))
}

async fn serve_hls_file(
    State(state): State<AppState>,
    AxumPath((job_id, file)): AxumPath<(String, String)>,
) -> Result<Response, AppError> {
    let clean_file = sanitize_relative_hls_path(&file)
        .ok_or_else(|| AppError::bad_request("非法文件路径"))?;
    let file_path = state.data_dir.join(&job_id).join(clean_file);

    let bytes = tokio::fs::read(&file_path)
        .await
        .map_err(|_| AppError {
            status: StatusCode::NOT_FOUND,
            message: "文件不存在".to_string(),
        })?;

    let content_type = content_type_for_path(&file_path);
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    if let Ok(value) = HeaderValue::from_str(&bytes.len().to_string()) {
        headers.insert(header::CONTENT_LENGTH, value);
    }

    let stream = async_stream::stream! {
        let mut offset = 0usize;
        while offset < bytes.len() {
            let end = min(offset + THROTTLE_CHUNK_BYTES, bytes.len());
            let chunk_len = end - offset;
            yield Result::<Bytes, Infallible>::Ok(Bytes::copy_from_slice(&bytes[offset..end]));
            offset = end;

            if offset < bytes.len() {
                let sleep_duration =
                    Duration::from_secs_f64(chunk_len as f64 / MAX_DOWNLOAD_BYTES_PER_SECOND as f64);
                tokio::time::sleep(sleep_duration).await;
            }
        }
    };

    Ok((StatusCode::OK, headers, Body::from_stream(stream)).into_response())
}

async fn create_hls_job(
    state: &AppState,
    input_path: &Path,
    playlist_name: Option<String>,
    source_name: String,
) -> Result<(), AppError> {
    let job_id = Uuid::new_v4().to_string();
    let requested_name = playlist_name.unwrap_or_else(|| {
        input_path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("sample")
            .to_string()
    });
    let playlist_name = sanitize_slug(&requested_name, "sample");
    let job_dir = state.data_dir.join(&job_id);

    tokio::fs::create_dir_all(&job_dir)
        .await
        .map_err(|e| AppError::internal(format!("创建输出目录失败: {}", e)))?;

    let playlist_path = job_dir.join("index.m3u8");
    let segment_pattern = job_dir.join("seg_%04d.ts");

    let output = Command::new("ffmpeg")
        .arg("-y")
        .arg("-i")
        .arg(input_path)
        .args(["-c:v", "libx264"])
        .args(["-c:a", "aac"])
        .args(["-f", "hls"])
        .args(["-hls_time", "6"])
        .args(["-hls_playlist_type", "vod"])
        .args(["-hls_segment_filename"])
        .arg(&segment_pattern)
        .arg(&playlist_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| AppError::internal(format!("启动 ffmpeg 失败: {}", e)))?;

    if !output.status.success() {
        let _ = tokio::fs::remove_dir_all(&job_dir).await;
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        let detail = if stderr.is_empty() {
            "未返回额外错误信息".to_string()
        } else {
            stderr
                .lines()
                .rev()
                .take(6)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join(" | ")
        };
        return Err(AppError::internal(format!(
            "ffmpeg 执行失败，退出码: {}。错误详情: {}。请确认输入视频可读，且本机已正确安装 ffmpeg。",
            output.status.code().unwrap_or(-1),
            detail
        )));
    }

    let meta = JobMeta {
        id: job_id.clone(),
        playlist_name,
        source_name,
        created_at: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    };
    let meta_json = serde_json::to_vec_pretty(&meta)
        .map_err(|e| AppError::internal(format!("序列化任务信息失败: {}", e)))?;
    tokio::fs::write(job_dir.join("job.json"), meta_json)
        .await
        .map_err(|e| AppError::internal(format!("写入任务信息失败: {}", e)))?;

    Ok(())
}

async fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|status| status.success())
        .unwrap_or(false)
}

async fn ensure_ffmpeg_available() -> Result<(), AppError> {
    if ffmpeg_available().await {
        Ok(())
    } else {
        Err(AppError::bad_request(
            "当前系统未检测到 ffmpeg。请先安装 ffmpeg，再使用这个测试服务器生成 HLS 文件。",
        ))
    }
}

async fn load_jobs(state: &AppState) -> Result<Vec<JobSummary>, AppError> {
    let mut jobs = Vec::new();
    let mut entries = tokio::fs::read_dir(&state.data_dir)
        .await
        .map_err(|e| AppError::internal(format!("读取数据目录失败: {}", e)))?;

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| AppError::internal(format!("读取任务目录失败: {}", e)))?
    {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let meta_path = path.join("job.json");
        let playlist_path = path.join("index.m3u8");
        if !meta_path.is_file() || !playlist_path.is_file() {
            continue;
        }

        let meta_bytes = tokio::fs::read(&meta_path)
            .await
            .map_err(|e| AppError::internal(format!("读取任务信息失败: {}", e)))?;
        let meta: JobMeta = serde_json::from_slice(&meta_bytes)
            .map_err(|e| AppError::internal(format!("解析任务信息失败: {}", e)))?;

        let mut segment_count = 0usize;
        let mut files = tokio::fs::read_dir(&path)
            .await
            .map_err(|e| AppError::internal(format!("读取切片目录失败: {}", e)))?;
        while let Some(file) = files
            .next_entry()
            .await
            .map_err(|e| AppError::internal(format!("读取切片文件失败: {}", e)))?
        {
            let file_path = file.path();
            if file_path.extension().and_then(|ext| ext.to_str()) == Some("ts") {
                segment_count += 1;
            }
        }

        jobs.push(JobSummary {
            playlist_path: format!("/hls/{}/index.m3u8", meta.id),
            meta,
            segment_count,
        });
    }

    jobs.sort_by(|a, b| b.meta.created_at.cmp(&a.meta.created_at));
    Ok(jobs)
}

fn render_index_page(
    jobs: &[JobSummary],
    ffmpeg_ready: bool,
    base_url: &str,
    root_dir: &Path,
) -> String {
    let status_badge = if ffmpeg_ready {
        "<span class=\"badge badge-ok\">ffmpeg 已就绪</span>".to_string()
    } else {
        "<span class=\"badge badge-warn\">未检测到 ffmpeg</span>".to_string()
    };
    let download_limit_text = format!(
        "{} MB/s（约 {} Mb/s）",
        MAX_DOWNLOAD_BYTES_PER_SECOND as f64 / (1024.0 * 1024.0),
        (MAX_DOWNLOAD_BYTES_PER_SECOND as f64 * 8.0) / 1_000_000.0,
    );
    let default_playlist_url = jobs
        .first()
        .map(|job| format!("{}{}", base_url, job.playlist_path))
        .unwrap_or_default();
    let player_html = r#"
        <section class="panel player-panel">
          <div class="section-head section-head-tight">
            <div>
              <h2>M3U8 在线播放</h2>
              <p>支持直接播放当前服务生成的测试流，也支持手动粘贴任意 M3U8 地址。</p>
            </div>
          </div>
          <div class="player-toolbar">
            <div class="field field-grow">
              <label for="player_url">M3U8 地址</label>
              <input id="player_url" class="mono" type="text" value="__DEFAULT_PLAYLIST_URL__" placeholder="http://127.0.0.1:7878/hls/.../index.m3u8">
            </div>
            <div class="player-buttons">
              <button id="player_load" type="button">开始播放</button>
              <button id="player_stop" type="button" class="button-secondary">停止</button>
            </div>
          </div>
          <p id="player_status" class="player-status">等待选择播放源。</p>
          <video id="m3u8_player" class="player-video" controls playsinline preload="metadata"></video>
          <script>
            (() => {
              const player = document.getElementById('m3u8_player');
              const urlInput = document.getElementById('player_url');
              const loadButton = document.getElementById('player_load');
              const stopButton = document.getElementById('player_stop');
              const statusText = document.getElementById('player_status');
              let hlsInstance = null;
              let scriptPromise = null;

              function setStatus(message) {
                statusText.textContent = message;
              }

              function cleanupPlayer() {
                if (hlsInstance) {
                  hlsInstance.destroy();
                  hlsInstance = null;
                }
                player.pause();
                player.removeAttribute('src');
                player.load();
              }

              function attachNative(url) {
                cleanupPlayer();
                player.src = url;
                player.load();
                player.play().catch(() => {});
                setStatus('已使用浏览器原生能力加载该 M3U8。');
              }

              async function ensureHlsScript() {
                if (window.Hls) {
                  return window.Hls;
                }
                if (!scriptPromise) {
                  scriptPromise = new Promise((resolve, reject) => {
                    const script = document.createElement('script');
                    script.src = 'https://cdn.jsdelivr.net/npm/hls.js@1.6.15/dist/hls.min.js';
                    script.onload = () => resolve(window.Hls);
                    script.onerror = () => reject(new Error('加载 hls.js 失败，请检查网络或改用 Safari。'));
                    document.head.appendChild(script);
                  });
                }
                return scriptPromise;
              }

              async function playM3u8(url) {
                const source = url.trim();
                if (!source) {
                  setStatus('请先输入一个 M3U8 地址。');
                  urlInput.focus();
                  return;
                }

                setStatus('正在加载播放流...');

                if (player.canPlayType('application/vnd.apple.mpegurl')) {
                  attachNative(source);
                  return;
                }

                const Hls = await ensureHlsScript();
                if (!Hls || !Hls.isSupported()) {
                  throw new Error('当前浏览器不支持 M3U8 播放，请改用 Safari 或启用 hls.js 支持的环境。');
                }

                cleanupPlayer();
                hlsInstance = new Hls({
                  enableWorker: true,
                });
                hlsInstance.loadSource(source);
                hlsInstance.attachMedia(player);
                hlsInstance.on(Hls.Events.MANIFEST_PARSED, () => {
                  setStatus('播放列表已加载，正在尝试开始播放。');
                  player.play().catch(() => {
                    setStatus('播放列表已加载，点击播放器上的播放按钮即可开始。');
                  });
                });
                hlsInstance.on(Hls.Events.ERROR, (_event, data) => {
                  if (data && data.fatal) {
                    setStatus('播放失败：' + (data.details || data.type || '未知错误'));
                  }
                });
              }

              loadButton.addEventListener('click', () => {
                playM3u8(urlInput.value).catch((error) => {
                  console.error('Failed to play m3u8', error);
                  setStatus('播放失败：' + (error.message || String(error)));
                });
              });

              stopButton.addEventListener('click', () => {
                cleanupPlayer();
                setStatus('已停止播放。');
              });

              urlInput.addEventListener('keydown', (event) => {
                if (event.key === 'Enter') {
                  event.preventDefault();
                  loadButton.click();
                }
              });

              document.querySelectorAll('.js-play-job').forEach((button) => {
                button.addEventListener('click', () => {
                  const playlistUrl = button.getAttribute('data-playlist-url') || '';
                  urlInput.value = playlistUrl;
                  loadButton.click();
                  player.scrollIntoView({ behavior: 'smooth', block: 'center' });
                });
              });
            })();
          </script>
        </section>
    "#
    .replace(
        "__DEFAULT_PLAYLIST_URL__",
        &escape_html(&default_playlist_url),
    );

    let jobs_html = if jobs.is_empty() {
        "<div class=\"empty\">还没有生成过测试流。上传一个视频或填写本地路径试试。</div>".to_string()
    } else {
        jobs.iter()
            .map(|job| {
                let playlist_url = format!("{}{}", base_url, job.playlist_path);
                format!(
                    "<article class=\"job-card\">\
                       <div class=\"job-top\">\
                         <div><h3>{}</h3><p>来源文件：{}</p></div>\
                         <span class=\"job-time\">{}</span>\
                       </div>\
                       <p>任务 ID：<code>{}</code></p>\
                       <p>切片数量：<strong>{}</strong></p>\
                       <p>M3U8 地址：<code>{}</code></p>\
                       <div class=\"actions\">\
                         <button type=\"button\" class=\"button-link js-play-job\" data-playlist-url=\"{}\">在线播放</button>\
                         <a href=\"{}\" target=\"_blank\" rel=\"noreferrer\">打开 M3U8</a>\
                         <a href=\"{}\" download>下载 M3U8</a>\
                       </div>\
                     </article>",
                    escape_html(&job.meta.playlist_name),
                    escape_html(&job.meta.source_name),
                    escape_html(&job.meta.created_at),
                    escape_html(&job.meta.id),
                    job.segment_count,
                    escape_html(&playlist_url),
                    escape_html(&playlist_url),
                    job.playlist_path,
                    job.playlist_path,
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };

    format!(
        "<!doctype html>\
         <html lang=\"zh-CN\">\
         <head>\
           <meta charset=\"utf-8\">\
           <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
           <title>Test HLS Server</title>\
           <style>\
             :root{{color-scheme:light;background:#f5f7fb;color:#101828}}\
             *{{box-sizing:border-box}}\
             body{{margin:0;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;background:linear-gradient(180deg,#eef4ff 0%,#f8fafc 100%);color:#0f172a}}\
             main{{max-width:1100px;margin:0 auto;padding:32px 20px 48px}}\
             .hero{{background:#fff;border:1px solid #dbe5f0;border-radius:24px;padding:28px 28px 24px;box-shadow:0 12px 40px rgba(15,23,42,.06)}}\
             h1{{margin:0;font-size:32px}}\
             h2{{margin:0 0 16px;font-size:20px}}\
             h3{{margin:0 0 8px;font-size:18px}}\
             p{{margin:8px 0;color:#475467}}\
             code{{font-family:'SFMono-Regular',Consolas,monospace;background:#f2f4f7;padding:2px 6px;border-radius:6px;word-break:break-all}}\
             .hero-top{{display:flex;justify-content:space-between;align-items:center;gap:16px;flex-wrap:wrap}}\
             .badge{{display:inline-flex;align-items:center;padding:6px 12px;border-radius:999px;font-size:14px;font-weight:600}}\
             .badge-ok{{background:#dcfce7;color:#166534}}\
             .badge-warn{{background:#fef3c7;color:#92400e}}\
             .grid{{display:grid;grid-template-columns:repeat(auto-fit,minmax(320px,1fr));gap:20px;margin-top:24px}}\
             .panel{{background:#fff;border:1px solid #dbe5f0;border-radius:20px;padding:22px;box-shadow:0 10px 30px rgba(15,23,42,.05)}}\
             label{{display:block;font-weight:600;margin-bottom:8px}}\
             input[type='text'],input[type='file']{{width:100%;padding:12px 14px;border:1px solid #cbd5e1;border-radius:12px;font:inherit;background:#fff}}\
             .field{{display:grid;gap:8px;margin-bottom:16px}}\
             button{{appearance:none;border:none;border-radius:12px;background:#2563eb;color:#fff;padding:12px 16px;font:inherit;font-weight:700;cursor:pointer}}\
             button:hover{{background:#1d4ed8}}\
             .hint{{font-size:14px;color:#667085}}\
             .section-head{{display:flex;justify-content:space-between;align-items:end;gap:16px;margin:28px 0 16px;flex-wrap:wrap}}\
             .section-head-tight{{margin:0 0 16px}}\
             .jobs{{display:grid;gap:16px}}\
             .job-card{{background:#fff;border:1px solid #dbe5f0;border-radius:18px;padding:20px;box-shadow:0 8px 24px rgba(15,23,42,.05)}}\
             .job-top{{display:flex;justify-content:space-between;align-items:start;gap:16px;flex-wrap:wrap}}\
             .job-time{{font-size:13px;color:#667085}}\
             .actions{{display:flex;gap:12px;flex-wrap:wrap;margin-top:14px}}\
             .actions a{{text-decoration:none;color:#2563eb;font-weight:600}}\
             .actions .button-link{{appearance:none;border:none;padding:0;background:none;color:#2563eb;font:inherit;font-weight:600;cursor:pointer}}\
             .empty{{background:#fff;border:1px dashed #cbd5e1;border-radius:18px;padding:28px;color:#475467}}\
             .mono{{font-family:'SFMono-Regular',Consolas,monospace}}\
             .player-panel{{margin-top:24px}}\
             .player-toolbar{{display:flex;gap:16px;align-items:end;flex-wrap:wrap}}\
             .field-grow{{flex:1 1 420px;margin-bottom:0}}\
             .player-buttons{{display:flex;gap:12px;flex-wrap:wrap}}\
             .button-secondary{{background:#e2e8f0;color:#0f172a}}\
             .button-secondary:hover{{background:#cbd5e1}}\
             .player-status{{margin:14px 0 12px;font-size:14px;color:#334155;min-height:22px}}\
             .player-video{{width:100%;border-radius:18px;background:#020617;aspect-ratio:16/9}}\
           </style>\
         </head>\
         <body>\
           <main>\
             <section class=\"hero\">\
               <div class=\"hero-top\">\
                 <div>\
                   <h1>Test HLS Server</h1>\
                   <p>把本地视频快速切成 <code>.m3u8</code> 和 <code>.ts</code>，专门给当前仓库做下载联调。</p>\
                   <p>当前 HLS 响应限速：<code>{}</code></p>\
                 </div>\
                 {}\
               </div>\
               <p>服务根目录：<code>{}</code></p>\
               <p>生成后的文件会放在 <code>{}</code>。</p>\
             </section>\
             <section class=\"grid\">\
               <form class=\"panel\" action=\"/generate/upload\" method=\"post\" enctype=\"multipart/form-data\">\
                 <h2>上传视频并生成</h2>\
                 <div class=\"field\">\
                   <label for=\"video\">视频文件</label>\
                   <input id=\"video\" type=\"file\" name=\"video\" accept=\"video/*\" required>\
                 </div>\
                 <div class=\"field\">\
                   <label for=\"upload_name\">播放列表名称（可选）</label>\
                   <input id=\"upload_name\" type=\"text\" name=\"playlist_name\" placeholder=\"例如 demo-video\">\
                 </div>\
                 <p class=\"hint\">上传后会先保存到临时目录，再调用本机 ffmpeg 生成 HLS。</p>\
                 <button type=\"submit\">开始生成</button>\
               </form>\
               <form class=\"panel\" action=\"/generate/path\" method=\"post\">\
                 <h2>使用本地路径生成</h2>\
                 <div class=\"field\">\
                   <label for=\"input_path\">本地视频路径</label>\
                   <input id=\"input_path\" class=\"mono\" type=\"text\" name=\"input_path\" placeholder=\"/Users/name/Movies/demo.mp4\" required>\
                 </div>\
                 <div class=\"field\">\
                   <label for=\"path_name\">播放列表名称（可选）</label>\
                   <input id=\"path_name\" type=\"text\" name=\"playlist_name\" placeholder=\"例如 local-sample\">\
                 </div>\
                 <p class=\"hint\">适合本机已有大文件，避免浏览器上传等待。</p>\
                 <button type=\"submit\">按路径生成</button>\
               </form>\
             </section>\
             {}\
             <section>\
               <div class=\"section-head\">\
                 <div>\
                   <h2>已生成的测试流</h2>\
                   <p>生成完成后，可以直接把 M3U8 地址喂给主应用测试下载。</p>\
                 </div>\
               </div>\
               <div class=\"jobs\">{}</div>\
             </section>\
           </main>\
         </body>\
         </html>",
        escape_html(&download_limit_text),
        status_badge,
        escape_html(&root_dir.to_string_lossy()),
        escape_html(&root_dir.join("data").to_string_lossy()),
        player_html,
        jobs_html,
    )
}

fn request_base_url(headers: &HeaderMap) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("127.0.0.1:7878");
    format!("http://{}", host)
}

fn sanitize_relative_hls_path(path: &str) -> Option<PathBuf> {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return None;
    }

    let mut clean = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }

    if clean.as_os_str().is_empty() {
        return None;
    }

    Some(clean)
}

fn content_type_for_path(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()).unwrap_or_default() {
        "m3u8" => "application/vnd.apple.mpegurl",
        "ts" => "video/mp2t",
        "json" => "application/json; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn sanitize_filename(input: &str, fallback: &str) -> String {
    let sanitized = input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string();

    if sanitized.is_empty() {
        fallback.to_string()
    } else {
        sanitized
    }
}

fn sanitize_slug(input: &str, fallback: &str) -> String {
    let lowered = input.trim().to_lowercase();
    let slug = lowered
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    if slug.is_empty() {
        fallback.to_string()
    } else {
        slug
    }
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
