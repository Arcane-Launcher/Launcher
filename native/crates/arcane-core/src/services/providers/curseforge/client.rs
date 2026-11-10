use std::time::Duration;

use serde::de::DeserializeOwned;

use crate::error::AppError;
use crate::services::cache::settings_store::SettingsStore;

const BASE_URL: &str = "https://api.curseforge.com";

/// Reads the API key from settings on every request rather than caching it
/// at construction, so saving a new key in Settings takes effect
/// immediately without restarting the launcher.
#[derive(Clone)]
pub struct CurseForgeClient {
    http: reqwest::Client,
    settings: SettingsStore,
}

impl CurseForgeClient {
    pub fn new(settings: SettingsStore) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("http client");
        Self { http, settings }
    }

    pub async fn is_configured(&self) -> bool {
        self.settings
            .get()
            .await
            .curseforge_api_key
            .is_some_and(|k| !k.trim().is_empty())
    }

    async fn require_key(&self) -> Result<String, AppError> {
        let key = self.settings.get().await.curseforge_api_key;
        key.filter(|k| !k.trim().is_empty()).ok_or_else(|| {
            AppError::new(
                "Add a CurseForge API key in Settings to enable CurseForge.",
                "curseforge api key not configured",
            )
        })
    }

    async fn handle_response<T: DeserializeOwned>(
        &self,
        url: &str,
        response: reqwest::Response,
    ) -> Result<T, AppError> {
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(AppError::new(
                "CurseForge rejected the configured API key.",
                format!("{status} from {url}"),
            ));
        }
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(AppError::not_found("This CurseForge resource"));
        }
        if !status.is_success() {
            return Err(AppError::new(
                "CurseForge returned an unexpected response.",
                format!("{status} from {url}"),
            ));
        }

        let bytes = response.bytes().await?;
        serde_json::from_slice(&bytes).map_err(|e| {
            AppError::new(
                "Received unexpected data from CurseForge.",
                format!("failed to parse {url}: {e}"),
            )
        })
    }

    pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, AppError> {
        let key = self.require_key().await?;
        let url = format!("{BASE_URL}{path}");
        let response = self
            .http
            .get(&url)
            .header("x-api-key", key)
            .header("Accept", "application/json")
            .send()
            .await?;
        self.handle_response(&url, response).await
    }

    pub async fn post_json<B: serde::Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, AppError> {
        let key = self.require_key().await?;
        let url = format!("{BASE_URL}{path}");
        let response = self
            .http
            .post(&url)
            .header("x-api-key", key)
            .header("Accept", "application/json")
            .json(body)
            .send()
            .await?;
        self.handle_response(&url, response).await
    }
}
