use std::path::{Path, PathBuf};

use serde::Deserialize;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::paths::AppPaths;
use crate::services::download::manager::{DownloadManager, ExpectedHash};

use super::java::probe_version;

/// The Java major version to provision when nothing specific is required
/// (e.g. the Settings page's "no Java found" check, before any instance is
/// selected). Matches the current Minecraft LTS requirement.
pub const DEFAULT_JAVA_MAJOR: u32 = 21;

#[derive(Deserialize)]
struct AdoptiumAsset {
    binary: AdoptiumBinary,
}

#[derive(Deserialize)]
struct AdoptiumBinary {
    package: AdoptiumPackage,
}

#[derive(Deserialize)]
struct AdoptiumPackage {
    link: String,
    checksum: String,
}

fn adoptium_os() -> &'static str {
    match std::env::consts::OS {
        "windows" => "windows",
        "macos" => "mac",
        _ => "linux",
    }
}

fn adoptium_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "aarch64",
        "x86" => "x86",
        other => other,
    }
}

fn managed_java_dir(paths: &AppPaths, major_version: u32) -> PathBuf {
    paths.java_dir().join(major_version.to_string())
}

fn java_executable_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "javaw.exe"
    } else {
        "java"
    }
}

/// Walks a freshly-extracted JRE tree (Adoptium archives nest everything
/// under one `jdk-21.0.5+11-jre`-style folder) to find the actual java
/// binary, rather than assuming a fixed relative path.
fn find_java_executable(dir: &Path) -> Option<PathBuf> {
    let target = java_executable_name();
    let direct = dir.join("bin").join(target);
    if direct.is_file() {
        return Some(direct);
    }
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_java_executable(&path) {
                return Some(found);
            }
        }
    }
    None
}

/// The Java `major_version` Arcane already downloaded into its own
/// `java/<major>/` folder, if any — without starting it or downloading
/// anything, so it's cheap enough for a pre-launch check.
pub fn managed_java_path(paths: &AppPaths, major_version: u32) -> Option<String> {
    find_java_executable(&managed_java_dir(paths, major_version))
        .map(|p| p.to_string_lossy().to_string())
}

/// Returns a working Java executable for `major_version`, downloading and
/// extracting an Eclipse Temurin JRE (the same build PrismLauncher and other
/// launchers use for this) if one isn't already provisioned locally. Never
/// touches any Java the user installed themselves — this lives entirely
/// under the app's own managed `java/<major>/` directory.
pub async fn ensure_java(
    major_version: u32,
    paths: &AppPaths,
    downloads: &DownloadManager,
) -> AppResult<String> {
    let install_dir = managed_java_dir(paths, major_version);
    if let Some(existing) = find_java_executable(&install_dir) {
        if probe_version(&existing).await.is_some() {
            return Ok(existing.to_string_lossy().to_string());
        }
    }

    let client = reqwest::Client::builder()
        .user_agent("ArcaneLauncher/0.1")
        .build()
        .expect("http client");
    let url = format!(
        "https://api.adoptium.net/v3/assets/latest/{major_version}/hotspot?os={}&architecture={}&image_type=jre",
        adoptium_os(),
        adoptium_arch(),
    );
    let assets: Vec<AdoptiumAsset> = client.get(&url).send().await?.json().await.map_err(|e| {
        AppError::new(
            format!("Couldn't find a Java {major_version} runtime to download."),
            format!("adoptium response parse failed for {url}: {e}"),
        )
    })?;
    let asset = assets.into_iter().next().ok_or_else(|| {
        AppError::new(
            format!("No Java {major_version} runtime is available for this platform."),
            format!("empty adoptium asset list for {url}"),
        )
    })?;

    let is_zip = asset.binary.package.link.ends_with(".zip");
    let temp_path = paths.downloads_temp_dir().join(format!(
        "{}.{}",
        Uuid::new_v4(),
        if is_zip { "zip" } else { "tar.gz" }
    ));

    downloads
        .download_file(
            &format!("Java {major_version} runtime"),
            &asset.binary.package.link,
            temp_path.clone(),
            ExpectedHash {
                sha1: None,
                sha256: Some(asset.binary.package.checksum),
                sha512: None,
            },
            None,
        )
        .await?;

    std::fs::create_dir_all(&install_dir)?;
    let extract_result = if is_zip {
        extract_zip(&temp_path, &install_dir)
    } else {
        extract_tar_gz(&temp_path, &install_dir)
    };
    let _ = tokio::fs::remove_file(&temp_path).await;
    extract_result?;

    find_java_executable(&install_dir)
        .map(|p| p.to_string_lossy().to_string())
        .ok_or_else(|| {
            AppError::new(
                "The downloaded Java runtime looks corrupted.",
                format!("no java executable found under {}", install_dir.display()),
            )
        })
}

fn extract_zip(archive_path: &Path, dest: &Path) -> AppResult<()> {
    let file = std::fs::File::open(archive_path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| AppError::new("The downloaded Java runtime is corrupted.", e.to_string()))?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| {
            AppError::new("The downloaded Java runtime is corrupted.", e.to_string())
        })?;
        let name = entry.name().to_string();
        if name.ends_with('/') {
            continue;
        }
        let Some(out_path) = crate::paths::safe_join(dest, &name) else {
            continue;
        };
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out_file = std::fs::File::create(&out_path)?;
        std::io::copy(&mut entry, &mut out_file)?;
    }
    Ok(())
}

fn extract_tar_gz(archive_path: &Path, dest: &Path) -> AppResult<()> {
    let file = std::fs::File::open(archive_path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    for entry in archive
        .entries()
        .map_err(|e| AppError::new("The downloaded Java runtime is corrupted.", e.to_string()))?
    {
        let mut entry = entry.map_err(|e| {
            AppError::new("The downloaded Java runtime is corrupted.", e.to_string())
        })?;
        let relative = entry.path()?.to_string_lossy().to_string();
        let Some(out_path) = crate::paths::safe_join(dest, &relative) else {
            continue;
        };
        if entry.header().entry_type().is_dir() {
            std::fs::create_dir_all(&out_path)?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        entry.unpack(&out_path)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(mode) = entry.header().mode() {
                let _ = std::fs::set_permissions(&out_path, std::fs::Permissions::from_mode(mode));
            }
        }
    }
    Ok(())
}
