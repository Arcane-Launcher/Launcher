use crate::error::AppError;
use crate::models::content::{DiscoveredInstance, ImportSource};
use crate::models::instance::{CreateInstanceRequest, MinecraftInstance};
use crate::services::import;
use crate::state::AppState;

pub async fn scan_for_instances(
    source: ImportSource,
    root_path: Option<String>,
) -> Result<Vec<DiscoveredInstance>, AppError> {
    Ok(import::scan(source, root_path.as_deref()))
}

pub async fn import_discovered_instance(
    state: &AppState,
    discovered: DiscoveredInstance,
) -> Result<MinecraftInstance, AppError> {
    let instance = state
        .instances
        .clone_external(
            CreateInstanceRequest {
                name: discovered.name,
                minecraft_version: discovered.minecraft_version,
                loader: discovered.loader,
                loader_version: discovered.loader_version,
                existing_game_dir: None,
                java_path: None,
            },
            discovered.game_dir,
        )
        .await?;
    let _ = import::sync_mods_from_disk(&instance).await;
    Ok(instance)
}
