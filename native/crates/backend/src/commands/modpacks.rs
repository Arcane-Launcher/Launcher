
use crate::error::AppError;
use crate::models::common::{ModSource, ModVersion};
use crate::models::instance::MinecraftInstance;
use crate::services::modpack::mrpack::{self, ModpackUpdateOutcome};
use crate::state::AppState;

/// Installs a Modrinth/CurseForge modpack by creating a brand-new instance
/// from its manifest, rather than dropping a file into an existing one —
/// modpacks bundle their own Minecraft version, loader, and mod list.
pub async fn install_modpack(
    state: &AppState,
    source: ModSource,
    project_id: String,
    version_id: String,
) -> Result<MinecraftInstance, AppError> {
    mrpack::install_modpack(
        &state.providers,
        &state.downloads,
        &state.instances,
        &state.paths,
        source,
        &project_id,
        &version_id,
    )
    .await
}

pub async fn check_modpack_update(
    state: &AppState,
    instance_id: String,
) -> Result<Option<ModVersion>, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    mrpack::check_for_update(&state.providers, &instance).await
}

pub async fn update_modpack(
    state: &AppState,
    instance_id: String,
    version_id: String,
) -> Result<ModpackUpdateOutcome, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    mrpack::update_modpack(
        &state.providers,
        &state.downloads,
        &state.instances,
        &state.paths,
        instance,
        &version_id,
    )
    .await
}
