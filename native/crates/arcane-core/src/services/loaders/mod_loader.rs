use crate::error::AppResult;
use crate::models::common::ModLoaderKind;
use crate::models::instance::MinecraftInstance;
use crate::services::minecraft::MinecraftEngine;

/// One Minecraft mod loader (Fabric, Forge, NeoForge, Quilt, ...). Adding a
/// new loader means implementing this trait and registering it in
/// [`super::LoaderRegistry`] — nothing else in the app needs to change.
#[async_trait::async_trait]
pub trait ModLoader: Send + Sync {
    fn kind(&self) -> ModLoaderKind;

    /// Whether this loader can actually be installed by the launcher yet.
    /// Forge/NeoForge report `false` in phase 1 (their installer/processor
    /// pipeline isn't implemented) so the UI can show "Coming soon" instead
    /// of silently failing or faking success.
    fn install_supported(&self) -> bool {
        true
    }

    async fn list_loader_versions(&self, minecraft_version: &str) -> AppResult<Vec<String>>;

    /// Installs the loader for `instance` (writing a merged version profile
    /// derived from the vanilla version) and returns the version key to
    /// pass to [`MinecraftEngine::ensure_installed`].
    async fn install(&self, instance: &MinecraftInstance, engine: &MinecraftEngine) -> AppResult<String>;

    /// Loader tags on a Modrinth/CurseForge version that this loader can run.
    fn accepted_version_tags(&self) -> &'static [&'static str] {
        self.kind().accepted_version_tags()
    }
}
