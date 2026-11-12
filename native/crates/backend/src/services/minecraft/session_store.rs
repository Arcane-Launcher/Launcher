use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::models::minecraft::AuthSession;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AccountsFile {
    #[serde(default)]
    accounts: Vec<AuthSession>,
    #[serde(default)]
    active_profile_id: Option<String>,
}

/// Persists every account the user has ever signed into (Microsoft or
/// offline), plus which one is currently active, so switching between them
/// is instant and never requires re-authenticating. Stored as a plain JSON
/// file under the app data directory rather than the OS keychain; see the
/// build plan's deferred-items list.
#[derive(Clone)]
pub struct SessionStore {
    path: PathBuf,
    state: Arc<RwLock<AccountsFile>>,
    /// Serializes token refreshes. Microsoft rotates refresh tokens, so two
    /// concurrent refreshes with the same one would invalidate each other.
    refresh_lock: Arc<tokio::sync::Mutex<()>>,
}

impl SessionStore {
    pub async fn load(path: PathBuf) -> Self {
        let data = match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => AccountsFile::default(),
        };
        Self {
            path,
            state: Arc::new(RwLock::new(data)),
            refresh_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Held for the whole read-check-refresh-save sequence so only one
    /// refresh runs at a time.
    pub async fn lock_refresh(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.refresh_lock.lock().await
    }

    /// Overwrites an already-saved account (e.g. with a refreshed token)
    /// without changing which account is active. No-op if it was removed in
    /// the meantime.
    pub async fn replace_existing(&self, session: AuthSession) -> std::io::Result<()> {
        let mut guard = self.state.write().await;
        let Some(slot) = guard
            .accounts
            .iter_mut()
            .find(|a| a.profile.id == session.profile.id)
        else {
            return Ok(());
        };
        *slot = session;
        self.persist(&guard).await
    }

    /// The currently active account, if any.
    pub async fn get(&self) -> Option<AuthSession> {
        let guard = self.state.read().await;
        let id = guard.active_profile_id.as_ref()?;
        guard.accounts.iter().find(|a| &a.profile.id == id).cloned()
    }

    /// Every saved account, active or not.
    pub async fn list(&self) -> Vec<AuthSession> {
        self.state.read().await.accounts.clone()
    }

    /// Saves a freshly-authenticated session (replacing any earlier one for
    /// the same profile, e.g. a refreshed Microsoft token) and makes it the
    /// active account.
    pub async fn upsert_and_activate(&self, session: AuthSession) -> std::io::Result<()> {
        let mut guard = self.state.write().await;
        guard
            .accounts
            .retain(|a| a.profile.id != session.profile.id);
        guard.accounts.push(session.clone());
        guard.active_profile_id = Some(session.profile.id.clone());
        self.persist(&guard).await
    }

    /// Switches the active account to an already-saved one, with no
    /// re-authentication needed.
    pub async fn set_active(&self, profile_id: &str) -> std::io::Result<Option<AuthSession>> {
        let mut guard = self.state.write().await;
        let Some(found) = guard
            .accounts
            .iter()
            .find(|a| a.profile.id == profile_id)
            .cloned()
        else {
            return Ok(None);
        };
        guard.active_profile_id = Some(profile_id.to_string());
        self.persist(&guard).await?;
        Ok(Some(found))
    }

    /// Signs out of the active account without forgetting it — it stays in
    /// the saved list for one-click switching back.
    pub async fn deactivate(&self) -> std::io::Result<()> {
        let mut guard = self.state.write().await;
        guard.active_profile_id = None;
        self.persist(&guard).await
    }

    /// Permanently forgets a saved account.
    pub async fn remove(&self, profile_id: &str) -> std::io::Result<()> {
        let mut guard = self.state.write().await;
        guard.accounts.retain(|a| a.profile.id != profile_id);
        if guard.active_profile_id.as_deref() == Some(profile_id) {
            guard.active_profile_id = None;
        }
        self.persist(&guard).await
    }

    async fn persist(&self, data: &AccountsFile) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(data)?;
        tokio::fs::write(&self.path, bytes).await
    }
}
