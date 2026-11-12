//! Embedded asset source for the GPUI frontend.
//!
//! Serves every runtime asset the Tauri app shipped in `public/` plus the
//! generated icon SVGs (`icons.rs`). The binary-backed assets (logo, app
//! background) are copied from the Tauri project into `assets/` and
//! embedded with `include_bytes!` — the app must render identically
//! offline, and none of this needs the network at runtime.

use std::borrow::Cow;
use std::sync::OnceLock;

use gpui::{AssetSource, Result, SharedString};

use crate::icons;

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(lookup(path))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        if path == icons::ASSET_DIR {
            return Ok(icons::all_icons()
                .iter()
                .map(|(name, _)| SharedString::from(format!("{}/{}", icons::ASSET_DIR, name)))
                .collect());
        }
        Ok(vec![])
    }
}

fn lookup(path: &str) -> Option<Cow<'static, [u8]>> {
    // Icons first — the most common lookup during rendering.
    for (name, data) in icons::all_icons() {
        if path == format!("{}/{}", icons::ASSET_DIR, name) {
            return Some(Cow::Borrowed(data.as_bytes()));
        }
    }

    match path {
        // The launcher mark rendered in the title bar (16px) and sidebar
        // (40px) — copied verbatim from `public/logo.png`.
        "logo.png" => Some(Cow::Borrowed(include_bytes!(
            "../assets/logo.png"
        ).as_slice())),
        // Default signed-in background — copied verbatim from
        // `public/bg.webp` (the custom-background/halloween swaps are Phase 5).
        "bg.webp" => Some(Cow::Borrowed(include_bytes!(
            "../assets/bg.webp"
        ).as_slice())),
        // Login hero backdrop — first frame of `public/login-bg.webm`
        // (ffmpeg-extracted into a still; gpui cannot loop a video, so the
        // 16%-opacity backdrop renders this frame instead).
        "login-bg-still.webp" => Some(Cow::Borrowed(include_bytes!(
            "../assets/login-bg-still.webp"
        ).as_slice())),
        _ => None,
    }
}

/// Ensure the embedded asset map is complete at startup: every declared
/// icon must resolve and be parseable SVG, and both binary assets must be
/// non-empty. Catches typos in the path table (a missing icon otherwise
/// renders as an invisible element with no error).
pub fn assert_assets_valid() {
    static CHECKED: OnceLock<()> = OnceLock::new();
    CHECKED.get_or_init(|| {
        let assets = Assets;
        for (name, _) in icons::all_icons() {
            let path = format!("{}/{}", icons::ASSET_DIR, name);
            assert!(
                lookup(&path).is_some(),
                "declared icon '{path}' is missing from the asset table"
            );
        }
        for required in ["logo.png", "bg.webp", "login-bg-still.webp"] {
            let data = lookup(required)
                .unwrap_or_else(|| panic!("required asset '{required}' is missing"));
            assert!(!data.is_empty(), "asset '{required}' is empty");
        }
        let _ = &assets;
    });
}
