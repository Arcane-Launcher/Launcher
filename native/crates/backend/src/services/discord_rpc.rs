use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::events::{Event, EventName, EventSink};
use base64::Engine;
use discord_rich_presence::{activity, DiscordIpc, DiscordIpcClient};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::models::common::ModLoaderKind;
use crate::models::instance::MinecraftInstance;
use crate::models::settings::{DiscordJoinMode, LauncherSettings};
use crate::services::cache::settings_store::SettingsStore;

/// Arcane Launcher's own Discord Application ID, baked in by `build.rs`
/// (from `DISCORD_CLIENT_ID` or the gitignored `discord-client-id.key`) so
/// it isn't in source control. Empty in a build that has neither.
const DEFAULT_CLIENT_ID: &str = env!("ARCANE_DISCORD_CLIENT_ID");

/// The Discord application ID to publish presence under: the user's
/// override if set, else the one baked into this build. `None` means Rich
/// Presence has nothing to connect with and is silently skipped.
pub fn resolve_client_id(user_override: Option<String>) -> Option<String> {
    user_override
        .filter(|s| !s.trim().is_empty())
        .or_else(|| (!DEFAULT_CLIENT_ID.is_empty()).then(|| DEFAULT_CLIENT_ID.to_string()))
}

pub const WEBSITE_URL: &str = "https://launcher.arcaneworld.lk";

/// Presence art is referenced by URL (Discord proxies external https
/// images) rather than by Developer Portal asset key: the app's uploaded
/// assets were checked via Discord's public
/// `/oauth2/applications/{id}/assets` endpoint and hold neither `logo` nor
/// any loader art, so the old keys rendered no images at all. A URL also
/// means updating the logo on the website updates it here, no re-upload.
const LOGO_URL: &str = "https://launcher.arcaneworld.lk/logo.png";

/// October's large image: a lit jack-o'-lantern, served by the website next
/// to the logo (`website/public/halloween-pumpkin.png`).
const PUMPKIN_URL: &str = "https://launcher.arcaneworld.lk/halloween-pumpkin.png";

/// The local calendar month, 1-12.
fn local_month() -> u32 {
    #[cfg(windows)]
    {
        u32::from(unsafe { windows::Win32::System::SystemInformation::GetLocalTime() }.wMonth)
    }
    #[cfg(not(windows))]
    {
        // UTC is close enough for "is it October" outside Windows.
        let days = (crate::services::cache::store::now_secs() / 86_400) as i64;
        month_from_unix_days(days)
    }
}

/// Civil month from days since 1970-01-01 (Howard Hinnant's algorithm).
#[cfg_attr(windows, allow(dead_code))]
fn month_from_unix_days(days: i64) -> u32 {
    let z = days + 719_468;
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    (if mp < 10 { mp + 3 } else { mp - 9 }) as u32
}

/// Spooky presence: all of October, unless turned off in Settings.
fn spooky_season(settings: &LauncherSettings) -> bool {
    !settings.discord_spooky_disabled && local_month() == 10
}

/// The player's head (face + hat layer) as the small presence image.
/// `skin_id` is a profile UUID (Microsoft accounts), a Mojang texture hash
/// (an offline account's skin uploaded to MineSkin) or `MHF_Steve`.
/// mc-heads.net takes all three in the path — no query string, which
/// Discord's external-image proxy mangles.
pub fn player_head_url(skin_id: &str) -> String {
    format!(
        "https://mc-heads.net/avatar/{}/128",
        skin_id.replace('-', "")
    )
}

/// What to render the head from when no Microsoft UUID or uploaded skin
/// is known: the default skin, never a guess by name (an offline name can
/// belong to someone else's real account).
pub const DEFAULT_HEAD: &str = "MHF_Steve";

fn loader_label(loader: ModLoaderKind) -> &'static str {
    match loader {
        ModLoaderKind::Vanilla => "Vanilla",
        ModLoaderKind::Fabric => "Fabric",
        ModLoaderKind::Quilt => "Quilt",
        ModLoaderKind::Forge => "Forge",
        ModLoaderKind::NeoForge => "NeoForge",
    }
}

/// A fully-specified Rich Presence activity. `party`/`join_url` are only
/// meaningful while connected to a public multiplayer server —
/// singleplayer/idle activities simply leave them `None`.
#[derive(Clone, Default)]
pub struct PresenceActivity {
    pub details: String,
    pub state: String,
    pub started_at: i64,
    pub small_image: Option<String>,
    pub small_text: Option<String>,
    pub party: Option<PresenceParty>,
    /// Enables Discord's "Ask to Join" (replaces the buttons).
    pub join_secret: Option<String>,
    /// The website's join page for this server, shown as a "Join server"
    /// button (see [`join_page_url`]).
    pub join_url: Option<String>,
    /// October: pumpkin art and " Haunting ..." wording.
    pub spooky: bool,
}

#[derive(Clone)]
pub struct PresenceParty {
    pub id: String,
    pub size: i32,
    pub max: i32,
}

/// Sent to the frontend when a Discord friend clicks "Ask to Join" while
/// this launcher is hosting (i.e. connected to a server with a join secret
/// set). The launcher must explicitly accept or decline within the window
/// below — Discord does not do this automatically.
#[derive(Debug, Clone, Serialize)]
pub struct DiscordJoinRequestEvent {
    pub user_id: String,
    pub username: String,
    pub avatar: Option<String>,
}

/// Sent to the frontend once a pending join request has been answered
/// (accepted, declined, or timed out), so any "X wants to join" prompt can
/// dismiss itself.
#[derive(Debug, Clone, Serialize)]
pub struct DiscordJoinResolvedEvent {
    pub accepted: bool,
}

/// Sent to the frontend when a friend has approved *this* launcher's request
/// to join their game — decoded from the join secret Discord hands back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordJoinEvent {
    pub server: String,
    pub minecraft_version: String,
    pub loader: String,
    /// Came from a "Join server" link (arcane://join) rather than a
    /// Discord Ask-to-Join invite.
    #[serde(default)]
    pub via_link: bool,
}

/// How long the host has to accept/decline an incoming "Ask to Join" before
/// it's automatically declined.
const JOIN_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The website page behind the "Join server" button. Discord only allows
/// https links on buttons, so the page hands off to `arcane://join` (see
/// `services::join_link`) or offers the download.
pub fn join_page_url(server: &str, minecraft_version: &str, loader: &str) -> String {
    format!(
        "{WEBSITE_URL}/join?server={}&mc={}&loader={}",
        urlencoding::encode(server),
        urlencoding::encode(minecraft_version),
        urlencoding::encode(loader)
    )
}

/// What a friend's launcher needs to follow you: the inverse of
/// [`decode_join_secret`].
fn encode_join_secret(server: &str, minecraft_version: &str, loader: &str) -> String {
    let json = json!({ "server": server, "mc": minecraft_version, "loader": loader }).to_string();
    base64::engine::general_purpose::STANDARD.encode(json)
}

/// How often the player count is re-pinged while on a server.
const PLAYER_COUNT_REFRESH: Duration = Duration::from_secs(60);

fn decode_join_secret(secret: &str) -> Option<DiscordJoinEvent> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(secret)
        .ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    Some(DiscordJoinEvent {
        server: value.get("server")?.as_str()?.to_string(),
        minecraft_version: value.get("mc")?.as_str()?.to_string(),
        loader: value.get("loader")?.as_str()?.to_string(),
        via_link: false,
    })
}

/// What the running game is doing, as far as its own log reveals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GamePhase {
    Loading,
    MainMenu,
    Singleplayer,
    /// `host[:port]` (port omitted when it's the default 25565).
    Multiplayer(String),
}

/// Recognizes the handful of vanilla log lines that mark a phase change.
/// All come from Minecraft's own code, so they read the same on every
/// loader: `Sound engine started` (title screen reached), `Starting
/// integrated minecraft server version ...` (a singleplayer world is
/// loading), `Stopping singleplayer server as player logged out` (back to
/// the menu), and `ConnectScreen`'s `Connecting to <host>, <port>`. There
/// is no equivalent line for leaving a multiplayer server, so that status
/// holds until the next one of these appears.
pub fn parse_game_log(line: &str, current: &GamePhase) -> Option<GamePhase> {
    if line.contains("Starting integrated minecraft server") {
        return Some(GamePhase::Singleplayer);
    }
    if line.contains("Stopping singleplayer server as player logged out") {
        return Some(GamePhase::MainMenu);
    }
    // Also logged on every resource reload (F3+T, changing packs), so it
    // only means "title screen" while the game is still starting up.
    if line.contains("Sound engine started") && *current == GamePhase::Loading {
        return Some(GamePhase::MainMenu);
    }
    if let Some(idx) = line.find("Connecting to ") {
        let rest = &line[idx + "Connecting to ".len()..];
        let (host, port) = rest.split_once(", ")?;
        let host = host.trim();
        let port: u16 = port.trim().parse().ok()?;
        if host.is_empty() {
            return None;
        }
        let address = if port == 25565 {
            host.to_string()
        } else {
            format!("{host}:{port}")
        };
        return Some(GamePhase::Multiplayer(address));
    }
    None
}

/// The advancement name when `line` is the game announcing one earned by
/// `player` — `[CHAT] Steve has made the advancement [Diamonds!]` (also
/// "completed the challenge" / "reached the goal"). Only English game text
/// is recognised, and a server's rank prefix before the name is allowed.
pub fn parse_advancement(line: &str, player: &str) -> Option<String> {
    let chat = &line[line.find("[CHAT] ")? + "[CHAT] ".len()..];
    let (who, rest) = [
        "has made the advancement [",
        "has completed the challenge [",
        "has reached the goal [",
    ]
    .iter()
    .find_map(|marker| {
        chat.find(marker)
            .map(|i| (&chat[..i], &chat[i + marker.len()..]))
    })?;
    let who = who.trim();
    let named = who.len() >= player.len()
        && who[who.len() - player.len()..].eq_ignore_ascii_case(player)
        && who[..who.len() - player.len()]
            .chars()
            .last()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_');
    if !named || player.is_empty() {
        return None;
    }
    let name = rest.trim_end().strip_suffix(']')?.trim();
    (!name.is_empty()).then(|| name.chars().take(64).collect())
}

/// How long a new advancement stays in the status.
const ADVANCEMENT_SHOWN_FOR: Duration = Duration::from_secs(60);

/// LAN/loopback/private addresses mean nothing to friends and would leak
/// the player's network layout, so they're never shown or offered to join.
pub fn is_private_address(address: &str) -> bool {
    let host = match address.parse::<std::net::SocketAddr>() {
        Ok(sock) => sock.ip().to_string(),
        Err(_) => address
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(address)
            .to_string(),
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".local") || host.ends_with(".lan")
    {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => {
            ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified()
        }
        Ok(std::net::IpAddr::V6(ip)) => {
            let first = ip.segments()[0];
            ip.is_loopback()
                || ip.is_unspecified()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
        }
        Err(_) => false,
    }
}

/// Everything presence needs about the game that's running.
#[derive(Clone)]
struct GameSession {
    instance_id: String,
    instance_name: String,
    minecraft_version: String,
    loader: ModLoaderKind,
    started_at: i64,
    phase: GamePhase,
    game_dir: std::path::PathBuf,
    player_name: String,
    /// Small presence image; hovering it shows `player_name`.
    player_head_url: String,
    /// The open singleplayer world's name, looked up once per world.
    world_name: Option<String>,
    /// The server's name from the player's own server list.
    server_name: Option<String>,
    /// Shown for [`ADVANCEMENT_SHOWN_FOR`] after it's earned.
    advancement: Option<String>,
}

/// Optional "Playing Minecraft" Discord status, plus (while connected to a
/// server) the ability for friends to "Ask to Join". Entirely cosmetic —
/// every operation here silently no-ops on failure (Discord not running, no
/// client ID configured, a stale pipe) rather than surfacing an error, since
/// this must never be able to interfere with an actual game launch.
#[derive(Clone)]
pub struct DiscordPresence {
    client: Arc<Mutex<Option<DiscordIpcClient>>>,
    /// Set while a join request is awaiting the user's accept/decline;
    /// `respond_join_request` sends into it to wake up the listener thread
    /// blocked on `recv_timeout`.
    pending_join: Arc<Mutex<Option<std_mpsc::Sender<bool>>>>,
    game: Arc<Mutex<Option<GameSession>>>,
    /// What the launcher UI is showing ("Browsing mods", ...), used for the
    /// idle status.
    page: Arc<Mutex<Option<String>>>,
    /// Bumped on every page change; a delayed update only applies if it's
    /// still the latest, since Discord rate-limits activity updates (5 per
    /// 20 s) and clicking through pages would otherwise blow through that.
    page_generation: Arc<AtomicU64>,
}

fn launcher_started_at() -> i64 {
    static STARTED: OnceLock<i64> = OnceLock::new();
    *STARTED.get_or_init(|| crate::services::cache::store::now_secs() as i64)
}

impl DiscordPresence {
    pub fn new() -> Self {
        launcher_started_at();
        Self {
            client: Arc::new(Mutex::new(None)),
            pending_join: Arc::new(Mutex::new(None)),
            game: Arc::new(Mutex::new(None)),
            page: Arc::new(Mutex::new(None)),
            page_generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Connects (if not already connected) using `client_id` and sets the
    /// given activity. Runs on a blocking thread since the underlying IPC
    /// pipe is synchronous. The returned success flag is informational
    /// only — callers never fail anything based on it, since this is purely
    /// cosmetic (Discord not running, or the user's Activity Privacy
    /// setting being off, are both normal, silent no-ops).
    pub async fn set_playing(&self, client_id: String, activity: PresenceActivity) -> bool {
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            let Ok(mut guard) = this.client.lock() else {
                return false;
            };
            if guard.is_none() {
                let mut new_client = DiscordIpcClient::new(&client_id);
                if new_client.connect().is_ok() {
                    *guard = Some(new_client);
                }
            }
            let Some(client) = guard.as_mut() else {
                return false;
            };

            let large_text = large_text(activity.spooky);
            let payload = build_payload(&activity, &large_text);
            if client.set_activity(payload).is_err() {
                // Connection likely went stale (Discord restarted); drop
                // it so the next call reconnects from scratch.
                *guard = None;
                return false;
            }
            // Discord answers every command; the crate doesn't read it, so
            // unread replies would pile up on the pipe and a rejected
            // activity (bad field, bad image URL) would fail invisibly.
            match client.recv() {
                Ok((_, reply)) if reply.get("evt").and_then(Value::as_str) == Some("ERROR") => {
                    log::warn!("Discord rejected the activity: {}", reply["data"]);
                    false
                }
                Ok(_) => true,
                Err(_) => {
                    *guard = None;
                    false
                }
            }
        })
        .await
        .unwrap_or(false)
    }

    pub async fn clear(&self) {
        let this = self.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let Ok(mut guard) = this.client.lock() else {
                return;
            };
            if let Some(client) = guard.as_mut() {
                if client.clear_activity().is_ok() {
                    let _ = client.recv();
                }
            }
        })
        .await;
    }

    /// Answers whichever "Ask to Join" request is currently pending. A
    /// no-op if nothing is pending (the request already timed out, was
    /// already answered, or none was ever sent).
    pub fn respond_join_request(&self, accept: bool) {
        if let Ok(mut guard) = self.pending_join.lock() {
            if let Some(tx) = guard.take() {
                let _ = tx.send(accept);
            }
        }
    }

    fn in_game(&self) -> bool {
        self.game.lock().map(|g| g.is_some()).unwrap_or(false)
    }

    /// Records a freshly launched game and publishes its presence. A
    /// quick-join launch (`target_server`) starts straight in the
    /// multiplayer phase; otherwise it shows as starting until the game's
    /// log says otherwise.
    pub async fn start_game(
        &self,
        settings: &SettingsStore,
        instance: &MinecraftInstance,
        target_server: Option<&str>,
        player_name: &str,
        head_skin_id: &str,
    ) {
        let session = GameSession {
            instance_id: instance.id.clone(),
            instance_name: instance.name.clone(),
            minecraft_version: instance.minecraft_version.clone(),
            loader: instance.loader,
            started_at: crate::services::cache::store::now_secs() as i64,
            phase: match target_server {
                Some(server) => GamePhase::Multiplayer(server.to_string()),
                None => GamePhase::Loading,
            },
            game_dir: std::path::PathBuf::from(&instance.game_dir),
            player_name: player_name.to_string(),
            player_head_url: player_head_url(head_skin_id),
            world_name: None,
            server_name: None,
            advancement: None,
        };
        if let Ok(mut game) = self.game.lock() {
            *game = Some(session.clone());
        }
        let started_at = session.started_at;
        let instance_id = session.instance_id.clone();
        self.publish_game(settings, session).await;

        // Keeps the server's player count current while this game runs.
        let this = self.clone();
        let settings = settings.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(PLAYER_COUNT_REFRESH).await;
                let current = match this.game.lock() {
                    Ok(game) => game.clone(),
                    Err(_) => return,
                };
                match current {
                    Some(s) if s.instance_id == instance_id && s.started_at == started_at => {
                        if matches!(&s.phase, GamePhase::Multiplayer(a) if !is_private_address(a)) {
                            this.publish_game(&settings, s).await;
                        }
                    }
                    _ => return, // that game ended
                }
            }
        });
    }

    /// Republishes the running game's status (after a setting changed).
    pub async fn refresh_game(&self, settings: &SettingsStore) {
        let current = self.game.lock().ok().and_then(|g| g.clone());
        if let Some(session) = current {
            self.publish_game(settings, session).await;
        }
    }

    /// Feeds one line of game output through [`parse_game_log`] and, on a
    /// phase change, republishes presence. Cheap enough to call for every
    /// line: the common case is a few substring checks.
    pub fn on_game_log(&self, settings: &SettingsStore, instance_id: &str, line: &str) {
        let updated = {
            let Ok(mut game) = self.game.lock() else {
                return;
            };
            let Some(session) = game.as_mut() else { return };
            if session.instance_id != instance_id {
                return;
            }
            if let Some(advancement) = parse_advancement(line, &session.player_name) {
                session.advancement = Some(advancement.clone());
                let this = self.clone();
                let settings = settings.clone();
                let instance_id = instance_id.to_string();
                // Take it back out of the status after a while, unless a
                // newer one replaced it.
                tokio::spawn(async move {
                    tokio::time::sleep(ADVANCEMENT_SHOWN_FOR).await;
                    let cleared = {
                        let Ok(mut game) = this.game.lock() else {
                            return;
                        };
                        match game.as_mut() {
                            Some(s)
                                if s.instance_id == instance_id
                                    && s.advancement.as_deref() == Some(&advancement) =>
                            {
                                s.advancement = None;
                                s.clone()
                            }
                            _ => return,
                        }
                    };
                    this.publish_game(&settings, cleared).await;
                });
                session.clone()
            } else {
                match parse_game_log(line, &session.phase) {
                    Some(phase) if phase != session.phase => {
                        session.phase = phase;
                        session.world_name = None;
                        session.server_name = None;
                        session.clone()
                    }
                    _ => return,
                }
            }
        };
        let this = self.clone();
        let settings = settings.clone();
        tokio::spawn(async move { this.publish_game(&settings, updated).await });
    }

    /// Forgets the game if `instance_id` is the one being shown (another
    /// instance may have been launched since) and falls back to idle.
    pub async fn end_game(&self, settings: &SettingsStore, instance_id: &str) {
        if let Ok(mut game) = self.game.lock() {
            if game.as_ref().is_some_and(|g| g.instance_id == instance_id) {
                *game = None;
            }
        }
        set_idle(self, settings).await;
    }

    /// Remembers which launcher page is open and, after a short settle
    /// delay, shows it as the idle status (unless a game is running).
    pub fn set_page(&self, settings: &SettingsStore, page: Option<String>) {
        if let Ok(mut current) = self.page.lock() {
            if *current == page {
                return;
            }
            *current = page;
        }
        let generation = self.page_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let this = self.clone();
        let settings = settings.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            if this.page_generation.load(Ordering::SeqCst) == generation {
                set_idle(&this, &settings).await;
            }
        });
    }

    async fn publish_game(&self, settings: &SettingsStore, session: GameSession) {
        let current = settings.get().await;
        if !current.discord_rpc_enabled {
            return;
        }
        let spooky = spooky_season(&current);
        let Some(client_id) = resolve_client_id(current.discord_client_id) else {
            return;
        };
        let session = self.resolve_names(session).await;
        let activity = build_game_activity(&session, current.discord_join_mode, spooky).await;
        // The game may have exited (or moved on) while the server ping ran.
        let still_current = self
            .game
            .lock()
            .map(|g| {
                g.as_ref().is_some_and(|g| {
                    g.instance_id == session.instance_id
                        && g.phase == session.phase
                        && g.advancement == session.advancement
                })
            })
            .unwrap_or(false);
        if still_current {
            self.set_playing(client_id, activity).await;
        }
    }
}

impl DiscordPresence {
    /// Fills in the world/server name for the current phase (once per
    /// world or server) and remembers it on the live session.
    async fn resolve_names(&self, mut session: GameSession) -> GameSession {
        match &session.phase {
            GamePhase::Singleplayer if session.world_name.is_none() => {
                // The line announcing singleplayer comes just before the
                // world is opened, so give the game a moment.
                let since = session.started_at.max(0) as u64;
                for _ in 0..8 {
                    let dir = session.game_dir.clone();
                    let found = tokio::task::spawn_blocking(move || {
                        crate::services::instance::worlds::world_being_played(&dir, since)
                    })
                    .await
                    .ok()
                    .flatten();
                    if found.is_some() {
                        session.world_name = found;
                        break;
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
            GamePhase::Multiplayer(address)
                if session.server_name.is_none() && !is_private_address(address) =>
            {
                session.server_name = crate::services::instance::servers::saved_server_name(
                    &session.game_dir,
                    address,
                )
                .await;
            }
            _ => return session,
        }
        if let Ok(mut game) = self.game.lock() {
            if let Some(live) = game.as_mut() {
                if live.instance_id == session.instance_id && live.phase == session.phase {
                    live.world_name.clone_from(&session.world_name);
                    live.server_name.clone_from(&session.server_name);
                }
            }
        }
        session
    }
}

impl Default for DiscordPresence {
    fn default() -> Self {
        Self::new()
    }
}

fn large_text(spooky: bool) -> String {
    if spooky {
        format!(
            "Arcane Launcher v{} · Spooky season",
            env!("CARGO_PKG_VERSION")
        )
    } else {
        format!("Arcane Launcher v{}", env!("CARGO_PKG_VERSION"))
    }
}

/// Turns a [`PresenceActivity`] into the IPC payload. Every activity carries
/// the Arcane logo linking to the website and a "Get Arcane Launcher"
/// button; on a public server a "Join server" button comes first. (Discord
/// allows buttons or an Ask-to-Join secret, not both — buttons were chosen
/// since anyone can use them, not just friends.)
fn build_payload<'a>(
    activity: &'a PresenceActivity,
    large_text: &'a str,
) -> activity::Activity<'a> {
    let mut assets = activity::Assets::new()
        .large_image(if activity.spooky {
            PUMPKIN_URL
        } else {
            LOGO_URL
        })
        .large_text(large_text)
        .large_url(WEBSITE_URL);
    if let Some(small_image) = &activity.small_image {
        assets = assets.small_image(small_image.as_str());
    }
    if let Some(small_text) = &activity.small_text {
        assets = assets.small_text(small_text.as_str());
    }

    let mut payload = activity::Activity::new()
        .activity_type(activity::ActivityType::Playing)
        .details(activity.details.as_str())
        .state(activity.state.as_str())
        .timestamps(activity::Timestamps::new().start(activity.started_at))
        .assets(assets);

    if let Some(party) = &activity.party {
        payload = payload.party(
            activity::Party::new()
                .id(party.id.as_str())
                .size([party.size, party.max]),
        );
    }
    if let Some(secret) = &activity.join_secret {
        return payload.secrets(activity::Secrets::new().join(secret.as_str()));
    }
    let mut buttons = Vec::new();
    if let Some(join_url) = &activity.join_url {
        buttons.push(activity::Button::new("Join server", join_url.as_str()));
    }
    buttons.push(activity::Button::new("Get Arcane Launcher", WEBSITE_URL));
    payload.buttons(buttons)
}

/// "My Pack · Fabric 26.2", but just "Fabric 26.2" when the instance is
/// named after its version (the default name) — never the same text twice.
fn instance_and_version(instance_name: &str, version_line: &str) -> String {
    let name = instance_name.trim();
    let (lower_name, lower_version) = (name.to_lowercase(), version_line.to_lowercase());
    if name.is_empty() || lower_version.contains(&lower_name) {
        version_line.to_string()
    } else if lower_name.contains(&lower_version) {
        name.to_string()
    } else {
        format!("{name} · {version_line}")
    }
}

async fn build_game_activity(
    session: &GameSession,
    join_mode: DiscordJoinMode,
    spooky: bool,
) -> PresenceActivity {
    let version_line = if session.loader == ModLoaderKind::Vanilla {
        format!("Minecraft {}", session.minecraft_version)
    } else {
        format!(
            "{} {}",
            loader_label(session.loader),
            session.minecraft_version
        )
    };

    let mut party = None;
    let mut join_url = None;
    let mut join_secret = None;
    // On a public server the second line is its address (Discord appends
    // the party's "(12 of 100)" player count to it).
    let mut server_line = None;
    let details = match &session.phase {
        GamePhase::Loading if spooky => " Rising from the grave".to_string(),
        GamePhase::Loading => "Starting the game".to_string(),
        GamePhase::MainMenu if spooky => " Lurking in the main menu".to_string(),
        GamePhase::MainMenu => "In the main menu".to_string(),
        GamePhase::Singleplayer => match (&session.world_name, spooky) {
            (Some(world), true) => format!(" Haunting {world}"),
            (None, true) => " Haunting singleplayer".to_string(),
            (Some(world), false) => format!("Singleplayer: {world}"),
            (None, false) => "Playing singleplayer".to_string(),
        },
        GamePhase::Multiplayer(address) if is_private_address(address) => if spooky {
            " Haunting a private server"
        } else {
            "Playing on a private server"
        }
        .to_string(),
        GamePhase::Multiplayer(address) => {
            let host = address.split(':').next().unwrap_or(address);
            let ping = crate::services::instance::server_ping::ping_server(address).await;
            let party_id = format!("arcane:{address}");
            party = match (ping.players_online, ping.players_max) {
                (Some(online), Some(max)) => Some(PresenceParty {
                    id: party_id,
                    // Discord needs size >= 1 and max >= size; counts above
                    // i32 never happen, but clamp rather than wrap.
                    size: (online.max(1)).min(i32::MAX as u32) as i32,
                    max: (max.max(online).max(1)).min(i32::MAX as u32) as i32,
                }),
                // Ask to Join needs a party even when the ping failed.
                _ if join_mode == DiscordJoinMode::Ask => Some(PresenceParty {
                    id: party_id,
                    size: 1,
                    max: 1,
                }),
                _ => None,
            };
            server_line = Some(address.clone());
            match join_mode {
                DiscordJoinMode::Ask => {
                    join_secret = Some(encode_join_secret(
                        address,
                        &session.minecraft_version,
                        session.loader.id(),
                    ))
                }
                DiscordJoinMode::Button => {
                    join_url = Some(join_page_url(
                        address,
                        &session.minecraft_version,
                        session.loader.id(),
                    ))
                }
            }
            let server = if host.eq_ignore_ascii_case("mc.arcaneworld.lk") {
                "Arcane World"
            } else {
                session.server_name.as_deref().unwrap_or(host)
            };
            match (spooky, server == "Arcane World") {
                (true, _) => format!(" Haunting {server}"),
                (false, true) => "Playing Arcane World".to_string(),
                (false, false) => format!("Playing on {server}"),
            }
        }
    };

    let state = match &session.advancement {
        Some(advancement) => format!("🏆 Just got: {advancement}"),
        None => match server_line {
            Some(address) => address,
            None => instance_and_version(&session.instance_name, &version_line),
        },
    };

    PresenceActivity {
        details: details.chars().take(128).collect(),
        state: state.chars().take(128).collect(),
        started_at: session.started_at,
        small_image: Some(session.player_head_url.clone()),
        small_text: Some(session.player_name.clone()).filter(|n| !n.is_empty()),
        party,
        join_url,
        join_secret,
        spooky,
    }
}

/// Shows the "in the launcher" status (with the open page, when the UI has
/// reported one) whenever the app is open but no game is running — set at
/// startup, on page changes, and whenever a game exits. No-ops entirely if
/// the feature is turned off in Settings.
pub async fn set_idle(discord: &DiscordPresence, settings: &SettingsStore) {
    let settings = settings.get().await;
    if !settings.discord_rpc_enabled || discord.in_game() {
        return;
    }
    let spooky = spooky_season(&settings);
    let Some(client_id) = resolve_client_id(settings.discord_client_id) else {
        return;
    };
    let page = discord.page.lock().ok().and_then(|p| p.clone());
    discord
        .set_playing(
            client_id,
            PresenceActivity {
                details: if spooky {
                    " Brewing in the launcher"
                } else {
                    "In the launcher"
                }
                .to_string(),
                state: page.unwrap_or_else(|| "Getting ready to play".to_string()),
                started_at: launcher_started_at(),
                spooky,
                ..Default::default()
            },
        )
        .await;
}

fn send_command(
    client: &mut DiscordIpcClient,
    cmd: &str,
    args: Value,
) -> Result<(), discord_rich_presence::error::Error> {
    client.send(
        json!({ "cmd": cmd, "args": args, "nonce": Uuid::new_v4().to_string() }),
        1,
    )
}

fn subscribe(
    client: &mut DiscordIpcClient,
    evt: &str,
) -> Result<(), discord_rich_presence::error::Error> {
    client.send(
        json!({ "cmd": "SUBSCRIBE", "evt": evt, "nonce": Uuid::new_v4().to_string() }),
        1,
    )?;
    // Discord echoes every command back as an immediate reply; consume it
    // here so the main loop's next `recv()` sees real events, not this ack.
    client.recv()?;
    Ok(())
}

/// Handles one `DISPATCH` frame from the listener connection. Anything that
/// isn't a `DISPATCH` (i.e. the echoed ack for a command we just sent, such
/// as our own SUBSCRIBE or SEND_ACTIVITY_JOIN_INVITE) is silently ignored —
/// see the note in `subscribe` about Discord's lock-step echo behavior.
fn handle_frame(
    client: &mut DiscordIpcClient,
    payload: &Value,
    sink: &Arc<dyn EventSink>,
    pending_join: &Arc<Mutex<Option<std_mpsc::Sender<bool>>>>,
) {
    if payload.get("cmd").and_then(Value::as_str) != Some("DISPATCH") {
        return;
    }
    match payload.get("evt").and_then(Value::as_str) {
        Some("ACTIVITY_JOIN_REQUEST") => {
            let user = &payload["data"]["user"];
            let Some(user_id) = user.get("id").and_then(Value::as_str).map(str::to_string) else {
                return;
            };
            let username = user
                .get("username")
                .and_then(Value::as_str)
                .unwrap_or("A friend")
                .to_string();
            let avatar = user
                .get("avatar")
                .and_then(Value::as_str)
                .map(str::to_string);

            let (tx, rx) = std_mpsc::channel::<bool>();
            match pending_join.lock() {
                Ok(mut guard) => *guard = Some(tx),
                Err(_) => return,
            }

            sink.emit(
                Event::raw(
                    EventName::DiscordJoinRequest,
                    DiscordJoinRequestEvent {
                        user_id: user_id.clone(),
                        username,
                        avatar,
                    },
                )
                .expect("DiscordJoinRequestEvent serializes"),
            );

            let accepted = rx.recv_timeout(JOIN_REQUEST_TIMEOUT).unwrap_or(false);
            if let Ok(mut guard) = pending_join.lock() {
                *guard = None;
            }
            sink.emit(
                Event::raw(
                    EventName::DiscordJoinRequestResolved,
                    DiscordJoinResolvedEvent { accepted },
                )
                .expect("DiscordJoinResolvedEvent serializes"),
            );

            let result = if accepted {
                send_command(
                    client,
                    "SEND_ACTIVITY_JOIN_INVITE",
                    json!({ "user_id": user_id }),
                )
            } else {
                send_command(
                    client,
                    "CLOSE_ACTIVITY_REQUEST",
                    json!({ "user_id": user_id }),
                )
            };
            let _ = result;
        }
        Some("ACTIVITY_JOIN") => {
            if let Some(secret) = payload["data"]["secret"].as_str() {
                if let Some(decoded) = decode_join_secret(secret) {
                    sink.emit(
                        Event::raw(EventName::DiscordJoin, decoded)
                            .expect("DiscordJoinEvent serializes"),
                    );
                }
            }
        }
        _ => {}
    }
}

/// Runs for the entire lifetime of the app on a dedicated blocking thread,
/// maintaining a second IPC connection (separate from the one `set_playing`
/// uses) whose only job is to sit in a blocking `recv()` loop waiting for
/// "Ask to Join" activity. A single connection can't do both, since
/// `recv()` has no timeout and would otherwise starve any concurrent
/// `set_activity` call of the same lock for however long it takes a friend
/// to send a request.
///
/// Reconnects with backoff on any error (Discord not running is the normal
/// case while developing/testing, not an error worth logging loudly). A
/// settings change (RPC turned off, client ID edited) only takes effect the
/// next time this loop reconnects, not instantly — acceptable for a
/// cosmetic feature that already treats every failure as a silent no-op.
pub fn spawn_join_listener(
    discord: DiscordPresence,
    sink: Arc<dyn EventSink>,
    settings: SettingsStore,
) {
    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Handle::current();
        loop {
            let current = rt.block_on(settings.get());
            if !current.discord_rpc_enabled {
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
            let Some(client_id) = resolve_client_id(current.discord_client_id) else {
                std::thread::sleep(Duration::from_secs(60));
                continue;
            };

            let mut client = DiscordIpcClient::new(&client_id);
            if client.connect().is_err() {
                std::thread::sleep(Duration::from_secs(15));
                continue;
            }
            if subscribe(&mut client, "ACTIVITY_JOIN_REQUEST").is_err()
                || subscribe(&mut client, "ACTIVITY_JOIN").is_err()
            {
                std::thread::sleep(Duration::from_secs(15));
                continue;
            }

            while let Ok((_, payload)) = client.recv() {
                handle_frame(&mut client, &payload, &sink, &discord.pending_join);
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn join_secret_round_trips() {
        let secret = super::encode_join_secret("play.example.org:25577", "1.21.1", "fabric");
        let decoded = super::decode_join_secret(&secret).unwrap();
        assert_eq!(decoded.server, "play.example.org:25577");
        assert_eq!(decoded.minecraft_version, "1.21.1");
        assert_eq!(decoded.loader, "fabric");
        assert!(!decoded.via_link);
        // Discord caps secrets at 128 characters.
        assert!(super::encode_join_secret(&"a".repeat(40), "1.21.10", "neoforge").len() <= 128);
    }

    #[test]
    fn state_line_never_repeats_the_version() {
        use super::instance_and_version as f;
        assert_eq!(f("Fabric 26.2", "Fabric 26.2"), "Fabric 26.2");
        assert_eq!(f("fabric 26.2", "Fabric 26.2"), "Fabric 26.2");
        assert_eq!(f("26.2", "Fabric 26.2"), "Fabric 26.2");
        assert_eq!(f("Fabric 26.2 (2)", "Fabric 26.2"), "Fabric 26.2 (2)");
        assert_eq!(f("My Pack", "Fabric 26.2"), "My Pack · Fabric 26.2");
    }

    use super::{is_private_address, join_page_url, parse_advancement, parse_game_log, GamePhase};

    #[test]
    fn spots_own_advancements_only() {
        let line = "[12:00:00] [Render thread/INFO]: [System] [CHAT] MaN_EkA has made the advancement [Diamonds!]";
        assert_eq!(
            parse_advancement(line, "MaN_EkA").as_deref(),
            Some("Diamonds!")
        );
        assert_eq!(
            parse_advancement(line, "man_eka").as_deref(),
            Some("Diamonds!")
        );
        assert_eq!(parse_advancement(line, "EkA"), None);
        assert_eq!(
            parse_advancement(
                "[CHAT] [VIP] Steve has completed the challenge [Monsters Hunted]",
                "Steve"
            )
            .as_deref(),
            Some("Monsters Hunted")
        );
        assert_eq!(
            parse_advancement(
                "[CHAT] Alex has reached the goal [Sky's the Limit]",
                "Steve"
            ),
            None
        );
        assert_eq!(
            parse_advancement("[CHAT] <Steve> has made the advancement [fake]", "Steve"),
            None
        );
        assert_eq!(
            parse_advancement("Steve has made the advancement [Diamonds!]", "Steve"),
            None
        );
    }

    #[test]
    fn join_page_url_is_encoded() {
        assert_eq!(
            join_page_url("play.example.org:25577", "1.21.1", "fabric"),
            "https://launcher.arcaneworld.lk/join?server=play.example.org%3A25577&mc=1.21.1&loader=fabric"
        );
    }

    #[test]
    fn detects_phases_from_real_log_lines() {
        assert_eq!(
            parse_game_log(
                "[12:00:01] [Render thread/INFO]: Sound engine started",
                &GamePhase::Loading
            ),
            Some(GamePhase::MainMenu)
        );
        // A resource reload in-world must not flip back to "main menu".
        assert_eq!(
            parse_game_log(
                "[Render thread/INFO]: Sound engine started",
                &GamePhase::Singleplayer
            ),
            None
        );
        assert_eq!(
            parse_game_log(
                "[Server thread/INFO]: Starting integrated minecraft server version 1.21.4",
                &GamePhase::MainMenu
            ),
            Some(GamePhase::Singleplayer)
        );
        assert_eq!(
            parse_game_log(
                "[Server thread/INFO]: Stopping singleplayer server as player logged out",
                &GamePhase::Singleplayer
            ),
            Some(GamePhase::MainMenu)
        );
        assert_eq!(
            parse_game_log(
                "[Render thread/INFO]: Connecting to mc.arcaneworld.lk, 25565",
                &GamePhase::MainMenu
            ),
            Some(GamePhase::Multiplayer("mc.arcaneworld.lk".into()))
        );
        assert_eq!(
            parse_game_log(
                "[Render thread/INFO]: Connecting to play.example.org, 25577",
                &GamePhase::MainMenu
            ),
            Some(GamePhase::Multiplayer("play.example.org:25577".into()))
        );
        assert_eq!(
            parse_game_log(
                "[Render thread/INFO]: Loaded 1234 recipes",
                &GamePhase::MainMenu
            ),
            None
        );
    }

    #[test]
    fn month_from_unix_days_matches_known_dates() {
        use super::month_from_unix_days;
        assert_eq!(month_from_unix_days(0), 1); // 1970-01-01
        assert_eq!(month_from_unix_days(20_727), 10); // 2026-10-01
        assert_eq!(month_from_unix_days(20_757), 10); // 2026-10-31
        assert_eq!(month_from_unix_days(20_758), 11); // 2026-11-01
        assert_eq!(month_from_unix_days(20_726), 9); // 2026-09-30
        assert_eq!(month_from_unix_days(19_782), 2); // 2024-02-29
    }

    #[test]
    fn private_addresses_are_hidden() {
        assert!(is_private_address("192.168.1.20"));
        assert!(is_private_address("10.0.0.5:25566"));
        assert!(is_private_address("localhost"));
        assert!(is_private_address("127.0.0.1:25565"));
        assert!(!is_private_address("mc.arcaneworld.lk"));
        assert!(!is_private_address("8.8.8.8:25565"));
    }

    /// Sends real activities to the Discord client running on this machine
    /// and asserts Discord accepts each one (images, buttons, party). Needs Discord open: `cargo test -- --ignored live_discord`.
    #[test]
    #[ignore]
    fn live_discord_accepts_every_activity_shape() {
        use super::*;
        let client_id =
            resolve_client_id(None).expect("no Discord client id baked into this build");
        let mut client = DiscordIpcClient::new(&client_id);
        client.connect().expect("Discord isn't running");
        let now = crate::services::cache::store::now_secs() as i64;
        let shapes = [
            PresenceActivity {
                details: "In the launcher".into(),
                state: "Browsing shaders".into(),
                started_at: now,
                ..Default::default()
            },
            PresenceActivity {
                details: "Singleplayer: My Survival World".into(),
                state: "🏆 Just got: Diamonds!".into(),
                started_at: now,
                small_image: Some(player_head_url("069a79f4-44e9-4726-a5be-fca90e38aaf5")),
                small_text: Some("Notch".into()),
                ..Default::default()
            },
            PresenceActivity {
                details: "Playing Arcane World".into(),
                state: "mc.arcaneworld.lk".into(),
                started_at: now,
                small_image: Some(player_head_url(DEFAULT_HEAD)),
                small_text: Some("Steve".into()),
                party: Some(PresenceParty {
                    id: "arcane:mc.arcaneworld.lk".into(),
                    size: 12,
                    max: 100,
                }),
                join_url: Some(join_page_url("mc.arcaneworld.lk", "1.21.1", "neoforge")),
                join_secret: None,
                spooky: false,
            },
            // Ask to Join: secret + party, no buttons.
            PresenceActivity {
                details: "Playing Arcane World".into(),
                state: "mc.arcaneworld.lk".into(),
                started_at: now,
                small_image: Some(player_head_url(DEFAULT_HEAD)),
                small_text: Some("Steve".into()),
                party: Some(PresenceParty {
                    id: "arcane:mc.arcaneworld.lk".into(),
                    size: 12,
                    max: 100,
                }),
                join_url: None,
                join_secret: Some(encode_join_secret(
                    "mc.arcaneworld.lk",
                    "1.21.1",
                    "neoforge",
                )),
                spooky: false,
            },
        ];
        let spooky_shapes = shapes.iter().cloned().map(|mut s| {
            s.spooky = true;
            s.details = format!(" {}", s.details.replace("Playing ", "Haunting "));
            s
        });
        let shapes: Vec<PresenceActivity> = shapes.iter().cloned().chain(spooky_shapes).collect();
        for shape in &shapes {
            let text = large_text(shape.spooky);
            client
                .set_activity(build_payload(shape, &text))
                .expect("send failed");
            let (_, reply) = client.recv().expect("no reply from Discord");
            println!("{} -> {}", shape.details, reply);
            assert_ne!(
                reply["evt"], "ERROR",
                "Discord rejected '{}': {}",
                shape.details, reply["data"]
            );
            std::thread::sleep(Duration::from_secs(4));
        }
        client.clear_activity().ok();
        client.recv().ok();
    }
}
