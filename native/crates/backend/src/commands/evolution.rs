use crate::error::AppError;
use crate::models::common::ModLoaderKind;
use crate::models::instance::MinecraftInstance;
use crate::services::instance::evolution::{
    self, InProgressMarker, MigrationHistoryEntry, MigrationPlan, MigrationReport, SnapshotRecord,
};
use crate::state::AppState;

pub async fn scan_instance_migration(
    state: &AppState,
    instance_id: String,
    target_version: String,
    target_loader: ModLoaderKind,
    target_loader_version: Option<String>,
) -> Result<MigrationPlan, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    evolution::scan(
        &instance,
        &target_version,
        target_loader,
        target_loader_version,
        &state.providers,
        &state.engine.manifest,
    )
    .await
}

pub async fn execute_instance_migration(
    state: &AppState,
    instance_id: String,
    plan: MigrationPlan,
) -> Result<MigrationReport, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    evolution::execute(
        &instance,
        &plan,
        &state.instances,
        &state.providers,
        &state.downloads,
        &state.engine,
        &state.loaders,
        &state.paths,
    )
    .await
}

pub async fn create_instance_snapshot(
    state: &AppState,
    instance_id: String,
) -> Result<SnapshotRecord, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    evolution::create_snapshot(&state.paths, &instance, "Manual backup").await
}

pub async fn list_instance_snapshots(
    state: &AppState,
    instance_id: String,
) -> Result<Vec<SnapshotRecord>, AppError> {
    Ok(evolution::list_snapshots(&state.paths, &instance_id).await)
}

pub async fn delete_instance_snapshot(
    state: &AppState,
    instance_id: String,
    snapshot_id: String,
) -> Result<(), AppError> {
    evolution::delete_snapshot(&state.paths, &instance_id, &snapshot_id).await
}

pub async fn rollback_instance_snapshot(
    state: &AppState,
    instance_id: String,
    snapshot_id: String,
) -> Result<MinecraftInstance, AppError> {
    evolution::rollback(&state.paths, &state.instances, &instance_id, &snapshot_id).await
}

pub async fn list_instance_migration_history(
    state: &AppState,
    instance_id: String,
) -> Result<Vec<MigrationHistoryEntry>, AppError> {
    Ok(evolution::list_history(&state.paths, &instance_id).await)
}

pub async fn check_interrupted_migration(
    state: &AppState,
    instance_id: String,
) -> Result<Option<InProgressMarker>, AppError> {
    Ok(evolution::check_interrupted(&state.paths, &instance_id).await)
}

pub async fn resolve_interrupted_migration(
    state: &AppState,
    instance_id: String,
    restore: bool,
) -> Result<Option<MinecraftInstance>, AppError> {
    evolution::resolve_interrupted(&state.paths, &state.instances, &instance_id, restore).await
}
