use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct RawSearchResponse {
    pub hits: Vec<RawSearchHit>,
    pub offset: u32,
    pub limit: u32,
    pub total_hits: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawSearchHit {
    pub project_id: String,
    #[serde(default)]
    pub project_type: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub author: String,
    pub icon_url: Option<String>,
    pub downloads: u64,
    pub follows: u64,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub versions: Vec<String>,
    #[serde(default)]
    pub display_categories: Vec<String>,
    pub license: Option<String>,
    pub date_created: Option<String>,
    pub date_modified: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawGalleryImage {
    pub url: String,
}

/// Response shape of Modrinth's v3-only `/collection/{id}` endpoint — the
/// rest of this client targets v2, but collections were never added there.
#[derive(Debug, Clone, Deserialize)]
pub struct RawCollection {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub icon_url: Option<String>,
    #[serde(default)]
    pub projects: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawProjectLicense {
    pub id: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawProject {
    pub id: String,
    #[serde(default)]
    pub project_type: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub body: Option<String>,
    pub icon_url: Option<String>,
    pub downloads: u64,
    pub followers: u64,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub loaders: Vec<String>,
    #[serde(default)]
    pub game_versions: Vec<String>,
    pub license: Option<RawProjectLicense>,
    #[serde(default)]
    pub gallery: Vec<RawGalleryImage>,
    pub source_url: Option<String>,
    pub published: Option<String>,
    pub updated: Option<String>,
    #[serde(default)]
    pub team: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawTeamMember {
    pub user: RawUser,
    pub role: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawUser {
    pub username: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RawDependencyType {
    Required,
    Optional,
    Incompatible,
    Embedded,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawVersionDependency {
    pub version_id: Option<String>,
    pub project_id: Option<String>,
    pub file_name: Option<String>,
    pub dependency_type: RawDependencyType,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawFileHashes {
    pub sha1: Option<String>,
    pub sha512: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawVersionFile {
    pub hashes: RawFileHashes,
    pub url: String,
    pub filename: String,
    pub primary: bool,
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawVersion {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub version_number: String,
    pub changelog: Option<String>,
    #[serde(default)]
    pub dependencies: Vec<RawVersionDependency>,
    #[serde(default)]
    pub game_versions: Vec<String>,
    pub version_type: String,
    #[serde(default)]
    pub loaders: Vec<String>,
    pub downloads: u64,
    pub date_published: String,
    #[serde(default)]
    pub files: Vec<RawVersionFile>,
}
