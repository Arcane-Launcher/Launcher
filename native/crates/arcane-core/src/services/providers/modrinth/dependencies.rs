use serde::Serialize;

use crate::error::AppResult;
use crate::models::common::{DependencyType, ModDependency, ModProject, ModVersion, ProjectType};

use super::client::ModrinthClient;
use super::{projects, versions};

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedDependency {
    pub dependency: ModDependency,
    pub project: Option<ModProject>,
    pub compatible_version: Option<ModVersion>,
}

/// Expands a version's raw dependency list into full project/version detail
/// for the target loader + Minecraft version, so the install-confirmation
/// UI can show real names and the exact file that would be installed.
pub async fn resolve_dependencies(
    client: &ModrinthClient,
    version: &ModVersion,
    loader: &str,
    minecraft_version: &str,
) -> AppResult<Vec<ResolvedDependency>> {
    let mut resolved = Vec::new();

    for dep in &version.dependencies {
        let Some(project_id) = dep.project_id.clone() else {
            resolved.push(ResolvedDependency {
                dependency: dep.clone(),
                project: None,
                compatible_version: None,
            });
            continue;
        };

        let project = projects::get_project(client, &project_id).await.ok();
        let mut dependency = dep.clone();
        dependency.project_name = project.as_ref().map(|p| p.name.clone());

        let compatible_version = if matches!(
            dep.dependency_type,
            DependencyType::Required | DependencyType::Optional
        ) {
            if let Some(version_id) = &dep.version_id {
                versions::get_version(client, version_id).await.ok()
            } else {
                let mut candidates = versions::get_versions(
                    client,
                    &project_id,
                    Some(loader),
                    Some(minecraft_version),
                    ProjectType::Mod,
                )
                .await
                .unwrap_or_default();
                candidates.sort_by(|a, b| b.date_published.cmp(&a.date_published));
                candidates.into_iter().next()
            }
        } else {
            None
        };

        resolved.push(ResolvedDependency {
            dependency,
            project,
            compatible_version,
        });
    }

    Ok(resolved)
}
