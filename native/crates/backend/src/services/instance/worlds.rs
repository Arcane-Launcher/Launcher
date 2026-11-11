use std::io::Read;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde::Deserialize;

use crate::error::{AppError, AppResult};
use crate::models::content::WorldInfo;
use crate::models::instance::MinecraftInstance;
use crate::paths::safe_join;

#[derive(Debug, Deserialize, Default)]
struct LevelDatRoot {
    #[serde(rename = "Data", default)]
    data: LevelData,
}

#[derive(Debug, Deserialize, Default)]
struct LevelData {
    #[serde(rename = "LevelName", default)]
    level_name: Option<String>,
    #[serde(rename = "LastPlayed", default)]
    last_played: Option<i64>,
    #[serde(rename = "Version", default)]
    version: Option<LevelVersion>,
}

#[derive(Debug, Deserialize, Default)]
struct LevelVersion {
    #[serde(rename = "Name", default)]
    name: Option<String>,
}

fn saves_dir(instance: &MinecraftInstance) -> PathBuf {
    Path::new(&instance.game_dir).join("saves")
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else { continue };
        if metadata.is_dir() {
            total += dir_size(&entry.path());
        } else {
            total += metadata.len();
        }
    }
    total
}

fn read_level_dat(world_dir: &Path) -> Option<LevelDatRoot> {
    let bytes = std::fs::read(world_dir.join("level.dat")).ok()?;
    let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut decompressed = Vec::new();
    decoder.read_to_end(&mut decompressed).ok()?;
    fastnbt::from_bytes(&decompressed).ok()
}

fn read_icon(world_dir: &Path) -> Option<String> {
    let bytes = std::fs::read(world_dir.join("icon.png")).ok()?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Some(format!("data:image/png;base64,{encoded}"))
}

/// Lists every world folder under this instance's `saves/`, reading each
/// one's `level.dat` for its real in-game name and last-played time (the
/// game's own data, not just the folder name) and computing on-disk size.
/// Runs the filesystem walk off the async runtime since a heavily-explored
/// world can be several gigabytes.
pub async fn list_worlds(instance: &MinecraftInstance) -> AppResult<Vec<WorldInfo>> {
    let dir = saves_dir(instance);
    tokio::task::spawn_blocking(move || {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut worlds = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let folder_name = entry.file_name().to_string_lossy().to_string();
            let level = read_level_dat(&path);
            let display_name = level
                .as_ref()
                .and_then(|l| l.data.level_name.clone())
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| folder_name.clone());
            let last_played = level
                .as_ref()
                .and_then(|l| l.data.last_played)
                .and_then(|ms| u64::try_from(ms).ok())
                .map(|ms| ms / 1000);
            worlds.push(WorldInfo {
                folder_name,
                display_name,
                last_played,
                size_bytes: dir_size(&path),
                icon_data_uri: read_icon(&path),
            });
        }
        worlds.sort_by_key(|w| std::cmp::Reverse(w.last_played.unwrap_or(0)));
        worlds
    })
    .await
    .map_err(|e| AppError::new("Couldn't read this instance's worlds.", e.to_string()))
}

/// The real Minecraft version (from `level.dat`'s own `Version.Name` NBT
/// field) that last opened this world — `None` if the world or its
/// level.dat can't be read. Used to warn before downgrading past a world
/// that's actually been opened in a newer version, since Minecraft doesn't
/// support opening a world in an older version than it was last saved with.
pub fn last_opened_version(instance: &MinecraftInstance, folder_name: &str) -> Option<String> {
    let path = safe_join(&saves_dir(instance), folder_name)?;
    read_level_dat(&path)?.data.version?.name
}

pub async fn delete_world(instance: &MinecraftInstance, folder_name: &str) -> AppResult<()> {
    let path = safe_join(&saves_dir(instance), folder_name)
        .ok_or_else(|| AppError::new("Invalid world name.", "path traversal blocked"))?;
    if !path.is_dir() {
        return Err(AppError::not_found("This world"));
    }
    tokio::task::spawn_blocking(move || std::fs::remove_dir_all(path))
        .await
        .map_err(|e| AppError::new("Couldn't delete this world.", e.to_string()))??;
    Ok(())
}

/// Zips a world into `<instance>/backups/<name>-<timestamp>.zip` rather than
/// copying it uncompressed, so backups don't silently double the instance's
/// disk usage every time one is made.
pub async fn backup_world(instance: &MinecraftInstance, folder_name: &str) -> AppResult<String> {
    let world_path = safe_join(&saves_dir(instance), folder_name)
        .ok_or_else(|| AppError::new("Invalid world name.", "path traversal blocked"))?;
    if !world_path.is_dir() {
        return Err(AppError::not_found("This world"));
    }

    let backups_dir = Path::new(&instance.game_dir).join("backups");
    std::fs::create_dir_all(&backups_dir)?;
    let timestamp = crate::services::cache::store::now_secs();
    let zip_path = backups_dir.join(format!("{folder_name}-{timestamp}.zip"));
    let zip_path_for_task = zip_path.clone();

    tokio::task::spawn_blocking(move || crate::services::fs_utils::zip_directory(&world_path, &zip_path_for_task))
        .await
        .map_err(|e| AppError::new("Couldn't back up this world.", e.to_string()))??;

    Ok(zip_path.to_string_lossy().to_string())
}

/// The display name of the world the game has open: Minecraft rewrites a
/// world's `session.lock` every time it opens it, so the most recently
/// touched lock (no older than `since_secs`, the session start) marks it.
pub fn world_being_played(game_dir: &Path, since_secs: u64) -> Option<String> {
    let entries = std::fs::read_dir(game_dir.join("saves")).ok()?;
    let mut newest: Option<(u64, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(modified) = std::fs::metadata(path.join("session.lock")).and_then(|m| m.modified()) else { continue };
        let secs = modified.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        if secs + 2 >= since_secs && newest.as_ref().is_none_or(|(n, _)| secs > *n) {
            newest = Some((secs, path));
        }
    }
    let (_, dir) = newest?;
    read_level_dat(&dir)
        .and_then(|l| l.data.level_name)
        .filter(|n| !n.trim().is_empty())
        .or_else(|| dir.file_name().map(|f| f.to_string_lossy().into_owned()))
}
