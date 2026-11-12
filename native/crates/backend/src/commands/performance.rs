use crate::error::AppError;
use crate::services::instance::performance::{self, BoostItem, BoostOutcome};
use crate::state::AppState;

/// The performance mods that fit this instance and whether each is already
/// there, can be installed, or has no release for it.
pub async fn preview_performance_boost(
    state: &AppState,
    instance_id: String,
) -> Result<Vec<BoostItem>, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    performance::preview(&instance, &state.providers).await
}

pub async fn apply_performance_boost(
    state: &AppState,
    instance_id: String,
    project_ids: Vec<String>,
) -> Result<BoostOutcome, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    performance::apply(
        &instance,
        &project_ids,
        &state.instances,
        &state.providers,
        &state.downloads,
    )
    .await
}
