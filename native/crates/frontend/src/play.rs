//! The Play page — direct port of `src/app/page.tsx` (`HomePage`,
//! `GameBanner`, `StatTile`, `RecentInstanceCard`, `RecentInstances`).
//!
//! Every literal is copied from the React sources; the deferred pieces are
//! documented inline (the content tabs, the instance switcher dropdown,
//! Configure/Record, the "Create instance" button, the SVG banner strokes —
//! approximated with the pre-rasterized glow orbs from `background.rs`,
//! and the running dot's pulse animation, which is static here).
//!
//! The react-query hooks (`useInstances`, `useInstanceValidation`,
//! `useInstanceHealth`, `useInstalledMods`) become fetch-once caches
//! invalidated the same moments React invalidated them: running changes
//! (`launch://started`/`exit`) and modal dismissal after fixes/launches.

use std::collections::{HashMap, HashSet};

use gpui::{Context, Entity, FontWeight, SharedString, Window, div, prelude::*, px, relative};

use backend::commands;
use backend::models::common::ModLoaderKind;
use backend::models::instance::{InstanceValidationReport, MinecraftInstance};
use backend::services::instance::health::HealthReport;
use backend::services::minecraft::running::RunningInfo;

use crate::{
    background::{Orb, glow_orb},
    bridge,
    bridge::{DownloadsStore, RunningStore},
    format, icons,
    launch::LaunchButton,
    modals, theme,
    theme::Theme,
    toasts, ui,
};

// Tailwind neutral palette values used by the page (not theme tokens).
const NEUTRAL_200: gpui::Rgba = theme::rgba_const(0xE5E5E5FF);
const NEUTRAL_400: gpui::Rgba = theme::rgba_const(0xA1A1AAFF);
const NEUTRAL_500: gpui::Rgba = theme::rgba_const(0x737373FF);
const NEUTRAL_600: gpui::Rgba = theme::rgba_const(0x525252FF);
const WHITE_HERO: gpui::Rgba = theme::rgba_const(0xFFFFFFFF);

/// `loaderLabel` from page.tsx — loader.id() already gives the lowercase
/// keys; this maps to the display labels.
fn loader_label(loader: ModLoaderKind) -> &'static str {
    match loader {
        ModLoaderKind::Vanilla => "Vanilla",
        ModLoaderKind::Fabric => "Fabric",
        ModLoaderKind::Forge => "Forge",
        ModLoaderKind::NeoForge => "NeoForge",
        ModLoaderKind::Quilt => "Quilt",
    }
}

fn loader_tag(instance: &MinecraftInstance) -> String {
    if instance.loader == ModLoaderKind::Vanilla {
        "Vanilla".to_string()
    } else {
        match &instance.loader_version {
            Some(version) => {
                format!("{} {version}", loader_label(instance.loader))
            }
            None => loader_label(instance.loader).to_string(),
        }
    }
}

fn format_mb(mb: Option<u32>) -> String {
    let Some(mb) = mb else {
        return "Default".to_string();
    };
    if mb % 1024 == 0 {
        format!("{} GB", mb / 1024)
    } else {
        format!("{:.1} GB", mb as f64 / 1024.0)
    }
}

fn health_label(score: u32) -> (&'static str, bool) {
    if score >= 90 {
        ("Excellent", true)
    } else if score >= 70 {
        ("Good", true)
    } else {
        ("Needs attention", false)
    }
}

pub struct PlayPage {
    instances: Option<Vec<MinecraftInstance>>,
    /// The react-query caches, per instance id.
    validation: HashMap<String, InstanceValidationReport>,
    health: HashMap<String, HealthReport>,
    mod_counts: HashMap<String, usize>,
    /// In-flight fetch guards (`mods:<id>`, `validation:<id>`, `health:<id>`).
    pending: HashSet<String>,
    /// One LaunchButton per role — the React page rendered separate
    /// button instances for the banner (lg, primary) and each card
    /// (sm, ghost), so the native page keeps two per instance too.
    banner_buttons: HashMap<String, Entity<LaunchButton>>,
    card_buttons: HashMap<String, Entity<LaunchButton>>,
    quitting: bool,
    quit_error: Option<String>,
    /// `useCountUp`'s eased playtime animation.
    animated_playtime: u64,
    anim_target: Option<u64>,
    /// `useClockTick` — 1s re-render while the primary instance runs.
    clock_on: bool,
    /// The banner's radial-gradient accent glows, pre-rasterized.
    orb_far: Orb,
    orb_near: Orb,
}

impl PlayPage {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let accent = Theme::obsidian().accent;
        let mut this = Self {
            instances: None,
            validation: HashMap::new(),
            health: HashMap::new(),
            mod_counts: HashMap::new(),
            pending: HashSet::new(),
            banner_buttons: HashMap::new(),
            card_buttons: HashMap::new(),
            quitting: false,
            quit_error: None,
            animated_playtime: 0,
            anim_target: None,
            clock_on: false,
            // page.tsx's banner gradients: `radial-gradient(1100px 480px at
            // 88% -15%, accent/0.16)` and `(700px 380px at 0% 120%,
            // accent/0.09)` — approximated as blurred discs overhanging the
            // banner's top-right / bottom-left corners (gpui has no CSS
            // radial gradients).
            orb_far: glow_orb(560.0, 150.0, 0.16, accent),
            orb_near: glow_orb(400.0, 120.0, 0.09, accent),
        };

        cx.observe_global::<RunningStore>(|this, cx| {
            // `launch://started`/`exit` — last_played/playtime changed after
            // a session, so the list (and the count-up) refetches.
            this.refetch(cx);
            this.ensure_clock(cx);
            cx.notify();
        })
        .detach();
        cx.observe_global::<DownloadsStore>(|_, cx| cx.notify())
            .detach();
        cx.observe_global::<modals::ModalStore>(|this, cx| {
            // The React app invalidated its queries after fixes and
            // launches; on the native side those moments end with the last
            // dialog closing, so an all-closed store is the refetch trigger.
            let all_closed = cx.global::<modals::ModalStore>().all_closed();
            if all_closed {
                this.refetch(cx);
            }
            cx.notify();
        })
        .detach();
        cx.observe_global::<bridge::SelectionStore>(|this, cx| {
            // The Instances page's Select buttons change the primary
            // instance (and, with it, the banner button's console-open
            // behavior) — the React app's zustand subscription.
            this.after_instances_changed(cx);
            cx.notify();
        })
        .detach();

        this.refetch(cx);
        this
    }

    /// `selectedInstanceId ?? most recently played` (page.tsx's `primary`).
    fn primary(&self, cx: &Context<Self>) -> Option<MinecraftInstance> {
        let instances = self.instances.as_ref()?;
        let selected = cx.global::<bridge::SelectionStore>().selected.clone();
        let found = selected
            .as_ref()
            .and_then(|id| instances.iter().find(|i| &i.id == id))
            .cloned();
        found.or_else(|| {
            instances
                .iter()
                .max_by_key(|i| i.last_played.unwrap_or(0))
                .cloned()
        })
    }

    fn refetch(&mut self, cx: &mut Context<Self>) {
        let task = bridge::command(cx, |state| async move {
            commands::instances::list_instances(&state).await
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(list)) = task.await {
                this.update(cx, |this, cx| {
                    this.instances = Some(list);
                    this.after_instances_changed(cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// Runs after the instance list changes: rebuilds the button entities,
    /// refetches the missing stat queries, restarts the playtime animation
    /// and the clock.
    fn after_instances_changed(&mut self, cx: &mut Context<Self>) {
        let primary_id = self.primary(cx).map(|p| p.id.clone());
        let instances = self.instances.clone().unwrap_or_default();

        for buttons in [&mut self.banner_buttons, &mut self.card_buttons] {
            buttons.retain(|id, _| instances.iter().any(|i| &i.id == id));
        }
        for instance in &instances {
            let id = instance.id.clone();
            self.banner_buttons.entry(id.clone()).or_insert_with(|| {
                let instance = instance.clone();
                cx.new(|_| {
                    LaunchButton::new(
                        instance,
                        false,
                        ui::ButtonSize::Lg,
                        ui::ButtonVariant::Primary,
                    )
                })
            });
            self.card_buttons.entry(id).or_insert_with(|| {
                let instance = instance.clone();
                cx.new(|_| {
                    LaunchButton::new(
                        instance,
                        false,
                        ui::ButtonSize::Sm,
                        ui::ButtonVariant::Ghost,
                    )
                })
            });
        }
        // Only the banner's button opens the console on launch
        // (`onLaunchStart` on the hero's LaunchButton).
        for (id, button) in &self.banner_buttons {
            let is_banner = primary_id.as_deref() == Some(id.as_str());
            button.update(cx, |b, _| b.set_open_console(is_banner));
        }
        for instance in &instances {
            if let Some(button) = self.banner_buttons.get(&instance.id) {
                button.update(cx, |b, _| b.set_instance(instance.clone()));
            }
            if let Some(button) = self.card_buttons.get(&instance.id) {
                button.update(cx, |b, _| b.set_instance(instance.clone()));
            }
        }

        self.fetch_missing(cx);
        if let Some(primary) = self.primary(cx) {
            self.animate_playtime(primary.total_playtime_secs, cx);
        }
        self.ensure_clock(cx);
        cx.notify();
    }

    /// Spawns the stat queries that have no cached value yet (the react-query
    /// hooks per instance).
    fn fetch_missing(&mut self, cx: &mut Context<Self>) {
        let instances = self.instances.clone().unwrap_or_default();

        for instance in &instances {
            let id = instance.id.clone();
            let key = format!("mods:{id}");
            if self.mod_counts.contains_key(&id) || self.pending.contains(&key) {
                continue;
            }
            self.pending.insert(key.clone());
            let command_id = id.clone();
            let task = bridge::command(cx, move |state| async move {
                commands::mods::list_installed_mods(&state, command_id).await
            });
            cx.spawn(async move |this, cx| {
                let count = task.await.ok().and_then(|r| r.ok()).map(|mods| mods.len());
                this.update(cx, |this, cx| {
                    this.pending.remove(&key);
                    if let Some(count) = count {
                        this.mod_counts.insert(id, count);
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }

        let Some(primary) = self.primary(cx) else {
            return;
        };

        {
            let id = primary.id.clone();
            let key = format!("validation:{id}");
            if !self.validation.contains_key(&id) && !self.pending.contains(&key) {
                self.pending.insert(key.clone());
                let command_id = id.clone();
                let task = bridge::command(cx, move |state| async move {
                    commands::instances::validate_instance(&state, command_id).await
                });
                cx.spawn(async move |this, cx| {
                    let report = task.await.ok().and_then(|r| r.ok());
                    this.update(cx, |this, cx| {
                        this.pending.remove(&key);
                        if let Some(report) = report {
                            this.validation.insert(id, report);
                            cx.notify();
                        }
                    })
                    .ok();
                })
                .detach();
            }
        }

        {
            let id = primary.id.clone();
            let key = format!("health:{id}");
            if !self.health.contains_key(&id) && !self.pending.contains(&key) {
                self.pending.insert(key.clone());
                let command_id = id.clone();
                let task = bridge::command(cx, move |state| async move {
                    commands::launch::check_instance_health(&state, command_id).await
                });
                cx.spawn(async move |this, cx| {
                    let report = task.await.ok().and_then(|r| r.ok());
                    this.update(cx, |this, cx| {
                        this.pending.remove(&key);
                        if let Some(report) = report {
                            this.health.insert(id, report);
                            cx.notify();
                        }
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    /// `useClockTick(Boolean(runningInfo))` — re-render once a second while
    /// the primary instance runs, for the live elapsed time.
    fn ensure_clock(&mut self, cx: &mut Context<Self>) {
        if self.clock_on {
            return;
        }
        let Some(primary) = self.primary(cx) else {
            return;
        };
        if !cx.global::<RunningStore>().is_running(&primary.id) {
            return;
        }
        self.clock_on = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                let keep = this
                    .update(cx, |this, cx| {
                        let running = this
                            .primary(cx)
                            .map(|p| cx.global::<RunningStore>().is_running(&p.id))
                            .unwrap_or(false);
                        if running {
                            cx.notify();
                        }
                        running
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
            this.update(cx, |this, _| this.clock_on = false).ok();
        })
        .detach();
    }

    /// `useCountUp` — ease the displayed playtime toward the new value over
    /// 500ms (cubic ease-out); the first value counts up from 0 because the
    /// initial display is 0 until the query resolves.
    fn animate_playtime(&mut self, target: u64, cx: &mut Context<Self>) {
        if self.anim_target == Some(target) || self.animated_playtime == target {
            self.anim_target = Some(target);
            return;
        }
        self.anim_target = Some(target);
        let from = self.animated_playtime;
        cx.spawn(async move |this, cx| {
            let start = std::time::Instant::now();
            loop {
                let t = ((std::time::Instant::now() - start).as_secs_f32() / 0.5).min(1.0);
                let eased = 1.0 - (1.0 - t).powi(3);
                let value = (from as f32 + (target - from) as f32 * eased).round() as u64;
                let done = t >= 1.0;
                let stop = this
                    .update(cx, |this, cx| {
                        if this.anim_target != Some(target) {
                            return true; // superseded by a newer animation
                        }
                        this.animated_playtime = if done { target } else { value };
                        cx.notify();
                        done
                    })
                    .unwrap_or(true);
                if stop {
                    return;
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(16))
                    .await;
            }
        })
        .detach();
    }

    /// "Force quit" — `api.launch.kill`, with the button's Stopping state.
    fn force_quit(&mut self, cx: &mut Context<Self>) {
        let Some(primary) = self.primary(cx) else {
            return;
        };
        self.quitting = true;
        self.quit_error = None;
        cx.notify();

        let id = primary.id.clone();
        let task = bridge::command(cx, move |state| async move {
            commands::launch::kill_instance(&state, id).await
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|e| {
                Err(backend::error::AppError::new(
                    "Couldn't stop Minecraft.",
                    e.to_string(),
                ))
            });
            this.update(cx, |this, cx| {
                this.quitting = false;
                if let Err(err) = result {
                    this.quit_error = Some(err.message);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // -------------------------------------------------------------------------
    // Render pieces (page.tsx's GameBanner / StatTile / RecentInstances)
    // -------------------------------------------------------------------------

    fn banner(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = *Theme::get(cx);
        let Some(primary) = self.primary(cx) else {
            // The empty state; the "Create instance" button lands with the
            // instances page (Phase 3) — documented deferral.
            return ui::glass_panel(theme)
                .id("play-empty")
                .p(px(40.0))
                .flex()
                .flex_col()
                .items_center()
                .child(
                    div()
                        .text_size(px(11.0))
                        .font_weight(FontWeight::BOLD)
                        .text_color(NEUTRAL_500)
                        .child("NO INSTANCES YET"),
                )
                .child(
                    div()
                        .mt(px(8.0))
                        .text_size(px(18.0))
                        .font_weight(FontWeight::BOLD)
                        .text_color(WHITE_HERO)
                        .child("Your next world starts here."),
                )
                .into_any_element();
        };

        let running_info: Option<RunningInfo> =
            cx.global::<RunningStore>().get(&primary.id).cloned();
        let validation = self.validation.get(&primary.id);
        let health = self.health.get(&primary.id);
        let mod_count = self.mod_counts.get(&primary.id).copied();

        // The first active download (sorted by name in the store).
        let active_download = cx.global::<DownloadsStore>().active().into_iter().next();

        // Status row (`text-[11px] font-bold uppercase tracking-[0.14em]`).
        let status = div()
            .mb(px(12.0)) // mb-3
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(11.0))
            .font_weight(FontWeight::BOLD)
            .child(
                div()
                    .h(px(6.0)) // h-1.5
                    .w(px(6.0))
                    .rounded_full()
                    .bg(theme.success),
            )
            .child(div().text_color(theme.success).child(match &running_info {
                Some(info) => {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    format!("Running · {}", format::format_elapsed(info.started_at, now))
                }
                None => "Ready to launch".to_string(),
            }));

        // Name row (the switcher dropdown is deferred with the instances
        // page — documented).
        let name_row = div().flex().items_center().gap_1p5().child(
            div()
                .min_w_0()
                .truncate()
                .text_size(px(40.0))
                .font_weight(FontWeight::EXTRA_BOLD)
                .line_height(relative(1.0)) // leading-none
                .text_color(WHITE_HERO)
                .child(primary.name.clone()),
        );

        // Meta row.
        let mut meta = div()
            .mt(px(12.0)) // mt-3
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .text_size(px(13.0))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(NEUTRAL_400)
            .child(
                div()
                    .text_color(NEUTRAL_200)
                    .child(format!("Minecraft {}", primary.minecraft_version)),
            )
            .child(div().text_color(NEUTRAL_600).child("·"))
            .child(div().text_color(NEUTRAL_200).child(loader_tag(&primary)))
            .child(div().text_color(NEUTRAL_600).child("·"))
            .child(div().child(format!("{} mods", mod_count.unwrap_or(0))));
        if let Some(last_played) = primary.last_played {
            meta = meta
                .child(div().text_color(NEUTRAL_600).child("·"))
                .child(div().child(format!(
                    "Last played {}",
                    format::time_ago(Some(last_played))
                )));
        }

        // Play Time tile.
        let playtime = div()
            .flex_none()
            .child(
                div()
                    .text_size(px(10.0))
                    .font_weight(FontWeight::BOLD)
                    .text_color(NEUTRAL_500)
                    .child("PLAY TIME"),
            )
            .child(
                div()
                    .mt(px(4.0)) // mt-1
                    .font_family(theme::FONT_FAMILY_MONO_FALLBACK)
                    .text_size(px(20.0)) // text-xl
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(WHITE_HERO)
                    .child(format::format_playtime(self.animated_playtime)),
            );

        // Buttons row.
        let buttons_row = match &running_info {
            Some(_) => div().flex().items_center().gap_3().child(
                ui::button(
                    "force-quit",
                    theme,
                    ui::ButtonVariant::Ghost,
                    ui::ButtonSize::Md,
                    self.quitting,
                )
                .child(if self.quitting {
                    "Stopping..."
                } else {
                    "Force quit"
                })
                .when(!self.quitting, |b| {
                    b.on_click(cx.listener(|this, _, _, cx| this.force_quit(cx)))
                }),
            ),
            None => {
                let button = self.banner_buttons.get(&primary.id).cloned();
                div().flex().items_center().gap_3().children(button)
            }
        };

        let mut content = div()
            .relative() // above the glow layers
            .flex()
            .flex_col()
            .gap_6()
            .p(px(32.0)) // p-8
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap_6()
                    .child(div().min_w_0().child(status).child(name_row).child(meta))
                    .child(playtime),
            )
            .child(buttons_row);
        if let Some(quit_error) = &self.quit_error {
            content = content.child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.danger)
                    .child(quit_error.clone()),
            );
        }

        // The banner box: base gradient + pre-rasterized accent glows + the
        // left dark overlay (`bg-linear-to-r from-black/35 via-black/0`).
        let banner_box = div()
            .id("game-banner")
            .relative()
            .overflow_hidden()
            .rounded(px(16.0)) // rounded-2xl
            .border_1()
            .border_color(theme::white(0.10))
            .bg(gpui::linear_gradient(
                160.0,
                // The React gradient has a middle stop (#0d0e10 at 55%);
                // gpui's `linear_gradient` only takes two stops, so the
                // endpoints are blended — visually indistinguishable here.
                gpui::linear_color_stop(theme::rgba_const(0x16171BFF), 0.0),
                gpui::linear_color_stop(theme::rgba_const(0x0A0A0CFF), 1.0),
            ))
            .child(self.orb_far.render().top(px(-300.0)).right(px(-100.0)))
            .child(self.orb_near.render().bottom(px(-320.0)).left(px(-200.0)))
            .child(div().absolute().inset_0().bg(gpui::linear_gradient(
                90.0,
                gpui::linear_color_stop(theme::tinted_rgba(0x000000, 0.35), 0.0),
                gpui::linear_color_stop(theme::tinted_rgba(0x000000, 0.0), 1.0),
            )))
            .child(content);

        // Stat tiles (grid-cols-5 at this window width → flex row).
        let (perf_label, perf_ok) = match health {
            Some(report) => health_label(report.score),
            None => ("—", false),
        };
        let tiles = div()
            .flex()
            .gap_2p5()
            .child(stat_tile(
                "Mods".to_string(),
                mod_count.unwrap_or(0).to_string(),
                validation.map(|v| {
                    if v.ready {
                        "Compatible"
                    } else {
                        "Needs attention"
                    }
                    .to_string()
                }),
                validation.map(|v| v.ready),
            ))
            .child(stat_tile(
                "Minecraft".to_string(),
                primary.minecraft_version.clone(),
                None,
                None,
            ))
            .child(stat_tile(
                "Loader".to_string(),
                loader_tag(&primary),
                None,
                None,
            ))
            .child(stat_tile(
                "Memory".to_string(),
                format_mb(primary.memory_mb),
                None,
                None,
            ))
            .child(stat_tile(
                "Performance".to_string(),
                perf_label.to_string(),
                health.map(|h| format!("{}/100", h.score)),
                Some(perf_ok),
            ));

        // The active-download bar.
        let download_bar = active_download.map(|task| {
            let is_downloading =
                task.status == backend::models::download::DownloadStatus::Downloading;
            let pause = is_downloading;
            let download_task_id = task.id.clone();
            let remaining = task.total_bytes.saturating_sub(task.downloaded_bytes);
            let eta = format::format_eta(remaining, task.speed_bps);
            let pct = if task.total_bytes > 0 {
                ((task.downloaded_bytes as f64 / task.total_bytes as f64) * 100.0).round() as u64
            } else {
                0
            };

            let mut sub = div()
                .mt(px(2.0)) // mt-0.5
                .flex()
                .items_center()
                .text_size(px(12.0))
                .text_color(NEUTRAL_400)
                .child(div().child(format::format_bytes(task.downloaded_bytes)));
            if task.total_bytes > 0 {
                sub = sub
                    .child(div().child(format!(" / {}", format::format_bytes(task.total_bytes))));
            }
            if task.speed_bps > 0 {
                sub = sub
                    .child(
                        div()
                            .mx(px(4.0)) // mx-1
                            .text_color(NEUTRAL_500)
                            .child("•"),
                    )
                    .child(div().child(format::format_speed(task.speed_bps)));
            }

            ui::glass_panel(theme)
                .id("download-bar")
                .flex()
                .flex_col()
                .items_center()
                .justify_between()
                .gap_4()
                .rounded(px(16.0))
                .px(px(24.0))
                .py(px(16.0))
                .child(
                    div()
                        .flex_none()
                        .child(
                            div()
                                .text_size(px(14.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(WHITE_HERO)
                                .child(task.name.clone()),
                        )
                        .child(sub),
                )
                .child(
                    div()
                        .w_full()
                        .max_w(px(576.0)) // max-w-xl
                        .flex()
                        .flex_1()
                        .items_center()
                        .gap_3p5()
                        .child(
                            // Track + fill (the fill's glow shadow is
                            // deferred — gpui has no box-shadow).
                            div()
                                .h(px(8.0))
                                .w_full()
                                .overflow_hidden()
                                .rounded_full()
                                .border_1()
                                .border_color(theme::white(0.04))
                                .bg(theme::rgba_const(0x1B1E28FF))
                                .p(px(2.0))
                                .child(
                                    div()
                                        .h_full()
                                        .w(relative(pct as f32 / 100.0))
                                        .rounded_full()
                                        .bg(gpui::linear_gradient(
                                            90.0,
                                            gpui::linear_color_stop(theme.accent, 0.0),
                                            gpui::linear_color_stop(theme.accent_strong, 1.0),
                                        )),
                                ),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(12.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(NEUTRAL_400)
                                .child(match eta {
                                    Some(eta) => format!("{pct}% • {eta}"),
                                    None => format!("{pct}%"),
                                }),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            ui::button(
                                format!("download-{}", task.id),
                                theme,
                                ui::ButtonVariant::Ghost,
                                ui::ButtonSize::Sm,
                                false,
                            )
                            .child(if is_downloading { "Pause" } else { "Resume" })
                            .on_click(cx.listener(
                                move |_, _, _, cx| {
                                    let id = task.id.clone();
                                    let task = bridge::command(cx, move |state| async move {
                                        if pause {
                                            commands::downloads::pause_download(&state, id).await
                                        } else {
                                            commands::downloads::resume_download(&state, id).await
                                        }
                                    });
                                    cx.spawn(async move |_, cx| {
                                        if let Err(err) = task.await.unwrap_or(Err(
                                            backend::error::AppError::new(
                                                "That download couldn't be updated.",
                                                "task cancelled",
                                            ),
                                        )) {
                                            cx.update(|cx| toasts::toast_error(cx, err.message));
                                        }
                                    })
                                    .detach();
                                },
                            )),
                        )
                        .child(
                            ui::button(
                                SharedString::from(format!("download-cancel-{}", download_task_id)),
                                theme,
                                ui::ButtonVariant::Danger,
                                ui::ButtonSize::Sm,
                                false,
                            )
                            .child("Cancel")
                            .on_click(cx.listener(
                                move |_, _, _, cx| {
                                    let id = download_task_id.clone();
                                    let task = bridge::command(cx, move |state| async move {
                                        commands::downloads::cancel_download(&state, id).await
                                    });
                                    cx.spawn(async move |_, cx| {
                                        if let Err(err) = task.await.unwrap_or(Err(
                                            backend::error::AppError::new(
                                                "That download couldn't be cancelled.",
                                                "task cancelled",
                                            ),
                                        )) {
                                            cx.update(|cx| toasts::toast_error(cx, err.message));
                                        }
                                    })
                                    .detach();
                                },
                            )),
                        ),
                )
        });

        // Everything between the banner and Recent Instances
        // (`flex flex-col gap-5` in GameBanner).
        let mut banner_group = div()
            .flex()
            .flex_col()
            .gap_5()
            .child(banner_box)
            .child(tiles);
        if let Some(download_bar) = download_bar {
            banner_group = banner_group.child(download_bar);
        }
        banner_group.into_any_element()
    }

    fn recent_instances(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = *Theme::get(cx);
        let Some(instances) = self.instances.clone() else {
            return div().into_any_element();
        };
        let primary_id = self.primary(cx).map(|p| p.id.clone());

        // page.tsx's rest: everything but the primary, most-recent first,
        // capped at 5.
        let mut rest: Vec<&MinecraftInstance> = instances
            .iter()
            .filter(|i| primary_id.as_deref() != Some(i.id.as_str()))
            .collect();
        rest.sort_by_key(|i| std::cmp::Reverse(i.last_played.unwrap_or(i.created_at)));
        rest.truncate(5);
        if rest.is_empty() {
            return div().into_any_element();
        }

        let mut cards = div()
            .id("recent-cards")
            .flex()
            .gap_3()
            .overflow_x_scroll()
            .pb(px(4.0));
        for instance in &rest {
            let mod_count = self.mod_counts.get(&instance.id).copied().unwrap_or(0);

            cards = cards.child(
                ui::glass_card(theme)
                    .id(SharedString::from(format!("recent-{}", instance.id)))
                    .w(px(224.0)) // w-56
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .rounded(px(12.0)) // rounded-xl
                    .p(px(16.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .h(px(36.0)) // h-9
                                    .w(px(36.0))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(8.0)) // rounded-lg
                                    .border_1()
                                    .border_color(theme::white(0.08))
                                    .bg(theme::white(0.05))
                                    .child(
                                        icons::icon_at(icons::STACK, 16.0).text_color(NEUTRAL_400),
                                    ),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(px(14.0))
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(WHITE_HERO)
                                            .child(instance.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(px(11.0))
                                            .text_color(NEUTRAL_500)
                                            .child(format!(
                                                "{} · {}",
                                                loader_tag(instance),
                                                instance.minecraft_version
                                            )),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .text_size(px(11.0))
                            .text_color(NEUTRAL_500)
                            .child(div().child(format!("{mod_count} mods")))
                            .child(div().child(match instance.last_played {
                                Some(_) => format::time_ago(instance.last_played),
                                None => "Never played".to_string(),
                            })),
                    )
                    .child(
                        div()
                            .w_full()
                            .children(self.card_buttons.get(&instance.id).cloned()),
                    ),
            );
        }

        div()
            .child(
                div()
                    .mb(px(14.0)) // mb-3.5
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(13.0))
                            .font_weight(FontWeight::BOLD)
                            .text_color(NEUTRAL_400)
                            .child("RECENT INSTANCES"),
                    ),
            )
            .child(cards)
            .into_any_element()
    }
}

impl Render for PlayPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_6()
            .pt(px(8.0)) // pt-2
            .child(
                div()
                    .child(
                        div()
                            .mb(px(6.0)) // mb-1.5
                            .text_size(px(36.0)) // text-4xl
                            .font_weight(FontWeight::EXTRA_BOLD)
                            .text_color(WHITE_HERO)
                            .child("Ready to play?"),
                    )
                    .child(
                        div()
                            .text_size(px(14.0))
                            .text_color(NEUTRAL_400)
                            .child("Launch Minecraft and jump back into your world."),
                    ),
            )
            .child(self.banner(cx))
            .child(self.recent_instances(cx))
    }
}

/// page.tsx's `StatTile` — `rounded-xl border border-white/7 bg-white/3
/// px-4 py-3`, with the uppercase label, the bold value and the optional
/// success/warning sub line.
fn stat_tile(
    label: String,
    value: String,
    sub: Option<String>,
    ok: Option<bool>,
) -> gpui::Stateful<gpui::Div> {
    let theme = Theme::obsidian();
    let mut tile = div()
        .id(SharedString::from(format!("stat-{label}")))
        .flex_1()
        .rounded(px(12.0))
        .border_1()
        .border_color(theme::white(0.07))
        .bg(theme::white(0.03))
        .px(px(16.0))
        .py(px(12.0))
        .child(
            div()
                .text_size(px(10.0))
                .font_weight(FontWeight::BOLD)
                .text_color(NEUTRAL_500)
                .child(label),
        )
        .child(
            div()
                .mt(px(6.0)) // mt-1.5
                .truncate()
                .text_size(px(15.0))
                .font_weight(FontWeight::BOLD)
                .text_color(WHITE_HERO)
                .child(value),
        );
    if let Some(sub) = sub {
        let color = if ok.unwrap_or(false) {
            theme.success
        } else {
            theme.warning
        };
        tile = tile.child(
            div()
                .mt(px(2.0)) // mt-0.5
                .truncate()
                .text_size(px(11.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(color)
                .child(sub),
        );
    }
    tile
}
