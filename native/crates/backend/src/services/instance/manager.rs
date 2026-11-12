use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::models::instance::{CreateInstanceRequest, MinecraftInstance};
use crate::paths::AppPaths;
use crate::services::cache::store::now_secs;

use super::servers;
use super::store::InstanceStore;

#[derive(Clone)]
pub struct InstanceManager {
    store: InstanceStore,
    paths: AppPaths,
}

impl InstanceManager {
    pub fn new(store: InstanceStore, paths: AppPaths) -> Self {
        Self { store, paths }
    }

    pub async fn list(&self) -> Vec<MinecraftInstance> {
        self.store.list().await
    }

    pub async fn get(&self, id: &str) -> AppResult<MinecraftInstance> {
        self.store
            .get(id)
            .await
            .ok_or_else(|| AppError::not_found("This instance"))
    }

    pub async fn create(&self, req: CreateInstanceRequest) -> AppResult<MinecraftInstance> {
        if req.name.trim().is_empty() {
            return Err(AppError::new(
                "Give the instance a name.",
                "empty instance name",
            ));
        }

        let id = Uuid::new_v4().to_string();
        let (game_dir, imported) = if let Some(existing) = &req.existing_game_dir {
            let path = std::path::PathBuf::from(existing);
            if !path.is_dir() {
                return Err(AppError::new(
                    "That folder doesn't exist.",
                    format!("existing_game_dir not found: {existing}"),
                ));
            }
            (path, true)
        } else {
            let path = self.paths.unique_instance_dir(&req.name);
            (path, false)
        };

        for sub in [
            "mods",
            "config",
            "resourcepacks",
            "shaderpacks",
            "saves",
            "mods/.disabled",
            "resourcepacks/.disabled",
            "shaderpacks/.disabled",
        ] {
            std::fs::create_dir_all(game_dir.join(sub))?;
        }

        let instance = MinecraftInstance {
            id,
            name: req.name,
            minecraft_version: req.minecraft_version,
            loader: req.loader,
            loader_version: req.loader_version,
            game_dir: game_dir.to_string_lossy().to_string(),
            java_path: req.java_path,
            java_args: None,
            memory_mb: Some(crate::services::system::default_instance_memory_mb()),
            window_width: None,
            window_height: None,
            icon: None,
            created_at: now_secs(),
            last_played: None,
            total_playtime_secs: 0,
            imported,
            modpack_source: None,
            modpack_project_id: None,
            modpack_version_id: None,
            auto_update_content: false,
            group: None,
            notes: None,
            pre_launch_command: None,
            post_exit_command: None,
            favorite: false,
        };

        self.store.upsert(instance.clone()).await?;

        // A fresh instance's server list starts empty; pre-seed it with
        // Arcane World (the community's own server) rather than leaving new
        // players to find it themselves. Imported instances already have a
        // real servers.dat from their source launcher, so this only applies
        // to instances actually created here. Best-effort only — this is a
        // convenience default, not something that should ever fail
        // instance creation.
        if !instance.imported {
            let _ = servers::add_server(
                &instance,
                "Arcane World".to_string(),
                "mc.arcaneworld.lk".to_string(),
            )
            .await;
        }

        Ok(instance)
    }

    pub async fn update(&self, instance: MinecraftInstance) -> AppResult<MinecraftInstance> {
        self.store.upsert(instance.clone()).await?;
        Ok(instance)
    }

    pub async fn delete(&self, id: &str, delete_files: bool) -> AppResult<()> {
        let instance = self.get(id).await?;
        self.store.remove(id).await?;
        if delete_files && !instance.imported {
            let _ = std::fs::remove_dir_all(&instance.game_dir);
        }
        Ok(())
    }

    pub async fn touch_last_played(&self, id: &str) -> AppResult<()> {
        let mut instance = self.get(id).await?;
        instance.last_played = Some(now_secs());
        self.store.upsert(instance).await?;
        Ok(())
    }

    pub async fn add_playtime(&self, id: &str, elapsed_secs: u64) -> AppResult<()> {
        let mut instance = self.get(id).await?;
        instance.total_playtime_secs += elapsed_secs;
        self.store.upsert(instance).await?;
        Ok(())
    }

    /// Clones an instance's own directory (mods, config, saves,
    /// resourcepacks, everything under `game_dir`) into a brand-new managed
    /// instance. Shared libraries/assets live outside `game_dir` entirely,
    /// so nothing gets re-downloaded — this is a cheap copy, not a
    /// reinstall. The copy is always a fresh managed folder, even if the
    /// source was an imported one, so two instances never share a directory.
    pub async fn duplicate(
        &self,
        source_id: &str,
        new_name: String,
    ) -> AppResult<MinecraftInstance> {
        if new_name.trim().is_empty() {
            return Err(AppError::new(
                "Give the copy a name.",
                "empty duplicate name",
            ));
        }
        let source = self.get(source_id).await?;

        let new_id = Uuid::new_v4().to_string();
        let new_dir = self.paths.unique_instance_dir(&new_name);
        let source_dir = std::path::PathBuf::from(&source.game_dir);
        let new_dir_for_copy = new_dir.clone();
        tokio::task::spawn_blocking(move || {
            crate::services::fs_utils::copy_dir_recursive(&source_dir, &new_dir_for_copy)
        })
        .await
        .map_err(|e| AppError::new("Couldn't duplicate this instance.", e.to_string()))??;

        let mut new_instance = source;
        new_instance.id = new_id;
        new_instance.name = new_name;
        new_instance.game_dir = new_dir.to_string_lossy().to_string();
        new_instance.created_at = now_secs();
        new_instance.last_played = None;
        new_instance.total_playtime_secs = 0;
        new_instance.imported = false;

        self.store.upsert(new_instance.clone()).await?;
        Ok(new_instance)
    }

    /// Brings an instance discovered on disk from another launcher
    /// (PrismLauncher, CurseForge, ...) into Arcane's own instance folder,
    /// by copying its whole directory rather than just pointing at it in
    /// place. Pointing at the original folder would leave the instance
    /// fragile — the other launcher could move, rename, or delete it out
    /// from under Arcane at any time — and would mean Arcane never truly
    /// "owns" it (e.g. deleting it here couldn't safely also remove its
    /// files). A real copy fixes both: it's a fully independent, readably
    /// named instance under Arcane's own `instances/` folder afterward.
    pub async fn clone_external(
        &self,
        req: CreateInstanceRequest,
        source_dir: String,
    ) -> AppResult<MinecraftInstance> {
        if req.name.trim().is_empty() {
            return Err(AppError::new(
                "Give the instance a name.",
                "empty instance name",
            ));
        }
        let source_dir = std::path::PathBuf::from(source_dir);
        if !source_dir.is_dir() {
            return Err(AppError::new(
                "That folder doesn't exist.",
                format!("clone_external source not found: {}", source_dir.display()),
            ));
        }

        let id = Uuid::new_v4().to_string();
        let game_dir = self.paths.unique_instance_dir(&req.name);
        let game_dir_for_copy = game_dir.clone();
        // A whole shared `.minecraft` (official launcher, TLauncher,
        // SKlauncher 4.x) also holds gigabytes of launcher-owned files and
        // the other launcher's login data; only the game content comes along.
        let shared = crate::services::import::is_shared_minecraft(&source_dir);
        let skipped = tokio::task::spawn_blocking(move || {
            let skip = |name: &str| shared && crate::services::import::is_launcher_owned(name);
            crate::services::fs_utils::copy_import_dir(&source_dir, &game_dir_for_copy, &skip)
        })
        .await
        .map_err(|e| AppError::new("Couldn't copy this instance.", e.to_string()))??;
        if skipped > 0 {
            log::warn!(
                "import of {}: {skipped} locked/unreadable file(s) skipped",
                req.name
            );
        }

        for sub in [
            "mods",
            "config",
            "resourcepacks",
            "shaderpacks",
            "saves",
            "mods/.disabled",
            "resourcepacks/.disabled",
            "shaderpacks/.disabled",
        ] {
            std::fs::create_dir_all(game_dir.join(sub))?;
        }

        let instance = MinecraftInstance {
            id,
            name: req.name,
            minecraft_version: req.minecraft_version,
            loader: req.loader,
            loader_version: req.loader_version,
            game_dir: game_dir.to_string_lossy().to_string(),
            java_path: req.java_path,
            java_args: None,
            memory_mb: Some(crate::services::system::default_instance_memory_mb()),
            window_width: None,
            window_height: None,
            icon: None,
            created_at: now_secs(),
            last_played: None,
            total_playtime_secs: 0,
            imported: false,
            modpack_source: None,
            modpack_project_id: None,
            modpack_version_id: None,
            auto_update_content: false,
            group: None,
            notes: None,
            pre_launch_command: None,
            post_exit_command: None,
            favorite: false,
        };

        self.store.upsert(instance.clone()).await?;
        let _ = servers::add_server(
            &instance,
            "Arcane World".to_string(),
            "mc.arcaneworld.lk".to_string(),
        )
        .await;
        Ok(instance)
    }

    /// Zips this instance's `game_dir` (mods, config, saves, resourcepacks,
    /// shaderpacks, local metadata) to `dest_zip`. Deliberately does not
    /// bundle shared libraries/assets — this is a "share your setup"
    /// export, not a portable standalone Minecraft install.
    pub async fn export(&self, id: &str, dest_zip: std::path::PathBuf) -> AppResult<()> {
        let instance = self.get(id).await?;
        let source_dir = std::path::PathBuf::from(&instance.game_dir);
        tokio::task::spawn_blocking(move || {
            crate::services::fs_utils::zip_directory(&source_dir, &dest_zip)
        })
        .await
        .map_err(|e| AppError::new("Couldn't export this instance.", e.to_string()))?
    }
}
