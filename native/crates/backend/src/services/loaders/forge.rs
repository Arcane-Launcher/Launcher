use std::collections::HashMap;

use serde::Deserialize;

use crate::error::{AppError, AppResult};
use crate::models::common::ModLoaderKind;
use crate::models::instance::MinecraftInstance;
use crate::services::minecraft::MinecraftEngine;

use super::forgewrapper::{self, ForgeLikeConfig};
use super::mod_loader::ModLoader;

const PROMOTIONS_URL: &str = "https://files.minecraftforge.net/net/minecraftforge/forge/promotions_slim.json";

#[derive(Deserialize)]
struct PromotionsResponse {
    promos: HashMap<String, String>,
}

pub struct ForgeLoader {
    http: reqwest::Client,
}

impl ForgeLoader {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .user_agent("ArcaneLauncher/0.1")
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("http client"),
        }
    }

    async fn fetch_promotions(&self) -> AppResult<PromotionsResponse> {
        let res = self.http.get(PROMOTIONS_URL).send().await?;
        if !res.status().is_success() {
            return Err(AppError::new(
                "Couldn't reach the Forge version list.",
                format!("{} from promotions_slim.json", res.status()),
            ));
        }
        Ok(res.json().await?)
    }

    async fn recommended_loader_version(&self, minecraft_version: &str) -> AppResult<String> {
        let promos = self.fetch_promotions().await?;
        promos
            .promos
            .get(&format!("{minecraft_version}-recommended"))
            .or_else(|| promos.promos.get(&format!("{minecraft_version}-latest")))
            .cloned()
            .ok_or_else(|| {
                AppError::new(
                    "No Forge build is available for this Minecraft version.",
                    format!("no promo for {minecraft_version}"),
                )
            })
    }

    fn config() -> ForgeLikeConfig {
        ForgeLikeConfig {
            loader_name: "forge",
            group: "net.minecraftforge",
            maven_base: "https://maven.minecraftforge.net/",
            artifact_id: "forge",
            add_fml_args: true,
        }
    }
}

impl Default for ForgeLoader {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ModLoader for ForgeLoader {
    fn kind(&self) -> ModLoaderKind {
        ModLoaderKind::Forge
    }

    async fn list_loader_versions(&self, minecraft_version: &str) -> AppResult<Vec<String>> {
        let body = self.fetch_promotions().await?;
        let prefix = format!("{minecraft_version}-");
        let mut versions: Vec<String> = body
            .promos
            .into_iter()
            .filter_map(|(key, version)| key.starts_with(&prefix).then_some(version))
            .collect();
        versions.sort();
        versions.dedup();
        Ok(versions)
    }

    async fn install(&self, instance: &MinecraftInstance, engine: &MinecraftEngine) -> AppResult<String> {
        let loader_version = match &instance.loader_version {
            Some(v) => v.clone(),
            None => self.recommended_loader_version(&instance.minecraft_version).await?,
        };
        let long_version = format!("{}-{loader_version}", instance.minecraft_version);
        forgewrapper::install(instance, engine, &Self::config(), &long_version, &loader_version).await
    }
}
