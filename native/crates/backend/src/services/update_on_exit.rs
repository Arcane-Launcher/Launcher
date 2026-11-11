//! Installs a waiting launcher update when the player closes the launcher,
//! instead of interrupting them with a countdown later.
//!
//! Runs in Rust (not the UI) for one reason: closing the window by any
//! route — the title-bar X, Alt+F4, the taskbar — all funnels through one
//! close handler.
//!
//! MIGRATION NOTE: in the Tauri build this module owned the whole close
//! interception (`intercept_close`) including hiding webview windows and
//! running the plugin-updater install without a relaunch. Those are
//! window-shell + installer-suite concerns, so `intercept_close`/`install`
//! move to the native desktop crate together with the self-updater rewrite
//! (MIGRATION.md Phase 5) and keep these exact statics as the shared state
//! between the two — same fields, same semantics, so the settings command
//! (`set_update_preferences` → `set_enabled`) and the updater flow
//! (`set_ready`) work unchanged across the migration.

use std::sync::atomic::{AtomicBool, Ordering};

/// Set by the updater once an update has been downloaded and verified.
static UPDATE_READY: AtomicBool = AtomicBool::new(false);
static INSTALLING: AtomicBool = AtomicBool::new(false);
/// Settings > About > "Install updates when closing the launcher".
static ENABLED: AtomicBool = AtomicBool::new(true);

pub fn set_ready(ready: bool) {
    UPDATE_READY.store(ready, Ordering::SeqCst);
}

pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::SeqCst);
}

/// Whether a close can trigger the install (all three conditions the old
/// `intercept_close` gate checked; the shell additionally requires no game
/// running before consulting these).
pub fn close_can_install() -> bool {
    ENABLED.load(Ordering::SeqCst) && UPDATE_READY.load(Ordering::SeqCst)
}

/// Marks an install as started (dedupes repeated close clicks); returns
/// `false` if one is already under way.
pub fn begin_install() -> bool {
    !INSTALLING.swap(true, Ordering::SeqCst)
}

/// Clears the installing flag (updater finished or gave up).
pub fn end_install() {
    INSTALLING.store(false, Ordering::SeqCst);
}
