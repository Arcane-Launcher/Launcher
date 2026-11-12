use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::models::common::{
    ModProject, ModVersion, ProjectType, SearchQuery, SearchResult, SortOrder,
};
use crate::models::curseforge::{CfFilesResponse, CfModResponse, CfSearchResponse};

use super::client::CurseForgeClient;
use super::convert::{
    class_id_for_project_type, loader_type_from_name, project_from_cf_mod, version_from_cf_file,
};

const MINECRAFT_GAME_ID: u32 = 432;

fn sort_field(sort: SortOrder) -> u32 {
    match sort {
        SortOrder::Relevance => 2,
        SortOrder::Downloads => 6,
        SortOrder::Updated => 3,
        SortOrder::Newest => 11,
        SortOrder::Followers => 2,
    }
}

pub async fn search(client: &CurseForgeClient, query: &SearchQuery) -> AppResult<SearchResult> {
    let class_id = class_id_for_project_type(query.project_type);
    let mut path = format!(
        "/v1/mods/search?gameId={MINECRAFT_GAME_ID}&classId={class_id}&sortField={}&sortOrder=desc&index={}&pageSize={}",
        sort_field(query.sort),
        query.offset,
        query.limit.clamp(1, 50),
    );
    if !query.query.is_empty() {
        path.push_str(&format!(
            "&searchFilter={}",
            urlencoding::encode(&query.query)
        ));
    }
    if query.project_type.checks_loader_compatibility() {
        if let Some(loader) = &query.loader {
            if let Some(loader_type) = loader_type_from_name(loader) {
                path.push_str(&format!("&modLoaderType={loader_type}"));
            }
        }
    }
    if let Some(version) = &query.game_version {
        if !version.is_empty() && version != "all" {
            path.push_str(&format!("&gameVersion={}", urlencoding::encode(version)));
        }
    }

    let raw: CfSearchResponse = client.get_json(&path).await?;
    Ok(SearchResult {
        hits: raw
            .data
            .into_iter()
            .map(|m| project_from_cf_mod(m, query.project_type))
            .collect(),
        offset: raw.pagination.index,
        limit: raw.pagination.page_size,
        total_hits: raw.pagination.total_count,
    })
}

pub async fn get_project(client: &CurseForgeClient, project_id: &str) -> AppResult<ModProject> {
    let path = format!("/v1/mods/{project_id}");
    let raw: CfModResponse = client.get_json(&path).await?;
    Ok(project_from_cf_mod(raw.data, ProjectType::Mod))
}

pub async fn get_versions(
    client: &CurseForgeClient,
    project_id: &str,
    loader: Option<&str>,
    game_version: Option<&str>,
    project_type: ProjectType,
) -> AppResult<Vec<ModVersion>> {
    let mut path = format!("/v1/mods/{project_id}/files?pageSize=50");
    if project_type.checks_loader_compatibility() {
        if let Some(loader) = loader {
            if let Some(loader_type) = loader_type_from_name(loader) {
                path.push_str(&format!("&modLoaderType={loader_type}"));
            }
        }
    }
    if let Some(version) = game_version {
        if !version.is_empty() && version != "all" {
            path.push_str(&format!("&gameVersion={}", urlencoding::encode(version)));
        }
    }
    let raw: CfFilesResponse = client.get_json(&path).await?;
    Ok(raw
        .data
        .into_iter()
        .map(|f| version_from_cf_file(f, project_type))
        .collect())
}

#[derive(Serialize)]
struct FilesByIdBody {
    #[serde(rename = "fileIds")]
    file_ids: Vec<u32>,
}

/// CurseForge files are addressed as `{modId}/{fileId}` in most endpoints,
/// but the mod-agnostic batch lookup (`POST /v1/mods/files`) only needs the
/// file id, which is all a `ModVersion.id` carries once round-tripped
/// through the app. That's what backs looking up "the version the user
/// picked" during install.
pub async fn get_file(client: &CurseForgeClient, file_id: &str) -> AppResult<ModVersion> {
    let id: u32 = file_id.parse().map_err(|_| {
        AppError::new(
            "This CurseForge file could not be found.",
            format!("invalid file id: {file_id}"),
        )
    })?;
    let raw: CfFilesResponse = client
        .post_json("/v1/mods/files", &FilesByIdBody { file_ids: vec![id] })
        .await?;
    raw.data
        .into_iter()
        .next()
        .map(|f| version_from_cf_file(f, ProjectType::Mod))
        .ok_or_else(|| AppError::not_found("This CurseForge file"))
}
