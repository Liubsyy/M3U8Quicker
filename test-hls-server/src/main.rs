use std::cmp::min;
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use aes::{Aes128, Aes192, Aes256};
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Form, Multipart, Path as AxumPath, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Json;
use axum::Router;
use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
use chrono::Local;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::Mutex;
use uuid::Uuid;

type Aes128CbcEnc = cbc::Encryptor<Aes128>;
type Aes192CbcEnc = cbc::Encryptor<Aes192>;
type Aes256CbcEnc = cbc::Encryptor<Aes256>;

#[derive(Clone)]
struct AppState {
    root_dir: PathBuf,
    data_dir: PathBuf,
    temp_dir: PathBuf,
    mp4_source_path: Arc<Mutex<Option<PathBuf>>>,
    live_source_path: Arc<Mutex<Option<PathBuf>>>,
    live_jobs: Arc<Mutex<HashMap<String, Arc<LiveJob>>>>,
}

#[derive(Debug, Clone)]
struct LiveSegment {
    file_name: String,
    duration: f64,
}

#[derive(Debug)]
struct LiveJob {
    id: String,
    source_name: String,
    target_duration: u64,
    ts_segments: Vec<LiveSegment>,
    fmp4_segments: Vec<LiveSegment>,
    fmp4_init_name: String,
    started_at: Instant,
    created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct LiveJobMeta {
    id: String,
    source_name: String,
    created_at: String,
    #[serde(default)]
    target_duration: Option<u64>,
    #[serde(default)]
    fmp4_init_name: Option<String>,
}

const MAX_UPLOAD_BYTES: usize = 10 * 1024 * 1024 * 1024;
const MAX_DOWNLOAD_BYTES_PER_SECOND: usize = 1024 * 1024;
const THROTTLE_CHUNK_BYTES: usize = 16 * 1024;

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
    dash_manifest_path: Option<String>,
    dash_json_path: Option<String>,
    aes128_playlist_path: Option<String>,
    aes192_playlist_path: Option<String>,
    aes256_playlist_path: Option<String>,
}

#[derive(Debug)]
struct AppError {
    status: StatusCode,
    message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HlsEncryptionMode {
    None,
    Aes128,
    Aes192,
    Aes256,
}

impl HlsEncryptionMode {
    fn all_encrypted() -> [Self; 3] {
        [Self::Aes128, Self::Aes192, Self::Aes256]
    }

    fn playlist_file_name(self) -> &'static str {
        match self {
            Self::None => "index.m3u8",
            Self::Aes128 => "index-aes128.m3u8",
            Self::Aes192 => "index-aes192.m3u8",
            Self::Aes256 => "index-aes256.m3u8",
        }
    }

    fn segment_file_pattern(self) -> &'static str {
        match self {
            Self::None => "seg_%04d.ts",
            Self::Aes128 => "enc_seg_%04d.ts",
            Self::Aes192 => "enc192_seg_%04d.ts",
            Self::Aes256 => "enc256_seg_%04d.ts",
        }
    }

    fn segment_name(self, index: usize) -> String {
        match self {
            Self::None => format!("seg_{index:04}.ts"),
            Self::Aes128 => format!("enc_seg_{index:04}.ts"),
            Self::Aes192 => format!("enc192_seg_{index:04}.ts"),
            Self::Aes256 => format!("enc256_seg_{index:04}.ts"),
        }
    }

    fn key_file_name(self) -> &'static str {
        match self {
            Self::None => "plain.key",
            Self::Aes128 => "enc-aes128.key",
            Self::Aes192 => "enc-aes192.key",
            Self::Aes256 => "enc-aes256.key",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::None => "普通流",
            Self::Aes128 => "AES-128",
            Self::Aes192 => "AES-192",
            Self::Aes256 => "AES-256",
        }
    }

    fn key_len(self) -> usize {
        match self {
            Self::None => 0,
            Self::Aes128 => 16,
            Self::Aes192 => 24,
            Self::Aes256 => 32,
        }
    }
}

#[derive(Debug)]
struct EncryptionArtifacts {
    key_bytes: Vec<u8>,
    iv: [u8; 16],
    key_uri: String,
}

#[derive(Debug, Clone)]
struct PlaylistVariant {
    label: &'static str,
    path: String,
}

#[derive(Debug, Deserialize)]
struct Mp4PathForm {
    path: String,
}

#[derive(Debug, Serialize)]
struct Mp4PickResponse {
    selected: bool,
    path: Option<String>,
    message: Option<String>,
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
        mp4_source_path: Arc::new(Mutex::new(
            std::env::var_os("TEST_HLS_SERVER_MP4_PATH").map(PathBuf::from),
        )),
        live_source_path: Arc::new(Mutex::new(
            std::env::var_os("TEST_HLS_SERVER_LIVE_PATH").map(PathBuf::from),
        )),
        live_jobs: Arc::new(Mutex::new(HashMap::new())),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/dash", get(dash_index))
        .route("/mp4", get(mp4_index).post(set_mp4_source))
        .route("/mp4/pick", post(pick_mp4_source))
        .route("/healthz", get(healthz))
        .route(
            "/generate/upload",
            post(generate_from_upload).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)),
        )
        .route(
            "/generate/dash/upload",
            post(generate_dash_from_upload).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)),
        )
        .route(
            "/generate/local-file",
            post(generate_from_local_file).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)),
        )
        .route(
            "/generate/dash/local-file",
            post(generate_dash_from_local_file).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)),
        )
        .route("/hls/{job_id}/{*file}", get(serve_hls_file))
        .route("/dash-test/{job_id}/{*file}", get(serve_dash_file))
        .route("/mp4/local-file.mp4", get(serve_mp4_test_file))
        .route("/live", get(live_index).post(set_live_source))
        .route("/live/pick", post(pick_live_source))
        .route("/generate/live", post(generate_live_from_local_file))
        .route("/live-test/{job_id}/{flavor}/{file}", get(serve_live_file))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], 7878));
    println!("Test HLS server listening at http://{}", addr);
    println!("DASH generation page: http://{}/dash", addr);
    println!("Direct MP4 playback page: http://{}/mp4", addr);
    println!("HLS Live simulation page: http://{}/live", addr);
    println!("Direct MP4 test URL: http://{}/mp4/local-file.mp4", addr);
    println!(
        "DASH test MPD URL template: http://{}/dash-test/<job_id>/manifest.mpd",
        addr
    );
    println!(
        "DASH test JSON URL template: http://{}/dash-test/<job_id>/manifest.json",
        addr
    );

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

async fn dash_index(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Html<String>, AppError> {
    let jobs = load_dash_jobs(&state).await?;
    let ffmpeg_ready = ffmpeg_available().await;
    let base_url = request_base_url(&headers);

    Ok(Html(render_dash_page(
        &jobs,
        ffmpeg_ready,
        &base_url,
        &state.root_dir,
    )))
}

async fn mp4_index(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Html<String>, AppError> {
    let base_url = request_base_url(&headers);
    let mp4_url = format!("{}/mp4/local-file.mp4", base_url);
    let current_path = state.mp4_source_path.lock().await.clone();
    Ok(Html(render_mp4_page(
        &mp4_url,
        &state.root_dir,
        current_path.as_deref(),
    )))
}

async fn set_mp4_source(
    State(state): State<AppState>,
    Form(form): Form<Mp4PathForm>,
) -> Result<Redirect, AppError> {
    let canonical = normalize_mp4_source_path(&form.path)?;
    *state.mp4_source_path.lock().await = Some(canonical);
    Ok(Redirect::to("/mp4"))
}

async fn pick_mp4_source(State(state): State<AppState>) -> Result<Json<Mp4PickResponse>, AppError> {
    let Some(selected_path) = pick_mp4_file_with_system_dialog().await? else {
        return Ok(Json(Mp4PickResponse {
            selected: false,
            path: None,
            message: Some("已取消选择".to_string()),
        }));
    };

    let canonical = normalize_mp4_source_path(&selected_path)?;
    let display_path = canonical.to_string_lossy().to_string();
    *state.mp4_source_path.lock().await = Some(canonical);

    Ok(Json(Mp4PickResponse {
        selected: true,
        path: Some(display_path),
        message: None,
    }))
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

    let result = create_hls_job(&state, &video_path, playlist_name, source_name).await;

    let _ = tokio::fs::remove_dir_all(&upload_dir).await;
    result?;
    Ok(Redirect::to("/"))
}

async fn generate_from_local_file(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Redirect, AppError> {
    ensure_ffmpeg_available().await?;

    let upload_id = Uuid::new_v4().to_string();
    let upload_dir = state.temp_dir.join(&upload_id);
    tokio::fs::create_dir_all(&upload_dir)
        .await
        .map_err(|e| AppError::internal(format!("创建本地文件上传临时目录失败: {}", e)))?;

    let mut playlist_name: Option<String> = None;
    let mut video_path: Option<PathBuf> = None;
    let mut source_name: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("解析本地文件上传内容失败: {}", e)))?
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

        if field_name != "local_video" {
            continue;
        }

        let original_name = field.file_name().unwrap_or_default().to_string();
        if !is_supported_video_file(&original_name) {
            continue;
        }
        if video_path.is_some() {
            let _ = tokio::fs::remove_dir_all(&upload_dir).await;
            return Err(AppError::bad_request("请只选择一个本地视频文件"));
        }

        let safe_name = sanitize_filename(
            Path::new(&original_name)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("video.mp4"),
            "video.mp4",
        );
        let file_path = upload_dir.join(&safe_name);
        let mut file = tokio::fs::File::create(&file_path)
            .await
            .map_err(|e| AppError::internal(format!("创建本地文件上传失败: {}", e)))?;
        let mut field = field;

        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|e| AppError::bad_request(format!("读取本地文件上传失败: {}", e)))?
        {
            file.write_all(&chunk)
                .await
                .map_err(|e| AppError::internal(format!("写入本地文件上传失败: {}", e)))?;
        }

        source_name = Some(original_name);
        video_path = Some(file_path);
    }

    let video_path = video_path.ok_or_else(|| AppError::bad_request("请先选择一个本地视频文件"))?;
    let source_name = source_name.unwrap_or_else(|| {
        video_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("video.mp4")
            .to_string()
    });

    let result = create_hls_job(&state, &video_path, playlist_name, source_name).await;
    let _ = tokio::fs::remove_dir_all(&upload_dir).await;
    result?;
    Ok(Redirect::to("/"))
}

async fn generate_dash_from_upload(
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

    let result = create_dash_job(&state, &video_path, playlist_name, source_name).await;
    let _ = tokio::fs::remove_dir_all(&upload_dir).await;
    result?;
    Ok(Redirect::to("/dash"))
}

async fn generate_dash_from_local_file(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Redirect, AppError> {
    ensure_ffmpeg_available().await?;

    let upload_id = Uuid::new_v4().to_string();
    let upload_dir = state.temp_dir.join(&upload_id);
    tokio::fs::create_dir_all(&upload_dir)
        .await
        .map_err(|e| AppError::internal(format!("创建本地文件上传临时目录失败: {}", e)))?;

    let mut playlist_name: Option<String> = None;
    let mut video_path: Option<PathBuf> = None;
    let mut source_name: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("解析本地文件上传内容失败: {}", e)))?
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

        if field_name != "local_video" {
            continue;
        }

        let original_name = field.file_name().unwrap_or_default().to_string();
        if !is_supported_video_file(&original_name) {
            continue;
        }
        if video_path.is_some() {
            let _ = tokio::fs::remove_dir_all(&upload_dir).await;
            return Err(AppError::bad_request("请只选择一个本地视频文件"));
        }

        let safe_name = sanitize_filename(
            Path::new(&original_name)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("video.mp4"),
            "video.mp4",
        );
        let file_path = upload_dir.join(&safe_name);
        let mut file = tokio::fs::File::create(&file_path)
            .await
            .map_err(|e| AppError::internal(format!("创建本地文件上传失败: {}", e)))?;
        let mut field = field;

        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|e| AppError::bad_request(format!("读取本地文件上传失败: {}", e)))?
        {
            file.write_all(&chunk)
                .await
                .map_err(|e| AppError::internal(format!("写入本地文件上传失败: {}", e)))?;
        }

        source_name = Some(original_name);
        video_path = Some(file_path);
    }

    let video_path = video_path.ok_or_else(|| AppError::bad_request("请先选择一个本地视频文件"))?;
    let source_name = source_name.unwrap_or_else(|| {
        video_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("video.mp4")
            .to_string()
    });

    let result = create_dash_job(&state, &video_path, playlist_name, source_name).await;
    let _ = tokio::fs::remove_dir_all(&upload_dir).await;
    result?;
    Ok(Redirect::to("/dash"))
}

async fn serve_hls_file(
    State(state): State<AppState>,
    AxumPath((job_id, file)): AxumPath<(String, String)>,
) -> Result<Response, AppError> {
    let clean_file =
        sanitize_relative_hls_path(&file).ok_or_else(|| AppError::bad_request("非法文件路径"))?;
    let file_path = state.data_dir.join(&job_id).join(clean_file);

    let bytes = tokio::fs::read(&file_path).await.map_err(|_| AppError {
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

async fn serve_dash_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath((job_id, file)): AxumPath<(String, String)>,
) -> Result<Response, AppError> {
    let clean_file = sanitize_relative_hls_path(&file)
        .ok_or_else(|| AppError::bad_request("无效的 DASH 文件路径"))?;
    let job_dir = state.data_dir.join(sanitize_slug(&job_id, "job"));
    let file_path = job_dir.join(&clean_file);

    if clean_file == PathBuf::from("manifest.json") {
        let base_url = format!("{}/dash-test/{}/", request_base_url(&headers), job_id);
        let body = build_dash_json_manifest(&job_dir, &base_url).await?;
        return Ok((
            [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
            body,
        )
            .into_response());
    }

    if !file_path.is_file() {
        return Err(AppError::bad_request("DASH 文件不存在"));
    }

    let bytes = tokio::fs::read(&file_path)
        .await
        .map_err(|e| AppError::internal(format!("读取 DASH 文件失败: {}", e)))?;
    let content_type = content_type_for_path(&file_path);
    Ok(([(header::CONTENT_TYPE, content_type)], bytes).into_response())
}

async fn serve_mp4_test_file(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let file_path = state
        .mp4_source_path
        .lock()
        .await
        .clone()
        .ok_or_else(|| AppError::bad_request("请先在 /mp4 页面设置本机 MP4 文件路径"))?;
    let metadata = tokio::fs::metadata(&file_path)
        .await
        .map_err(|e| AppError::internal(format!("读取 MP4 文件信息失败: {}", e)))?;
    if !metadata.is_file() {
        return Err(AppError::bad_request("当前 MP4 路径不是文件"));
    }

    let total_len = metadata.len();
    if total_len == 0 {
        return Err(AppError::internal("MP4 文件为空"));
    }

    let range = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| parse_single_byte_range(value, total_len));
    let (status, start, end) = range.unwrap_or((StatusCode::OK, 0, total_len - 1));
    let content_length = end - start + 1;

    let mut response_headers = HeaderMap::new();
    response_headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp4"));
    response_headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if let Ok(value) = HeaderValue::from_str(&content_length.to_string()) {
        response_headers.insert(header::CONTENT_LENGTH, value);
    }
    if status == StatusCode::PARTIAL_CONTENT {
        if let Ok(value) = HeaderValue::from_str(&format!("bytes {}-{}/{}", start, end, total_len))
        {
            response_headers.insert(header::CONTENT_RANGE, value);
        }
    }

    let stream = async_stream::stream! {
        let mut file = match tokio::fs::File::open(&file_path).await {
            Ok(file) => file,
            Err(_) => return,
        };
        if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
            return;
        }

        let mut remaining = content_length;
        let mut buffer = vec![0u8; 64 * 1024];
        while remaining > 0 {
            let read_len = min(buffer.len() as u64, remaining) as usize;
            let bytes_read = match file.read(&mut buffer[..read_len]).await {
                Ok(0) => break,
                Ok(bytes_read) => bytes_read,
                Err(_) => break,
            };
            remaining = remaining.saturating_sub(bytes_read as u64);
            yield Result::<Bytes, Infallible>::Ok(Bytes::copy_from_slice(&buffer[..bytes_read]));
        }
    };

    Ok((status, response_headers, Body::from_stream(stream)).into_response())
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

    let plain_result = run_ffmpeg_hls_encode(
        input_path,
        &job_dir.join(HlsEncryptionMode::None.playlist_file_name()),
        &job_dir.join(HlsEncryptionMode::None.segment_file_pattern()),
    )
    .await;
    if let Err(error) = plain_result {
        let _ = tokio::fs::remove_dir_all(&job_dir).await;
        return Err(error);
    }

    let encrypted_result = generate_encrypted_playlists(
        &job_dir,
        &job_dir.join(HlsEncryptionMode::None.playlist_file_name()),
    )
    .await;
    if let Err(error) = encrypted_result {
        let _ = tokio::fs::remove_dir_all(&job_dir).await;
        return Err(error);
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

async fn create_dash_job(
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
            .unwrap_or("dash-sample")
            .to_string()
    });
    let playlist_name = sanitize_slug(&requested_name, "dash-sample");
    let job_dir = state.data_dir.join(&job_id);

    tokio::fs::create_dir_all(&job_dir)
        .await
        .map_err(|e| AppError::internal(format!("创建 DASH 输出目录失败: {}", e)))?;

    if let Err(error) = run_ffmpeg_dash_encode(input_path, &job_dir.join("manifest.mpd")).await {
        let _ = tokio::fs::remove_dir_all(&job_dir).await;
        return Err(error);
    }

    let meta = JobMeta {
        id: job_id.clone(),
        playlist_name,
        source_name,
        created_at: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    };
    let meta_json = serde_json::to_vec_pretty(&meta)
        .map_err(|e| AppError::internal(format!("序列化 DASH 任务信息失败: {}", e)))?;
    tokio::fs::write(job_dir.join("job.json"), meta_json)
        .await
        .map_err(|e| AppError::internal(format!("写入 DASH 任务信息失败: {}", e)))?;

    Ok(())
}

async fn run_ffmpeg_dash_encode(input_path: &Path, manifest_path: &Path) -> Result<(), AppError> {
    let mut command = Command::new("ffmpeg");
    command
        .arg("-y")
        .arg("-i")
        .arg(input_path)
        .args(["-map", "0:v:0"])
        .args(["-map", "0:a:0?"])
        .args(["-c:v", "libx264"])
        .args(["-c:a", "aac"])
        .args(["-f", "dash"])
        .args(["-seg_duration", "6"])
        .args(["-use_template", "1"])
        .args(["-use_timeline", "1"])
        .args(["-init_seg_name", "init-stream$RepresentationID$.m4s"])
        .args([
            "-media_seg_name",
            "chunk-stream$RepresentationID$-$Number%05d$.m4s",
        ])
        .arg(manifest_path);

    let output = command
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| AppError::internal(format!("启动 ffmpeg DASH 失败: {}", e)))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr
        .trim()
        .lines()
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" | ");
    Err(AppError::internal(format!(
        "ffmpeg 生成 DASH 失败，退出码: {}。错误详情: {}",
        output.status.code().unwrap_or(-1),
        if detail.is_empty() {
            "未返回额外错误信息"
        } else {
            &detail
        }
    )))
}

async fn build_dash_json_manifest(job_dir: &Path, base_url: &str) -> Result<String, AppError> {
    let mut video_segments = Vec::new();
    let mut audio_segments = Vec::new();
    let mut files = tokio::fs::read_dir(job_dir)
        .await
        .map_err(|e| AppError::internal(format!("读取 DASH 目录失败: {}", e)))?;

    while let Some(file) = files
        .next_entry()
        .await
        .map_err(|e| AppError::internal(format!("读取 DASH 文件失败: {}", e)))?
    {
        let Some(name) = file.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if name.starts_with("chunk-stream0-") && name.ends_with(".m4s") {
            video_segments.push(name);
        } else if name.starts_with("chunk-stream1-") && name.ends_with(".m4s") {
            audio_segments.push(name);
        }
    }

    video_segments.sort();
    audio_segments.sort();
    if video_segments.is_empty() {
        return Err(AppError::bad_request("DASH 视频分片不存在"));
    }

    let video_track = json!({
        "id": "0",
        "label": "DASH Video",
        "bandwidth": 2500000,
        "resolution": null,
        "codecs": "avc1",
        "init": "init-stream0.m4s",
        "segments": video_segments
            .into_iter()
            .map(|uri| json!({ "uri": uri, "duration": 6.0 }))
            .collect::<Vec<_>>()
    });
    let audio_tracks = if audio_segments.is_empty() || !job_dir.join("init-stream1.m4s").is_file() {
        Vec::new()
    } else {
        vec![json!({
            "id": "1",
            "label": "DASH Audio",
            "language": "und",
            "codecs": "mp4a.40.2",
            "init": "init-stream1.m4s",
            "segments": audio_segments
                .into_iter()
                .map(|uri| json!({ "uri": uri, "duration": 6.0 }))
                .collect::<Vec<_>>()
        })]
    };
    let default_selection = if audio_tracks.is_empty() {
        json!({ "video_id": "0" })
    } else {
        json!({ "video_id": "0", "audio_id": "1" })
    };

    let manifest = json!({
        "format": "m3u8quicker-dash-v1",
        "title": "dash-test",
        "base_url": base_url,
        "tracks": {
            "video": [video_track],
            "audio": audio_tracks
        },
        "default_selection": default_selection
    });

    serde_json::to_string_pretty(&manifest)
        .map_err(|e| AppError::internal(format!("生成 DASH JSON 失败: {}", e)))
}

async fn run_ffmpeg_hls_encode(
    input_path: &Path,
    playlist_path: &Path,
    segment_pattern: &Path,
) -> Result<(), AppError> {
    let mut command = Command::new("ffmpeg");
    command
        .arg("-y")
        .arg("-i")
        .arg(input_path)
        .args(["-c:v", "libx264"])
        .args(["-c:a", "aac"])
        .args(["-f", "hls"])
        .args(["-hls_time", "6"])
        .args(["-hls_playlist_type", "vod"]);

    let output = command
        .args(["-hls_segment_filename"])
        .arg(segment_pattern)
        .arg(playlist_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| AppError::internal(format!("启动 ffmpeg 失败: {}", e)))?;

    if output.status.success() {
        return Ok(());
    }

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
    Err(AppError::internal(format!(
        "ffmpeg 执行失败，退出码: {}。错误详情: {}。请确认输入视频可读，且本机已正确安装 ffmpeg。",
        output.status.code().unwrap_or(-1),
        detail
    )))
}

async fn generate_encrypted_playlists(
    job_dir: &Path,
    plain_playlist_path: &Path,
) -> Result<(), AppError> {
    let plain_playlist = tokio::fs::read_to_string(plain_playlist_path)
        .await
        .map_err(|e| AppError::internal(format!("读取明文播放列表失败: {}", e)))?;
    let segment_names = plain_playlist_segment_names(&plain_playlist);

    for mode in HlsEncryptionMode::all_encrypted() {
        let encryption = prepare_encryption_artifacts(job_dir, mode).await?;
        for (index, segment_name) in segment_names.iter().enumerate() {
            let plain_bytes = tokio::fs::read(job_dir.join(segment_name))
                .await
                .map_err(|e| AppError::internal(format!("读取明文切片失败: {}", e)))?;
            let encrypted_bytes =
                encrypt_segment(&plain_bytes, &encryption.key_bytes, &encryption.iv)?;
            tokio::fs::write(job_dir.join(mode.segment_name(index)), encrypted_bytes)
                .await
                .map_err(|e| AppError::internal(format!("写入加密切片失败: {}", e)))?;
        }

        let encrypted_playlist = build_encrypted_playlist(&plain_playlist, mode, &encryption);
        tokio::fs::write(job_dir.join(mode.playlist_file_name()), encrypted_playlist)
            .await
            .map_err(|e| AppError::internal(format!("写入加密播放列表失败: {}", e)))?;
    }

    Ok(())
}

async fn prepare_encryption_artifacts(
    job_dir: &Path,
    mode: HlsEncryptionMode,
) -> Result<EncryptionArtifacts, AppError> {
    let key_path = job_dir.join(mode.key_file_name());
    let mut key_bytes = vec![0u8; mode.key_len()];
    let mut iv = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut key_bytes);
    rand::rngs::OsRng.fill_bytes(&mut iv);

    tokio::fs::write(&key_path, &key_bytes)
        .await
        .map_err(|e| AppError::internal(format!("写入 {} key 失败: {}", mode.display_name(), e)))?;

    Ok(EncryptionArtifacts {
        key_bytes,
        iv,
        key_uri: mode.key_file_name().to_string(),
    })
}

fn plain_playlist_segment_names(playlist: &str) -> Vec<String> {
    playlist
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect()
}

fn build_encrypted_playlist(
    plain_playlist: &str,
    mode: HlsEncryptionMode,
    encryption: &EncryptionArtifacts,
) -> String {
    let key_line = format!(
        "#EXT-X-KEY:METHOD={},URI=\"{}\",IV=0x{}",
        mode.display_name(),
        encryption.key_uri,
        bytes_to_hex(&encryption.iv)
    );

    let mut result = Vec::new();
    let mut key_inserted = false;
    let mut segment_index = 0usize;

    for line in plain_playlist.lines() {
        let trimmed = line.trim();
        if !key_inserted && trimmed.starts_with("#EXTINF") {
            result.push(key_line.clone());
            key_inserted = true;
        }

        if !trimmed.is_empty() && !trimmed.starts_with('#') {
            result.push(mode.segment_name(segment_index));
            segment_index += 1;
            continue;
        }

        result.push(line.to_string());
    }

    result.join("\n") + "\n"
}

fn encrypt_segment(data: &[u8], key: &[u8], iv: &[u8; 16]) -> Result<Vec<u8>, AppError> {
    let block_size = 16usize;
    let mut buf = vec![0u8; data.len() + block_size];
    buf[..data.len()].copy_from_slice(data);

    match key.len() {
        16 => {
            let key: [u8; 16] = key
                .try_into()
                .map_err(|_| AppError::internal("无效的 AES-128 key 长度"))?;
            let encrypted = Aes128CbcEnc::new((&key).into(), iv.into())
                .encrypt_padded_mut::<Pkcs7>(&mut buf, data.len())
                .map_err(|e| AppError::internal(format!("AES-128 加密失败: {}", e)))?;
            Ok(encrypted.to_vec())
        }
        24 => {
            let key: [u8; 24] = key
                .try_into()
                .map_err(|_| AppError::internal("无效的 AES-192 key 长度"))?;
            let encrypted = Aes192CbcEnc::new((&key).into(), iv.into())
                .encrypt_padded_mut::<Pkcs7>(&mut buf, data.len())
                .map_err(|e| AppError::internal(format!("AES-192 加密失败: {}", e)))?;
            Ok(encrypted.to_vec())
        }
        32 => {
            let key: [u8; 32] = key
                .try_into()
                .map_err(|_| AppError::internal("无效的 AES-256 key 长度"))?;
            let encrypted = Aes256CbcEnc::new((&key).into(), iv.into())
                .encrypt_padded_mut::<Pkcs7>(&mut buf, data.len())
                .map_err(|e| AppError::internal(format!("AES-256 加密失败: {}", e)))?;
            Ok(encrypted.to_vec())
        }
        other => Err(AppError::internal(format!(
            "不支持的 AES key 长度: {}",
            other
        ))),
    }
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

fn normalize_mp4_source_path(input: &str) -> Result<PathBuf, AppError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(AppError::bad_request("请填写本机 MP4 文件路径"));
    }

    let file_path = PathBuf::from(trimmed);
    if !file_path.is_file() {
        return Err(AppError::bad_request("本机 MP4 文件不存在"));
    }
    if !matches!(
        file_path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref(),
        Some("mp4")
    ) {
        return Err(AppError::bad_request("请选择 .mp4 文件"));
    }

    Ok(file_path.canonicalize().unwrap_or(file_path))
}

async fn pick_mp4_file_with_system_dialog() -> Result<Option<String>, AppError> {
    pick_video_file_with_system_dialog(VideoFilter::Mp4Only).await
}

fn normalize_video_source_path(input: &str) -> Result<PathBuf, AppError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(AppError::bad_request("请填写本机视频文件路径"));
    }
    let file_path = PathBuf::from(trimmed);
    if !file_path.is_file() {
        return Err(AppError::bad_request("本机视频文件不存在"));
    }
    let name = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if !is_supported_video_file(name) {
        return Err(AppError::bad_request(
            "请选择常见视频文件 (mp4 / mov / mkv / webm / ...)",
        ));
    }
    Ok(file_path.canonicalize().unwrap_or(file_path))
}

#[derive(Debug, Clone, Copy)]
enum VideoFilter {
    Mp4Only,
    AnyVideo,
}

async fn pick_video_file_with_system_dialog(
    filter: VideoFilter,
) -> Result<Option<String>, AppError> {
    if cfg!(target_os = "windows") {
        let win_filter = match filter {
            VideoFilter::Mp4Only => "MP4 文件 (*.mp4)|*.mp4",
            VideoFilter::AnyVideo => {
                "视频文件 (*.mp4;*.mov;*.m4v;*.mkv;*.webm;*.avi;*.flv;*.mpeg;*.mpg;*.ts)|\
                 *.mp4;*.mov;*.m4v;*.mkv;*.webm;*.avi;*.flv;*.mpeg;*.mpg;*.ts"
            }
        };
        let script = format!(
            "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; \
             Add-Type -AssemblyName System.Windows.Forms; \
             $dialog = New-Object System.Windows.Forms.OpenFileDialog; \
             $dialog.Filter = '{}'; \
             $dialog.Multiselect = $false; \
             if ($dialog.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {{ \
                 Write-Output $dialog.FileName \
             }}",
            win_filter
        );
        let output = Command::new("powershell")
            .args(["-NoProfile", "-STA", "-Command"])
            .arg(script)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| AppError::internal(format!("打开文件选择框失败: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AppError::internal(format!(
                "文件选择框退出失败: {}",
                stderr.trim()
            )));
        }
        let selected = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Ok(if selected.is_empty() {
            None
        } else {
            Some(selected)
        });
    }

    if cfg!(target_os = "macos") {
        let type_list = match filter {
            VideoFilter::Mp4Only => "{\"mp4\", \"public.mpeg-4\"}",
            VideoFilter::AnyVideo => {
                "{\"mp4\", \"mov\", \"m4v\", \"mkv\", \"webm\", \"avi\", \"flv\", \
                 \"mpeg\", \"mpg\", \"ts\", \"public.movie\", \"public.video\"}"
            }
        };
        let prompt = match filter {
            VideoFilter::Mp4Only => "选择本机 MP4 文件",
            VideoFilter::AnyVideo => "选择本机视频文件",
        };
        let script = format!(
            "try\n  set f to choose file with prompt \"{prompt}\" of type {types}\n  POSIX path of f\non error number -128\n  return \"\"\nend try",
            prompt = prompt,
            types = type_list,
        );
        let output = Command::new("osascript")
            .arg("-e")
            .arg(script)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| AppError::internal(format!("打开文件选择框失败: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AppError::internal(format!(
                "文件选择框退出失败: {}",
                stderr.trim()
            )));
        }
        let selected = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Ok(if selected.is_empty() {
            None
        } else {
            Some(selected)
        });
    }

    // Linux / other: try zenity then kdialog.
    let zenity_filter = match filter {
        VideoFilter::Mp4Only => "*.mp4",
        VideoFilter::AnyVideo => "*.mp4 *.mov *.m4v *.mkv *.webm *.avi *.flv *.mpeg *.mpg *.ts",
    };
    if let Ok(output) = Command::new("zenity")
        .args(["--file-selection", "--title=选择本机视频"])
        .arg(format!("--file-filter=视频 | {}", zenity_filter))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
    {
        if output.status.success() {
            let selected = String::from_utf8_lossy(&output.stdout).trim().to_string();
            return Ok(if selected.is_empty() {
                None
            } else {
                Some(selected)
            });
        } else if output.status.code() == Some(1) {
            // User cancelled.
            return Ok(None);
        }
    }
    if let Ok(output) = Command::new("kdialog")
        .args(["--getopenfilename", ".", zenity_filter])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
    {
        if output.status.success() {
            let selected = String::from_utf8_lossy(&output.stdout).trim().to_string();
            return Ok(if selected.is_empty() {
                None
            } else {
                Some(selected)
            });
        }
    }
    Err(AppError::bad_request(
        "当前系统未检测到可用的文件选择框（请安装 zenity 或 kdialog，或手动粘贴文件路径）",
    ))
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
            let is_plain_segment = file_path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with("seg_") && name.ends_with(".ts"))
                .unwrap_or(false);
            if is_plain_segment {
                segment_count += 1;
            }
        }

        jobs.push(JobSummary {
            playlist_path: format!("/hls/{}/index.m3u8", meta.id),
            dash_manifest_path: None,
            dash_json_path: None,
            aes128_playlist_path: path
                .join(HlsEncryptionMode::Aes128.playlist_file_name())
                .is_file()
                .then(|| format!("/hls/{}/index-aes128.m3u8", meta.id)),
            aes192_playlist_path: path
                .join(HlsEncryptionMode::Aes192.playlist_file_name())
                .is_file()
                .then(|| format!("/hls/{}/index-aes192.m3u8", meta.id)),
            aes256_playlist_path: path
                .join(HlsEncryptionMode::Aes256.playlist_file_name())
                .is_file()
                .then(|| format!("/hls/{}/index-aes256.m3u8", meta.id)),
            meta,
            segment_count,
        });
    }

    jobs.sort_by(|a, b| b.meta.created_at.cmp(&a.meta.created_at));
    Ok(jobs)
}

async fn load_dash_jobs(state: &AppState) -> Result<Vec<JobSummary>, AppError> {
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
        let manifest_path = path.join("manifest.mpd");
        if !meta_path.is_file() || !manifest_path.is_file() {
            continue;
        }

        let meta_bytes = tokio::fs::read(&meta_path)
            .await
            .map_err(|e| AppError::internal(format!("读取 DASH 任务信息失败: {}", e)))?;
        let meta: JobMeta = serde_json::from_slice(&meta_bytes)
            .map_err(|e| AppError::internal(format!("解析 DASH 任务信息失败: {}", e)))?;

        let mut segment_count = 0usize;
        let mut files = tokio::fs::read_dir(&path)
            .await
            .map_err(|e| AppError::internal(format!("读取 DASH 切片目录失败: {}", e)))?;
        while let Some(file) = files
            .next_entry()
            .await
            .map_err(|e| AppError::internal(format!("读取 DASH 切片文件失败: {}", e)))?
        {
            let file_path = file.path();
            let is_dash_segment = file_path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.ends_with(".m4s") && name.starts_with("chunk-"))
                .unwrap_or(false);
            if is_dash_segment {
                segment_count += 1;
            }
        }

        jobs.push(JobSummary {
            playlist_path: String::new(),
            dash_manifest_path: Some(format!("/dash-test/{}/manifest.mpd", meta.id)),
            dash_json_path: Some(format!("/dash-test/{}/manifest.json", meta.id)),
            aes128_playlist_path: None,
            aes192_playlist_path: None,
            aes256_playlist_path: None,
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
              <p>支持直接选择普通流、AES-128、AES-192、AES-256 测试流，也支持手动粘贴任意 M3U8 地址。</p>
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
        "<div class=\"empty\">还没有生成过测试流。上传一个视频或填写本地路径试试。</div>"
            .to_string()
    } else {
        jobs.iter()
            .map(|job| {
                let variants = collect_playlist_variants(job);
                let variant_html = variants
                    .iter()
                    .map(|variant| {
                        let playlist_url = format!("{}{}", base_url, variant.path);
                        format!(
                            "<p>{} M3U8：<code>{}</code></p>\
                             <div class=\"actions\">\
                               <button type=\"button\" class=\"button-link js-play-job\" data-playlist-url=\"{}\">播放 {}</button>\
                               <a href=\"{}\" target=\"_blank\" rel=\"noreferrer\">打开 {}</a>\
                               <a href=\"{}\" download>下载 {}</a>\
                             </div>",
                            variant.label,
                            escape_html(&playlist_url),
                            escape_html(&playlist_url),
                            variant.label,
                            variant.path,
                            variant.label,
                            variant.path,
                            variant.label,
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("");
                format!(
                    "<article class=\"job-card\">\
                       <div class=\"job-top\">\
                         <div><h3>{}</h3><p>来源文件：{}</p></div>\
                         <span class=\"job-time\">{}</span>\
                       </div>\
                       <p>任务 ID：<code>{}</code></p>\
                       <p>切片数量：<strong>{}</strong></p>\
                       <p class=\"hint\">AES-192 / AES-256 主要用于下载联调，浏览器在线播放未必支持。</p>\
                       {}\
                     </article>",
                    escape_html(&job.meta.playlist_name),
                    escape_html(&job.meta.source_name),
                    escape_html(&job.meta.created_at),
                    escape_html(&job.meta.id),
                    job.segment_count,
                    variant_html,
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
               <p><a href=\"/dash\">打开 DASH 独立测试页面</a> · <a href=\"/mp4\">打开 Direct MP4 播放测试页</a></p>\
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
               <form class=\"panel\" action=\"/generate/local-file\" method=\"post\" enctype=\"multipart/form-data\">\
                 <h2>选择本地视频并生成</h2>\
                 <div class=\"field\">\
                   <label for=\"local_video\">本地视频文件</label>\
                   <input id=\"local_video\" type=\"file\" name=\"local_video\" accept=\"video/*,.ts,.mkv,.flv,.avi,.mpeg,.mpg\" required>\
                 </div>\
                 <div class=\"field\">\
                   <label for=\"path_name\">播放列表名称（可选）</label>\
                   <input id=\"path_name\" type=\"text\" name=\"playlist_name\" placeholder=\"例如 local-sample\">\
                 </div>\
                 <p class=\"hint\">直接选择一个本地视频文件生成，不需要手写路径。</p>\
                 <button type=\"submit\">按本地视频生成</button>\
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

fn render_dash_page(
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
    let jobs_html = if jobs.is_empty() {
        "<div class=\"empty\">还没有生成过 DASH 测试流。上传一个视频试试。</div>".to_string()
    } else {
        jobs.iter()
            .map(|job| {
                let mpd_path = job.dash_manifest_path.as_deref().unwrap_or_default();
                let json_path = job.dash_json_path.as_deref().unwrap_or_default();
                let mpd_url = format!("{}{}", base_url, mpd_path);
                let json_url = format!("{}{}", base_url, json_path);
                format!(
                    "<article class=\"job-card\">\
                       <div class=\"job-top\">\
                         <div><h3>{}</h3><p>来源文件：{}</p></div>\
                         <span class=\"job-time\">{}</span>\
                       </div>\
                       <p>任务 ID：<code>{}</code></p>\
                       <p>DASH 分片数量：<strong>{}</strong></p>\
                       <p>DASH MPD：<code>{}</code></p>\
                       <p>DASH JSON：<code>{}</code></p>\
                       <div class=\"actions\">\
                         <a href=\"{}\" target=\"_blank\" rel=\"noreferrer\">打开 MPD</a>\
                         <a href=\"{}\" target=\"_blank\" rel=\"noreferrer\">打开 JSON</a>\
                         <a href=\"{}\" download>下载 MPD</a>\
                       </div>\
                     </article>",
                    escape_html(&job.meta.playlist_name),
                    escape_html(&job.meta.source_name),
                    escape_html(&job.meta.created_at),
                    escape_html(&job.meta.id),
                    job.segment_count,
                    escape_html(&mpd_url),
                    escape_html(&json_url),
                    mpd_path,
                    json_path,
                    mpd_path,
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
           <title>Test DASH Server</title>\
           <style>\
             :root{{color-scheme:light;background:#f5f7fb;color:#101828}}\
             *{{box-sizing:border-box}}\
             body{{margin:0;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;background:#f8fafc;color:#0f172a}}\
             main{{max-width:1100px;margin:0 auto;padding:32px 20px 48px}}\
             .hero,.panel,.job-card{{background:#fff;border:1px solid #dbe5f0;border-radius:20px;padding:22px;box-shadow:0 10px 30px rgba(15,23,42,.05)}}\
             .hero{{border-radius:24px;padding:28px}}\
             h1{{margin:0;font-size:32px}}h2{{margin:0 0 16px;font-size:20px}}h3{{margin:0 0 8px;font-size:18px}}\
             p{{margin:8px 0;color:#475467}}\
             code{{font-family:'SFMono-Regular',Consolas,monospace;background:#f2f4f7;padding:2px 6px;border-radius:6px;word-break:break-all}}\
             .hero-top,.job-top{{display:flex;justify-content:space-between;align-items:start;gap:16px;flex-wrap:wrap}}\
             .badge{{display:inline-flex;align-items:center;padding:6px 12px;border-radius:999px;font-size:14px;font-weight:600}}\
             .badge-ok{{background:#dcfce7;color:#166534}}.badge-warn{{background:#fef3c7;color:#92400e}}\
             .grid{{display:grid;grid-template-columns:repeat(auto-fit,minmax(320px,1fr));gap:20px;margin-top:24px}}\
             label{{display:block;font-weight:600;margin-bottom:8px}}\
             input[type='text'],input[type='file']{{width:100%;padding:12px 14px;border:1px solid #cbd5e1;border-radius:12px;font:inherit;background:#fff}}\
             .field{{display:grid;gap:8px;margin-bottom:16px}}\
             button{{appearance:none;border:none;border-radius:12px;background:#2563eb;color:#fff;padding:12px 16px;font:inherit;font-weight:700;cursor:pointer}}\
             .hint{{font-size:14px;color:#667085}}\
             .section-head{{margin:28px 0 16px}}.jobs{{display:grid;gap:16px}}\
             .job-time{{font-size:13px;color:#667085}}\
             .actions{{display:flex;gap:12px;flex-wrap:wrap;margin-top:14px}}\
             a,.actions a{{color:#2563eb;font-weight:600;text-decoration:none}}\
             .empty{{background:#fff;border:1px dashed #cbd5e1;border-radius:18px;padding:28px;color:#475467}}\
           </style>\
         </head>\
         <body>\
           <main>\
             <section class=\"hero\">\
               <div class=\"hero-top\">\
                 <div>\
                   <h1>Test DASH Server</h1>\
                   <p>这个页面只生成 DASH 测试流，不生成 M3U8。</p>\
                 </div>\
                 {}\
               </div>\
               <p>服务根目录：<code>{}</code></p>\
               <p><a href=\"/\">返回 HLS 测试页面</a></p>\
             </section>\
             <section class=\"grid\">\
               <form class=\"panel\" action=\"/generate/dash/upload\" method=\"post\" enctype=\"multipart/form-data\">\
                 <h2>上传视频并生成 DASH</h2>\
                 <div class=\"field\"><label for=\"dash_video\">视频文件</label><input id=\"dash_video\" type=\"file\" name=\"video\" accept=\"video/*\" required></div>\
                 <div class=\"field\"><label for=\"dash_upload_name\">名称（可选）</label><input id=\"dash_upload_name\" type=\"text\" name=\"playlist_name\" placeholder=\"例如 dash-demo\"></div>\
                 <p class=\"hint\">生成后会得到独立的 <code>manifest.mpd</code> 和 <code>manifest.json</code>。</p>\
                 <button type=\"submit\">生成 DASH</button>\
               </form>\
               <form class=\"panel\" action=\"/generate/dash/local-file\" method=\"post\" enctype=\"multipart/form-data\">\
                 <h2>选择本地视频并生成 DASH</h2>\
                 <div class=\"field\"><label for=\"dash_local_video\">本地视频文件</label><input id=\"dash_local_video\" type=\"file\" name=\"local_video\" accept=\"video/*,.ts,.mkv,.flv,.avi,.mpeg,.mpg\" required></div>\
                 <div class=\"field\"><label for=\"dash_path_name\">名称（可选）</label><input id=\"dash_path_name\" type=\"text\" name=\"playlist_name\" placeholder=\"例如 dash-local-sample\"></div>\
                 <p class=\"hint\">直接选择一个本地视频文件，不需要手写路径。</p>\
                 <button type=\"submit\">按本地视频生成 DASH</button>\
               </form>\
             </section>\
             <section>\
               <div class=\"section-head\"><h2>已生成的 DASH 测试流</h2><p>把 MPD 地址或 JSON 内容喂给主应用的 DASH 下载模式。</p></div>\
               <div class=\"jobs\">{}</div>\
             </section>\
           </main>\
         </body>\
         </html>",
        status_badge,
        escape_html(&root_dir.to_string_lossy()),
        jobs_html,
    )
}

fn render_mp4_page(mp4_url: &str, root_dir: &Path, current_path: Option<&Path>) -> String {
    let current_path_value = current_path
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default();
    let current_path_html = current_path
        .map(|path| {
            format!(
                "<p>当前本地文件：<code>{}</code></p>",
                escape_html(&path.to_string_lossy())
            )
        })
        .unwrap_or_else(|| "<p>当前还没有设置本地 MP4 文件。</p>".to_string());
    format!(
        "<!doctype html>\
         <html lang=\"zh-CN\">\
         <head>\
           <meta charset=\"utf-8\">\
           <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
           <title>Direct MP4 Playback Test</title>\
           <style>\
             :root{{color-scheme:light;background:#f5f7fb;color:#101828}}\
             *{{box-sizing:border-box}}\
             body{{margin:0;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;background:#f8fafc;color:#0f172a}}\
             main{{max-width:1000px;margin:0 auto;padding:32px 20px 48px}}\
             .hero,.panel{{background:#fff;border:1px solid #dbe5f0;border-radius:20px;padding:22px;box-shadow:0 10px 30px rgba(15,23,42,.05)}}\
             .hero{{border-radius:24px;padding:28px;margin-bottom:20px}}\
             h1{{margin:0;font-size:32px}}h2{{margin:0 0 12px;font-size:20px}}\
             p{{margin:8px 0;color:#475467}}\
             code{{font-family:'SFMono-Regular',Consolas,monospace;background:#f2f4f7;padding:2px 6px;border-radius:6px;word-break:break-all}}\
             label{{display:block;font-weight:600;margin-bottom:8px}}\
             input[type='text']{{width:100%;padding:12px 14px;border:1px solid #cbd5e1;border-radius:12px;font:inherit;background:#fff}}\
             button{{appearance:none;border:none;border-radius:12px;background:#2563eb;color:#fff;padding:12px 16px;font:inherit;font-weight:700;cursor:pointer}}\
             button:hover{{background:#1d4ed8}}\
             .button-secondary{{background:#e2e8f0;color:#0f172a}}\
             .button-secondary:hover{{background:#cbd5e1}}\
             a{{color:#2563eb;font-weight:600;text-decoration:none}}\
             video{{width:100%;border-radius:18px;background:#020617;aspect-ratio:16/9}}\
             .actions{{display:flex;gap:12px;flex-wrap:wrap;margin-top:14px}}\
             .field{{display:grid;gap:8px;margin-bottom:16px}}\
             .form-actions{{display:flex;gap:12px;flex-wrap:wrap}}\
             .pick-status{{min-height:22px;font-size:14px;color:#475467}}\
           </style>\
         </head>\
         <body>\
           <main>\
             <section class=\"hero\">\
               <h1>Direct MP4 Playback Test</h1>\
               <p>把本机 MP4 文件通过这个测试服务器端口暴露成 HTTP 地址，不上传、不转码。</p>\
               <p>MP4 地址：<code>{}</code></p>\
               <p>服务根目录：<code>{}</code></p>\
               {}\
               <p><a href=\"/\">返回 HLS 测试页面</a></p>\
             </section>\
             <form class=\"panel\" action=\"/mp4\" method=\"post\">\
               <h2>选择本机 MP4</h2>\
               <div class=\"field\">\
                 <label for=\"mp4_path\">本机 MP4 绝对路径</label>\
                 <input id=\"mp4_path\" type=\"text\" name=\"path\" value=\"{}\" placeholder=\"例如 D:\\Videos\\sample.mp4\" required>\
               </div>\
               <div class=\"form-actions\">\
                 <button type=\"button\" id=\"mp4_pick\" class=\"button-secondary\">浏览选择</button>\
                 <button type=\"submit\">使用这个 MP4</button>\
               </div>\
               <p id=\"mp4_pick_status\" class=\"pick-status\"></p>\
             </form>\
             <section class=\"panel\">\
               <h2>本地 MP4 播放</h2>\
               <video src=\"{}\" controls autoplay muted playsinline preload=\"auto\"></video>\
               <div class=\"actions\">\
                 <a href=\"{}\" target=\"_blank\" rel=\"noreferrer\">打开 MP4 地址</a>\
                 <a href=\"{}\" download>下载 MP4</a>\
               </div>\
             </section>\
             <script>\
               (() => {{\
                 const pickButton = document.getElementById('mp4_pick');\
                 const pathInput = document.getElementById('mp4_path');\
                 const statusText = document.getElementById('mp4_pick_status');\
                 pickButton.addEventListener('click', async () => {{\
                   pickButton.disabled = true;\
                   statusText.textContent = '正在打开文件选择框...';\
                   try {{\
                     const response = await fetch('/mp4/pick', {{ method: 'POST' }});\
                     const data = await response.json();\
                     if (!response.ok) {{\
                       throw new Error(data.message || '选择失败');\
                     }}\
                     if (data.selected && data.path) {{\
                       pathInput.value = data.path;\
                       statusText.textContent = '已选择 MP4，正在刷新播放。';\
                       window.location.reload();\
                     }} else {{\
                       statusText.textContent = data.message || '已取消选择。';\
                     }}\
                   }} catch (error) {{\
                     statusText.textContent = '选择失败：' + (error.message || String(error));\
                   }} finally {{\
                     pickButton.disabled = false;\
                   }}\
                 }});\
               }})();\
             </script>\
           </main>\
         </body>\
         </html>",
        escape_html(mp4_url),
        escape_html(&root_dir.to_string_lossy()),
        current_path_html,
        escape_html(&current_path_value),
        escape_html(mp4_url),
        escape_html(mp4_url),
        escape_html(mp4_url),
    )
}

fn collect_playlist_variants(job: &JobSummary) -> Vec<PlaylistVariant> {
    let mut variants = vec![PlaylistVariant {
        label: HlsEncryptionMode::None.display_name(),
        path: job.playlist_path.clone(),
    }];

    if let Some(path) = &job.aes128_playlist_path {
        variants.push(PlaylistVariant {
            label: HlsEncryptionMode::Aes128.display_name(),
            path: path.clone(),
        });
    }
    if let Some(path) = &job.aes192_playlist_path {
        variants.push(PlaylistVariant {
            label: HlsEncryptionMode::Aes192.display_name(),
            path: path.clone(),
        });
    }
    if let Some(path) = &job.aes256_playlist_path {
        variants.push(PlaylistVariant {
            label: HlsEncryptionMode::Aes256.display_name(),
            path: path.clone(),
        });
    }

    variants
}

fn parse_single_byte_range(value: &str, total_len: u64) -> Option<(StatusCode, u64, u64)> {
    let raw_range = value.trim().strip_prefix("bytes=")?;
    let (start, end) = raw_range.split_once('-')?;
    if start.contains(',') || end.contains(',') {
        return None;
    }

    if start.is_empty() {
        let suffix_len = end.parse::<u64>().ok()?.min(total_len);
        if suffix_len == 0 {
            return None;
        }
        return Some((
            StatusCode::PARTIAL_CONTENT,
            total_len - suffix_len,
            total_len - 1,
        ));
    }

    let start = start.parse::<u64>().ok()?;
    if start >= total_len {
        return None;
    }
    let end = if end.is_empty() {
        total_len - 1
    } else {
        end.parse::<u64>().ok()?.min(total_len - 1)
    };
    if end < start {
        return None;
    }

    Some((StatusCode::PARTIAL_CONTENT, start, end))
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
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
    {
        "m3u8" => "application/vnd.apple.mpegurl",
        "mpd" => "application/dash+xml",
        "m4s" | "mp4" => "video/iso.segment",
        "ts" => "video/mp2t",
        "json" => "application/json; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn is_supported_video_file(name: &str) -> bool {
    matches!(
        Path::new(name)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref(),
        Some("mp4")
            | Some("mov")
            | Some("m4v")
            | Some("mkv")
            | Some("webm")
            | Some("avi")
            | Some("flv")
            | Some("mpeg")
            | Some("mpg")
            | Some("ts")
    )
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
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
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

fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{:02x}", byte))
        .collect::<String>()
}

// ============================================================================
// HLS Live simulation
// ============================================================================

async fn live_index(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Html<String>, AppError> {
    ensure_live_jobs_loaded(&state).await?;
    let base_url = request_base_url(&headers);
    let current_path = state.live_source_path.lock().await.clone();
    let jobs: Vec<(String, String, String, String, String, String)> = {
        let map = state.live_jobs.lock().await;
        let mut list: Vec<_> = map
            .values()
            .map(|job| {
                (
                    job.id.clone(),
                    job.source_name.clone(),
                    job.created_at.clone(),
                    format!("{}/live-test/{}/ts/index.m3u8", base_url, job.id),
                    format!("{}/live-test/{}/fmp4/index.m3u8", base_url, job.id),
                    format!("{}", job.ts_segments.len().max(job.fmp4_segments.len())),
                )
            })
            .collect();
        list.sort_by(|a, b| b.2.cmp(&a.2));
        list
    };
    Ok(Html(render_live_page(
        &base_url,
        &state.root_dir,
        current_path.as_deref(),
        &jobs,
    )))
}

async fn set_live_source(
    State(state): State<AppState>,
    Form(form): Form<Mp4PathForm>,
) -> Result<Redirect, AppError> {
    let canonical = normalize_video_source_path(&form.path)?;
    *state.live_source_path.lock().await = Some(canonical);
    Ok(Redirect::to("/live"))
}

async fn pick_live_source(State(state): State<AppState>) -> Json<Mp4PickResponse> {
    let selected_path = match pick_video_file_with_system_dialog(VideoFilter::AnyVideo).await {
        Ok(Some(path)) => path,
        Ok(None) => {
            return Json(Mp4PickResponse {
                selected: false,
                path: None,
                message: Some("已取消选择".to_string()),
            });
        }
        Err(err) => {
            return Json(Mp4PickResponse {
                selected: false,
                path: None,
                message: Some(err.message),
            });
        }
    };
    let canonical = match normalize_video_source_path(&selected_path) {
        Ok(p) => p,
        Err(err) => {
            return Json(Mp4PickResponse {
                selected: false,
                path: None,
                message: Some(err.message),
            });
        }
    };
    let display_path = canonical.to_string_lossy().to_string();
    *state.live_source_path.lock().await = Some(canonical);
    Json(Mp4PickResponse {
        selected: true,
        path: Some(display_path),
        message: None,
    })
}

async fn generate_live_from_local_file(
    State(state): State<AppState>,
) -> Result<Redirect, AppError> {
    ensure_ffmpeg_available().await?;
    let source = state
        .live_source_path
        .lock()
        .await
        .clone()
        .ok_or_else(|| AppError::bad_request("请先选择本地视频文件"))?;
    if !source.is_file() {
        return Err(AppError::bad_request("当前选择的不是有效的视频文件"));
    }

    let job_id = Uuid::new_v4().to_string();
    let job_dir = state.data_dir.join(format!("live_{}", job_id));
    let ts_dir = job_dir.join("ts");
    let fmp4_dir = job_dir.join("fmp4");
    tokio::fs::create_dir_all(&ts_dir)
        .await
        .map_err(|e| AppError::internal(format!("创建 ts 目录失败: {}", e)))?;
    tokio::fs::create_dir_all(&fmp4_dir)
        .await
        .map_err(|e| AppError::internal(format!("创建 fmp4 目录失败: {}", e)))?;

    // Generate TS playlist + segments.
    let ts_playlist = ts_dir.join("source.m3u8");
    let ts_pattern = ts_dir.join("seg_%04d.ts");
    run_ffmpeg_hls_encode(&source, &ts_playlist, &ts_pattern).await?;

    // Generate fMP4 playlist + segments (init.mp4 + seg_%04d.m4s).
    let fmp4_playlist = fmp4_dir.join("source.m3u8");
    let fmp4_pattern = fmp4_dir.join("seg_%04d.m4s");
    let fmp4_init = fmp4_dir.join("init.mp4");
    run_ffmpeg_hls_fmp4_encode(&source, &fmp4_playlist, &fmp4_pattern, &fmp4_init).await?;

    let ts_segments = parse_segment_list(&ts_playlist).await?;
    let fmp4_segments = parse_segment_list(&fmp4_playlist).await?;

    if ts_segments.is_empty() {
        return Err(AppError::internal("ts 切片为空，无法启动直播模拟"));
    }
    let target_duration = ts_segments
        .iter()
        .map(|seg| seg.duration.ceil() as u64)
        .max()
        .unwrap_or(6)
        .max(1);

    let source_name = source
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("video.mp4")
        .to_string();
    let created_at = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let job = LiveJob {
        id: job_id.clone(),
        source_name: source_name.clone(),
        target_duration,
        ts_segments,
        fmp4_segments,
        fmp4_init_name: "init.mp4".to_string(),
        started_at: Instant::now(),
        created_at: created_at.clone(),
    };
    write_live_job_meta(
        &job_dir,
        &LiveJobMeta {
            id: job_id.clone(),
            source_name,
            created_at,
            target_duration: Some(target_duration),
            fmp4_init_name: Some("init.mp4".to_string()),
        },
    )
    .await?;

    {
        let mut map = state.live_jobs.lock().await;
        map.insert(job_id.clone(), Arc::new(job));
    }

    Ok(Redirect::to("/live"))
}

async fn serve_live_file(
    State(state): State<AppState>,
    AxumPath((job_id, flavor, file)): AxumPath<(String, String, String)>,
) -> Result<Response, AppError> {
    ensure_live_jobs_loaded(&state).await?;
    let job = {
        let map = state.live_jobs.lock().await;
        map.get(&job_id).cloned()
    }
    .ok_or_else(|| AppError {
        status: StatusCode::NOT_FOUND,
        message: "live 任务不存在".to_string(),
    })?;

    let flavor = flavor.as_str();
    if !matches!(flavor, "ts" | "fmp4") {
        return Err(AppError::bad_request("flavor 必须为 ts 或 fmp4"));
    }

    // Dynamic playlist endpoint.
    if file == "index.m3u8" {
        let body = build_live_playlist(&job, flavor);
        return Ok((
            [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
            body,
        )
            .into_response());
    }

    // Static file (segment / init).
    let clean =
        sanitize_relative_hls_path(&file).ok_or_else(|| AppError::bad_request("非法文件路径"))?;
    let file_path = state
        .data_dir
        .join(format!("live_{}", job_id))
        .join(flavor)
        .join(clean);
    let bytes = tokio::fs::read(&file_path).await.map_err(|_| AppError {
        status: StatusCode::NOT_FOUND,
        message: "文件不存在".to_string(),
    })?;
    let content_type = content_type_for_path(&file_path);
    Ok(([(header::CONTENT_TYPE, content_type)], bytes).into_response())
}

async fn ensure_live_jobs_loaded(state: &AppState) -> Result<(), AppError> {
    let mut entries = tokio::fs::read_dir(&state.data_dir)
        .await
        .map_err(|e| AppError::internal(format!("读取直播任务目录失败: {}", e)))?;

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| AppError::internal(format!("读取直播任务失败: {}", e)))?
    {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(dir_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(job_id) = dir_name.strip_prefix("live_") else {
            continue;
        };

        {
            let map = state.live_jobs.lock().await;
            if map.contains_key(job_id) {
                continue;
            }
        }

        if let Some(job) = load_live_job_from_dir(&path, job_id).await? {
            let mut map = state.live_jobs.lock().await;
            map.entry(job.id.clone()).or_insert_with(|| Arc::new(job));
        }
    }

    Ok(())
}

async fn load_live_job_from_dir(
    job_dir: &Path,
    fallback_job_id: &str,
) -> Result<Option<LiveJob>, AppError> {
    let ts_playlist = job_dir.join("ts").join("source.m3u8");
    if !ts_playlist.is_file() {
        return Ok(None);
    }

    let ts_segments = parse_segment_list(&ts_playlist).await?;
    if ts_segments.is_empty() {
        return Ok(None);
    }

    let fmp4_playlist = job_dir.join("fmp4").join("source.m3u8");
    let fmp4_segments = if fmp4_playlist.is_file() {
        parse_segment_list(&fmp4_playlist).await?
    } else {
        Vec::new()
    };

    let meta = read_live_job_meta(job_dir).await?;
    let target_duration = meta
        .as_ref()
        .and_then(|meta| meta.target_duration)
        .unwrap_or_else(|| {
            ts_segments
                .iter()
                .map(|seg| seg.duration.ceil() as u64)
                .max()
                .unwrap_or(6)
                .max(1)
        });
    let created_at = match meta.as_ref().map(|meta| meta.created_at.trim()) {
        Some(value) if !value.is_empty() => value.to_string(),
        _ => live_job_dir_time(job_dir).await,
    };
    let source_name = meta
        .as_ref()
        .map(|meta| meta.source_name.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback_job_id)
        .to_string();
    let fmp4_init_name = meta
        .as_ref()
        .and_then(|meta| meta.fmp4_init_name.as_deref())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("init.mp4")
        .to_string();

    Ok(Some(LiveJob {
        id: fallback_job_id.to_string(),
        source_name,
        target_duration,
        ts_segments,
        fmp4_segments,
        fmp4_init_name,
        started_at: Instant::now(),
        created_at,
    }))
}

async fn read_live_job_meta(job_dir: &Path) -> Result<Option<LiveJobMeta>, AppError> {
    let meta_path = job_dir.join("live-job.json");
    if !meta_path.is_file() {
        return Ok(None);
    }
    let meta_bytes = tokio::fs::read(&meta_path)
        .await
        .map_err(|e| AppError::internal(format!("读取直播任务信息失败: {}", e)))?;
    let meta: LiveJobMeta = serde_json::from_slice(&meta_bytes)
        .map_err(|e| AppError::internal(format!("解析直播任务信息失败: {}", e)))?;
    Ok(Some(meta))
}

async fn write_live_job_meta(job_dir: &Path, meta: &LiveJobMeta) -> Result<(), AppError> {
    let meta_json = serde_json::to_vec_pretty(meta)
        .map_err(|e| AppError::internal(format!("序列化直播任务信息失败: {}", e)))?;
    tokio::fs::write(job_dir.join("live-job.json"), meta_json)
        .await
        .map_err(|e| AppError::internal(format!("写入直播任务信息失败: {}", e)))?;
    Ok(())
}

async fn live_job_dir_time(job_dir: &Path) -> String {
    let modified = match tokio::fs::metadata(job_dir).await {
        Ok(metadata) => metadata.modified().unwrap_or_else(|_| SystemTime::now()),
        Err(_) => SystemTime::now(),
    };
    let datetime: chrono::DateTime<Local> = modified.into();
    datetime.format("%Y-%m-%d %H:%M:%S").to_string()
}

fn build_live_playlist(job: &LiveJob, flavor: &str) -> String {
    let (segments, is_fmp4) = if flavor == "fmp4" && !job.fmp4_segments.is_empty() {
        (&job.fmp4_segments, true)
    } else {
        (&job.ts_segments, false)
    };
    let total = segments.len();
    let elapsed = job.started_at.elapsed().as_secs_f64();
    let avg = (job.target_duration as f64).max(1.0);
    let current_seq = (elapsed / avg).floor() as i64;
    let window: usize = 6;
    let start_seq = current_seq.saturating_sub((window as i64) - 1).max(0) as u64;
    let last_seq = (start_seq + window as u64).saturating_sub(1);

    let mut out = String::new();
    out.push_str("#EXTM3U\n");
    out.push_str("#EXT-X-VERSION:6\n");
    out.push_str(&format!("#EXT-X-TARGETDURATION:{}\n", job.target_duration));
    out.push_str(&format!("#EXT-X-MEDIA-SEQUENCE:{}\n", start_seq));
    if is_fmp4 {
        out.push_str(&format!("#EXT-X-MAP:URI=\"{}\"\n", job.fmp4_init_name));
    }
    for seq in start_seq..=last_seq {
        let idx = (seq as usize) % total;
        let seg = &segments[idx];
        out.push_str(&format!("#EXTINF:{:.3},\n", seg.duration));
        out.push_str(&format!("{}\n", seg.file_name));
    }
    // No #EXT-X-ENDLIST — this is a live playlist.
    out
}

async fn parse_segment_list(playlist_path: &Path) -> Result<Vec<LiveSegment>, AppError> {
    let text = tokio::fs::read_to_string(playlist_path)
        .await
        .map_err(|e| AppError::internal(format!("读取切片播放列表失败: {}", e)))?;
    let mut out = Vec::new();
    let mut pending_duration: Option<f64> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("#EXTINF:") {
            let value = rest.split(',').next().unwrap_or("0");
            if let Ok(d) = value.trim().parse::<f64>() {
                pending_duration = Some(d);
            }
        } else if !trimmed.is_empty() && !trimmed.starts_with('#') {
            if let Some(d) = pending_duration.take() {
                out.push(LiveSegment {
                    file_name: trimmed.to_string(),
                    duration: d,
                });
            }
        }
    }
    Ok(out)
}

async fn run_ffmpeg_hls_fmp4_encode(
    input_path: &Path,
    playlist_path: &Path,
    segment_pattern: &Path,
    init_path: &Path,
) -> Result<(), AppError> {
    // `-hls_fmp4_init_filename` only accepts a bare filename (it is also written into
    // the playlist as the EXT-X-MAP URI). ffmpeg resolves it against its current working
    // directory, so run ffmpeg from the playlist's directory to keep init.mp4 next to
    // the segments instead of dropping it into the process CWD.
    let work_dir = init_path
        .parent()
        .ok_or_else(|| AppError::internal("fmp4 输出目录无效"))?;
    let init_name = init_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("init.mp4");

    let mut command = Command::new("ffmpeg");
    command
        .current_dir(work_dir)
        .arg("-y")
        .arg("-i")
        .arg(input_path)
        .args(["-c:v", "libx264"])
        .args(["-c:a", "aac"])
        .args(["-f", "hls"])
        .args(["-hls_time", "6"])
        .args(["-hls_playlist_type", "vod"])
        .args(["-hls_segment_type", "fmp4"])
        .arg("-hls_fmp4_init_filename")
        .arg(init_name)
        .arg("-hls_segment_filename")
        .arg(segment_pattern)
        .arg(playlist_path);
    let output = command
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| AppError::internal(format!("启动 ffmpeg (fmp4) 失败: {}", e)))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(AppError::internal(format!(
        "ffmpeg (fmp4) 退出码 {}: {}",
        output.status.code().unwrap_or(-1),
        stderr
            .lines()
            .rev()
            .take(6)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" | ")
    )))
}

fn render_live_page(
    base_url: &str,
    root_dir: &Path,
    current_path: Option<&Path>,
    jobs: &[(String, String, String, String, String, String)],
) -> String {
    let current_path_value = current_path
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let current_path_html = current_path
        .map(|p| {
            format!(
                "<p>当前选择视频：<code>{}</code></p>",
                escape_html(&p.to_string_lossy())
            )
        })
        .unwrap_or_else(|| "<p>还没有选择本地视频。</p>".to_string());

    let mut job_rows = String::new();
    if jobs.is_empty() {
        job_rows.push_str(
            "<p style=\"color:#475467\">还没有 Live 任务，选择一个本地视频并点击「切片并启动」开始模拟。</p>",
        );
    } else {
        job_rows.push_str("<ul style=\"padding-left:0;list-style:none;display:grid;gap:12px\">");
        for (id, source_name, created_at, ts_url, fmp4_url, seg_count) in jobs {
            job_rows.push_str(&format!(
                "<li style=\"border:1px solid #e5e7eb;border-radius:14px;padding:14px;background:#fff\">\
                   <p><strong>{name}</strong> · 切片数 {count} · 创建于 {time}</p>\
                   <p>TS 直播：<code>{ts}</code></p>\
                   <p>fMP4 直播：<code>{fmp4}</code></p>\
                   <p style=\"color:#94a3b8;font-size:12px\">job_id: {id}</p>\
                 </li>",
                name = escape_html(source_name),
                count = escape_html(seg_count),
                time = escape_html(created_at),
                ts = escape_html(ts_url),
                fmp4 = escape_html(fmp4_url),
                id = escape_html(id),
            ));
        }
        job_rows.push_str("</ul>");
    }

    format!(
        "<!doctype html>\
         <html lang=\"zh-CN\">\
         <head>\
           <meta charset=\"utf-8\">\
           <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
           <title>HLS Live Simulation</title>\
           <style>\
             *{{box-sizing:border-box}}\
             body{{margin:0;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;background:#f8fafc;color:#0f172a}}\
             main{{max-width:1000px;margin:0 auto;padding:32px 20px 48px}}\
             .hero,.panel{{background:#fff;border:1px solid #dbe5f0;border-radius:20px;padding:22px;box-shadow:0 10px 30px rgba(15,23,42,.05);margin-bottom:20px}}\
             .hero{{border-radius:24px;padding:28px}}\
             h1{{margin:0;font-size:32px}}h2{{margin:0 0 12px;font-size:20px}}\
             p{{margin:8px 0;color:#475467}}\
             code{{font-family:'SFMono-Regular',Consolas,monospace;background:#f2f4f7;padding:2px 6px;border-radius:6px;word-break:break-all}}\
             label{{display:block;font-weight:600;margin-bottom:8px}}\
             input[type='text']{{width:100%;padding:12px 14px;border:1px solid #cbd5e1;border-radius:12px;font:inherit;background:#fff}}\
             button{{appearance:none;border:none;border-radius:12px;background:#2563eb;color:#fff;padding:12px 16px;font:inherit;font-weight:700;cursor:pointer}}\
             button:hover{{background:#1d4ed8}}\
             .button-secondary{{background:#e2e8f0;color:#0f172a}}\
             .button-secondary:hover{{background:#cbd5e1}}\
             a{{color:#2563eb;font-weight:600;text-decoration:none}}\
             .actions{{display:flex;gap:12px;flex-wrap:wrap;margin-top:14px}}\
             .field{{display:grid;gap:8px;margin-bottom:16px}}\
             .form-actions{{display:flex;gap:12px;flex-wrap:wrap}}\
             .pick-status{{min-height:22px;font-size:14px;color:#475467}}\
           </style>\
         </head>\
         <body>\
           <main>\
             <section class=\"hero\">\
               <h1>HLS Live 模拟</h1>\
               <p>选择一个本地视频，本服务会用 ffmpeg 预切成 TS 和 fMP4 两套分片，然后通过 <code>/live-test/&lt;job&gt;/&lt;flavor&gt;/index.m3u8</code> 输出一个会随服务器时间滚动的播放列表，模拟一个一直在播放该视频的直播流。</p>\
               <p>服务根目录：<code>{root}</code></p>\
               <p>基础地址：<code>{base}</code></p>\
               {current}\
               <p><a href=\"/\">返回 HLS VOD 页面</a></p>\
             </section>\
             <form class=\"panel\" action=\"/live\" method=\"post\">\
               <h2>选择本机视频</h2>\
               <div class=\"field\">\
                 <label for=\"live_path\">本机视频绝对路径</label>\
                 <input id=\"live_path\" type=\"text\" name=\"path\" value=\"{value}\" placeholder=\"例如 /Users/me/Videos/sample.mp4 或 D:\\Videos\\sample.mkv\" required>\
               </div>\
               <div class=\"form-actions\">\
                 <button type=\"button\" id=\"live_pick\" class=\"button-secondary\">浏览选择</button>\
                 <button type=\"submit\">使用这个视频</button>\
               </div>\
               <p id=\"live_pick_status\" class=\"pick-status\"></p>\
             </form>\
             <section class=\"panel\">\
               <h2>切片并启动直播模拟</h2>\
               <p>使用上方选择的视频，预切成 TS 和 fMP4，然后注册一个直播任务。每次刷新 <code>index.m3u8</code> 时窗口都会向前滚动。</p>\
               <form action=\"/generate/live\" method=\"post\">\
                 <button type=\"submit\">切片并启动</button>\
               </form>\
             </section>\
             <section class=\"panel\">\
               <h2>已有的直播任务</h2>\
               {jobs}\
             </section>\
             <script>\
               (() => {{\
                 const pickButton = document.getElementById('live_pick');\
                 const pathInput = document.getElementById('live_path');\
                 const statusText = document.getElementById('live_pick_status');\
                 pickButton.addEventListener('click', async () => {{\
                   pickButton.disabled = true;\
                   statusText.textContent = '正在打开文件选择框...';\
                   try {{\
                     const response = await fetch('/live/pick', {{ method: 'POST' }});\
                     const data = await response.json();\
                     if (!response.ok) {{ throw new Error(data.message || '选择失败'); }}\
                     if (data.selected && data.path) {{\
                       pathInput.value = data.path;\
                       statusText.textContent = '已选择，正在刷新。';\
                       window.location.reload();\
                     }} else {{\
                       statusText.textContent = data.message || '已取消选择。';\
                     }}\
                   }} catch (error) {{\
                     statusText.textContent = '选择失败：' + (error.message || String(error));\
                   }} finally {{\
                     pickButton.disabled = false;\
                   }}\
                 }});\
               }})();\
             </script>\
           </main>\
         </body>\
         </html>",
        root = escape_html(&root_dir.to_string_lossy()),
        base = escape_html(base_url),
        current = current_path_html,
        value = escape_html(&current_path_value),
        jobs = job_rows,
    )
}
