//! Arcane Launcher — native GPUI frontend application.
//!
//! Phase 2 (see MIGRATION.md §3): app shell + auth. This file carries the
//! application bootstrap — module wiring, embedded assets, font
//! registration and the main window options.
//!
//! Source of every literal below (do not "improve" these values):
//! - 1280×800 / min 960×640 / resizable / centered — `src-tauri/tauri.conf.json`
//! - `decorations: false` + custom-drawn title bar — GPUI
//!   `TitlebarOptions { appears_transparent: true }` (verified against
//!   gpui-0.2.2 `platform/windows/window.rs`, which hides the OS frame and
//!   keeps WS_THICKFRAME resizing in exactly this configuration)
//! - `#070708` — root `--background` token, `src/app/globals.css:5`
//! - title "Arcane Launcher" — `tauri.conf.json`

mod account;
mod assets;
mod background;
mod bridge;
mod icons;
mod login;
mod login_host;
mod shell;
mod text_input;
mod theme;
mod toasts;

use gpui::{px, size, Application, AppContext as _, SharedString, TitlebarOptions, WindowBounds, WindowOptions};
use theme::Theme;

use crate::assets::Assets;

fn main() {
    Application::new().with_assets(Assets).run(|cx| {
        theme::register_fonts(cx);
        assets::assert_assets_valid();
        cx.set_global(Theme::obsidian());
        // Toast store — the Toaster renders/dismisses from it on every gate.
        cx.set_global(toasts::ToastStore::default());
        // Boots the tokio runtime + AppState on a background thread; the
        // gate flips to LoggedOut/LoggedIn once `get_session` resolves.
        bridge::Backend::initialize(cx);
        let shell = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::centered(
                    size(px(1280.), px(800.)),
                    cx,
                )),
                titlebar: Some(TitlebarOptions {
                    title: Some(SharedString::from("Arcane Launcher")),
                    appears_transparent: true,
                    traffic_light_position: None,
                }),
                window_min_size: Some(size(px(960.), px(640.))),
                is_resizable: true,
                ..Default::default()
            },
            |_, cx| cx.new(shell::ShellView::new),
        );
        if let Err(error) = shell {
            eprintln!("failed to open the Arcane Launcher window: {error:?}");
            std::process::exit(1);
        }
        cx.activate(true);
    });
}
