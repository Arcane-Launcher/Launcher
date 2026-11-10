
use crate::error::AppError;
use crate::models::download::DownloadTask;
use crate::state::AppState;

pub async fn list_downloads(state: &AppState) -> Result<Vec<DownloadTask>, AppError> {
    Ok(state.downloads.list().await)
}

pub async fn pause_download(state: &AppState, task_id: String) -> Result<(), AppError> {
    state.downloads.pause(&task_id).await;
    Ok(())
}

pub async fn resume_download(state: &AppState, task_id: String) -> Result<(), AppError> {
    state.downloads.resume(&task_id).await;
    Ok(())
}

pub async fn cancel_download(state: &AppState, task_id: String) -> Result<(), AppError> {
    state.downloads.cancel(&task_id).await;
    Ok(())
}
