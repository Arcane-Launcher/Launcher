use std::path::PathBuf;

use crate::error::AppResult;
use crate::models::minecraft::{Library, LibraryRule, VersionDetail};
use crate::paths::AppPaths;
use crate::services::download::manager::{
    DownloadItem, DownloadManager, ExpectedHash, MAX_PARALLEL_DOWNLOADS,
};

pub fn current_os_name() -> &'static str {
    match std::env::consts::OS {
        "windows" => "windows",
        "macos" => "osx",
        _ => "linux",
    }
}

pub fn current_arch_bits() -> &'static str {
    if cfg!(target_pointer_width = "64") {
        "64"
    } else {
        "32"
    }
}

fn rule_matches(rule: &LibraryRule) -> bool {
    if rule.features.is_some() {
        return false;
    }
    match &rule.os {
        None => true,
        Some(os) => {
            let name_ok = os.name.as_deref().is_none_or(|n| n == current_os_name());
            let arch_ok = os
                .arch
                .as_deref()
                .is_none_or(|a| a == current_arch_bits() || a == std::env::consts::ARCH);
            name_ok && arch_ok
        }
    }
}

pub fn rules_allow(rules: &[LibraryRule]) -> bool {
    if rules.is_empty() {
        return true;
    }
    let mut allowed = false;
    for rule in rules {
        if rule_matches(rule) {
            allowed = rule.action == "allow";
        }
    }
    allowed
}

pub fn library_allowed(rules: &Option<Vec<LibraryRule>>) -> bool {
    match rules {
        None => true,
        Some(rules) => rules_allow(rules),
    }
}

/// Turns a maven coordinate (`group:artifact:version[:classifier]`) into its
/// relative repository path, the way every Mojang/Fabric/Quilt/Forge meta
/// service expects libraries to be laid out.
pub fn maven_coordinate_to_path(coordinate: &str) -> Option<String> {
    let mut parts = coordinate.splitn(4, ':');
    let group = parts.next()?;
    let artifact = parts.next()?;
    let rest = parts.next()?;
    let (version, ext) = match rest.split_once('@') {
        Some((v, e)) => (v, e),
        None => (rest, "jar"),
    };
    let classifier = parts.next();

    let group_path = group.replace('.', "/");
    let file_name = match classifier {
        Some(c) => format!("{artifact}-{version}-{c}.{ext}"),
        None => format!("{artifact}-{version}.{ext}"),
    };
    Some(format!("{group_path}/{artifact}/{version}/{file_name}"))
}

pub struct ResolvedLibrary {
    pub relative_path: String,
    pub url: String,
    pub sha1: Option<String>,
    pub size: Option<u64>,
}

pub fn resolve_library_artifact(lib: &Library) -> Option<ResolvedLibrary> {
    if let Some(downloads) = &lib.downloads {
        if let Some(artifact) = &downloads.artifact {
            let relative_path = artifact
                .path
                .clone()
                .or_else(|| maven_coordinate_to_path(&lib.name))?;
            return Some(ResolvedLibrary {
                relative_path,
                url: artifact.url.clone(),
                sha1: artifact.sha1.clone(),
                size: artifact.size,
            });
        }
    }
    if let Some(base_url) = &lib.url {
        let relative_path = maven_coordinate_to_path(&lib.name)?;
        let url = format!("{}/{relative_path}", base_url.trim_end_matches('/'));
        return Some(ResolvedLibrary {
            relative_path,
            url,
            sha1: None,
            size: None,
        });
    }
    None
}

pub fn resolve_native_artifact(lib: &Library) -> Option<ResolvedLibrary> {
    let natives = lib.natives.as_ref()?;
    let mut classifier = natives.get(current_os_name())?.clone();
    classifier = classifier.replace("${arch}", current_arch_bits());
    let downloads = lib.downloads.as_ref()?;
    let classifiers = downloads.classifiers.as_ref()?;
    let artifact = classifiers.get(&classifier)?;
    Some(ResolvedLibrary {
        relative_path: artifact
            .path
            .clone()
            .unwrap_or_else(|| format!("{}-{classifier}.jar", lib.name)),
        url: artifact.url.clone(),
        sha1: artifact.sha1.clone(),
        size: artifact.size,
    })
}

/// Downloads every library (and native) this version needs that isn't
/// already present in the shared library cache, respecting per-library OS
/// rules so unrelated-platform natives are never fetched. Missing files are
/// downloaded concurrently (a version can need 50-150+ of these) rather than
/// one at a time, which is what actually made a fresh install feel slow.
pub async fn ensure_libraries(
    version: &VersionDetail,
    paths: &AppPaths,
    downloads: &DownloadManager,
    instance_id: Option<String>,
) -> AppResult<Vec<PathBuf>> {
    let mut classpath = Vec::new();
    let mut pending: Vec<DownloadItem> = Vec::new();
    let mut queued = std::collections::HashSet::new();
    let lib_root = paths.libraries_dir();

    let mut queue = |resolved: ResolvedLibrary, dest: PathBuf| {
        if !dest.exists() && queued.insert(dest.clone()) {
            pending.push(DownloadItem {
                url: resolved.url,
                dest,
                expected: ExpectedHash {
                    sha1: resolved.sha1,
                    sha256: None,
                    sha512: None,
                },
                size: resolved.size,
            });
        }
    };

    for lib in &version.libraries {
        if !library_allowed(&lib.rules) {
            continue;
        }

        if let Some(resolved) = resolve_library_artifact(lib) {
            let dest = lib_root.join(&resolved.relative_path);
            queue(resolved, dest.clone());
            classpath.push(dest);
        }

        if let Some(native) = resolve_native_artifact(lib) {
            let dest = lib_root.join(&native.relative_path);
            queue(native, dest);
        }
    }

    downloads
        .download_many(
            &format!("Libraries ({})", version.id),
            pending,
            instance_id,
            MAX_PARALLEL_DOWNLOADS,
        )
        .await?;
    Ok(classpath)
}

/// Extracts the natives jars for this version into `dest_dir`, which the
/// caller should point at a per-launch scratch folder (cleared beforehand).
pub fn extract_natives(
    version: &VersionDetail,
    paths: &AppPaths,
    dest_dir: &std::path::Path,
) -> AppResult<()> {
    std::fs::create_dir_all(dest_dir)?;
    let lib_root = paths.libraries_dir();

    for lib in &version.libraries {
        if !library_allowed(&lib.rules) {
            continue;
        }
        let Some(native) = resolve_native_artifact(lib) else {
            continue;
        };
        let jar_path = lib_root.join(&native.relative_path);
        if !jar_path.exists() {
            continue;
        }
        let file = std::fs::File::open(&jar_path)?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| {
            crate::error::AppError::new("A native library file is corrupted.", e.to_string())
        })?;
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).map_err(|e| {
                crate::error::AppError::new("A native library file is corrupted.", e.to_string())
            })?;
            let name = entry
                .name()
                .map_err(|e| {
                    crate::error::AppError::new(
                        "A native library file is corrupted.",
                        e.to_string(),
                    )
                })?
                .into_owned();
            if name.starts_with("META-INF/") || name.ends_with('/') {
                continue;
            }
            let Some(out_path) = crate::paths::safe_join(dest_dir, &name) else {
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
