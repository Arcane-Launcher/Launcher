use std::collections::HashMap;

use serde::Serialize;

use crate::error::AppResult;
use crate::models::common::{InstalledMod, ModSource, ProjectType};
use crate::services::download::manager::DownloadManager;
use crate::services::providers::mod_provider::ModProvider;
use crate::services::providers::modrinth::collections::extract_collection_id;
use crate::services::providers::ProviderRegistry;

use super::install::{install_mod, InstallModRequest};
use super::manager::InstanceManager;

#[derive(Debug, Clone, Serialize)]
pub struct CollectionPreviewItem {
    pub project_id: String,
    pub name: String,
    pub icon_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CollectionPreview {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub items: Vec<CollectionPreviewItem>,
}

/// Looks up a public Modrinth collection and resolves every project in it to
/// a real name/icon (falling back to the bare id for anything that failed to
/// resolve — e.g. a deleted project) so the import dialog can show the user
/// exactly what they're about to install before committing to it.
pub async fn preview_collection(
    providers: &ProviderRegistry,
    collection_url_or_id: &str,
) -> AppResult<CollectionPreview> {
    let collection_id = extract_collection_id(collection_url_or_id);
    let collection = providers.modrinth.get_collection(&collection_id).await?;

    let resolved = providers
        .modrinth
        .resolve_projects(&collection.projects)
        .await
        .unwrap_or_default();
    let by_id: HashMap<&str, _> = resolved.iter().map(|p| (p.id.as_str(), p)).collect();

    let items = collection
        .projects
        .iter()
        .map(|id| match by_id.get(id.as_str()) {
            Some(project) => CollectionPreviewItem {
                project_id: id.clone(),
                name: project.name.clone(),
                icon_url: project.icon_url.clone(),
            },
            None => CollectionPreviewItem {
                project_id: id.clone(),
                name: id.clone(),
                icon_url: None,
            },
        })
        .collect();

    Ok(CollectionPreview {
        id: collection.id,
        name: collection.name,
        description: collection.description,
        items,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct CollectionImportSkip {
    pub project_id: String,
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CollectionImportOutcome {
    pub collection_name: String,
    pub installed: Vec<InstalledMod>,
    pub skipped: Vec<CollectionImportSkip>,
}

/// Installs every project in a Modrinth collection onto one instance, one at
/// a time, reusing the exact same install/dependency-resolution pipeline as
/// a manual single-mod install. Nothing here is special-cased for
/// collections: a project that's already installed, incompatible with this
/// instance's loader/version, or is itself a modpack is simply recorded as
/// skipped (with the real reason from the normal install path) rather than
/// aborting the rest of the batch.
pub async fn import_collection(
    instances: &InstanceManager,
    providers: &ProviderRegistry,
    downloads: &DownloadManager,
    instance_id: &str,
    collection_url_or_id: &str,
    selected_project_ids: Option<&[String]>,
    auto_install_required: bool,
) -> AppResult<CollectionImportOutcome> {
    let preview = preview_collection(providers, collection_url_or_id).await?;

    let mut installed = Vec::new();
    let mut skipped = Vec::new();

    let items = match selected_project_ids {
        Some(ids) => preview
            .items
            .into_iter()
            .filter(|item| ids.contains(&item.project_id))
            .collect(),
        None => preview.items,
    };

    for item in items {
        let request = InstallModRequest {
            instance_id: instance_id.to_string(),
            project_id: item.project_id.clone(),
            version_id: None,
            source: ModSource::Modrinth,
            project_type: ProjectType::Mod,
            world: None,
        };
        match install_mod(
            instances,
            providers,
            downloads,
            request,
            auto_install_required,
        )
        .await
        {
            Ok(outcome) => {
                installed.push(outcome.installed);
                installed.extend(outcome.installed_dependencies);
            }
            Err(err) => skipped.push(CollectionImportSkip {
                project_id: item.project_id,
                name: item.name,
                reason: err.message,
            }),
        }
    }

    Ok(CollectionImportOutcome {
        collection_name: preview.name,
        installed,
        skipped,
    })
}
