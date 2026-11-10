
use crate::error::AppError;
use crate::models::common::{InstalledMod, ProjectType};
use crate::models::content::{ModProfile, ScreenshotInfo, WorldInfo};
use crate::services::instance::collection_import::{self, CollectionImportOutcome, CollectionPreview};
use crate::services::instance::mod_identify::{self, ScanResult};
use crate::services::instance::{mod_profiles, mods as instance_mods, resourcepack_order, screenshots, worlds};
use crate::state::AppState;

pub async fn list_worlds(state: &AppState, instance_id: String) -> Result<Vec<WorldInfo>, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    worlds::list_worlds(&instance).await
}

pub async fn delete_world(
    state: &AppState,
    instance_id: String,
    folder_name: String,
) -> Result<(), AppError> {
    let instance = state.instances.get(&instance_id).await?;
    worlds::delete_world(&instance, &folder_name).await
}

pub async fn backup_world(
    state: &AppState,
    instance_id: String,
    folder_name: String,
) -> Result<String, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    worlds::backup_world(&instance, &folder_name).await
}

pub async fn list_screenshots(
    state: &AppState,
    instance_id: String,
) -> Result<Vec<ScreenshotInfo>, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    screenshots::list_screenshots(&instance).await
}

pub async fn delete_screenshot(
    state: &AppState,
    instance_id: String,
    filename: String,
) -> Result<(), AppError> {
    let instance = state.instances.get(&instance_id).await?;
    screenshots::delete_screenshot(&instance, &filename).await
}

pub async fn list_mod_profiles(
    state: &AppState,
    instance_id: String,
) -> Result<Vec<ModProfile>, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    mod_profiles::list_profiles(&instance).await
}

pub async fn create_mod_profile(
    state: &AppState,
    instance_id: String,
    name: String,
    mod_ids: Vec<String>,
) -> Result<ModProfile, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    mod_profiles::create_profile(&instance, name, mod_ids).await
}

pub async fn update_mod_profile(
    state: &AppState,
    instance_id: String,
    profile: ModProfile,
) -> Result<ModProfile, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    mod_profiles::update_profile(&instance, profile).await
}

pub async fn delete_mod_profile(
    state: &AppState,
    instance_id: String,
    profile_id: String,
) -> Result<(), AppError> {
    let instance = state.instances.get(&instance_id).await?;
    mod_profiles::delete_profile(&instance, &profile_id).await
}

pub async fn apply_mod_profile(
    state: &AppState,
    instance_id: String,
    profile_id: String,
    enabled: bool,
) -> Result<Vec<InstalledMod>, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    mod_profiles::apply_profile(&instance, &profile_id, enabled).await
}

/// Resource pack order, lowest to highest priority (matching options.txt's
/// own convention — the caller is expected to reverse this for display if
/// it wants "top of list = highest priority").
pub async fn get_resourcepack_order(state: &AppState, instance_id: String) -> Result<Vec<String>, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    let installed = instance_mods::list_installed(&instance).await?;
    let enabled_files: Vec<String> = installed
        .iter()
        .filter(|m| m.project_type == ProjectType::ResourcePack && m.enabled)
        .map(|m| m.file_name.clone())
        .collect();
    Ok(resourcepack_order::get_order(&instance, &enabled_files).await)
}

pub async fn set_resourcepack_order(
    state: &AppState,
    instance_id: String,
    ordered_files: Vec<String>,
) -> Result<(), AppError> {
    let instance = state.instances.get(&instance_id).await?;
    resourcepack_order::set_order(&instance, ordered_files).await
}

/// Accepts either a bare Modrinth collection id or a full collection URL.
pub async fn preview_modrinth_collection(
    state: &AppState,
    collection: String,
) -> Result<CollectionPreview, AppError> {
    collection_import::preview_collection(&state.providers, &collection).await
}

pub async fn import_modrinth_collection(
    state: &AppState,
    instance_id: String,
    collection: String,
    selected_project_ids: Option<Vec<String>>,
) -> Result<CollectionImportOutcome, AppError> {
    let auto_install_required = state.settings.get().await.auto_install_required_dependencies;
    collection_import::import_collection(
        &state.instances,
        &state.providers,
        &state.downloads,
        &instance_id,
        &collection,
        selected_project_ids.as_deref(),
        auto_install_required,
    )
    .await
}

/// Identifies a file dropped straight onto the launcher (bytes, no
/// filesystem path) via its content hash and, if it's a real Modrinth mod,
/// installs and tracks it properly.
pub async fn identify_mod_bytes(
    state: &AppState,
    instance_id: String,
    bytes: Vec<u8>,
) -> Result<InstalledMod, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    mod_identify::identify_and_track_bytes(&instance, &state.providers, bytes).await
}

/// Same as `identify_mod_bytes`, but for a file picked via a native file
/// dialog (a path, not in-memory bytes) — the frontend has no filesystem
/// access, so this reads the file server-side.
pub async fn identify_mod_file(
    state: &AppState,
    instance_id: String,
    file_path: String,
) -> Result<InstalledMod, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    let bytes = tokio::fs::read(&file_path)
        .await
        .map_err(|e| AppError::new("Couldn't read that file.", format!("{e} ({file_path})")))?;
    mod_identify::identify_and_track_bytes(&instance, &state.providers, bytes).await
}

/// Scans an instance's content folders for files the tracker doesn't know
/// about yet and identifies as many as possible via Modrinth.
pub async fn scan_untracked_mods(state: &AppState, instance_id: String) -> Result<ScanResult, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    mod_identify::scan_untracked(&instance, &state.providers).await
}
