use std::path::Path;

use crate::models::common::ModLoaderKind;
use crate::models::instance::{
    InstanceValidationReport, MinecraftInstance, ValidationIssue, ValidationSeverity,
};
use crate::paths::AppPaths;
use crate::services::minecraft::java;
use crate::services::minecraft::libraries::{library_allowed, resolve_library_artifact};
use crate::services::minecraft::manifest::VersionManifestService;

fn issue(code: &str, message: impl Into<String>, severity: ValidationSeverity) -> ValidationIssue {
    ValidationIssue {
        code: code.to_string(),
        message: message.into(),
        severity,
    }
}

/// The version key expected in the local meta cache for a loader install
/// that Arcane Launcher itself performed. Forge/NeoForge loader
/// installation isn't implemented yet, so those instances are only
/// validated at the Java/files/mods level, not against a merged profile.
pub fn expected_version_key(instance: &MinecraftInstance) -> Option<String> {
    match instance.loader {
        ModLoaderKind::Vanilla => Some(instance.minecraft_version.clone()),
        ModLoaderKind::Fabric => instance
            .loader_version
            .as_ref()
            .map(|lv| format!("fabric-loader-{lv}-{}", instance.minecraft_version)),
        ModLoaderKind::Quilt => instance
            .loader_version
            .as_ref()
            .map(|lv| format!("quilt-loader-{lv}-{}", instance.minecraft_version)),
        ModLoaderKind::Forge => instance
            .loader_version
            .as_ref()
            .map(|lv| format!("forge-loader-{lv}-{}", instance.minecraft_version)),
        ModLoaderKind::NeoForge => instance
            .loader_version
            .as_ref()
            .map(|lv| format!("neoforge-loader-{lv}-{}", instance.minecraft_version)),
    }
}

/// Fully local (no network) readiness check: are the client jar,
/// libraries, assets, Java, and mod files this instance needs actually
/// present on disk? This is what backs "Ready to play offline".
pub async fn validate_instance(
    instance: &MinecraftInstance,
    paths: &AppPaths,
    manifest: &VersionManifestService,
    default_java_path: Option<String>,
) -> InstanceValidationReport {
    let mut issues = Vec::new();

    if !Path::new(&instance.game_dir).is_dir() {
        issues.push(issue(
            "missing_game_dir",
            "This instance's folder no longer exists.",
            ValidationSeverity::Error,
        ));
        return InstanceValidationReport {
            ready: false,
            issues,
        };
    }

    let java_path = instance.java_path.clone().or(default_java_path);
    match &java_path {
        None => issues.push(issue(
            "no_java",
            "No Java runtime is configured for this instance.",
            ValidationSeverity::Error,
        )),
        Some(path) => {
            if !java::is_valid_java(path).await {
                issues.push(issue(
                    "invalid_java",
                    "The configured Java runtime couldn't be started.",
                    ValidationSeverity::Error,
                ));
            }
        }
    }

    let client_jar = paths
        .versions_meta_dir()
        .join(crate::paths::sanitize_component(
            &instance.minecraft_version,
        ))
        .join("client.jar");
    if !client_jar.exists() {
        issues.push(issue(
            "missing_client",
            "The Minecraft client hasn't been downloaded yet.",
            ValidationSeverity::Error,
        ));
    }

    match expected_version_key(instance) {
        Some(version_key) => match manifest.resolve_effective_version_offline(&version_key).await {
            None => issues.push(issue(
                "missing_version_metadata",
                "Version metadata is missing. Connect to the internet to repair this installation.",
                ValidationSeverity::Error,
            )),
            Some(version) => {
                let lib_root = paths.libraries_dir();
                let missing = version
                    .libraries
                    .iter()
                    .filter(|lib| library_allowed(&lib.rules))
                    .filter_map(resolve_library_artifact)
                    .filter(|resolved| !lib_root.join(&resolved.relative_path).exists())
                    .count();
                if missing > 0 {
                    issues.push(issue(
                        "missing_libraries",
                        format!("{missing} required library file(s) are missing."),
                        ValidationSeverity::Error,
                    ));
                }

                if let Some(asset_index) = &version.asset_index {
                    let index_path = paths
                        .assets_indexes_dir()
                        .join(format!("{}.json", asset_index.id));
                    if !index_path.exists() {
                        issues.push(issue(
                            "missing_assets",
                            "Game assets haven't been downloaded yet.",
                            ValidationSeverity::Error,
                        ));
                    }
                }
            }
        },
        None => issues.push(issue(
            "unmanaged_loader",
            format!(
                "{} installation is managed outside Arcane Launcher; only Java and mod files are checked.",
                instance.loader.id()
            ),
            ValidationSeverity::Warning,
        )),
    }

    if let Ok(mods) = crate::services::instance::mods::list_installed(instance).await {
        let mut seen_files = std::collections::HashSet::new();
        for m in &mods {
            let dir = if m.enabled {
                crate::services::instance::mods::content_dir(
                    instance,
                    m.project_type,
                    m.world.as_deref(),
                )
            } else {
                crate::services::instance::mods::disabled_dir(
                    instance,
                    m.project_type,
                    m.world.as_deref(),
                )
            };
            let Some(path) = crate::paths::safe_join(&dir, &m.file_name) else {
                continue;
            };
            match std::fs::metadata(&path) {
                Ok(meta) if meta.len() > 0 => {}
                Ok(_) => issues.push(issue(
                    "corrupted_mod",
                    format!("{} appears to be corrupted (0 bytes).", m.name),
                    ValidationSeverity::Error,
                )),
                Err(_) => issues.push(issue(
                    "missing_mod_file",
                    format!("{} is registered but its file is missing.", m.name),
                    ValidationSeverity::Error,
                )),
            }
            if m.loader != instance.loader.id() && m.loader != "unknown" {
                issues.push(issue(
                    "loader_mismatch",
                    format!(
                        "{} was installed for {} but this instance uses {}.",
                        m.name,
                        m.loader,
                        instance.loader.id()
                    ),
                    ValidationSeverity::Error,
                ));
            }
            if !seen_files.insert(m.file_name.clone()) {
                issues.push(issue(
                    "duplicate_mod",
                    format!(
                        "{} has more than one entry pointing at the same file.",
                        m.name
                    ),
                    ValidationSeverity::Warning,
                ));
            }
        }
    }

    let ready = !issues
        .iter()
        .any(|i| i.severity == ValidationSeverity::Error);
    InstanceValidationReport { ready, issues }
}
