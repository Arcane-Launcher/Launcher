//! The thin command wrappers the Tauri frontend invoked (command surface
//! from `src-tauri/src/commands`, unchanged in shape). In the native app the
//! GPUI shell calls these Rust functions directly — no serialization or
//! command registry in between — so every wrapper keeps the exact same
//! signature, error type, and defaults.
//!
//! NOTE: `pub mod updates` is deferred with the whole updater rewrite to
//! Phase 5 (MIGRATION.md): `updater.rs` was webview/updater-plugin specific,
//! so its command wrappers return together with the native updater.

pub mod auth;
pub mod background;
pub mod content;
pub mod crash;
pub mod downloads;
pub mod evolution;
pub mod import;
pub mod instances;
pub mod launch;
pub mod mods;
pub mod modpacks;
pub mod network;
pub mod performance;
pub mod recorder;
pub mod servers;
pub mod settings;
pub mod skins;
pub mod storage;
pub mod system;
