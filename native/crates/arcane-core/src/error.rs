//! Shared error shape.
//!
//! Mirrors `src-tauri/src/error.rs::AppError` exactly — the frontend
//! (`src/lib/tauri-client.ts::ArcaneApiError`) depends on these three fields,
//! and the ported GPUI UI will deserialize/handle the identical shape.
//!
//! NOTE (Phase 1): replaced by a verbatim copy of the Tauri module during the
//! extraction; kept here only so the scaffold compiles against the contract.

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
#[error("{message}")]
pub struct AppError {
    pub message: String,
    #[serde(default)]
    pub technical: Option<String>,
    #[serde(default)]
    pub retryable: bool,
}
