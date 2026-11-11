use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::models::common::{ModLoaderKind, ProjectType};
use crate::models::instance::MinecraftInstance;
use crate::paths::AppPaths;
use crate::services::cache::store::now_secs;
use crate::services::download::manager::DownloadManager;
use crate::services::instance::install::{install_mod, InstallModRequest};
use crate::services::instance::manager::InstanceManager;
use crate::services::instance::mods as instance_mods;
use crate::services::instance::worlds;
use crate::services::loaders::LoaderRegistry;
use crate::services::minecraft::manifest::VersionManifestService;
use crate::services::minecraft::MinecraftEngine;
use crate::services::providers::ProviderRegistry;

// --- Types -----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompatibilityStatus {
    /// The currently-installed version already works for the target.
    Compatible,
    UpdateAvailable,
    DowngradeAvailable,
    /// A real check ran and found nothing that fits.
    Incompatible,
    /// The check itself couldn't be completed (unlinked/local file, network
    /// error, provider unreachable) — never treated as a pass.
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentMigration {
    pub installed_id: String,
    pub project_id: String,
    pub source: crate::models::common::ModSource,
    pub project_type: ProjectType,
    pub name: String,
    pub current_version_number: String,
    pub current_version_id: String,
    pub status: CompatibilityStatus,
    pub resolved_version_id: Option<String>,
    pub resolved_version_number: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldWarning {
    pub folder_name: String,
    pub display_name: String,
    /// The real Minecraft version (from the world's own level.dat) that
    /// last opened it — `None` if it couldn't be determined.
    pub last_version: Option<String>,
    /// True only when we could actually confirm (via the version manifest)
    /// that the target version is older than `last_version` — never
    /// guessed from a missing comparison.
    pub target_is_older: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationPlan {
    pub source_version: String,
    pub target_version: String,
    pub source_loader: ModLoaderKind,
    pub target_loader: ModLoaderKind,
    pub target_loader_version: Option<String>,
    pub mods: Vec<ContentMigration>,
    pub resourcepacks: Vec<ContentMigration>,
    pub shaders: Vec<ContentMigration>,
    pub world_warnings: Vec<WorldWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotRecord {
    pub id: String,
    pub created_at: u64,
    pub reason: String,
    pub minecraft_version: String,
    pub loader: ModLoaderKind,
    pub loader_version: Option<String>,
    pub zip_path: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MigrationReport {
    pub mods_unchanged: u32,
    pub mods_updated: u32,
    pub mods_disabled: u32,
    pub mods_unknown: u32,
    pub resourcepacks_compatible: u32,
    pub resourcepacks_unknown: u32,
    pub shaders_compatible: u32,
    pub shaders_unknown: u32,
    pub worlds_preserved: u32,
    pub snapshot_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationHistoryEntry {
    pub id: String,
    pub timestamp: u64,
    pub source_version: String,
    pub target_version: String,
    pub source_loader: ModLoaderKind,
    pub target_loader: ModLoaderKind,
    pub snapshot_id: String,
    pub report: MigrationReport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InProgressMarker {
    pub started_at: u64,
    pub source_version: String,
    pub target_version: String,
    pub snapshot_id: String,
}

// --- Storage layout ----------------------------------------------------
//
// Snapshots deliberately live *outside* the instance's own game_dir (under
// the app data root instead) so a snapshot zip never ends up inside a
// later snapshot of the same instance, and so a full-instance delete never
// silently drags its evolution history down with it.

fn evolution_dir(paths: &AppPaths, instance_id: &str) -> PathBuf {
    paths.root().join("evolution").join(crate::paths::sanitize_component(instance_id))
}

fn snapshots_index_path(paths: &AppPaths, instance_id: &str) -> PathBuf {
    evolution_dir(paths, instance_id).join("snapshots.json")
}

fn history_path(paths: &AppPaths, instance_id: &str) -> PathBuf {
    evolution_dir(paths, instance_id).join("history.json")
}

fn marker_path(paths: &AppPaths, instance_id: &str) -> PathBuf {
    evolution_dir(paths, instance_id).join("in_progress.json")
}

async fn read_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> T {
    match tokio::fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => T::default(),
    }
}

async fn write_json<T: Serialize>(path: &Path, value: &T) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let bytes = serde_json::to_vec_pretty(value)?;
    tokio::fs::write(path, bytes).await?;
    Ok(())
}

// --- Snapshots -----------------------------------------------------------

/// Zips the instance's entire `game_dir` (mods, config, saves, resource
/// packs, shaders, tracked-mod metadata — everything) into a standalone
/// archive, and records it in this instance's snapshot index. This is the
/// safety net every migration creates *before* touching anything.
pub async fn create_snapshot(paths: &AppPaths, instance: &MinecraftInstance, reason: impl Into<String>) -> AppResult<SnapshotRecord> {
    let id = uuid::Uuid::new_v4().simple().to_string();
    let created_at = now_secs();
    let zip_path = evolution_dir(paths, &instance.id).join(format!("{created_at}-{id}.zip"));
    let source_dir = PathBuf::from(&instance.game_dir);
    let zip_path_for_task = zip_path.clone();
    tokio::task::spawn_blocking(move || crate::services::fs_utils::zip_directory(&source_dir, &zip_path_for_task))
        .await
        .map_err(|e| AppError::new("Couldn't create a backup snapshot.", e.to_string()))??;

    let size_bytes = tokio::fs::metadata(&zip_path).await.map(|m| m.len()).unwrap_or(0);
    let record = SnapshotRecord {
        id,
        created_at,
        reason: reason.into(),
        minecraft_version: instance.minecraft_version.clone(),
        loader: instance.loader,
        loader_version: instance.loader_version.clone(),
        zip_path: zip_path.to_string_lossy().to_string(),
        size_bytes,
    };

    let index_path = snapshots_index_path(paths, &instance.id);
    let mut index: Vec<SnapshotRecord> = read_json(&index_path).await;
    index.push(record.clone());
    write_json(&index_path, &index).await?;

    Ok(record)
}

pub async fn list_snapshots(paths: &AppPaths, instance_id: &str) -> Vec<SnapshotRecord> {
    let mut index: Vec<SnapshotRecord> = read_json(&snapshots_index_path(paths, instance_id)).await;
    index.sort_by_key(|s| std::cmp::Reverse(s.created_at));
    index
}

pub async fn delete_snapshot(paths: &AppPaths, instance_id: &str, snapshot_id: &str) -> AppResult<()> {
    let index_path = snapshots_index_path(paths, instance_id);
    let mut index: Vec<SnapshotRecord> = read_json(&index_path).await;
    let Some(pos) = index.iter().position(|s| s.id == snapshot_id) else {
        return Err(AppError::not_found("This snapshot"));
    };
    let record = index.remove(pos);
    let _ = tokio::fs::remove_file(&record.zip_path).await;
    write_json(&index_path, &index).await
}

/// Restores an instance's mods/config/resource packs/shaders/tracked-mod
/// metadata/instance settings from a snapshot — deliberately *never*
/// touches the current `saves/` folder, so a rollback can never silently
/// discard world progress made after the snapshot was taken. Restoring
/// worlds too is a distinct, separately-invoked action (`restore_worlds`)
/// precisely because it has very different safety implications.
pub async fn rollback(
    paths: &AppPaths,
    instances: &InstanceManager,
    instance_id: &str,
    snapshot_id: &str,
) -> AppResult<MinecraftInstance> {
    let index = list_snapshots(paths, instance_id).await;
    let record = index
        .into_iter()
        .find(|s| s.id == snapshot_id)
        .ok_or_else(|| AppError::not_found("This snapshot"))?;

    let mut instance = instances.get(instance_id).await?;
    let game_dir = PathBuf::from(&instance.game_dir);
    let zip_path = PathBuf::from(&record.zip_path);

    tokio::task::spawn_blocking(move || restore_zip_excluding_saves(&zip_path, &game_dir))
        .await
        .map_err(|e| AppError::new("Couldn't restore this snapshot.", e.to_string()))??;

    instance.minecraft_version = record.minecraft_version;
    instance.loader = record.loader;
    instance.loader_version = record.loader_version;
    instances.update(instance.clone()).await?;
    Ok(instance)
}

fn restore_zip_excluding_saves(zip_path: &Path, game_dir: &Path) -> AppResult<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| AppError::new("Couldn't read this snapshot archive.", e.to_string()))?;

    // Clear everything except `saves/` before extracting, so files removed
    // since the snapshot (e.g. a mod that was deleted) don't linger.
    if let Ok(entries) = std::fs::read_dir(game_dir) {
        for entry in entries.flatten() {
            if entry.file_name() == "saves" {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                let _ = std::fs::remove_dir_all(&path);
            } else {
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| AppError::new("Couldn't read this snapshot archive.", e.to_string()))?;
        let Some(relative) = entry.enclosed_name() else { continue };
        if relative.starts_with("saves") {
            continue;
        }
        let dest = game_dir.join(&relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&dest)?;
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&dest)?;
        std::io::copy(&mut entry, &mut out)?;
    }
    Ok(())
}

// --- History & interrupted-migration recovery -----------------------------

pub async fn list_history(paths: &AppPaths, instance_id: &str) -> Vec<MigrationHistoryEntry> {
    let mut history: Vec<MigrationHistoryEntry> = read_json(&history_path(paths, instance_id)).await;
    history.sort_by_key(|h| std::cmp::Reverse(h.timestamp));
    history
}

async fn append_history(paths: &AppPaths, instance_id: &str, entry: MigrationHistoryEntry) -> AppResult<()> {
    let path = history_path(paths, instance_id);
    let mut history: Vec<MigrationHistoryEntry> = read_json(&path).await;
    history.push(entry);
    write_json(&path, &history).await
}

pub async fn check_interrupted(paths: &AppPaths, instance_id: &str) -> Option<InProgressMarker> {
    let path = marker_path(paths, instance_id);
    let bytes = tokio::fs::read(&path).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

async fn write_marker(paths: &AppPaths, instance_id: &str, marker: &InProgressMarker) -> AppResult<()> {
    write_json(&marker_path(paths, instance_id), marker).await
}

async fn clear_marker(paths: &AppPaths, instance_id: &str) {
    let _ = tokio::fs::remove_file(marker_path(paths, instance_id)).await;
}

/// Called from the frontend when `check_interrupted` found a marker left
/// behind by a migration that never finished (the app closed/crashed
/// mid-way). Resuming a partial download isn't attempted — each item was
/// already applied atomically (temp-download-then-move) by the same
/// pipeline every other install goes through, so nothing is ever
/// half-written; this only ever restores the pre-migration snapshot or
/// discards the marker, never anything less clear-cut.
pub async fn resolve_interrupted(
    paths: &AppPaths,
    instances: &InstanceManager,
    instance_id: &str,
    restore: bool,
) -> AppResult<Option<MinecraftInstance>> {
    let marker = check_interrupted(paths, instance_id)
        .await
        .ok_or_else(|| AppError::not_found("An interrupted migration"))?;
    clear_marker(paths, instance_id).await;
    if restore {
        Ok(Some(rollback(paths, instances, instance_id, &marker.snapshot_id).await?))
    } else {
        Ok(None)
    }
}

// --- Compatibility scan ----------------------------------------------------

fn empty_migration(installed: &crate::models::common::InstalledMod, status: CompatibilityStatus, reason: Option<String>) -> ContentMigration {
    ContentMigration {
        installed_id: installed.id.clone(),
        project_id: installed.project_id.clone(),
        source: installed.source,
        project_type: installed.project_type,
        name: installed.name.clone(),
        current_version_number: installed.version_number.clone(),
        current_version_id: installed.version_id.clone(),
        status,
        resolved_version_id: None,
        resolved_version_number: None,
        reason,
    }
}

async fn scan_one(
    installed: &crate::models::common::InstalledMod,
    target_version: &str,
    target_loader: ModLoaderKind,
    version_order: Option<std::cmp::Ordering>,
    providers: &ProviderRegistry,
) -> ContentMigration {
    if installed.project_id.starts_with("local:") {
        return empty_migration(
            installed,
            CompatibilityStatus::Unknown,
            Some("Not linked to Modrinth/CurseForge — can't check compatibility.".to_string()),
        );
    }

    let provider = providers.get(installed.source);
    let loader_filter = if installed.project_type.checks_loader_compatibility() {
        Some(target_loader.id())
    } else {
        None
    };

    match provider
        .get_versions(&installed.project_id, loader_filter, Some(target_version), installed.project_type)
        .await
    {
        Ok(mut versions) => {
            versions.sort_by(|a, b| b.date_published.cmp(&a.date_published));
            match versions.into_iter().next() {
                Some(v) if v.id == installed.version_id => {
                    let mut m = empty_migration(installed, CompatibilityStatus::Compatible, None);
                    m.resolved_version_id = Some(v.id);
                    m.resolved_version_number = Some(v.version_number);
                    m
                }
                Some(v) => {
                    let status = match version_order {
                        Some(std::cmp::Ordering::Greater) => CompatibilityStatus::UpdateAvailable,
                        Some(std::cmp::Ordering::Less) => CompatibilityStatus::DowngradeAvailable,
                        _ => CompatibilityStatus::UpdateAvailable,
                    };
                    let mut m = empty_migration(installed, status, None);
                    m.resolved_version_id = Some(v.id);
                    m.resolved_version_number = Some(v.version_number);
                    m
                }
                None => empty_migration(
                    installed,
                    CompatibilityStatus::Incompatible,
                    Some(format!("No version compatible with {target_version} was found.")),
                ),
            }
        }
        Err(e) => empty_migration(installed, CompatibilityStatus::Unknown, Some(e.message)),
    }
}

/// Scans every installed mod/resource pack/shader against a target
/// Minecraft version + loader and produces a deterministic migration plan —
/// this never modifies anything, it only reports. Nothing here is
/// fabricated: a mod whose compatibility can't actually be determined
/// (unlinked file, network failure) is reported `Unknown`, never guessed.
pub async fn scan(
    instance: &MinecraftInstance,
    target_version: &str,
    target_loader: ModLoaderKind,
    target_loader_version: Option<String>,
    providers: &ProviderRegistry,
    manifest: &VersionManifestService,
) -> AppResult<MigrationPlan> {
    let installed = instance_mods::list_installed(instance).await?;
    let version_order = manifest.compare_versions(target_version, &instance.minecraft_version).await;

    let mut mods = Vec::new();
    let mut resourcepacks = Vec::new();
    let mut shaders = Vec::new();

    for m in &installed {
        if m.project_type == ProjectType::Datapack {
            // Pure vanilla data with no loader/mod-version concept of its
            // own — never touched by a Minecraft-version change.
            continue;
        }
        let migration = scan_one(m, target_version, target_loader, version_order, providers).await;
        match m.project_type {
            ProjectType::ResourcePack => resourcepacks.push(migration),
            ProjectType::Shader => shaders.push(migration),
            _ => mods.push(migration),
        }
    }

    let mut world_warnings = Vec::new();
    if let Ok(world_list) = worlds::list_worlds(instance).await {
        for w in world_list {
            let Some(last_version) = worlds::last_opened_version(instance, &w.folder_name) else {
                continue;
            };
            let target_is_older = manifest
                .compare_versions(target_version, &last_version)
                .await
                .map(|o| o == std::cmp::Ordering::Less)
                .unwrap_or(false);
            if target_is_older {
                world_warnings.push(WorldWarning {
                    folder_name: w.folder_name,
                    display_name: w.display_name,
                    last_version: Some(last_version),
                    target_is_older,
                });
            }
        }
    }

    Ok(MigrationPlan {
        source_version: instance.minecraft_version.clone(),
        target_version: target_version.to_string(),
        source_loader: instance.loader,
        target_loader,
        target_loader_version,
        mods,
        resourcepacks,
        shaders,
        world_warnings,
    })
}

// --- Execution ---------------------------------------------------------

async fn apply_group(
    instance: &MinecraftInstance,
    items: &[ContentMigration],
    instances: &InstanceManager,
    providers: &ProviderRegistry,
    downloads: &DownloadManager,
    disable_unconfirmed: bool,
) -> (u32, u32, u32, u32) {
    let (mut unchanged, mut updated, mut disabled, mut unknown) = (0u32, 0u32, 0u32, 0u32);

    // For mods specifically (`disable_unconfirmed` is only ever true for
    // the mods group — see `execute_content_changes`), anything that isn't
    // confirmed either already-compatible or successfully replaced gets
    // disabled rather than left running. A mod loader hard-crashes the
    // *entire* game the moment any installed mod declares a version range
    // that doesn't include what's actually present — unlike a resource
    // pack or shader, which Minecraft just ignores gracefully if it turns
    // out wrong — so leaving a mod active on "we resolved a fix but the
    // download/install failed" or "we couldn't determine compatibility at
    // all" gambles with the same crash this whole system exists to
    // prevent. Resource packs/shaders never disable this way; an unknown
    // one is just reported, since being wrong there costs nothing worse
    // than the game ignoring it.
    let fall_back_to_disable = |disabled: &mut u32, unknown: &mut u32| {
        if disable_unconfirmed {
            *disabled += 1;
        } else {
            *unknown += 1;
        }
    };

    for cm in items {
        match cm.status {
            CompatibilityStatus::Compatible => unchanged += 1,
            CompatibilityStatus::UpdateAvailable | CompatibilityStatus::DowngradeAvailable => {
                let Some(version_id) = &cm.resolved_version_id else {
                    if disable_unconfirmed {
                        let _ = instance_mods::set_enabled(instance, &cm.installed_id, false).await;
                    }
                    fall_back_to_disable(&mut disabled, &mut unknown);
                    continue;
                };
                let req = InstallModRequest {
                    instance_id: instance.id.clone(),
                    project_id: cm.project_id.clone(),
                    version_id: Some(version_id.clone()),
                    source: cm.source,
                    project_type: cm.project_type,
                    world: None,
                };
                match install_mod(instances, providers, downloads, req, true).await {
                    Ok(_) => updated += 1,
                    Err(_) => {
                        if disable_unconfirmed {
                            let _ = instance_mods::set_enabled(instance, &cm.installed_id, false).await;
                        }
                        fall_back_to_disable(&mut disabled, &mut unknown);
                    }
                }
            }
            CompatibilityStatus::Incompatible | CompatibilityStatus::Unknown => {
                if disable_unconfirmed {
                    let _ = instance_mods::set_enabled(instance, &cm.installed_id, false).await;
                }
                fall_back_to_disable(&mut disabled, &mut unknown);
            }
        }
    }
    (unchanged, updated, disabled, unknown)
}

/// Executes a previously-scanned migration plan: snapshots the instance
/// first (always), downloads the target Minecraft client/libraries/assets
/// and installs the target loader (exactly what a normal launch would do
/// on demand, just done up front instead of deferred), then applies every
/// resolved mod/resource pack/shader change through the same install
/// pipeline every manual install already uses (temp-download, hash-verify,
/// then move — never a half-written file), disabling mods with no
/// compatible version rather than deleting them. Worlds are never touched.
/// If the client/loader step fails, the instance's version/loader fields
/// are never persisted and no mods are touched — only the (harmless,
/// extra) snapshot exists, so the original instance is left exactly as it
/// was.
#[allow(clippy::too_many_arguments)]
pub async fn execute(
    instance: &MinecraftInstance,
    plan: &MigrationPlan,
    instances: &InstanceManager,
    providers: &ProviderRegistry,
    downloads: &DownloadManager,
    engine: &MinecraftEngine,
    loaders: &LoaderRegistry,
    paths: &AppPaths,
) -> AppResult<MigrationReport> {
    let snapshot = create_snapshot(
        paths,
        instance,
        format!("Before migrating {} → {}", plan.source_version, plan.target_version),
    )
    .await?;

    write_marker(
        paths,
        &instance.id,
        &InProgressMarker {
            started_at: now_secs(),
            source_version: plan.source_version.clone(),
            target_version: plan.target_version.clone(),
            snapshot_id: snapshot.id.clone(),
        },
    )
    .await?;

    let result = execute_content_changes(instance, plan, instances, providers, downloads, engine, loaders, &snapshot).await;
    clear_marker(paths, &instance.id).await;
    let report = result?;

    append_history(
        paths,
        &instance.id,
        MigrationHistoryEntry {
            id: uuid::Uuid::new_v4().simple().to_string(),
            timestamp: now_secs(),
            source_version: plan.source_version.clone(),
            target_version: plan.target_version.clone(),
            source_loader: plan.source_loader,
            target_loader: plan.target_loader,
            snapshot_id: snapshot.id.clone(),
            report: report.clone(),
        },
    )
    .await?;

    Ok(report)
}

#[allow(clippy::too_many_arguments)]
async fn execute_content_changes(
    instance: &MinecraftInstance,
    plan: &MigrationPlan,
    instances: &InstanceManager,
    providers: &ProviderRegistry,
    downloads: &DownloadManager,
    engine: &MinecraftEngine,
    loaders: &LoaderRegistry,
    snapshot: &SnapshotRecord,
) -> AppResult<MigrationReport> {
    // Download the target Minecraft client/libraries/assets and install
    // the target loader *before* touching any mod, and before persisting
    // the version change — if this fails (offline, bad loader version),
    // nothing else about the instance has changed yet.
    let mut updated_instance = instance.clone();
    updated_instance.minecraft_version = plan.target_version.clone();
    updated_instance.loader = plan.target_loader;
    updated_instance.loader_version = plan.target_loader_version.clone();

    let version_key = crate::services::loaders::ensure_loader_ready(loaders, engine, instances, &mut updated_instance)
        .await
        .map_err(|e| AppError::new(format!("Couldn't prepare Minecraft {}: {}", plan.target_version, e.message), e.technical))?;
    engine.ensure_installed(&updated_instance, &version_key, false).await?;
    instances.update(updated_instance.clone()).await?;

    let (mods_unchanged, mods_updated, mods_disabled, mods_unknown) =
        apply_group(&updated_instance, &plan.mods, instances, providers, downloads, true).await;
    let (rp_unchanged, rp_updated, _, rp_unknown) =
        apply_group(&updated_instance, &plan.resourcepacks, instances, providers, downloads, false).await;
    let (sh_unchanged, sh_updated, _, sh_unknown) =
        apply_group(&updated_instance, &plan.shaders, instances, providers, downloads, false).await;

    let worlds_preserved = worlds::list_worlds(&updated_instance).await.map(|w| w.len() as u32).unwrap_or(0);

    Ok(MigrationReport {
        mods_unchanged,
        mods_updated,
        mods_disabled,
        mods_unknown,
        resourcepacks_compatible: rp_unchanged + rp_updated,
        resourcepacks_unknown: rp_unknown,
        shaders_compatible: sh_unchanged + sh_updated,
        shaders_unknown: sh_unknown,
        worlds_preserved,
        snapshot_id: snapshot.id.clone(),
    })
}
