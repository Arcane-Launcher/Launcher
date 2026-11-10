//! Typed event boundary — the single abstraction that removes Tauri from the
//! backend.
//!
//! Tauri's `AppHandle::emit(name, payload)` in services is replaced by
//! `EventSink::emit(Event::…)` (see MIGRATION.md §2.2). The 14 event kinds
//! below are the exact set listened to by the current frontend
//! (`src/lib/tauri-client.ts` + components), with payload shapes preserved
//! 1:1 from the Tauri emit sites:
//!
//! | Event (Tauri name)        | Emitted by (today)                   | Listened by (today)                    |
//! |---------------------------|---------------------------------------|----------------------------------------|
//! | `auth://progress`         | minecraft/auth.rs::emit_progress      | auth-panel                             |
//! | `launch://started`        | minecraft/launch.rs                   | use-running-instances, console         |
//! | `launch://log`            | minecraft/launch.rs (stdout + stderr) | instance-console                       |
//! | `launch://exit`           | minecraft/launch.rs                   | use-running-instances, crash-listener  |
//! | `install://stage`         | commands/launch.rs (install stages)   | haunted-launch, create-instance        |
//! | `download://progress`     | download/manager.rs::emit             | use-downloads / download-tray          |
//! | `updater://progress`      | services/updater.rs                   | lib/updater.ts                         |
//! | `network://status`        | network/connectivity.rs::set_state    | use-network-status                     |
//! | `recorder://state`        | recorder/mod.rs                       | recorder-listener                      |
//! | `recorder://saved`        | recorder/mod.rs (6 sites)             | recorder-listener                      |
//! | `discord://join`          | discord_rpc.rs, join_link.rs          | discord-join-listener                  |
//! | `discord://join_request`  | discord_rpc.rs                        | discord-join-listener                  |
//! | `discord://join_request_resolved` | discord_rpc.rs                 | discord-join-listener                  |
//!
//! Payload structs are typed per-module in the migration; during
//! Phase 1 each payload type is copied verbatim next to its emitting service
//! and referenced here, so the enum grows exactly as services are ported.

use crate::error::AppError;
use serde::{Deserialize, Serialize};

/// Mirror of the frontend event names, so string breakdowns never happen by
/// accident. The GPUI side maps kinds → UI store updates using the same
/// routing the react-query/listen subscribers used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventName {
    #[serde(rename = "auth://progress")]
    AuthProgress,
    #[serde(rename = "launch://started")]
    LaunchStarted,
    #[serde(rename = "launch://log")]
    LaunchLog,
    #[serde(rename = "launch://exit")]
    LaunchExit,
    #[serde(rename = "install://stage")]
    InstallStage,
    #[serde(rename = "download://progress")]
    DownloadProgress,
    #[serde(rename = "updater://progress")]
    UpdaterProgress,
    #[serde(rename = "network://status")]
    NetworkStatus,
    #[serde(rename = "recorder://state")]
    RecorderState,
    #[serde(rename = "recorder://saved")]
    RecorderSaved,
    #[serde(rename = "discord://join")]
    DiscordJoin,
    #[serde(rename = "discord://join_request")]
    DiscordJoinRequest,
    #[serde(rename = "discord://join_request_resolved")]
    DiscordJoinRequestResolved,
}

impl EventName {
    /// The exact Tauri event name this kind corresponds to.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AuthProgress => "auth://progress",
            Self::LaunchStarted => "launch://started",
            Self::LaunchLog => "launch://log",
            Self::LaunchExit => "launch://exit",
            Self::InstallStage => "install://stage",
            Self::DownloadProgress => "download://progress",
            Self::UpdaterProgress => "updater://progress",
            Self::NetworkStatus => "network://status",
            Self::RecorderState => "recorder://state",
            Self::RecorderSaved => "recorder://saved",
            Self::DiscordJoin => "discord://join",
            Self::DiscordJoinRequest => "discord://join_request",
            Self::DiscordJoinRequestResolved => "discord://join_request_resolved",
        }
    }
}

/// Untyped payload until the corresponding service is ported in Phase 1;
/// the serialized JSON must be identical to the Tauri emit for the same site.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Payload(pub serde_json::Value);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub name: EventName,
    pub payload: Payload,
}

impl Event {
    /// Constructor used by the mechanical emit-site replacements in
    /// Phase 1: `sink.emit(Event::raw(EventName::LaunchLog, payload)?)`
    /// before the typed variant for that service lands.
    pub fn raw<E: Serialize>(name: EventName, payload: E) -> Result<Event, AppError> {
        let value = serde_json::to_value(payload).map_err(|e| AppError {
            message: format!("event serialization failed for {}", name.as_str()),
            technical: e.to_string(),
            retryable: false,
        })?;
        Ok(Event { name, payload: Payload(value) })
    }
}

/// The one trait services get instead of `tauri::AppHandle`.
///
/// Implementations: the GPUI shell drains events onto its main thread and
/// updates the store entities; tests can collect events into a Vec.
pub trait EventSink: Send + Sync {
    fn emit(&self, event: Event);
}

/// Fan-out sink used during migration so several consumers (UI, audio cues,
/// background services) can observe the same stream, mirroring how the
/// frontend had multiple independent `listen()` subscribers per event.
pub struct FanOut {
    sinks: Vec<Box<dyn EventSink>>,
}

impl FanOut {
    pub fn new() -> Self {
        Self { sinks: Vec::new() }
    }

    pub fn push(&mut self, sink: Box<dyn EventSink>) {
        self.sinks.push(sink);
    }
}

impl Default for FanOut {
    fn default() -> Self {
        Self::new()
    }
}

impl EventSink for FanOut {
    fn emit(&self, event: Event) {
        for sink in &self.sinks {
            sink.emit(event.clone());
        }
    }
}

/// Test/dev sink collecting everything seen (also the error-reporting helper
/// for emit failures: silent on the Tauri side today, so silent here too).
#[derive(Default)]
pub struct CollectingSink {
    pub events: std::sync::Mutex<Vec<Event>>,
}

impl CollectingSink {
    pub fn new() -> Self {
        Self::default()
    }
}

impl EventSink for CollectingSink {
    fn emit(&self, event: Event) {
        if let Ok(mut events) = self.events.lock() {
            events.push(event);
        }
    }
}

/// `Arc<T>` handles to the same sink (the UI and background services hold
/// shared handles to one sink instance).
impl<T: EventSink + ?Sized> EventSink for std::sync::Arc<T> {
    fn emit(&self, event: Event) {
        (**self).emit(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct Progress {
        stage: String,
        message: String,
    }

    #[test]
    fn raw_serializes_payload_and_preserves_name() {
        let event = Event::raw(
            EventName::AuthProgress,
            Progress { stage: "token".into(), message: "exchanging token".into() },
        )
        .unwrap();
        assert_eq!(event.name.as_str(), "auth://progress");
        assert_eq!(event.payload.0["stage"], "token");
    }

    #[test]
    fn fan_out_delivers_to_all_sinks() {
        let a = std::sync::Arc::new(CollectingSink::new());
        let b = std::sync::Arc::new(CollectingSink::new());
        let mut fan = FanOut::new();
        fan.push(Box::new(a.clone()));
        fan.push(Box::new(b.clone()));
        let event = Event::raw(EventName::NetworkStatus, serde_json::json!({"state": "online"})).unwrap();
        fan.emit(event);
        assert_eq!(a.events.lock().unwrap().len(), 1);
        assert_eq!(b.events.lock().unwrap().len(), 1);
    }
}
