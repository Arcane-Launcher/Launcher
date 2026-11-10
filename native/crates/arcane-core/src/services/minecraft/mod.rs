pub mod assets;
pub mod auth;
pub mod crash;
pub mod java;
pub mod java_provision;
pub mod launch;
pub mod libraries;
pub mod manifest;
pub mod mineskin;
pub mod running;
pub mod session_store;
pub mod skins;

use crate::error::AppResult;
use crate::models::instance::MinecraftInstance;
use crate::models::minecraft::{AuthSession, VersionDetail};
use crate::paths::AppPaths;
use crate::services::download::manager::{DownloadManager, ExpectedHash};
use crate::services::instance::manager::InstanceManager;

use running::RunningInstances;

use manifest::VersionManifestService;

#[derive(Clone)]
pub struct MinecraftEngine {
    pub manifest: VersionManifestService,
    pub downloads: DownloadManager,
    pub paths: AppPaths,
}

impl MinecraftEngine {
    pub fn new(manifest: VersionManifestService, downloads: DownloadManager, paths: AppPaths) -> Self {
        Self {
            manifest,
            downloads,
            paths,
        }
    }

    /// Ensures the vanilla client jar for `minecraft_version` is present in
    /// the shared version cache, downloading it if needed.
    pub async fn ensure_client_jar(&self, minecraft_version: &str, instance_id: Option<String>) -> AppResult<()> {
        let vanilla = self
            .manifest
            .ensure_vanilla_version_detail(minecraft_version)
            .await?;
        let Some(downloads) = &vanilla.downloads else {
            return Ok(());
        };
        let client_jar_path = self
            .paths
            .versions_meta_dir()
            .join(crate::paths::sanitize_component(minecraft_version))
            .join("client.jar");
        if !client_jar_path.exists() {
            self.downloads
                .download_file(
                    "Minecraft client",
                    &downloads.client.url,
                    client_jar_path,
                    ExpectedHash {
                        sha1: Some(downloads.client.sha1.clone()),
                        sha256: None,
                        sha512: None,
                    },
                    instance_id,
                )
                .await?;
        }
        Ok(())
    }

    /// Resolves `version_key`'s effective profile (following inheritance)
    /// and makes sure its libraries and assets are downloaded. The client
    /// jar is handled separately via [`Self::ensure_client_jar`] since it is
    /// keyed by the vanilla version rather than the (possibly loader-merged)
    /// profile id. `defer_optional_assets`: see [`assets::ensure_assets`].
    pub async fn ensure_installed(
        &self,
        instance: &MinecraftInstance,
        version_key: &str,
        defer_optional_assets: bool,
    ) -> AppResult<VersionDetail> {
        let instance_id = Some(instance.id.clone());
        // Fetched once up front so the two branches below never both try
        // to download and write the same version json at the same time.
        self.manifest
            .ensure_vanilla_version_detail(&instance.minecraft_version)
            .await?;
        // The client jar, libraries and assets are independent, so they
        // download side by side instead of each waiting for the last —
        // the ~25 MB client jar no longer holds up thousands of tiny assets.
        let (_, version) = tokio::try_join!(
            self.ensure_client_jar(&instance.minecraft_version, instance_id.clone()),
            async {
                let version = self.manifest.resolve_effective_version(version_key).await?;
                tokio::try_join!(
                    libraries::ensure_libraries(&version, &self.paths, &self.downloads, instance_id.clone()),
                    assets::ensure_assets(
                        &version,
                        &self.paths,
                        &self.downloads,
                        instance_id.clone(),
                        defer_optional_assets,
                    ),
                )?;
                AppResult::Ok(version)
            },
        )?;
        Ok(version)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn launch(
        &self,
        state: &crate::state::AppState,
        instance: &MinecraftInstance,
        version: &VersionDetail,
        session: &AuthSession,
        java_path: &str,
        target_server: Option<&str>,
        env: Vec<(String, String)>,
        running: RunningInstances,
        instances: InstanceManager,
    ) -> AppResult<()> {
        let natives_dir = std::path::Path::new(&instance.game_dir)
            .join(format!(".natives-{}", instance.minecraft_version));
        let _ = std::fs::remove_dir_all(&natives_dir);
        libraries::extract_natives(version, &self.paths, &natives_dir)?;
        let mut plan = launch::build_launch_plan(
            instance,
            version,
            session,
            java_path,
            &natives_dir,
            &self.paths,
            target_server,
        )?;
        plan.env = env;
        launch::spawn_and_stream(state, instance, plan, running, instances).await
    }
}
