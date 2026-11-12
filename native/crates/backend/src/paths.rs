use std::path::{Path, PathBuf};

/// The application data root. Same folder layout and location as the Tauri
/// build (`{data_dir}/com.arcanelauncher.app`) so existing user data keeps
/// working — see MIGRATION.md §1 data-dir rule.
pub const DATA_DIR_IDENTIFIER: &str = "com.arcanelauncher.app";
#[derive(Debug, Clone)]
pub struct AppPaths {
    root: PathBuf,
}

impl AppPaths {
    /// Resolves the platform data dir + identifier (the exact folder the
    /// Tauri app used: `%APPDATA%\com.arcanelauncher.app` on Windows,
    /// `$XDG_DATA_HOME/com.arcanelauncher.app` on Linux) and opens/creates
    /// every standard subdirectory.
    pub fn new() -> std::io::Result<Self> {
        let base = dirs::data_dir().expect("user data dir must be resolvable");
        Self::new_root(base.join(DATA_DIR_IDENTIFIER))
    }

    /// Explicit-root constructor (used by the desktop app's own resolution
    /// logic, e.g. portable builds, and by tests).
    pub fn new_root(root: PathBuf) -> std::io::Result<Self> {
        let paths = Self { root };
        paths.ensure_dirs()?;
        Ok(paths)
    }

    fn ensure_dirs(&self) -> std::io::Result<()> {
        for dir in [
            self.instances_dir(),
            self.cache_dir(),
            self.modrinth_cache_dir(),
            self.curseforge_cache_dir(),
            self.image_cache_dir(),
            self.downloads_temp_dir(),
            self.downloads_cache_dir(),
            self.java_dir(),
            self.libraries_dir(),
            self.assets_indexes_dir(),
            self.assets_objects_dir(),
            self.versions_meta_dir(),
            self.skins_dir(),
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn settings_file(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    pub fn auth_file(&self) -> PathBuf {
        self.root.join("auth.json")
    }

    pub fn instances_file(&self) -> PathBuf {
        self.root.join("instances.json")
    }

    pub fn instances_dir(&self) -> PathBuf {
        self.root.join("instances")
    }

    pub fn instance_dir(&self, instance_id: &str) -> PathBuf {
        self.instances_dir().join(sanitize_component(instance_id))
    }

    /// Turns a user-facing instance name into a fresh, unique instance
    /// directory (e.g. "My Survival World", or "My Survival World (2)" if
    /// that name is already taken) instead of a raw UUID, so an instance's
    /// folder is something a user opening it in a file browser actually
    /// recognizes. The instance's real identity is still its id, tracked
    /// separately in `instances.json` — this only picks the human-facing
    /// directory name.
    pub fn unique_instance_dir(&self, display_name: &str) -> PathBuf {
        let base = sanitize_component(display_name);
        let root = self.instances_dir();
        let mut candidate = root.join(&base);
        let mut n = 2;
        while candidate.exists() {
            candidate = root.join(format!("{base} ({n})"));
            n += 1;
        }
        candidate
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }

    pub fn modrinth_cache_dir(&self) -> PathBuf {
        self.cache_dir().join("modrinth")
    }

    pub fn curseforge_cache_dir(&self) -> PathBuf {
        self.cache_dir().join("curseforge")
    }

    pub fn image_cache_dir(&self) -> PathBuf {
        self.cache_dir().join("images")
    }

    pub fn downloads_dir(&self) -> PathBuf {
        self.root.join("downloads")
    }

    pub fn downloads_temp_dir(&self) -> PathBuf {
        self.downloads_dir().join("temp")
    }

    pub fn downloads_cache_dir(&self) -> PathBuf {
        self.downloads_dir().join("cache")
    }

    pub fn java_dir(&self) -> PathBuf {
        self.root.join("java")
    }

    pub fn meta_dir(&self) -> PathBuf {
        self.root.join("meta")
    }

    pub fn libraries_dir(&self) -> PathBuf {
        self.meta_dir().join("libraries")
    }

    pub fn assets_dir(&self) -> PathBuf {
        self.meta_dir().join("assets")
    }

    pub fn assets_indexes_dir(&self) -> PathBuf {
        self.assets_dir().join("indexes")
    }

    pub fn assets_objects_dir(&self) -> PathBuf {
        self.assets_dir().join("objects")
    }

    pub fn versions_meta_dir(&self) -> PathBuf {
        self.meta_dir().join("versions")
    }

    /// Locally-stored skins for offline accounts, which have no real Mojang
    /// profile to upload a skin to — see `services::minecraft::skins`.
    pub fn skins_dir(&self) -> PathBuf {
        self.root.join("skins")
    }

    /// A user-chosen custom background image, if any — see
    /// `services::background`. Its extension reflects the actual format
    /// (png/jpg) so it can be served back with the right content type.
    pub fn background_file(&self, extension: &str) -> PathBuf {
        self.root.join(format!("background.{extension}"))
    }
}

/// Strips path separators and traversal sequences from a single path
/// component. Used any time a remote-controlled string (an instance id, a
/// mod filename, a version id) is turned into part of a filesystem path, so
/// metadata from the network can never escape the intended directory.
pub fn sanitize_component(input: &str) -> String {
    let cleaned: String = input
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_string();
    if cleaned.is_empty() {
        "_".to_string()
    } else {
        cleaned
    }
}

/// Resolves `child` (a filename or relative path taken from remote
/// metadata) against `base`, guaranteeing the result stays inside `base`.
/// Returns None if the resolved path would escape it.
pub fn safe_join(base: &Path, child: &str) -> Option<PathBuf> {
    let mut result = base.to_path_buf();
    for part in child.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return None;
        }
        result.push(sanitize_component(part));
    }
    result.strip_prefix(base).ok()?;
    Some(result)
}
