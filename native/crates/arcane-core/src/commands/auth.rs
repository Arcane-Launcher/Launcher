
use crate::error::AppError;
use crate::models::minecraft::AuthSession;
use crate::services::minecraft::auth::{self, LoginHost};
use crate::state::AppState;

pub async fn get_session(state: &AppState) -> Result<Option<AuthSession>, AppError> {
    Ok(state.session.get().await)
}

pub async fn list_accounts(state: &AppState) -> Result<Vec<AuthSession>, AppError> {
    Ok(state.session.list().await)
}

pub async fn login_offline(state: &AppState, username: String) -> Result<AuthSession, AppError> {
    let session = auth::create_offline_session(username)?;
    state.session.upsert_and_activate(session.clone()).await?;
    Ok(session)
}

async fn microsoft_client_id(state: &AppState) -> String {
    state
        .settings
        .get()
        .await
        .microsoft_client_id
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| auth::DEFAULT_MICROSOFT_CLIENT_ID.to_string())
}

/// The active account, with its Minecraft access token silently refreshed
/// first if it has expired (or is about to). Use this — not
/// `state.session.get()` — anywhere the token is sent to Mojang, since the
/// token only lives about a day. A refresh that fails leaves the saved
/// session untouched and reports why (usually: sign in again).
pub async fn fresh_session(state: &AppState) -> Result<Option<AuthSession>, AppError> {
    let Some(session) = state.session.get().await else {
        return Ok(None);
    };
    if !auth::needs_refresh(&session) {
        return Ok(Some(session));
    }

    let _guard = state.session.lock_refresh().await;
    // Another caller may have refreshed while we waited for the lock.
    let Some(session) = state.session.get().await else {
        return Ok(None);
    };
    if !auth::needs_refresh(&session) {
        return Ok(Some(session));
    }

    let client_id = microsoft_client_id(state).await;
    let refreshed = auth::refresh_session(&client_id, &session).await?;
    state.session.replace_existing(refreshed.clone()).await?;
    Ok(Some(refreshed))
}

/// Runs interactive Microsoft sign-in through the `LoginHost` — the
/// desktop shell plugs in its embedded sign-in window here (MIGRATION.md
/// §6: the window opens only for OAuth, then closes).
pub async fn login_microsoft(state: &AppState, host: &dyn LoginHost) -> Result<AuthSession, AppError> {
    let client_id = microsoft_client_id(state).await;
    let session = auth::login_with_microsoft(host, &state.events, &client_id).await?;
    state.session.upsert_and_activate(session.clone()).await?;
    Ok(session)
}

/// Switches to an already-saved account instantly, with no
/// re-authentication — the whole point of keeping a saved account list.
pub async fn switch_account(state: &AppState, profile_id: String) -> Result<AuthSession, AppError> {
    state
        .session
        .set_active(&profile_id)
        .await?
        .ok_or_else(|| AppError::not_found("This account"))
}

/// Signs out of the active account without forgetting it, so it still shows
/// up for one-click switching back. Use `remove_account` to fully forget one.
pub async fn logout(state: &AppState) -> Result<(), AppError> {
    state.session.deactivate().await?;
    Ok(())
}

pub async fn remove_account(state: &AppState, profile_id: String) -> Result<Vec<AuthSession>, AppError> {
    state.session.remove(&profile_id).await?;
    Ok(state.session.list().await)
}
