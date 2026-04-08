mod commands;
mod downloader;
mod error;
mod models;
mod persistence;
mod playback;
mod remux;
mod state;

use std::collections::HashMap;
use tauri::Manager;
use tauri_plugin_deep_link::DeepLinkExt;

use crate::models::{DownloadId, DownloadTask};
use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let download_dir = dirs::download_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap().join("Downloads"))
        .to_string_lossy()
        .to_string();

    let app_state = AppState::new(download_dir);

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(app_state)
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
                    let mut delete_ts_temp_dir_after_download =
                        state.delete_ts_temp_dir_after_download.lock().await;
                    *delete_ts_temp_dir_after_download = settings.delete_ts_temp_dir_after_download;
                }
                {
                    let mut convert_to_mp4 = state.convert_to_mp4.lock().await;
                    *convert_to_mp4 = settings.convert_to_mp4;
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
            });

            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
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
            commands::set_download_output_settings,
            commands::open_file_location,
            commands::install_chromium_extension,
            commands::open_chromium_extensions_page,
            commands::install_firefox_extension,
            commands::open_firefox_addons_page,
            commands::merge_ts_files,
            commands::convert_ts_to_mp4_file,
            commands::open_download_playback_session,
            commands::prioritize_download_playback_position,
            commands::close_download_playback_session,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
