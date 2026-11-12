use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::AppResult;
use crate::models::common::ProjectType;
use crate::models::instance::MinecraftInstance;
use crate::paths::AppPaths;
use crate::services::fs_utils::dir_size;
use crate::services::instance::mods as instance_mods;

/// A crash report or rotated log left untouched for at least this long is
/// treated as safe to offer for cleanup — long enough that a crash the
/// player still wants to look at (or report to a mod author) isn't
/// silently swept away the moment it happens.
const LOG_MIN_AGE_SECS: u64 = 7 * 24 * 60 * 60;

#[derive(Debug, Clone, Serialize)]
pub struct StorageCategory {
    pub id: String,
    pub label: String,
    pub size_bytes: u64,
    pub cleanable_bytes: u64,
    pub item_count: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageReport {
    pub categories: Vec<StorageCategory>,
    pub total_bytes: u64,
    /// Sum of every category's `cleanable_bytes` — the headline "Clean N"
    /// figure. Includes backups even though those default to unselected in
    /// the preview (the player may still choose to clean them); it is
    /// never silently deleted without the player seeing exactly what's in
    /// it first, via `preview_storage_cleanup`.
    pub cleanable_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CleanableItem {
    pub id: String,
    pub category: String,
    pub label: String,
    pub path: String,
    pub size_bytes: u64,
    pub is_dir: bool,
    /// Categories a player should actively opt into (backups are real,
    /// meaningful snapshots) start unchecked in the UI; everything else
    /// (unused version caches, old logs, provider/network cache) is safe
    /// enough to default to checked.
    pub default_selected: bool,
}

fn count_entries(path: &Path) -> u32 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries.flatten().count() as u32
}

fn age_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Every folder under `versions_meta_dir` that actually holds its own
/// `client.jar` — i.e. a real vanilla version cache, not a loader's merged
/// profile stub (which only ever has a small `version.json`, no jar, and is
/// left alone entirely here since misjudging "in use" for one of those
/// risks nothing but a few KB anyway).
fn scan_old_versions(paths: &AppPaths, instances: &[MinecraftInstance]) -> Vec<CleanableItem> {
    let in_use: std::collections::HashSet<&str> = instances
        .iter()
        .map(|i| i.minecraft_version.as_str())
        .collect();

    let Ok(entries) = std::fs::read_dir(paths.versions_meta_dir()) else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || !path.join("client.jar").exists() {
            continue;
        }
        let version_id = entry.file_name().to_string_lossy().to_string();
        if in_use.contains(version_id.as_str()) {
            continue;
        }
        let size = dir_size(&path);
        if size == 0 {
            continue;
        }
        items.push(CleanableItem {
            id: format!("old_version:{version_id}"),
            category: "old_versions".to_string(),
            label: format!("Minecraft {version_id} (unused)"),
            path: path.to_string_lossy().to_string(),
            size_bytes: size,
            is_dir: true,
            default_selected: true,
        });
    }
    items
}

/// Rotated logs (`*.log.gz`) and old crash reports across every instance —
/// `latest.log`/`debug.log` (still actively written/read) are never
/// touched, and nothing younger than `LOG_MIN_AGE_SECS` is offered.
fn scan_old_logs(instances: &[MinecraftInstance]) -> Vec<CleanableItem> {
    let mut items = Vec::new();
    for instance in instances {
        let mut size = 0u64;
        let mut count = 0u32;

        let logs_dir = Path::new(&instance.game_dir).join("logs");
        if let Ok(entries) = std::fs::read_dir(&logs_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                let Ok(meta) = entry.metadata() else { continue };
                if meta.is_dir() || !name.ends_with(".log.gz") || age_secs(&meta) < LOG_MIN_AGE_SECS
                {
                    continue;
                }
                size += meta.len();
                count += 1;
            }
        }

        let crash_dir = Path::new(&instance.game_dir).join("crash-reports");
        if let Ok(entries) = std::fs::read_dir(&crash_dir) {
            for entry in entries.flatten() {
                let Ok(meta) = entry.metadata() else { continue };
                if meta.is_dir() || age_secs(&meta) < LOG_MIN_AGE_SECS {
                    continue;
                }
                size += meta.len();
                count += 1;
            }
        }

        if size == 0 {
            continue;
        }
        items.push(CleanableItem {
            id: format!("logs:{}", instance.id),
            category: "logs".to_string(),
            label: format!(
                "{} — {count} old log file{}",
                instance.name,
                if count == 1 { "" } else { "s" }
            ),
            // Deleted file-by-file in `clean_storage_items` (not a single
            // directory) since `latest.log`/`debug.log` live alongside
            // these and must survive.
            path: instance.id.clone(),
            size_bytes: size,
            is_dir: false,
            default_selected: true,
        });
    }
    items
}

/// Provider/API metadata cache and leftover download temp files — nothing
/// here is anything other than a cache; everything just gets refetched or
/// re-downloaded on demand.
fn scan_cache(paths: &AppPaths) -> Vec<CleanableItem> {
    let dirs: [(&str, PathBuf); 5] = [
        ("Modrinth cache", paths.modrinth_cache_dir()),
        ("CurseForge cache", paths.curseforge_cache_dir()),
        ("Image cache", paths.image_cache_dir()),
        ("Download temp files", paths.downloads_temp_dir()),
        ("Download cache", paths.downloads_cache_dir()),
    ];
    dirs.into_iter()
        .filter_map(|(label, dir)| {
            let size = dir_size(&dir);
            if size == 0 {
                return None;
            }
            Some(CleanableItem {
                id: format!("cache:{label}"),
                category: "cache".to_string(),
                label: label.to_string(),
                path: dir.to_string_lossy().to_string(),
                size_bytes: size,
                is_dir: true,
                default_selected: true,
            })
        })
        .collect()
}

/// Every world backup `.zip` this launcher has created, one item per file
/// (named with its own timestamp) so a player can keep recent ones and
/// clear out old ones individually rather than an all-or-nothing wipe.
/// Unlike every other category, these default to *unselected* — they're
/// deliberate snapshots, not incidental cache growth.
fn scan_backups(instances: &[MinecraftInstance]) -> Vec<CleanableItem> {
    let mut items = Vec::new();
    for instance in instances {
        let backups_dir = Path::new(&instance.game_dir).join("backups");
        let Ok(entries) = std::fs::read_dir(&backups_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() || path.extension().and_then(|e| e.to_str()) != Some("zip") {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            items.push(CleanableItem {
                id: format!("backup:{}:{name}", instance.id),
                category: "backups".to_string(),
                label: format!("{} — {name}", instance.name),
                path: path.to_string_lossy().to_string(),
                size_bytes: meta.len(),
                is_dir: false,
                default_selected: false,
            });
        }
    }
    items
}

fn sum_content(instances: &[MinecraftInstance], project_type: ProjectType) -> (u64, u32) {
    let mut size = 0u64;
    let mut count = 0u32;
    for instance in instances {
        for dir in [
            instance_mods::content_dir(instance, project_type, None),
            instance_mods::disabled_dir(instance, project_type, None),
        ] {
            size += dir_size(&dir);
            count += count_entries(&dir);
        }
    }
    (size, count)
}

/// All cleanable items across every category — the full picture behind the
/// "Clean N" summary, meant to be shown to the player (with per-item
/// checkboxes) before anything is actually deleted.
pub async fn preview(paths: &AppPaths, instances: &[MinecraftInstance]) -> Vec<CleanableItem> {
    let mut items = scan_old_versions(paths, instances);
    items.extend(scan_old_logs(instances));
    items.extend(scan_cache(paths));
    items.extend(scan_backups(instances));
    items
}

/// The at-a-glance breakdown: informational totals for content that's
/// actively in use (mods/resource packs/shaders — never offered for
/// cleanup here) alongside the real cleanable categories.
pub async fn report(paths: &AppPaths, instances: &[MinecraftInstance]) -> StorageReport {
    let (mods_size, mods_count) = sum_content(instances, ProjectType::Mod);
    let (rp_size, rp_count) = sum_content(instances, ProjectType::ResourcePack);
    let (shader_size, shader_count) = sum_content(instances, ProjectType::Shader);

    let cleanable_items = preview(paths, instances).await;
    let mut by_category: std::collections::HashMap<&str, (u64, u32)> =
        std::collections::HashMap::new();
    for item in &cleanable_items {
        let entry = by_category.entry(item.category.as_str()).or_default();
        entry.0 += item.size_bytes;
        entry.1 += 1;
    }
    let cleanable_of = |cat: &str| by_category.get(cat).copied().unwrap_or_default();

    let (old_versions_bytes, old_versions_count) = cleanable_of("old_versions");
    let (logs_bytes, logs_count) = cleanable_of("logs");
    let (cache_bytes, cache_count) = cleanable_of("cache");
    let (backups_bytes, backups_count) = cleanable_of("backups");

    let categories = vec![
        StorageCategory {
            id: "mods".to_string(),
            label: "Mods".to_string(),
            size_bytes: mods_size,
            cleanable_bytes: 0,
            item_count: mods_count,
        },
        StorageCategory {
            id: "resourcepacks".to_string(),
            label: "Resource Packs".to_string(),
            size_bytes: rp_size,
            cleanable_bytes: 0,
            item_count: rp_count,
        },
        StorageCategory {
            id: "shaders".to_string(),
            label: "Shaders".to_string(),
            size_bytes: shader_size,
            cleanable_bytes: 0,
            item_count: shader_count,
        },
        StorageCategory {
            id: "old_versions".to_string(),
            label: "Old Versions".to_string(),
            size_bytes: old_versions_bytes,
            cleanable_bytes: old_versions_bytes,
            item_count: old_versions_count,
        },
        StorageCategory {
            id: "logs".to_string(),
            label: "Logs".to_string(),
            size_bytes: logs_bytes,
            cleanable_bytes: logs_bytes,
            item_count: logs_count,
        },
        StorageCategory {
            id: "cache".to_string(),
            label: "Cache".to_string(),
            size_bytes: cache_bytes,
            cleanable_bytes: cache_bytes,
            item_count: cache_count,
        },
        StorageCategory {
            id: "backups".to_string(),
            label: "Backups".to_string(),
            size_bytes: backups_bytes,
            cleanable_bytes: backups_bytes,
            item_count: backups_count,
        },
    ];

    let total_bytes = categories.iter().map(|c| c.size_bytes).sum();
    let cleanable_bytes = categories.iter().map(|c| c.cleanable_bytes).sum();

    StorageReport {
        categories,
        total_bytes,
        cleanable_bytes,
    }
}

/// Deletes exactly the item ids the player confirmed (from a `preview()`
/// call) — never anything else, and never re-scans to decide what
/// "should" be deleted. A failure on one item never stops the rest;
/// returns total bytes actually freed.
pub async fn clean(
    paths: &AppPaths,
    instances: &[MinecraftInstance],
    item_ids: &[String],
) -> AppResult<u64> {
    let all_items = preview(paths, instances).await;
    let wanted: std::collections::HashSet<&str> = item_ids.iter().map(|s| s.as_str()).collect();

    let mut freed = 0u64;
    for item in all_items.iter().filter(|i| wanted.contains(i.id.as_str())) {
        if item.category == "logs" {
            // `path` holds the instance id, not a real filesystem path —
            // re-walk that instance's logs/crash-reports and remove just
            // the same old, already-measured files.
            let Some(instance) = instances.iter().find(|i| i.id == item.path) else {
                continue;
            };
            freed += delete_old_logs(instance);
            continue;
        }
        let fs_path = PathBuf::from(&item.path);
        let removed = if item.is_dir {
            std::fs::remove_dir_all(&fs_path).is_ok()
        } else {
            std::fs::remove_file(&fs_path).is_ok()
        };
        if removed {
            freed += item.size_bytes;
        }
    }
    Ok(freed)
}

fn delete_old_logs(instance: &MinecraftInstance) -> u64 {
    let mut freed = 0u64;
    let logs_dir = Path::new(&instance.game_dir).join("logs");
    if let Ok(entries) = std::fs::read_dir(&logs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() || !name.ends_with(".log.gz") || age_secs(&meta) < LOG_MIN_AGE_SECS {
                continue;
            }
            if std::fs::remove_file(&path).is_ok() {
                freed += meta.len();
            }
        }
    }
    let crash_dir = Path::new(&instance.game_dir).join("crash-reports");
    if let Ok(entries) = std::fs::read_dir(&crash_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() || age_secs(&meta) < LOG_MIN_AGE_SECS {
                continue;
            }
            if std::fs::remove_file(&path).is_ok() {
                freed += meta.len();
            }
        }
    }
    freed
}
