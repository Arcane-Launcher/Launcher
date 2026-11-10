use serde::{Deserialize, Serialize};

use super::common::ModLoaderKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportSource {
    PrismLauncher,
    CurseForge,
    TLauncher,
    SKLauncher,
    ModrinthApp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredInstance {
    pub source: ImportSource,
    pub name: String,
    pub minecraft_version: String,
    pub loader: ModLoaderKind,
    pub loader_version: Option<String>,
    pub game_dir: String,
    pub root_path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorldInfo {
    pub folder_name: String,
    pub display_name: String,
    pub last_played: Option<u64>,
    pub size_bytes: u64,
    pub icon_data_uri: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScreenshotInfo {
    pub filename: String,
    pub path: String,
    pub taken_at: u64,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModProfile {
    pub id: String,
    pub name: String,
    pub mod_ids: Vec<String>,
}
