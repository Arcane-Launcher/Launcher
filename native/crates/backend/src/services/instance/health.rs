use std::path::Path;

use serde::Serialize;

use crate::models::common::{DependencyType, ProjectType};
use crate::models::instance::MinecraftInstance;
use crate::paths::AppPaths;
use crate::services::instance::crash_help::{CrashFix, FixAction};
use crate::services::instance::mods as instance_mods;
use crate::services::instance::update_check;
use crate::services::instance::validation;
use crate::services::minecraft::java;
use crate::services::minecraft::java_provision;
use crate::services::minecraft::manifest::VersionManifestService;
use crate::services::providers::ProviderRegistry;
use crate::services::system;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Ok,
    Warning,
    Error,
    /// The check couldn't actually be run (e.g. offline) — deliberately
    /// distinct from `Ok` so a lack of data is never shown as a pass.
    Skipped,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthCheckItem {
    pub id: String,
    pub label: String,
    pub status: HealthStatus,
    pub detail: String,
    /// Whether `fix_instance_health` has a real, automatic remedy for this
    /// specific item — mod conflicts, low disk space, and RAM sizing never
    /// are, since they need the player's own judgment.
    pub fixable: bool,
    /// A one-click remedy for just this item (e.g. "Lower to 4096 MB"),
    /// applied through the crash helper's `apply_crash_fix`.
    pub fix: Option<CrashFix>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthReport {
    pub score: u32,
    pub items: Vec<HealthCheckItem>,
}

fn item(id: &str, label: &str, status: HealthStatus, detail: impl Into<String>, fixable: bool) -> HealthCheckItem {
    HealthCheckItem {
        id: id.to_string(),
        label: label.to_string(),
        status,
        detail: detail.into(),
        fixable,
        fix: None,
    }
}

fn with_fix(mut item: HealthCheckItem, label: impl Into<String>, action: FixAction) -> HealthCheckItem {
    item.fix = Some(CrashFix {
        label: label.into(),
        action,
    });
    item
}

/// Rounds down to a multiple of 512 MB, the steps the memory slider uses.
fn round_down_512(mb: u64) -> u64 {
    mb / 512 * 512
}

/// The memory check, kept pure (no system calls) so it can be tested.
/// Compares the allocation against what's *free right now*, not just total
/// RAM: -Xmx is only a ceiling, but a modded game does grow into it, and
/// once it outgrows free memory Windows starts paging and the game stutters.
fn memory_item(allocated_mb: u64, total_mb: u64, available_mb: u64) -> HealthCheckItem {
    const LABEL: &str = "Memory allocation";
    if total_mb == 0 {
        return item(
            "ram",
            LABEL,
            HealthStatus::Skipped,
            format!("{allocated_mb} MB allocated (couldn't read system memory)."),
            false,
        );
    }
    if allocated_mb as f64 / total_mb as f64 > 0.9 {
        let suggested = round_down_512(total_mb / 2).clamp(1024, 8192);
        return with_fix(
            item(
                "ram",
                LABEL,
                HealthStatus::Error,
                format!("{allocated_mb} MB allocated of {total_mb} MB total system RAM — this leaves almost nothing for Windows and other apps."),
                false,
            ),
            format!("Use {suggested} MB"),
            FixAction::SetMemory { mb: suggested as u32 },
        );
    }
    if allocated_mb > available_mb {
        // Leave ~512 MB of what's free for the game's own non-heap memory.
        let suggested = round_down_512(available_mb.saturating_sub(512));
        let detail = format!(
            "{allocated_mb} MB allocated, but only {available_mb} MB is free right now — close other apps (browsers, Discord, other games) or lower it, or the game may stutter."
        );
        let warning = item("ram", LABEL, HealthStatus::Warning, detail, false);
        return if suggested >= 2048 && suggested < allocated_mb {
            with_fix(warning, format!("Lower to {suggested} MB"), FixAction::SetMemory { mb: suggested as u32 })
        } else {
            warning
        };
    }
    if allocated_mb < 1536 {
        let warning = item(
            "ram",
            LABEL,
            HealthStatus::Warning,
            format!("Only {allocated_mb} MB allocated — modern or modded Minecraft may run poorly."),
            false,
        );
        let suggested = round_down_512(available_mb.saturating_sub(512)).min(4096);
        return if suggested >= 2048 {
            with_fix(warning, format!("Raise to {suggested} MB"), FixAction::SetMemory { mb: suggested as u32 })
        } else {
            warning
        };
    }
    item(
        "ram",
        LABEL,
        HealthStatus::Ok,
        format!("{allocated_mb} MB allocated · {available_mb} MB free of {total_mb} MB."),
        false,
    )
}

fn score_items(items: &[HealthCheckItem]) -> u32 {
    let mut score: i32 = 100;
    for i in items {
        match i.status {
            HealthStatus::Error => score -= 20,
            HealthStatus::Warning => score -= 5,
            HealthStatus::Ok | HealthStatus::Skipped => {}
        }
    }
    score.clamp(0, 100) as u32
}

async fn java_item(
    instance: &MinecraftInstance,
    paths: &AppPaths,
    required_major: Option<u32>,
    default_java_path: Option<&str>,
) -> HealthCheckItem {
    const LABEL: &str = "Java version";
    let Some(required) = required_major else {
        return item(
            "java",
            LABEL,
            HealthStatus::Skipped,
            "Couldn't tell which Java this version needs yet — it's checked again once the game files are downloaded.",
            false,
        );
    };
    let use_required = |i: HealthCheckItem| with_fix(i, format!("Use Java {required}"), FixAction::UseJava { major: required });

    if let Some(path) = instance.java_path.as_deref().filter(|p| !p.trim().is_empty()) {
        return match java::major_version_of(path).await {
            None => use_required(item(
                "java",
                LABEL,
                HealthStatus::Warning,
                format!("This instance's custom Java can't be started, so Java {required} will be used instead."),
                true,
            )),
            Some(actual) if actual != required => use_required(item(
                "java",
                LABEL,
                HealthStatus::Warning,
                format!(
                    "Minecraft {} needs Java {required}, but this instance is set to Java {actual} — it may crash on start.",
                    instance.minecraft_version
                ),
                true,
            )),
            Some(actual) => item("java", LABEL, HealthStatus::Ok, format!("Java {actual} (this instance's own) — matches."), false),
        };
    }

    if let Some(path) = default_java_path {
        if java::major_version_of(path).await == Some(required) {
            return item("java", LABEL, HealthStatus::Ok, format!("Java {required} (your default) — matches."), false);
        }
    }
    let detail = if java_provision::managed_java_path(paths, required).is_some() {
        format!("Java {required}, managed by Arcane — matches.")
    } else {
        format!("Java {required} will be downloaded automatically on this launch (about 45 MB).")
    };
    item("java", LABEL, HealthStatus::Ok, detail, false)
}

/// Every Arcane Smart Launch pre-flight check, scored out of 100 (each
/// error -20, each warning -5, floor 0). Every signal here is real —
/// nothing is fabricated, and a check that couldn't actually be performed
/// (e.g. an update check while offline) is reported as `Skipped` rather
/// than quietly counted as a pass.
pub async fn check(
    instance: &MinecraftInstance,
    paths: &AppPaths,
    manifest: &VersionManifestService,
    providers: &ProviderRegistry,
    default_java_path: Option<String>,
) -> HealthReport {
    let mut items = Vec::new();

    // --- Installation files (client jar / libraries / assets) ---
    let validation_report = validation::validate_instance(instance, paths, manifest, default_java_path.clone()).await;
    let install_issues: Vec<_> = validation_report
        .issues
        .iter()
        .filter(|i| {
            matches!(
                i.code.as_str(),
                "missing_client" | "missing_libraries" | "missing_assets" | "missing_version_metadata" | "missing_game_dir"
            )
        })
        .collect();
    if install_issues.is_empty() {
        items.push(item(
            "installation",
            "Minecraft installation",
            HealthStatus::Ok,
            "Client, libraries, and assets are all present.",
            false,
        ));
    } else {
        let detail = install_issues.iter().map(|i| i.message.clone()).collect::<Vec<_>>().join(" ");
        items.push(item("installation", "Minecraft installation", HealthStatus::Error, detail, true));
    }

    // --- Java: the runtime this launch will really use (mirrors
    // `resolve_or_provision_java` in commands/launch.rs), checked against
    // Mojang's own declared requirement for this version ---
    let required_major = match validation::expected_version_key(instance) {
        Some(key) => manifest
            .resolve_effective_version_offline(&key)
            .await
            .and_then(|v| v.java_version)
            .map(|j| j.major_version),
        None => None,
    };
    items.push(java_item(instance, paths, required_major, default_java_path.as_deref()).await);

    // --- RAM allocation ---
    let allocated_mb = instance.memory_mb.map(|v| v as u64).unwrap_or_else(|| system::default_instance_memory_mb() as u64);
    items.push(memory_item(allocated_mb, system::total_memory_mb(), system::available_memory_mb()));

    // --- Disk space on the drive this instance actually lives on ---
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let game_dir = Path::new(&instance.game_dir);
    let best_disk = disks
        .list()
        .iter()
        .filter(|d| game_dir.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len());
    if let Some(disk) = best_disk {
        let free_mb = disk.available_space() / (1024 * 1024);
        if free_mb < 500 {
            items.push(item(
                "disk",
                "Disk space",
                HealthStatus::Error,
                format!("Only {free_mb}MB free — downloads and world saves may fail."),
                false,
            ));
        } else if free_mb < 2048 {
            items.push(item("disk", "Disk space", HealthStatus::Warning, format!("{free_mb}MB free — running low."), false));
        } else {
            items.push(item(
                "disk",
                "Disk space",
                HealthStatus::Ok,
                format!("{:.1}GB free.", free_mb as f64 / 1024.0),
                false,
            ));
        }
    }

    // --- GPU (informational — no principled pass/fail line to draw) ---
    match system::detect_gpu_name().await {
        Some(name) => items.push(item("gpu", "Graphics", HealthStatus::Ok, format!("Detected: {name}"), false)),
        None => items.push(item(
            "gpu",
            "Graphics",
            HealthStatus::Warning,
            "Couldn't detect your GPU.",
            false,
        )),
    }

    // --- Mod-level checks: updates, dependencies, conflicts, shaders ---
    let installed = instance_mods::list_installed(instance).await.unwrap_or_default();
    if !installed.is_empty() {
        let updates = update_check::check_updates(instance, &installed, providers).await;
        let any_unchecked = updates.iter().any(|u| !u.checked);
        let outdated_count = updates.iter().filter(|u| u.checked && u.latest.is_some()).count();
        if outdated_count > 0 {
            items.push(item(
                "outdated_mods",
                "Mod updates",
                HealthStatus::Warning,
                format!("{outdated_count} mod{} outdated.", if outdated_count == 1 { "" } else { "s" }),
                true,
            ));
        } else if any_unchecked {
            items.push(item(
                "outdated_mods",
                "Mod updates",
                HealthStatus::Skipped,
                "Couldn't check for updates (offline or provider unreachable).",
                false,
            ));
        } else {
            items.push(item("outdated_mods", "Mod updates", HealthStatus::Ok, "Everything is up to date.", false));
        }

        let installed_ids: std::collections::HashSet<&str> = installed.iter().map(|m| m.project_id.as_str()).collect();
        let mut missing_deps = Vec::new();
        let mut incompatible = Vec::new();
        for m in &installed {
            for dep in &m.dependencies {
                let Some(dep_id) = &dep.project_id else { continue };
                match dep.dependency_type {
                    DependencyType::Required if !installed_ids.contains(dep_id.as_str()) => {
                        missing_deps.push(dep.project_name.clone().unwrap_or_else(|| dep_id.clone()));
                    }
                    DependencyType::Incompatible if installed_ids.contains(dep_id.as_str()) => {
                        incompatible.push(format!("{} and {}", m.name, dep.project_name.clone().unwrap_or_else(|| dep_id.clone())));
                    }
                    _ => {}
                }
            }
        }
        if missing_deps.is_empty() {
            items.push(item("dependencies", "Mod dependencies", HealthStatus::Ok, "All dependencies satisfied.", false));
        } else {
            items.push(item(
                "dependencies",
                "Mod dependencies",
                HealthStatus::Warning,
                format!("Missing: {}.", missing_deps.join(", ")),
                true,
            ));
        }
        if !incompatible.is_empty() {
            items.push(item(
                "incompatible_mods",
                "Mod conflicts",
                HealthStatus::Error,
                format!("Conflicting: {}.", incompatible.join(", ")),
                false,
            ));
        }

        let has_shaders = installed.iter().any(|m| m.project_type == ProjectType::Shader);
        if has_shaders {
            let has_loader = installed.iter().any(|m| {
                let n = m.name.to_lowercase();
                n.contains("iris") || n.contains("oculus") || n.contains("optifine")
            });
            if has_loader {
                items.push(item(
                    "shader_loader",
                    "Shader compatibility",
                    HealthStatus::Ok,
                    "A shader loader (Iris/Oculus/OptiFine) is installed.",
                    false,
                ));
            } else {
                items.push(item(
                    "shader_loader",
                    "Shader compatibility",
                    HealthStatus::Warning,
                    "Shader packs are installed but no shader loader (Iris/Oculus/OptiFine) was found.",
                    false,
                ));
            }
        }
    }

    let score = score_items(&items);
    HealthReport { score, items }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suggested_mb(i: HealthCheckItem) -> Option<u32> {
        match i.fix.map(|f| f.action) {
            Some(FixAction::SetMemory { mb }) => Some(mb),
            _ => None,
        }
    }

    #[test]
    fn memory_ok_when_it_fits() {
        let i = memory_item(4096, 16384, 9000);
        assert_eq!(i.status, HealthStatus::Ok);
        assert!(i.fix.is_none());
    }

    #[test]
    fn memory_over_free_ram_suggests_what_fits() {
        let i = memory_item(8192, 16384, 5000);
        assert_eq!(i.status, HealthStatus::Warning);
        assert_eq!(suggested_mb(i), Some(4096));
    }

    #[test]
    fn memory_over_free_ram_without_a_useful_lower_value_has_no_fix() {
        let i = memory_item(4096, 8192, 1200);
        assert_eq!(i.status, HealthStatus::Warning);
        assert!(i.fix.is_none());
    }

    #[test]
    fn memory_near_total_is_an_error() {
        let i = memory_item(15360, 16384, 15000);
        assert_eq!(i.status, HealthStatus::Error);
        assert_eq!(suggested_mb(i), Some(8192));
    }

    #[test]
    fn memory_too_low_suggests_raising() {
        let i = memory_item(1024, 16384, 9000);
        assert_eq!(i.status, HealthStatus::Warning);
        assert_eq!(suggested_mb(i), Some(4096));
    }
}
