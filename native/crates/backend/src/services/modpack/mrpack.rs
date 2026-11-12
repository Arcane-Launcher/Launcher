use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::models::common::{InstalledMod, ModLoaderKind, ModSource, ModVersion, ProjectType};
use crate::models::instance::{CreateInstanceRequest, MinecraftInstance};
use crate::paths::{safe_join, AppPaths};
use crate::services::cache::store::now_secs;
use crate::services::download::manager::{DownloadManager, ExpectedHash};
use crate::services::instance::manager::InstanceManager;
use crate::services::instance::mods as instance_mods;
use crate::services::providers::ProviderRegistry;

/// The `modrinth.index.json` manifest at the root of every `.mrpack` file.
/// See https://docs.modrinth.com/modpacks/format for the format this
/// mirrors.
#[derive(Debug, Deserialize)]
struct MrpackIndex {
    game: String,
    name: String,
    #[serde(default)]
    files: Vec<MrpackFile>,
    #[serde(default)]
    dependencies: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct MrpackFile {
    path: String,
    hashes: MrpackHashes,
    #[serde(default)]
    env: Option<MrpackEnv>,
    #[serde(default)]
    downloads: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
struct MrpackHashes {
    sha1: Option<String>,
    sha512: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MrpackEnv {
    #[serde(default)]
    client: Option<String>,
}

fn parse_index(bytes: &[u8]) -> AppResult<MrpackIndex> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| AppError::new("This modpack file is corrupted.", e.to_string()))?;
    let index: MrpackIndex = {
        let mut entry = archive.by_name("modrinth.index.json").map_err(|_| {
            AppError::new(
                "This doesn't look like a valid Modrinth modpack file.",
                "modrinth.index.json missing from mrpack",
            )
        })?;
        let mut buf = String::new();
        entry.read_to_string(&mut buf)?;
        serde_json::from_str(&buf).map_err(|e| {
            AppError::new("This modpack's manifest could not be read.", e.to_string())
        })?
    };
    if index.game != "minecraft" {
        return Err(AppError::new(
            "This modpack isn't for Minecraft.",
            format!("mrpack game field was {:?}", index.game),
        ));
    }
    Ok(index)
}

fn pack_loader(index: &MrpackIndex) -> (ModLoaderKind, Option<String>) {
    if let Some(v) = index.dependencies.get("fabric-loader") {
        (ModLoaderKind::Fabric, Some(v.clone()))
    } else if let Some(v) = index.dependencies.get("quilt-loader") {
        (ModLoaderKind::Quilt, Some(v.clone()))
    } else if let Some(v) = index.dependencies.get("forge") {
        (ModLoaderKind::Forge, Some(v.clone()))
    } else if let Some(v) = index.dependencies.get("neoforge") {
        (ModLoaderKind::NeoForge, Some(v.clone()))
    } else {
        (ModLoaderKind::Vanilla, None)
    }
}

/// Infers which content folder a pack file belongs in from its own declared
/// path (`mods/`, `resourcepacks/`, `shaderpacks/`) — the same folder names
/// `ProjectType::install_dir_name` uses, since `.mrpack` files are placed at
/// their literal path under the instance root. Anything else (configs,
/// datapacks, ...) is a real file on disk but isn't a "mod" to track.
fn project_type_from_path(path: &str) -> Option<ProjectType> {
    if path.starts_with("mods/") {
        Some(ProjectType::Mod)
    } else if path.starts_with("resourcepacks/") {
        Some(ProjectType::ResourcePack)
    } else if path.starts_with("shaderpacks/") {
        Some(ProjectType::Shader)
    } else {
        None
    }
}

/// Modrinth's CDN serves every file at
/// `https://cdn.modrinth.com/data/{project_id}/versions/{version_id}/{filename}`
/// (verified against the live API) — `.mrpack` files reference this same
/// CDN directly, so the ids can be recovered from the URL alone without an
/// extra API round trip per file. Anything hosted elsewhere (e.g. a
/// GitHub-released file some packs reference) simply isn't tracked as a
/// manageable mod, rather than inventing an id for it.
fn parse_modrinth_cdn_ids(url: &str) -> Option<(String, String)> {
    let rest = url.split("cdn.modrinth.com/data/").nth(1)?;
    let mut segments = rest.split('/');
    let project_id = segments.next()?.to_string();
    if segments.next()? != "versions" {
        return None;
    }
    let version_id = segments.next()?.to_string();
    if project_id.is_empty() || version_id.is_empty() {
        return None;
    }
    Some((project_id, version_id))
}

/// Records one pack-bundled file as a real, manageable `InstalledMod` (so it
/// shows up, can be enabled/disabled, and is included in update checks) —
/// best-effort only: a file that isn't Modrinth-hosted, or isn't under a
/// recognized content folder, is still installed on disk and works fine in
/// game, it's just not tracked in the mod manager.
async fn track_pack_file(instance: &MinecraftInstance, file: &MrpackFile) -> AppResult<()> {
    let Some(project_type) = project_type_from_path(&file.path) else {
        return Ok(());
    };
    let Some(url) = file.downloads.first() else {
        return Ok(());
    };
    let Some((project_id, version_id)) = parse_modrinth_cdn_ids(url) else {
        return Ok(());
    };
    let file_name = PathBuf::from(&file.path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| file.path.clone());

    instance_mods::record_install(
        instance,
        InstalledMod {
            id: Uuid::new_v4().to_string(),
            project_id,
            version_id,
            source: ModSource::Modrinth,
            project_type,
            name: file_name.clone(),
            version_number: String::new(),
            loader: instance.loader.id().to_string(),
            minecraft_version: instance.minecraft_version.clone(),
            author: String::new(),
            description: String::new(),
            icon_url: None,
            file_name,
            enabled: true,
            installed_at: now_secs(),
            dependencies: Vec::new(),
            sha1: file.hashes.sha1.clone(),
            world: None,
            from_modpack: true,
        },
    )
    .await
}

/// Downloads every file the pack declares and extracts its
/// `overrides/`/`client-overrides/` folders (configs, default resource
/// packs, etc.) directly into the instance directory. Used for both a fresh
/// install and an in-place pack update — an update simply re-runs this
/// against the new version's files, overwriting what changed. Files that
/// existed under an older pack version but are no longer listed are left in
/// place rather than guessed-removed, since deleting the wrong thing is far
/// worse than leaving a stale file behind.
async fn apply_pack_files(
    bytes: &[u8],
    index: &MrpackIndex,
    downloads: &DownloadManager,
    instance: &MinecraftInstance,
) -> AppResult<()> {
    let instance_dir = PathBuf::from(&instance.game_dir);

    for f in &index.files {
        let is_client_supported = f
            .env
            .as_ref()
            .and_then(|e| e.client.as_deref())
            .is_none_or(|c| !c.eq_ignore_ascii_case("unsupported"));
        if !is_client_supported {
            continue;
        }
        let Some(dest) = safe_join(&instance_dir, &f.path) else {
            continue;
        };
        let Some(url) = f.downloads.first() else {
            continue;
        };
        downloads
            .download_file(
                &f.path,
                url,
                dest,
                ExpectedHash {
                    sha1: f.hashes.sha1.clone(),
                    sha256: None,
                    sha512: f.hashes.sha512.clone(),
                },
                Some(instance.id.clone()),
            )
            .await?;

        // Best-effort: a file this can't identify (non-Modrinth host, or
        // outside mods/resourcepacks/shaderpacks) is still downloaded and
        // works in game, it just won't show up in the mod manager.
        let _ = track_pack_file(instance, f).await;
    }

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| AppError::new("This modpack file is corrupted.", e.to_string()))?;
    for prefix in ["overrides/", "client-overrides/"] {
        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| AppError::new("This modpack file is corrupted.", e.to_string()))?;
            let name = entry
                .name()
                .map_err(|e| AppError::new("This modpack file is corrupted.", e.to_string()))?
                .into_owned();
            if !name.starts_with(prefix) || name.ends_with('/') {
                continue;
            }
            let relative = &name[prefix.len()..];
            let Some(out_path) = safe_join(&instance_dir, relative) else {
                continue;
            };
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out_file = std::fs::File::create(&out_path)?;
            std::io::copy(&mut entry, &mut out_file)?;
        }
    }

    Ok(())
}

async fn download_mrpack_bytes(
    downloads: &DownloadManager,
    paths: &AppPaths,
    label: &str,
    version: &ModVersion,
) -> AppResult<Vec<u8>> {
    let file = version
        .files
        .iter()
        .find(|f| f.primary)
        .or_else(|| version.files.first())
        .ok_or_else(|| {
            AppError::new(
                "No downloadable file was found for this modpack.",
                "no files on version",
            )
        })?;

    let temp_path = paths
        .downloads_temp_dir()
        .join(format!("{}.mrpack", Uuid::new_v4()));
    downloads
        .download_file(
            label,
            &file.url,
            temp_path.clone(),
            ExpectedHash {
                sha1: file.sha1.clone(),
                sha256: None,
                sha512: file.sha512.clone(),
            },
            None,
        )
        .await?;
    let bytes = tokio::fs::read(&temp_path).await?;
    let _ = tokio::fs::remove_file(&temp_path).await;
    Ok(bytes)
}

/// Downloads a modpack's `.mrpack` file, parses its manifest, and turns it
/// into a brand-new, fully populated instance: creates the instance with
/// the Minecraft version/loader the pack declares, then applies its files
/// and overrides. Records the pack's project/version id on the instance so
/// [`check_for_update`] can later tell when a newer release is published.
pub async fn install_modpack(
    providers: &ProviderRegistry,
    downloads: &DownloadManager,
    instances: &InstanceManager,
    paths: &AppPaths,
    source: ModSource,
    project_id: &str,
    version_id: &str,
) -> AppResult<MinecraftInstance> {
    let provider = providers.get(source);
    let version = provider.get_version(version_id).await?;
    let project = provider.get_project(project_id).await.ok();

    let pack_display_name = project
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_else(|| version.name.clone());
    let pack_icon_url = project.as_ref().and_then(|p| p.icon_url.clone());

    let bytes = download_mrpack_bytes(
        downloads,
        paths,
        &format!("{pack_display_name} (modpack)"),
        &version,
    )
    .await?;
    let index = parse_index(&bytes)?;

    let minecraft_version = index
        .dependencies
        .get("minecraft")
        .cloned()
        .ok_or_else(|| {
            AppError::new(
                "This modpack doesn't specify a Minecraft version.",
                "mrpack missing minecraft dependency",
            )
        })?;
    let (loader, loader_version) = pack_loader(&index);
    let instance_name = if index.name.trim().is_empty() {
        pack_display_name
    } else {
        index.name.clone()
    };

    let mut instance = instances
        .create(CreateInstanceRequest {
            name: instance_name,
            minecraft_version,
            loader,
            loader_version,
            existing_game_dir: None,
            java_path: None,
        })
        .await?;
    instance.icon = pack_icon_url;
    instance.modpack_source = Some(source);
    instance.modpack_project_id = Some(project_id.to_string());
    instance.modpack_version_id = Some(version_id.to_string());
    instance = instances.update(instance).await?;

    apply_pack_files(&bytes, &index, downloads, &instance).await?;
    Ok(instance)
}

/// What changed in the mod tracker across a pack update, by comparing the
/// tracked mods before and after re-applying the new version's files —
/// no separate bookkeeping needed, just a before/after diff of real state.
/// Mods that dropped out of the new manifest aren't reported as "removed":
/// their files are deliberately left in place (see `apply_pack_files`), so
/// they're still tracked and nothing was actually taken away.
#[derive(Debug, Clone, Serialize)]
pub struct ModpackUpdateOutcome {
    pub instance: MinecraftInstance,
    pub added: Vec<String>,
    pub updated: Vec<String>,
}

/// Re-applies a newer version of the same pack onto an existing instance:
/// updates its Minecraft version/loader if the pack changed them, then
/// downloads the new file set and re-extracts overrides. Additive/overwrite
/// only — see [`apply_pack_files`] for why stale files aren't removed.
pub async fn update_modpack(
    providers: &ProviderRegistry,
    downloads: &DownloadManager,
    instances: &InstanceManager,
    paths: &AppPaths,
    instance: MinecraftInstance,
    version_id: &str,
) -> AppResult<ModpackUpdateOutcome> {
    let source = instance.modpack_source.ok_or_else(|| {
        AppError::new(
            "This instance wasn't created from a modpack.",
            "update_modpack called on an instance with no modpack_source",
        )
    })?;
    let provider = providers.get(source);
    let version = provider.get_version(version_id).await?;

    let bytes = download_mrpack_bytes(
        downloads,
        paths,
        &format!("{} (update)", instance.name),
        &version,
    )
    .await?;
    let index = parse_index(&bytes)?;
    let (loader, loader_version) = pack_loader(&index);

    let mut updated = instance;
    if let Some(minecraft_version) = index.dependencies.get("minecraft") {
        updated.minecraft_version = minecraft_version.clone();
    }
    updated.loader = loader;
    updated.loader_version = loader_version;
    updated.modpack_version_id = Some(version_id.to_string());
    let updated = instances.update(updated).await?;

    let before: HashMap<String, String> = instance_mods::list_installed(&updated)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|m| (m.project_id, m.version_id))
        .collect();

    apply_pack_files(&bytes, &index, downloads, &updated).await?;

    let mut added = Vec::new();
    let mut changed = Vec::new();
    for m in instance_mods::list_installed(&updated)
        .await
        .unwrap_or_default()
    {
        match before.get(&m.project_id) {
            None => added.push(m.name),
            Some(prev_version_id) if prev_version_id != &m.version_id => changed.push(m.name),
            _ => {}
        }
    }

    Ok(ModpackUpdateOutcome {
        instance: updated,
        added,
        updated: changed,
    })
}

/// The latest published version of this instance's source pack, if it's
/// newer than what's installed. `None` means either this instance wasn't
/// created from a modpack, or it's already on the latest version.
pub async fn check_for_update(
    providers: &ProviderRegistry,
    instance: &MinecraftInstance,
) -> AppResult<Option<ModVersion>> {
    let (Some(source), Some(project_id)) = (
        instance.modpack_source,
        instance.modpack_project_id.as_deref(),
    ) else {
        return Ok(None);
    };
    let provider = providers.get(source);
    let mut versions = provider
        .get_versions(project_id, None, None, ProjectType::Modpack)
        .await?;
    versions.sort_by(|a, b| b.date_published.cmp(&a.date_published));
    Ok(versions
        .into_iter()
        .find(|v| Some(v.id.as_str()) != instance.modpack_version_id.as_deref()))
}
