use crate::error::AppError;
use crate::services::storage::{self, CleanableItem, StorageReport};
use crate::state::AppState;

pub async fn get_storage_report(state: &AppState) -> Result<StorageReport, AppError> {
    let instances = state.instances.list().await;
    Ok(storage::report(&state.paths, &instances).await)
}

pub async fn preview_storage_cleanup(state: &AppState) -> Result<Vec<CleanableItem>, AppError> {
    let instances = state.instances.list().await;
    Ok(storage::preview(&state.paths, &instances).await)
}

pub async fn clean_storage_items(state: &AppState, item_ids: Vec<String>) -> Result<u64, AppError> {
    let instances = state.instances.list().await;
    storage::clean(&state.paths, &instances, &item_ids).await
}
