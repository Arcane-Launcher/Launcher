use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::AppResult;
use crate::models::instance::MinecraftInstance;

/// Mirrors vanilla Minecraft's `servers.dat` NBT layout exactly (root
/// compound with a `servers` list of `{name, ip, icon?, acceptTextures?}`
/// compounds) so this launcher reads and writes the same file the game
/// itself uses — no separate/duplicate server list to keep in sync.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ServersDatRoot {
    #[serde(default)]
    servers: Vec<ServerDatEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ServerDatEntry {
    name: String,
    ip: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    icon: Option<String>,
    #[serde(
        rename = "acceptTextures",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    accept_textures: Option<i8>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerEntry {
    pub index: usize,
    pub name: String,
    pub address: String,
    pub icon_base64: Option<String>,
}

fn servers_dat_path(instance: &MinecraftInstance) -> PathBuf {
    Path::new(&instance.game_dir).join("servers.dat")
}

async fn read_root(instance: &MinecraftInstance) -> ServersDatRoot {
    match tokio::fs::read(servers_dat_path(instance)).await {
        Ok(bytes) => fastnbt::from_bytes(&bytes).unwrap_or_default(),
        Err(_) => ServersDatRoot::default(),
    }
}

async fn write_root(instance: &MinecraftInstance, root: &ServersDatRoot) -> AppResult<()> {
    let bytes = fastnbt::to_bytes(root).map_err(|e| {
        crate::error::AppError::new("Couldn't save the server list.", e.to_string())
    })?;
    tokio::fs::write(servers_dat_path(instance), bytes).await?;
    Ok(())
}

/// The name the player gave `address` in their in-game server list, if it's
/// there. Addresses compare case-insensitively with the default port
/// optional, since the game logs `host` for `host:25565`.
pub async fn saved_server_name(game_dir: &Path, address: &str) -> Option<String> {
    fn normalize(a: &str) -> String {
        let a = a.trim().to_ascii_lowercase();
        a.strip_suffix(":25565").map(str::to_string).unwrap_or(a)
    }
    let bytes = tokio::fs::read(game_dir.join("servers.dat")).await.ok()?;
    let root: ServersDatRoot = fastnbt::from_bytes(&bytes).ok()?;
    let want = normalize(address);
    root.servers
        .into_iter()
        .find(|s| normalize(&s.ip) == want)
        .map(|s| s.name)
        .filter(|n| !n.trim().is_empty())
}

pub async fn list_servers(instance: &MinecraftInstance) -> Vec<ServerEntry> {
    let root = read_root(instance).await;
    root.servers
        .into_iter()
        .enumerate()
        .map(|(index, s)| ServerEntry {
            index,
            name: s.name,
            address: s.ip,
            icon_base64: s.icon,
        })
        .collect()
}

pub async fn add_server(
    instance: &MinecraftInstance,
    name: String,
    address: String,
) -> AppResult<Vec<ServerEntry>> {
    let mut root = read_root(instance).await;
    root.servers.push(ServerDatEntry {
        name,
        ip: address,
        icon: None,
        accept_textures: None,
    });
    write_root(instance, &root).await?;
    Ok(list_servers(instance).await)
}

/// Renames and/or re-points one saved server in place, keeping its spot in
/// the list. A changed address drops the cached server icon, since it
/// belonged to the old server (the game fetches a fresh one on next ping).
pub async fn update_server(
    instance: &MinecraftInstance,
    index: usize,
    name: String,
    address: String,
) -> AppResult<Vec<ServerEntry>> {
    let mut root = read_root(instance).await;
    let Some(entry) = root.servers.get_mut(index) else {
        return Err(crate::error::AppError::new(
            "That server isn't in the list anymore.",
            format!("update_server: index {index} out of range"),
        ));
    };
    if !entry.ip.trim().eq_ignore_ascii_case(address.trim()) {
        entry.icon = None;
    }
    entry.name = name;
    entry.ip = address;
    write_root(instance, &root).await?;
    Ok(list_servers(instance).await)
}

pub async fn remove_server(
    instance: &MinecraftInstance,
    index: usize,
) -> AppResult<Vec<ServerEntry>> {
    let mut root = read_root(instance).await;
    if index < root.servers.len() {
        root.servers.remove(index);
    }
    write_root(instance, &root).await?;
    Ok(list_servers(instance).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::common::ModLoaderKind;

    fn instance_in(dir: &Path) -> MinecraftInstance {
        MinecraftInstance {
            id: "test".into(),
            name: "Test".into(),
            minecraft_version: "1.21.1".into(),
            loader: ModLoaderKind::Vanilla,
            loader_version: None,
            game_dir: dir.to_string_lossy().to_string(),
            java_path: None,
            java_args: None,
            memory_mb: None,
            window_width: None,
            window_height: None,
            icon: None,
            created_at: 0,
            last_played: None,
            total_playtime_secs: 0,
            imported: false,
            modpack_source: None,
            modpack_project_id: None,
            modpack_version_id: None,
            auto_update_content: false,
            group: None,
            notes: None,
            pre_launch_command: None,
            post_exit_command: None,
            favorite: false,
        }
    }

    #[tokio::test]
    async fn update_server_changes_ip_in_place_and_drops_stale_icon() {
        let dir =
            std::env::temp_dir().join(format!("arcane-servers-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let instance = instance_in(&dir);

        let root = ServersDatRoot {
            servers: vec![
                ServerDatEntry {
                    name: "A".into(),
                    ip: "a.example.net".into(),
                    icon: Some("iconA".into()),
                    accept_textures: Some(1),
                },
                ServerDatEntry {
                    name: "B".into(),
                    ip: "b.example.net".into(),
                    icon: Some("iconB".into()),
                    accept_textures: None,
                },
            ],
        };
        write_root(&instance, &root).await.unwrap();

        // Rename only: the icon stays.
        let list = update_server(&instance, 1, "Bee".into(), "B.example.net".into())
            .await
            .unwrap();
        assert_eq!(list[1].name, "Bee");
        assert_eq!(list[1].icon_base64.as_deref(), Some("iconB"));

        // New IP: stays at index 0, old icon dropped, other fields kept.
        let list = update_server(&instance, 0, "A".into(), "mc.arcaneworld.lk:2504".into())
            .await
            .unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].address, "mc.arcaneworld.lk:2504");
        assert_eq!(list[0].icon_base64, None);
        assert_eq!(
            read_root(&instance).await.servers[0].accept_textures,
            Some(1)
        );

        assert!(update_server(&instance, 5, "X".into(), "x".into())
            .await
            .is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
