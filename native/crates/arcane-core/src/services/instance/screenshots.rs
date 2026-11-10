use std::path::{Path, PathBuf};

use crate::error::{AppError, AppResult};
use crate::models::content::ScreenshotInfo;
use crate::models::instance::MinecraftInstance;
use crate::paths::safe_join;

fn screenshots_dir(instance: &MinecraftInstance) -> PathBuf {
    Path::new(&instance.game_dir).join("screenshots")
}

/// Lists this instance's screenshots with real file metadata (taken-at from
/// mtime, actual size on disk). Returns paths rather than image data — the
/// frontend renders them via Tauri's asset protocol (`convertFileSrc`)
/// instead of shipping every PNG through IPC as base64.
pub async fn list_screenshots(instance: &MinecraftInstance) -> AppResult<Vec<ScreenshotInfo>> {
    let dir = screenshots_dir(instance);
    tokio::task::spawn_blocking(move || {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut shots = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let is_png = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("png"));
            if !is_png {
                continue;
            }
            let Ok(metadata) = entry.metadata() else { continue };
            let taken_at = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            shots.push(ScreenshotInfo {
                filename: entry.file_name().to_string_lossy().to_string(),
                path: path.to_string_lossy().to_string(),
                taken_at,
                size_bytes: metadata.len(),
            });
        }
        shots.sort_by_key(|s| std::cmp::Reverse(s.taken_at));
        shots
    })
    .await
    .map_err(|e| AppError::new("Couldn't read this instance's screenshots.", e.to_string()))
}

pub async fn delete_screenshot(instance: &MinecraftInstance, filename: &str) -> AppResult<()> {
    let path = safe_join(&screenshots_dir(instance), filename)
        .ok_or_else(|| AppError::new("Invalid file name.", "path traversal blocked"))?;
    tokio::fs::remove_file(path).await?;
    Ok(())
}
