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
mod console;
mod format;
mod icons;
mod instance_settings;
mod instances;
mod instances_dialogs;
mod launch;
mod login;
mod login_host;
mod modals;
mod play;
mod shell;
mod text_input;
mod theme;
mod toasts;
mod ui;

use gpui::{
    AppContext as _, SharedString, TitlebarOptions, WindowBounds, WindowOptions, application, px,
    size,
};
use theme::Theme;

use crate::assets::Assets;

fn main() {
    application().with_assets(Assets).run(|cx| {
        // Registers the styled-component/theme layers (gpui-component).
        gpui::init(cx);
        theme::register_fonts(cx);
        assets::assert_assets_valid();
        cx.set_global(Theme::obsidian());
        // gpui-component's widgets read their own theme global; mirror the
        // obsidian palette into it (after both init steps above).
        Theme::sync_component_theme(cx);
        // Toast store — the Toaster renders/dismisses from it on every gate.
        cx.set_global(toasts::ToastStore::default());
        // Boots the tokio runtime + AppState on a background thread; the
        // gate flips to LoggedOut/LoggedIn once `get_session` resolves.
        bridge::Backend::initialize(cx);
        let shell = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1280.), px(800.)), cx)),
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
