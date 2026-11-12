//! The app shell — direct port of the Tauri app's layout components:
//! `title-bar.tsx`, `sidebar.tsx`, `top-bar.tsx` and the gate logic of
//! `app-shell.tsx`. Every dimension/color here is copied verbatim from the
//! React sources; see the comments for the source of each literal.
//!
//! The gate reads the `AuthStore` global (backend `get_session`): unknown
//! → blank, no session → the full-screen login screen, session → the
//! sidebar + top bar shell with the account switcher.

use std::time::{Duration, Instant};

use gpui::{
    div, img, prelude::*, px, App, Context, Entity, FontWeight, Hsla, Rgba,
    SharedString, Window, WindowControlArea,
};

use crate::{
    account::AccountSwitcher,
    background::BackgroundStack,
    bridge::{AuthStore, Gate},
    icons,
    login::LoginScreen,
    theme::{self, Theme},
    toasts::Toaster,
};

// Tailwind neutral palette values used by the shell (not theme tokens —
// they are hardcoded in the React components).
const NEUTRAL_200: Rgba = crate::theme::rgba_const(0xE5E5E5FF);
const NEUTRAL_400: Rgba = crate::theme::rgba_const(0xA1A1AAFF);
const NEUTRAL_500: Rgba = crate::theme::rgba_const(0x737373FF);
const NEUTRAL_600: Rgba = crate::theme::rgba_const(0x525252FF);

/// How long the fade-out plays before the real window action fires —
/// `app-shell.tsx` WINDOW_EXIT_MS. The CSS exit animations also scale
/// (0.85 + translateY(48px) for minimize, 0.96 for close); gpui 0.2 has no
/// div transforms, so the fade is opacity-only for now.
const WINDOW_EXIT_MS: u64 = 180;

/// Which window action an exit animation is leading into.
#[derive(Clone, Copy, PartialEq)]
enum ExitKind {
    Minimize,
    Close,
}

struct ExitAnim {
    kind: ExitKind,
    started: Instant,
    /// Whether the real window action already fired (the content stays
    /// faded out until the window is minimized/destroyed).
    fired: bool,
}

/// `app-shell.tsx`'s gate lives in `backend::Gate` (the `AuthStore`
/// global): session not loaded yet → blank; logged out → login screen;
/// logged in → sidebar + top bar shell.

pub struct ShellView {
    /// Created when the gate first flips to LoggedOut; dropped again on
    /// the other gates.
    login: Option<Entity<LoginScreen>>,
    exit: Option<ExitAnim>,
    background: Entity<BackgroundStack>,
    account: Entity<AccountSwitcher>,
    toaster: Entity<Toaster>,
}

impl ShellView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // Re-render on every auth-state change (gate, progress messages).
        cx.observe_global::<AuthStore>(|_, cx| cx.notify()).detach();
        Self {
            login: None,
            exit: None,
            background: cx.new(|_| BackgroundStack::new(0.70, false)),
            account: cx.new(AccountSwitcher::new),
            toaster: cx.new(|_| Toaster),
        }
    }

    /// Renders the window content with the exit fade applied while an exit
    /// animation is running. The toaster and the "Add account" modal hang
    /// off the root so their overlays can cover the whole window (gpui has
    /// no `position: fixed`).
    fn content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = Theme::get(cx);

        let mut root = div()
            .id("app-root")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(theme.background)
            .text_color(theme.foreground)
            .font_family(theme::FONT_FAMILY)
            .child(self.title_bar(window, cx))
            .child(self.body(window, cx))
            .child(self.toaster.clone());

        if let Some(modal) = self.add_account_modal(cx) {
            root = root.child(modal);
        }

        if let Some(exit) = &self.exit {
            let fraction = ((Instant::now() - exit.started).as_secs_f32()
                / (WINDOW_EXIT_MS as f32 / 1000.0))
            .clamp(0.0, 1.0);
            // CSS `ease-in` = cubic-bezier(0.42, 0, 1, 1); the tail of the
            // curve dominates at these durations.
            let eased = fraction * fraction;
            root = root.opacity(1.0 - eased);
        }

        root.into_any_element()
    }

    // -----------------------------------------------------------------
    // Title bar — `title-bar.tsx`
    // -----------------------------------------------------------------

    /// 36px bar (`h-9`): logo + app name on the left, window controls on
    /// the right; the left half is the native drag region (HTCAPTION —
    /// gives native dragging, snap layouts and double-click maximize, the
    /// same behavior Tauri's `data-tauri-drag-region` produced).
    fn title_bar(&self, window: &Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = Theme::get(cx);
        let maximized = window.is_maximized();

        div()
            .id("title-bar")
            .h(px(36.0))
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(theme.login_hero_border) // border-white/6
            .bg(theme.titlebar)
            // Left half is the drag region (logo + name; the Beta/recorder
            // buttons join it in later phases).
            .child(
                div()
                    .id("title-bar-left")
                    .flex_1()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pl_3()
                    .window_control_area(WindowControlArea::Drag)
                    .child(
                        img("logo.png")
                            .size(px(16.0))
                            .rounded(px(4.0))
                            .flex_none(),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(NEUTRAL_400)
                            .child("Arcane Launcher"),
                    ),
            )
            // Window controls: 44px-wide buttons (`w-11`), hover white/8,
            // close hover bg-danger — title-bar.tsx:70-84.
            .child(
                div()
                    .id("title-bar-controls")
                    .flex_none()
                    .h_full()
                    .flex()
                    .child(self.window_button(
                        cx,
                        "minimize",
                        if maximized { icons::CORNERS_IN } else { icons::SQUARE },
                        11.0,
                        ExitKind::Minimize,
                        false,
                    ))
                    .child(self.window_button(
                        cx,
                        "close",
                        icons::X,
                        13.0,
                        ExitKind::Close,
                        true,
                    )),
            )
            .into_any_element()
    }

    /// One window control: hover tint, then the 180ms exit fade leading
    /// into the real action (`app-shell.tsx` handleMinimize/handleClose).
    /// Deliberately NOT a native `WindowControlArea` — the native hit-test
    /// would minimize/close immediately and skip the animation, exactly
    /// why the Tauri app uses custom buttons instead of the OS frame.
    fn window_button(
        &self,
        cx: &Context<Self>,
        id: &'static str,
        icon: &'static str,
        icon_size: f32,
        kind: ExitKind,
        is_close: bool,
    ) -> gpui::AnyElement {
        let theme = *Theme::get(cx);
        let hover_bg = if is_close { theme.danger } else { theme::white(0.08) };

        div()
            .id(id)
            .w(px(44.0))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(NEUTRAL_400)
            .hover(move |style| style.bg(hover_bg).text_color(theme.foreground))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.start_exit(kind, window, cx);
            }))
            .child(icons::icon_at(icon, icon_size))
            .into_any_element()
    }

    // -----------------------------------------------------------------
    // Body — `app-shell.tsx`
    // -----------------------------------------------------------------

    fn body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let gate = cx.global::<AuthStore>().gate.clone();
        match gate {
            // Session status not yet known — avoid flashing either state.
            Gate::Unknown => div()
                .flex_1()
                .h_full()
                .bg(Theme::get(cx).background)
                .into_any_element(),
            Gate::LoggedOut => {
                if self.login.is_none() {
                    self.login = Some(cx.new(|cx| LoginScreen::new(window, cx)));
                }
                div()
                    .id("login-body")
                    .flex_1()
                    .min_h_0()
                    .h_full()
                    .overflow_hidden()
                    .child(self.login.as_ref().unwrap().clone())
                    .into_any_element()
            }
            Gate::LoggedIn(session) => {
                // The login screen (and its busy state) doesn't survive
                // into the signed-in shell.
                self.login = None;
                let _ = session;
                div()
                    .id("shell-body")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .overflow_hidden()
                    .child(self.sidebar(cx))
                    // Right column: background stack + top bar + routed page.
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .overflow_hidden()
                            .child(self.background.clone())
                            .child(
                                div()
                                    .relative()
                                    .flex_1()
                                    .min_h_0()
                                    .flex()
                                    .flex_col()
                                    .overflow_hidden()
                                    .child(self.top_bar(cx))
                                    // main: routed pages land here (px-8 pb-8,
                                    // scrollable — `main` in app-shell.tsx).
                                    .child(
                                        div()
                                            .id("page-content")
                                            .flex_1()
                                            .min_h_0()
                                            .overflow_y_scroll()
                                            .px_8()
                                            .pb_8(),
                                    ),
                            ),
                    )
                    .into_any_element()
            }
        }
    }

    /// The "Add account" modal from the account switcher
    /// (`fixed inset-0 bg-black/60`, centered `glass-panel max-w-sm p-6`),
    /// rendered off the root so it covers the whole window.
    fn add_account_modal(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.account.read(cx).is_adding() {
            return None;
        }
        let Some(form) = self.account.read(cx).modal_form() else {
            return None;
        };
        let theme = Theme::get(cx);

        Some(
            div()
                .id("add-account-modal")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme::rgba_const(0x00000099)) // bg-black/60
                .p(px(16.0))
                .occlude()
                .child(
                    div()
                        .w(px(384.0)) // max-w-sm
                        .flex()
                        .flex_col()
                        .p(px(24.0)) // p-6
                        .rounded(px(16.0)) // rounded-2xl (glass-panel)
                        .bg(theme.surface)
                        .border_1()
                        .border_color(theme.border)
                        .shadow(vec![gpui::BoxShadow {
                            color: Hsla {
                                a: 0.25,
                                ..Hsla::from(theme.background)
                            },
                            offset: gpui::point(px(0.), px(25.)),
                            blur_radius: px(50.),
                            spread_radius: px(-12.),
                        }])
                        .child(
                            div()
                                .mb(px(16.0)) // mb-4
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(px(14.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.muted)
                                        .child("Add account"),
                                )
                                .child(
                                    div()
                                        .id("add-account-close")
                                        .flex_none()
                                        .h(px(32.0)) // sm: h-8
                                        .px(px(12.0)) // px-3
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(8.0)) // rounded-lg
                                        .text_size(px(12.0))
                                        .text_color(theme.foreground)
                                        .border_1()
                                        .border_color(theme.border)
                                        .hover(move |style| {
                                            style
                                                .bg(theme.glass_hover)
                                                .border_color(theme.border_strong)
                                        })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.account.update(cx, |switcher, cx| {
                                                switcher.cancel_add(cx);
                                            });
                                        }))
                                        .child(icons::icon_at(icons::X, 13.0)),
                                ),
                        )
                        .child(form),
                )
                .into_any_element(),
        )
    }

    // -----------------------------------------------------------------
    // Sidebar — `sidebar.tsx`
    // -----------------------------------------------------------------

    /// 84px rail (`w-21`): 40px logo + "ARC" mark, then the nav items
    /// (48×48, r12, gap 6px) with the 3×24 accent indicator on the active
    /// item.
    fn sidebar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = Theme::get(cx);
        let links: [(&'static str, &'static str, &'static str); 6] = [
            ("play", "Play", icons::PLAY),
            ("mods", "Explore mods", icons::COMPASS),
            ("instances", "Instances", icons::STACK),
            ("downloads", "Downloads", icons::DOWNLOAD_SIMPLE),
            ("skin", "Skin", icons::USER),
            ("settings", "Settings", icons::GEAR_SIX),
        ];
        // Routing lands with the page port; "Play" is the active page.
        let active = "play";

        let mut rail = div()
            .id("sidebar")
            .w(px(84.0))
            .flex_none()
            .relative()
            .flex()
            .flex_col()
            .items_center()
            .border_r_1()
            .border_color(theme.login_hero_border) // border-white/6
            .bg(theme.sidebar) // #0c0d12/90 over the window background
            .py_6()
            // Logo block (mb-9 = 36px), with its tooltip.
            .child(
                div()
                    .id("sidebar-logo")
                    .mb_9()
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(
                        img("logo.png")
                            .size(px(40.0))
                            .rounded(px(12.0)) // rounded-xl
                            .shadow(vec![gpui::BoxShadow {
                                color: Hsla {
                                    a: 0.5,
                                    ..Hsla::from(theme.background)
                                },
                                offset: gpui::point(px(0.), px(0.)),
                                blur_radius: px(10.0),
                                spread_radius: px(0.),
                            }]),
                    )
                    .child(
                        div()
                            .mt_1p5()
                            .font_family(theme::FONT_FAMILY_MONO_FALLBACK)
                            .text_size(px(10.0))
                            .font_weight(FontWeight::EXTRA_BOLD)
                            .text_color(theme.foreground)
                            .child("ARC"),
                    ),
            );

        let mut nav = div()
            .id("sidebar-nav")
            .w_full()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .gap_1p5()
            .px_3();

        for (key, label, icon) in links {
            let is_active = key == active;
            let theme = Theme::get(cx);
            let mut item = div()
                .id(SharedString::from(format!("nav-{key}")))
                .relative()
                .size(px(48.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(12.0)) // rounded-xl
                .tooltip(|window, cx| {
                    crate::shell::tooltip(label, window, cx)
                })
                .when(is_active, |item| {
                    // 3×24 accent pill, centered on the item's left edge
                    // minus 12px (`-left-3`), with the accent glow.
                    item.child(
                        div()
                            .absolute()
                            .left(px(-12.0))
                            .top(px(12.0)) // top-1/2 - h-6/2
                            .w(px(3.0))
                            .h(px(24.0))
                            .rounded(px(1.5))
                            .bg(theme.accent)
                            .shadow(vec![gpui::BoxShadow {
                                color: Hsla::from(theme::accent_alpha(0.55)),
                                offset: gpui::point(px(0.), px(0.)),
                                blur_radius: px(10.0),
                                spread_radius: px(1.0),
                            }]),
                    )
                })
                .child(icons::icon_at(icon, 20.0).text_color(if is_active {
                    theme.accent
                } else {
                    NEUTRAL_500
                }));

            if !is_active {
                let hover_icon = NEUTRAL_200;
                item = item.hover(move |style| {
                    style
                        .bg(theme::white(0.06))
                        .text_color(hover_icon)
                });
                // `active:scale-95` — no transforms in gpui 0.2.
            }

            nav = nav.child(item);
        }

        rail = rail.child(nav);
        rail.into_any_element()
    }

    // -----------------------------------------------------------------
    // Top bar — `top-bar.tsx`
    // -----------------------------------------------------------------

    /// Breadcrumb on the left (`ARCANE / PAGE`), download tray + network
    /// indicator + memory chip + account switcher on the right. The memory
    /// chip and account switcher mount with the backend/auth wiring (the
    /// React app hides the chip until the system query resolves too).
    fn top_bar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = Theme::get(cx);

        div()
            .id("top-bar")
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(theme.login_hero_border) // border-white/6
            .px_6()
            .py_3p5()
            .child(
                // Breadcrumb: 12px semibold; "ARCANE" bold white, "/" muted,
                // page name medium neutral-400.
                div()
                    .id("breadcrumb")
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(12.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(
                        div()
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.foreground)
                            .child("ARCANE"),
                    )
                    .child(div().font_weight(FontWeight::NORMAL).text_color(NEUTRAL_600).child("/"))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(NEUTRAL_400)
                            .child("HOME"),
                    ),
            )
            .child(
                div()
                    .id("top-bar-right")
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        // DownloadTray, idle state: bare 16px lucide icon.
                        div()
                            .id("download-tray")
                            .text_color(theme.muted)
                            .hover(|style| style.text_color(theme.foreground))
                            .child(icons::icon_at(icons::LUCIDE_DOWNLOAD, 16.0)),
                    )
                    .child(self.network_indicator(cx))
                    // Divider (`mx-1 h-5 w-px bg-white/8`).
                    .child(
                        div()
                            .mx_1()
                            .w(px(1.0))
                            .h(px(20.0))
                            .bg(theme::white(0.08)),
                    )
                    // Account switcher (hidden without a session — the view
                    // renders null, exactly like the React component).
                    .child(self.account.clone()),
            )
            .into_any_element()
    }

    /// `network-indicator.tsx`: 6px dot + 11px label. Pre-data the React
    /// app shows CONNECTING with a pulsing warning dot; the live state
    /// arrives with the backend wiring (the pulse is the only animation
    /// not reproduced yet).
    fn network_indicator(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = Theme::get(cx);

        div()
            .id("network-indicator")
            .flex()
            .items_center()
            .gap_1p5()
            .text_size(px(11.0))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.muted)
            .child(
                div()
                    .size(px(6.0))
                    .rounded_full()
                    .bg(theme.warning),
            )
            .child("CONNECTING")
            .into_any_element()
    }

    // -----------------------------------------------------------------
    // Window exit animations
    // -----------------------------------------------------------------

    fn start_exit(&mut self, kind: ExitKind, window: &mut Window, cx: &mut Context<Self>) {
        if self.exit.is_some() {
            return;
        }
        self.exit = Some(ExitAnim {
            kind,
            started: Instant::now(),
            fired: false,
        });
        self.tick_exit(window, cx);
    }

    /// Drives the exit animation: notifies the view every frame until the
    /// fade completes, then fires the real window action (and, for
    /// minimize, resets once the window is hidden).
    fn tick_exit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(exit) = &self.exit else { return };
        if exit.fired {
            return;
        }
        let kind = exit.kind;

        if exit.started.elapsed() < Duration::from_millis(WINDOW_EXIT_MS) {
            cx.notify();
            cx.spawn_in(window, async move |view, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                view.update_in(cx, |view, window, cx| {
                    if view.exit.as_ref().is_some_and(|exit| !exit.fired) {
                        view.tick_exit(window, cx);
                    }
                })
                .ok();
            })
            .detach();
            return;
        }

        // Animation complete: fire the real action.
        if let Some(exit) = &mut self.exit {
            exit.fired = true;
        }
        match kind {
            ExitKind::Minimize => {
                window.minimize_window();
                // Reset while the window is hidden; the next render after
                // restore shows the full content again.
                cx.spawn_in(window, async move |view, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(120))
                        .await;
                    view.update(cx, |view, cx| {
                        if view
                            .exit
                            .as_ref()
                            .is_some_and(|exit| exit.kind == ExitKind::Minimize && exit.fired)
                        {
                            view.exit = None;
                            cx.notify();
                        }
                    })
                    .ok();
                })
                .detach();
            }
            ExitKind::Close => cx.quit(),
        }
        cx.notify();
    }
}

impl Render for ShellView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.content(window, cx)
    }
}

/// A glass tooltip matching `ui/tooltip.tsx` (side=right variant):
/// surface background, hairline border, 8px radius, 12px medium text.
struct Tip {
    label: SharedString,
}

impl Render for Tip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx);
        div()
            .rounded(px(8.0))
            .px(px(10.0)) // px-2.5
            .py(px(6.0)) // py-1.5
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .text_size(px(12.0))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.foreground)
            .child(self.label.clone())
    }
}

/// Builds a tooltip view for the `.tooltip()` element API.
pub fn tooltip(label: &'static str, _window: &mut Window, cx: &mut App) -> gpui::AnyView {
    cx.new(|_| Tip {
        label: SharedString::from(label),
    })
    .into()
}
