//! `arcane://join?server=…&mc=…&loader=…` links — what the website's join
//! page (behind the Discord "Join server" button) opens. A link only ever
//! opens the launcher's "Join" dialog; nothing launches until the player
//! picks an instance there.

use std::sync::{Arc, Mutex};

use crate::events::{Event, EventName, EventSink};
use crate::models::common::ModLoaderKind;
use crate::services::discord_rpc::DiscordJoinEvent;

pub use url::Url;

/// A join link that arrived before the UI was listening (the launcher was
/// started by the link). The UI collects it once on startup.
///
/// NOTE: the old Tauri version managed this via `app.manage(PendingJoin)`;
/// natively the owning shell keeps it in `AppState` (same Mutex semantics).
#[derive(Default)]
pub struct PendingJoin(pub Mutex<Option<DiscordJoinEvent>>);

// `Mutex` isn't `Clone`, so this is hand-written (as in the original
// Tauri-side usage, where the managed state was cloned the same way):
// the pending join, if any, is duplicated.
impl Clone for PendingJoin {
    fn clone(&self) -> Self {
        Self(Mutex::new(
            self.0.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        ))
    }
}

/// Reads a join link, rejecting anything that isn't a plain public
/// `host[:port]`, a short version string, and a known loader — the link can
/// come from any web page.
pub fn parse_join_url(url: &Url) -> Option<DiscordJoinEvent> {
    if url.scheme() != "arcane" || url.host_str() != Some("join") {
        return None;
    }
    let param = |key: &str| {
        url.query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.trim().to_string())
    };
    let server = param("server")?;
    let minecraft_version = param("mc")?;
    let loader = ModLoaderKind::from_id(&param("loader")?)?;

    let (host, port) = match server.rsplit_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (server.as_str(), None),
    };
    let host_ok = !host.is_empty()
        && host.len() <= 253
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && !host.starts_with(['.', '-']);
    let port_ok = port.is_none_or(|p| p.parse::<u16>().is_ok_and(|p| p > 0));
    let version_ok = !minecraft_version.is_empty()
        && minecraft_version.len() <= 32
        && minecraft_version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' '));
    if !host_ok
        || !port_ok
        || !version_ok
        || crate::services::discord_rpc::is_private_address(&server)
    {
        return None;
    }

    Some(DiscordJoinEvent {
        server,
        minecraft_version,
        loader: loader.id().to_string(),
        via_link: true,
    })
}

/// Handles links delivered while the launcher runs: hands the join to the
/// UI. The owning shell is responsible for focusing/raising the window
/// *before* calling this (the old Tauri version did
/// `unminimize + show + set_focus` inline; natively that lives in the GPUI
/// shell's open-urls handler, see MIGRATION.md §2.4 deep links).
pub fn handle_urls(sink: &Arc<dyn EventSink>, urls: &[Url]) {
    let Some(join) = urls.iter().find_map(parse_join_url) else {
        return;
    };
    sink.emit(Event::raw(EventName::DiscordJoin, join).expect("DiscordJoinEvent serializes"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Option<DiscordJoinEvent> {
        parse_join_url(&Url::parse(s).unwrap())
    }

    #[test]
    fn accepts_a_normal_link() {
        let j =
            parse("arcane://join?server=play.example.org%3A25577&mc=1.21.1&loader=fabric").unwrap();
        assert_eq!(
            (
                j.server.as_str(),
                j.minecraft_version.as_str(),
                j.loader.as_str()
            ),
            ("play.example.org:25577", "1.21.1", "fabric")
        );
        assert!(j.via_link);
    }

    #[test]
    fn rejects_bad_or_private_links() {
        assert!(parse("arcane://join?server=192.168.1.5&mc=1.21.1&loader=fabric").is_none());
        assert!(parse("arcane://join?server=localhost&mc=1.21.1&loader=fabric").is_none());
        assert!(parse("arcane://join?server=evil.com%20--x&mc=1.21.1&loader=fabric").is_none());
        assert!(parse("arcane://join?server=a.com:99999&mc=1.21.1&loader=fabric").is_none());
        assert!(parse("arcane://join?server=a.com&mc=1.21.1&loader=bukkit").is_none());
        assert!(parse("arcane://other?server=a.com&mc=1.21.1&loader=fabric").is_none());
        assert!(parse("arcane://join?server=a.com&loader=fabric").is_none());
    }
}
