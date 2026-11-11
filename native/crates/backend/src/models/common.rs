use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModLoaderKind {
    Vanilla,
    Fabric,
    Forge,
    NeoForge,
    Quilt,
}

impl ModLoaderKind {
    pub fn id(&self) -> &'static str {
        match self {
            ModLoaderKind::Vanilla => "vanilla",
            ModLoaderKind::Fabric => "fabric",
            ModLoaderKind::Forge => "forge",
            ModLoaderKind::NeoForge => "neoforge",
            ModLoaderKind::Quilt => "quilt",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id.to_ascii_lowercase().as_str() {
            "vanilla" | "minecraft" => Some(ModLoaderKind::Vanilla),
            "fabric" => Some(ModLoaderKind::Fabric),
            "forge" => Some(ModLoaderKind::Forge),
            "neoforge" => Some(ModLoaderKind::NeoForge),
            "quilt" => Some(ModLoaderKind::Quilt),
            _ => None,
        }
    }

    /// Loader tags that a Modrinth/CurseForge version may carry which this
    /// loader kind is able to run. Quilt can run Fabric-tagged mods that use
    /// the Fabric compatibility layer, so it accepts both tags.
    pub fn accepted_version_tags(&self) -> &'static [&'static str] {
        match self {
            ModLoaderKind::Vanilla => &[],
            ModLoaderKind::Fabric => &["fabric"],
            ModLoaderKind::Forge => &["forge"],
            ModLoaderKind::NeoForge => &["neoforge"],
            ModLoaderKind::Quilt => &["quilt", "fabric"],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModSource {
    #[default]
    Modrinth,
    CurseForge,
}

/// The kind of content a project is. Mods, resource packs, shaders, and
/// modpacks are all separate Modrinth project types with their own search
/// facet, install target folder, and (for modpacks) install mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectType {
    #[default]
    Mod,
    ResourcePack,
    Shader,
    Modpack,
    /// Unlike every other type, a datapack installs into a *specific
    /// world's* `datapacks/` folder rather than a single instance-wide
    /// folder — see `services::instance::mods::content_dir`, which needs a
    /// world name for this type and ignores it for every other one.
    Datapack,
}

impl ProjectType {
    /// Value Modrinth expects in its `project_type:<value>` search facet.
    pub fn modrinth_facet_value(&self) -> &'static str {
        match self {
            ProjectType::Mod => "mod",
            ProjectType::ResourcePack => "resourcepack",
            ProjectType::Shader => "shader",
            ProjectType::Modpack => "modpack",
            ProjectType::Datapack => "datapack",
        }
    }

    pub fn from_modrinth_value(value: &str) -> Self {
        match value {
            "resourcepack" => ProjectType::ResourcePack,
            "shader" => ProjectType::Shader,
            "modpack" => ProjectType::Modpack,
            "datapack" => ProjectType::Datapack,
            _ => ProjectType::Mod,
        }
    }

    /// The folder inside an instance's game directory this project type's
    /// files are installed into. Meaningless for `Datapack` (see the variant
    /// doc) — `services::instance::mods::content_dir` special-cases it
    /// instead of calling this.
    pub fn install_dir_name(&self) -> &'static str {
        match self {
            ProjectType::Mod | ProjectType::Modpack => "mods",
            ProjectType::ResourcePack => "resourcepacks",
            ProjectType::Shader => "shaderpacks",
            ProjectType::Datapack => "datapacks",
        }
    }

    /// Whether compatibility should be checked against the instance's mod
    /// loader. Resource packs and shaders aren't loader-specific the way
    /// mods are (a shader's "loaders" tag names the shader-loading mod it
    /// needs, e.g. Iris/OptiFine, not our instance's Fabric/Forge/etc.) —
    /// datapacks are pure vanilla data and never loader-specific either.
    pub fn checks_loader_compatibility(&self) -> bool {
        matches!(self, ProjectType::Mod)
    }

    /// Whether `loader` is a meaningful Modrinth loader filter for this
    /// type. Shaders are tagged with the shader loader they run on
    /// (`iris`, `optifine`, `canvas`, `vanilla` — Modrinth's live
    /// `/tag/loader` list), never `fabric`/`forge`, so filtering a shader by
    /// the instance's mod loader matched nothing and every shader failed
    /// with "No compatible version found".
    pub fn accepts_loader_filter(&self, loader: &str) -> bool {
        match self {
            ProjectType::Mod => true,
            ProjectType::Shader => SHADER_LOADERS.contains(&loader),
            _ => false,
        }
    }
}

pub const SHADER_LOADERS: [&str; 4] = ["iris", "optifine", "canvas", "vanilla"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DependencyType {
    Required,
    Optional,
    Incompatible,
    Embedded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModDependency {
    pub project_id: Option<String>,
    pub version_id: Option<String>,
    pub dependency_type: DependencyType,
    pub project_name: Option<String>,
    pub file_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModFile {
    pub filename: String,
    pub url: String,
    pub primary: bool,
    pub size: u64,
    pub sha1: Option<String>,
    pub sha512: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModProject {
    pub source: ModSource,
    pub project_type: ProjectType,
    pub id: String,
    pub slug: String,
    pub name: String,
    pub summary: String,
    pub description: Option<String>,
    pub author: String,
    pub icon_url: Option<String>,
    pub downloads: u64,
    pub followers: u64,
    pub categories: Vec<String>,
    pub loaders: Vec<String>,
    pub game_versions: Vec<String>,
    pub license: Option<String>,
    pub gallery: Vec<String>,
    pub source_url: Option<String>,
    pub date_created: Option<String>,
    pub date_updated: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModVersion {
    pub source: ModSource,
    #[serde(default)]
    pub project_type: ProjectType,
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub version_number: String,
    pub changelog: Option<String>,
    pub loaders: Vec<String>,
    pub game_versions: Vec<String>,
    pub dependencies: Vec<ModDependency>,
    pub files: Vec<ModFile>,
    pub date_published: Option<String>,
    pub downloads: u64,
    pub version_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub hits: Vec<ModProject>,
    pub offset: u32,
    pub limit: u32,
    pub total_hits: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchQuery {
    pub query: String,
    pub project_type: ProjectType,
    pub loader: Option<String>,
    pub game_version: Option<String>,
    pub categories: Vec<String>,
    pub sort: SortOrder,
    pub offset: u32,
    pub limit: u32,
    pub source: ModSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortOrder {
    #[default]
    Relevance,
    Downloads,
    Updated,
    Newest,
    Followers,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompatibilityResult {
    pub compatible: bool,
    pub reason: Option<String>,
    pub missing_dependencies: Vec<ModDependency>,
    pub incompatible_with: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledMod {
    pub id: String,
    pub project_id: String,
    pub version_id: String,
    pub source: ModSource,
    #[serde(default)]
    pub project_type: ProjectType,
    pub name: String,
    pub version_number: String,
    pub loader: String,
    pub minecraft_version: String,
    pub author: String,
    pub description: String,
    pub icon_url: Option<String>,
    pub file_name: String,
    pub enabled: bool,
    pub installed_at: u64,
    pub dependencies: Vec<ModDependency>,
    pub sha1: Option<String>,
    /// Which world's folder this lives in — only ever set for
    /// `ProjectType::Datapack`, since every other type installs at the
    /// instance root.
    #[serde(default)]
    pub world: Option<String>,
    /// True when this was installed as part of a modpack (its own or a
    /// later "update pack" pass) rather than added manually by the player —
    /// lets the installed-content list show which is which, and means a
    /// pack update won't quietly overwrite something the player added
    /// themselves on top of it.
    #[serde(default)]
    pub from_modpack: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectivityState {
    Online,
    Offline,
    Connecting,
    Limited,
}

#[cfg(test)]
mod loader_filter_tests {
    use super::ProjectType;

    #[test]
    fn shaders_ignore_the_instance_mod_loader() {
        // Regression: the install dialog passes the instance's loader, and
        // `loaders=["fabric"]` returns zero shader versions on Modrinth.
        assert!(!ProjectType::Shader.accepts_loader_filter("fabric"));
        assert!(!ProjectType::Shader.accepts_loader_filter("neoforge"));
        assert!(ProjectType::Shader.accepts_loader_filter("iris"));
        assert!(ProjectType::Shader.accepts_loader_filter("optifine"));
    }

    #[test]
    fn mods_still_filter_by_loader_and_packs_never_do() {
        assert!(ProjectType::Mod.accepts_loader_filter("fabric"));
        assert!(!ProjectType::ResourcePack.accepts_loader_filter("fabric"));
    }
}
