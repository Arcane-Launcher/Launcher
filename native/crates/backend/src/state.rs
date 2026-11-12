use std::sync::Arc;

use crate::events::EventSink;
use crate::paths::AppPaths;
use crate::services::cache::settings_store::SettingsStore;
use crate::services::cache::store::JsonCache;
use crate::services::discord_rpc::DiscordPresence;
use crate::services::download::manager::DownloadManager;
use crate::services::instance::manager::InstanceManager;
use crate::services::instance::store::InstanceStore;
use crate::services::loaders::LoaderRegistry;
use crate::services::minecraft::manifest::VersionManifestService;
use crate::services::minecraft::running::RunningInstances;
use crate::services::minecraft::session_store::SessionStore;
use crate::services::minecraft::MinecraftEngine;
use crate::services::network::connectivity::NetworkMonitor;
use crate::services::providers::curseforge::CurseForgeProvider;
use crate::services::providers::modrinth::ModrinthProvider;
use crate::services::providers::ProviderRegistry;
use crate::services::recorder::Hotkeys;

#[derive(Clone)]
pub struct AppState {
    pub paths: AppPaths,
    pub settings: SettingsStore,
    pub network: NetworkMonitor,
    pub instances: InstanceManager,
    pub providers: ProviderRegistry,
    pub downloads: DownloadManager,
    pub engine: MinecraftEngine,
    pub loaders: LoaderRegistry,
    pub session: SessionStore,
    pub discord: DiscordPresence,
    pub running: RunningInstances,
    pub recorder: crate::services::recorder::Recorder,
    /// Global-shortcut registration (recording / instant-replay combos),
    /// supplied by the desktop shell — its `Hotkeys` implementation. Core
    /// never registers OS shortcuts itself (the old
    /// `tauri_plugin_global_shortcut` handle lived on the AppHandle).
    pub hotkeys: Arc<dyn Hotkeys>,
    /// The event boundary that replaced `tauri::AppHandle::emit` (see
    /// `events.rs`); every service the UI must observe routes through it.
    pub events: Arc<dyn EventSink>,
    /// A join link that arrived before the UI was listening (the launcher
    /// was started by the link). The old Tauri version managed this via
    /// `app.manage(PendingJoin)`; natively it lives here with the same
    /// `Mutex<Option<…>>` semantics.
    pub pending_join: crate::services::join_link::PendingJoin,
}

impl AppState {
    pub async fn initialize(
        events: Arc<dyn EventSink>,
        hotkeys: Arc<dyn Hotkeys>,
    ) -> std::io::Result<Self> {
        Self::initialize_at(events, hotkeys, AppPaths::new()?).await
    }

    /// Explicit-root variant (portable builds, tests). Same construction
    /// sequence as the Tauri `AppState::initialize(&AppHandle)`, which was:
    /// paths → settings → network (+ probe loop) → instance store →
    /// providers → downloads → manifest/engine → loaders → sessions →
    /// discord → running-instances.
    pub async fn initialize_at(
        events: Arc<dyn EventSink>,
        hotkeys: Arc<dyn Hotkeys>,
        paths: AppPaths,
    ) -> std::io::Result<Self> {
        let settings = SettingsStore::load(paths.settings_file()).await;
        let network = NetworkMonitor::new(events.clone(), settings.clone());
        network.spawn_probe_loop();

        let instance_store = InstanceStore::load(paths.instances_file()).await;
        let instances = InstanceManager::new(instance_store, paths.clone());

        let modrinth_cache = JsonCache::new(paths.modrinth_cache_dir());
        let modrinth = Arc::new(ModrinthProvider::new(modrinth_cache, network.clone()));

        let curseforge = Arc::new(CurseForgeProvider::new(settings.clone()));

        let providers = ProviderRegistry {
            modrinth,
            curseforge,
        };

        let downloads = DownloadManager::new(events.clone(), paths.clone());

        let manifest_cache = JsonCache::new(paths.cache_dir().join("mojang"));
        let manifest = VersionManifestService::new(manifest_cache, network.clone(), paths.clone());
        let engine = MinecraftEngine::new(manifest, downloads.clone(), paths.clone());

        let loaders = LoaderRegistry::new();
        let session = SessionStore::load(paths.auth_file()).await;
        let discord = DiscordPresence::new();
        let running = RunningInstances::new();

        Ok(Self {
            paths,
            settings,
            network,
            instances,
            providers,
            downloads,
            engine,
            loaders,
            session,
            discord,
            running,
            recorder: crate::services::recorder::Recorder::default(),
            hotkeys,
            events,
            pending_join: crate::services::join_link::PendingJoin::default(),
        })
    }
}
