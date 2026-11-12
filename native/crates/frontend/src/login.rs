//! The login screen — direct port of `components/auth/login-screen.tsx` +
//! `auth-panel.tsx`: a full-screen landing page (no sidebar/top bar) shown
//! until a session exists.
//!
//! Approximations (documented, gpui 0.2 limitations):
//! - the `login-bg.webm` looping video renders as its extracted first
//!   frame (`login-bg-still.webp`) at the same 16% opacity,
//! - the hero orbs don't drift (`animate-[drift_18s...]`),
//! - the 56px grid + `feTurbulence` noise textures are rasterized once at
//!   startup at the hero's initial size (the grid/noise are static CSS
//!   layers in the original too, so only the mask anchoring drifts on
//!   resize),
//! - `.accent-gradient-text` ("supercharged.") renders solid `--accent`
//!   (gpui has no background-clip: text) — the gradient is a 90° two-stop
//!   accent→accent-strong wash, so the mid-glyph hue differs slightly,
//! - the busy spinner doesn't rotate (`animate-spin`).

use std::sync::Arc;

use gpui::{
    App, Context, Entity, FontWeight, ObjectFit, RenderImage, SharedString, Window, div, img,
    prelude::*, px, relative,
};
use image::Frame;
use smallvec::smallvec;

use backend::models::minecraft::AuthSession;
use backend::services::minecraft::auth::LoginHost;

use crate::{
    bridge::{self, AuthStore},
    icons,
    login_host::WebViewLoginHost,
    text_input::TextField,
    theme::{self, Theme},
};

// Tailwind neutrals used by this screen.
const NEUTRAL_400: gpui::Rgba = theme::rgba_const(0xA1A1AAFF);
const NEUTRAL_500: gpui::Rgba = theme::rgba_const(0x737373FF);
const NEUTRAL_600: gpui::Rgba = theme::rgba_const(0x525252FF);

/// Tailwind `shadow-2xl`: `0 25px 50px -12px rgb(0 0 0 / 0.25)`.
const fn shadow_2xl() -> gpui::BoxShadow {
    gpui::BoxShadow {
        color: gpui::Hsla {
            h: 0.,
            s: 0.,
            l: 0.,
            a: 0.25,
        },
        offset: gpui::point(px(0.), px(25.)),
        blur_radius: px(50.),
        spread_radius: px(-12.),
    }
}

// ---------------------------------------------------------------------------
// Hero textures
// ---------------------------------------------------------------------------

/// The hero's `linear-gradient(to right/&, white 1px, transparent 1px)`
/// grid, 56px cells, at 6% opacity, with the
/// `mask-image: radial-gradient(ellipse at 30% 20%, black 0%, transparent
/// 70%)` mask baked in. CSS's default `farthest-corner` ellipse puts the
/// radii at (width − cx, height − cy).
fn grid_texture(width: u32, height: u32) -> Arc<RenderImage> {
    let mut buffer = image::RgbaImage::new(width, height);
    let cx = f64::from(width) * 0.30;
    let cy = f64::from(height) * 0.20;
    let rx = f64::from(width) - cx;
    let ry = f64::from(height) - cy;

    for (x, y, pixel) in buffer.enumerate_pixels_mut() {
        let on_line = (x % 56) < 1 || (y % 56) < 1;
        if !on_line {
            continue;
        }
        let dx = (f64::from(x) + 0.5 - cx) / rx;
        let dy = (f64::from(y) + 0.5 - cy) / ry;
        let d = (dx * dx + dy * dy).sqrt();
        let mask = (1.0 - d / 0.7).clamp(0.0, 1.0);
        let a = (255.0 * 0.06 * mask).round() as u8;
        *pixel = if a == 0 {
            image::Rgba([0, 0, 0, 0])
        } else {
            image::Rgba([255, 255, 255, a])
        };
    }

    Arc::new(RenderImage::new(smallvec![Frame::new(buffer)]))
}

/// The `feTurbulence` noise layer (`opacity: 0.35`, `mix-blend-overlay`).
/// Overlay-blend of a mid-biased gray noise ≈ a ±35% luminance modulation:
/// brighter noise → white pixels, darker → black, alpha = |g − 0.5| · 2 ·
/// 0.35. gpui has no blend modes, so this is painted directly.
fn noise_texture(width: u32, height: u32) -> Arc<RenderImage> {
    use std::cell::Cell;

    // xorshift — deterministic, no dependencies.
    let state = Cell::new(0x9E37_79B9_7F4A_7C15u64);
    let next = move || {
        let mut s = state.get();
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        state.set(s);
        s
    };

    let mut buffer = image::RgbaImage::new(width, height);
    for (_, _, pixel) in buffer.enumerate_pixels_mut() {
        let g = (next() % 1000) as f64 / 1000.0;
        let a = ((g - 0.5).abs() * 2.0 * 0.35 * 255.0).round() as u8;
        if g >= 0.5 {
            *pixel = image::Rgba([255, 255, 255, a]);
        } else {
            *pixel = image::Rgba([0, 0, 0, a]);
        }
    }

    Arc::new(RenderImage::new(smallvec![Frame::new(buffer)]))
}

// ---------------------------------------------------------------------------
// LoginScreen
// ---------------------------------------------------------------------------

/// The full-screen sign-in experience (`login-screen.tsx`).
pub struct LoginScreen {
    signin: Entity<SignInForm>,
    grid: Arc<RenderImage>,
    noise: Arc<RenderImage>,
    orb_far: crate::background::Orb,
    orb_near: crate::background::Orb,
}

impl LoginScreen {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Texture generation is anchored at the hero's initial size — the
        // left panel = the window minus the fixed 480px right column.
        let viewport = window.viewport_size();
        let hero_w = ((f32::from(viewport.width) - 480.0).max(1.0)) as u32;
        let hero_h = (f32::from(viewport.height).max(1.0)) as u32;

        let theme = *Theme::get(cx);
        let signin = cx.new(|cx| SignInForm::new(cx, Box::new(bridge::publish_session)));

        Self {
            signin,
            grid: grid_texture(hero_w, hero_h),
            noise: noise_texture(hero_w, hero_h),
            // `absolute -top-32 -left-24 h-120 w-120 bg-accent/20
            // blur-[150px]` (480px disc, σ = 75).
            orb_far: crate::background::glow_orb(480.0, 75.0, 0.20, theme.accent),
            // `absolute -bottom-40 right-0 h-96 w-96 bg-accent/10
            // blur-[130px]` (384px disc, σ = 65).
            orb_near: crate::background::glow_orb(384.0, 65.0, 0.10, theme.accent),
        }
    }

    /// The left hero panel — `HeroPanel`.
    fn hero(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = *Theme::get(cx);
        let scrim = crate::background::hsla_of(theme.login_hero, 0.70);

        div()
            .id("login-hero")
            .relative()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .border_r_1()
            .border_color(theme.login_hero_border) // border-white/6
            .bg(theme.login_hero)
            .px(px(56.0)) // px-14
            .py(px(40.0)) // py-10
            // Backdrop layers (painted before the content below).
            .child(
                img("login-bg-still.webp")
                    .absolute()
                    .inset_0()
                    .size_full()
                    .object_fit(ObjectFit::Cover)
                    .opacity(0.16),
            )
            .child(div().absolute().inset_0().bg(scrim))
            // Orbs (drift animation omitted — static).
            .child(
                div()
                    .absolute()
                    .top(px(-128.0)) // -top-32
                    .left(px(-96.0)) // -left-24
                    .size(px(self.orb_far.disc))
                    .child(self.orb_far.render()),
            )
            .child(
                div()
                    .absolute()
                    .bottom(px(-160.0)) // -bottom-40
                    .right_0()
                    .size(px(self.orb_near.disc))
                    .child(self.orb_near.render()),
            )
            .child(
                img(self.grid.clone())
                    .absolute()
                    .inset_0()
                    .size_full()
                    .object_fit(ObjectFit::Fill),
            )
            .child(
                img(self.noise.clone())
                    .absolute()
                    .inset_0()
                    .size_full()
                    .object_fit(ObjectFit::Fill),
            )
            // Brand row (`gap-3`, 44px mark, mono 12px extrabold ARCANE —
            // the 0.35em letter spacing has no gpui equivalent).
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        img("logo.png")
                            .size(px(44.0))
                            .rounded(px(16.0)) // rounded-2xl
                            .shadow(vec![shadow_2xl()]),
                    )
                    .child(
                        div()
                            .font_family(theme::FONT_FAMILY_MONO_FALLBACK)
                            .text_size(px(12.0))
                            .font_weight(FontWeight::EXTRA_BOLD)
                            .text_color(theme::rgba_const(0xFFFFFFFF))
                            .child("ARCANE"),
                    ),
            )
            // Centered copy block.
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .child(
                        div()
                            .w(px(448.0)) // max-w-md
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(41.6)) // text-[2.6rem]
                                    .line_height(relative(1.08))
                                    .font_weight(FontWeight::EXTRA_BOLD)
                                    .text_color(theme::rgba_const(0xFFFFFFFF))
                                    .child("Your Minecraft,")
                                    .child(
                                        div()
                                            // `.accent-gradient-text` — solid
                                            // accent (no text gradients in gpui).
                                            .text_color(theme.accent)
                                            .child("supercharged."),
                                    ),
                            )
                            .child(
                                div()
                                    .mt(px(16.0))
                                    .text_size(px(15.0))
                                    .line_height(relative(1.625)) // leading-relaxed
                                    .text_color(NEUTRAL_400)
                                    .child(
                                        "Manage mods, modpacks, shaders, resource packs, \
                                         versions, and instances from one powerful, \
                                         intelligent experience — built to keep your game \
                                         ready, online or offline.",
                                    ),
                            )
                            .child(self.features(theme)),
                    ),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(NEUTRAL_600)
                    .child("Arcane Launcher · v0.3.0"),
            )
            .into_any_element()
    }

    /// The three feature rows (`mt-9 flex flex-col gap-5`).
    fn features(&self, theme: Theme) -> gpui::AnyElement {
        let features: [(&'static str, &'static str, &'static str); 3] = [
            (
                icons::PUZZLE_PIECE,
                "Full mod manager",
                "Search, install and update Modrinth & CurseForge content without leaving the launcher.",
            ),
            (
                icons::STACK,
                "Any loader, any version",
                "Fabric, Forge, NeoForge and Quilt install and launch automatically — no manual setup.",
            ),
            (
                icons::GLOBE,
                "Built for offline play",
                "Instances, mods and worlds all work without a connection once they're installed.",
            ),
        ];

        let mut list = div().mt(px(36.0)).flex().flex_col().gap(px(20.0)); // mt-9 gap-5

        for (icon, title, description) in features {
            list = list.child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(14.0)) // gap-3.5
                    .child(
                        div()
                            .mt(px(2.0)) // mt-0.5
                            .flex_none()
                            .size(px(36.0)) // h-9 w-9
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(12.0)) // rounded-xl
                            .border_1()
                            .border_color(theme::white(0.08))
                            .bg(theme::white(0.05))
                            .child(icons::icon_at(icon, 16.0).text_color(theme.accent)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(14.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::rgba_const(0xFFFFFFFF))
                                    .child(title),
                            )
                            .child(
                                div()
                                    .mt(px(2.0))
                                    .text_size(px(13.0))
                                    .line_height(relative(1.625))
                                    .text_color(NEUTRAL_500)
                                    .child(description),
                            ),
                    ),
            );
        }

        list.into_any_element()
    }

    /// The right 480px column with the glass sign-in panel
    /// (`md:w-120 md:shrink-0 md:px-16`, panel `max-w-sm p-7 gap-5`).
    fn right_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = *Theme::get(cx);

        div()
            .id("login-right")
            .relative()
            .w(px(480.0)) // md:w-120
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .px(px(64.0)) // md:px-16
            .child(
                div()
                    .id("login-glass-panel")
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(20.0)) // gap-5
                    .p(px(28.0)) // p-7
                    .rounded(px(16.0)) // rounded-2xl (GlassPanel)
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .shadow(vec![shadow_2xl()])
                    .child(
                        div()
                            .child(
                                div()
                                    .text_size(px(18.0)) // text-lg
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::rgba_const(0xFFFFFFFF))
                                    .child("Welcome back"),
                            )
                            .child(
                                div()
                                    .mt(px(4.0))
                                    .text_size(px(12.0))
                                    .text_color(NEUTRAL_500)
                                    .child("Sign in to launch, manage instances and play."),
                            ),
                    )
                    .child(self.signin.clone()),
            )
            .into_any_element()
    }
}

impl Render for LoginScreen {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("login-screen")
            .size_full()
            .flex()
            .overflow_hidden()
            .bg(Theme::get(cx).background)
            .child(self.hero(cx))
            .child(self.right_panel(cx))
    }
}

// ---------------------------------------------------------------------------
// SignInForm — auth-panel.tsx
// ---------------------------------------------------------------------------

type OnSession = Box<dyn Fn(&mut App, AuthSession)>;

/// The sign-in form (Microsoft + offline username), used both by the
/// login screen and the account switcher's "Add account" modal.
pub struct SignInForm {
    busy: bool,
    error: Option<SharedString>,
    username: Entity<TextField>,
    on_session: OnSession,
}

impl SignInForm {
    pub fn new(cx: &mut Context<Self>, on_session: OnSession) -> Self {
        cx.observe_global::<AuthStore>(|_, cx| cx.notify()).detach();
        Self {
            busy: false,
            error: None,
            username: cx.new(|cx| TextField::new(cx, "Username")),
            on_session,
        }
    }

    /// Shared tail of both sign-in paths: completes the busy state and
    /// routes the result.
    fn finish(
        &mut self,
        result: Result<AuthSession, backend::error::AppError>,
        cx: &mut Context<Self>,
    ) {
        self.busy = false;
        cx.update_global(|store: &mut AuthStore, _| store.progress = None);
        match result {
            Ok(session) => {
                self.error = None;
                (self.on_session)(cx, session);
            }
            Err(err) => {
                self.error = Some(SharedString::from(err.message));
            }
        }
        cx.notify();
    }

    fn start_microsoft(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        cx.update_global(|store: &mut AuthStore, _| store.progress = None);
        cx.notify();

        let host: Arc<dyn LoginHost> = Arc::new(WebViewLoginHost);
        let task = bridge::login_microsoft(cx, host);
        cx.spawn(async move |view, cx| {
            let result = task.await.unwrap_or_else(|e| {
                Err(backend::error::AppError::new(
                    "Sign-in failed.",
                    e.to_string(),
                ))
            });
            view.update(cx, |view, cx| view.finish(result, cx)).ok();
        })
        .detach();
    }

    fn start_offline(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let username = self.username.read(cx).value().trim().to_string();
        if username.is_empty() {
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();

        let task = bridge::login_offline(cx, username);
        cx.spawn(async move |view, cx| {
            let result = task.await.unwrap_or_else(|e| {
                Err(backend::error::AppError::new(
                    "Couldn't start offline session.",
                    e.to_string(),
                ))
            });
            view.update(cx, |view, cx| view.finish(result, cx)).ok();
        })
        .detach();
    }
}

impl Render for SignInForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *Theme::get(cx);
        let progress = cx
            .try_global::<AuthStore>()
            .and_then(|store| store.progress.clone())
            .map(SharedString::from);

        let mut form = div().flex().flex_col().gap_4();

        // Error line (above everything, like the React conditional).
        if let Some(error) = &self.error {
            form = form.child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.danger)
                    .child(error.clone()),
            );
        }

        if self.busy {
            // `flex items-center gap-2 text-sm text-muted`: the spinner
            // (static — no rotate animation) + the live auth://progress
            // message.
            form = form.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(14.0))
                    .text_color(theme.muted)
                    .child(icons::icon_at(icons::ARROW_CLOCKWISE, 14.0))
                    .child(progress.unwrap_or_else(|| SharedString::from("Signing in..."))),
            );
        } else {
            // Microsoft button — `.btn-primary` at `lg` (h-12 px-6
            // rounded-xl, font-weight 600), full width.
            form = form.child(
                div()
                    .id("ms-sign-in")
                    .h(px(48.0)) // lg: h-12
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(12.0)) // rounded-xl
                    .text_size(px(14.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .bg(theme.accent)
                    .text_color(theme.accent_foreground)
                    .hover(move |style| style.bg(theme.accent_strong))
                    .on_click(cx.listener(|this, _, _, cx| this.start_microsoft(cx)))
                    .child("Sign in with Microsoft"),
            );

            // Divider — `or play offline` between two hairlines.
            form = form.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(12.0))
                    .text_color(theme.muted)
                    .child(div().flex_1().h(px(1.0)).bg(theme.border))
                    .child("or play offline")
                    .child(div().flex_1().h(px(1.0)).bg(theme.border)),
            );

            // Offline row: username field + ghost button.
            let username = self.username.read(cx).value().trim().is_empty();
            let enabled = !username;
            let field = self
                .username
                .update(cx, |field, cx| field.render_field(window, cx));

            form = form.child(
                div()
                    .flex()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(field))
                    .child(
                        div()
                            .id("offline-sign-in")
                            .flex_none()
                            .h(px(40.0)) // md: h-10
                            .px(px(16.0)) // md: px-4
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(12.0))
                            .text_size(px(14.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.foreground)
                            .border_1()
                            .border_color(theme.border)
                            .hover(move |style| {
                                if enabled {
                                    style
                                        .bg(theme.glass_hover)
                                        .border_color(theme.border_strong)
                                } else {
                                    style
                                }
                            })
                            .when(!enabled, |button| button.opacity(0.4))
                            .on_click(cx.listener(|this, _, _, cx| this.start_offline(cx)))
                            .child("Play offline"),
                    ),
            );
        }

        form
    }
}
