pub mod fabric;
pub mod forge;
pub mod forgewrapper;
pub mod mod_loader;
pub mod neoforge;
pub mod quilt;

use std::sync::Arc;

use crate::error::AppResult;
use crate::models::common::ModLoaderKind;
use crate::models::instance::MinecraftInstance;
use crate::services::instance::manager::InstanceManager;
use crate::services::instance::validation;
use crate::services::minecraft::MinecraftEngine;

use mod_loader::ModLoader;

#[derive(Clone)]
pub struct LoaderRegistry {
    fabric: Arc<fabric::FabricLoader>,
    forge: Arc<forge::ForgeLoader>,
    neoforge: Arc<neoforge::NeoForgeLoader>,
    quilt: Arc<quilt::QuiltLoader>,
}

impl LoaderRegistry {
    pub fn new() -> Self {
        Self {
            fabric: Arc::new(fabric::FabricLoader::new()),
            forge: Arc::new(forge::ForgeLoader::new()),
            neoforge: Arc::new(neoforge::NeoForgeLoader::new()),
            quilt: Arc::new(quilt::QuiltLoader::new()),
        }
    }

    pub fn get(&self, kind: ModLoaderKind) -> Option<Arc<dyn ModLoader>> {
        match kind {
            ModLoaderKind::Vanilla => None,
            ModLoaderKind::Fabric => Some(self.fabric.clone()),
            ModLoaderKind::Forge => Some(self.forge.clone()),
            ModLoaderKind::NeoForge => Some(self.neoforge.clone()),
            ModLoaderKind::Quilt => Some(self.quilt.clone()),
        }
    }
}

impl Default for LoaderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Installs (if needed) whatever this instance's loader requires and
/// returns the effective version key ready to pass to
/// `MinecraftEngine::ensure_installed`/`resolve_effective_version` — a
/// no-op for vanilla. Shared by the launch pipeline, launch-validation
/// "Fix", and Instance Evolution migrations: anywhere an instance needs to
/// actually become launch-ready for its current (or a freshly changed)
/// version/loader, not just have its metadata say so.
pub async fn ensure_loader_ready(
    loaders: &LoaderRegistry,
    engine: &MinecraftEngine,
    instances: &InstanceManager,
    instance: &mut MinecraftInstance,
) -> AppResult<String> {
    match instance.loader {
        ModLoaderKind::Vanilla => Ok(instance.minecraft_version.clone()),
        ModLoaderKind::Fabric
        | ModLoaderKind::Quilt
        | ModLoaderKind::Forge
        | ModLoaderKind::NeoForge => {
            let loader = loaders
                .get(instance.loader)
                .expect("every non-vanilla loader is always registered");

            if let Some(expected_key) = validation::expected_version_key(instance) {
                if engine
                    .manifest
                    .resolve_effective_version_offline(&expected_key)
                    .await
                    .is_some()
                {
                    return Ok(expected_key);
                }
            }

            let version_key = loader.install(instance, engine).await?;
            if instance.loader_version.is_none() {
                let prefix = format!("{}-loader-", instance.loader.id());
                let suffix = format!("-{}", instance.minecraft_version);
                if let Some(rest) = version_key
                    .strip_prefix(&prefix)
                    .and_then(|s| s.strip_suffix(&suffix))
                {
                    instance.loader_version = Some(rest.to_string());
                    let _ = instances.update(instance.clone()).await;
                }
            }
            Ok(version_key)
        }
    }
}
