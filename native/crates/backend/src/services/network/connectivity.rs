use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::RwLock;

use crate::events::{Event, EventName, EventSink};

use crate::models::common::ConnectivityState;
use crate::models::settings::OfflineModePreference;
use crate::services::cache::settings_store::SettingsStore;

const MODRINTH_PROBE_URL: &str = "https://api.modrinth.com/v2/tag/category";
const GENERAL_PROBE_URL: &str = "https://www.gstatic.com/generate_204";
const PROBE_INTERVAL: Duration = Duration::from_secs(20);
const PROBE_TIMEOUT: Duration = Duration::from_secs(4);

/// Pins outgoing connections to IPv4. Mojang/Microsoft's login, profile, and
/// download endpoints (Xbox Live, api.minecraftservices.com,
/// piston-meta/piston-data.mojang.com) all sit behind Azure Front Door, and
/// a dual-stack machine with a broken IPv6 path to Azure (a common real-world
/// router/ISP misconfiguration — see PrismLauncher's own issue tracker and
/// troubleshooting wiki, which documents the identical symptom against the
/// same Microsoft-owned endpoints) will see every request to them hang or
/// time out even though IPv4 works fine. reqwest's connector already does
/// Happy-Eyeballs racing, but that only covers the initial TCP handshake —
/// it can't help when the handshake itself succeeds and the connection
/// stalls afterward (a classic symptom of IPv6 MTU/MSS-clamping blackholes),
/// which is exactly the failure mode this pins around. PrismLauncher tried
/// fixing this with a manual Host/SNI-rewriting hack and abandoned it after
/// it broke cookies and non-default ports; binding the local socket address
/// via reqwest's own `local_address` achieves the same outcome without any
/// of that — DNS resolution, the Host header, and TLS SNI are untouched, so
/// only which resolved address family gets connected to changes. This is
/// only applied to Mojang/Microsoft clients, never Modrinth/CurseForge,
/// since there's no evidence of the same issue there.
pub fn prefer_ipv4(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    builder.local_address(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
}

#[derive(Debug, Clone, Serialize)]
pub struct NetworkStatus {
    pub state: ConnectivityStateJson,
    pub modrinth_reachable: bool,
    pub always_offline: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectivityStateJson {
    Online,
    Offline,
    Connecting,
    Limited,
}

impl From<ConnectivityState> for ConnectivityStateJson {
    fn from(value: ConnectivityState) -> Self {
        match value {
            ConnectivityState::Online => Self::Online,
            ConnectivityState::Offline => Self::Offline,
            ConnectivityState::Connecting => Self::Connecting,
            ConnectivityState::Limited => Self::Limited,
        }
    }
}

#[derive(Clone)]
pub struct NetworkMonitor {
    inner: Arc<Inner>,
}

struct Inner {
    sink: Arc<dyn EventSink>,
    client: reqwest::Client,
    settings: SettingsStore,
    state: RwLock<ConnectivityState>,
    modrinth_reachable: RwLock<bool>,
}

impl NetworkMonitor {
    pub fn new(sink: Arc<dyn EventSink>, settings: SettingsStore) -> Self {
        let client = reqwest::Client::builder()
            .timeout(PROBE_TIMEOUT)
            .build()
            .expect("http client");
        Self {
            inner: Arc::new(Inner {
                sink,
                client,
                settings,
                state: RwLock::new(ConnectivityState::Connecting),
                modrinth_reachable: RwLock::new(false),
            }),
        }
    }

    pub fn spawn_probe_loop(&self) {
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                this.force_check().await;
                tokio::time::sleep(PROBE_INTERVAL).await;
            }
        });
    }

    pub async fn state(&self) -> ConnectivityState {
        if self.is_always_offline().await {
            return ConnectivityState::Offline;
        }
        *self.inner.state.read().await
    }

    pub async fn is_modrinth_reachable(&self) -> bool {
        if self.is_always_offline().await {
            return false;
        }
        *self.inner.modrinth_reachable.read().await
    }

    /// Whether the user has explicitly opted into Always-Offline mode —
    /// unlike [`Self::state`], this never depends on the background probe
    /// loop, so callers that want a hard, deterministic "skip the network
    /// entirely" signal (rather than a heuristic snapshot that can be
    /// briefly wrong after a single slow probe) should check this instead.
    pub async fn is_always_offline(&self) -> bool {
        matches!(
            self.inner.settings.get().await.offline_mode,
            OfflineModePreference::AlwaysOffline
        )
    }

    pub async fn force_check(&self) -> ConnectivityState {
        if self.is_always_offline().await {
            self.set_state(ConnectivityState::Offline, false).await;
            return ConnectivityState::Offline;
        }

        let modrinth_ok = probe(&self.inner.client, MODRINTH_PROBE_URL).await;
        let new_state = if modrinth_ok {
            ConnectivityState::Online
        } else {
            let general_ok = probe(&self.inner.client, GENERAL_PROBE_URL).await;
            if general_ok {
                ConnectivityState::Limited
            } else {
                ConnectivityState::Offline
            }
        };

        self.set_state(new_state, modrinth_ok).await;
        new_state
    }

    async fn set_state(&self, new_state: ConnectivityState, modrinth_ok: bool) {
        let mut state = self.inner.state.write().await;
        let mut reachable = self.inner.modrinth_reachable.write().await;
        let changed = *state != new_state || *reachable != modrinth_ok;
        *state = new_state;
        *reachable = modrinth_ok;
        if changed {
            let _ = self.inner.sink.emit(
                Event::raw(EventName::NetworkStatus, NetworkStatus {
                    state: new_state.into(),
                    modrinth_reachable: modrinth_ok,
                    always_offline: false,
                })
                .expect("NetworkStatus serializes"),
            );
        }
    }
}

async fn probe(client: &reqwest::Client, url: &str) -> bool {
    client
        .get(url)
        .send()
        .await
        .map(|res| res.status().is_success() || res.status().as_u16() == 204)
        .unwrap_or(false)
}
