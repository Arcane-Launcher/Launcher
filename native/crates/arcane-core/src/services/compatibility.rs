use crate::models::common::{CompatibilityResult, DependencyType, InstalledMod, ModDependency, ModVersion};
use crate::models::instance::MinecraftInstance;

/// Whether `version` (a Modrinth/CurseForge file) can run on `instance` at
/// all. For mods this checks both the declared loader and Minecraft
/// version. Resource packs and shaders aren't tied to a mod loader (a
/// shader's "loaders" tag names the shader-loading mod it needs, like
/// Iris/OptiFine, not our instance's Fabric/Forge/etc.), so only the
/// Minecraft version is checked for those.
pub fn version_matches_instance(version: &ModVersion, instance: &MinecraftInstance) -> bool {
    let mc_ok = version.game_versions.iter().any(|v| v == &instance.minecraft_version);
    if !version.project_type.checks_loader_compatibility() {
        return mc_ok;
    }
    let accepted = instance.loader.accepted_version_tags();
    let loader_ok = version.loaders.is_empty()
        || version.loaders.iter().any(|l| accepted.contains(&l.as_str()));
    loader_ok && mc_ok
}

/// Checks a candidate version against the instance and its already-installed
/// mods: loader/version support, declared incompatibilities, and which
/// required dependencies still need to be installed.
pub fn check_compatibility(
    version: &ModVersion,
    instance: &MinecraftInstance,
    installed: &[InstalledMod],
) -> CompatibilityResult {
    if !version_matches_instance(version, instance) {
        return CompatibilityResult {
            compatible: false,
            reason: Some(format!(
                "This version doesn't support {} on {}.",
                instance.minecraft_version,
                instance.loader.id()
            )),
            missing_dependencies: vec![],
            incompatible_with: vec![],
        };
    }

    let installed_project_ids: Vec<&str> = installed.iter().map(|m| m.project_id.as_str()).collect();

    let incompatible_with: Vec<String> = version
        .dependencies
        .iter()
        .filter(|d| matches!(d.dependency_type, DependencyType::Incompatible))
        .filter_map(|d| d.project_id.as_deref())
        .filter(|id| installed_project_ids.contains(id))
        .map(|id| {
            installed
                .iter()
                .find(|m| m.project_id == id)
                .map(|m| m.name.clone())
                .unwrap_or_else(|| id.to_string())
        })
        .collect();

    if !incompatible_with.is_empty() {
        return CompatibilityResult {
            compatible: false,
            reason: Some(format!(
                "Conflicts with already-installed mod(s): {}.",
                incompatible_with.join(", ")
            )),
            missing_dependencies: vec![],
            incompatible_with,
        };
    }

    let missing_dependencies: Vec<ModDependency> = version
        .dependencies
        .iter()
        .filter(|d| matches!(d.dependency_type, DependencyType::Required))
        .filter(|d| {
            d.project_id
                .as_deref()
                .is_none_or(|id| !installed_project_ids.contains(&id))
        })
        .cloned()
        .collect();

    CompatibilityResult {
        compatible: true,
        reason: None,
        missing_dependencies,
        incompatible_with: vec![],
    }
}
