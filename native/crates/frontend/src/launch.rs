//! The launch flow — direct ports of `launch-button.tsx`,
//! `smart-launch-dialog.tsx` and `launch-validation-dialog.tsx`.
//!
//! The flow works exactly as in React: Play always opens Smart Launch
//! first (a real pre-flight check, never a gate), the launch itself runs
//! the backend pipeline (which downloads/installs/provisions on demand),
//! and only an actual failed attempt pulls up the validation dialog to
//! explain what went wrong and offer a repair. The Halloween-themed
//! `HauntedLaunch` overlay is deferred with the halloween theme (Phase 5).
//!
//! Dialogs render off the shell root via `modals::ModalStore` (gpui has
//! no `position: fixed`); each spec keeps a weak handle to the button that
//! opened it so the flow state (busy label, error line) lives on the
//! button, exactly where the React component kept it.

use gpui::{Context, FontWeight, SharedString, Window, div, prelude::*, px};

use backend::commands;
use backend::models::instance::{InstanceValidationReport, MinecraftInstance, ValidationSeverity};
use backend::services::instance::health::{HealthCheckItem, HealthReport, HealthStatus};

use crate::{bridge, icons, modals, theme, theme::Theme, toasts, ui};

/// `text-danger/90`, `text-warning/90` — console tones, defined here
/// because `toConsoleLine`'s consumers live in console.rs.
pub const DANGER_TONE: gpui::Rgba = theme::tinted_rgba(0xE6455A, 0.90);
pub const WARNING_TONE: gpui::Rgba = theme::tinted_rgba(0xF2C14E, 0.90);

/// The checks that most often stop a game from starting or make it
/// stutter — called out above the full list so they can't be missed.
const LAUNCH_BLOCKERS: [&str; 2] = ["java", "ram"];

fn is_problem(i: &HealthCheckItem) -> bool {
    matches!(i.status, HealthStatus::Warning | HealthStatus::Error)
}

fn score_color(score: u32) -> gpui::Rgba {
    let theme = Theme::obsidian();
    if score >= 90 {
        theme.success
    } else if score >= 70 {
        theme.warning
    } else {
        theme.danger
    }
}

// ---------------------------------------------------------------------------
// LaunchButton — `launch-button.tsx`
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Launching,
    Updating,
}

/// One Play button, wherever it renders (the banner or a recent-instance
/// card). Owns the whole flow: smart launch → confirm → auto-update pass →
/// launch → validate on failure.
pub struct LaunchButton {
    instance: MinecraftInstance,
    /// Only the banner's button opens the console the moment the launch
    /// actually begins (`onLaunchStart`).
    open_console_on_launch: bool,
    size: ui::ButtonSize,
    variant: ui::ButtonVariant,
    /// Optional override; the default label is "Play".
    label: Option<&'static str>,
    busy: bool,
    phase: Phase,
    error: Option<String>,
}

impl LaunchButton {
    pub fn new(
        instance: MinecraftInstance,
        open_console_on_launch: bool,
        size: ui::ButtonSize,
        variant: ui::ButtonVariant,
    ) -> Self {
        Self {
            instance,
            open_console_on_launch,
            size,
            variant,
            label: None,
            busy: false,
            phase: Phase::Launching,
            error: None,
        }
    }

    /// Fresh instance data after a refetch (playtime, auto-update flag…).
    pub fn set_instance(&mut self, instance: MinecraftInstance) {
        self.instance = instance;
    }

    /// Whether this is the banner's button right now (`onLaunchStart`).
    pub fn set_open_console(&mut self, open_console_on_launch: bool) {
        self.open_console_on_launch = open_console_on_launch;
    }

    fn open_smart_launch(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let instance_id = self.instance.id.clone();
        let owner = cx.entity().downgrade();
        modals::open_smart_launch(cx, instance_id, owner);
    }

    /// `handleConfirmedLaunch` — behind the Smart Launch dialog's Launch
    /// button. Runs the auto-update pass (best-effort) first, then the
    /// launch itself, and on failure the validation report.
    fn begin_launch(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        let auto_update = self.instance.auto_update_content;

        // onLaunchStart: the console opens the moment the real launch
        // begins (only the banner button asks for this).
        if self.open_console_on_launch {
            modals::open_console(cx, self.instance.id.clone(), self.instance.name.clone());
        }

        if auto_update {
            self.phase = Phase::Updating;
            cx.notify();
            let instance_id = self.instance.id.clone();
            let updates = bridge::command(cx, move |state| async move {
                commands::mods::apply_all_mod_updates(&state, instance_id).await
            });
            cx.spawn(async move |this, cx| {
                let outcome = updates.await;
                this.update(cx, |this, cx| {
                    this.phase = Phase::Launching;
                    if let Ok(Ok(outcome)) = outcome {
                        let count = outcome.updated.len();
                        if count > 0 {
                            let s = if count == 1 { "" } else { "s" };
                            toasts::toast_success(
                                cx,
                                format!("Updated {count} item{s} before launch."),
                            );
                        }
                    }
                    // Best-effort — a failed update pass (e.g. offline)
                    // never blocks the actual launch.
                    this.run_launch(cx);
                })
                .ok();
            })
            .detach();
        } else {
            cx.notify();
            self.run_launch(cx);
        }
    }

    /// `runLaunch` — the actual launch; a failure pulls up the validation
    /// report to explain what went wrong and offer a real repair.
    fn run_launch(&mut self, cx: &mut Context<Self>) {
        let instance_id = self.instance.id.clone();
        let task = bridge::command(cx, move |state| async move {
            match commands::launch::launch_instance(&state, instance_id.clone(), None).await {
                Ok(()) => Ok(()),
                Err(err) => {
                    let report = commands::instances::validate_instance(&state, instance_id)
                        .await
                        .ok();
                    Err((err.message, report))
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .unwrap_or_else(|_e| Err(("Couldn't launch.".to_string(), None)));
            this.update(cx, |this, cx| this.finish_launch(result, cx))
                .ok();
        })
        .detach();
    }

    fn finish_launch(
        &mut self,
        result: Result<(), (String, Option<InstanceValidationReport>)>,
        cx: &mut Context<Self>,
    ) {
        self.busy = false;
        match result {
            Ok(()) => {}
            Err((message, report)) => match report {
                Some(report) if !report.ready => {
                    let instance_id = self.instance.id.clone();
                    let name = self.instance.name.clone();
                    let owner = cx.entity().downgrade();
                    modals::open_validation(cx, instance_id, name, report, owner);
                }
                _ => self.error = Some(message),
            },
        }
        cx.notify();
    }

    /// `doLaunch` — "Launch anyway"/post-repair relaunch: no validation
    /// step, the error line is all the feedback.
    fn launch_direct(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();

        let instance_id = self.instance.id.clone();
        let task = bridge::command(cx, move |state| async move {
            commands::launch::launch_instance(&state, instance_id, None).await
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|e| {
                Err(backend::error::AppError::new(
                    "Couldn't launch.",
                    e.to_string(),
                ))
            });
            this.update(cx, |this, cx| {
                this.busy = false;
                if let Err(err) = result {
                    this.error = Some(err.message);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for LaunchButton {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *Theme::get(cx);
        let label: SharedString = if self.busy {
            match self.phase {
                Phase::Updating => "Updating...".into(),
                Phase::Launching => "Checking...".into(),
            }
        } else {
            self.label.unwrap_or("Play").into()
        };
        let id: SharedString = format!("launch-button-{}", self.instance.id).into();

        // launch-button.tsx's wrapper: `flex flex-col items-end gap-1`.
        let mut wrapper = div().flex().flex_col().items_end().gap_1();

        let mut button = ui::button(id, theme, self.variant, self.size, self.busy).child(label);
        if !self.busy {
            button = button.on_click(cx.listener(|this, _, _, cx| this.open_smart_launch(cx)));
        }
        wrapper = wrapper.child(button);

        if let Some(error) = &self.error {
            wrapper = wrapper.child(
                div()
                    .text_size(px(12.0)) // text-xs
                    .text_color(theme.danger)
                    .child(error.clone()),
            );
        }

        wrapper
    }
}

// ---------------------------------------------------------------------------
// SmartLaunchDialog — `smart-launch-dialog.tsx`
// ---------------------------------------------------------------------------

/// Arcane Smart Launch: a quick, real pre-flight check (Java version match,
/// RAM headroom against what's free right now, disk, GPU,
/// outdated/conflicting/missing-dependency mods, shader-loader presence)
/// shown before every launch. Never blocks launching — "Launch" always
/// works regardless of score.
pub struct SmartLaunchDialog {
    instance_id: String,
    owner: gpui::WeakEntity<LaunchButton>,
    report: Option<HealthReport>,
    error: Option<String>,
    fixing: bool,
    busy_item: Option<String>,
}

impl SmartLaunchDialog {
    pub fn new(spec: &modals::SmartLaunchSpec, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            instance_id: spec.instance_id.clone(),
            owner: spec.owner.clone(),
            report: None,
            error: None,
            fixing: false,
            busy_item: None,
        };
        this.run_check(cx);
        this
    }

    fn run_check(&mut self, cx: &mut Context<Self>) {
        let instance_id = self.instance_id.clone();
        let task = bridge::command(cx, move |state| async move {
            commands::launch::check_instance_health(&state, instance_id).await
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|e| {
                Err(backend::error::AppError::new(
                    "Couldn't run the pre-launch check.",
                    e.to_string(),
                ))
            });
            this.update(cx, |this, cx| {
                match result {
                    Ok(report) => {
                        this.report = Some(report);
                        this.error = None;
                    }
                    Err(err) => this.error = Some(err.message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// One item's one-click remedy, applied through the crash helper's
    /// `apply_crash_fix` — then the whole check re-runs.
    fn apply_item_fix(&mut self, item: HealthCheckItem, cx: &mut Context<Self>) {
        let Some(fix) = item.fix else { return };
        self.busy_item = Some(item.id.clone());
        cx.notify();

        let instance_id = self.instance_id.clone();
        let action = fix.action;
        let task = bridge::command(cx, move |state| async move {
            commands::crash::apply_crash_fix(&state, instance_id, action).await
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|e| {
                Err(backend::error::AppError::new(
                    "That fix didn't work.",
                    e.to_string(),
                ))
            });
            this.update(cx, |this, cx| {
                this.busy_item = None;
                match result {
                    Ok(message) => {
                        toasts::toast_success(cx, message);
                        this.run_check(cx);
                    }
                    Err(err) => toasts::toast_error(cx, err.message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// "Fix All" — every automatic remedy Smart Launch found.
    fn fix_all(&mut self, cx: &mut Context<Self>) {
        self.fixing = true;
        cx.notify();

        let instance_id = self.instance_id.clone();
        let task = bridge::command(cx, move |state| async move {
            commands::launch::fix_instance_health(&state, instance_id).await
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|e| {
                Err(backend::error::AppError::new(
                    "Couldn't fix these automatically.",
                    e.to_string(),
                ))
            });
            this.update(cx, |this, cx| {
                this.fixing = false;
                match result {
                    Ok(report) => this.report = Some(report),
                    Err(err) => this.error = Some(err.message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn launch(&mut self, cx: &mut Context<Self>) {
        let owner = self.owner.clone();
        modals::close_smart_launch(cx);
        let _ = owner.update(cx, |button, cx| button.begin_launch(cx));
    }
}

impl Render for SmartLaunchDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *Theme::get(cx);
        let busy = self.fixing || self.busy_item.is_some();

        // glass-panel flex w-full max-w-md flex-col rounded-2xl p-6
        let mut panel = ui::glass_panel(theme)
            .id("smart-launch-panel")
            .flex()
            .w(px(448.0)) // max-w-md
            .flex_col()
            .rounded(px(16.0))
            .p(px(24.0))
            .child(
                // Title row.
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(icons::icon_at(icons::HEARTBEAT, 17.0).text_color(theme.accent))
                    .child(
                        div()
                            .text_size(px(16.0)) // text-base
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Arcane Smart Launch"),
                    ),
            );

        if let Some(error) = &self.error {
            panel = panel.child(
                div()
                    .id("smart-launch-error")
                    .mt(px(16.0))
                    .flex()
                    .items_center()
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(theme::tinted_rgba(0xE6455A, 0.30))
                    .bg(theme::tinted_rgba(0xE6455A, 0.10))
                    .p(px(12.0))
                    .text_size(px(14.0))
                    .text_color(theme.danger)
                    .child(div().flex_1().child(error.clone()))
                    .child(
                        div()
                            .id("smart-launch-retry")
                            .ml(px(8.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.danger)
                            .child("Retry")
                            .on_click(cx.listener(|this, _, _, cx| this.run_check(cx))),
                    ),
            );
        } else if self.report.is_none() {
            panel = panel.child(
                div()
                    .mt(px(24.0))
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(14.0))
                    .text_color(theme.muted)
                    .child(icons::icon_at(icons::ARROW_CLOCKWISE, 13.0))
                    .child("Checking Java, RAM, disk space, and mods..."),
            );
        }

        if let Some(report) = &self.report {
            let items = report.items.clone();
            let fixable_count = items
                .iter()
                .filter(|i| i.fixable && !matches!(i.status, HealthStatus::Ok))
                .count();
            let blockers: Vec<HealthCheckItem> = items
                .iter()
                .filter(|i| LAUNCH_BLOCKERS.contains(&i.id.as_str()) && is_problem(i))
                .cloned()
                .collect();

            // Score.
            panel = panel.child(
                div()
                    .mt(px(16.0))
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_size(px(30.0)) // text-3xl
                            .font_weight(FontWeight::BOLD)
                            .text_color(score_color(report.score))
                            .child(format!("{}", report.score)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.muted)
                                    .child("Arcane Health Score"),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(theme.muted)
                                    .child("out of 100"),
                            ),
                    ),
            );

            // The launch blockers, called out.
            if !blockers.is_empty() {
                let mut block = div()
                    .mt(px(16.0))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .rounded(px(12.0))
                    .border_1()
                    .border_color(theme::tinted_rgba(0xF2C14E, 0.30))
                    .bg(theme::tinted_rgba(0xF2C14E, 0.10))
                    .p(px(12.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .text_size(px(12.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.warning)
                            .child(icons::icon_at(icons::WARNING_CIRCLE, 13.0))
                            .child("Check this before you play"),
                    );
                for item in &blockers {
                    block = block.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(
                                div().text_size(px(12.0)).child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .child(
                                            div()
                                                .font_weight(FontWeight::MEDIUM)
                                                .child(format!("{}: ", item.label)),
                                        )
                                        .child(
                                            div()
                                                .text_color(theme.muted)
                                                .child(item.detail.clone()),
                                        ),
                                ),
                            )
                            .children(self.fix_button(item, busy, cx)),
                    );
                }
                panel = panel.child(block);
            }

            // The full check list.
            let mut list = div().mt(px(16.0)).flex().flex_col().gap_1p5();
            for item in &items {
                let (icon, color) = match item.status {
                    HealthStatus::Ok => (icons::CHECK_CIRCLE, theme.success),
                    HealthStatus::Warning => (icons::WARNING_CIRCLE, theme.warning),
                    HealthStatus::Error => (icons::X_CIRCLE, theme.danger),
                    HealthStatus::Skipped => (icons::QUESTION, theme.muted),
                };
                list = list.child(
                    div()
                        .flex()
                        .items_start()
                        .gap_2()
                        .rounded(px(8.0))
                        .px(px(8.0))
                        .py(px(6.0))
                        .text_size(px(12.0))
                        .hover(move |style| style.bg(theme::tinted_rgba(0xFFFFFF, 0.04)))
                        .child(
                            div()
                                .mt(px(2.0))
                                .flex_none()
                                .child(icons::icon_at(icon, 14.0).text_color(color)),
                        )
                        .child(
                            div().min_w_0().flex_1().child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .child(
                                        div()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(item.label.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_color(theme.muted)
                                            .child(format!(" — {}", item.detail)),
                                    ),
                            ),
                        )
                        .children(if LAUNCH_BLOCKERS.contains(&item.id.as_str()) {
                            None
                        } else {
                            self.fix_button(item, busy, cx)
                        }),
                );
            }
            panel = panel.child(list);

            // Footer: Fix All on the left, Cancel + Launch on the right.
            let mut footer = div()
                .mt(px(24.0))
                .flex()
                .items_center()
                .justify_between()
                .gap_2();

            if fixable_count > 0 {
                footer = footer.child(
                    ui::button(
                        "smart-launch-fix-all",
                        theme,
                        ui::ButtonVariant::Ghost,
                        ui::ButtonSize::Sm,
                        busy,
                    )
                    .child(icons::icon_at(
                        if self.fixing {
                            icons::ARROW_CLOCKWISE
                        } else {
                            icons::WRENCH
                        },
                        13.0,
                    ))
                    .child(if self.fixing {
                        "Fixing...".to_string()
                    } else {
                        format!("Fix All ({fixable_count})")
                    })
                    .when(!busy, |b| {
                        b.on_click(cx.listener(|this, _, _, cx| this.fix_all(cx)))
                    }),
                );
            } else {
                footer = footer.child(div());
            }

            footer = footer.child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        ui::button(
                            "smart-launch-cancel",
                            theme,
                            ui::ButtonVariant::Ghost,
                            ui::ButtonSize::Sm,
                            busy,
                        )
                        .child("Cancel")
                        .when(!busy, |b| {
                            b.on_click(cx.listener(|_, _, _, cx| modals::close_smart_launch(cx)))
                        }),
                    )
                    .child(
                        ui::button(
                            "smart-launch-launch",
                            theme,
                            ui::ButtonVariant::Primary,
                            ui::ButtonSize::Sm,
                            busy,
                        )
                        .child(if !blockers.is_empty() {
                            "Launch anyway"
                        } else {
                            "Launch"
                        })
                        .when(!busy, |b| {
                            b.on_click(cx.listener(|this, _, _, cx| this.launch(cx)))
                        }),
                    ),
            );
            panel = panel.child(footer);
        }

        ui::modal_backdrop("smart-launch-backdrop").child(panel)
    }
}

impl SmartLaunchDialog {
    /// `fixButton` — the per-item one-click remedy, when this item has one
    /// and is actually a problem.
    fn fix_button(
        &self,
        item: &HealthCheckItem,
        busy: bool,
        cx: &Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        let fix = item.fix.as_ref().filter(|_| is_problem(item))?;
        let theme = *Theme::get(cx);
        let busy_item = self.busy_item.as_deref() == Some(item.id.as_str());
        let id: SharedString = format!("smart-fix-{}", item.id).into();
        Some(
            ui::button(
                id,
                theme,
                ui::ButtonVariant::Ghost,
                ui::ButtonSize::Sm,
                busy,
            )
            .flex_none()
            .child(icons::icon_at(
                if busy_item {
                    icons::ARROW_CLOCKWISE
                } else {
                    icons::WRENCH
                },
                12.0,
            ))
            .child(fix.label.clone())
            .when(!busy, |b| {
                let item = item.clone();
                b.on_click(cx.listener(move |this, _, _, cx| this.apply_item_fix(item.clone(), cx)))
            }),
        )
    }
}

// ---------------------------------------------------------------------------
// LaunchValidationDialog — `launch-validation-dialog.tsx`
// ---------------------------------------------------------------------------

const BLOCKING_CODES: [&str; 6] = [
    "missing_client",
    "missing_libraries",
    "missing_version_metadata",
    "no_java",
    "invalid_java",
    "missing_game_dir",
];

pub struct LaunchValidationDialog {
    instance_id: String,
    #[allow(dead_code)]
    name: String,
    owner: gpui::WeakEntity<LaunchButton>,
    fixing: bool,
}

impl LaunchValidationDialog {
    pub fn new(spec: &modals::ValidationSpec, _cx: &mut Context<Self>) -> Self {
        Self {
            instance_id: spec.instance_id.clone(),
            name: spec.name.clone(),
            owner: spec.owner.clone(),
            fixing: false,
        }
    }

    fn apply_fix(&mut self, cx: &mut Context<Self>) {
        if self.fixing {
            return;
        }
        self.fixing = true;
        cx.notify();

        let instance_id = self.instance_id.clone();
        let owner = self.owner.clone();
        let task = bridge::command(cx, move |state| async move {
            commands::launch::fix_instance(&state, instance_id).await
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|e| {
                Err(backend::error::AppError::new(
                    "Couldn't repair this instance.",
                    e.to_string(),
                ))
            });
            this.update(cx, |this, cx| {
                this.fixing = false;
                let instance_id = this.instance_id.clone();
                match result {
                    Ok(updated) => {
                        let ready = updated.ready;
                        modals::update_validation_report(cx, &instance_id, updated);
                        if ready {
                            modals::close_validation(cx);
                            let _ = owner.update(cx, |button, cx| button.launch_direct(cx));
                        }
                    }
                    Err(err) => toasts::toast_error(cx, err.message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn launch_anyway(&mut self, cx: &mut Context<Self>) {
        let owner = self.owner.clone();
        modals::close_validation(cx);
        let _ = owner.update(cx, |button, cx| button.launch_direct(cx));
    }
}

impl Render for LaunchValidationDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *Theme::get(cx);
        let Some(spec) = cx
            .global::<modals::ModalStore>()
            .validation
            .as_ref()
            .filter(|spec| spec.instance_id == self.instance_id)
            .map(|spec| spec.report.clone())
        else {
            return div().into_any_element();
        };
        let report = spec;

        let blocking = report.issues.iter().any(|issue| {
            issue.severity == ValidationSeverity::Error
                && BLOCKING_CODES.contains(&issue.code.as_str())
        });

        // glass-panel w-full max-w-md rounded-2xl p-6
        let mut panel = ui::glass_panel(theme)
            .id("launch-validation-panel")
            .w(px(448.0)) // max-w-md
            .rounded(px(16.0))
            .p(px(24.0))
            .child(
                div()
                    .text_size(px(16.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("{} problem(s) found", report.issues.len())),
            );

        let mut list = div()
            .mt(px(16.0))
            .flex()
            .flex_col()
            .gap_2()
            .text_size(px(14.0));
        for (i, issue) in report.issues.iter().enumerate() {
            let color = match issue.severity {
                ValidationSeverity::Error => theme.danger,
                ValidationSeverity::Warning => theme.warning,
            };
            list = list.child(div().text_color(color).child(format!(
                "{}. {}",
                i + 1,
                issue.message
            )));
        }
        panel = panel.child(list);

        if blocking {
            panel = panel.child(
                div()
                    .mt(px(16.0))
                    .text_size(px(12.0))
                    .text_color(theme.muted)
                    .child(
                        "This instance can't launch until these are fixed — connect to the internet and try Fix Automatically.",
                    ),
            );
        }

        let mut footer = div().mt(px(24.0)).flex().flex_wrap().justify_end().gap_2();
        footer = footer.child(
            ui::button(
                "validation-back",
                theme,
                ui::ButtonVariant::Ghost,
                ui::ButtonSize::Sm,
                false,
            )
            .child("Back")
            .on_click(cx.listener(|_, _, _, cx| modals::close_validation(cx))),
        );
        if !blocking {
            footer = footer.child(
                ui::button(
                    "validation-anyway",
                    theme,
                    ui::ButtonVariant::Ghost,
                    ui::ButtonSize::Sm,
                    false,
                )
                .child("Launch anyway")
                .on_click(cx.listener(|this, _, _, cx| this.launch_anyway(cx))),
            );
        }
        footer = footer.child(
            ui::button(
                "validation-fix",
                theme,
                ui::ButtonVariant::Primary,
                ui::ButtonSize::Sm,
                self.fixing,
            )
            .child(if self.fixing {
                "Fixing..."
            } else {
                "Fix automatically"
            })
            .when(!self.fixing, |b| {
                b.on_click(cx.listener(|this, _, _, cx| this.apply_fix(cx)))
            }),
        );
        panel = panel.child(footer);

        ui::modal_backdrop("launch-validation-backdrop")
            .child(panel)
            .into_any_element()
    }
}
