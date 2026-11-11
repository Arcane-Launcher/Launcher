//! Tokio runtime plumbing — replaces `tauri::async_runtime`.
//!
//! Tauri kept one global multi-thread runtime and services used
//! `tauri::async_runtime::{block_on, spawn}` at a handful of call sites
//! (recorder, update_on_exit). This module provides the same contract on a
//! plain `tokio` runtime: one global multi-thread runtime, `block_on` must
//! be called from a NON-async context (a blocking thread or the UI thread),
//! exactly like the Tauri API it replaces — no call-site semantics changed.

use std::future::Future;
use std::sync::OnceLock;

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// The global backend runtime (multi-thread, all features).
pub fn runtime() -> &'static tokio::runtime::Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to build the Arcane tokio runtime")
    })
}

/// `tauri::async_runtime::block_on` equivalent: blocks the current thread
/// on the global runtime. Same constraint as the Tauri API it replaces —
/// never call this from inside an async task.
pub fn block_on<F: Future>(future: F) -> F::Output {
    runtime().block_on(future)
}

/// `tauri::async_runtime::spawn` equivalent.
pub fn spawn<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    runtime().spawn(future)
}
