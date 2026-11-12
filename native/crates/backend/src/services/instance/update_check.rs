use serde::Serialize;

use crate::models::common::{InstalledMod, ModVersion};
use crate::models::instance::MinecraftInstance;
use crate::services::download::manager::DownloadManager;
use crate::services::instance::install::{install_mod, InstallModRequest};
use crate::services::instance::manager::InstanceManager;
use crate::services::providers::ProviderRegistry;

#[derive(Debug, Clone, Serialize)]
pub struct ModUpdateInfo {
    pub installed: InstalledMod,
    pub latest: Option<ModVersion>,
    pub checked: bool,
    pub error: Option<String>,
}

/// Checks every installed mod for a newer version compatible with the
/// instance's current loader and Minecraft version. A failure on one mod
/// (network hiccup, provider outage) never stops the rest from being
/// checked — it is reported inline via `error` instead.
pub async fn check_updates(
    instance: &MinecraftInstance,
    installed: &[InstalledMod],
    providers: &ProviderRegistry,
) -> Vec<ModUpdateInfo> {
    let mut results = Vec::with_capacity(installed.len());

    for m in installed {
        // Mods synced from disk during a launcher import (see
        // `services::import`) aren't linked to any real Modrinth/CurseForge
        // project — there's nothing to check them against.
        if m.project_id.starts_with("local:") {
            results.push(ModUpdateInfo {
                installed: m.clone(),
                latest: None,
                checked: false,
                error: Some(
                    "Not linked to Modrinth/CurseForge — reinstall via Browse to track updates."
                        .to_string(),
                ),
            });
            continue;
        }

        let provider = providers.get(m.source);
        match provider
            .get_versions(
                &m.project_id,
                Some(instance.loader.id()),
                Some(&instance.minecraft_version),
                m.project_type,
            )
            .await
        {
            Ok(mut versions) => {
                versions.sort_by(|a, b| b.date_published.cmp(&a.date_published));
                // The most recently published compatible version is the
                // only thing that can ever count as "an update" — picking
                // the first one merely *different* from what's installed
                // (the previous logic) meant that a mod already on the
                // latest version got offered the *second*-newest instead,
                // i.e. a downgrade mislabeled as an update.
                let latest = match versions.into_iter().next() {
                    Some(v) if v.id != m.version_id => Some(v),
                    _ => None,
                };
                results.push(ModUpdateInfo {
                    installed: m.clone(),
                    latest,
                    checked: true,
                    error: None,
                });
            }
            Err(err) => results.push(ModUpdateInfo {
                installed: m.clone(),
                latest: None,
                checked: false,
                error: Some(err.message),
            }),
        }
    }

    results
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AutoUpdateOutcome {
    pub updated: Vec<String>,
    pub failed: Vec<String>,
}

/// Applies every available update this instance has, in one pass — the
/// shared engine behind both the "Update all" bulk action and automatic
/// updates just before launch (see `MinecraftInstance::auto_update_content`).
/// A failure updating one mod is recorded and never stops the rest.
pub async fn apply_all_updates(
    instance: &MinecraftInstance,
    installed: &[InstalledMod],
    instances: &InstanceManager,
    providers: &ProviderRegistry,
    downloads: &DownloadManager,
    auto_install_required: bool,
) -> AutoUpdateOutcome {
    let updates = check_updates(instance, installed, providers).await;
    let mut outcome = AutoUpdateOutcome::default();

    for info in updates {
        let Some(latest) = info.latest else { continue };
        let req = InstallModRequest {
            instance_id: instance.id.clone(),
            project_id: info.installed.project_id.clone(),
            version_id: Some(latest.id),
            source: info.installed.source,
            project_type: info.installed.project_type,
            world: info.installed.world.clone(),
        };
        match install_mod(instances, providers, downloads, req, auto_install_required).await {
            Ok(_) => outcome.updated.push(info.installed.name),
            Err(_) => outcome.failed.push(info.installed.name),
        }
    }

    outcome
}
