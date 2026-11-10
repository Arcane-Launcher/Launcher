use crate::models::common::{ModFile, ModProject, ModSource, ModVersion, ProjectType};
use crate::models::curseforge::{CfFile, CfMod};

pub fn loader_type_from_name(name: &str) -> Option<u32> {
    match name.to_ascii_lowercase().as_str() {
        "forge" => Some(1),
        "fabric" => Some(4),
        "quilt" => Some(5),
        "neoforge" => Some(6),
        _ => None,
    }
}

/// CurseForge separates content types by numeric "class id" rather than a
/// named field. These are the Minecraft-game class ids for the types this
/// app supports; unverified against a live API key (CurseForge access is
/// gated behind a user-supplied key), so double check if search results
/// for resource packs/shaders/modpacks look off once one is configured.
pub fn class_id_for_project_type(project_type: ProjectType) -> u32 {
    match project_type {
        ProjectType::Mod => 6,
        ProjectType::ResourcePack => 12,
        ProjectType::Shader => 6552,
        ProjectType::Modpack => 4471,
        ProjectType::Datapack => 6945,
    }
}

fn project_type_from_class_id(class_id: u32, fallback: ProjectType) -> ProjectType {
    match class_id {
        6 => ProjectType::Mod,
        12 => ProjectType::ResourcePack,
        6552 => ProjectType::Shader,
        4471 => ProjectType::Modpack,
        _ => fallback,
    }
}

/// `fallback` is used when the response doesn't carry a recognized
/// `classId` (e.g. `class_id` is 0/missing) — normally the case only when
/// CurseForge changes their class ids; otherwise the real `classId` wins.
pub fn project_from_cf_mod(m: CfMod, fallback: ProjectType) -> ModProject {
    let project_type = project_type_from_class_id(m.class_id, fallback);
    let loaders: Vec<String> = m
        .latest_files
        .iter()
        .flat_map(|f| f.game_versions.iter())
        .filter(|v| {
            matches!(
                v.to_ascii_lowercase().as_str(),
                "forge" | "fabric" | "quilt" | "neoforge"
            )
        })
        .cloned()
        .collect();
    let game_versions: Vec<String> = m
        .latest_files
        .iter()
        .flat_map(|f| f.game_versions.iter())
        .filter(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .cloned()
        .collect();

    ModProject {
        source: ModSource::CurseForge,
        project_type,
        id: m.id.to_string(),
        slug: m.slug,
        name: m.name,
        summary: m.summary,
        description: None,
        author: m
            .authors
            .first()
            .map(|a| a.name.clone())
            .unwrap_or_default(),
        icon_url: m.logo.and_then(|l| l.thumbnail_url.or(l.url)),
        downloads: m.download_count,
        followers: 0,
        categories: m.categories.into_iter().map(|c| c.name).collect(),
        loaders,
        game_versions,
        license: None,
        gallery: vec![],
        source_url: m.links.and_then(|l| l.website_url),
        date_created: m.date_created,
        date_updated: m.date_modified,
    }
}

pub fn version_from_cf_file(f: CfFile, project_type: ProjectType) -> ModVersion {
    let loaders: Vec<String> = f
        .game_versions
        .iter()
        .filter(|v| {
            matches!(
                v.to_ascii_lowercase().as_str(),
                "forge" | "fabric" | "quilt" | "neoforge"
            )
        })
        .cloned()
        .collect();
    let game_versions: Vec<String> = f
        .game_versions
        .iter()
        .filter(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .cloned()
        .collect();
    let sha1 = f
        .hashes
        .iter()
        .find(|h| h.algo == 1)
        .map(|h| h.value.clone());

    ModVersion {
        source: ModSource::CurseForge,
        project_type,
        id: f.id.to_string(),
        project_id: f.mod_id.to_string(),
        name: f.display_name,
        version_number: f.file_name.clone(),
        changelog: None,
        loaders,
        game_versions,
        dependencies: vec![],
        files: vec![ModFile {
            filename: f.file_name,
            url: f.download_url.unwrap_or_default(),
            primary: true,
            size: f.file_length,
            sha1,
            sha512: None,
        }],
        date_published: Some(f.file_date),
        downloads: f.download_count,
        version_type: "release".to_string(),
    }
}
