mod commands;
mod downloader;
mod error;
mod ffmpeg;
mod fix_path;
mod live_recorder;
mod models;
mod persistence;
mod playback;
mod preview;
mod remux;
mod state;
mod update;

use std::collections::HashMap;
use tauri::Manager;
use tauri_plugin_deep_link::DeepLinkExt;

use crate::models::{DownloadId, DownloadTask};
use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 修复 macOS/Linux GUI 应用不继承用户 shell PATH 的问题，
    // 使通过包管理器（如 Homebrew）安装的 ffmpeg 等命令可被检测到。
    let _ = fix_path::fix();

    let download_dir = dirs::download_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap().join("Downloads"))
        .to_string_lossy()
        .to_string();

    let app_state = AppState::new(download_dir);

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                if window.is_minimized().unwrap_or(false) {
                    let _ = window.unminimize();
                }
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(app_state)
        .on_window_event(|window, event| {
            let tauri::WindowEvent::CloseRequested { api, .. } = event else {
                return;
            };

            if let Some(token) = preview::token_from_window_label(window.label()).map(str::to_owned)
            {
                api.prevent_close();
                let window = window.clone();
                let app_handle = window.app_handle().clone();
                tauri::async_runtime::spawn(async move {
                    let _ = window.hide();
                    let _ = window.destroy();

                    let state = app_handle.state::<AppState>();
                    preview::close_session(&state, &token).await;
                });
                return;
            }

            let Some(task_id) =
                playback::task_id_from_window_label(window.label()).map(str::to_owned)
            else {
                return;
            };

            api.prevent_close();
            let window = window.clone();
            let app_handle = window.app_handle().clone();
            tauri::async_runtime::spawn(async move {
                let _ = window.hide();
                let _ = window.destroy();

                let state = app_handle.state::<AppState>();
                playback::remove_playback_session(&state.playback_sessions, &task_id).await;
                commands::maybe_cleanup_completed_temp_dir(&app_handle, &state, &task_id).await;
            });
        })
        .setup(|app| {
            #[cfg(any(windows, target_os = "linux"))]
            app.deep_link().register_all()?;

            let state = app.state::<AppState>();
            let playback_server = tauri::async_runtime::block_on(playback::start_playback_server(
                state.downloads.clone(),
                state.playback_sessions.clone(),
                state.download_priorities.clone(),
            ))?;
            tauri::async_runtime::block_on(async {
                let mut playback_server_state = state.playback_server.write().await;
                *playback_server_state = Some(playback_server);
            });

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let settings = persistence::load_settings(&handle).await;
                let state = handle.state::<AppState>();
                if let Some(default_download_dir) = settings.default_download_dir {
                    let mut dir = state.default_download_dir.lock().await;
                    *dir = default_download_dir;
                }
                {
                    let mut proxy = state.proxy_settings.lock().await;
                    *proxy = settings.proxy;
                }
                {
                    let mut max_concurrent_segments = state.max_concurrent_segments.lock().await;
                    *max_concurrent_segments = settings.download_concurrency;
                }
                state
                    .download_rate_limiter
                    .set_limit_kbps(settings.download_speed_limit_kbps)
                    .await;
                {
                    let mut preview_columns = state.preview_columns.lock().await;
                    *preview_columns = settings.preview_columns;
                }
                {
                    let mut preview_count = state.preview_count.lock().await;
                    *preview_count = settings.preview_count;
                }
                {
                    let mut preview_thumbnail_width =
                        state.preview_thumbnail_width.lock().await;
                    *preview_thumbnail_width = settings.preview_thumbnail_width;
                }
                {
                    let mut preview_jpeg_quality = state.preview_jpeg_quality.lock().await;
                    *preview_jpeg_quality = settings.preview_jpeg_quality;
                }
                {
                    let mut delete_ts_temp_dir_after_download =
                        state.delete_ts_temp_dir_after_download.lock().await;
                    *delete_ts_temp_dir_after_download = settings.delete_ts_temp_dir_after_download;
                }
                {
                    let mut convert_to_mp4 = state.convert_to_mp4.lock().await;
                    *convert_to_mp4 = settings.convert_to_mp4;
                }
                {
                    let mut ffmpeg_enabled = state.ffmpeg_enabled.lock().await;
                    *ffmpeg_enabled = settings.ffmpeg_enabled;
                }
                {
                    let mut ffmpeg_path = state.ffmpeg_path.lock().await;
                    *ffmpeg_path = settings.ffmpeg_path;
                }

                let _ = persistence::migrate_legacy_downloads(&handle).await;
                let saved = persistence::load_active_downloads(&handle)
                    .await
                    .unwrap_or_default();
                let mut downloads: tokio::sync::MutexGuard<'_, HashMap<DownloadId, DownloadTask>> =
                    state.downloads.lock().await;
                for mut task in saved {
                    if matches!(
                        task.status,
                        crate::models::DownloadStatus::Pending
                            | crate::models::DownloadStatus::Downloading
                            | crate::models::DownloadStatus::Merging
                            | crate::models::DownloadStatus::Converting
                    ) {
                        task.status = crate::models::DownloadStatus::Paused;
                        task.speed_bytes_per_sec = 0;
                        task.touch();
                        let _ = persistence::save_task(&handle, &task).await;
                    }
                    downloads.insert(task.id.clone(), task);
                }
                drop(downloads);

                let saved_lives = persistence::load_live_active_tasks(&handle)
                    .await
                    .unwrap_or_default();
                let mut live_records = state.live_records.lock().await;
                for mut task in saved_lives {
                    if matches!(task.status, crate::models::LiveRecordStatus::Recording) {
                        task.status = crate::models::LiveRecordStatus::Paused;
                        task.speed_bytes_per_sec = 0;
                        task.touch();
                        let _ = persistence::save_live_task(&handle, &task).await;
                    }
                    live_records.insert(task.id.clone(), task);
                }
            });

            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::inspect_hls_tracks,
            commands::inspect_dash_tracks,
            commands::create_download,
            commands::pause_download,
            commands::check_resume_download,
            commands::resume_download,
            commands::retry_failed_segments,
            commands::cancel_download,
            commands::get_download_counts,
            commands::get_downloads_page,
            commands::get_download_segment_state,
            commands::get_download_summary,
            commands::remove_download,
            commands::clear_history_downloads,
            commands::get_default_download_dir,
            commands::set_default_download_dir,
            commands::get_app_settings,
            commands::set_proxy_settings,
            commands::set_download_concurrency,
            commands::set_download_speed_limit,
            commands::set_preview_columns,
            commands::set_preview_count,
            commands::set_preview_thumbnail_settings,
            commands::set_download_output_settings,
            commands::open_file_location,
            commands::open_url,
            commands::install_chromium_extension,
            commands::open_chromium_extensions_page,
            commands::install_firefox_extension,
            commands::open_firefox_addons_page,
            commands::merge_ts_files,
            commands::convert_ts_to_mp4_file,
            commands::convert_local_m3u8_to_mp4_file,
            commands::convert_media_file,
            commands::clip_video_file,
            commands::transcode_media_file,
            commands::analyze_media_file,
            commands::merge_video_files,
            commands::convert_multi_track_hls_to_mp4_dir,
            commands::get_ffmpeg_status,
            commands::download_ffmpeg,
            commands::set_ffmpeg_path,
            commands::set_ffmpeg_enabled,
            commands::create_preview_session,
            commands::extract_preview_thumbnails,
            commands::cancel_preview_thumbnails,
            commands::close_preview_session,
            commands::open_download_playback_session,
            commands::prioritize_download_playback_position,
            commands::close_download_playback_session,
            commands::create_live_record,
            commands::pause_live_record,
            commands::resume_live_record,
            commands::stop_live_record,
            commands::cancel_live_record,
            commands::remove_live_record,
            commands::clear_live_history,
            commands::get_live_record_counts,
            commands::get_live_records_page,
            commands::convert_live_hls_to_mp4,
            update::check_for_update,
            update::download_update_installer,
            update::open_update_installer,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
