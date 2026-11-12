use std::path::{Path, PathBuf};

use crate::error::{AppError, AppResult};
use crate::models::common::{InstalledMod, ProjectType};
use crate::models::instance::MinecraftInstance;
use crate::paths::{safe_join, sanitize_component};

fn metadata_path(instance: &MinecraftInstance) -> PathBuf {
    Path::new(&instance.game_dir).join("arcane_mods.json")
}

/// Where this project type's enabled files live inside the instance.
/// Every type except `Datapack` installs at a single instance-wide folder
/// (`mods/`, `resourcepacks/`, `shaderpacks/`) and ignores `world`; a
/// datapack installs into that specific world's own `datapacks/` folder
/// instead, so `world` is required for it.
pub fn content_dir(
    instance: &MinecraftInstance,
    project_type: ProjectType,
    world: Option<&str>,
) -> PathBuf {
    if project_type == ProjectType::Datapack {
        let world = world.unwrap_or_default();
        return Path::new(&instance.game_dir)
            .join("saves")
            .join(sanitize_component(world))
            .join("datapacks");
    }
    Path::new(&instance.game_dir).join(project_type.install_dir_name())
}

pub fn disabled_dir(
    instance: &MinecraftInstance,
    project_type: ProjectType,
    world: Option<&str>,
) -> PathBuf {
    content_dir(instance, project_type, world).join(".disabled")
}

/// Back-compat helper for call sites that only ever deal with mods.
pub fn mods_dir(instance: &MinecraftInstance) -> PathBuf {
    content_dir(instance, ProjectType::Mod, None)
}

pub async fn list_installed(instance: &MinecraftInstance) -> AppResult<Vec<InstalledMod>> {
    let path = metadata_path(instance);
    match tokio::fs::read(&path).await {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).unwrap_or_default()),
        Err(_) => Ok(Vec::new()),
    }
}

async fn save_installed(instance: &MinecraftInstance, mods: &[InstalledMod]) -> AppResult<()> {
    let path = metadata_path(instance);
    let bytes = serde_json::to_vec_pretty(mods)?;
    tokio::fs::write(path, bytes).await?;
    Ok(())
}

pub async fn find_by_project(
    instance: &MinecraftInstance,
    project_id: &str,
) -> AppResult<Option<InstalledMod>> {
    let mods = list_installed(instance).await?;
    Ok(mods.into_iter().find(|m| m.project_id == project_id))
}

/// Records a freshly installed mod's metadata. If an entry with the same
/// project id (and, for a datapack, the same world) already exists it is
/// replaced (used by the update flow) so nothing is ever tracked twice
/// under the same project — except a datapack installed into several
/// different worlds, which is legitimately several distinct entries.
pub async fn record_install(
    instance: &MinecraftInstance,
    installed: InstalledMod,
) -> AppResult<()> {
    let mut mods = list_installed(instance).await?;
    mods.retain(|m| !(m.project_id == installed.project_id && m.world == installed.world));
    mods.push(installed);
    save_installed(instance, &mods).await
}

pub async fn set_enabled(
    instance: &MinecraftInstance,
    mod_id: &str,
    enabled: bool,
) -> AppResult<()> {
    let mut mods = list_installed(instance).await?;
    let entry = mods
        .iter_mut()
        .find(|m| m.id == mod_id)
        .ok_or_else(|| AppError::not_found("This mod"))?;

    if entry.enabled == enabled {
        return Ok(());
    }

    let enabled_dir = content_dir(instance, entry.project_type, entry.world.as_deref());
    let disabled_dir = disabled_dir(instance, entry.project_type, entry.world.as_deref());
    let enabled_path = safe_join(&enabled_dir, &entry.file_name)
        .ok_or_else(|| AppError::new("Invalid file name.", "path traversal blocked"))?;
    let disabled_path = safe_join(&disabled_dir, &entry.file_name)
        .ok_or_else(|| AppError::new("Invalid file name.", "path traversal blocked"))?;

    tokio::fs::create_dir_all(&disabled_dir).await?;

    if enabled {
        tokio::fs::rename(&disabled_path, &enabled_path).await?;
    } else {
        tokio::fs::rename(&enabled_path, &disabled_path).await?;
    }

    let project_type = entry.project_type;
    let file_name = entry.file_name.clone();
    entry.enabled = enabled;
    save_installed(instance, &mods).await?;

    // Minecraft warns about "missing" packs still listed in options.txt, so
    // a disabled resource pack has to come out of the active order too —
    // and a re-enabled one needs to go back in to actually apply again.
    if project_type == ProjectType::ResourcePack {
        if enabled {
            let _ = super::resourcepack_order::ensure_entry_present(instance, &file_name).await;
        } else {
            let _ = super::resourcepack_order::remove_entry(instance, &file_name).await;
        }
    }

    Ok(())
}

/// Removes metadata entries whose backing file is missing or corrupted
/// (0 bytes), and collapses duplicate entries that point at the same file.
/// Used by the "Fix Automatically" launch-validation action; it never
/// touches files that are actually present and valid.
pub async fn cleanup_broken_entries(instance: &MinecraftInstance) -> AppResult<Vec<String>> {
    let mods = list_installed(instance).await?;
    let mut removed_names = Vec::new();
    let mut kept = Vec::new();
    let mut seen_files = std::collections::HashSet::new();

    for m in mods {
        let dir = if m.enabled {
            content_dir(instance, m.project_type, m.world.as_deref())
        } else {
            disabled_dir(instance, m.project_type, m.world.as_deref())
        };
        let Some(path) = safe_join(&dir, &m.file_name) else {
            removed_names.push(m.name.clone());
            continue;
        };
        let valid = std::fs::metadata(&path)
            .map(|meta| meta.len() > 0)
            .unwrap_or(false);
        if !valid || !seen_files.insert((m.project_type, m.world.clone(), m.file_name.clone())) {
            removed_names.push(m.name.clone());
            continue;
        }
        kept.push(m);
    }

    if !removed_names.is_empty() {
        save_installed(instance, &kept).await?;
    }
    Ok(removed_names)
}

pub async fn uninstall(instance: &MinecraftInstance, mod_id: &str) -> AppResult<()> {
    let mut mods = list_installed(instance).await?;
    let index = mods
        .iter()
        .position(|m| m.id == mod_id)
        .ok_or_else(|| AppError::not_found("This mod"))?;
    let entry = mods.remove(index);

    let base = if entry.enabled {
        content_dir(instance, entry.project_type, entry.world.as_deref())
    } else {
        disabled_dir(instance, entry.project_type, entry.world.as_deref())
    };
    if let Some(path) = safe_join(&base, &entry.file_name) {
        let _ = tokio::fs::remove_file(path).await;
    }
    if entry.project_type == ProjectType::ResourcePack {
        let _ = super::resourcepack_order::remove_entry(instance, &entry.file_name).await;
    }

    save_installed(instance, &mods).await
}
