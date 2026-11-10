
use crate::error::AppError;
use crate::services::background;
use crate::state::AppState;

pub async fn get_background_image(state: &AppState) -> Result<Option<String>, AppError> {
    Ok(background::get_background(&state.paths).await)
}

pub async fn set_background_image(state: &AppState, file_path: String) -> Result<(), AppError> {
    let bytes = tokio::fs::read(&file_path)
        .await
        .map_err(|e| AppError::new("Couldn't read that image file.", format!("{e} ({file_path})")))?;
    background::set_background(&state.paths, bytes).await
}

pub async fn reset_background_image(state: &AppState) -> Result<(), AppError> {
    background::reset_background(&state.paths).await
}
