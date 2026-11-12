use serde::Deserialize;

use crate::error::{AppError, AppResult};
use crate::models::common::ModLoaderKind;
use crate::models::instance::MinecraftInstance;
use crate::services::minecraft::MinecraftEngine;

use super::forgewrapper::{self, ForgeLikeConfig};
use super::mod_loader::ModLoader;

const VERSIONS_URL: &str =
    "https://maven.neoforged.net/api/maven/versions/releases/net/neoforged/neoforge";

#[derive(Deserialize)]
struct NeoForgeVersionsResponse {
    versions: Vec<String>,
}

pub struct NeoForgeLoader {
    http: reqwest::Client,
}

impl NeoForgeLoader {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .user_agent("ArcaneLauncher/0.1")
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("http client"),
        }
    }

    /// NeoForge versions look like `<mc-minor>.<mc-patch>.<build>`, mirroring
    /// the Minecraft version they target minus its leading `1.` (Minecraft
    /// 1.21.1 -> NeoForge 21.1.x). Mojang has since moved off the `1.x`
    /// scheme entirely (e.g. `26.2`) for newer releases; since it's unknown
    /// what convention loader builds for those will use once they exist,
    /// this falls back to matching the version as-is rather than assuming
    /// every id still has a `1.` prefix to strip.
    fn version_prefix(minecraft_version: &str) -> Option<String> {
        let base = minecraft_version
            .strip_prefix("1.")
            .unwrap_or(minecraft_version);
        Some(format!("{base}."))
    }

    /// Numeric-aware sort so `21.1.10` correctly ranks above `21.1.9`
    /// (a plain string sort would not).
    fn sort_key(version: &str) -> Vec<u32> {
        version.split('.').map(|s| s.parse().unwrap_or(0)).collect()
    }

    async fn fetch_versions(&self, minecraft_version: &str) -> AppResult<Vec<String>> {
        let Some(prefix) = Self::version_prefix(minecraft_version) else {
            return Ok(vec![]);
        };
        let res = self.http.get(VERSIONS_URL).send().await?;
        if !res.status().is_success() {
            return Err(AppError::new(
                "Couldn't reach the NeoForge version list.",
                format!("{} from {VERSIONS_URL}", res.status()),
            ));
        }
        let body: NeoForgeVersionsResponse = res.json().await?;
        let mut versions: Vec<String> = body
            .versions
            .into_iter()
            .filter(|v| v.starts_with(&prefix))
            .collect();
        versions.sort_by_key(|v| Self::sort_key(v));
        Ok(versions)
    }

    fn config() -> ForgeLikeConfig {
        ForgeLikeConfig {
            loader_name: "neoforge",
            group: "net.neoforged",
            maven_base: "https://maven.neoforged.net/releases/",
            artifact_id: "neoforge",
            add_fml_args: false,
        }
    }
}

impl Default for NeoForgeLoader {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ModLoader for NeoForgeLoader {
    fn kind(&self) -> ModLoaderKind {
        ModLoaderKind::NeoForge
    }

    async fn list_loader_versions(&self, minecraft_version: &str) -> AppResult<Vec<String>> {
        self.fetch_versions(minecraft_version).await
    }

    async fn install(
        &self,
        instance: &MinecraftInstance,
        engine: &MinecraftEngine,
    ) -> AppResult<String> {
        let loader_version = match &instance.loader_version {
            Some(v) => v.clone(),
            None => {
                let versions = self.fetch_versions(&instance.minecraft_version).await?;
                versions.last().cloned().ok_or_else(|| {
                    AppError::new(
                        "No NeoForge build is available for this Minecraft version.",
                        format!(
                            "empty neoforge version list for {}",
                            instance.minecraft_version
                        ),
                    )
                })?
            }
        };
        // NeoForge's maven version segment is the raw loader version itself
        // (unlike Forge it isn't prefixed with the Minecraft version).
        forgewrapper::install(
            instance,
            engine,
            &Self::config(),
            &loader_version,
            &loader_version,
        )
        .await
    }
}
