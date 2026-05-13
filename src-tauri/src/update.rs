use std::{path::PathBuf, time::Duration};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tokio::io::AsyncWriteExt;

use crate::error::AppError;
use crate::state::AppState;

const GITHUB_LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/Liubsyy/M3U8Quicker/releases/latest";
const GITHUB_RELEASES_PAGE: &str = "https://github.com/Liubsyy/M3U8Quicker/releases";
const UPDATE_DOWNLOAD_PROGRESS_EVENT: &str = "update-download-progress";
const UPDATE_HTTP_TIMEOUT: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, Serialize)]
pub struct UpdateAsset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    pub current_version: String,
    pub latest_version: String,
    pub has_update: bool,
    pub release_url: String,
    pub release_notes: String,
    pub published_at: Option<String>,
    pub asset: Option<UpdateAsset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DownloadAssetArg {
    pub name: String,
    pub url: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateDownloadProgress {
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub stage: UpdateDownloadStage,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateDownloadStage {
    Downloading,
    Done,
    Failed,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

#[tauri::command]
pub async fn check_for_update(
    app_handle: AppHandle,
    _state: State<'_, AppState>,
) -> Result<UpdateInfo, AppError> {
    let current_version = app_handle.package_info().version.to_string();
    let user_agent = format!("m3u8quicker/{}", current_version);

    let client = build_update_http_client()?;
    let response = client
        .get(GITHUB_LATEST_RELEASE_API)
        .header("User-Agent", user_agent)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(AppError::Network(format!(
            "GitHub API returned HTTP {}",
            response.status().as_u16()
        )));
    }

    let body_text = response.text().await?;
    let release: GithubRelease = serde_json::from_str(&body_text)
        .map_err(|e| AppError::Network(format!("解析发布信息失败: {}", e)))?;

    let latest_raw = release.tag_name.trim();
    let latest_clean = latest_raw.trim_start_matches(['v', 'V']);

    let has_update = match (
        semver::Version::parse(latest_clean),
        semver::Version::parse(&current_version),
    ) {
        (Ok(latest_ver), Ok(current_ver)) => latest_ver > current_ver,
        _ => latest_clean != current_version,
    };

    let release_url = release
        .html_url
        .clone()
        .unwrap_or_else(|| GITHUB_RELEASES_PAGE.to_string());

    let asset = if has_update {
        pick_asset_for_current_platform(&release.assets)
    } else {
        None
    };

    let release_notes = release
        .body
        .clone()
        .or(release.name.clone())
        .unwrap_or_default();

    Ok(UpdateInfo {
        current_version,
        latest_version: latest_clean.to_string(),
        has_update,
        release_url,
        release_notes,
        published_at: release.published_at,
        asset,
    })
}

fn pick_asset_for_current_platform(assets: &[GithubAsset]) -> Option<UpdateAsset> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let preferences: Vec<(&[&str], &[&str])> = match (os, arch) {
        ("windows", "x86_64") => vec![(&["_windows_x64"], &["_setup.exe", ".exe"])],
        ("windows", "x86") => vec![(&["_windows_x86"], &["_setup.exe", ".exe"])],
        ("macos", "aarch64") => vec![(&["_macos_aarch64"], &[".dmg"])],
        ("macos", "x86_64") => vec![(&["_macos_x64"], &[".dmg"])],
        ("linux", "x86_64") => vec![
            (&["_linux_amd64"], &[".AppImage"]),
            (&["_linux_amd64"], &[".deb"]),
            (&["_linux_amd64"], &[".rpm"]),
        ],
        _ => Vec::new(),
    };

    for (must_contain, allowed_suffixes) in preferences {
        if let Some(asset) = assets.iter().find(|asset| {
            let lowered = asset.name.to_lowercase();
            must_contain
                .iter()
                .all(|needle| lowered.contains(&needle.to_lowercase()))
                && allowed_suffixes
                    .iter()
                    .any(|suffix| lowered.ends_with(&suffix.to_lowercase()))
        }) {
            return Some(UpdateAsset {
                name: asset.name.clone(),
                url: asset.browser_download_url.clone(),
                size: asset.size,
            });
        }
    }

    None
}

#[tauri::command]
pub async fn download_update_installer(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    asset: DownloadAssetArg,
) -> Result<String, AppError> {
    if asset.url.trim().is_empty() || asset.name.trim().is_empty() {
        return Err(AppError::InvalidInput("无效的更新包信息".to_string()));
    }

    let target_dir = resolve_update_download_dir(&state).await;
    tokio::fs::create_dir_all(&target_dir).await?;
    let target_path = target_dir.join(sanitize_asset_filename(&asset.name));

    let client = build_update_http_client()?;

    if let Err(error) = perform_download(&app_handle, client, &asset, &target_path).await {
        let _ = app_handle.emit(
            UPDATE_DOWNLOAD_PROGRESS_EVENT,
            &UpdateDownloadProgress {
                downloaded_bytes: 0,
                total_bytes: 0,
                stage: UpdateDownloadStage::Failed,
            },
        );
        let _ = tokio::fs::remove_file(&target_path).await;
        return Err(error);
    }

    let _ = app_handle.emit(
        UPDATE_DOWNLOAD_PROGRESS_EVENT,
        &UpdateDownloadProgress {
            downloaded_bytes: asset.size,
            total_bytes: asset.size,
            stage: UpdateDownloadStage::Done,
        },
    );

    Ok(target_path.to_string_lossy().to_string())
}

fn build_update_http_client() -> Result<reqwest::Client, AppError> {
    Ok(reqwest::Client::builder()
        .timeout(UPDATE_HTTP_TIMEOUT)
        .build()?)
}

async fn perform_download(
    app_handle: &AppHandle,
    client: reqwest::Client,
    asset: &DownloadAssetArg,
    target_path: &std::path::Path,
) -> Result<(), AppError> {
    let user_agent = format!(
        "m3u8quicker/{}",
        app_handle.package_info().version
    );

    let response = client
        .get(&asset.url)
        .header("User-Agent", user_agent)
        .send()
        .await?
        .error_for_status()?;

    let total_bytes = response.content_length().unwrap_or(asset.size);

    let mut file = tokio::fs::File::create(target_path).await?;
    let mut stream = response.bytes_stream();
    let mut downloaded: u64 = 0;
    let mut last_emit = std::time::Instant::now();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        downloaded = downloaded.saturating_add(chunk.len() as u64);

        if last_emit.elapsed() >= std::time::Duration::from_millis(120) {
            let _ = app_handle.emit(
                UPDATE_DOWNLOAD_PROGRESS_EVENT,
                &UpdateDownloadProgress {
                    downloaded_bytes: downloaded,
                    total_bytes,
                    stage: UpdateDownloadStage::Downloading,
                },
            );
            last_emit = std::time::Instant::now();
        }
    }

    file.flush().await?;
    file.sync_all().await?;
    Ok(())
}

#[tauri::command]
pub async fn open_update_installer(
    app_handle: AppHandle,
    path: String,
) -> Result<(), AppError> {
    let installer_path = PathBuf::from(path.trim());
    if installer_path.as_os_str().is_empty() || !installer_path.exists() {
        return Err(AppError::InvalidInput("安装包不存在".to_string()));
    }

    #[cfg(target_os = "linux")]
    {
        if installer_path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("AppImage"))
            .unwrap_or(false)
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = std::fs::metadata(&installer_path) {
                let mut perms = metadata.permissions();
                perms.set_mode(perms.mode() | 0o111);
                let _ = std::fs::set_permissions(&installer_path, perms);
            }
        }
    }

    open::that(&installer_path).map_err(|e| AppError::Internal(e.to_string()))?;

    let handle_for_exit = app_handle.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        handle_for_exit.exit(0);
    });

    Ok(())
}

async fn resolve_update_download_dir(state: &State<'_, AppState>) -> PathBuf {
    let configured = state.default_download_dir.lock().await.clone();
    let trimmed = configured.trim();
    let base = if trimmed.is_empty() {
        dirs::download_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."))
    } else {
        PathBuf::from(trimmed)
    };
    base.join("M3U8Quicker_update")
}

fn sanitize_asset_filename(name: &str) -> String {
    let trimmed = name.trim();
    let cleaned: String = trimmed
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    if cleaned.is_empty() {
        "m3u8quicker_update.bin".to_string()
    } else {
        cleaned
    }
}
