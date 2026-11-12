use crate::error::AppError;
use crate::models::settings::{
    DiscordJoinMode, GpuPreference, LauncherSettings, OfflineModePreference, UpdatePreferences,
};
use crate::state::AppState;

pub async fn get_settings(state: &AppState) -> Result<LauncherSettings, AppError> {
    Ok(state.settings.get().await)
}

pub async fn set_offline_mode(
    state: &AppState,
    preference: OfflineModePreference,
) -> Result<LauncherSettings, AppError> {
    let updated = state
        .settings
        .update(|s| s.offline_mode = preference)
        .await?;
    state.network.force_check().await;
    Ok(updated)
}

pub async fn set_curseforge_api_key(
    state: &AppState,
    api_key: Option<String>,
) -> Result<LauncherSettings, AppError> {
    Ok(state
        .settings
        .update(|s| s.curseforge_api_key = api_key)
        .await?)
}

pub async fn set_microsoft_client_id(
    state: &AppState,
    client_id: Option<String>,
) -> Result<LauncherSettings, AppError> {
    Ok(state
        .settings
        .update(|s| s.microsoft_client_id = client_id)
        .await?)
}

pub async fn set_default_java_path(
    state: &AppState,
    path: Option<String>,
) -> Result<LauncherSettings, AppError> {
    Ok(state
        .settings
        .update(|s| s.default_java_path = path)
        .await?)
}

pub async fn set_auto_install_dependencies(
    state: &AppState,
    enabled: bool,
) -> Result<LauncherSettings, AppError> {
    Ok(state
        .settings
        .update(|s| s.auto_install_required_dependencies = enabled)
        .await?)
}

pub async fn set_update_preferences(
    state: &AppState,
    preferences: UpdatePreferences,
) -> Result<LauncherSettings, AppError> {
    let preferences = preferences.sanitized();
    crate::services::update_on_exit::set_enabled(preferences.install_on_close);
    Ok(state.settings.update(|s| s.updates = preferences).await?)
}

pub async fn set_full_assets_before_launch(
    state: &AppState,
    enabled: bool,
) -> Result<LauncherSettings, AppError> {
    Ok(state
        .settings
        .update(|s| s.full_assets_before_launch = enabled)
        .await?)
}

pub async fn set_discord_rpc_enabled(
    state: &AppState,
    enabled: bool,
) -> Result<LauncherSettings, AppError> {
    let updated = state
        .settings
        .update(|s| s.discord_rpc_enabled = enabled)
        .await?;
    if enabled {
        crate::services::discord_rpc::set_idle(&state.discord, &state.settings).await;
    } else {
        state.discord.clear().await;
    }
    Ok(updated)
}

pub async fn set_discord_join_mode(
    state: &AppState,
    mode: DiscordJoinMode,
) -> Result<LauncherSettings, AppError> {
    let updated = state
        .settings
        .update(|s| s.discord_join_mode = mode)
        .await?;
    state.discord.refresh_game(&state.settings).await;
    Ok(updated)
}

pub async fn set_discord_client_id(
    state: &AppState,
    client_id: Option<String>,
) -> Result<LauncherSettings, AppError> {
    Ok(state
        .settings
        .update(|s| s.discord_client_id = client_id)
        .await?)
}

pub async fn set_background_enabled(
    state: &AppState,
    enabled: bool,
) -> Result<LauncherSettings, AppError> {
    Ok(state
        .settings
        .update(|s| s.background_disabled = !enabled)
        .await?)
}

pub async fn set_background_overlay_opacity(
    state: &AppState,
    opacity: u8,
) -> Result<LauncherSettings, AppError> {
    let opacity = opacity.min(100);
    Ok(state
        .settings
        .update(|s| s.background_overlay_opacity = opacity)
        .await?)
}

pub async fn set_gpu_preference(
    state: &AppState,
    preference: GpuPreference,
) -> Result<LauncherSettings, AppError> {
    let updated = state
        .settings
        .update(|s| s.gpu_preference = preference)
        .await?;
    let root = state.paths.root().to_path_buf();
    let _ = tokio::task::spawn_blocking(move || {
        crate::services::gpu::reapply_recorded(&root, preference)
    })
    .await;
    Ok(updated)
}

pub async fn set_discord_spooky_enabled(
    state: &AppState,
    enabled: bool,
) -> Result<LauncherSettings, AppError> {
    let updated = state
        .settings
        .update(|s| s.discord_spooky_disabled = !enabled)
        .await?;
    // Takes effect on the next presence update; refresh the idle one now.
    crate::services::discord_rpc::set_idle(&state.discord, &state.settings).await;
    Ok(updated)
}
