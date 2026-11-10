use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::AppResult;
use crate::models::common::{InstalledMod, ModLoaderKind, ModSource, ProjectType};
use crate::models::content::{DiscoveredInstance, ImportSource};
use crate::models::instance::MinecraftInstance;
use crate::services::cache::store::now_secs;
use crate::services::instance::mods as instance_mods;

/// Well-known default data directories for each launcher, checked only if
/// they actually exist — manual folder selection always remains the
/// reliable fallback since portable installs can live anywhere.
pub fn candidate_roots(source: ImportSource) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let Some(home) = dirs_home() else {
        return candidates;
    };

    match source {
        ImportSource::PrismLauncher => {
            candidates.push(home.join("AppData/Roaming/PrismLauncher"));
            candidates.push(home.join("AppData/Local/PrismLauncher"));
            candidates.push(home.join("AppData/Roaming/MultiMC"));
            candidates.push(home.join(".local/share/PrismLauncher"));
            candidates.push(home.join("Library/Application Support/PrismLauncher"));
        }
        ImportSource::CurseForge => {
            candidates.push(home.join("curseforge/minecraft"));
            candidates.push(home.join("Documents/curseforge/minecraft"));
        }
        // TLauncher and SKlauncher (3.x-era, and 4.0's still-supported classic
        // layout) are both, at their core, forks of the official launcher UI
        // that share its exact `.minecraft` directory and
        // `launcher_profiles.json` schema — verified against TLauncher's own
        // support docs (tlauncher.org) and SKlauncher's own FAQ
        // (github.com/sklauncher/docs), rather than assumed.
        ImportSource::TLauncher | ImportSource::SKLauncher => {
            candidates.push(home.join("AppData/Roaming/.minecraft"));
            candidates.push(home.join(".minecraft"));
            candidates.push(home.join("Library/Application Support/minecraft"));
        }
        // Verified directly against a real installed copy: its app data
        // directory is literally named `ModrinthApp` (older third-party
        // write-ups say `com.modrinth.theseus`, which is stale) and holds an
        // `app.db` SQLite database plus a `profiles/<name>/` folder per
        // instance.
        ImportSource::ModrinthApp => {
            candidates.push(home.join("AppData/Roaming/ModrinthApp"));
            candidates.push(home.join(".local/share/ModrinthApp"));
            candidates.push(home.join("Library/Application Support/ModrinthApp"));
        }
    }

    candidates.into_iter().filter(|p| p.is_dir()).collect()
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

fn component_loader_from_uid(uid: &str) -> Option<ModLoaderKind> {
    match uid {
        "net.fabricmc.fabric-loader" => Some(ModLoaderKind::Fabric),
        "net.minecraftforge" => Some(ModLoaderKind::Forge),
        "net.neoforged" => Some(ModLoaderKind::NeoForge),
        "org.quiltmc.quilt-loader" => Some(ModLoaderKind::Quilt),
        _ => None,
    }
}

/// Parses a PrismLauncher/MultiMC `instance.cfg` — a flat `key=value` file
/// (optionally with a `[General]` header) — well enough to pull out the
/// display name; everything else this importer needs lives in
/// `mmc-pack.json` instead.
fn read_instance_cfg_name(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == "name" {
                return Some(value.trim().to_string());
            }
        }
    }
    None
}

#[derive(Deserialize)]
struct MmcPack {
    #[serde(default)]
    components: Vec<MmcComponent>,
}

#[derive(Deserialize)]
struct MmcComponent {
    uid: String,
    #[serde(default)]
    version: String,
    #[serde(default, rename = "cachedVersion")]
    cached_version: String,
}

/// Scans `<root>/instances/*/` for PrismLauncher/MultiMC instances by
/// reading each one's `mmc-pack.json` (component list: `net.minecraft` plus
/// whichever loader component is present) and `instance.cfg` (display
/// name). Mirrors PrismLauncher's own `gameRoot()` — prefer `.minecraft`,
/// fall back to `minecraft`.
pub fn scan_prism_style(root: &Path) -> Vec<DiscoveredInstance> {
    let instances_dir = root.join("instances");
    let Ok(entries) = std::fs::read_dir(&instances_dir) else {
        return Vec::new();
    };

    let mut found = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Ok(pack_bytes) = std::fs::read(dir.join("mmc-pack.json")) else {
            continue;
        };
        let Ok(pack) = serde_json::from_slice::<MmcPack>(&pack_bytes) else {
            continue;
        };

        let mut minecraft_version = None;
        let mut loader = ModLoaderKind::Vanilla;
        let mut loader_version = None;
        for component in &pack.components {
            let version = if component.version.is_empty() {
                component.cached_version.clone()
            } else {
                component.version.clone()
            };
            if component.uid == "net.minecraft" {
                minecraft_version = Some(version);
            } else if let Some(kind) = component_loader_from_uid(&component.uid) {
                loader = kind;
                loader_version = Some(version);
            }
        }
        let Some(minecraft_version) = minecraft_version else {
            continue;
        };

        let dot_minecraft = dir.join(".minecraft");
        let plain_minecraft = dir.join("minecraft");
        let game_dir = if dot_minecraft.is_dir() && !plain_minecraft.is_dir() {
            dot_minecraft
        } else {
            plain_minecraft
        };
        if !game_dir.is_dir() {
            continue;
        }

        let name = read_instance_cfg_name(&dir.join("instance.cfg"))
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| entry.file_name().to_string_lossy().to_string());

        found.push(DiscoveredInstance {
            source: ImportSource::PrismLauncher,
            name,
            minecraft_version,
            loader,
            loader_version,
            game_dir: game_dir.to_string_lossy().to_string(),
            root_path: dir.to_string_lossy().to_string(),
        });
    }
    found
}

/// CurseForge's desktop app format is undocumented (closed source); this
/// mirrors the field names used by community conversion tools (e.g.
/// `minecraftinstance.json`'s `name` / `baseModLoader.minecraftVersion`).
/// Loader detection is deliberately conservative: if the loader string
/// doesn't match a known prefix, this reports Vanilla rather than guessing,
/// since a wrong loader is worse than none — either way the instance still
/// imports and is playable, and the loader can be corrected afterward in
/// Configure.
fn detect_curseforge_loader(value: &serde_json::Value) -> (ModLoaderKind, Option<String>) {
    let Some(base) = value.get("baseModLoader") else {
        return (ModLoaderKind::Vanilla, None);
    };
    if let Some(name) = base.get("name").and_then(|v| v.as_str()) {
        for (prefix, kind) in [
            ("forge-", ModLoaderKind::Forge),
            ("neoforge-", ModLoaderKind::NeoForge),
            ("fabric-", ModLoaderKind::Fabric),
            ("quilt-", ModLoaderKind::Quilt),
        ] {
            if let Some(version) = name.strip_prefix(prefix) {
                return (kind, Some(version.to_string()));
            }
        }
    }
    if let Some(forge_version) = base.get("forgeVersion").and_then(|v| v.as_str()) {
        if !forge_version.is_empty() {
            return (ModLoaderKind::Forge, Some(forge_version.to_string()));
        }
    }
    (ModLoaderKind::Vanilla, None)
}

pub fn scan_curseforge_style(root: &Path) -> Vec<DiscoveredInstance> {
    let instances_dir = if root.join("Instances").is_dir() {
        root.join("Instances")
    } else {
        root.to_path_buf()
    };
    let Ok(entries) = std::fs::read_dir(&instances_dir) else {
        return Vec::new();
    };

    let mut found = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Ok(bytes) = std::fs::read(dir.join("minecraftinstance.json")) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };

        let Some(minecraft_version) = value
            .get("baseModLoader")
            .and_then(|b| b.get("minecraftVersion"))
            .and_then(|v| v.as_str())
        else {
            continue;
        };
        let name = value
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|n| !n.trim().is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| entry.file_name().to_string_lossy().to_string());
        let (loader, loader_version) = detect_curseforge_loader(&value);

        found.push(DiscoveredInstance {
            source: ImportSource::CurseForge,
            name,
            minecraft_version: minecraft_version.to_string(),
            loader,
            loader_version,
            game_dir: dir.to_string_lossy().to_string(),
            root_path: dir.to_string_lossy().to_string(),
        });
    }
    found
}

/// Reads the standard (Mojang-format) `launcher_profiles.json` used by the
/// official launcher and its forks (TLauncher, SKlauncher). Returns
/// `(display name, resolved game dir, version id)` for every profile whose
/// `lastVersionId` resolves to a real version folder — which filters out the
/// "Latest release"/"Latest snapshot" profiles (their `lastVersionId` is a
/// placeholder token); the versions those point at are picked up by
/// [`installed_versions`] instead.
///
/// Read leniently, profile by profile: one oddly-typed field written by some
/// launcher must not hide every other profile, and a UTF-8 BOM (written by
/// some Windows tools) must not make the whole file unreadable.
fn read_standard_profiles(minecraft_root: &Path) -> Vec<(String, PathBuf, String)> {
    let mut found: Vec<(String, PathBuf, String)> = Vec::new();
    // The Microsoft Store / Xbox app build of the official launcher has used
    // its own file name.
    for file_name in ["launcher_profiles.json", "launcher_profiles_microsoft_store.json"] {
        let Ok(bytes) = std::fs::read(minecraft_root.join(file_name)) else {
            continue;
        };
        let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
        let Ok(file) = serde_json::from_slice::<serde_json::Value>(bytes) else {
            log::warn!("import: {file_name} in {} isn't valid JSON", minecraft_root.display());
            continue;
        };
        let Some(profiles) = file.get("profiles").and_then(|p| p.as_object()) else {
            continue;
        };
        for (key, profile) in profiles {
            let text = |field: &str| {
                profile
                    .get(field)
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
            };
            let Some(version_id) = text("lastVersionId") else {
                continue;
            };
            let game_dir = text("gameDir")
                .map(PathBuf::from)
                .filter(|p| p.is_dir())
                .unwrap_or_else(|| minecraft_root.to_path_buf());
            if !game_dir.join("versions").join(version_id).is_dir() {
                continue;
            }
            let name = text("name").map(str::to_string).unwrap_or_else(|| key.clone());
            if !found.iter().any(|(n, d, v)| n == &name && d == &game_dir && v == version_id) {
                found.push((name, game_dir, version_id.to_string()));
            }
        }
    }
    found
}

/// Version folders under `<game_dir>/versions` that are actually installed:
/// a `<id>.json` plus either the client jar or (for Fabric/Forge/...
/// profiles, which have no jar of their own) an installed parent version.
/// A folder with only the JSON is a version the launcher looked at but
/// never downloaded, so it's skipped. Newest first.
fn installed_versions(game_dir: &Path) -> Vec<String> {
    let versions_dir = game_dir.join("versions");
    let Ok(entries) = std::fs::read_dir(&versions_dir) else {
        return Vec::new();
    };
    let has_jar = |id: &str| versions_dir.join(id).join(format!("{id}.jar")).is_file();
    let mut found: Vec<(String, std::time::SystemTime)> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let id = e.file_name().to_string_lossy().to_string();
            let bytes = std::fs::read(e.path().join(format!("{id}.json"))).ok()?;
            let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
            let parent = json.get("inheritsFrom").and_then(|v| v.as_str());
            let installed = has_jar(&id) || parent.is_some_and(has_jar);
            let modified = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            installed.then_some((id, modified))
        })
        .collect();
    found.sort_by_key(|(_, modified)| std::cmp::Reverse(*modified));
    found.into_iter().map(|(id, _)| id).collect()
}

/// Everything playable in a Mojang-style `.minecraft`: its named profiles,
/// plus each installed version no profile points at (a version installed
/// from TLauncher's / SKlauncher's version list, or the one behind "Latest
/// release"), named after the version.
fn scan_standard_minecraft(minecraft_root: &Path, source: ImportSource) -> Vec<DiscoveredInstance> {
    let profiles = read_standard_profiles(minecraft_root);
    let mut entries = profiles.clone();
    for version_id in installed_versions(minecraft_root) {
        let covered = profiles
            .iter()
            .any(|(_, dir, v)| dir.as_path() == minecraft_root && v == &version_id);
        if !covered {
            entries.push((format!("Minecraft {version_id}"), minecraft_root.to_path_buf(), version_id));
        }
    }
    entries
        .into_iter()
        .filter_map(|(name, game_dir, version_id)| {
            let (minecraft_version, loader, loader_version) = read_version_json(&game_dir, &version_id)?;
            Some(DiscoveredInstance {
                source,
                name,
                minecraft_version,
                loader,
                loader_version,
                game_dir: game_dir.to_string_lossy().to_string(),
                // Unique per entry: several profiles can share one folder.
                root_path: format!("{}#{version_id}", game_dir.to_string_lossy()),
            })
        })
        .collect()
}

#[derive(Deserialize)]
struct StandardVersionJson {
    id: String,
    #[serde(default, rename = "inheritsFrom")]
    inherits_from: Option<String>,
    #[serde(default)]
    libraries: Vec<StandardLibrary>,
}

#[derive(Deserialize)]
struct StandardLibrary {
    #[serde(default)]
    name: String,
}

/// Determines the real Minecraft version and mod loader for a version
/// folder by reading its own standard version JSON — the same file format
/// Minecraft itself reads to launch — rather than trusting any particular
/// launcher's own bookkeeping. A library's Maven coordinate group/artifact
/// (e.g. `net.fabricmc:fabric-loader:0.15.11-1.20.1`) is the reliable, public
/// signal every real installer writes; an unrecognized `inheritsFrom` still
/// reports Vanilla rather than guessing, since a wrong loader is worse than
/// none and the instance is still playable either way.
fn read_version_json(game_dir: &Path, version_id: &str) -> Option<(String, ModLoaderKind, Option<String>)> {
    let path = game_dir.join("versions").join(version_id).join(format!("{version_id}.json"));
    let bytes = std::fs::read(path).ok()?;
    let version: StandardVersionJson = serde_json::from_slice(&bytes).ok()?;
    let minecraft_version = version.inherits_from.unwrap_or_else(|| version.id.clone());

    for lib in &version.libraries {
        let mut parts = lib.name.splitn(3, ':');
        let (Some(group), Some(artifact), Some(lib_version)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let kind = match (group, artifact) {
            ("net.fabricmc", "fabric-loader") => Some(ModLoaderKind::Fabric),
            ("net.minecraftforge", "forge") => Some(ModLoaderKind::Forge),
            ("net.neoforged", "neoforge" | "fancymodloader") => Some(ModLoaderKind::NeoForge),
            ("org.quiltmc", "quilt-loader") => Some(ModLoaderKind::Quilt),
            _ => None,
        };
        if let Some(kind) = kind {
            return Some((minecraft_version, kind, Some(lib_version.to_string())));
        }
    }
    Some((minecraft_version, ModLoaderKind::Vanilla, None))
}

/// TLauncher and the official launcher have no isolated per-instance
/// folders — every profile shares the one `.minecraft` unless it sets a
/// custom `gameDir` — so "instances" here are its profiles and installed
/// versions, each fully playable on its own.
pub fn scan_tlauncher_style(minecraft_root: &Path) -> Vec<DiscoveredInstance> {
    scan_standard_minecraft(minecraft_root, ImportSource::TLauncher)
}

/// SKlauncher's classic layout keeps each custom profile in its own
/// self-contained folder under `.minecraft/skprofiles/<name>/` — a real,
/// isolated `.minecraft`-equivalent with its own `versions/` — verified
/// against SKlauncher's own FAQ (github.com/sklauncher/docs). The main
/// `launcher_profiles.json` is consulted only for a nicer display name;
/// the folder itself is the source of truth for what's actually installed.
///
/// SKlauncher 4.x no longer creates `skprofiles/`: its profiles live in the
/// shared `launcher_profiles.json` like the official launcher's (seen on a
/// real 4.x install), so those are scanned too.
pub fn scan_sklauncher_style(minecraft_root: &Path) -> Vec<DiscoveredInstance> {
    let mut found = scan_skprofiles(minecraft_root);
    let shared: Vec<DiscoveredInstance> = scan_standard_minecraft(minecraft_root, ImportSource::SKLauncher)
        .into_iter()
        .filter(|d| !found.iter().any(|f| f.game_dir == d.game_dir))
        .collect();
    found.extend(shared);
    found
}

fn scan_skprofiles(minecraft_root: &Path) -> Vec<DiscoveredInstance> {
    let skprofiles_dir = minecraft_root.join("skprofiles");
    let Ok(entries) = std::fs::read_dir(&skprofiles_dir) else {
        return Vec::new();
    };
    let known_names = read_standard_profiles(minecraft_root);

    let mut found = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let versions_dir = dir.join("versions");
        let Ok(version_dirs) = std::fs::read_dir(&versions_dir) else {
            continue;
        };
        // A profile folder normally has exactly one installed version; if
        // more accumulated over time, the most recently modified one is the
        // one actually last played.
        let latest_version_id = version_dirs
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| {
                let modified = e.metadata().and_then(|m| m.modified()).ok()?;
                Some((e.file_name().to_string_lossy().to_string(), modified))
            })
            .max_by_key(|(_, modified)| *modified)
            .map(|(id, _)| id);
        let Some(version_id) = latest_version_id else {
            continue;
        };
        let Some((minecraft_version, loader, loader_version)) = read_version_json(&dir, &version_id) else {
            continue;
        };

        let folder_name = entry.file_name().to_string_lossy().to_string();
        let name = known_names
            .iter()
            .find(|(_, game_dir, _)| game_dir == &dir)
            .map(|(name, _, _)| name.clone())
            .unwrap_or(folder_name);

        found.push(DiscoveredInstance {
            source: ImportSource::SKLauncher,
            name,
            minecraft_version,
            loader,
            loader_version,
            game_dir: dir.to_string_lossy().to_string(),
            root_path: dir.to_string_lossy().to_string(),
        });
    }
    found
}

fn modrinth_loader_kind(loader: &str) -> ModLoaderKind {
    match loader {
        "fabric" => ModLoaderKind::Fabric,
        "forge" => ModLoaderKind::Forge,
        "neoforge" => ModLoaderKind::NeoForge,
        "quilt" => ModLoaderKind::Quilt,
        // Includes "vanilla" and anything unrecognized — a wrong loader is
        // worse than none, and the instance still imports and plays either
        // way (same conservative rule the CurseForge importer above uses).
        _ => ModLoaderKind::Vanilla,
    }
}

/// Reads the Modrinth App's real SQLite database (`app.db`) directly —
/// verified against an actual installed copy rather than guessed from
/// (partly outdated) third-party write-ups, since the app's own on-disk
/// format moved from a simple per-instance `profile.json` to this database
/// at some point. `instances.path` is the instance's folder name relative to
/// `profiles/`; its Minecraft version and loader live on the
/// `instance_content_sets` row its `applied_content_set_id` points at, not
/// on the instance row itself. Opened strictly read-only — this only ever
/// reads, and SQLite's WAL mode (which the app uses) supports concurrent
/// readers alongside the app itself still running.
pub fn scan_modrinth_app_style(root: &Path) -> Vec<DiscoveredInstance> {
    let db_path = root.join("app.db");
    if !db_path.is_file() {
        return Vec::new();
    }
    let Ok(conn) = rusqlite::Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return Vec::new();
    };

    let query = "
        SELECT i.name, i.path, cs.game_version, cs.loader, cs.loader_version
        FROM instances i
        JOIN instance_content_sets cs ON cs.id = i.applied_content_set_id
    ";
    let Ok(mut stmt) = conn.prepare(query) else {
        return Vec::new();
    };
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    });
    let Ok(rows) = rows else {
        return Vec::new();
    };

    let profiles_dir = root.join("profiles");
    let mut found = Vec::new();
    for row in rows.flatten() {
        let (name, path, minecraft_version, loader, loader_version) = row;
        let game_dir = profiles_dir.join(&path);
        if !game_dir.is_dir() {
            continue;
        }
        found.push(DiscoveredInstance {
            source: ImportSource::ModrinthApp,
            name,
            minecraft_version,
            loader: modrinth_loader_kind(&loader),
            loader_version,
            game_dir: game_dir.to_string_lossy().to_string(),
            root_path: game_dir.to_string_lossy().to_string(),
        });
    }
    found
}

/// Accepts the folders people commonly pick instead of the exact one a
/// scanner wants: `AppData\Roaming` (holds `.minecraft`), a subfolder like
/// `.minecraft\versions` or `saves`, PrismLauncher's `instances` folder, or
/// the Modrinth App's `profiles` folder.
fn normalize_picked_root(source: ImportSource, picked: &Path) -> PathBuf {
    let parent = || picked.parent().map(Path::to_path_buf);
    let named = |names: &[&str]| {
        picked
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
            .is_some_and(|n| names.contains(&n.as_str()))
    };
    let resolved = match source {
        ImportSource::TLauncher | ImportSource::SKLauncher => {
            let is_minecraft_root =
                |p: &Path| p.join("launcher_profiles.json").is_file() || p.join("versions").is_dir();
            if is_minecraft_root(picked) {
                None
            } else if picked.join(".minecraft").is_dir() {
                Some(picked.join(".minecraft"))
            } else if named(&[
                "versions", "saves", "mods", "skprofiles", "sklauncher", "resourcepacks", "shaderpacks", "config",
            ]) {
                parent().filter(|p| is_minecraft_root(p))
            } else {
                None
            }
        }
        ImportSource::PrismLauncher if !picked.join("instances").is_dir() && named(&["instances"]) => parent(),
        ImportSource::ModrinthApp if !picked.join("app.db").is_file() && named(&["profiles"]) => parent(),
        _ => None,
    };
    resolved.unwrap_or_else(|| picked.to_path_buf())
}

/// Top-level entries of a shared `.minecraft` that belong to a launcher, not
/// to the player's worlds/mods/settings: downloaded game files (Arcane
/// fetches its own), Java runtimes, web caches, logs, and launcher settings
/// and login files (which must never be copied into an instance).
pub fn is_launcher_owned(name: &str) -> bool {
    const DIRS: &[&str] = &[
        "assets", "libraries", "versions", "runtime", "webcache", "webcache2", "downloads", "staging",
        "logs", "crash-reports", "sklauncher", "skprofiles", "tlauncher", ".tlauncher", "cache", "bin",
        "quickplay", "natives", ".fabric",
    ];
    let lower = name.to_ascii_lowercase();
    DIRS.contains(&lower.as_str())
        || lower.starts_with("launcher_")
        || lower.starts_with("clientid")
        || lower.starts_with("tlauncher")
        || lower == "treatment_tags.json"
        || lower == "command_history.txt"
        || lower.ends_with(".jar")
        || lower.ends_with(".exe")
        || lower.ends_with(".log")
}

/// Whether an import's folder is a whole shared `.minecraft` (official
/// launcher / TLauncher / SKlauncher 4.x) rather than an instance folder of
/// its own, so only the game content should be copied out of it.
pub fn is_shared_minecraft(game_dir: &Path) -> bool {
    game_dir.join("launcher_profiles.json").is_file()
        || game_dir.join("launcher_profiles_microsoft_store.json").is_file()
}

pub fn scan(source: ImportSource, root: Option<&str>) -> Vec<DiscoveredInstance> {
    let roots: Vec<PathBuf> = match root {
        Some(r) => vec![normalize_picked_root(source, Path::new(r))],
        None => candidate_roots(source),
    };
    roots
        .iter()
        .flat_map(|r| match source {
            ImportSource::PrismLauncher => scan_prism_style(r),
            ImportSource::CurseForge => scan_curseforge_style(r),
            ImportSource::TLauncher => scan_tlauncher_style(r),
            ImportSource::SKLauncher => scan_sklauncher_style(r),
            ImportSource::ModrinthApp => scan_modrinth_app_style(r),
        })
        .collect()
}

/// Registers every `.jar` already sitting in the imported instance's `mods/`
/// folder as a locally-tracked mod, so Installed Mods isn't empty right
/// after import. These aren't linked to Modrinth/CurseForge (we don't know
/// where they came from), which `update_check` accounts for — they show up
/// as "not linked", never as silently-fake Modrinth entries.
pub async fn sync_mods_from_disk(instance: &MinecraftInstance) -> AppResult<usize> {
    let mods_dir = Path::new(&instance.game_dir).join("mods");
    let Ok(entries) = std::fs::read_dir(&mods_dir) else {
        return Ok(0);
    };

    let existing = instance_mods::list_installed(instance).await?;
    let known_files: std::collections::HashSet<String> = existing.iter().map(|m| m.file_name.clone()).collect();

    let mut count = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let is_jar = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("jar"));
        if !is_jar {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().to_string();
        if known_files.contains(&file_name) {
            continue;
        }
        let display_name = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| file_name.clone());

        instance_mods::record_install(
            instance,
            InstalledMod {
                id: uuid::Uuid::new_v4().to_string(),
                project_id: format!("local:{file_name}"),
                version_id: String::new(),
                source: ModSource::Modrinth,
                project_type: ProjectType::Mod,
                name: display_name,
                version_number: "unknown".to_string(),
                loader: instance.loader.id().to_string(),
                minecraft_version: instance.minecraft_version.clone(),
                author: String::new(),
                description: String::new(),
                icon_url: None,
                file_name,
                enabled: true,
                installed_at: now_secs(),
                dependencies: Vec::new(),
                sha1: None,
                world: None,
                from_modpack: false,
            },
        )
        .await?;
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod standard_minecraft_tests {
    use super::*;

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// Same shape as a real SKlauncher 4.x `.minecraft`: no `skprofiles`,
    /// one custom profile, "latest" placeholder profiles, a vanilla version
    /// installed without a profile, a JSON-only (never downloaded) version,
    /// a malformed profile, and a BOM.
    fn fake_minecraft() -> PathBuf {
        let root = std::env::temp_dir().join(format!("arcane-import-test-{}", uuid::Uuid::new_v4()));
        write(
            &root.join("launcher_profiles.json"),
            "\u{feff}{\"profiles\":{\
              \"a\":{\"lastVersionId\":\"latest-release\",\"type\":\"latest-release\"},\
              \"b\":{\"name\":\"test\",\"lastVersionId\":\"fabric-loader-0.19.5-26.2\",\"type\":\"custom\"},\
              \"c\":{\"name\":123,\"lastVersionId\":42}}}",
        );
        let v = root.join("versions");
        write(&v.join("26.2/26.2.json"), r#"{"id":"26.2"}"#);
        write(&v.join("26.2/26.2.jar"), "");
        write(&v.join("26.3/26.3.json"), r#"{"id":"26.3"}"#);
        write(
            &v.join("fabric-loader-0.19.5-26.2/fabric-loader-0.19.5-26.2.json"),
            r#"{"id":"fabric-loader-0.19.5-26.2","inheritsFrom":"26.2","libraries":[{"name":"net.fabricmc:fabric-loader:0.19.5"}]}"#,
        );
        write(&root.join("saves/World/level.dat"), "");
        root
    }

    #[test]
    fn finds_profiles_and_installed_versions_in_sklauncher_4_layout() {
        let root = fake_minecraft();
        let mut found: Vec<(String, String, ModLoaderKind)> = scan_sklauncher_style(&root)
            .into_iter()
            .map(|d| (d.name, d.minecraft_version, d.loader))
            .collect();
        found.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            found,
            vec![
                ("Minecraft 26.2".to_string(), "26.2".to_string(), ModLoaderKind::Vanilla),
                ("test".to_string(), "26.2".to_string(), ModLoaderKind::Fabric),
            ]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn picking_a_subfolder_still_finds_minecraft() {
        let root = fake_minecraft();
        assert_eq!(normalize_picked_root(ImportSource::TLauncher, &root.join("versions")), root);
        assert_eq!(normalize_picked_root(ImportSource::TLauncher, &root), root);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn launcher_files_are_not_copied() {
        for name in [
            "assets", "versions", "launcher_msa_credentials_microsoft_store.bin", "sklauncher", "webcache2",
            "sklauncher-fx.jar",
        ] {
            assert!(is_launcher_owned(name), "{name}");
        }
        for name in ["saves", "mods", "config", "options.txt", "servers.dat", "resourcepacks", "screenshots", "journeymap"] {
            assert!(!is_launcher_owned(name), "{name}");
        }
    }
}
