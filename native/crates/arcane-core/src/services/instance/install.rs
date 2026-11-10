use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::models::common::{DependencyType, InstalledMod, ModDependency, ModSource, ProjectType};
use crate::paths::sanitize_component;
use crate::services::cache::store::now_secs;
use crate::services::compatibility::check_compatibility;
use crate::services::download::manager::{DownloadManager, ExpectedHash};
use crate::services::providers::ProviderRegistry;

use super::manager::InstanceManager;
use super::mods as instance_mods;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct InstallModRequest {
    pub instance_id: String,
    pub project_id: String,
    pub version_id: Option<String>,
    pub source: ModSource,
    pub project_type: ProjectType,
    /// Required when `project_type` is `Datapack` (it installs into a
    /// specific world's own folder); ignored for every other type.
    #[serde(default)]
    pub world: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct InstallModOutcome {
    pub installed: InstalledMod,
    pub installed_dependencies: Vec<InstalledMod>,
    pub skipped_optional: Vec<ModDependency>,
}

pub async fn install_mod(
    instances: &InstanceManager,
    providers: &ProviderRegistry,
    downloads: &DownloadManager,
    req: InstallModRequest,
    auto_install_required: bool,
) -> AppResult<InstallModOutcome> {
    if req.project_type == ProjectType::Modpack {
        return Err(AppError::new(
            "Modpacks create their own instance — install them from the Modpacks tab.",
            "install_mod called with project_type=modpack",
        ));
    }
    if req.project_type == ProjectType::Datapack && req.world.as_deref().unwrap_or_default().trim().is_empty() {
        return Err(AppError::new(
            "Pick a world to install this datapack into.",
            "install_mod called with project_type=datapack and no world",
        ));
    }
    let mut visited = Vec::new();
    install_mod_inner(instances, providers, downloads, req, auto_install_required, &mut visited).await
}

fn install_mod_inner<'a>(
    instances: &'a InstanceManager,
    providers: &'a ProviderRegistry,
    downloads: &'a DownloadManager,
    req: InstallModRequest,
    auto_install_required: bool,
    visited: &'a mut Vec<String>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = AppResult<InstallModOutcome>> + Send + 'a>> {
    Box::pin(async move {
        if visited.contains(&req.project_id) {
            return Err(AppError::new(
                "These mods depend on each other in a loop.",
                format!("circular dependency detected at {}", req.project_id),
            ));
        }
        visited.push(req.project_id.clone());

        let instance = instances.get(&req.instance_id).await?;
        let provider = providers.get(req.source);

        let project = provider.get_project(&req.project_id).await.ok();
        // Datapack is special-cased: Modrinth reports the *project's* own
        // type as "mod" even when installing its datapack-flavored version
        // (see the comment in `modrinth::versions::get_versions`), so the
        // live project fetch would otherwise silently clobber the caller's
        // intent back to `Mod`. Every other type trusts the live project
        // data over a possibly-stale frontend value, same as before.
        let project_type = if req.project_type == ProjectType::Datapack {
            ProjectType::Datapack
        } else {
            project.as_ref().map(|p| p.project_type).unwrap_or(req.project_type)
        };

        let loader_filter = if project_type.checks_loader_compatibility() {
            Some(instance.loader.id())
        } else {
            None
        };

        let mut version = match &req.version_id {
            Some(id) => provider.get_version(id).await?,
            None => {
                let candidates = provider
                    .get_versions(
                        &req.project_id,
                        loader_filter,
                        Some(&instance.minecraft_version),
                        project_type,
                    )
                    .await?;
                let mut candidates = candidates;
                candidates.sort_by(|a, b| b.date_published.cmp(&a.date_published));
                candidates.into_iter().next().ok_or_else(|| {
                    AppError::new(
                        "No compatible version found.",
                        format!(
                            "no version of {} matches {} on {}",
                            req.project_id, instance.minecraft_version, instance.loader.id()
                        ),
                    )
                })?
            }
        };
        version.project_type = project_type;

        let installed_list = instance_mods::list_installed(&instance).await?;
        // A datapack can be installed into several different worlds at
        // once under the same project id, so "already installed" only
        // means something when the world matches too — every other type
        // installs at most once per instance, so `world` is always `None`
        // for them and this comparison is a no-op.
        let existing = installed_list
            .iter()
            .find(|m| m.project_id == req.project_id && m.world == req.world)
            .cloned();

        if let Some(existing) = &existing {
            if existing.version_id == version.id {
                return Err(AppError::new(
                    "Already installed.",
                    format!("{} version {} is already installed", req.project_id, version.id),
                ));
            }
        }

        let compat = check_compatibility(&version, &instance, &installed_list);
        if !compat.compatible {
            return Err(AppError::new(
                compat.reason.unwrap_or_else(|| "This isn't compatible.".to_string()),
                "compatibility check failed",
            ));
        }

        let file = version
            .files
            .iter()
            .find(|f| f.primary)
            .or_else(|| version.files.first())
            .ok_or_else(|| AppError::new("No downloadable file was found.", "no files on version"))?
            .clone();

        let display_name = project.as_ref().map(|p| p.name.clone()).unwrap_or_else(|| version.name.clone());

        let dest = instance_mods::content_dir(&instance, project_type, req.world.as_deref())
            .join(sanitize_component(&file.filename));
        downloads
            .download_file(
                &display_name,
                &file.url,
                dest,
                ExpectedHash {
                    sha1: file.sha1.clone(),
                    sha256: None,
                    sha512: file.sha512.clone(),
                },
                Some(instance.id.clone()),
            )
            .await?;

        if let Some(existing) = &existing {
            if existing.file_name != file.filename {
                let old_dir = if existing.enabled {
                    instance_mods::content_dir(&instance, existing.project_type, existing.world.as_deref())
                } else {
                    instance_mods::disabled_dir(&instance, existing.project_type, existing.world.as_deref())
                };
                if let Some(old_path) = crate::paths::safe_join(&old_dir, &existing.file_name) {
                    let _ = tokio::fs::remove_file(old_path).await;
                }
            }
        }

        let installed = InstalledMod {
            id: existing.as_ref().map(|e| e.id.clone()).unwrap_or_else(|| Uuid::new_v4().to_string()),
            project_id: req.project_id.clone(),
            version_id: version.id.clone(),
            source: req.source,
            project_type,
            name: display_name,
            version_number: version.version_number.clone(),
            loader: instance.loader.id().to_string(),
            minecraft_version: instance.minecraft_version.clone(),
            author: project.as_ref().map(|p| p.author.clone()).unwrap_or_default(),
            description: project.as_ref().map(|p| p.summary.clone()).unwrap_or_default(),
            icon_url: project.as_ref().and_then(|p| p.icon_url.clone()),
            file_name: file.filename.clone(),
            enabled: true,
            installed_at: now_secs(),
            dependencies: version.dependencies.clone(),
            sha1: file.sha1.clone(),
            world: req.world.clone(),
            from_modpack: false,
        };
        instance_mods::record_install(&instance, installed.clone()).await?;
        if project_type == ProjectType::ResourcePack {
            let _ = super::resourcepack_order::ensure_entry_present(&instance, &installed.file_name).await;
        }

        let mut installed_dependencies = Vec::new();
        let mut skipped_optional = Vec::new();

        // Dependencies are only meaningful for mods — resource packs and
        // shaders don't declare them the same way, and installing one never
        // needs to cascade into installing others.
        if project_type == ProjectType::Mod {
            for dep in &compat.missing_dependencies {
                let Some(dep_project_id) = dep.project_id.clone() else {
                    continue;
                };
                match dep.dependency_type {
                    DependencyType::Required => {
                        if auto_install_required {
                            let dep_req = InstallModRequest {
                                instance_id: req.instance_id.clone(),
                                project_id: dep_project_id,
                                version_id: dep.version_id.clone(),
                                source: req.source,
                                project_type: ProjectType::Mod,
                                world: None,
                            };
                            if let Ok(outcome) = install_mod_inner(
                                instances,
                                providers,
                                downloads,
                                dep_req,
                                auto_install_required,
                                visited,
                            )
                            .await
                            {
                                installed_dependencies.push(outcome.installed);
                                installed_dependencies.extend(outcome.installed_dependencies);
                            }
                        }
                    }
                    DependencyType::Optional => skipped_optional.push(dep.clone()),
                    _ => {}
                }
            }
        }

        Ok(InstallModOutcome {
            installed,
            installed_dependencies,
            skipped_optional,
        })
    })
}
