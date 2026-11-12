//! Shared widget builders — direct ports of `ui/button.tsx` (sizes,
//! `btn-primary`/`btn-ghost` variants, the `danger` variant's own classes)
//! plus the `glass-panel` / `glass-card` utility classes from
//! `globals.css`, and the `fixed inset-0 bg-black/60 p-4` modal overlay
//! the dialogs share.

use gpui::{Div, FontWeight, SharedString, Stateful, div, prelude::*, px};

use crate::theme::{self, Theme};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonVariant {
    /// `btn-primary` — accent fill, semibold.
    Primary,
    /// `btn-ghost` — hairline border, transparent fill.
    Ghost,
    /// `bg-danger/15 text-danger border border-danger/30 hover:bg-danger/25`.
    Danger,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonSize {
    /// `h-8 px-3 text-xs rounded-lg`.
    Sm,
    /// `h-10 px-4 text-sm rounded-xl`.
    Md,
    /// `h-12 px-6 text-sm rounded-xl`.
    Lg,
}

/// The `sizeClasses` + `variantClasses` tables from `ui/button.tsx` —
/// a styled (non-interactive until the caller adds `.on_click`) button
/// shell; callers add their own label/icon children. A `disabled` button
/// matches React's `disabled` (`pointer-events-none`, no hover) plus the
/// primary variant's `disabled:opacity-40`.
pub fn button(
    id: impl Into<SharedString>,
    theme: Theme,
    variant: ButtonVariant,
    size: ButtonSize,
    disabled: bool,
) -> Stateful<Div> {
    let (height, pad_x, text, radius) = match size {
        ButtonSize::Sm => (32.0, 12.0, 12.0, 8.0),
        ButtonSize::Md => (40.0, 16.0, 14.0, 12.0),
        ButtonSize::Lg => (48.0, 24.0, 14.0, 12.0),
    };

    let mut el = div()
        .id(id.into())
        .h(px(height))
        .px(px(pad_x))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap_2() // gap-2 from the base classes
        .text_size(px(text))
        .font_weight(FontWeight::MEDIUM)
        .rounded(px(radius));

    match variant {
        ButtonVariant::Primary => {
            el = el
                .bg(theme.accent)
                .text_color(theme.accent_foreground)
                .font_weight(FontWeight::SEMIBOLD);
            if disabled {
                el = el.opacity(0.4); // .btn-primary:disabled
            } else {
                el = el.hover(move |style| style.bg(theme.accent_strong));
            }
        }
        ButtonVariant::Ghost => {
            el = el
                .bg(theme::rgba_const(0x00000000))
                .border_1()
                .border_color(theme.border)
                .text_color(theme.foreground);
            if !disabled {
                el = el.hover(move |style| {
                    style
                        .bg(theme.glass_hover)
                        .border_color(theme.border_strong)
                });
            }
        }
        ButtonVariant::Danger => {
            el = el
                .bg(theme::tinted_rgba(0xE6455A, 0.15)) // --danger/15
                .border_1()
                .border_color(theme::tinted_rgba(0xE6455A, 0.30))
                .text_color(theme.danger);
            if !disabled {
                el = el.hover(move |style| style.bg(theme::tinted_rgba(0xE6455A, 0.25)));
            }
        }
    }

    el
}

/// `.glass-panel { background: var(--surface); border: 1px solid var(--border); }`
/// (the webview's `backdrop-filter` blur has nothing behind the panels to
/// blur through that isn't already flattened into `--surface`).
pub fn glass_panel(theme: Theme) -> Div {
    div()
        .bg(theme.surface)
        .border_1()
        .border_color(theme.border)
}

/// `.glass-card` — glass-panel plus the hover lift
/// (`:hover { background: var(--surface-strong); border-color: var(--border-strong); }`).
pub fn glass_card(theme: Theme) -> Div {
    div()
        .bg(theme.surface)
        .border_1()
        .border_color(theme.border)
        .hover(move |style| {
            style
                .bg(theme.surface_strong)
                .border_color(theme.border_strong)
        })
}

/// `fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4` —
/// the shared modal backdrop every dialog renders against. gpui has no
/// `position: fixed`, so dialogs are rendered off the shell root instead
/// (see `shell.rs`); `.occlude()` keeps events from reaching the page
/// underneath. The `id` keeps element ids unique when two dialogs are
/// open at once (console + validation is a real combination).
pub fn modal_backdrop(id: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme::rgba_const(0x00000099)) // bg-black/60
        .p(px(16.0))
        .occlude()
}
