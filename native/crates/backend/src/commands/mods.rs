use crate::error::AppError;
use crate::models::common::{
    CompatibilityResult, InstalledMod, ModProject, ModSource, ModVersion, ProjectType, SearchQuery,
    SearchResult,
};
use crate::services::compatibility::check_compatibility;
use crate::services::instance::install::{
    install_mod as install_mod_service, InstallModOutcome, InstallModRequest,
};
use crate::services::instance::{mods as instance_mods, update_check};
use crate::services::providers::mod_provider::ModProvider;
use crate::services::providers::modrinth::dependencies::ResolvedDependency;
use crate::state::AppState;

pub async fn search_mods(state: &AppState, query: SearchQuery) -> Result<SearchResult, AppError> {
    state.providers.get(query.source).search(&query).await
}

pub async fn get_mod_project(
    state: &AppState,
    source: ModSource,
    project_id: String,
) -> Result<ModProject, AppError> {
    state.providers.get(source).get_project(&project_id).await
}

pub async fn get_mod_versions(
    state: &AppState,
    source: ModSource,
    project_id: String,
    loader: Option<String>,
    game_version: Option<String>,
    project_type: ProjectType,
) -> Result<Vec<ModVersion>, AppError> {
    state
        .providers
        .get(source)
        .get_versions(
            &project_id,
            loader.as_deref(),
            game_version.as_deref(),
            project_type,
        )
        .await
}

pub async fn resolve_mod_dependencies(
    state: &AppState,
    source: ModSource,
    version_id: String,
    loader: String,
    minecraft_version: String,
) -> Result<Vec<ResolvedDependency>, AppError> {
    if !matches!(source, ModSource::Modrinth) {
        return Ok(vec![]);
    }
    let version = state.providers.modrinth.get_version(&version_id).await?;
    state
        .providers
        .modrinth
        .get_dependencies(&version, &loader, &minecraft_version)
        .await
}

pub async fn check_mod_compatibility(
    state: &AppState,
    instance_id: String,
    source: ModSource,
    version_id: String,
) -> Result<CompatibilityResult, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    let mut version = state.providers.get(source).get_version(&version_id).await?;
    if let Ok(project) = state
        .providers
        .get(source)
        .get_project(&version.project_id)
        .await
    {
        version.project_type = project.project_type;
    }
    let installed = instance_mods::list_installed(&instance).await?;
    Ok(check_compatibility(&version, &instance, &installed))
}

pub async fn install_mod(
    state: &AppState,
    request: InstallModRequest,
) -> Result<InstallModOutcome, AppError> {
    let auto = state
        .settings
        .get()
        .await
        .auto_install_required_dependencies;
    install_mod_service(
        &state.instances,
        &state.providers,
        &state.downloads,
        request,
        auto,
    )
    .await
}

pub async fn update_mod(
    state: &AppState,
    instance_id: String,
    mod_id: String,
) -> Result<InstallModOutcome, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    let installed = instance_mods::list_installed(&instance).await?;
    let target = installed
        .into_iter()
        .find(|m| m.id == mod_id)
        .ok_or_else(|| AppError::not_found("This mod"))?;

    let versions = state
        .providers
        .get(target.source)
        .get_versions(
            &target.project_id,
            Some(instance.loader.id()),
            Some(&instance.minecraft_version),
            target.project_type,
        )
        .await?;
    let mut versions = versions;
    versions.sort_by(|a, b| b.date_published.cmp(&a.date_published));
    let latest = versions
        .into_iter()
        .next()
        .ok_or_else(|| AppError::new("No compatible version found.", "no versions available"))?;

    let auto = state
        .settings
        .get()
        .await
        .auto_install_required_dependencies;
    install_mod_service(
        &state.instances,
        &state.providers,
        &state.downloads,
        InstallModRequest {
            instance_id,
            project_id: target.project_id,
            version_id: Some(latest.id),
            source: target.source,
            project_type: target.project_type,
            world: target.world,
        },
        auto,
    )
    .await
}

pub async fn list_installed_mods(
    state: &AppState,
    instance_id: String,
) -> Result<Vec<InstalledMod>, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    instance_mods::list_installed(&instance).await
}

pub async fn set_mod_enabled(
    state: &AppState,
    instance_id: String,
    mod_id: String,
    enabled: bool,
) -> Result<(), AppError> {
    let instance = state.instances.get(&instance_id).await?;
    instance_mods::set_enabled(&instance, &mod_id, enabled).await
}

pub async fn uninstall_mod(
    state: &AppState,
    instance_id: String,
    mod_id: String,
) -> Result<(), AppError> {
    let instance = state.instances.get(&instance_id).await?;
    instance_mods::uninstall(&instance, &mod_id).await
}

pub async fn check_mod_updates(
    state: &AppState,
    instance_id: String,
) -> Result<Vec<update_check::ModUpdateInfo>, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    let installed = instance_mods::list_installed(&instance).await?;
    Ok(update_check::check_updates(&instance, &installed, &state.providers).await)
}

/// Updates every mod/resource pack/shader on this instance that has a
/// compatible newer version — used by both the "Update all" button and
/// (when `MinecraftInstance::auto_update_content` is on) automatically
/// right before launch.
pub async fn apply_all_mod_updates(
    state: &AppState,
    instance_id: String,
) -> Result<update_check::AutoUpdateOutcome, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    let installed = instance_mods::list_installed(&instance).await?;
    let auto = state
        .settings
        .get()
        .await
        .auto_install_required_dependencies;
    Ok(update_check::apply_all_updates(
        &instance,
        &installed,
        &state.instances,
        &state.providers,
        &state.downloads,
        auto,
    )
    .await)
}

pub async fn is_curseforge_configured(state: &AppState) -> Result<bool, AppError> {
    Ok(state.providers.curseforge.is_configured().await)
}
