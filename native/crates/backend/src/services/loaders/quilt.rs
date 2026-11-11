use serde::Deserialize;

use crate::error::{AppError, AppResult};
use crate::models::common::ModLoaderKind;
use crate::models::instance::MinecraftInstance;
use crate::services::minecraft::MinecraftEngine;

use super::mod_loader::ModLoader;

const QUILT_META: &str = "https://meta.quiltmc.org/v3";

#[derive(Deserialize)]
struct LoaderEntry {
    loader: LoaderInfo,
}

#[derive(Deserialize)]
struct LoaderInfo {
    version: String,
}

pub struct QuiltLoader {
    http: reqwest::Client,
}

impl QuiltLoader {
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
            .first()
            .map(|e| e.loader.version.clone())
            .ok_or_else(|| AppError::new(
                "No Quilt loader is available for this Minecraft version.",
                format!("empty quilt loader list for {minecraft_version}"),
            ))
    }

    async fn fetch_loader_entries(&self, minecraft_version: &str) -> AppResult<Vec<LoaderEntry>> {
        let url = format!("{QUILT_META}/versions/loader/{minecraft_version}");
        let res = self.http.get(&url).send().await?;
        if !res.status().is_success() {
            return Err(AppError::new(
                "Couldn't reach the Quilt loader metadata service.",
                format!("{} from {url}", res.status()),
            ));
        }
        Ok(res.json().await?)
    }
}

impl Default for QuiltLoader {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ModLoader for QuiltLoader {
    fn kind(&self) -> ModLoaderKind {
        ModLoaderKind::Quilt
    }

    async fn list_loader_versions(&self, minecraft_version: &str) -> AppResult<Vec<String>> {
        Ok(self
            .fetch_loader_entries(minecraft_version)
            .await?
            .into_iter()
            .map(|e| e.loader.version)
            .collect())
    }

    async fn install(&self, instance: &MinecraftInstance, engine: &MinecraftEngine) -> AppResult<String> {
        let loader_version = match &instance.loader_version {
            Some(v) => v.clone(),
            None => self.recommended_loader_version(&instance.minecraft_version).await?,
        };

        let url = format!(
            "{QUILT_META}/versions/loader/{}/{}/profile/json",
            instance.minecraft_version, loader_version
        );
        let res = self.http.get(&url).send().await?;
        if !res.status().is_success() {
            return Err(AppError::new(
                "This Quilt version isn't available for this Minecraft version.",
                format!("{} from {url}", res.status()),
            ));
        }
        let bytes = res.bytes().await?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        let id = value
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("quilt-loader-{loader_version}-{}", instance.minecraft_version));

        engine.manifest.store_version_detail(&id, &bytes).await?;
        Ok(id)
    }
}
