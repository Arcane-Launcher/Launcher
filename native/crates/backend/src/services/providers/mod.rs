pub mod curseforge;
pub mod mod_provider;
pub mod modrinth;

use std::sync::Arc;

use crate::models::common::ModSource;

use mod_provider::ModProvider;

/// Holds one provider implementation per mod source and dispatches to the
/// right one. Adding a new source (a second CurseForge-alike, etc.) means
/// implementing `ModProvider` and adding one arm here — nothing else in the
/// app needs to change.
#[derive(Clone)]
pub struct ProviderRegistry {
    pub modrinth: Arc<modrinth::ModrinthProvider>,
    pub curseforge: Arc<curseforge::CurseForgeProvider>,
}

impl ProviderRegistry {
    pub fn get(&self, source: ModSource) -> Arc<dyn ModProvider> {
        match source {
            ModSource::Modrinth => self.modrinth.clone(),
            ModSource::CurseForge => self.curseforge.clone(),
        }
    }
}
