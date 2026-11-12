use serde::Deserialize;

use crate::error::{AppError, AppResult};
use crate::models::common::ModLoaderKind;
use crate::models::instance::MinecraftInstance;
use crate::services::minecraft::MinecraftEngine;

use super::mod_loader::ModLoader;

const FABRIC_META: &str = "https://meta.fabricmc.net/v2";

#[derive(Deserialize)]
struct LoaderEntry {
    loader: LoaderInfo,
}

#[derive(Deserialize)]
struct LoaderInfo {
    version: String,
    stable: bool,
}

pub struct FabricLoader {
    http: reqwest::Client,
}

impl FabricLoader {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .user_agent("ArcaneLauncher/0.1")
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("http client"),
        }
    }

    pub async fn recommended_loader_version(&self, minecraft_version: &str) -> AppResult<String> {
        let entries = self.fetch_loader_entries(minecraft_version).await?;
        entries
            .iter()
            .find(|e| e.loader.stable)
            .or_else(|| entries.first())
            .map(|e| e.loader.version.clone())
            .ok_or_else(|| {
                AppError::new(
                    "No Fabric loader is available for this Minecraft version.",
                    format!("empty fabric loader list for {minecraft_version}"),
                )
            })
    }

    async fn fetch_loader_entries(&self, minecraft_version: &str) -> AppResult<Vec<LoaderEntry>> {
        let url = format!("{FABRIC_META}/versions/loader/{minecraft_version}");
        let res = self.http.get(&url).send().await?;
        if !res.status().is_success() {
            return Err(AppError::new(
                "Couldn't reach the Fabric loader metadata service.",
                format!("{} from {url}", res.status()),
            ));
        }
        Ok(res.json().await?)
    }
}

impl Default for FabricLoader {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ModLoader for FabricLoader {
    fn kind(&self) -> ModLoaderKind {
        ModLoaderKind::Fabric
    }

    async fn list_loader_versions(&self, minecraft_version: &str) -> AppResult<Vec<String>> {
        Ok(self
            .fetch_loader_entries(minecraft_version)
            .await?
            .into_iter()
            .map(|e| e.loader.version)
            .collect())
    }

    async fn install(
        &self,
        instance: &MinecraftInstance,
        engine: &MinecraftEngine,
    ) -> AppResult<String> {
        let loader_version = match &instance.loader_version {
            Some(v) => v.clone(),
            None => {
                self.recommended_loader_version(&instance.minecraft_version)
                    .await?
            }
        };

        let url = format!(
            "{FABRIC_META}/versions/loader/{}/{}/profile/json",
            instance.minecraft_version, loader_version
        );
        let res = self.http.get(&url).send().await?;
        if !res.status().is_success() {
            return Err(AppError::new(
                "This Fabric version isn't available for this Minecraft version.",
                format!("{} from {url}", res.status()),
            ));
        }
        let bytes = res.bytes().await?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        let id = value
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                format!(
                    "fabric-loader-{loader_version}-{}",
                    instance.minecraft_version
                )
            });

        engine.manifest.store_version_detail(&id, &bytes).await?;
        Ok(id)
    }
}
