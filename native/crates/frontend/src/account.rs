//! The account switcher — direct port of `components/layout/account-switcher.tsx`.
//!
//! The avatar button lives in the top bar; its dropdown anchors to it.
//! The "Add account" modal is rendered by the shell (see `ShellView`)
//! because gpui has no `position: fixed` — an overlay can only cover the
//! subtree it's a child of, so it must hang off the root element.
//!
//! Skins are Phase 5: until the skin library is wired, Microsoft avatars
//! come straight from mc-heads.net and offline accounts show the initial
//! letter (`SkinFaceIcon` support lands with the skin screen).

use gpui::{Context, Entity, FontWeight, SharedString, Window, div, img, prelude::*, px, relative};

use backend::{
    commands,
    models::minecraft::{AuthSession, SessionType},
};

use crate::{
    bridge::{self, AuthStore, Backend},
    icons,
    login::SignInForm,
    theme::{self, Theme},
};

const NEUTRAL_400: gpui::Rgba = theme::rgba_const(0xA1A1AAFF);
const EMERALD_500: gpui::Rgba = theme::rgba_const(0x10B981FF);
/// The green dot's border color — `border-[#0e1015]` in the React source.
const DOT_BORDER: gpui::Rgba = theme::rgba_const(0x0E1015FF);

/// Tailwind `shadow-md`: two layers.
fn shadow_md() -> Vec<gpui::BoxShadow> {
    vec![
        gpui::BoxShadow {
            color: gpui::Hsla {
                a: 0.1,
                ..gpui::Hsla::from(theme::rgba_const(0x000000FF))
            },
            offset: gpui::point(px(0.), px(4.)),
            blur_radius: px(6.),
            spread_radius: px(-1.),
            inset: false,
        },
        gpui::BoxShadow {
            color: gpui::Hsla {
                a: 0.1,
                ..gpui::Hsla::from(theme::rgba_const(0x000000FF))
            },
            offset: gpui::point(px(0.), px(2.)),
            blur_radius: px(4.),
            spread_radius: px(-2.),
            inset: false,
        },
    ]
}

/// Tailwind `shadow-2xl`: `0 25px 50px -12px rgb(0 0 0 / 0.25)`.
fn shadow_2xl() -> Vec<gpui::BoxShadow> {
    vec![gpui::BoxShadow {
        color: gpui::Hsla {
            a: 0.25,
            ..gpui::Hsla::from(theme::rgba_const(0x000000FF))
        },
        offset: gpui::point(px(0.), px(25.)),
        blur_radius: px(50.),
        spread_radius: px(-12.),
        inset: false,
    }]
}

/// `AccountAvatar`: mc-heads face for Microsoft sessions, initial letter
/// otherwise (`bg-accent/20 text-accent`).
fn avatar(session: &AuthSession, size: f32, theme: &Theme) -> gpui::AnyElement {
    let base = || {
        div()
            .size(px(size))
            .flex_none()
            .rounded_full()
            .border_1()
            .border_color(theme::white(0.20))
            .shadow(shadow_md())
    };

    match session.session_type {
        SessionType::Microsoft => base()
            .child(
                img(SharedString::from(format!(
                    "https://mc-heads.net/avatar/{}/64",
                    session.profile.id
                )))
                .size(px(size))
                .rounded_full()
                .object_fit(gpui::ObjectFit::Cover),
            )
            .into_any_element(),
        SessionType::Offline => base()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme::accent_alpha(0.20))
            .text_size(px(14.0)) // text-sm
            .font_weight(FontWeight::BOLD)
            .text_color(theme.accent)
            .child(
                session
                    .profile
                    .name
                    .chars()
                    .next()
                    .unwrap_or('?')
                    .to_uppercase()
                    .to_string(),
            )
            .into_any_element(),
    }
}

fn type_label(session: &AuthSession, long: bool) -> &'static str {
    match (session.session_type, long) {
        (SessionType::Microsoft, true) => "Microsoft account",
        (SessionType::Microsoft, false) => "Microsoft",
        (SessionType::Offline, true) => "Offline session",
        (SessionType::Offline, false) => "Offline",
    }
}

pub struct AccountSwitcher {
    open: bool,
    adding: bool,
    /// The modal's form — created fresh when "Add account" opens, torn
    /// down when it closes (matching the React mount/unmount).
    signin: Option<Entity<SignInForm>>,
}

impl AccountSwitcher {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.observe_global::<AuthStore>(|_, cx| cx.notify()).detach();
        Self {
            open: false,
            adding: false,
            signin: None,
        }
    }

    /// True while the "Add account" modal is up (the shell reads this to
    /// render the overlay).
    pub fn is_adding(&self) -> bool {
        self.adding
    }

    /// The modal's sign-in form entity, if one exists.
    pub fn modal_form(&self) -> Option<Entity<SignInForm>> {
        self.signin.clone()
    }

    /// Closes the modal (the shell's close button routes here).
    pub fn cancel_add(&mut self, cx: &mut Context<Self>) {
        self.adding = false;
        self.signin = None;
        cx.notify();
    }

    fn start_add(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        if self.signin.is_none() {
            let weak = cx.weak_entity();
            self.signin = Some(cx.new(|cx| {
                SignInForm::new(
                    cx,
                    Box::new(move |cx, session| {
                        bridge::publish_session(cx, session.clone());
                        crate::toasts::toast_success(
                            cx,
                            format!("Signed in as {}.", session.profile.name),
                        );
                        let _ = weak.update(cx, |switcher, cx| {
                            switcher.adding = false;
                            switcher.signin = None;
                            cx.notify();
                        });
                    }),
                )
            }));
        }
        self.adding = true;
        cx.notify();
    }

    fn switch(&mut self, profile_id: String, cx: &mut Context<Self>) {
        let state = cx.global::<Backend>().state.clone();
        let task = cx
            .global::<Backend>()
            .run(async move { commands::auth::switch_account(&state, profile_id).await });

        cx.spawn(async move |view, cx| {
            let result = task.await.unwrap_or_else(|e| {
                Err(backend::error::AppError::new(
                    "Couldn't switch accounts.",
                    e.to_string(),
                ))
            });
            view.update(cx, |view, cx| match result {
                Ok(session) => {
                    bridge::publish_session(cx, session.clone());
                    crate::toasts::toast_success(
                        cx,
                        format!("Switched to {}.", session.profile.name),
                    );
                    view.open = false;
                    cx.notify();
                }
                Err(_) => {
                    crate::toasts::toast_error(cx, "Couldn't switch accounts.");
                }
            })
            .ok();
        })
        .detach();
    }

    fn remove(&mut self, profile_id: String, cx: &mut Context<Self>) {
        let state = cx.global::<Backend>().state.clone();
        let task = cx
            .global::<Backend>()
            .run(async move { commands::auth::remove_account(&state, profile_id).await });

        cx.spawn(async move |_view, cx| {
            if let Ok(Ok(accounts)) = task.await {
                cx.update(|cx| {
                    cx.update_global(|store: &mut AuthStore, _| store.accounts = accounts);
                });
                // The removed account may have been the active one.
                cx.update(bridge::refresh_session);
            }
        })
        .detach();
    }

    fn sign_out(&mut self, cx: &mut Context<Self>) {
        let state = cx.global::<Backend>().state.clone();
        let task = cx
            .global::<Backend>()
            .run(async move { commands::auth::logout(&state).await });

        cx.spawn(async move |view, cx| {
            if let Ok(Ok(())) = task.await {
                cx.update(|cx| {
                    cx.update_global(|store: &mut AuthStore, _| {
                        store.gate = bridge::Gate::LoggedOut;
                        store.progress = None;
                    });
                });
                view.update(cx, |view, cx| {
                    view.open = false;
                    view.adding = false;
                    view.signin = None;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }
}

impl Render for AccountSwitcher {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let store = cx.global::<AuthStore>();
        let Some(session) = store.session().cloned() else {
            // React returns null without a session.
            return div().into_any_element();
        };
        let accounts = store.accounts.clone();

        let theme = *Theme::get(cx);

        // Click-anywhere-outside closes the dropdown: a raw window-level
        // mouse listener (gpui has no "click outside" primitive). Runs in
        // the bubble phase, after any row's own click handler, so actions
        // still land.
        if self.open {
            let weak = cx.weak_entity();
            window.on_mouse_event(move |_event: &gpui::MouseDownEvent, phase, _window, cx| {
                if phase == gpui::DispatchPhase::Bubble {
                    let _ = weak.update(cx, |switcher, cx| {
                        if switcher.open {
                            switcher.open = false;
                            cx.notify();
                        }
                    });
                }
            });
        }

        let mut root = div()
            .id("account-switcher")
            .relative()
            .flex()
            .items_center()
            // The avatar button (`hover:opacity-90`).
            .child(
                div()
                    .id("account-button")
                    .flex()
                    .items_center()
                    .gap_3() // space-x-3
                    .rounded_full()
                    .hover(|style| style.opacity(0.9))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.open = !this.open;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .text_right()
                            .line_height(relative(1.25)) // leading-tight
                            .child(
                                div()
                                    .text_size(px(14.0))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme::rgba_const(0xFFFFFFFF))
                                    .child(session.profile.name.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(NEUTRAL_400)
                                    .child(type_label(&session, true)),
                            ),
                    )
                    .child(
                        div()
                            .relative()
                            .child(avatar(&session, 40.0, &theme))
                            // The active-account dot.
                            .child(
                                div()
                                    .absolute()
                                    .bottom_0()
                                    .right_0()
                                    .size(px(10.0)) // h-2.5 w-2.5
                                    .rounded_full()
                                    .border_2()
                                    .border_color(DOT_BORDER)
                                    .bg(EMERALD_500),
                            ),
                    ),
            );

        if self.open {
            let others: Vec<AuthSession> = accounts
                .iter()
                .filter(|a| a.profile.id != session.profile.id)
                .cloned()
                .collect();

            let mut dropdown = div()
                .id("account-dropdown")
                .absolute()
                .right_0()
                .top(relative(1.0)) // top-full
                .mt_2()
                .w(px(288.0)) // w-72
                .flex()
                .flex_col()
                .occlude()
                .p_2()
                .rounded(px(16.0)) // rounded-2xl (glass-panel)
                .bg(theme.surface)
                .border_1()
                .border_color(theme.border)
                .shadow(shadow_2xl());

            // The active account row.
            dropdown = dropdown.child(
                div()
                    .id("account-active")
                    .flex()
                    .items_center()
                    .gap_3()
                    .rounded(px(12.0)) // rounded-xl
                    .p(px(10.0)) // p-2.5
                    .child(avatar(&session, 36.0, &theme))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(14.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .overflow_hidden()
                                    .child(session.profile.name.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(theme.muted)
                                    .child(type_label(&session, true)),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.accent)
                            .child("Active"),
                    ),
            );

            if !others.is_empty() {
                let mut section = div()
                    .mt_1()
                    .flex()
                    .flex_col()
                    .gap(px(2.0)) // gap-0.5
                    .border_t_1()
                    .border_color(theme::white(0.06))
                    .pt_1();

                for account in others {
                    let profile_id = account.profile.id.clone();
                    let trash_id = account.profile.id.clone();
                    section = section.child(
                        div()
                            .id(SharedString::from(format!(
                                "account-{}",
                                account.profile.id
                            )))
                            .flex()
                            .items_center()
                            .gap_3()
                            .rounded(px(12.0))
                            .p(px(10.0))
                            .hover(|style| style.bg(theme::white(0.05)))
                            .child(
                                div()
                                    .id(SharedString::from(format!(
                                        "account-switch-{}",
                                        account.profile.id
                                    )))
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.switch(profile_id.clone(), cx);
                                    }))
                                    .child(avatar(&account, 32.0, &theme))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .child(
                                                div()
                                                    .text_size(px(12.0))
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(theme.foreground)
                                                    .overflow_hidden()
                                                    .child(account.profile.name.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(10.0))
                                                    .text_color(theme.muted)
                                                    .child(type_label(&account, false)),
                                            ),
                                    ),
                            )
                            // "Forget this account" — visible in the React
                            // app only while the row is hovered
                            // (`opacity-0 group-hover:opacity-100`); gpui
                            // has no group-hover, so it stays visible at
                            // muted→danger hover.
                            .child(
                                div()
                                    .id(SharedString::from(format!(
                                        "account-remove-{}",
                                        account.profile.id
                                    )))
                                    .flex_none()
                                    .text_color(theme.muted)
                                    .hover(move |style| style.text_color(theme.danger))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.remove(trash_id.clone(), cx);
                                    }))
                                    .child(icons::icon_at(icons::TRASH, 13.0)),
                            ),
                    );
                }

                dropdown = dropdown.child(section);
            }

            // Footer actions.
            dropdown = dropdown.child(
                div()
                    .mt_1()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .border_t_1()
                    .border_color(theme::white(0.06))
                    .pt_1()
                    .child(
                        div()
                            .id("account-add")
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded(px(12.0))
                            .p(px(10.0))
                            .text_size(px(12.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.muted)
                            .hover(|style| {
                                style.bg(theme::white(0.05)).text_color(theme.foreground)
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.start_add(cx)))
                            .child(icons::icon_at(icons::PLUS, 13.0))
                            .child("Add account"),
                    )
                    .child(
                        div()
                            .id("account-sign-out")
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded(px(12.0))
                            .p(px(10.0))
                            .text_size(px(12.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.muted)
                            .hover(move |style| {
                                style.bg(theme::white(0.05)).text_color(theme.danger)
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.sign_out(cx)))
                            .child("Sign out"),
                    ),
            );

            root = root.child(dropdown);
        }

        root.into_any_element()
    }
}
