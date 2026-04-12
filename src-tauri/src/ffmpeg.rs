use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::error::AppError;
use crate::state::AppState;

const SUBTITLE_TRACK_HEIGHT_RATIO_NUMERATOR: u32 = 22;
const SUBTITLE_TRACK_HEIGHT_RATIO_DENOMINATOR: u32 = 100;
const MIN_SUBTITLE_TRACK_HEIGHT: u32 = 128;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SubtitleCue {
    start_ms: u64,
    end_ms: u64,
    text: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FfmpegStatus {
    NotInstalled,
    Installed { path: String, version: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct FfmpegDownloadProgress {
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub stage: FfmpegDownloadStage,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FfmpegDownloadStage {
    Downloading,
    Unpacking,
    Done,
}

#[derive(Debug, Clone, Serialize)]
pub struct MediaAnalysisStream {
    pub index: usize,
    pub codec_type: Option<String>,
    pub codec_name: Option<String>,
    pub codec_long_name: Option<String>,
    pub profile: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub pix_fmt: Option<String>,
    pub level: Option<i32>,
    pub r_frame_rate: Option<String>,
    pub avg_frame_rate: Option<String>,
    pub sample_rate: Option<String>,
    pub channels: Option<u32>,
    pub channel_layout: Option<String>,
    pub bit_rate: Option<String>,
    pub duration: Option<String>,
    pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MediaAnalysisResult {
    pub file_path: String,
    pub format_name: Option<String>,
    pub format_long_name: Option<String>,
    pub duration: Option<String>,
    pub size: Option<String>,
    pub bit_rate: Option<String>,
    pub probe_score: Option<i32>,
    pub stream_count: usize,
    pub video_streams: Vec<MediaAnalysisStream>,
    pub audio_streams: Vec<MediaAnalysisStream>,
    pub subtitle_streams: Vec<MediaAnalysisStream>,
    pub other_streams: Vec<MediaAnalysisStream>,
    pub raw_json: String,
}

#[derive(Debug, Deserialize)]
struct FfprobeOutput {
    streams: Vec<FfprobeStream>,
    format: Option<FfprobeFormat>,
}

#[derive(Debug, Deserialize)]
struct FfprobeFormat {
    format_name: Option<String>,
    format_long_name: Option<String>,
    duration: Option<String>,
    size: Option<String>,
    bit_rate: Option<String>,
    probe_score: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct FfprobeStream {
    index: usize,
    codec_type: Option<String>,
    codec_name: Option<String>,
    codec_long_name: Option<String>,
    profile: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    pix_fmt: Option<String>,
    level: Option<i32>,
    r_frame_rate: Option<String>,
    avg_frame_rate: Option<String>,
    sample_rate: Option<String>,
    channels: Option<u32>,
    channel_layout: Option<String>,
    bit_rate: Option<String>,
    duration: Option<String>,
    tags: Option<FfprobeStreamTags>,
}

#[derive(Debug, Deserialize)]
struct FfprobeStreamTags {
    language: Option<String>,
}

#[derive(Debug, Clone)]
struct MergeVideoInputInfo {
    width: u32,
    height: u32,
    video_codec: Option<String>,
    video_frame_rate: Option<String>,
    has_audio: bool,
    audio_codec: Option<String>,
    audio_sample_rate: Option<String>,
    audio_channels: Option<u32>,
}

fn ffmpeg_dir(app_handle: &AppHandle) -> PathBuf {
    app_handle
        .path()
        .app_data_dir()
        .expect("Failed to get app data dir")
        .join("ffmpeg")
}

fn ffmpeg_binary_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }
}

fn ffmpeg_binary_path(app_handle: &AppHandle) -> PathBuf {
    ffmpeg_dir(app_handle).join(ffmpeg_binary_name())
}

fn configure_background_command(command: &mut tokio::process::Command) {
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
}

async fn probe_ffmpeg_version(path: &Path) -> Option<String> {
    let mut command = tokio::process::Command::new(path);
    configure_background_command(&mut command);
    let output = command
        .arg("-version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Parse "ffmpeg version N.N.N ..." from first line
    stdout
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("ffmpeg version "))
        .map(|rest| {
            rest.split_whitespace()
                .next()
                .unwrap_or("unknown")
                .to_string()
        })
}

async fn detect_at_path(path: &Path) -> Option<FfmpegStatus> {
    if !path.exists() {
        return None;
    }
    let version = probe_ffmpeg_version(path).await?;
    Some(FfmpegStatus::Installed {
        path: path.to_string_lossy().into_owned(),
        version,
    })
}

async fn detect_system_ffmpeg() -> Option<FfmpegStatus> {
    let mut command = tokio::process::Command::new("ffmpeg");
    configure_background_command(&mut command);
    let output = command
        .arg("-version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let version = stdout
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("ffmpeg version "))
        .map(|rest| {
            rest.split_whitespace()
                .next()
                .unwrap_or("unknown")
                .to_string()
        })
        .unwrap_or_else(|| "unknown".to_string());

    // Resolve actual path via `where` (Windows) or `which` (Unix)
    let which_cmd = if cfg!(target_os = "windows") {
        "where"
    } else {
        "which"
    };
    let mut which_command = tokio::process::Command::new(which_cmd);
    configure_background_command(&mut which_command);
    let resolved_path = which_command
        .arg("ffmpeg")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| "ffmpeg".to_string());

    Some(FfmpegStatus::Installed {
        path: resolved_path,
        version,
    })
}

/// Detect ffmpeg: 1) user custom path → 2) app data dir → 3) system PATH
pub async fn detect_ffmpeg(app_handle: &AppHandle) -> FfmpegStatus {
    // 1. User-specified custom path
    let custom_path = app_handle
        .state::<AppState>()
        .ffmpeg_path
        .lock()
        .await
        .clone();
    if let Some(ref custom) = custom_path {
        let path = Path::new(custom);
        if let Some(status) = detect_at_path(path).await {
            return status;
        }
    }

    // 2. App data dir managed copy
    let managed_path = ffmpeg_binary_path(app_handle);
    if let Some(status) = detect_at_path(&managed_path).await {
        return status;
    }

    // 3. System PATH
    if let Some(status) = detect_system_ffmpeg().await {
        return status;
    }

    FfmpegStatus::NotInstalled
}

/// Resolve the ffmpeg binary path if available (for use by conversion fallback).
pub async fn resolve_ffmpeg_path(app_handle: &AppHandle) -> Option<PathBuf> {
    match detect_ffmpeg(app_handle).await {
        FfmpegStatus::Installed { path, .. } => Some(PathBuf::from(path)),
        FfmpegStatus::NotInstalled => None,
    }
}

pub async fn analyze_media_file(
    ffmpeg_path: &Path,
    input_path: &Path,
) -> Result<MediaAnalysisResult, AppError> {
    let raw_json = run_ffprobe_json(ffmpeg_path, input_path).await?;
    let parsed: FfprobeOutput = serde_json::from_str(&raw_json)
        .map_err(|e| AppError::Conversion(format!("解析 ffprobe 输出失败: {}", e)))?;

    let mut video_streams = Vec::new();
    let mut audio_streams = Vec::new();
    let mut subtitle_streams = Vec::new();
    let mut other_streams = Vec::new();

    for stream in parsed.streams {
        let mapped = MediaAnalysisStream {
            index: stream.index,
            codec_type: stream.codec_type.clone(),
            codec_name: stream.codec_name,
            codec_long_name: stream.codec_long_name,
            profile: stream.profile,
            width: stream.width,
            height: stream.height,
            pix_fmt: stream.pix_fmt,
            level: stream.level,
            r_frame_rate: stream.r_frame_rate,
            avg_frame_rate: stream.avg_frame_rate,
            sample_rate: stream.sample_rate,
            channels: stream.channels,
            channel_layout: stream.channel_layout,
            bit_rate: stream.bit_rate,
            duration: stream.duration,
            language: stream.tags.and_then(|tags| tags.language),
        };

        match stream.codec_type.as_deref() {
            Some("video") => video_streams.push(mapped),
            Some("audio") => audio_streams.push(mapped),
            Some("subtitle") => subtitle_streams.push(mapped),
            _ => other_streams.push(mapped),
        }
    }

    let stream_count =
        video_streams.len() + audio_streams.len() + subtitle_streams.len() + other_streams.len();
    let format = parsed.format;

    Ok(MediaAnalysisResult {
        file_path: input_path.to_string_lossy().into_owned(),
        format_name: format.as_ref().and_then(|item| item.format_name.clone()),
        format_long_name: format
            .as_ref()
            .and_then(|item| item.format_long_name.clone()),
        duration: format.as_ref().and_then(|item| item.duration.clone()),
        size: format.as_ref().and_then(|item| item.size.clone()),
        bit_rate: format.as_ref().and_then(|item| item.bit_rate.clone()),
        probe_score: format.as_ref().and_then(|item| item.probe_score),
        stream_count,
        video_streams,
        audio_streams,
        subtitle_streams,
        other_streams,
        raw_json,
    })
}

/// Download ffmpeg to app data dir using ffmpeg-sidecar, emitting progress events.
pub async fn download_ffmpeg(app_handle: AppHandle) -> Result<PathBuf, AppError> {
    let dest_dir = ffmpeg_dir(&app_handle);

    let download_url = ffmpeg_sidecar::download::ffmpeg_download_url()
        .map_err(|e| AppError::Internal(format!("Failed to get ffmpeg download URL: {}", e)))?;

    let app_handle_progress = app_handle.clone();

    // Download and unpack in a blocking task since ffmpeg-sidecar's API is synchronous
    let final_path = tokio::task::spawn_blocking(move || -> Result<PathBuf, AppError> {
        std::fs::create_dir_all(&dest_dir)?;

        let archive_path = ffmpeg_sidecar::download::download_ffmpeg_package_with_progress(
            &download_url,
            &dest_dir,
            |event| {
                use ffmpeg_sidecar::download::FfmpegDownloadProgressEvent as P;
                let progress = match event {
                    P::Starting => FfmpegDownloadProgress {
                        downloaded_bytes: 0,
                        total_bytes: 0,
                        stage: FfmpegDownloadStage::Downloading,
                    },
                    P::Downloading {
                        total_bytes,
                        downloaded_bytes,
                    } => FfmpegDownloadProgress {
                        downloaded_bytes,
                        total_bytes,
                        stage: FfmpegDownloadStage::Downloading,
                    },
                    P::UnpackingArchive => FfmpegDownloadProgress {
                        downloaded_bytes: 0,
                        total_bytes: 0,
                        stage: FfmpegDownloadStage::Unpacking,
                    },
                    P::Done => FfmpegDownloadProgress {
                        downloaded_bytes: 0,
                        total_bytes: 0,
                        stage: FfmpegDownloadStage::Done,
                    },
                };
                let _ = app_handle_progress.emit("ffmpeg-download-progress", &progress);
            },
        )
        .map_err(|e| AppError::Internal(format!("Failed to download ffmpeg: {}", e)))?;

        ffmpeg_sidecar::download::unpack_ffmpeg(&archive_path, &dest_dir)
            .map_err(|e| AppError::Internal(format!("Failed to unpack ffmpeg: {}", e)))?;

        // Clean up the archive
        let _ = std::fs::remove_file(&archive_path);

        let binary_path = dest_dir.join(ffmpeg_binary_name());
        if !binary_path.exists() {
            return Err(AppError::Internal(
                "ffmpeg binary not found after unpacking".to_string(),
            ));
        }

        Ok(binary_path)
    })
    .await
    .map_err(|e| AppError::Internal(format!("Download task join error: {}", e)))??;

    let _ = app_handle.emit(
        "ffmpeg-download-progress",
        &FfmpegDownloadProgress {
            downloaded_bytes: 0,
            total_bytes: 0,
            stage: FfmpegDownloadStage::Done,
        },
    );

    Ok(final_path)
}

/// Convert TS to MP4 using ffmpeg: stream-copy with faststart.
pub async fn convert_ts_to_mp4(
    ffmpeg_path: &Path,
    ts_path: &Path,
    mp4_path: &Path,
) -> Result<(), AppError> {
    let mut command = tokio::process::Command::new(ffmpeg_path);
    configure_background_command(&mut command);
    let output = command
        .args(["-y", "-i"])
        .arg(ts_path)
        .args(["-c", "copy", "-movflags", "+faststart"])
        .arg(mp4_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| AppError::Conversion(format!("Failed to run ffmpeg: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: String = stderr.lines().rev().take(5).collect::<Vec<_>>().join("\n");
        return Err(AppError::Conversion(format!(
            "ffmpeg exited with {}: {}",
            output.status, tail
        )));
    }

    Ok(())
}

pub async fn convert_media_file(
    ffmpeg_path: &Path,
    input_path: &Path,
    output_path: &Path,
    target_format: &str,
    convert_mode: &str,
) -> Result<(), AppError> {
    let normalized_format = target_format.trim().to_lowercase();
    let normalized_mode = convert_mode.trim().to_lowercase();
    let args = build_media_convert_args(
        input_path,
        output_path,
        &normalized_format,
        &normalized_mode,
    )?;
    run_ffmpeg_command(ffmpeg_path, &args).await
}

pub async fn transcode_media_file(
    ffmpeg_path: &Path,
    input_path: &Path,
    output_path: &Path,
    output_format: &str,
    video_codec: &str,
    audio_codec: &str,
) -> Result<(), AppError> {
    let args = build_media_transcode_args(
        input_path,
        output_path,
        &output_format.trim().to_lowercase(),
        &video_codec.trim().to_lowercase(),
        &audio_codec.trim().to_lowercase(),
    )?;
    run_ffmpeg_command(ffmpeg_path, &args).await
}

pub async fn merge_video_files(
    ffmpeg_path: &Path,
    input_paths: &[PathBuf],
    output_path: &Path,
    merge_mode: &str,
) -> Result<(), AppError> {
    if input_paths.len() < 2 {
        return Err(AppError::InvalidInput(
            "请至少选择两个视频文件进行拼接".to_string(),
        ));
    }

    let temp_dir =
        std::env::temp_dir().join(format!("m3u8quicker_video_merge_{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&temp_dir).await?;

    let result = async {
        let mut input_infos = Vec::with_capacity(input_paths.len());
        for input_path in input_paths {
            input_infos.push(inspect_merge_video_input(ffmpeg_path, input_path).await?);
        }

        let normalized_paths = if merge_mode == "fast" {
            validate_fast_merge_inputs(&input_infos)?;
            let mut remuxed_paths = Vec::with_capacity(input_paths.len());

            for (index, input_path) in input_paths.iter().enumerate() {
                let remuxed_path = temp_dir.join(format!("clip_{index:03}.mp4"));
                let args = build_merge_video_fast_remux_args(input_path, &remuxed_path);
                run_ffmpeg_command(ffmpeg_path, &args).await?;
                remuxed_paths.push(remuxed_path);
            }

            remuxed_paths
        } else {
            let target_size = calculate_merge_video_output_size(&input_infos)?;
            let mut transcoded_paths = Vec::with_capacity(input_paths.len());

            for (index, (input_path, input_info)) in
                input_paths.iter().zip(input_infos.iter()).enumerate()
            {
                let normalized_path = temp_dir.join(format!("clip_{index:03}.mp4"));
                let args = build_merge_video_normalize_args(
                    input_path,
                    &normalized_path,
                    target_size,
                    input_info,
                );
                run_ffmpeg_command(ffmpeg_path, &args).await?;
                transcoded_paths.push(normalized_path);
            }

            transcoded_paths
        };
        let concat_list_path = temp_dir.join("concat.txt");
        tokio::fs::write(
            &concat_list_path,
            build_ffmpeg_concat_list(&normalized_paths),
        )
        .await?;

        let args = build_merge_video_concat_args(&concat_list_path, output_path);
        run_ffmpeg_command(ffmpeg_path, &args).await
    }
    .await;

    let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    result
}

fn build_media_convert_args(
    input_path: &Path,
    output_path: &Path,
    target_format: &str,
    convert_mode: &str,
) -> Result<Vec<String>, AppError> {
    let mut args = vec![
        "-y".to_string(),
        "-hide_banner".to_string(),
        "-i".to_string(),
        input_path.to_string_lossy().into_owned(),
    ];

    match convert_mode {
        "quick" => match target_format {
            "mp4" | "mov" => args.extend([
                "-c".to_string(),
                "copy".to_string(),
                "-movflags".to_string(),
                "+faststart".to_string(),
            ]),
            "mkv" => args.extend(["-c".to_string(), "copy".to_string()]),
            "mp3" | "m4a" | "wav" => {
                args.extend(["-vn".to_string(), "-c:a".to_string(), "copy".to_string()])
            }
            _ => {
                return Err(AppError::InvalidInput(format!(
                    "暂不支持转换为 {} 格式",
                    target_format
                )))
            }
        },
        "compatible" => match target_format {
            "mp4" => args.extend([
                "-c:v".to_string(),
                "libx264".to_string(),
                "-c:a".to_string(),
                "aac".to_string(),
                "-movflags".to_string(),
                "+faststart".to_string(),
            ]),
            "mkv" => args.extend([
                "-c:v".to_string(),
                "libx264".to_string(),
                "-c:a".to_string(),
                "aac".to_string(),
            ]),
            "mov" => args.extend([
                "-c:v".to_string(),
                "libx264".to_string(),
                "-c:a".to_string(),
                "aac".to_string(),
                "-movflags".to_string(),
                "+faststart".to_string(),
            ]),
            "mp3" => args.extend([
                "-vn".to_string(),
                "-c:a".to_string(),
                "libmp3lame".to_string(),
                "-b:a".to_string(),
                "192k".to_string(),
            ]),
            "m4a" => args.extend([
                "-vn".to_string(),
                "-c:a".to_string(),
                "aac".to_string(),
                "-b:a".to_string(),
                "192k".to_string(),
            ]),
            "wav" => args.extend([
                "-vn".to_string(),
                "-c:a".to_string(),
                "pcm_s16le".to_string(),
            ]),
            _ => {
                return Err(AppError::InvalidInput(format!(
                    "暂不支持转换为 {} 格式",
                    target_format
                )))
            }
        },
        _ => {
            return Err(AppError::InvalidInput(format!(
                "暂不支持 {} 转换模式",
                convert_mode
            )))
        }
    }

    args.push(output_path.to_string_lossy().into_owned());
    Ok(args)
}

fn build_media_transcode_args(
    input_path: &Path,
    output_path: &Path,
    output_format: &str,
    video_codec: &str,
    audio_codec: &str,
) -> Result<Vec<String>, AppError> {
    validate_transcode_combination(output_format, video_codec, audio_codec)?;

    let mut args = vec![
        "-y".to_string(),
        "-hide_banner".to_string(),
        "-i".to_string(),
        input_path.to_string_lossy().into_owned(),
    ];

    args.extend([
        "-c:v".to_string(),
        map_video_codec(video_codec)?.to_string(),
    ]);

    match audio_codec {
        "aac" => args.extend([
            "-c:a".to_string(),
            "aac".to_string(),
            "-b:a".to_string(),
            "192k".to_string(),
        ]),
        "mp3" => args.extend([
            "-c:a".to_string(),
            "libmp3lame".to_string(),
            "-b:a".to_string(),
            "192k".to_string(),
        ]),
        "opus" => args.extend([
            "-c:a".to_string(),
            "libopus".to_string(),
            "-b:a".to_string(),
            "160k".to_string(),
        ]),
        "copy" => args.extend(["-c:a".to_string(), "copy".to_string()]),
        _ => {
            return Err(AppError::InvalidInput(format!(
                "暂不支持 {} 音频编码",
                audio_codec
            )))
        }
    }

    if matches!(output_format, "mp4" | "mov") {
        args.extend(["-movflags".to_string(), "+faststart".to_string()]);
    }

    args.push(output_path.to_string_lossy().into_owned());
    Ok(args)
}

fn map_video_codec(video_codec: &str) -> Result<&'static str, AppError> {
    match video_codec {
        "h264" => Ok("libx264"),
        "h265" => Ok("libx265"),
        "vp9" => Ok("libvpx-vp9"),
        "copy" => Ok("copy"),
        _ => Err(AppError::InvalidInput(format!(
            "暂不支持 {} 视频编码",
            video_codec
        ))),
    }
}

fn validate_transcode_combination(
    output_format: &str,
    video_codec: &str,
    audio_codec: &str,
) -> Result<(), AppError> {
    match output_format {
        "mp4" => {
            if !matches!(video_codec, "h264" | "h265" | "copy") {
                return Err(AppError::InvalidInput(
                    "MP4 仅支持 H.264、H.265 或复制视频编码".to_string(),
                ));
            }
            if !matches!(audio_codec, "aac" | "mp3" | "copy") {
                return Err(AppError::InvalidInput(
                    "MP4 仅支持 AAC、MP3 或复制音频编码".to_string(),
                ));
            }
        }
        "mkv" => {
            if !matches!(video_codec, "h264" | "h265" | "vp9" | "copy") {
                return Err(AppError::InvalidInput(
                    "MKV 仅支持 H.264、H.265、VP9 或复制视频编码".to_string(),
                ));
            }
            if !matches!(audio_codec, "aac" | "mp3" | "opus" | "copy") {
                return Err(AppError::InvalidInput(
                    "MKV 仅支持 AAC、MP3、Opus 或复制音频编码".to_string(),
                ));
            }
        }
        "mov" => {
            if !matches!(video_codec, "h264" | "h265" | "copy") {
                return Err(AppError::InvalidInput(
                    "MOV 仅支持 H.264、H.265 或复制视频编码".to_string(),
                ));
            }
            if !matches!(audio_codec, "aac" | "copy") {
                return Err(AppError::InvalidInput(
                    "MOV 仅支持 AAC 或复制音频编码".to_string(),
                ));
            }
        }
        _ => {
            return Err(AppError::InvalidInput(format!(
                "暂不支持 {} 输出格式",
                output_format
            )))
        }
    }

    Ok(())
}

async fn inspect_merge_video_input(
    ffmpeg_path: &Path,
    input_path: &Path,
) -> Result<MergeVideoInputInfo, AppError> {
    let raw_json = run_ffprobe_json(ffmpeg_path, input_path).await?;
    let parsed: FfprobeOutput = serde_json::from_str(&raw_json)
        .map_err(|e| AppError::Conversion(format!("解析 ffprobe 输出失败: {}", e)))?;

    let video_stream = parsed
        .streams
        .iter()
        .find(|stream| stream.codec_type.as_deref() == Some("video"))
        .ok_or_else(|| {
            AppError::InvalidInput(format!(
                "文件 {} 不包含视频轨",
                input_path.to_string_lossy()
            ))
        })?;

    let width = video_stream.width.ok_or_else(|| {
        AppError::InvalidInput(format!(
            "无法识别文件 {} 的视频宽度",
            input_path.to_string_lossy()
        ))
    })?;
    let height = video_stream.height.ok_or_else(|| {
        AppError::InvalidInput(format!(
            "无法识别文件 {} 的视频高度",
            input_path.to_string_lossy()
        ))
    })?;
    let has_audio = parsed
        .streams
        .iter()
        .any(|stream| stream.codec_type.as_deref() == Some("audio"));
    let audio_stream = parsed
        .streams
        .iter()
        .find(|stream| stream.codec_type.as_deref() == Some("audio"));

    Ok(MergeVideoInputInfo {
        width,
        height,
        video_codec: video_stream.codec_name.clone(),
        video_frame_rate: video_stream
            .avg_frame_rate
            .clone()
            .or_else(|| video_stream.r_frame_rate.clone()),
        has_audio,
        audio_codec: audio_stream.and_then(|stream| stream.codec_name.clone()),
        audio_sample_rate: audio_stream.and_then(|stream| stream.sample_rate.clone()),
        audio_channels: audio_stream.and_then(|stream| stream.channels),
    })
}

fn validate_fast_merge_inputs(input_infos: &[MergeVideoInputInfo]) -> Result<(), AppError> {
    let Some(first) = input_infos.first() else {
        return Err(AppError::InvalidInput("请选择有效的视频文件".to_string()));
    };

    if !matches!(first.video_codec.as_deref(), Some("h264") | Some("hevc")) {
        return Err(AppError::InvalidInput(
            "极速合并目前仅支持 H.264 或 H.265 视频，请改用兼容合并".to_string(),
        ));
    }
    if first.has_audio && !matches!(first.audio_codec.as_deref(), Some("aac") | Some("mp3")) {
        return Err(AppError::InvalidInput(
            "极速合并目前仅支持 AAC 或 MP3 音频，请改用兼容合并".to_string(),
        ));
    }

    for info in input_infos.iter().skip(1) {
        if info.width != first.width
            || info.height != first.height
            || info.video_codec != first.video_codec
            || info.video_frame_rate != first.video_frame_rate
            || info.has_audio != first.has_audio
            || info.audio_codec != first.audio_codec
            || info.audio_sample_rate != first.audio_sample_rate
            || info.audio_channels != first.audio_channels
        {
            return Err(AppError::InvalidInput(
                "极速合并要求所有视频的分辨率、视频编码、帧率和音频轨规格一致；当前文件不一致，请改用兼容合并".to_string(),
            ));
        }
    }

    Ok(())
}

fn calculate_merge_video_output_size(
    input_infos: &[MergeVideoInputInfo],
) -> Result<(u32, u32), AppError> {
    let max_width = input_infos
        .iter()
        .map(|item| item.width)
        .max()
        .ok_or_else(|| AppError::InvalidInput("请选择有效的视频文件".to_string()))?;
    let max_height = input_infos
        .iter()
        .map(|item| item.height)
        .max()
        .ok_or_else(|| AppError::InvalidInput("请选择有效的视频文件".to_string()))?;

    Ok((round_up_to_even(max_width), round_up_to_even(max_height)))
}

fn round_up_to_even(value: u32) -> u32 {
    if value % 2 == 0 {
        value
    } else {
        value.saturating_add(1)
    }
}

fn build_merge_video_normalize_args(
    input_path: &Path,
    output_path: &Path,
    target_size: (u32, u32),
    input_info: &MergeVideoInputInfo,
) -> Vec<String> {
    let (target_width, target_height) = target_size;
    let scale_filter = format!(
        "scale=w={target_width}:h={target_height}:force_original_aspect_ratio=decrease,pad={target_width}:{target_height}:(ow-iw)/2:(oh-ih)/2:black,setsar=1,fps=30,format=yuv420p"
    );

    let mut args = vec![
        "-y".to_string(),
        "-hide_banner".to_string(),
        "-i".to_string(),
        input_path.to_string_lossy().into_owned(),
    ];

    if !input_info.has_audio {
        args.extend([
            "-f".to_string(),
            "lavfi".to_string(),
            "-i".to_string(),
            "anullsrc=channel_layout=stereo:sample_rate=48000".to_string(),
        ]);
    }

    args.extend([
        "-map".to_string(),
        "0:v:0".to_string(),
        "-map".to_string(),
        if input_info.has_audio {
            "0:a:0".to_string()
        } else {
            "1:a:0".to_string()
        },
        "-vf".to_string(),
        scale_filter,
        "-c:v".to_string(),
        "libx264".to_string(),
        "-preset".to_string(),
        "veryfast".to_string(),
        "-crf".to_string(),
        "20".to_string(),
        "-c:a".to_string(),
        "aac".to_string(),
        "-b:a".to_string(),
        "192k".to_string(),
        "-ar".to_string(),
        "48000".to_string(),
        "-ac".to_string(),
        "2".to_string(),
        "-movflags".to_string(),
        "+faststart".to_string(),
    ]);

    if !input_info.has_audio {
        args.push("-shortest".to_string());
    }

    args.push(output_path.to_string_lossy().into_owned());
    args
}

fn build_merge_video_fast_remux_args(input_path: &Path, output_path: &Path) -> Vec<String> {
    vec![
        "-y".to_string(),
        "-hide_banner".to_string(),
        "-i".to_string(),
        input_path.to_string_lossy().into_owned(),
        "-map".to_string(),
        "0".to_string(),
        "-c".to_string(),
        "copy".to_string(),
        "-movflags".to_string(),
        "+faststart".to_string(),
        output_path.to_string_lossy().into_owned(),
    ]
}

fn build_ffmpeg_concat_list(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| {
            let normalized = path.to_string_lossy().replace('\\', "/");
            format!("file '{}'\n", normalized.replace('\'', "'\\''"))
        })
        .collect::<String>()
}

fn build_merge_video_concat_args(concat_list_path: &Path, output_path: &Path) -> Vec<String> {
    vec![
        "-y".to_string(),
        "-hide_banner".to_string(),
        "-f".to_string(),
        "concat".to_string(),
        "-safe".to_string(),
        "0".to_string(),
        "-i".to_string(),
        concat_list_path.to_string_lossy().into_owned(),
        "-c".to_string(),
        "copy".to_string(),
        "-movflags".to_string(),
        "+faststart".to_string(),
        output_path.to_string_lossy().into_owned(),
    ]
}

pub async fn convert_multi_track_hls_to_mp4(
    ffmpeg_path: &Path,
    video_playlist: &Path,
    audio_playlist: Option<&Path>,
    subtitle_playlist: Option<&Path>,
    mp4_path: &Path,
) -> Result<(), AppError> {
    let temp_dir = subtitle_playlist.map(|_| {
        std::env::temp_dir().join(format!("m3u8quicker_subtitles_{}", uuid::Uuid::new_v4()))
    });
    let subtitle_input_path =
        if let (Some(subtitle_playlist), Some(temp_dir)) = (subtitle_playlist, temp_dir.as_ref()) {
            tokio::fs::create_dir_all(temp_dir).await?;
            let subtitle_srt_path = temp_dir.join("subtitle.srt");
            export_hls_subtitle_playlist_to_srt(subtitle_playlist, &subtitle_srt_path).await?;
            Some(subtitle_srt_path)
        } else {
            None
        };

    let subtitle_dimensions = if subtitle_input_path.is_some() {
        probe_video_dimensions(ffmpeg_path, video_playlist)
            .await
            .map(calculate_subtitle_track_size)
    } else {
        None
    };
    let args = build_multi_track_hls_to_mp4_args(
        video_playlist,
        audio_playlist,
        subtitle_input_path.as_deref(),
        subtitle_dimensions,
        mp4_path,
    );
    let result = run_ffmpeg_command(ffmpeg_path, &args).await;

    if let Some(temp_dir) = temp_dir {
        let _ = tokio::fs::remove_dir_all(temp_dir).await;
    }

    result
}

fn build_multi_track_hls_to_mp4_args(
    video_playlist: &Path,
    audio_playlist: Option<&Path>,
    subtitle_playlist: Option<&Path>,
    subtitle_dimensions: Option<(u32, u32)>,
    mp4_path: &Path,
) -> Vec<String> {
    let mut args = vec!["-y".to_string(), "-hide_banner".to_string()];
    for input in [Some(video_playlist), audio_playlist].into_iter().flatten() {
        args.push("-allowed_extensions".to_string());
        args.push("ALL".to_string());
        args.push("-i".to_string());
        args.push(input.to_string_lossy().into_owned());
    }
    if let Some(subtitle_playlist) = subtitle_playlist {
        args.push("-i".to_string());
        args.push(subtitle_playlist.to_string_lossy().into_owned());
    }

    args.push("-map".to_string());
    args.push("0:v:0".to_string());

    let mut next_input_index = 1usize;
    if audio_playlist.is_some() {
        args.push("-map".to_string());
        args.push(format!("{}:a:0", next_input_index));
        next_input_index += 1;
    }
    if subtitle_playlist.is_some() {
        args.push("-map".to_string());
        args.push(format!("{}:s:0", next_input_index));
    }

    args.extend([
        "-c:v".to_string(),
        "copy".to_string(),
        "-c:a".to_string(),
        "copy".to_string(),
        "-c:s".to_string(),
        "mov_text".to_string(),
    ]);
    if let Some((subtitle_width, subtitle_height)) = subtitle_dimensions {
        args.push("-s:s:0".to_string());
        args.push(format!("{}x{}", subtitle_width, subtitle_height));
        args.push("-height:s:0".to_string());
        args.push(subtitle_height.to_string());
    }
    args.extend([
        "-movflags".to_string(),
        "+faststart".to_string(),
        mp4_path.to_string_lossy().into_owned(),
    ]);

    args
}

async fn run_ffmpeg_command(ffmpeg_path: &Path, args: &[String]) -> Result<(), AppError> {
    run_ffmpeg_command_in_dir(ffmpeg_path, args, None).await
}

async fn run_ffmpeg_command_in_dir(
    ffmpeg_path: &Path,
    args: &[String],
    current_dir: Option<&Path>,
) -> Result<(), AppError> {
    let mut command = tokio::process::Command::new(ffmpeg_path);
    configure_background_command(&mut command);
    command
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null());
    if let Some(current_dir) = current_dir {
        command.current_dir(current_dir);
    }

    let output = command
        .output()
        .await
        .map_err(|e| AppError::Conversion(format!("启动 FFmpeg 失败: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail = stderr
            .lines()
            .rev()
            .take(6)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" | ");
        let detail = if tail.trim().is_empty() {
            format!("FFmpeg 退出码 {}", output.status)
        } else {
            tail
        };
        return Err(AppError::Conversion(format!("FFmpeg 处理失败: {}", detail)));
    }

    Ok(())
}

fn ffprobe_binary_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "ffprobe.exe"
    } else {
        "ffprobe"
    }
}

async fn probe_video_dimensions(ffmpeg_path: &Path, video_playlist: &Path) -> Option<(u32, u32)> {
    let sibling_ffprobe = ffmpeg_path.with_file_name(ffprobe_binary_name());
    if let Some(dimensions) =
        run_ffprobe_dimensions(sibling_ffprobe.as_os_str(), video_playlist).await
    {
        return Some(dimensions);
    }

    run_ffprobe_dimensions(OsStr::new("ffprobe"), video_playlist).await
}

async fn run_ffprobe_json(ffmpeg_path: &Path, input_path: &Path) -> Result<String, AppError> {
    let sibling_ffprobe = ffmpeg_path.with_file_name(ffprobe_binary_name());

    if let Ok(result) = run_ffprobe_json_command(sibling_ffprobe.as_os_str(), input_path).await {
        return Ok(result);
    }

    run_ffprobe_json_command(OsStr::new("ffprobe"), input_path).await
}

async fn run_ffprobe_json_command(
    ffprobe_command: &OsStr,
    input_path: &Path,
) -> Result<String, AppError> {
    let mut command = tokio::process::Command::new(ffprobe_command);
    configure_background_command(&mut command);
    let output = command
        .args([
            "-v",
            "error",
            "-show_format",
            "-show_streams",
            "-of",
            "json",
        ])
        .arg(input_path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| AppError::Conversion(format!("启动 ffprobe 失败: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        return Err(AppError::Conversion(if detail.is_empty() {
            format!("ffprobe 退出码 {}", output.status)
        } else {
            format!("ffprobe 处理失败: {}", detail)
        }));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        return Err(AppError::Conversion(
            "ffprobe 未返回可用的媒体信息".to_string(),
        ));
    }

    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| AppError::Conversion(format!("解析 ffprobe 输出失败: {}", e)))?;
    serde_json::to_string_pretty(&parsed)
        .map_err(|e| AppError::Conversion(format!("格式化 ffprobe 输出失败: {}", e)))
}

async fn run_ffprobe_dimensions(
    ffprobe_command: &OsStr,
    video_playlist: &Path,
) -> Option<(u32, u32)> {
    let mut command = tokio::process::Command::new(ffprobe_command);
    configure_background_command(&mut command);
    let output = command
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0:s=x",
        ])
        .arg(video_playlist)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    parse_video_dimensions(String::from_utf8_lossy(&output.stdout).trim())
}

async fn export_hls_subtitle_playlist_to_srt(
    subtitle_playlist: &Path,
    subtitle_srt_path: &Path,
) -> Result<(), AppError> {
    let cues = collect_hls_subtitle_cues(subtitle_playlist).await?;
    if cues.is_empty() {
        return Err(AppError::Conversion(
            "字幕内容为空，无法生成修正后的字幕文件".to_string(),
        ));
    }

    tokio::fs::write(subtitle_srt_path, render_srt_content(&cues)).await?;
    Ok(())
}

async fn collect_hls_subtitle_cues(subtitle_playlist: &Path) -> Result<Vec<SubtitleCue>, AppError> {
    let playlist_content = tokio::fs::read(subtitle_playlist).await?;
    let playlist = m3u8_rs::parse_playlist_res(&playlist_content).map_err(|_| {
        AppError::InvalidInput("字幕播放列表格式无效，无法重建字幕时间轴".to_string())
    })?;
    let m3u8_rs::Playlist::MediaPlaylist(media_playlist) = playlist else {
        return Err(AppError::InvalidInput(
            "字幕播放列表不是有效的媒体列表，无法重建字幕时间轴".to_string(),
        ));
    };
    let parent_dir = subtitle_playlist.parent().ok_or_else(|| {
        AppError::InvalidInput("字幕播放列表路径无效，无法重建字幕时间轴".to_string())
    })?;

    let mut cues = Vec::new();
    let mut leading_empty_duration_ms = 0u64;
    let mut encountered_non_empty_segment = false;
    for segment in media_playlist.segments {
        let segment_path = parent_dir.join(segment.uri);
        let segment_content = tokio::fs::read_to_string(&segment_path).await?;
        let segment_cues = parse_webvtt_cues(&segment_content);
        if !encountered_non_empty_segment && segment_cues.is_empty() {
            leading_empty_duration_ms += (segment.duration * 1000.0).round() as u64;
        } else {
            encountered_non_empty_segment = true;
        }
        cues.extend(segment_cues);
    }

    Ok(normalize_subtitle_cues(apply_leading_empty_offset(
        cues,
        leading_empty_duration_ms,
    )))
}

fn parse_webvtt_cues(content: &str) -> Vec<SubtitleCue> {
    let normalized = content.replace("\r\n", "\n");
    let mut cues = Vec::new();

    for raw_block in normalized.split("\n\n") {
        let block = raw_block.trim();
        if block.is_empty() || block.eq_ignore_ascii_case("WEBVTT") {
            continue;
        }

        let lines = block.lines().collect::<Vec<_>>();
        let timestamp_index = lines.iter().position(|line| line.contains("-->"));
        let Some(timestamp_index) = timestamp_index else {
            continue;
        };

        let Some((start_ms, end_ms)) = parse_webvtt_time_range(lines[timestamp_index]) else {
            continue;
        };
        let text = lines
            .iter()
            .skip(timestamp_index + 1)
            .copied()
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();
        let normalized_text = normalize_subtitle_text(&text);
        if normalized_text.is_empty() || normalized_text == "WEBVTT" {
            continue;
        }

        cues.push(SubtitleCue {
            start_ms,
            end_ms,
            text,
        });
    }

    cues
}

fn parse_webvtt_time_range(line: &str) -> Option<(u64, u64)> {
    let (start, end) = line.split_once("-->")?;
    let start_ms = parse_webvtt_timestamp(start.trim())?;
    let end_ms = parse_webvtt_timestamp(end.split_whitespace().next()?.trim())?;
    Some((start_ms, end_ms))
}

fn parse_webvtt_timestamp(raw: &str) -> Option<u64> {
    let parts = raw.trim().split(':').collect::<Vec<_>>();
    let (hours, minutes, seconds) = match parts.as_slice() {
        [minutes, seconds] => (0u64, minutes.parse::<u64>().ok()?, *seconds),
        [hours, minutes, seconds] => (
            hours.parse::<u64>().ok()?,
            minutes.parse::<u64>().ok()?,
            *seconds,
        ),
        _ => return None,
    };
    let (seconds, millis) = seconds.split_once('.')?;
    let seconds = seconds.parse::<u64>().ok()?;
    let millis = millis.parse::<u64>().ok()?;

    Some((((hours * 60 + minutes) * 60) + seconds) * 1000 + millis)
}

fn normalize_subtitle_cues(mut cues: Vec<SubtitleCue>) -> Vec<SubtitleCue> {
    cues.sort_by(|left, right| {
        left.start_ms
            .cmp(&right.start_ms)
            .then(left.end_ms.cmp(&right.end_ms))
            .then(left.text.cmp(&right.text))
    });

    let mut seen = HashSet::new();
    cues.retain(|cue| seen.insert((cue.start_ms, cue.end_ms, normalize_subtitle_text(&cue.text))));
    cues
}

fn apply_leading_empty_offset(
    mut cues: Vec<SubtitleCue>,
    leading_empty_duration_ms: u64,
) -> Vec<SubtitleCue> {
    let Some(first_start_ms) = cues.first().map(|cue| cue.start_ms) else {
        return cues;
    };

    if leading_empty_duration_ms == 0 {
        return cues;
    }

    // Some segmented WebVTT sources keep a blank intro segment outside the cue timeline.
    // When the first real cue starts well before that blank duration, shift the whole
    // subtitle timeline forward by the blank intro duration.
    if first_start_ms + 250 >= leading_empty_duration_ms {
        return cues;
    }

    for cue in &mut cues {
        cue.start_ms = cue.start_ms.saturating_add(leading_empty_duration_ms);
        cue.end_ms = cue.end_ms.saturating_add(leading_empty_duration_ms);
    }

    cues
}

fn normalize_subtitle_text(text: &str) -> String {
    text.replace(['\u{feff}', '\u{a0}'], "").trim().to_string()
}

fn render_srt_content(cues: &[SubtitleCue]) -> String {
    let mut output = String::new();

    for (index, cue) in cues.iter().enumerate() {
        output.push_str(&(index + 1).to_string());
        output.push('\n');
        output.push_str(&format!(
            "{} --> {}\n",
            format_srt_timestamp(cue.start_ms),
            format_srt_timestamp(cue.end_ms)
        ));
        output.push_str(cue.text.trim_end());
        output.push_str("\n\n");
    }

    output
}

fn format_srt_timestamp(total_ms: u64) -> String {
    let hours = total_ms / 3_600_000;
    let minutes = (total_ms % 3_600_000) / 60_000;
    let seconds = (total_ms % 60_000) / 1_000;
    let millis = total_ms % 1_000;

    format!("{:02}:{:02}:{:02},{:03}", hours, minutes, seconds, millis)
}

fn parse_video_dimensions(raw: &str) -> Option<(u32, u32)> {
    let (width, height) = raw.trim().split_once('x')?;
    let width = width.trim().parse::<u32>().ok()?;
    let height = height.trim().parse::<u32>().ok()?;
    if width == 0 || height == 0 {
        return None;
    }

    Some((width, height))
}

fn calculate_subtitle_track_size((video_width, video_height): (u32, u32)) -> (u32, u32) {
    let scaled_height = video_height.saturating_mul(SUBTITLE_TRACK_HEIGHT_RATIO_NUMERATOR)
        / SUBTITLE_TRACK_HEIGHT_RATIO_DENOMINATOR;
    let subtitle_height = scaled_height
        .max(MIN_SUBTITLE_TRACK_HEIGHT)
        .min(video_height);
    (video_width, subtitle_height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn build_multi_track_hls_to_mp4_args_maps_audio_and_subtitle_inputs() {
        let args = build_multi_track_hls_to_mp4_args(
            &PathBuf::from("video/index.m3u8"),
            Some(Path::new("audio/index.m3u8")),
            Some(Path::new("subtitle/subtitle.srt")),
            Some((1920, 238)),
            &PathBuf::from("output.mp4"),
        );

        assert!(args.windows(2).any(|window| window == ["-map", "0:v:0"]));
        assert!(args.windows(2).any(|window| window == ["-map", "1:a:0"]));
        assert!(args.windows(2).any(|window| window == ["-map", "2:s:0"]));
        assert!(args.windows(2).any(|window| window == ["-c:s", "mov_text"]));
        assert!(args
            .windows(2)
            .any(|window| window == ["-i", "subtitle/subtitle.srt"]));
        assert!(args
            .windows(2)
            .any(|window| window == ["-s:s:0", "1920x238"]));
        assert!(args
            .windows(2)
            .any(|window| window == ["-height:s:0", "238"]));
    }

    #[test]
    fn build_multi_track_hls_to_mp4_args_skips_missing_audio_input() {
        let args = build_multi_track_hls_to_mp4_args(
            &PathBuf::from("video/index.m3u8"),
            None,
            Some(Path::new("subtitle/subtitle.srt")),
            Some((1280, 128)),
            &PathBuf::from("output.mp4"),
        );

        assert!(args.windows(2).any(|window| window == ["-map", "0:v:0"]));
        assert!(args.windows(2).any(|window| window == ["-map", "1:s:0"]));
        assert!(!args.windows(2).any(|window| window == ["-map", "1:a:0"]));
        assert!(args
            .windows(2)
            .any(|window| window == ["-s:s:0", "1280x128"]));
    }

    #[test]
    fn calculate_merge_video_output_size_rounds_up_odd_values() {
        let size = calculate_merge_video_output_size(&[
            MergeVideoInputInfo {
                width: 1279,
                height: 719,
                video_codec: Some("h264".to_string()),
                video_frame_rate: Some("30/1".to_string()),
                has_audio: true,
                audio_codec: Some("aac".to_string()),
                audio_sample_rate: Some("48000".to_string()),
                audio_channels: Some(2),
            },
            MergeVideoInputInfo {
                width: 640,
                height: 360,
                video_codec: Some("h264".to_string()),
                video_frame_rate: Some("30/1".to_string()),
                has_audio: false,
                audio_codec: None,
                audio_sample_rate: None,
                audio_channels: None,
            },
        ])
        .expect("size");

        assert_eq!(size, (1280, 720));
    }

    #[test]
    fn build_merge_video_normalize_args_adds_silent_audio_when_missing() {
        let args = build_merge_video_normalize_args(
            Path::new("clip1.mov"),
            Path::new("temp/clip1.mp4"),
            (1280, 720),
            &MergeVideoInputInfo {
                width: 640,
                height: 360,
                video_codec: Some("h264".to_string()),
                video_frame_rate: Some("30/1".to_string()),
                has_audio: false,
                audio_codec: None,
                audio_sample_rate: None,
                audio_channels: None,
            },
        );

        assert!(args.windows(2).any(|window| window == ["-f", "lavfi"]));
        assert!(args.windows(2).any(|window| window == ["-map", "1:a:0"]));
        assert!(args.iter().any(|item| item == "-shortest"));
    }

    #[test]
    fn build_merge_video_concat_args_uses_concat_demuxer() {
        let args = build_merge_video_concat_args(
            Path::new("/tmp/concat.txt"),
            Path::new("/tmp/output.mp4"),
        );

        assert!(args.windows(2).any(|window| window == ["-f", "concat"]));
        assert!(args.windows(2).any(|window| window == ["-safe", "0"]));
        assert!(args.windows(2).any(|window| window == ["-c", "copy"]));
    }

    #[test]
    fn validate_fast_merge_inputs_accepts_matching_h264_aac_inputs() {
        let result = validate_fast_merge_inputs(&[
            MergeVideoInputInfo {
                width: 1280,
                height: 720,
                video_codec: Some("h264".to_string()),
                video_frame_rate: Some("30/1".to_string()),
                has_audio: true,
                audio_codec: Some("aac".to_string()),
                audio_sample_rate: Some("48000".to_string()),
                audio_channels: Some(2),
            },
            MergeVideoInputInfo {
                width: 1280,
                height: 720,
                video_codec: Some("h264".to_string()),
                video_frame_rate: Some("30/1".to_string()),
                has_audio: true,
                audio_codec: Some("aac".to_string()),
                audio_sample_rate: Some("48000".to_string()),
                audio_channels: Some(2),
            },
        ]);

        assert!(result.is_ok());
    }

    #[test]
    fn validate_fast_merge_inputs_rejects_mixed_sizes() {
        let result = validate_fast_merge_inputs(&[
            MergeVideoInputInfo {
                width: 1280,
                height: 720,
                video_codec: Some("h264".to_string()),
                video_frame_rate: Some("30/1".to_string()),
                has_audio: true,
                audio_codec: Some("aac".to_string()),
                audio_sample_rate: Some("48000".to_string()),
                audio_channels: Some(2),
            },
            MergeVideoInputInfo {
                width: 1920,
                height: 1080,
                video_codec: Some("h264".to_string()),
                video_frame_rate: Some("30/1".to_string()),
                has_audio: true,
                audio_codec: Some("aac".to_string()),
                audio_sample_rate: Some("48000".to_string()),
                audio_channels: Some(2),
            },
        ]);

        assert!(matches!(result, Err(AppError::InvalidInput(_))));
    }

    #[test]
    fn build_merge_video_fast_remux_args_copies_all_streams() {
        let args =
            build_merge_video_fast_remux_args(Path::new("clip1.mkv"), Path::new("temp/clip1.mp4"));

        assert!(args.windows(2).any(|window| window == ["-map", "0"]));
        assert!(args.windows(2).any(|window| window == ["-c", "copy"]));
        assert!(args
            .windows(2)
            .any(|window| window == ["-movflags", "+faststart"]));
    }

    #[test]
    fn parse_webvtt_cues_reads_timestamped_text_blocks() {
        let cues = parse_webvtt_cues("WEBVTT\n\n00:00:03.700 --> 00:00:05.068\nBut guess what?\n");

        assert_eq!(
            cues,
            vec![SubtitleCue {
                start_ms: 3_700,
                end_ms: 5_068,
                text: "But guess what?".to_string(),
            }]
        );
    }

    #[test]
    fn normalize_subtitle_cues_deduplicates_overlap_segments() {
        let cues = normalize_subtitle_cues(vec![
            SubtitleCue {
                start_ms: 28_992,
                end_ms: 32_829,
                text: "but I've applied\nfor about roughly three decades".to_string(),
            },
            SubtitleCue {
                start_ms: 697,
                end_ms: 3_667,
                text: "I am billed as the world's\ngreatest mind reader.".to_string(),
            },
            SubtitleCue {
                start_ms: 28_992,
                end_ms: 32_829,
                text: "but I've applied\nfor about roughly three decades".to_string(),
            },
        ]);

        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start_ms, 697);
        assert_eq!(cues[1].start_ms, 28_992);
    }

    #[test]
    fn render_srt_content_formats_cues_in_order() {
        let srt = render_srt_content(&[
            SubtitleCue {
                start_ms: 697,
                end_ms: 3_667,
                text: "I am billed as the world's\ngreatest mind reader.".to_string(),
            },
            SubtitleCue {
                start_ms: 3_700,
                end_ms: 5_068,
                text: "But guess what?".to_string(),
            },
        ]);

        assert_eq!(
            srt,
            "1\n00:00:00,697 --> 00:00:03,667\nI am billed as the world's\ngreatest mind reader.\n\n2\n00:00:03,700 --> 00:00:05,068\nBut guess what?\n\n"
        );
    }

    #[test]
    fn apply_leading_empty_offset_shifts_timeline_forward() {
        let cues = apply_leading_empty_offset(
            vec![
                SubtitleCue {
                    start_ms: 697,
                    end_ms: 3_667,
                    text: "I am billed as the world's\ngreatest mind reader.".to_string(),
                },
                SubtitleCue {
                    start_ms: 28_992,
                    end_ms: 32_829,
                    text: "but I've applied\nfor about roughly three decades".to_string(),
                },
            ],
            3_504,
        );

        assert_eq!(cues[0].start_ms, 4_201);
        assert_eq!(cues[1].start_ms, 32_496);
    }

    #[test]
    fn apply_leading_empty_offset_skips_when_shift_would_be_redundant() {
        let cues = apply_leading_empty_offset(
            vec![SubtitleCue {
                start_ms: 4_000,
                end_ms: 5_000,
                text: "Hello".to_string(),
            }],
            3_504,
        );

        assert_eq!(cues[0].start_ms, 4_000);
    }

    #[test]
    fn parse_video_dimensions_supports_ffprobe_csv_output() {
        assert_eq!(parse_video_dimensions("1920x1080"), Some((1920, 1080)));
        assert_eq!(parse_video_dimensions(" 1280x720 "), Some((1280, 720)));
        assert_eq!(parse_video_dimensions(""), None);
    }

    #[test]
    fn calculate_subtitle_track_size_uses_larger_box_height() {
        assert_eq!(calculate_subtitle_track_size((1920, 1080)), (1920, 237));
        assert_eq!(calculate_subtitle_track_size((1280, 360)), (1280, 128));
    }
}
