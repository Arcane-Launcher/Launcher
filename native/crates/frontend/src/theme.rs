//! Design tokens — direct port of `src/app/globals.css` (`:root`, obsidian
//! default) plus the self-hosted font families from `src/app/layout.tsx`.
//!
//! Every literal here is copied from the Tauri app; do not "improve" them.
//! Accent presets (17 via `data-accent`) and theme presets arrive in Phase 5
//! — the defaults below are the shipped look.

use std::borrow::Cow;

use gpui::{App, Global, Rgba, px};

// ---------------------------------------------------------------------------
// Color tokens (src/app/globals.css:5-24)
// ---------------------------------------------------------------------------

/// `0xRRGGBBAA` → `Rgba` in a const context (`gpui::rgba` is not `const fn`).
pub const fn rgba_const(hex: u32) -> Rgba {
    Rgba {
        r: ((hex >> 24) & 0xFF) as f32 / 255.0,
        g: ((hex >> 16) & 0xFF) as f32 / 255.0,
        b: ((hex >> 8) & 0xFF) as f32 / 255.0,
        a: (hex & 0xFF) as f32 / 255.0,
    }
}

/// `0xRRGGBB` + alpha percentage → `0xRRGGBBAA`, rounded the same way the
/// browser does (`alpha * 255` then round-half-up).
pub(crate) const fn tinted(base: u32, alpha: f64) -> u32 {
    base | (((alpha * 255. + 0.5) as u32) << 24)
}

/// `base` (0xRRGGBB) at `alpha` opacity — for one-off Tailwind `color/N`
/// classes the components use that aren't theme tokens
/// (e.g. `bg-white/7`, `text-danger/90`, `border-warning/30`).
pub(crate) const fn tinted_rgba(base: u32, alpha: f64) -> Rgba {
    rgba_const(tinted(base, alpha))
}

/// White at `alpha` opacity — Tailwind `white/N`.
pub const fn white(alpha: f64) -> Rgba {
    rgba_const(tinted(0xFFFFFF, alpha))
}

/// The accent red at `alpha` opacity — `rgba(239, 59, 77, alpha)`
/// (`--accent-rgb: 239, 59, 77`).
pub const fn accent_alpha(alpha: f64) -> Rgba {
    rgba_const(tinted(0xEF3B4D, alpha))
}

/// The default obsidian palette.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // Tokens land with their screens (login, settings, pages).
pub struct Theme {
    /// `--background` — app background.
    pub background: Rgba,
    /// `--surface`.
    pub surface: Rgba,
    /// `--surface-strong`.
    pub surface_strong: Rgba,
    /// `--foreground`.
    pub foreground: Rgba,
    /// `--muted`.
    pub muted: Rgba,
    /// `--border` — white at 7%.
    pub border: Rgba,
    /// `--border-strong` — white at 14%.
    pub border_strong: Rgba,
    /// `--glass` — white at 3.5%.
    pub glass: Rgba,
    /// `--glass-hover` — white at 6%.
    pub glass_hover: Rgba,
    /// `--accent`.
    pub accent: Rgba,
    /// `--accent-strong`.
    pub accent_strong: Rgba,
    /// `--accent-foreground`.
    pub accent_foreground: Rgba,
    /// `--danger`.
    pub danger: Rgba,
    /// `--success`.
    pub success: Rgba,
    /// `--warning`.
    pub warning: Rgba,
    /// `--overlay-tint` — tint of the overlay darkening the background image.
    pub overlay_tint: Rgba,
    /// Title bar background — `title-bar.tsx`: `bg-[#0a0a0c]`.
    pub titlebar: Rgba,
    /// Sidebar background — `sidebar.tsx`: `bg-[#0c0d12]/90`.
    pub sidebar: Rgba,
    /// Login hero panel — `login-screen.tsx`: `bg-[#0a0a0c]`.
    pub login_hero: Rgba,
    /// Login hero right border — `login-screen.tsx`: `border-white/6`.
    pub login_hero_border: Rgba,
}

impl Theme {
    pub const fn obsidian() -> Self {
        Self {
            background: rgba_const(0x070708FF),
            surface: rgba_const(0x141416FF),
            surface_strong: rgba_const(0x1A1A1DFF),
            foreground: rgba_const(0xF5F5F6FF),
            muted: rgba_const(0x96969DFF),
            border: white(0.07),
            border_strong: white(0.14),
            glass: white(0.035),
            glass_hover: white(0.06),
            accent: rgba_const(0xEF3B4DFF),
            accent_strong: rgba_const(0xFF5064FF),
            accent_foreground: rgba_const(0xFFFFFFFF),
            danger: rgba_const(0xE6455AFF),
            success: rgba_const(0x4ADE80FF),
            warning: rgba_const(0xF2C14EFF),
            overlay_tint: rgba_const(0x000000FF),
            titlebar: rgba_const(0x0A0A0CFF),
            sidebar: rgba_const(tinted(0x0C0D12, 0.90)),
            login_hero: rgba_const(0x0A0A0CFF),
            login_hero_border: white(0.06),
        }
    }
}

impl Global for Theme {}

impl Theme {
    /// The active theme. Phase 5 adds accent/theme preset switching; the
    /// default is the obsidian + red accent look shipped by the Tauri app.
    pub fn get(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    /// Mirrors the obsidian palette into gpui-component's own `Theme`
    /// global — the styled widgets (Switch, Slider, and the future
    /// selects/comboboxes) read that global, not this module's tokens, and
    /// `gpui::init` leaves them on a Light-mode default.
    ///
    /// Two steps, per the library's own doc: switching the mode inside the
    /// same `update` would load the registered dark palette *after* the
    /// closure's edits and overwrite them.
    pub fn sync_component_theme(cx: &mut App) {
        use gpui::component::theme::{Theme as ComponentTheme, ThemeMode};

        ComponentTheme::change(ThemeMode::Dark, None, cx);
        ComponentTheme::update(cx, |theme| {
            let t = Self::obsidian();
            let c = &mut theme.colors;
            // shadcn semantics: `primary` is the brand color (the switch's
            // checked fill), `accent` is the subtle hover tint.
            c.background = t.background.into();
            c.foreground = t.foreground.into();
            c.muted = t.glass_hover.into();
            c.muted_foreground = t.muted.into();
            c.accent = t.glass_hover.into();
            c.accent_foreground = t.foreground.into();
            c.primary = t.accent.into();
            c.primary_foreground = t.accent_foreground.into();
            c.secondary = white(0.08).into();
            c.secondary_foreground = t.foreground.into();
            c.border = t.border.into();
            c.input = t.glass.into();
            c.ring = t.accent.into();
            c.popover = t.surface.into();
            c.popover_foreground = t.foreground.into();
            c.danger = t.danger.into();
            c.success = t.success.into();
            c.link = t.accent.into();
            // The dialogs' shared geometry (`rounded-lg` inputs, glass
            // panels at `rounded-2xl` set per-element).
            theme.radius = px(8.0);
        });
    }
}

// ---------------------------------------------------------------------------
// Fonts (src/app/layout.tsx @fontsource imports + src/lib/font-preset.ts)
// ---------------------------------------------------------------------------

/// `--font-family` default: `"Sora", ui-sans-serif, system-ui, sans-serif`.
pub const FONT_FAMILY: &str = "Sora";
/// Mono family — JetBrains Mono, used only when the mono font preset is
/// active (font preset switching is Phase 5 settings scope).
#[allow(dead_code)]
pub const FONT_FAMILY_MONO: &str = "JetBrains Mono";
/// Tailwind's default `font-mono` stack resolves to the system mono font —
/// Cascadia Mono on Windows 10+ (verified installed), Consolas on older
/// systems. Used by the sidebar "ARC" mark and the top-bar memory chip.
pub const FONT_FAMILY_MONO_FALLBACK: &str = "Cascadia Mono";

/// Google-Fonts family slug per bundled TTF stem (see `build.rs`).
const FONT_FILES: &[(&str, &str)] = &[
    ("sora", "Sora"),
    ("poppins", "Poppins"),
    ("inter", "Inter"),
    ("manrope", "Manrope"),
    ("outfit", "Outfit"),
    ("jetbrains-mono", "JetBrains Mono"),
];

/// Registers every self-hosted family (weights 400/500/600/700) with the
/// text system. The TTFs are fetched at build time by `build.rs` into the
/// gitignored `assets/fonts/` directory and embedded into the binary, so
/// the app renders identically offline and never touches Google Fonts at
/// runtime.
pub fn register_fonts(cx: &mut App) {
    macro_rules! embedded {
        ($stem:literal, $weight:literal) => {
            Cow::Borrowed(
                include_bytes!(concat!("../assets/fonts/", $stem, "-", $weight, ".ttf")).as_slice(),
            )
        };
    }

    let families = [
        embedded!("sora", 400),
        embedded!("sora", 500),
        embedded!("sora", 600),
        embedded!("sora", 700),
        embedded!("poppins", 400),
        embedded!("poppins", 500),
        embedded!("poppins", 600),
        embedded!("poppins", 700),
        embedded!("inter", 400),
        embedded!("inter", 500),
        embedded!("inter", 600),
        embedded!("inter", 700),
        embedded!("manrope", 400),
        embedded!("manrope", 500),
        embedded!("manrope", 600),
        embedded!("manrope", 700),
        embedded!("outfit", 400),
        embedded!("outfit", 500),
        embedded!("outfit", 600),
        embedded!("outfit", 700),
        embedded!("jetbrains-mono", 400),
        embedded!("jetbrains-mono", 500),
        embedded!("jetbrains-mono", 600),
        embedded!("jetbrains-mono", 700),
    ];

    cx.text_system()
        .add_fonts(families.to_vec())
        .expect("failed to register the embedded UI fonts");

    // Sanity check: every family must resolve, otherwise DirectWrite would
    // silently fall back to the system UI font and break pixel fidelity.
    let registered = cx.text_system().all_font_names();
    for (_, family) in FONT_FILES {
        if !registered.iter().any(|name| name == *family) {
            panic!("font family '{family}' did not register with the text system");
        }
    }
}
