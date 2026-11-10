use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::models::common::InstalledMod;
use crate::models::content::ModProfile;
use crate::models::instance::MinecraftInstance;

use super::mods as instance_mods;

fn profiles_path(instance: &MinecraftInstance) -> PathBuf {
    Path::new(&instance.game_dir).join("arcane_mod_profiles.json")
}

pub async fn list_profiles(instance: &MinecraftInstance) -> AppResult<Vec<ModProfile>> {
    match tokio::fs::read(profiles_path(instance)).await {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).unwrap_or_default()),
        Err(_) => Ok(Vec::new()),
    }
}

async fn save_profiles(instance: &MinecraftInstance, profiles: &[ModProfile]) -> AppResult<()> {
    let bytes = serde_json::to_vec_pretty(profiles)?;
    tokio::fs::write(profiles_path(instance), bytes).await?;
    Ok(())
}

pub async fn create_profile(
    instance: &MinecraftInstance,
    name: String,
    mod_ids: Vec<String>,
) -> AppResult<ModProfile> {
    let mut profiles = list_profiles(instance).await?;
    let profile = ModProfile {
        id: Uuid::new_v4().to_string(),
        name,
        mod_ids,
    };
    profiles.push(profile.clone());
    save_profiles(instance, &profiles).await?;
    Ok(profile)
}

pub async fn update_profile(instance: &MinecraftInstance, updated: ModProfile) -> AppResult<ModProfile> {
    let mut profiles = list_profiles(instance).await?;
    let Some(existing) = profiles.iter_mut().find(|p| p.id == updated.id) else {
        return Err(AppError::not_found("This mod profile"));
    };
    *existing = updated.clone();
    save_profiles(instance, &profiles).await?;
    Ok(updated)
}

pub async fn delete_profile(instance: &MinecraftInstance, profile_id: &str) -> AppResult<()> {
    let mut profiles = list_profiles(instance).await?;
    profiles.retain(|p| p.id != profile_id);
    save_profiles(instance, &profiles).await
}

/// Enables or disables every mod in a profile in one action. This is a
/// batch toggle, not an exclusive loadout switch — mods outside the profile
/// are never touched.
pub async fn apply_profile(
    instance: &MinecraftInstance,
    profile_id: &str,
    enabled: bool,
) -> AppResult<Vec<InstalledMod>> {
    let profiles = list_profiles(instance).await?;
    let profile = profiles
        .iter()
        .find(|p| p.id == profile_id)
        .ok_or_else(|| AppError::not_found("This mod profile"))?;
    for mod_id in &profile.mod_ids {
        let _ = instance_mods::set_enabled(instance, mod_id, enabled).await;
    }
    instance_mods::list_installed(instance).await
}
