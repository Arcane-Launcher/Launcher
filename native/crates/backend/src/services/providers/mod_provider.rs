




use crate::error::AppResult;
use crate::models::common::{ModProject, ModVersion, ProjectType, SearchQuery, SearchResult};

/// A source of mods (Modrinth, CurseForge, ...). Every provider returns the
/// same canonical `ModProject`/`ModVersion` shapes so the rest of the app
/// never needs to know which backend a mod came from.
#[async_trait::async_trait]
pub trait ModProvider: Send + Sync {
    fn source_name(&self) -> &'static str;

    /// Whether this provider is currently usable (e.g. CurseForge needs an
    /// API key configured before it can make requests).
    async fn is_configured(&self) -> bool;

    async fn search(&self, query: &SearchQuery) -> AppResult<SearchResult>;

    async fn get_project(&self, project_id: &str) -> AppResult<ModProject>;

    async fn get_versions(
        &self,
        project_id: &str,
        loader: Option<&str>,
        game_version: Option<&str>,
        project_type: ProjectType,
    ) -> AppResult<Vec<ModVersion>>;

    async fn get_version(&self, version_id: &str) -> AppResult<ModVersion>;

    async fn resolve_projects(&self, project_ids: &[String]) -> AppResult<Vec<ModProject>>;
}
