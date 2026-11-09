//! Arcane Launcher — native GPUI desktop application.
//!
//! Phase 0 scaffold (see MIGRATION.md §3): opens the shell window with the
//! exact geometry and chrome of the Tauri window so Phase 2 can fill in the
//! shell screens and diff them side-by-side against the Tauri app.
//!
//! Source of every literal below (do not "improve" these values):
//! - 1280×800 / min 960×640 / resizable / centered — `src-tauri/tauri.conf.json`
//! - `decorations: false` + custom-drawn title bar — GPUI
//!   `TitlebarOptions { appears_transparent: true }` (verified against
//!   gpui-0.2.2 `platform/windows/window.rs`, which hides the OS frame and
//!   keeps WS_THICKFRAME resizing in exactly this configuration)
//! - `#070708` — root `--background` token, `src/app/globals.css:5`
//! - title "Arcane Launcher" — `tauri.conf.json`

use gpui::{
    div, prelude::*, px, rgb, size, Application, Context, Render, SharedString,
    TitlebarOptions, Window, WindowBounds, WindowOptions,
};

/// The persistent shell view. Phase 2 replaces this with the real
/// AppShell (title bar + sidebar + top bar + routed screens); the
/// geometry and background below are already final values.
struct ShellView;

impl Render for ShellView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(0x070708))
            .text_color(rgb(0xF5F5F6))
    }
}

fn main() {
    Application::new().run(|cx| {
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
            |_, cx| cx.new(|_| ShellView),
        );
        if let Err(error) = shell {
            eprintln!("failed to open the Arcane Launcher window: {error:?}");
            std::process::exit(1);
        }
        cx.activate(true);
    });
}
