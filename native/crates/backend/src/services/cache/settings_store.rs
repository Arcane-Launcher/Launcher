use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::models::settings::LauncherSettings;

#[derive(Clone)]
pub struct SettingsStore {
    path: PathBuf,
    state: Arc<RwLock<LauncherSettings>>,
}

impl SettingsStore {
    pub async fn load(path: PathBuf) -> Self {
        let settings = match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => LauncherSettings::default(),
        };
        Self {
            path,
            state: Arc::new(RwLock::new(settings)),
        }
    }

    pub async fn get(&self) -> LauncherSettings {
        self.state.read().await.clone()
    }

    pub async fn update<F>(&self, mutator: F) -> std::io::Result<LauncherSettings>
    where
        F: FnOnce(&mut LauncherSettings),
    {
        let mut guard = self.state.write().await;
        mutator(&mut guard);
        let bytes = serde_json::to_vec_pretty(&*guard)?;
        tokio::fs::write(&self.path, bytes).await?;
        Ok(guard.clone())
    }
}
