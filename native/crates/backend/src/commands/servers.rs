use crate::error::AppError;
use crate::services::instance::server_ping::{ping_server, ServerPingInfo};
use crate::services::instance::servers::{self, ServerEntry};
use crate::state::AppState;

pub async fn list_servers(
    state: &AppState,
    instance_id: String,
) -> Result<Vec<ServerEntry>, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    Ok(servers::list_servers(&instance).await)
}

pub async fn add_server(
    state: &AppState,
    instance_id: String,
    name: String,
    address: String,
) -> Result<Vec<ServerEntry>, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    let name = name.trim();
    let address = address.trim();
    if name.is_empty() || address.is_empty() {
        return Err(AppError::new(
            "Enter a name and an address.",
            "empty server name/address",
        ));
    }
    servers::add_server(&instance, name.to_string(), address.to_string()).await
}

pub async fn update_server(
    state: &AppState,
    instance_id: String,
    index: usize,
    name: String,
    address: String,
) -> Result<Vec<ServerEntry>, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    let name = name.trim();
    let address = address.trim();
    if name.is_empty() || address.is_empty() {
        return Err(AppError::new(
            "Enter a name and an address.",
            "empty server name/address",
        ));
    }
    servers::update_server(&instance, index, name.to_string(), address.to_string()).await
}

pub async fn remove_server(
    state: &AppState,
    instance_id: String,
    index: usize,
) -> Result<Vec<ServerEntry>, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    servers::remove_server(&instance, index).await
}

/// Pings a Minecraft server directly over TCP using the real Server List
/// Ping protocol — not something that needs the instance to exist, so this
/// takes a bare address rather than an instance id.
pub async fn ping_server_address(address: String) -> Result<ServerPingInfo, AppError> {
    Ok(ping_server(&address).await)
}
