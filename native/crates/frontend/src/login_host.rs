//! The embedded Microsoft sign-in window — the desktop shell's
//! `LoginHost` implementation (MIGRATION.md §6: a small real browser
//! window spawned only for OAuth, then closed).
//!
//! Uses `tao` (windowing) + `wry` (the WebView2/webkit bindings Tauri
//! itself sits on) — the same stack the Tauri app's
//! `WebviewWindowBuilder` produced, without Tauri. The window runs its
//! own event loop on a dedicated thread (Windows allows window+message
//! loops on any thread; WebView2 lives entirely on that thread), so the
//! gpui main loop is never blocked. Closing the window (by the user or
//! by `close()`) ends that thread.
//!
//! Behavior contract (from `backend/services/minecraft/auth.rs`):
//! - every navigation is reported to `on_navigation(url)`; returning
//!   `false` cancels it (Microsoft's `nativeclient` redirect is only
//!   *observed*, never loaded),
//! - `on_closed` fires when the window goes away,
//! - `close()` closes it programmatically (the 600s backend timeout).

use std::sync::Arc;

use backend::error::{AppError, AppResult};
use backend::services::minecraft::auth::{LoginHost, LoginWindow};
use tao::{
    event::{Event, StartCause, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::WindowBuilder,
};
use wry::WebViewBuilder;

/// Messages the sign-in window's event loop accepts from other threads.
enum LoginEvent {
    Close,
}

struct TaoLoginWindow {
    proxy: tao::event_loop::EventLoopProxy<LoginEvent>,
}

impl LoginWindow for TaoLoginWindow {
    fn close(&self) {
        let _ = self.proxy.send_event(LoginEvent::Close);
    }
}

pub struct WebViewLoginHost;

impl LoginHost for WebViewLoginHost {
    fn open_sign_in_window(
        &self,
        url: String,
        on_navigation: Arc<dyn Fn(&str) -> bool + Send + Sync>,
        on_closed: Arc<dyn Fn() + Send + Sync>,
    ) -> AppResult<Box<dyn LoginWindow>> {
        let (proxy_tx, proxy_rx) = std::sync::mpsc::channel();

        let nav = on_navigation.clone();
        let closed = on_closed.clone();

        std::thread::Builder::new()
            .name("ms-login".into())
            .spawn(move || {
                run_login_window(url, nav, closed, proxy_tx);
            })
            .map_err(|e| AppError::new("Couldn't open the sign-in window.", e.to_string()))?;

        // The thread sends us the proxy it created, which is what
        // `LoginWindow::close` needs.
        let proxy = proxy_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .map_err(|_| AppError::new("Couldn't open the sign-in window.", "login window thread did not start"))?;

        Ok(Box::new(TaoLoginWindow { proxy }))
    }
}

/// Creates the 480×640 centered decorated window ("Sign in with
/// Microsoft"), embeds the webview and pumps events until it closes.
fn run_login_window(
    url: String,
    on_navigation: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    on_closed: Arc<dyn Fn() + Send + Sync>,
    proxy_tx: std::sync::mpsc::Sender<tao::event_loop::EventLoopProxy<LoginEvent>>,
) {
    let event_loop = EventLoopBuilder::<LoginEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let _ = proxy_tx.send(proxy.clone());

    // Center on the primary monitor's work area.
    let (width, height) = (480.0_f64, 640.0_f64);
    let mut builder = WindowBuilder::new()
        .with_title("Sign in with Microsoft")
        .with_inner_size(tao::dpi::LogicalSize::new(width, height))
        .with_resizable(true)
        .with_decorations(true);
    let position = event_loop.primary_monitor().and_then(|monitor| {
        let size = monitor.size();
        let pos = monitor.position();
        let scale = monitor.scale_factor();
        Some(tao::dpi::PhysicalPosition::new(
            pos.x + ((size.width as f64 - width * scale) / 2.0) as i32,
            pos.y + ((size.height as f64 - height * scale) / 2.0) as i32,
        ))
    });
    if let Some(p) = position {
        builder = builder.with_position(p);
    }

    let window = match builder.build(&event_loop) {
        Ok(window) => window,
        Err(_) => {
            on_closed();
            return;
        }
    };

    let webview = WebViewBuilder::new()
        .with_url(url)
        .with_navigation_handler(move |uri: String| on_navigation(&uri))
        .build(&window);

    if webview.is_err() {
        on_closed();
        return;
    }

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::NewEvents(StartCause::Init) => {
                window.set_focus();
            }
            Event::UserEvent(LoginEvent::Close) => {
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested { .. },
                ..
            } => {
                // The user closed the window — read as "cancelled".
                *control_flow = ControlFlow::Exit;
                on_closed();
            }
            Event::WindowEvent {
                event: WindowEvent::Destroyed,
                ..
            } => {
                *control_flow = ControlFlow::Exit;
            }
            Event::MainEventsCleared => {
                window.request_redraw();
            }
            _ => {}
        }
    });
}
