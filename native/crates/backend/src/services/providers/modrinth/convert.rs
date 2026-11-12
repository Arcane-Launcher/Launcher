use crate::models::common::{
    DependencyType, ModDependency, ModFile, ModProject, ModSource, ModVersion, ProjectType,
};
use crate::models::modrinth::{
    RawDependencyType, RawProject, RawSearchHit, RawVersion, RawVersionDependency,
};

pub fn project_from_search_hit(hit: RawSearchHit) -> ModProject {
    ModProject {
        source: ModSource::Modrinth,
        project_type: ProjectType::from_modrinth_value(&hit.project_type),
        id: hit.project_id,
        slug: hit.slug,
        name: hit.title,
        summary: hit.description,
        description: None,
        author: hit.author,
        icon_url: hit.icon_url,
        downloads: hit.downloads,
        followers: hit.follows,
        categories: hit.categories,
        loaders: hit.display_categories,
        game_versions: hit.versions,
        license: hit.license,
        gallery: vec![],
        source_url: None,
        date_created: hit.date_created,
        date_updated: hit.date_modified,
    }
}

pub fn project_from_raw(raw: RawProject) -> ModProject {
    ModProject {
        source: ModSource::Modrinth,
        project_type: ProjectType::from_modrinth_value(&raw.project_type),
        id: raw.id,
        slug: raw.slug,
        name: raw.title,
        summary: raw.description,
        description: raw.body,
        author: String::new(),
        icon_url: raw.icon_url,
        downloads: raw.downloads,
        followers: raw.followers,
        categories: raw.categories,
        loaders: raw.loaders,
        game_versions: raw.game_versions,
        license: raw.license.and_then(|l| l.name.or(l.id)),
        gallery: raw.gallery.into_iter().map(|g| g.url).collect(),
        source_url: raw.source_url,
        date_created: raw.published,
        date_updated: raw.updated,
    }
}

pub fn dependency_type_from_raw(raw: RawDependencyType) -> DependencyType {
    match raw {
        RawDependencyType::Required => DependencyType::Required,
        RawDependencyType::Optional => DependencyType::Optional,
        RawDependencyType::Incompatible => DependencyType::Incompatible,
        RawDependencyType::Embedded => DependencyType::Embedded,
    }
}

pub fn dependency_from_raw(raw: RawVersionDependency) -> ModDependency {
    ModDependency {
        project_id: raw.project_id,
        version_id: raw.version_id,
        dependency_type: dependency_type_from_raw(raw.dependency_type),
        project_name: None,
        file_name: raw.file_name,
    }
}

/// Modrinth's version objects don't carry a `project_type` field (only
/// their parent project does), so this defaults to `Mod` — callers that
/// already know the parent project's type should overwrite
/// `ModVersion.project_type` with it once fetched.
pub fn version_from_raw(raw: RawVersion) -> ModVersion {
    ModVersion {
        source: ModSource::Modrinth,
        project_type: ProjectType::default(),
        id: raw.id,
        project_id: raw.project_id,
        name: raw.name,
        version_number: raw.version_number,
        changelog: raw.changelog,
        loaders: raw.loaders,
        game_versions: raw.game_versions,
        dependencies: raw
            .dependencies
            .into_iter()
            .map(dependency_from_raw)
            .collect(),
        files: raw
            .files
            .into_iter()
            .map(|f| ModFile {
                filename: f.filename,
                url: f.url,
                primary: f.primary,
                size: f.size,
                sha1: f.hashes.sha1,
                sha512: f.hashes.sha512,
            })
            .collect(),
        date_published: Some(raw.date_published),
        downloads: raw.downloads,
        version_type: raw.version_type,
    }
}
