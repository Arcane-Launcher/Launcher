pub mod client;
pub mod collections;
pub mod convert;
pub mod dependencies;
pub mod projects;
pub mod search;
pub mod versions;

use std::collections::HashMap;
use std::time::Duration;

use crate::error::{AppError, AppResult};
use crate::models::common::{ModProject, ModVersion, ProjectType, SearchQuery, SearchResult};
use crate::models::modrinth::RawCollection;
use crate::services::cache::store::JsonCache;
use crate::services::network::connectivity::NetworkMonitor;
use crate::services::providers::mod_provider::ModProvider;

use client::ModrinthClient;

const SEARCH_TTL: Duration = Duration::from_secs(5 * 60);
const PROJECT_TTL: Duration = Duration::from_secs(60 * 60);
const VERSIONS_TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Clone)]
pub struct ModrinthProvider {
    client: ModrinthClient,
    cache: JsonCache,
    network: NetworkMonitor,
}

impl ModrinthProvider {
    pub fn new(cache: JsonCache, network: NetworkMonitor) -> Self {
        Self {
            client: ModrinthClient::new(),
            cache,
            network,
        }
    }

    fn search_key(query: &SearchQuery) -> String {
        format!(
            "search|{}|{}|{}|{}|{}|{:?}|{}|{}",
            query.project_type.modrinth_facet_value(),
            query.query,
            query.loader.clone().unwrap_or_default(),
            query.game_version.clone().unwrap_or_default(),
            query.sort as u8,
            query.categories,
            query.offset,
            query.limit
        )
    }

    pub async fn get_dependencies(
        &self,
        version: &ModVersion,
        loader: &str,
        minecraft_version: &str,
    ) -> AppResult<Vec<dependencies::ResolvedDependency>> {
        dependencies::resolve_dependencies(&self.client, version, loader, minecraft_version).await
    }

    /// Collections are a Modrinth-only concept (CurseForge has no
    /// equivalent), so this lives directly on the provider rather than in
    /// the shared `ModProvider` trait.
    pub async fn get_collection(&self, collection_id: &str) -> AppResult<RawCollection> {
        if !self.network.is_modrinth_reachable().await {
            return Err(AppError::offline());
        }
        collections::get_collection(&self.client, collection_id).await
    }

    /// Identifies files by content hash — also Modrinth-only for now
    /// (CurseForge uses a different, murmur2-based fingerprint scheme).
    pub async fn lookup_by_sha1(
        &self,
        hashes: &[String],
    ) -> AppResult<HashMap<String, ModVersion>> {
        if !self.network.is_modrinth_reachable().await {
            return Err(AppError::offline());
        }
        versions::lookup_by_sha1(&self.client, hashes).await
    }
}

#[async_trait::async_trait]
impl ModProvider for ModrinthProvider {
    fn source_name(&self) -> &'static str {
        "modrinth"
    }

    async fn is_configured(&self) -> bool {
        true
    }

    async fn search(&self, query: &SearchQuery) -> AppResult<SearchResult> {
        let key = Self::search_key(query);
        if !self.network.is_modrinth_reachable().await {
            return self
                .cache
                .get::<SearchResult>(&key, Duration::from_secs(u64::MAX))
                .await
                .ok_or_else(AppError::offline);
        }
        match search::search(&self.client, query).await {
            Ok(result) => {
                let _ = self.cache.set(&key, &result).await;
                Ok(result)
            }
            Err(err) => match self.cache.get::<SearchResult>(&key, SEARCH_TTL * 100).await {
                Some(cached) => Ok(cached),
                None => Err(err),
            },
        }
    }

    async fn get_project(&self, project_id: &str) -> AppResult<ModProject> {
        let key = format!("project|{project_id}");
        if !self.network.is_modrinth_reachable().await {
            return self
                .cache
                .get::<ModProject>(&key, Duration::from_secs(u64::MAX))
                .await
                .ok_or_else(AppError::offline);
        }
        match projects::get_project(&self.client, project_id).await {
            Ok(project) => {
                let _ = self.cache.set(&key, &project).await;
                Ok(project)
            }
            Err(err) => match self.cache.get::<ModProject>(&key, PROJECT_TTL * 100).await {
                Some(cached) => Ok(cached),
                None => Err(err),
            },
        }
    }

    async fn get_versions(
        &self,
        project_id: &str,
        loader: Option<&str>,
        game_version: Option<&str>,
        project_type: ProjectType,
    ) -> AppResult<Vec<ModVersion>> {
        let key = format!(
            "versions|{project_id}|{}|{}",
            loader.unwrap_or(""),
            game_version.unwrap_or("")
        );
        if !self.network.is_modrinth_reachable().await {
            return self
                .cache
                .get::<Vec<ModVersion>>(&key, Duration::from_secs(u64::MAX))
                .await
                .ok_or_else(AppError::offline);
        }
        match versions::get_versions(&self.client, project_id, loader, game_version, project_type)
            .await
        {
            Ok(list) => {
                let _ = self.cache.set(&key, &list).await;
                Ok(list)
            }
            Err(err) => match self
                .cache
                .get::<Vec<ModVersion>>(&key, VERSIONS_TTL * 100)
                .await
            {
                Some(cached) => Ok(cached),
                None => Err(err),
            },
        }
    }

    async fn get_version(&self, version_id: &str) -> AppResult<ModVersion> {
        let key = format!("version|{version_id}");
        if !self.network.is_modrinth_reachable().await {
            return self
                .cache
                .get::<ModVersion>(&key, Duration::from_secs(u64::MAX))
                .await
                .ok_or_else(AppError::offline);
        }
        match versions::get_version(&self.client, version_id).await {
            Ok(version) => {
                let _ = self.cache.set(&key, &version).await;
                Ok(version)
            }
            Err(err) => match self.cache.get::<ModVersion>(&key, VERSIONS_TTL * 100).await {
                Some(cached) => Ok(cached),
                None => Err(err),
            },
        }
    }

    async fn resolve_projects(&self, project_ids: &[String]) -> AppResult<Vec<ModProject>> {
        if !self.network.is_modrinth_reachable().await {
            return Err(AppError::offline());
        }
        projects::resolve_projects(&self.client, project_ids).await
    }
}
