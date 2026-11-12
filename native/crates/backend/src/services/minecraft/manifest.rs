use std::time::Duration;

use crate::error::{AppError, AppResult};
use crate::models::minecraft::{Arguments, VersionDetail, VersionManifestEntry, VersionManifestV2};
use crate::paths::AppPaths;
use crate::services::cache::store::JsonCache;
use crate::services::network::connectivity::{prefer_ipv4, NetworkMonitor};

const MANIFEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const MANIFEST_TTL: Duration = Duration::from_secs(60 * 60);

/// A momentary DNS hiccup or dropped connection to Mojang's CDN is common
/// enough in practice (and these are small, cheap JSON fetches) that it's
/// worth a couple of quick retries before giving up — this only retries
/// connection-level/timeout failures, never a request that actually reached
/// the server (a bad status is returned as-is for the caller to handle).
async fn get_with_retry(
    http: &reqwest::Client,
    url: &str,
) -> Result<reqwest::Response, reqwest::Error> {
    const RETRY_DELAYS_MS: [u64; 2] = [300, 800];
    let mut attempt = 0;
    loop {
        match http.get(url).send().await {
            Ok(res) => return Ok(res),
            Err(e) if attempt < RETRY_DELAYS_MS.len() && (e.is_connect() || e.is_timeout()) => {
                tokio::time::sleep(Duration::from_millis(RETRY_DELAYS_MS[attempt])).await;
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

#[derive(Clone)]
pub struct VersionManifestService {
    http: reqwest::Client,
    cache: JsonCache,
    network: NetworkMonitor,
    paths: AppPaths,
}

impl VersionManifestService {
    pub fn new(cache: JsonCache, network: NetworkMonitor, paths: AppPaths) -> Self {
        Self {
            http: prefer_ipv4(
                reqwest::Client::builder()
                    .user_agent("ArcaneLauncher/0.1")
                    .timeout(Duration::from_secs(10)),
            )
            .build()
            .expect("http client"),
            cache,
            network,
            paths,
        }
    }

    pub async fn fetch_manifest(&self) -> AppResult<VersionManifestV2> {
        let key = "mojang:manifest";
        // Only ever skip the real request for the user's own explicit
        // Always-Offline setting. The background connectivity prober's
        // `state()` is a heuristic snapshot (one probe every 20s, 4s
        // timeout) that can briefly say "Offline" after a single slow
        // probe even though the network is fine — trusting it here used to
        // make the version list fail outright during that window instead
        // of just trying the request (now bounded by its own timeout above)
        // and falling back to cache like every other failure mode already
        // does below.
        if self.network.is_always_offline().await {
            return self
                .cache
                .get(key, Duration::from_secs(u64::MAX))
                .await
                .ok_or_else(AppError::offline);
        }
        match get_with_retry(&self.http, MANIFEST_URL).await {
            Ok(res) if res.status().is_success() => {
                let manifest: VersionManifestV2 = res.json().await.map_err(AppError::from)?;
                let _ = self.cache.set(key, &manifest).await;
                Ok(manifest)
            }
            _ => self
                .cache
                .get(key, MANIFEST_TTL * 1000)
                .await
                .ok_or_else(|| {
                    AppError::retryable(
                        "Couldn't reach Mojang's servers to list Minecraft versions.",
                        "version manifest fetch failed and no cache available",
                    )
                }),
        }
    }

    /// Chronological ordering of two Minecraft version ids, using Mojang's
    /// own `releaseTime` from the version manifest rather than guessing from
    /// the dotted version numbers (which sort wrong lexicographically, and
    /// don't cover snapshots at all). `None` if either id isn't a real
    /// version the manifest knows about.
    pub async fn compare_versions(&self, a: &str, b: &str) -> Option<std::cmp::Ordering> {
        let manifest = self.fetch_manifest().await.ok()?;
        let time_of = |id: &str| {
            manifest
                .versions
                .iter()
                .find(|v| v.id == id)
                .map(|v| v.release_time.clone())
        };
        let (time_a, time_b) = (time_of(a)?, time_of(b)?);
        Some(time_a.cmp(&time_b))
    }

    pub async fn find_entry(&self, version_id: &str) -> AppResult<VersionManifestEntry> {
        let manifest = self.fetch_manifest().await?;
        manifest
            .versions
            .into_iter()
            .find(|v| v.id == version_id)
            .ok_or_else(|| AppError::not_found("That Minecraft version"))
    }

    fn version_json_path(&self, version_id: &str) -> std::path::PathBuf {
        self.paths
            .versions_meta_dir()
            .join(crate::paths::sanitize_component(version_id))
            .join("version.json")
    }

    /// Persists an arbitrary (possibly loader-provided) version profile so
    /// it can be resolved offline later. Used by the Fabric/Quilt installers
    /// after they build a profile from their meta APIs.
    pub async fn store_version_detail(&self, version_id: &str, raw_json: &[u8]) -> AppResult<()> {
        let path = self.version_json_path(version_id);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(path, raw_json).await?;
        Ok(())
    }

    /// Same as [`Self::resolve_effective_version`] but never touches the
    /// network — used by offline instance validation, which must be able to
    /// report readiness without making any request.
    pub async fn resolve_effective_version_offline(
        &self,
        version_id: &str,
    ) -> Option<VersionDetail> {
        let detail = self.load_raw_version_detail(version_id).await?;
        let Some(parent_id) = detail.inherits_from.clone() else {
            return Some(detail);
        };
        let parent = Box::pin(self.resolve_effective_version_offline(&parent_id)).await?;
        Some(merge_versions(parent, detail))
    }

    async fn load_raw_version_detail(&self, version_id: &str) -> Option<VersionDetail> {
        let bytes = tokio::fs::read(self.version_json_path(version_id))
            .await
            .ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Ensures the vanilla version json for `version_id` is present locally,
    /// downloading it from Mojang if needed.
    pub async fn ensure_vanilla_version_detail(
        &self,
        version_id: &str,
    ) -> AppResult<VersionDetail> {
        if let Some(detail) = self.load_raw_version_detail(version_id).await {
            return Ok(detail);
        }
        if self.network.is_always_offline().await {
            return Err(AppError::new(
                "This Minecraft version needs to be downloaded first.",
                format!("no cached version json for {version_id} and offline"),
            ));
        }
        let entry = self.find_entry(version_id).await?;
        let res = get_with_retry(&self.http, &entry.url).await?;
        let bytes = res.bytes().await?;
        self.store_version_detail(version_id, &bytes).await?;
        serde_json::from_slice(&bytes).map_err(AppError::from)
    }

    /// Resolves a (possibly loader) version profile into its fully merged
    /// effective form by following `inheritsFrom` up to the vanilla base and
    /// concatenating libraries/arguments along the way.
    pub async fn resolve_effective_version(&self, version_id: &str) -> AppResult<VersionDetail> {
        let detail = match self.load_raw_version_detail(version_id).await {
            Some(d) => d,
            None => self.ensure_vanilla_version_detail(version_id).await?,
        };

        let Some(parent_id) = detail.inherits_from.clone() else {
            return Ok(detail);
        };

        let parent = Box::pin(self.resolve_effective_version(&parent_id)).await?;
        Ok(merge_versions(parent, detail))
    }
}

fn merge_versions(parent: VersionDetail, child: VersionDetail) -> VersionDetail {
    let mut libraries = parent.libraries;
    libraries.extend(child.libraries);

    let arguments = match (parent.arguments, child.arguments) {
        (Some(mut p), Some(c)) => {
            p.game.extend(c.game);
            p.jvm.extend(c.jvm);
            Some(p)
        }
        (None, Some(c)) => Some(c),
        (Some(p), None) => Some(p),
        (None, None) => None::<Arguments>,
    };

    VersionDetail {
        id: child.id,
        main_class: if child.main_class.is_empty() {
            parent.main_class
        } else {
            child.main_class
        },
        arguments,
        minecraft_arguments: child.minecraft_arguments.or(parent.minecraft_arguments),
        libraries,
        downloads: child.downloads.or(parent.downloads),
        asset_index: child.asset_index.or(parent.asset_index),
        assets: child.assets.or(parent.assets),
        java_version: child.java_version.or(parent.java_version),
        inherits_from: None,
        kind: child.kind.or(parent.kind),
    }
}
