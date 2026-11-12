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

use std::sync::Arc;

use gpui::{App, BorrowAppContext, Global};
use tokio::runtime::{Handle, Runtime};

use backend::{
    commands,
    events::{Event, EventName, EventSink},
    models::minecraft::AuthSession,
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
        // The store exists before the backend thread finishes — the shell
        // renders from it from the first frame (gate Unknown = blank).
        cx.set_global(AuthStore {
            gate: Gate::Unknown,
            progress: None,
            accounts: Vec::new(),
        });

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

        // Drain the event channel on the main thread, routing the kinds the
        // UI consumes today (more arrive with each ported screen).
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
                            cx.update(|cx| {
                                cx.update_global(|store: &mut AuthStore, _| {
                                    store.progress = Some(message);
                                });
                            })
                            .ok();
                        }
                    }
                    _ => {
                        // launch/download/network/recorder events land with
                        // their screens; they're safe to ignore until then.
                    }
                }
            }
        });
        drain.detach();

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
    cx.global::<Backend>().run(async move {
        commands::auth::login_offline(&state, username).await
    })
}

/// `login_microsoft` through the embedded sign-in window.
pub fn login_microsoft(
    cx: &mut App,
    host: Arc<dyn backend::services::minecraft::auth::LoginHost>,
) -> tokio::task::JoinHandle<Result<AuthSession, backend::error::AppError>> {
    let state = cx.global::<Backend>().state.clone();
    cx.global::<Backend>().run(async move {
        commands::auth::login_microsoft(&state, host.as_ref()).await
    })
}

/// Applies a successful sign-in to the store (gate + accounts).
pub fn publish_session(cx: &mut App, session: AuthSession) {
    cx.update_global(|store: &mut AuthStore, _| {
        store.gate = Gate::LoggedIn(session);
        store.progress = None;
    });
    refresh_accounts(cx);
}
