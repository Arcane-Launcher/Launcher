use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::models::instance::MinecraftInstance;

#[derive(Clone)]
pub struct InstanceStore {
    path: PathBuf,
    state: Arc<RwLock<Vec<MinecraftInstance>>>,
}

impl InstanceStore {
    pub async fn load(path: PathBuf) -> Self {
        let instances = match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        Self {
            path,
            state: Arc::new(RwLock::new(instances)),
        }
    }

    pub async fn list(&self) -> Vec<MinecraftInstance> {
        self.state.read().await.clone()
    }

    pub async fn get(&self, id: &str) -> Option<MinecraftInstance> {
        self.state.read().await.iter().find(|i| i.id == id).cloned()
    }

    pub async fn upsert(&self, instance: MinecraftInstance) -> std::io::Result<()> {
        let mut guard = self.state.write().await;
        if let Some(existing) = guard.iter_mut().find(|i| i.id == instance.id) {
            *existing = instance;
        } else {
            guard.push(instance);
        }
        self.persist(&guard).await
    }

    pub async fn remove(&self, id: &str) -> std::io::Result<()> {
        let mut guard = self.state.write().await;
        guard.retain(|i| i.id != id);
        self.persist(&guard).await
    }

    async fn persist(&self, instances: &[MinecraftInstance]) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(instances)?;
        tokio::fs::write(&self.path, bytes).await
    }
}
