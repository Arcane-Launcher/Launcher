use crate::error::AppResult;
use crate::models::common::ModProject;
use crate::models::modrinth::RawProject;

use super::client::ModrinthClient;
use super::convert::project_from_raw;

pub async fn get_project(client: &ModrinthClient, project_id: &str) -> AppResult<ModProject> {
    let path = format!("/project/{}", urlencoding::encode(project_id));
    let raw: RawProject = client.get_json(&path).await?;
    Ok(project_from_raw(raw))
}

pub async fn resolve_projects(
    client: &ModrinthClient,
    project_ids: &[String],
) -> AppResult<Vec<ModProject>> {
    if project_ids.is_empty() {
        return Ok(vec![]);
    }
    let ids_json = serde_json::to_string(project_ids).unwrap_or_default();
    let path = format!("/projects?ids={}", urlencoding::encode(&ids_json));
    let raw: Vec<RawProject> = client.get_json(&path).await?;
    Ok(raw.into_iter().map(project_from_raw).collect())
}
