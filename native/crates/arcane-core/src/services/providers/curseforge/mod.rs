pub mod client;
pub mod convert;
pub mod mods;

use crate::error::{AppError, AppResult};
use crate::models::common::{ModProject, ModVersion, ProjectType, SearchQuery, SearchResult};
use crate::services::cache::settings_store::SettingsStore;
use crate::services::providers::mod_provider::ModProvider;

use client::CurseForgeClient;

#[derive(Clone)]
pub struct CurseForgeProvider {
    client: CurseForgeClient,
}

impl CurseForgeProvider {
    pub fn new(settings: SettingsStore) -> Self {
        Self {
            client: CurseForgeClient::new(settings),
        }
    }
}

#[async_trait::async_trait]
impl ModProvider for CurseForgeProvider {
    fn source_name(&self) -> &'static str {
        "curseforge"
    }

    async fn is_configured(&self) -> bool {
        self.client.is_configured().await
    }

    async fn search(&self, query: &SearchQuery) -> AppResult<SearchResult> {
        if !self.client.is_configured().await {
            return Err(AppError::new(
                "Add a CurseForge API key in Settings to enable CurseForge.",
                "curseforge api key not configured",
            ));
        }
        mods::search(&self.client, query).await
    }

    async fn get_project(&self, project_id: &str) -> AppResult<ModProject> {
        mods::get_project(&self.client, project_id).await
    }

    async fn get_versions(
        &self,
        project_id: &str,
        loader: Option<&str>,
        game_version: Option<&str>,
        project_type: ProjectType,
    ) -> AppResult<Vec<ModVersion>> {
        mods::get_versions(&self.client, project_id, loader, game_version, project_type).await
    }

    async fn get_version(&self, version_id: &str) -> AppResult<ModVersion> {
        mods::get_file(&self.client, version_id).await
    }

    async fn resolve_projects(&self, project_ids: &[String]) -> AppResult<Vec<ModProject>> {
        let mut results = Vec::new();
        for id in project_ids {
            if let Ok(project) = mods::get_project(&self.client, id).await {
                results.push(project);
            }
        }
        Ok(results)
    }
}
