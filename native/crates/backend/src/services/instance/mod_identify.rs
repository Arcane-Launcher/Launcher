use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::Serialize;
use sha1::{Digest, Sha1};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::models::common::{InstalledMod, ModProject, ModSource, ModVersion, ProjectType};
use crate::models::instance::MinecraftInstance;
use crate::paths::safe_join;
use crate::services::cache::store::now_secs;
use crate::services::providers::mod_provider::ModProvider;
use crate::services::providers::ProviderRegistry;

use super::mods as instance_mods;

fn sha1_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

#[derive(Debug, Clone, Serialize)]
pub struct IdentifiedFile {
    pub file_name: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ScanResult {
    pub identified: Vec<IdentifiedFile>,
    pub unidentified: Vec<String>,
}

fn build_installed_mod(
    instance: &MinecraftInstance,
    version: &ModVersion,
    project: Option<&ModProject>,
    project_type: ProjectType,
    file_name: String,
    sha1: String,
    enabled: bool,
) -> InstalledMod {
    InstalledMod {
        id: Uuid::new_v4().to_string(),
        project_id: version.project_id.clone(),
        version_id: version.id.clone(),
        source: ModSource::Modrinth,
        project_type,
        name: project.map(|p| p.name.clone()).unwrap_or_else(|| version.name.clone()),
        version_number: version.version_number.clone(),
        loader: instance.loader.id().to_string(),
        minecraft_version: instance.minecraft_version.clone(),
        author: project.map(|p| p.author.clone()).unwrap_or_default(),
        description: project.map(|p| p.summary.clone()).unwrap_or_default(),
        icon_url: project.and_then(|p| p.icon_url.clone()),
        file_name,
        enabled,
        installed_at: now_secs(),
        dependencies: version.dependencies.clone(),
        sha1: Some(sha1),
        world: None,
        from_modpack: false,
    }
}

/// Hashes raw bytes (a file dropped straight onto the launcher, with no
/// filesystem path to read an extension/name from), identifies it via
/// Modrinth's hash lookup, writes it into the instance under its real
/// filename, and records it as a properly tracked mod.
pub async fn identify_and_track_bytes(
    instance: &MinecraftInstance,
    providers: &ProviderRegistry,
    bytes: Vec<u8>,
) -> AppResult<InstalledMod> {
    let sha1 = sha1_hex(&bytes);
    let mut matches = providers.modrinth.lookup_by_sha1(std::slice::from_ref(&sha1)).await?;
    let Some(version) = matches.remove(&sha1) else {
        return Err(AppError::new(
            "Couldn't identify this file — it may not be hosted on Modrinth (e.g. a CurseForge-only mod).",
            format!("no version_files match for sha1 {sha1}"),
        ));
    };
    let project = providers.modrinth.get_project(&version.project_id).await.ok();
    let project_type = project.as_ref().map(|p| p.project_type).unwrap_or(ProjectType::Mod);
    if project_type == ProjectType::Datapack {
        return Err(AppError::new(
            "Datapacks install into a specific world — add this one from the Worlds tab instead.",
            "identify_and_track_bytes: datapack requires a world, none available here",
        ));
    }

    let file_name = version
        .files
        .iter()
        .find(|f| f.sha1.as_deref() == Some(sha1.as_str()))
        .map(|f| f.filename.clone())
        .unwrap_or_else(|| format!("{sha1}.jar"));

    let dest_dir = instance_mods::content_dir(instance, project_type, None);
    tokio::fs::create_dir_all(&dest_dir).await?;
    let dest =
        safe_join(&dest_dir, &file_name).ok_or_else(|| AppError::new("Invalid file name.", "path traversal blocked"))?;
    tokio::fs::write(&dest, &bytes).await?;

    let installed = build_installed_mod(instance, &version, project.as_ref(), project_type, file_name, sha1, true);
    instance_mods::record_install(instance, installed.clone()).await?;
    Ok(installed)
}

/// Scans an instance's mod/resourcepack/shaderpack folders (both enabled and
/// `.disabled`) for files the tracker doesn't know about yet, hashes them,
/// and identifies as many as possible against Modrinth in a single batched
/// lookup — the same approach used for tracking a modpack's bundled files,
/// just applied retroactively to whatever's already sitting on disk. A file
/// that can't be matched (not Modrinth-hosted, or a corrupted/unknown blob)
/// is reported back rather than guessed at.
pub async fn scan_untracked(instance: &MinecraftInstance, providers: &ProviderRegistry) -> AppResult<ScanResult> {
    let tracked = instance_mods::list_installed(instance).await?;
    let tracked_files: HashSet<(ProjectType, String)> =
        tracked.iter().map(|m| (m.project_type, m.file_name.clone())).collect();

    let mut candidates: Vec<(ProjectType, String, bool, PathBuf)> = Vec::new();
    for project_type in [ProjectType::Mod, ProjectType::ResourcePack, ProjectType::Shader] {
        for (dir, enabled) in [
            (instance_mods::content_dir(instance, project_type, None), true),
            (instance_mods::disabled_dir(instance, project_type, None), false),
        ] {
            let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
                continue;
            };
            while let Ok(Some(entry)) = entries.next_entry().await {
                let Ok(file_type) = entry.file_type().await else { continue };
                if !file_type.is_file() {
                    continue;
                }
                let file_name = entry.file_name().to_string_lossy().to_string();
                if tracked_files.contains(&(project_type, file_name.clone())) {
                    continue;
                }
                candidates.push((project_type, file_name, enabled, entry.path()));
            }
        }
    }

    if candidates.is_empty() {
        return Ok(ScanResult::default());
    }

    let mut hashes = Vec::with_capacity(candidates.len());
    let mut by_hash: HashMap<String, (ProjectType, String, bool)> = HashMap::new();
    for (project_type, file_name, enabled, path) in &candidates {
        let Ok(bytes) = tokio::fs::read(path).await else { continue };
        let sha1 = sha1_hex(&bytes);
        hashes.push(sha1.clone());
        by_hash.insert(sha1, (*project_type, file_name.clone(), *enabled));
    }

    let matches = providers.modrinth.lookup_by_sha1(&hashes).await?;

    let project_ids: Vec<String> = matches.values().map(|v| v.project_id.clone()).collect();
    let projects = providers.modrinth.resolve_projects(&project_ids).await.unwrap_or_default();
    let projects_by_id: HashMap<&str, &ModProject> = projects.iter().map(|p| (p.id.as_str(), p)).collect();

    let mut result = ScanResult::default();
    for (hash, (project_type, file_name, enabled)) in &by_hash {
        match matches.get(hash) {
            Some(version) => {
                let project = projects_by_id.get(version.project_id.as_str()).copied();
                let installed =
                    build_installed_mod(instance, version, project, *project_type, file_name.clone(), hash.clone(), *enabled);
                result.identified.push(IdentifiedFile {
                    file_name: file_name.clone(),
                    name: installed.name.clone(),
                });
                instance_mods::record_install(instance, installed).await?;
            }
            None => result.unidentified.push(file_name.clone()),
        }
    }
    Ok(result)
}
