//! Toast notifications — direct port of `store/toast-store.ts` +
//! `layout/toaster.tsx` (the bottom-right stack).
//!
//! The zustand store becomes a gpui global; the 4s auto-dismiss becomes a
//! scheduled task per toast.

use gpui::{
    div, prelude::*, px, relative, App, Global, Hsla, SharedString, Window,
};

use crate::{icons, theme::Theme};

const AUTO_DISMISS_MS: u64 = 4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastVariant {
    Success,
    Error,
    /// Reserved for the info toasts other screens push as they're ported.
    #[allow(dead_code)]
    Info,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub id: u64,
    pub message: SharedString,
    pub variant: ToastVariant,
}

#[derive(Default)]
pub struct ToastStore {
    toasts: Vec<Toast>,
    next_id: u64,
}

impl Global for ToastStore {}

impl ToastStore {
    fn push(&mut self, message: impl Into<SharedString>, variant: ToastVariant) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.toasts.push(Toast {
            id,
            message: message.into(),
            variant,
        });
        id
    }

    fn dismiss(&mut self, id: u64) {
        self.toasts.retain(|t| t.id != id);
    }

    pub fn toasts(&self) -> &[Toast] {
        &self.toasts
    }
}

/// `toast.success/error/info` from the store — pushes and schedules the
/// auto-dismiss.
pub fn toast(cx: &mut App, variant: ToastVariant, message: impl Into<SharedString>) {
    let id = cx.update_global(|store: &mut ToastStore, _| store.push(message, variant));

    let task = cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(std::time::Duration::from_millis(AUTO_DISMISS_MS))
            .await;
        cx.update(|cx| {
            cx.update_global(|store: &mut ToastStore, _| store.dismiss(id));
        })
        .ok();
    });
    task.detach();
}

pub fn toast_success(cx: &mut App, message: impl Into<SharedString>) {
    toast(cx, ToastVariant::Success, message);
}

pub fn toast_error(cx: &mut App, message: impl Into<SharedString>) {
    toast(cx, ToastVariant::Error, message);
}

/// The bottom-right stack — `toaster.tsx`: `fixed bottom-6 right-6 z-100
/// flex flex-col gap-2`, each toast a 320px glass panel with a 17px variant
/// icon and a 13px dismiss button.
pub struct Toaster;

impl Render for Toaster {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let store = cx.global::<ToastStore>();
        if store.toasts().is_empty() {
            return div().into_any_element();
        }
        let toasts: Vec<Toast> = store.toasts().to_vec();

        let theme = *Theme::get(cx);

        let mut stack = div()
            .absolute()
            .bottom(px(24.0)) // bottom-6
            .right(px(24.0)) // right-6
            .flex()
            .flex_col()
            .gap_2();

        for t in toasts {
            let (icon_asset, icon_color) = match t.variant {
                ToastVariant::Success => (icons::CHECK_CIRCLE, theme.success),
                ToastVariant::Error => (icons::WARNING_CIRCLE, theme.danger),
                ToastVariant::Info => (icons::WARNING_CIRCLE, theme.accent),
            };
            let id = t.id;
            stack = stack.child(
                div()
                    .id(SharedString::from(format!("toast-{id}")))
                    .w(px(320.0)) // w-80
                    .flex()
                    .items_start()
                    .gap_2p5() // gap-2.5
                    .rounded(px(12.0)) // rounded-xl
                    .p(px(14.0)) // p-3.5
                    .bg(theme.surface) // glass-panel
                    .border_1()
                    .border_color(theme.border)
                    .shadow(vec![gpui::BoxShadow {
                        // Tailwind `shadow-xl`
                        color: Hsla::from(crate::theme::rgba_const(0x00000040)),
                        offset: gpui::point(px(0.), px(25.)),
                        blur_radius: px(50.),
                        spread_radius: px(-12.),
                    }])
                    // Icon column (mt-0.5 = 2px, shrink-0).
                    .child(
                        div()
                            .mt(px(2.0))
                            .flex_none()
                            .child(icons::icon_at(icon_asset, 17.0).text_color(icon_color)),
                    )
                    // Message.
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(14.0)) // text-sm
                            .line_height(relative(1.375)) // leading-snug
                            .text_color(theme.foreground)
                            .child(t.message.clone()),
                    )
                    // Dismiss (text-muted hover:text-foreground, x 13px).
                    .child(
                        div()
                            .id(SharedString::from(format!("toast-dismiss-{id}")))
                            .flex_none()
                            .text_color(theme.muted)
                            .hover(move |style| style.text_color(theme.foreground))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.update_global(|store: &mut ToastStore, _| store.dismiss(id));
                                cx.notify();
                            }))
                            .child(icons::icon_at(icons::X, 13.0)),
                    ),
            );
        }

        stack.into_any_element()
    }
}
