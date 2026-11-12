use std::collections::HashMap;

use serde::Serialize;

use crate::error::AppResult;
use crate::models::common::{ModVersion, ProjectType};
use crate::models::modrinth::RawVersion;

use super::client::ModrinthClient;
use super::convert::version_from_raw;

#[derive(Serialize)]
struct HashLookupRequest<'a> {
    hashes: &'a [String],
    algorithm: &'a str,
}

/// Identifies files by content hash via Modrinth's `/version_files` lookup
/// (verified against the live API: `POST` a batch of sha1 hashes, get back a
/// map of hash -> the version that file belongs to) — this is how a `.jar`
/// nobody told the launcher about gets matched back to a real mod.
pub async fn lookup_by_sha1(
    client: &ModrinthClient,
    hashes: &[String],
) -> AppResult<HashMap<String, ModVersion>> {
    if hashes.is_empty() {
        return Ok(HashMap::new());
    }
    let raw: HashMap<String, RawVersion> = client
        .post_json(
            "/version_files",
            &HashLookupRequest {
                hashes,
                algorithm: "sha1",
            },
        )
        .await?;
    Ok(raw
        .into_iter()
        .map(|(hash, v)| (hash, version_from_raw(v)))
        .collect())
}

pub async fn get_versions(
    client: &ModrinthClient,
    project_id: &str,
    loader: Option<&str>,
    game_version: Option<&str>,
    project_type: ProjectType,
) -> AppResult<Vec<ModVersion>> {
    let mut path = format!("/project/{}/version?", urlencoding::encode(project_id));
    // Resource packs aren't loader-specific; filtering by loader would just
    // return nothing since Modrinth never tags them with fabric/forge/etc.
    //
    // A datapack is a special case: Modrinth doesn't have a distinct
    // "datapack" project — the exact same project can publish both a mod
    // jar and a standalone datapack version (e.g. a mod with an optional
    // vanilla-only mode), so the *version* itself is what's tagged
    // `loaders: ["datapack"]`, not the project. Always filtering to that
    // literal loader (regardless of the instance's actual Fabric/Forge/etc.
    // loader, which is irrelevant here) is what keeps the other flavor's
    // versions from ever showing up in a datapack context — verified live
    // against a real mixed-type project before relying on it.
    if project_type == ProjectType::Datapack {
        path.push_str(&format!(
            "loaders={}&",
            urlencoding::encode("[\"datapack\"]")
        ));
    } else if let Some(loader) = loader {
        if !loader.is_empty() && loader != "all" && project_type.accepts_loader_filter(loader) {
            path.push_str(&format!(
                "loaders={}&",
                urlencoding::encode(&format!("[\"{loader}\"]"))
            ));
        }
    }
    if let Some(version) = game_version {
        if !version.is_empty() && version != "all" {
            path.push_str(&format!(
                "game_versions={}&",
                urlencoding::encode(&format!("[\"{version}\"]"))
            ));
        }
    }
    let raw: Vec<RawVersion> = client.get_json(&path).await?;
    Ok(raw
        .into_iter()
        .map(|r| {
            let mut version = version_from_raw(r);
            version.project_type = project_type;
            version
        })
        .collect())
}

pub async fn get_version(client: &ModrinthClient, version_id: &str) -> AppResult<ModVersion> {
    let path = format!("/version/{}", urlencoding::encode(version_id));
    let raw: RawVersion = client.get_json(&path).await?;
    Ok(version_from_raw(raw))
}

pub async fn get_versions_batch(
    client: &ModrinthClient,
    version_ids: &[String],
) -> AppResult<Vec<ModVersion>> {
    if version_ids.is_empty() {
        return Ok(vec![]);
    }
    let ids_json = serde_json::to_string(version_ids).unwrap_or_default();
    let path = format!("/versions?ids={}", urlencoding::encode(&ids_json));
    let raw: Vec<RawVersion> = client.get_json(&path).await?;
    Ok(raw.into_iter().map(version_from_raw).collect())
}
