use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::sync::Semaphore;

use crate::error::AppError;

const BASE_URL: &str = "https://api.modrinth.com/v2";
// Collections were only ever added to v3; every other endpoint this client
// uses still lives on v2.
const BASE_URL_V3: &str = "https://api.modrinth.com/v3";
const MAX_CONCURRENT_REQUESTS: usize = 6;
const MAX_RETRIES: u32 = 3;

/// Thin wrapper around the Modrinth REST API: bounded concurrency so a burst
/// of UI actions can't hammer the API, and retry-with-backoff that honors
/// `Retry-After` on 429s.
#[derive(Clone)]
pub struct ModrinthClient {
    http: reqwest::Client,
    semaphore: Arc<Semaphore>,
}

impl ModrinthClient {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent("ArcaneLauncher/0.1 (contact: launcher@arcane.local)")
            .timeout(Duration::from_secs(15))
            .build()
            .expect("http client");
        Self {
            http,
            semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS)),
        }
    }

    pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, AppError> {
        self.get_json_from(&format!("{BASE_URL}{path}")).await
    }

    /// Same as [`get_json`](Self::get_json), against the v3 API instead of
    /// this client's default v2 base URL.
    pub async fn get_json_v3<T: DeserializeOwned>(&self, path: &str) -> Result<T, AppError> {
        self.get_json_from(&format!("{BASE_URL_V3}{path}")).await
    }

    /// A one-off POST (the hash-lookup endpoint is the only place this
    /// client needs one), with the same rate-limit/retry handling as
    /// `get_json` rather than the full generalized-per-verb machinery.
    pub async fn post_json<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, AppError> {
        let url = format!("{BASE_URL}{path}");
        let _permit = self.semaphore.acquire().await.expect("semaphore closed");

        let mut attempt = 0;
        loop {
            attempt += 1;
            let response = self.http.post(&url).json(body).send().await?;
            let status = response.status();

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let retry_after = response
                    .headers()
                    .get("Retry-After")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(2);
                if attempt >= MAX_RETRIES {
                    return Err(AppError::retryable(
                        "Modrinth is rate-limiting requests right now.",
                        format!("429 from {url}"),
                    ));
                }
                tokio::time::sleep(Duration::from_secs(retry_after)).await;
                continue;
            }

            if !status.is_success() {
                return Err(AppError::new(
                    "Modrinth returned an unexpected response.",
                    format!("{status} from {url}"),
                ));
            }

            let bytes = response.bytes().await?;
            return serde_json::from_slice(&bytes).map_err(|e| {
                AppError::new(
                    "Received unexpected data from Modrinth.",
                    format!("failed to parse {url}: {e}"),
                )
            });
        }
    }

    async fn get_json_from<T: DeserializeOwned>(&self, url: &str) -> Result<T, AppError> {
        let url = url.to_string();
        let _permit = self.semaphore.acquire().await.expect("semaphore closed");

        let mut attempt = 0;
        loop {
            attempt += 1;
            let response = self.http.get(&url).send().await?;
            let status = response.status();

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let retry_after = response
                    .headers()
                    .get("Retry-After")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(2);
                if attempt >= MAX_RETRIES {
                    return Err(AppError::retryable(
                        "Modrinth is rate-limiting requests right now.",
                        format!("429 from {url}"),
                    ));
                }
                tokio::time::sleep(Duration::from_secs(retry_after)).await;
                continue;
            }

            if status == reqwest::StatusCode::NOT_FOUND {
                return Err(AppError::not_found("This Modrinth resource"));
            }

            if status.is_server_error() {
                if attempt >= MAX_RETRIES {
                    return Err(AppError::retryable(
                        "Modrinth is temporarily unavailable.",
                        format!("{status} from {url}"),
                    ));
                }
                tokio::time::sleep(Duration::from_millis(400 * attempt as u64)).await;
                continue;
            }

            if !status.is_success() {
                return Err(AppError::new(
                    "Modrinth returned an unexpected response.",
                    format!("{status} from {url}"),
                ));
            }

            let bytes = response.bytes().await?;
            return serde_json::from_slice(&bytes).map_err(|e| {
                AppError::new(
                    "Received unexpected data from Modrinth.",
                    format!("failed to parse {url}: {e}"),
                )
            });
        }
    }
}

impl Default for ModrinthClient {
    fn default() -> Self {
        Self::new()
    }
}
