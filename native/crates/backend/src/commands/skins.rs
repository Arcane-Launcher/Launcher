
use crate::commands::auth::fresh_session;
use crate::error::AppError;
use crate::models::minecraft::SessionType;
use crate::services::minecraft::mineskin::MineSkinRef;
use crate::services::minecraft::skins::{self, AccountSkinInfo, CapeInfo, SavedSkin, SkinInfo, SkinLibrary};
use crate::state::AppState;

async fn require_session(state: &AppState) -> Result<crate::models::minecraft::AuthSession, AppError> {
    fresh_session(state)
        .await?
        .ok_or_else(|| AppError::new("Sign in with a Microsoft account to use this.", "no active session"))
}

pub async fn get_skin(profile_id: String) -> Result<SkinInfo, AppError> {
    skins::get_skin(&profile_id).await
}

pub async fn list_saved_skins(state: &AppState, profile_id: String) -> Result<SkinLibrary, AppError> {
    Ok(skins::list_saved_skins(&state.paths, &profile_id).await)
}

pub async fn add_saved_skin(
    state: &AppState,
    profile_id: String,
    file_path: String,
    variant: String,
    name: Option<String>,
) -> Result<SavedSkin, AppError> {
    let bytes = tokio::fs::read(&file_path)
        .await
        .map_err(|e| AppError::new("Couldn't read that image file.", format!("{e} ({file_path})")))?;
    skins::add_saved_skin(&state.paths, &profile_id, bytes, &variant, name).await
}

/// Same as `add_saved_skin`, but for skins that arrived as in-memory bytes
/// (drag-and-drop from the webview) rather than a filesystem path — the
/// files involved are tiny (a 64x64 PNG), so shipping the bytes directly
/// over IPC avoids the cross-platform hassle of resolving a dropped
/// `File`'s real OS path.
pub async fn add_saved_skin_bytes(
    state: &AppState,
    profile_id: String,
    bytes: Vec<u8>,
    variant: String,
    name: Option<String>,
) -> Result<SavedSkin, AppError> {
    skins::add_saved_skin(&state.paths, &profile_id, bytes, &variant, name).await
}

pub async fn rename_saved_skin(
    state: &AppState,
    profile_id: String,
    skin_id: String,
    name: Option<String>,
) -> Result<(), AppError> {
    skins::rename_saved_skin(&state.paths, &profile_id, &skin_id, name).await
}

pub async fn export_saved_skin(
    state: &AppState,
    profile_id: String,
    skin_id: String,
    dest_path: String,
) -> Result<(), AppError> {
    skins::export_saved_skin(&state.paths, &profile_id, &skin_id, &dest_path).await
}

pub async fn import_skin_by_username(
    state: &AppState,
    profile_id: String,
    username: String,
) -> Result<SavedSkin, AppError> {
    skins::import_skin_by_username(&state.paths, &profile_id, &username).await
}

pub async fn set_skin_favorite(
    state: &AppState,
    profile_id: String,
    skin_id: String,
    favorite: bool,
) -> Result<(), AppError> {
    skins::set_skin_favorite(&state.paths, &profile_id, &skin_id, favorite).await
}

pub async fn add_skin_from_url(
    state: &AppState,
    profile_id: String,
    url: String,
    variant: String,
    name: Option<String>,
) -> Result<SavedSkin, AppError> {
    skins::add_skin_from_url(&state.paths, &profile_id, &url, &variant, name).await
}

pub async fn list_account_skins(state: &AppState) -> Result<Vec<AccountSkinInfo>, AppError> {
    let session = require_session(&state).await?;
    skins::list_account_skins(&session).await
}

pub async fn delete_saved_skin(state: &AppState, profile_id: String, skin_id: String) -> Result<(), AppError> {
    skins::delete_saved_skin(&state.paths, &profile_id, &skin_id).await
}

pub async fn select_saved_skin(state: &AppState, profile_id: String, skin_id: String) -> Result<(), AppError> {
    let session = fresh_session(&state).await?;
    skins::select_saved_skin(&state.paths, &profile_id, &skin_id, session.as_ref()).await
}

/// Uploads a saved skin to MineSkin so servers running SkinsRestorer can
/// apply it. Offline accounts only — Microsoft accounts push their skin to
/// Mojang directly (see `select_saved_skin`).
pub async fn sync_skin_to_mineskin(
    state: &AppState,
    profile_id: String,
    skin_id: String,
    force: Option<bool>,
) -> Result<MineSkinRef, AppError> {
    let session = state.session.get().await;
    if !session.is_some_and(|s| s.session_type == SessionType::Offline) {
        return Err(AppError::new(
            "Uploading to MineSkin is only for offline accounts.",
            "sync_skin_to_mineskin called without an offline session",
        ));
    }
    skins::sync_to_mineskin(&state.paths, &profile_id, &skin_id, force.unwrap_or(false)).await
}

pub async fn clear_active_skin(state: &AppState, profile_id: String) -> Result<(), AppError> {
    let session = fresh_session(&state).await?;
    skins::clear_active_skin(&state.paths, &profile_id, session.as_ref()).await
}

pub async fn list_owned_capes(state: &AppState) -> Result<Vec<CapeInfo>, AppError> {
    let session = require_session(&state).await?;
    skins::list_owned_capes(&session).await
}

pub async fn set_active_cape(state: &AppState, cape_id: String) -> Result<(), AppError> {
    let session = require_session(&state).await?;
    skins::set_active_cape(&session, &cape_id).await
}

pub async fn clear_active_cape(state: &AppState) -> Result<(), AppError> {
    let session = require_session(&state).await?;
    skins::clear_active_cape(&session).await
}
