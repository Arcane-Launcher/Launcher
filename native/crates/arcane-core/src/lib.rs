//! Arcane Launcher — domain logic layer.
//!
//! Every module is copied from or mechanically adapted out of the Tauri
//! backend (`../../src-tauri/src`, kept intact during the migration). See
//! `../../MIGRATION.md` for the per-module reuse map; this crate contains
//! **zero** Tauri references.
//!
//! Phase 1 scope: models + services + runtime plumbing compile green with
//! the event-sink boundary in place. The `commands` module (the ~110 thin
//! command wrappers) is re-introduced once the services are green.

pub mod commands;
pub mod error;
pub mod events;
pub mod models;
pub mod paths;
pub mod runtime;
pub mod services;
pub mod state;
