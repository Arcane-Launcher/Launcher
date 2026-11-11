
use crate::error::AppError;
use crate::models::settings::OfflineModePreference;
use crate::services::network::connectivity::NetworkStatus;
use crate::state::AppState;

pub async fn get_network_status(state: &AppState) -> Result<NetworkStatus, AppError> {
    Ok(build_status(&state).await)
}

pub async fn force_network_check(state: &AppState) -> Result<NetworkStatus, AppError> {
    state.network.force_check().await;
    Ok(build_status(&state).await)
}

async fn build_status(state: &&AppState) -> NetworkStatus {
    let always_offline = matches!(
        state.settings.get().await.offline_mode,
        OfflineModePreference::AlwaysOffline
    );
    NetworkStatus {
        state: state.network.state().await.into(),
        modrinth_reachable: state.network.is_modrinth_reachable().await,
        always_offline,
    }
}
