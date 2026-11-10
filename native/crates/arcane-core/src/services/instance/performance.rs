//! One-click performance boost: the well-known optimisation mods that fit an
//! instance's loader and Minecraft version, installed from Modrinth.

use futures_util::future::join_all;
use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::models::common::{InstalledMod, ModLoaderKind, ModSource, ProjectType};
use crate::models::instance::MinecraftInstance;
use crate::services::download::manager::DownloadManager;
use crate::services::providers::ProviderRegistry;

use super::install::{install_mod, InstallModRequest};
use super::jar_ids::{self, ModJar};
use super::manager::InstanceManager;
use super::mods as instance_mods;

use ModLoaderKind::{Fabric, Forge, NeoForge, Quilt};

struct Candidate {
    /// Modrinth slug (verified against the live API).
    slug: &'static str,
    why: &'static str,
    /// Loaders Modrinth lists for the project.
    loaders: &'static [ModLoaderKind],
    /// Only one mod per group is installed (Sodium and its Forge port
    /// Embeddium do the same job and can't be combined).
    group: Option<&'static str>,
    /// Mod ids that mean it (or an equivalent) is already there.
    ids: &'static [&'static str],
}

const CANDIDATES: &[Candidate] = &[
    Candidate {
        slug: "sodium",
        why: "Rewrites the renderer: often double the FPS or more.",
        loaders: &[Fabric, Quilt, NeoForge],
        group: Some("renderer"),
        ids: &["sodium", "embeddium", "rubidium"],
    },
    Candidate {
        slug: "embeddium",
        why: "Sodium's renderer for Forge: often double the FPS or more.",
        loaders: &[Forge, NeoForge],
        group: Some("renderer"),
        ids: &["embeddium", "rubidium", "sodium"],
    },
    Candidate {
        slug: "lithium",
        why: "Faster game logic: mobs, redstone, chunk ticking.",
        loaders: &[Fabric, Quilt, NeoForge],
        group: None,
        ids: &["lithium", "canary", "radium"],
    },
    Candidate {
        slug: "ferrite-core",
        why: "Cuts the game's RAM use a lot.",
        loaders: &[Fabric, Quilt, Forge, NeoForge],
        group: None,
        ids: &["ferritecore"],
    },
    Candidate {
        slug: "modernfix",
        why: "Faster startup and lower RAM use.",
        loaders: &[Fabric, Forge, NeoForge],
        group: None,
        ids: &["modernfix"],
    },
    Candidate {
        slug: "immediatelyfast",
        why: "Faster HUD, text and entity drawing.",
        loaders: &[Fabric, Quilt, Forge, NeoForge],
        group: None,
        ids: &["immediatelyfast"],
    },
    Candidate {
        slug: "entityculling",
        why: "Skips drawing mobs and chests you can't see.",
        loaders: &[Fabric, Quilt, Forge, NeoForge],
        group: None,
        ids: &["entityculling"],
    },
    Candidate {
        slug: "dynamic-fps",
        why: "Lowers FPS while the game is in the background, saving power.",
        loaders: &[Fabric, Quilt, Forge, NeoForge],
        group: None,
        ids: &["dynamic_fps", "dynamicfps"],
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BoostStatus {
    /// Can be installed now.
    Available,
    Installed,
    /// No release for this Minecraft version + loader.
    Unavailable,
    /// Would conflict with something installed (OptiFine).
    Blocked,
}

#[derive(Debug, Clone, Serialize)]
pub struct BoostItem {
    pub slug: String,
    pub project_id: Option<String>,
    pub name: String,
    pub why: String,
    pub icon_url: Option<String>,
    pub status: BoostStatus,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BoostOutcome {
    pub installed: Vec<String>,
    pub failed: Vec<BoostFailure>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BoostFailure {
    pub name: String,
    pub reason: String,
}

fn has_optifine(jars: &[ModJar]) -> bool {
    jars.iter().any(|j| {
        let f = j.file_name.to_ascii_lowercase();
        f.contains("optifine") || f.contains("optifabric")
    })
}

fn already_present(c: &Candidate, project_id: Option<&str>, jars: &[ModJar], installed: &[InstalledMod]) -> Option<bool> {
    if let Some(pid) = project_id {
        if let Some(m) = installed.iter().find(|m| m.project_id == pid) {
            return Some(m.enabled);
        }
    }
    c.ids.iter().any(|id| jar_ids::find_jar(jars, id).is_some()).then_some(true)
}

/// What the boost would do for this instance, one row per mod.
pub async fn preview(instance: &MinecraftInstance, providers: &ProviderRegistry) -> AppResult<Vec<BoostItem>> {
    if instance.loader == ModLoaderKind::Vanilla {
        return Err(AppError::new(
            "Performance mods need a mod loader. Switch this instance to Fabric with Change version, or create a Fabric instance.",
            "performance preview: vanilla instance",
        ));
    }
    let jars = jar_ids::scan_mod_jars(instance).await;
    let installed = instance_mods::list_installed(instance).await.unwrap_or_default();
    let optifine = has_optifine(&jars);
    let modrinth = providers.get(ModSource::Modrinth);

    let applicable: Vec<&Candidate> = CANDIDATES.iter().filter(|c| c.loaders.contains(&instance.loader)).collect();
    let lookups = join_all(applicable.iter().map(|c| {
        let modrinth = modrinth.clone();
        async move {
            let project = modrinth.get_project(c.slug).await.ok();
            let has_version = match &project {
                Some(p) => modrinth
                    .get_versions(&p.id, Some(instance.loader.id()), Some(&instance.minecraft_version), ProjectType::Mod)
                    .await
                    .map(|v| !v.is_empty())
                    .ok(),
                None => None,
            };
            (project, has_version)
        }
    }))
    .await;

    let mut items: Vec<BoostItem> = Vec::new();
    let mut settled_groups: Vec<&str> = Vec::new();
    // A group member that has no release, kept in case no member of its
    // group fits (so the row still explains why nothing is offered).
    let mut group_fallback: Vec<(&str, BoostItem)> = Vec::new();

    for (c, (project, has_version)) in applicable.iter().zip(lookups) {
        if c.group.is_some_and(|g| settled_groups.contains(&g)) {
            continue;
        }
        let present = already_present(c, project.as_ref().map(|p| p.id.as_str()), &jars, &installed);
        let (status, note) = if let Some(enabled) = present {
            (BoostStatus::Installed, (!enabled).then(|| "Installed but turned off in the Mods tab.".to_string()))
        } else if c.group == Some("renderer") && optifine {
            (BoostStatus::Blocked, Some("OptiFine is installed and conflicts with it.".to_string()))
        } else {
            match has_version {
                Some(true) => (BoostStatus::Available, None),
                Some(false) => (
                    BoostStatus::Unavailable,
                    Some(format!("No release for {} {} yet.", instance.loader.id(), instance.minecraft_version)),
                ),
                None => (BoostStatus::Unavailable, Some("Couldn't reach Modrinth.".to_string())),
            }
        };
        let item = BoostItem {
            slug: c.slug.to_string(),
            project_id: project.as_ref().map(|p| p.id.clone()),
            name: project.as_ref().map(|p| p.name.clone()).unwrap_or_else(|| c.slug.to_string()),
            why: c.why.to_string(),
            icon_url: project.as_ref().and_then(|p| p.icon_url.clone()),
            status,
            note,
        };
        match c.group {
            Some(g) if status == BoostStatus::Unavailable => {
                if !group_fallback.iter().any(|(fg, _)| *fg == g) {
                    group_fallback.push((g, item));
                }
            }
            Some(g) => {
                settled_groups.push(g);
                items.push(item);
            }
            None => items.push(item),
        }
    }
    for (g, item) in group_fallback {
        if !settled_groups.contains(&g) {
            items.insert(0, item);
        }
    }
    Ok(items)
}

/// Installs the chosen boost mods (only ones `preview` offers as
/// available), one after another so the mod list is never written twice at
/// once. Required dependencies come along automatically.
pub async fn apply(
    instance: &MinecraftInstance,
    project_ids: &[String],
    instances: &InstanceManager,
    providers: &ProviderRegistry,
    downloads: &DownloadManager,
) -> AppResult<BoostOutcome> {
    let offered = preview(instance, providers).await?;
    let mut outcome = BoostOutcome { installed: Vec::new(), failed: Vec::new() };
    for item in offered.into_iter().filter(|i| i.status == BoostStatus::Available) {
        let Some(project_id) = item.project_id.clone() else { continue };
        if !project_ids.contains(&project_id) {
            continue;
        }
        let req = InstallModRequest {
            instance_id: instance.id.clone(),
            project_id,
            version_id: None,
            source: ModSource::Modrinth,
            project_type: ProjectType::Mod,
            world: None,
        };
        match install_mod(instances, providers, downloads, req, true).await {
            Ok(done) => {
                outcome.installed.push(done.installed.name);
                outcome.installed.extend(done.installed_dependencies.into_iter().map(|d| d.name));
            }
            Err(e) => outcome.failed.push(BoostFailure { name: item.name, reason: e.message }),
        }
    }
    outcome.installed.dedup();
    Ok(outcome)
}
