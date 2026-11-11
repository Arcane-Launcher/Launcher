//! The signed-in background stack — direct port of the layer stack in
//! `app-shell.tsx` (the `absolute inset-0 z-0` div):
//!
//! 1. the background photo (`bg.webp`, cover/center; a user's custom
//!    background or a seasonal theme overrides it — Phase 5),
//! 2. the black overlay tint at `--overlay-tint` with the
//!    `background_overlay_opacity` setting (default 70%),
//! 3. two accent glow orbs — CSS `bg-accent/10 blur-[140px]` /
//!    `bg-accent/5 blur-[120px]` circles,
//! 4. a bottom fade from transparent to the background color.
//!
//! gpui has no CSS-style gaussian blur for divs, so the two glow orbs are
//! rasterized at startup into textures that sample the exact same math:
//! a blurred disc's alpha at distance `d` from the center of a circle of
//! radius `R` under gaussian blur with σ is `A · Φ((R − d)/σ)` (Φ = the
//! normal CDF; CSS `blur(N)` uses σ = N/2). Each texture is positioned by
//! first laying out its *unblurred* disc box at the CSS offset
//! (`-top-40 right-0 h-128 w-lg` / `bottom-0 left-1/3 h-96 w-96`), then
//! letting the texture overhang the box symmetrically by the blur margin.

use image::Frame;
use smallvec::smallvec;
use std::sync::Arc;

use gpui::{
    div, img, prelude::*, px, relative, ColorSpace, Hsla, ObjectFit, Render, RenderImage, Rgba,
    linear_gradient, linear_color_stop,
};


use crate::theme::Theme;

/// rgba → hsla with the alpha replaced.
fn hsla_of(color: Rgba, alpha: f32) -> Hsla {
    let hsla = Hsla::from(color);
    Hsla {
        a: alpha.clamp(0.0, 1.0),
        ..hsla
    }
}

/// Abramowitz & Stegun 7.1.26 — |error| < 1.5e-7, plenty for a glow.
fn erf(x: f64) -> f64 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let poly = t * (0.254_829_592
        + t * (-0.284_496_736
            + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    sign * (1.0 - poly * (-x * x).exp())
}

/// Normal CDF.
fn phi(z: f64) -> f64 {
    0.5 * (1.0 + erf(z / std::f64::consts::SQRT_2))
}

/// A gaussian-blurred accent disc, pre-rasterized: CSS
/// `w-{disc}px h-{disc}px rounded-full bg-accent/α blur-[2σ px]`.
struct Orb {
    image: Arc<RenderImage>,
    /// The unblurred disc's size (the CSS box size).
    disc: f32,
    /// How far the texture overhangs the disc box on every side (3σ).
    margin: f32,
}

fn glow_orb(disc: f32, sigma: f32, alpha: f64, accent: Rgba) -> Orb {
    let margin = (3.0 * sigma).ceil();
    let texture = (disc + 2.0 * margin) as u32;
    let radius = f64::from(disc) / 2.0;
    let sigma = f64::from(sigma);

    let packed = u32::from(accent);
    let r = ((packed >> 24) & 0xFF) as u8;
    let g = ((packed >> 16) & 0xFF) as u8;
    let b = ((packed >> 8) & 0xFF) as u8;
    let peak = alpha as f32;

    let mut buffer = image::RgbaImage::new(texture, texture);
    let center = f64::from(texture) / 2.0;
    for (x, y, pixel) in buffer.enumerate_pixels_mut() {
        let dx = x as f64 + 0.5 - center;
        let dy = y as f64 + 0.5 - center;
        let coverage = phi((radius - dx.hypot(dy)) / sigma);
        let a = (peak * coverage as f32 * 255.0).round() as u8;
        *pixel = if a == 0 {
            image::Rgba([0, 0, 0, 0])
        } else {
            image::Rgba([r, g, b, a])
        };
    }

    Orb {
        image: Arc::new(RenderImage::new(smallvec![Frame::new(buffer)])),
        disc,
        margin,
    }
}

impl Orb {
    /// The disc box laid out at the CSS offset, with the texture centered
    /// on it.
    fn render(&self) -> gpui::Img {
        img(self.image.clone())
            .absolute()
            .left(px(-self.margin))
            .top(px(-self.margin))
            .w(px(self.disc + 2.0 * self.margin))
            .h(px(self.disc + 2.0 * self.margin))
            .object_fit(ObjectFit::Fill)
    }
}

pub struct BackgroundStack {
    /// `settings.background_overlay_opacity ?? 70`, as a 0..1 fraction.
    overlay_opacity: f32,
    /// `settings.background_disabled`.
    background_disabled: bool,
    /// `absolute -top-40 right-0 h-128 w-lg rounded-full bg-accent/10
    /// blur-[140px]` — a 512px disc, σ = 70.
    orb_far: Orb,
    /// `bottom-0 left-1/3 h-96 w-96 rounded-full bg-accent/5 blur-[120px]`
    /// — a 384px disc, σ = 60.
    orb_near: Orb,
}

impl BackgroundStack {
    pub fn new(overlay_opacity: f32, background_disabled: bool) -> Self {
        let theme = Theme::obsidian();
        Self {
            overlay_opacity: overlay_opacity.clamp(0.0, 1.0),
            background_disabled,
            orb_far: glow_orb(512.0, 70.0, 0.10, theme.accent),
            orb_near: glow_orb(384.0, 60.0, 0.05, theme.accent),
        }
    }
}

impl Render for BackgroundStack {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx);

        div()
            .absolute()
            .inset_0()
            .overflow_hidden()
            .when(!self.background_disabled, |stack| {
                stack
                    // The background photo, cover/center.
                    .child(
                        img("bg.webp")
                            .absolute()
                            .inset_0()
                            .size_full()
                            .object_fit(ObjectFit::Cover),
                    )
                    // The black overlay tint.
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .bg(theme.overlay_tint)
                            .opacity(self.overlay_opacity),
                    )
            })
            // Glow orbs render even when the photo is disabled, exactly
            // like the Tauri app.
            .child(
                // Disc box: `-top-40 right-0 h-128 w-lg` (top: -160px, 512px).
                div()
                    .absolute()
                    .top(px(-160.0))
                    .right_0()
                    .size(px(self.orb_far.disc))
                    .child(self.orb_far.render()),
            )
            .child(
                // Disc box: `bottom-0 left-1/3 h-96 w-96` (384px).
                div()
                    .absolute()
                    .bottom_0()
                    .left(relative(1.0 / 3.0))
                    .size(px(self.orb_near.disc))
                    .child(self.orb_near.render()),
            )
            // Bottom fade: `bg-linear-to-b from-transparent via-transparent
            // to-background` — transparent until halfway, then the
            // background color (the via stop is transparent too, so the
            // two-stop form is identical).
            .child(
                div().absolute().inset_0().bg(linear_gradient(
                    180.0,
                    linear_color_stop(hsla_of(theme.background, 0.0), 0.0),
                    linear_color_stop(theme.background, 1.0),
                )),
            )
    }
}

// Keep the sRGB color-space choice explicit for the gradient (the browser
// interpolates sRGB by default here).
const _: ColorSpace = ColorSpace::Srgb;
