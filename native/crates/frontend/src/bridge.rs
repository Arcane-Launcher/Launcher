//! The backend driver — owns the tokio runtime the domain crate was
//! written against and bridges it into gpui.
//!
//! `AppState::initialize` runs on a dedicated tokio thread (the services
//! spawn tokio tasks — probe loops, download workers); every command
//! afterwards is spawned on that runtime's handle and awaited from gpui
//! tasks, which works because tokio's `oneshot`/`JoinHandle` futures are
//! waker-driven and executor-agnostic. Backend events (`auth://progress`
//! etc.) flow through an `EventSink` that feeds a smol channel drained on
//! the main thread into the `AuthStore` global.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{App, BorrowAppContext, Global};
use tokio::runtime::{Handle, Runtime};

use backend::{
    commands,
    events::{Event, EventName, EventSink},
    models::download::{DownloadStatus, DownloadTask},
    models::minecraft::AuthSession,
    services::minecraft::running::RunningInfo,
    services::recorder::DisabledHotkeys,
    state::AppState,
};

/// gpui-side view of the auth state — `useSession`/`useAccounts` queries +
/// the `auth://progress` listener from the React app, collapsed into one
/// global the shell and login views observe.
#[derive(Clone)]
pub struct AuthStore {
    /// `app-shell.tsx`'s gate: `session === undefined` → Unknown, `null` →
    /// LoggedOut, a session → LoggedIn.
    pub gate: Gate,
    /// Latest `auth://progress` message (None clears the busy line).
    pub progress: Option<String>,
    /// The full saved-account list (`list_accounts`), refreshed on
    /// login/logout/switch/remove.
    pub accounts: Vec<AuthSession>,
}

#[derive(Clone)]
pub enum Gate {
    Unknown,
    LoggedOut,
    LoggedIn(AuthSession),
}

impl Global for AuthStore {}

impl AuthStore {
    pub fn session(&self) -> Option<&AuthSession> {
        match &self.gate {
            Gate::LoggedIn(session) => Some(session),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Launch-slice stores — the `useRunningInstances` / `useDownloads` hooks and
// the console's event subscription, lifted into globals drained by the event
// pump below (zustand state + `listen()` collapsed into one place).
// ---------------------------------------------------------------------------

/// `use-running-instances.ts`: which instances have a live game process,
/// kept in sync via `launch://started` / `launch://exit` plus an initial
/// `list_running` fetch.
#[derive(Default)]
pub struct RunningStore {
    map: HashMap<String, RunningInfo>,
}

impl Global for RunningStore {}

impl RunningStore {
    pub fn get(&self, instance_id: &str) -> Option<&RunningInfo> {
        self.map.get(instance_id)
    }

    pub fn is_running(&self, instance_id: &str) -> bool {
        self.map.contains_key(instance_id)
    }
}

/// `use-downloads.ts`: every download task the backend reports, keyed by
/// task id. Completed/failed/cancelled tasks stay in the map with their
/// final status (exactly like the React hook); the Play page's banner bar
/// shows the first *active* one (sorted by name, like the hook's list).
#[derive(Default)]
pub struct DownloadsStore {
    map: HashMap<String, DownloadTask>,
}

impl Global for DownloadsStore {}

impl DownloadsStore {
    /// The active tasks (`downloading` / `paused` / `queued`), sorted by
    /// name — `useDownloads`'s `active` array.
    pub fn active(&self) -> Vec<DownloadTask> {
        let mut list: Vec<DownloadTask> = self
            .map
            .values()
            .filter(|t| {
                matches!(
                    t.status,
                    DownloadStatus::Downloading | DownloadStatus::Paused | DownloadStatus::Queued
                )
            })
            .cloned()
            .collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        list
    }
}

/// The instance console's line buffer, fed by `launch://log`. Lines are
/// classified once on arrival (tone) and batched: the drain pushes into
/// `pending` and a 100ms tick moves batches into `lines` —
/// instance-console.tsx's `MAX_CONSOLE_LINES`/`FLUSH_MS` mechanics, one
/// buffer shared by every console view instead of per-component state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogTone {
    /// stderr or an error/exception line (`text-danger/90`).
    Danger,
    /// a warn line (`text-warning/90`).
    Warning,
    /// everything else (`text-neutral-300`).
    Neutral,
}

#[derive(Clone, Debug)]
pub struct ConsoleLine {
    pub id: u64,
    pub line: String,
    pub tone: LogTone,
}

/// Same cap as the backend's own buffer (`MAX_LOG_LINES` in running.rs).
pub const MAX_CONSOLE_LINES: usize = 5000;

#[derive(Default)]
pub struct LogStore {
    lines: HashMap<String, Vec<ConsoleLine>>,
    pending: HashMap<String, Vec<ConsoleLine>>,
    /// Bumped on every `launch://started` for an instance — the backend
    /// replaces its buffer on a new session, so open consoles reset.
    generation: HashMap<String, u64>,
    next_line_id: u64,
}

impl Global for LogStore {}

impl LogStore {
    /// instance-console.tsx's `toConsoleLine` — classify on arrival.
    pub fn classify(line: &str, stream: &str) -> LogTone {
        let lower = line.to_lowercase();
        if stream == "stderr" || lower.contains("error") || lower.contains("exception") {
            LogTone::Danger
        } else if lower.contains("warn") {
            LogTone::Warning
        } else {
            LogTone::Neutral
        }
    }

    fn push_pending(&mut self, instance_id: &str, line: String, stream: &str) {
        let id = self.next_line_id;
        self.next_line_id += 1;
        let tone = Self::classify(&line, stream);
        self.pending
            .entry(instance_id.to_string())
            .or_default()
            .push(ConsoleLine { id, line, tone });
    }

    /// Moves pending batches into the buffers. Returns whether anything
    /// changed (observers are only notified then).
    fn flush(&mut self) -> bool {
        if self.pending.is_empty() {
            return false;
        }
        for (instance_id, batch) in self.pending.drain() {
            let lines = self.lines.entry(instance_id).or_default();
            lines.extend(batch);
            let overflow = lines.len().saturating_sub(MAX_CONSOLE_LINES);
            lines.drain(0..overflow);
        }
        true
    }

    /// `launch://started` — the backend dropped this instance's buffer.
    fn start(&mut self, instance_id: &str) {
        self.lines.remove(instance_id);
        self.pending.remove(instance_id);
        *self.generation.entry(instance_id.to_string()).or_default() += 1;
    }

    pub fn lines_for(&self, instance_id: &str) -> &[ConsoleLine] {
        self.lines
            .get(instance_id)
            .map(|lines| lines.as_slice())
            .unwrap_or(&[])
    }

    pub fn generation(&self, instance_id: &str) -> u64 {
        self.generation.get(instance_id).copied().unwrap_or(0)
    }
}

/// Event payloads as they serialize on the backend side — the drain reads
/// them straight out of the JSON envelope, mirroring the shapes the React
/// `listen()` callbacks destructured.
#[derive(serde::Deserialize)]
struct LaunchStartedPayload {
    instance_id: String,
    #[allow(dead_code)]
    started_at: u64,
}

#[derive(serde::Deserialize)]
struct LaunchExitPayload {
    instance_id: String,
}

#[derive(serde::Deserialize)]
struct LaunchLogPayload {
    instance_id: String,
    line: String,
    stream: String,
}

#[derive(serde::Deserialize)]
struct DownloadProgressPayload {
    task_id: String,
    name: String,
    downloaded_bytes: u64,
    total_bytes: u64,
    speed_bps: u64,
    status: DownloadStatus,
    error: Option<String>,
}

/// The `EventSink` handed to the backend: pushes events onto a channel the
/// main thread drains (services emit from arbitrary tokio threads).
struct BridgeSink {
    tx: smol::channel::Sender<Event>,
}

impl EventSink for BridgeSink {
    fn emit(&self, event: Event) {
        // Unbounded channel — never blocks the emitting service.
        let _ = self.tx.try_send(event);
    }
}

pub struct Backend {
    pub state: AppState,
    handle: Handle,
    /// Kept alive so the runtime never shuts down (a `Runtime` dropped
    /// while tasks are parked would panic on block-on paths).
    _runtime: Arc<Runtime>,
}

impl Global for Backend {}

impl Backend {
    /// Spawns the runtime + `AppState::initialize` on a background thread
    /// and registers the `Backend` global once it resolves, then runs
    /// `get_session` to flip the gate. `on_ready` runs on the main thread
    /// after the store exists (used to wire the first gate query).
    pub fn initialize(cx: &mut App) {
        // Every store exists before the backend thread finishes — views
        // render from them from the first frame (gate Unknown = blank).
        // `update_global` panics on a missing global, so all of them are
        // initialized here (the ToastStore panic that taught this lives in
        // main.rs for historical reasons).
        cx.set_global(AuthStore {
            gate: Gate::Unknown,
            progress: None,
            accounts: Vec::new(),
        });
        cx.set_global(RunningStore::default());
        cx.set_global(DownloadsStore::default());
        cx.set_global(LogStore::default());
        cx.set_global(crate::modals::ModalStore::default());

        let (tx, rx) = smol::channel::unbounded::<Event>();
        let sink: Arc<dyn EventSink> = Arc::new(BridgeSink { tx });

        let (state_tx, state_rx) = tokio::sync::oneshot::channel::<(AppState, Runtime)>();

        std::thread::Builder::new()
            .name("arcane-backend".into())
            .spawn(move || {
                let runtime = Runtime::new().expect("tokio runtime");
                let state = runtime
                    .block_on(AppState::initialize(sink, Arc::new(DisabledHotkeys)))
                    .expect("backend initialization");
                let _ = state_tx.send((state, runtime));
            })
            .expect("spawn arcane-backend thread");

        // Drain the event channel on the main thread, routing the kinds
        // the UI consumes. This is the gpui home of every `listen()` the
        // React app had (the rest of the kinds still land with their
        // screens).
        let drain = cx.spawn(async move |cx| {
            while let Ok(event) = rx.recv().await {
                match event.name {
                    EventName::AuthProgress => {
                        // `AuthProgress` only derives `Serialize` — read
                        // the message field straight off the payload.
                        if let Some(message) = event
                            .payload
                            .0
                            .get("message")
                            .and_then(|value| value.as_str())
                        {
                            let message = message.to_string();
                            cx.update_global(|store: &mut AuthStore, _| {
                                store.progress = Some(message);
                            })
                            .ok();
                        }
                    }
                    EventName::LaunchStarted => {
                        let Ok(payload) =
                            serde_json::from_value::<LaunchStartedPayload>(event.payload.0.clone())
                        else {
                            continue;
                        };
                        // use-running-instances.ts: started_at, pid unknown
                        // until the process reports it.
                        let instance_id = payload.instance_id.clone();
                        cx.update_global(|store: &mut RunningStore, _| {
                            store.map.insert(
                                instance_id.clone(),
                                RunningInfo {
                                    instance_id,
                                    started_at: payload.started_at,
                                    pid: None,
                                },
                            );
                        })
                        .ok();
                        // The backend replaced this session's log buffer.
                        cx.update_global(|store: &mut LogStore, _| {
                            store.start(&payload.instance_id);
                        })
                        .ok();
                    }
                    EventName::LaunchLog => {
                        let Ok(payload) =
                            serde_json::from_value::<LaunchLogPayload>(event.payload.0.clone())
                        else {
                            continue;
                        };
                        let instance_id = payload.instance_id.clone();
                        cx.update_global(|store: &mut LogStore, _| {
                            store.push_pending(&instance_id, payload.line, &payload.stream);
                        })
                        .ok();
                    }
                    EventName::LaunchExit => {
                        let Ok(payload) =
                            serde_json::from_value::<LaunchExitPayload>(event.payload.0.clone())
                        else {
                            continue;
                        };
                        let instance_id = payload.instance_id.clone();
                        // crash-listener.tsx reads the exit payload; its
                        // dialog lands later (documented deferral) — the
                        // console already sees everything it needs.
                        cx.update_global(|store: &mut RunningStore, _| {
                            store.map.remove(&instance_id);
                        })
                        .ok();
                    }
                    EventName::DownloadProgress => {
                        let Ok(payload) =
                            serde_json::from_value::<DownloadProgressPayload>(event.payload.0)
                        else {
                            continue;
                        };
                        cx.update_global(|store: &mut DownloadsStore, _| {
                            // use-downloads.ts: the instance/url context
                            // survives progress updates.
                            let previous = store.map.get(&payload.task_id);
                            store.map.insert(
                                payload.task_id.clone(),
                                DownloadTask {
                                    id: payload.task_id.clone(),
                                    name: payload.name,
                                    instance_id: previous.and_then(|t| t.instance_id.clone()),
                                    url: previous.map(|t| t.url.clone()).unwrap_or_default(),
                                    downloaded_bytes: payload.downloaded_bytes,
                                    total_bytes: payload.total_bytes,
                                    speed_bps: payload.speed_bps,
                                    status: payload.status,
                                    error: payload.error,
                                },
                            );
                        })
                        .ok();
                    }
                    _ => {
                        // network/recorder/updater/discord events land with
                        // their screens; they're safe to ignore until then.
                    }
                }
            }
        });
        drain.detach();

        // The console's flush timer — collect `launch://log` bursts and
        // apply them at 100ms so a modded game's hundreds of lines don't
        // re-render once per line (instance-console.tsx FLUSH_MS).
        let flush = cx.spawn(async move |cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(100))
                    .await;
                cx.update_global(|store: &mut LogStore, _| store.flush())
                    .ok();
            }
        });
        flush.detach();

        cx.spawn(async move |cx| {
            let (state, runtime) = match state_rx.await {
                Ok(pair) => pair,
                Err(_) => return, // backend thread died before initializing
            };
            let handle = runtime.handle().clone();
            let backend = Backend {
                state: state.clone(),
                handle,
                _runtime: Arc::new(runtime),
            };

            cx.update(|cx| {
                cx.set_global(backend);
                // Initial gate: `get_session` (session undefined while the
                // backend boots → Unknown, then null/session).
                refresh_session(cx);
                // Initial presence + download snapshots (`useRunningInstances`
                // and `useDownloads` both seed from a fetch on mount).
                refresh_running(cx);
                refresh_downloads(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Runs a backend command on the tokio runtime; the returned future is
    /// awaitable from any executor (gpui tasks await these directly).
    pub fn run<F, R>(&self, fut: F) -> tokio::task::JoinHandle<R>
    where
        F: std::future::Future<Output = R> + Send + 'static,
        R: Send + 'static,
    {
        self.handle.spawn(fut)
    }
}

/// Spawns `commands::get_session` and applies the result to the gate.
pub fn refresh_session(cx: &mut App) {
    let state = cx.global::<Backend>().state.clone();
    let handle = cx.global::<Backend>().handle.clone();
    let task = handle.spawn(async move { commands::auth::get_session(&state).await });

    cx.spawn(async move |cx| {
        if let Ok(Ok(session)) = task.await {
            cx.update(|cx| {
                cx.update_global(|store: &mut AuthStore, _| {
                    store.gate = match session {
                        Some(session) => Gate::LoggedIn(session),
                        None => Gate::LoggedOut,
                    };
                });
            })
            .ok();
        }
    })
    .detach();
}

/// The saved-account list for the account switcher.
pub fn refresh_accounts(cx: &mut App) {
    let state = cx.global::<Backend>().state.clone();
    let handle = cx.global::<Backend>().handle.clone();
    let task = handle.spawn(async move { commands::auth::list_accounts(&state).await });

    cx.spawn(async move |cx| {
        if let Ok(Ok(accounts)) = task.await {
            cx.update(|cx| {
                cx.update_global(|store: &mut AuthStore, _| {
                    store.accounts = accounts;
                });
            })
            .ok();
        }
    })
    .detach();
}

/// `login_offline` — resolves to the session or the error message.
pub fn login_offline(
    cx: &mut App,
    username: String,
) -> tokio::task::JoinHandle<Result<AuthSession, backend::error::AppError>> {
    let state = cx.global::<Backend>().state.clone();
    cx.global::<Backend>()
        .run(async move { commands::auth::login_offline(&state, username).await })
}

/// `login_microsoft` through the embedded sign-in window.
pub fn login_microsoft(
    cx: &mut App,
    host: Arc<dyn backend::services::minecraft::auth::LoginHost>,
) -> tokio::task::JoinHandle<Result<AuthSession, backend::error::AppError>> {
    let state = cx.global::<Backend>().state.clone();
    cx.global::<Backend>()
        .run(async move { commands::auth::login_microsoft(&state, host.as_ref()).await })
}

/// Applies a successful sign-in to the store (gate + accounts).
pub fn publish_session(cx: &mut App, session: AuthSession) {
    cx.update_global(|store: &mut AuthStore, _| {
        store.gate = Gate::LoggedIn(session);
        store.progress = None;
    });
    refresh_accounts(cx);
}

// ---------------------------------------------------------------------------
// Launch-slice command surface — every backend call the Play page, launch
// flow and console make, shaped like the `api.*` calls the React components
// issued (each awaits a `tokio` task from a gpui task).
// ---------------------------------------------------------------------------

/// Runs a backend command on the tokio runtime. `make` receives the
/// `AppState` and builds the future to run — the waker-driven JoinHandle is
/// awaited directly from gpui tasks, executor-agnostic like every other
/// bridge call.
pub fn command<R, F, M>(cx: &mut App, make: M) -> tokio::task::JoinHandle<R>
where
    R: Send + 'static,
    F: std::future::Future<Output = R> + Send + 'static,
    M: FnOnce(AppState) -> F + Send + 'static,
{
    let state = cx.global::<Backend>().state.clone();
    let handle = cx.global::<Backend>().handle.clone();
    handle.spawn(async move { make(state).await })
}

/// Seeds `RunningStore` from `list_running` (the hook's initial fetch).
pub fn refresh_running(cx: &mut App) {
    let task = command(cx, |state| async move {
        commands::launch::list_running_instances(&state).await
    });
    cx.spawn(async move |cx| {
        if let Ok(Ok(list)) = task.await {
            cx.update_global(|store: &mut RunningStore, _| {
                store.map = list
                    .into_iter()
                    .map(|info| (info.instance_id.clone(), info))
                    .collect();
            })
            .ok();
        }
    })
    .detach();
}

/// Seeds `DownloadsStore` from `list_downloads` (the hook's initial fetch).
pub fn refresh_downloads(cx: &mut App) {
    let task = command(cx, |state| async move {
        commands::downloads::list_downloads(&state).await
    });
    cx.spawn(async move |cx| {
        if let Ok(Ok(list)) = task.await {
            cx.update_global(|store: &mut DownloadsStore, _| {
                store.map = list.into_iter().map(|t| (t.id.clone(), t)).collect();
            })
            .ok();
        }
    })
    .detach();
}
