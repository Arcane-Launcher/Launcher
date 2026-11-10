use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{de::DeserializeOwned, Serialize};
use sha1::{Digest, Sha1};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CacheEnvelope<T> {
    cached_at: u64,
    data: T,
}

/// A persistent, file-backed cache with per-read TTL checks. Survives
/// application restarts because every entry is a JSON file on disk rather
/// than in-memory state.
#[derive(Debug, Clone)]
pub struct JsonCache {
    dir: PathBuf,
}

pub struct CacheHit<T> {
    pub data: T,
    pub cached_at: u64,
    pub is_stale: bool,
}

impl JsonCache {
    pub fn new(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        Self { dir }
    }

    fn path_for(&self, key: &str) -> PathBuf {
        let mut hasher = Sha1::new();
        hasher.update(key.as_bytes());
        let hash = hex::encode(hasher.finalize());
        self.dir.join(format!("{hash}.json"))
    }

    /// Returns the value only if present and within `ttl`.
    pub async fn get<T: DeserializeOwned>(&self, key: &str, ttl: Duration) -> Option<T> {
        let hit = self.get_any(key).await?;
        let now = now_secs();
        if now.saturating_sub(hit.cached_at) <= ttl.as_secs() {
            Some(hit.data)
        } else {
            None
        }
    }

    /// Returns the value regardless of age, flagging whether it is stale
    /// relative to `ttl`. Used for offline/degraded-mode fallbacks where
    /// showing old data with a "last updated" notice beats showing nothing.
    pub async fn get_with_staleness<T: DeserializeOwned>(
        &self,
        key: &str,
        ttl: Duration,
    ) -> Option<CacheHit<T>> {
        let hit = self.get_any(key).await?;
        let is_stale = now_secs().saturating_sub(hit.cached_at) > ttl.as_secs();
        Some(CacheHit {
            data: hit.data,
            cached_at: hit.cached_at,
            is_stale,
        })
    }

    async fn get_any<T: DeserializeOwned>(&self, key: &str) -> Option<CacheHit<T>> {
        let path = self.path_for(key);
        let bytes = tokio::fs::read(&path).await.ok()?;
        let envelope: CacheEnvelope<T> = serde_json::from_slice(&bytes).ok()?;
        Some(CacheHit {
            data: envelope.data,
            cached_at: envelope.cached_at,
            is_stale: false,
        })
    }

    pub async fn set<T: Serialize>(&self, key: &str, value: &T) -> std::io::Result<()> {
        let envelope = CacheEnvelope {
            cached_at: now_secs(),
            data: value,
        };
        let bytes = serde_json::to_vec(&envelope)?;
        let path = self.path_for(key);
        tokio::fs::write(path, bytes).await
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
