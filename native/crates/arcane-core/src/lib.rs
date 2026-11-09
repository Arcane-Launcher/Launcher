//! Arcane Launcher — domain logic layer.
//!
//! Every module in this crate is copied (or mechanically adapted) from the
//! Tauri backend in `../../src-tauri/src`. See `MIGRATION.md` for the
//! per-module reuse map: this crate must contain **zero** Tauri references.
//!
//! Phase 0 scaffold: the event-sink boundary and shared error type are laid
//! down here first because every copied service depends on them. Services
//! themselves arrive verbatim in Phase 1.

pub mod error;
pub mod events;
