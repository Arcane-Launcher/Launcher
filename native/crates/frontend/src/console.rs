//! InstanceConsole — direct port of `instance-console.tsx`: a live tail of
//! an instance's console output (stdout+stderr), seeded from the backend's
//! buffered last session so a crash is still inspectable even if this
//! panel wasn't open when it happened.
//!
//! Lines stream through the bridge's `LogStore` global (the drain batches
//! at 100ms exactly like FLUSH_MS did); each open console tracks what it
//! has already applied so "Clear" only resets what's shown here, never the
//! backend buffer. Stats poll every 2s (`get_process_stats`).
//!
//! Deferred with their features (documented in MIGRATION.md): "Share log"
//! (needs the share-log backend), "Find the problem" (CrashHelperDialog).

use gpui::{
    ClipboardItem, Context, FontWeight, ScrollHandle, Window, div, prelude::*, px, relative,
};

use backend::commands;
use backend::services::minecraft::running::ProcessStats;

use crate::{
    bridge::{self, ConsoleLine, LogStore, LogTone, RunningStore},
    icons, modals, theme,
    theme::Theme,
    toasts, ui,
};

const STATS_POLL_MS: u64 = 2000;
/// instance-console.tsx: `atBottom = scrollHeight - scrollTop - clientHeight < 24`.
const AT_BOTTOM_PX: f32 = 24.0;

/// Console text uses the mono stack — Tailwind's default `font-mono`
/// resolves to the system mono font (Cascadia Mono on Windows 10+).
pub struct InstanceConsole {
    instance_id: String,
    name: String,
    /// The view's own copy of the log (re-seeded from the backend buffer
    /// on open, cleared by "Clear" — the React component kept per-mount
    /// state too).
    lines: Vec<ConsoleLine>,
    /// Last `LogStore` line id applied to `lines`.
    seen: u64,
    /// `LogStore` generation for this instance — a new session resets it.
    generation: u64,
    auto_scroll: bool,
    stats: Option<ProcessStats>,
    scroll: ScrollHandle,
}

impl InstanceConsole {
    pub fn new(spec: &modals::ConsoleSpec, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            instance_id: spec.instance_id.clone(),
            name: spec.name.clone(),
            lines: Vec::new(),
            seen: 0,
            generation: 0,
            auto_scroll: true,
            stats: None,
            scroll: ScrollHandle::new(),
        };

        // Seed from the backend's buffered last session.
        let instance_id = this.instance_id.clone();
        let task = bridge::command(cx, move |state| async move {
            commands::launch::get_instance_log(&state, instance_id).await
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(initial)) = task.await {
                this.update(cx, |this, cx| {
                    for (line, stream) in initial.iter().map(|l| (l.line.clone(), l.stream.clone()))
                    {
                        let tone = LogStore::classify(&line, &stream);
                        this.push_line(ConsoleLine { id: 0, line, tone });
                    }
                    // The global store may already hold live lines for this
                    // session (they streamed in before this panel opened) —
                    // skip past them so the seed isn't doubled.
                    let latest = cx
                        .global::<LogStore>()
                        .lines_for(&this.instance_id)
                        .last()
                        .map(|l| l.id)
                        .unwrap_or(0);
                    this.seen = this.seen.max(latest);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();

        // Live updates: the flushed log batches + running-state changes.
        cx.observe_global::<LogStore>(|this, cx| this.sync_logs(cx))
            .detach();
        cx.observe_global::<RunningStore>(|_, cx| cx.notify())
            .detach();

        this.poll_stats(cx);
        this
    }

    fn push_line(&mut self, line: ConsoleLine) {
        self.lines.push(line);
        let overflow = self.lines.len().saturating_sub(bridge::MAX_CONSOLE_LINES);
        self.lines.drain(0..overflow);
    }

    /// Applies the global log store's new lines for this instance.
    fn sync_logs(&mut self, cx: &mut Context<Self>) {
        let store = cx.global::<LogStore>();
        let generation = store.generation(&self.instance_id);
        let mut changed = generation != self.generation;
        if changed {
            self.generation = generation;
            self.lines.clear();
            self.seen = 0;
        }
        let global_lines = store.lines_for(&self.instance_id);
        let mut seen = self.seen;
        for line in global_lines.iter().filter(|line| line.id > self.seen) {
            self.lines.push(line.clone());
            seen = seen.max(line.id);
            changed = true;
        }
        self.seen = seen;
        if changed {
            let overflow = self.lines.len().saturating_sub(bridge::MAX_CONSOLE_LINES);
            self.lines.drain(0..overflow);
            cx.notify();
        }
    }

    /// The 2s stats poll — only while the instance is actually running
    /// (`get_process_stats` answers `None` otherwise).
    fn poll_stats(&mut self, cx: &mut Context<Self>) {
        let instance_id = self.instance_id.clone();
        cx.spawn(async move |this, cx| {
            loop {
                let running = cx.update(|cx| cx.global::<RunningStore>().is_running(&instance_id));
                if !running {
                    this.update(cx, |this, cx| {
                        if this.stats.take().is_some() {
                            cx.notify();
                        }
                    })
                    .ok();
                    return;
                }
                // `bridge::command` needs the main thread; grab the task handle
                // through `AsyncApp::update`.
                let task = cx.update(|cx| {
                    let poll_id = instance_id.clone();
                    bridge::command(cx, move |state| async move {
                        commands::launch::get_process_stats(&state, poll_id).await
                    })
                });
                let stats = task.await.ok().and_then(|r| r.ok()).flatten();
                this.update(cx, |this, cx| {
                    let changed = match (this.stats, stats) {
                        (Some(a), Some(b)) => {
                            a.cpu_percent != b.cpu_percent || a.memory_mb != b.memory_mb
                        }
                        (None, None) => false,
                        _ => true,
                    };
                    if changed {
                        this.stats = stats;
                        cx.notify();
                    }
                })
                .ok();
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(STATS_POLL_MS))
                    .await;
            }
        })
        .detach();
    }

    fn copy_all(&mut self, cx: &mut Context<Self>) {
        if self.lines.is_empty() {
            return;
        }
        let text = self
            .lines
            .iter()
            .map(|l| l.line.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        toasts::toast_success(cx, "Log copied to clipboard.");
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        let instance_id = self.instance_id.clone();
        let task = bridge::command(cx, move |state| async move {
            commands::launch::kill_instance(&state, instance_id).await
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|e| {
                Err(backend::error::AppError::new(
                    "Couldn't stop this instance.",
                    e.to_string(),
                ))
            });
            this.update(cx, |_, cx| {
                if let Err(err) = result {
                    toasts::toast_error(cx, err.message);
                }
            })
            .ok();
        })
        .detach();
    }
}

impl Render for InstanceConsole {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *Theme::get(cx);
        let is_running = cx.global::<RunningStore>().is_running(&self.instance_id);

        // Auto-scroll: when following the tail, snap to the bottom before
        // this frame paints (ScrollHandle honors the flag during layout).
        if self.auto_scroll {
            self.scroll.scroll_to_bottom();
        }

        // Header: icon + name + Running badge + live stats.
        let mut header_left = div()
            .flex()
            .min_w_0()
            .items_center()
            .gap_2()
            .child(
                icons::icon_at(icons::TERMINAL_WINDOW, 16.0)
                    .flex_none()
                    .text_color(theme.accent),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(16.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("{} — Console", self.name)),
            )
            .child(
                div()
                    .flex_none()
                    .rounded_full()
                    .px(px(8.0))
                    .py(px(2.0))
                    .text_size(px(10.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .when(is_running, |badge| {
                        badge
                            .bg(theme::tinted_rgba(0x4ADE80, 0.15)) // bg-success/15
                            .text_color(theme.success)
                            .child("Running")
                    })
                    .when(!is_running, |badge| {
                        badge
                            .bg(theme::tinted_rgba(0xFFFFFF, 0.06))
                            .text_color(theme.muted)
                            .child("Not running")
                    }),
            );

        if is_running && let Some(stats) = &self.stats {
            header_left = header_left
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap_1()
                        .text_size(px(11.0))
                        .text_color(theme.muted)
                        .child(icons::icon_at(icons::CUBE, 11.0))
                        .child(format!("{:.0}% CPU", stats.cpu_percent)),
                )
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap_1()
                        .text_size(px(11.0))
                        .text_color(theme.muted)
                        .child(icons::icon_at(icons::STACK, 11.0))
                        .child(format!("{} MB", stats.memory_mb)),
                );
        }

        // The log area.
        let mut log_area = div()
            .id("console-log")
            .mt(px(12.0)) // mt-3
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .rounded(px(12.0)) // rounded-xl
            .border_1()
            .border_color(theme::tinted_rgba(0xFFFFFF, 0.08))
            .bg(theme::rgba_const(0x0A0A0CFF)) // bg-[#0a0a0c]
            .p(px(12.0))
            .font_family(theme::FONT_FAMILY_MONO_FALLBACK)
            .text_size(px(11.0))
            .line_height(relative(1.625)) // leading-relaxed
            .on_scroll(cx.listener(|this, _, _, cx| {
                // Re-evaluate "at bottom" from the last paint's scroll
                // state — auto-scroll releases as soon as the player
                // wheels away from the tail.
                let at_bottom = {
                    let max = f32::from(this.scroll.max_offset().y);
                    let scrolled = -f32::from(this.scroll.offset().y);
                    max <= AT_BOTTOM_PX || scrolled >= max - AT_BOTTOM_PX
                };
                if at_bottom != this.auto_scroll {
                    this.auto_scroll = at_bottom;
                    cx.notify();
                }
            }));

        if self.lines.is_empty() {
            log_area =
                log_area.child(div().text_color(theme.muted).child(
                    "No console output yet — this instance hasn't been launched this session.",
                ));
        } else {
            for line in &self.lines {
                let color = match line.tone {
                    LogTone::Danger => crate::launch::DANGER_TONE,
                    LogTone::Warning => crate::launch::WARNING_TONE,
                    LogTone::Neutral => NEUTRAL_300,
                };
                log_area =
                    log_area.child(div().w_full().text_color(color).child(line.line.clone()));
            }
        }

        // Footer buttons.
        let mut footer_left = div().flex().items_center().gap_2();
        if !self.auto_scroll {
            footer_left = footer_left.child(
                ui::button(
                    "console-jump-bottom",
                    theme,
                    ui::ButtonVariant::Ghost,
                    ui::ButtonSize::Sm,
                    false,
                )
                .child(icons::icon_at(icons::ARROW_CLOCKWISE, 12.0))
                .child("Jump to bottom")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.auto_scroll = true;
                    cx.notify();
                })),
            );
        }

        let mut footer_right = div().flex().items_center().gap_2().child(
            ui::button(
                "console-clear",
                theme,
                ui::ButtonVariant::Ghost,
                ui::ButtonSize::Sm,
                self.lines.is_empty(),
            )
            .child("Clear")
            .when(!self.lines.is_empty(), |button| {
                button.on_click(cx.listener(|this, _, _, cx| {
                    // Only resets what's shown here, never the backend
                    // buffer (this console is re-seeded from it on open).
                    this.lines.clear();
                    let store = cx.global::<LogStore>();
                    this.seen = store
                        .lines_for(&this.instance_id)
                        .last()
                        .map(|l| l.id)
                        .unwrap_or(this.seen);
                    cx.notify();
                }))
            }),
        );

        footer_right = footer_right.child(
            ui::button(
                "console-copy",
                theme,
                ui::ButtonVariant::Ghost,
                ui::ButtonSize::Sm,
                self.lines.is_empty(),
            )
            .child(icons::icon_at(icons::COPY, 12.0))
            .child("Copy")
            .when(!self.lines.is_empty(), |button| {
                button.on_click(cx.listener(|this, _, _, cx| this.copy_all(cx)))
            }),
        );

        if is_running {
            footer_right = footer_right.child(
                ui::button(
                    "console-stop",
                    theme,
                    ui::ButtonVariant::Danger,
                    ui::ButtonSize::Sm,
                    false,
                )
                .child("Stop")
                .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
            );
        }

        // glass-panel flex h-[min(700px,85vh)] w-full max-w-3xl flex-col
        // rounded-2xl p-5
        let panel = ui::glass_panel(theme)
            .id("console-panel")
            .flex()
            .w(px(768.0)) // max-w-3xl
            .h(px(700.0))
            .max_h(relative(0.85)) // h-[min(700px,85vh)]
            .flex_col()
            .rounded(px(16.0))
            .p(px(20.0)) // p-5
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(header_left)
                    .child(
                        div()
                            .id("console-close")
                            .flex_none()
                            .rounded(px(8.0))
                            .p(px(6.0)) // p-1.5
                            .text_color(theme.muted)
                            .hover(move |style| {
                                style
                                    .bg(theme::tinted_rgba(0xFFFFFF, 0.06))
                                    .text_color(theme.foreground)
                            })
                            .on_click(cx.listener(|_, _, _, cx| modals::close_console(cx)))
                            .child(icons::icon_at(icons::X, 14.0)),
                    ),
            )
            .child(log_area)
            .child(
                div()
                    .mt(px(12.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(footer_left)
                    .child(footer_right),
            );

        ui::modal_backdrop("console-backdrop")
            .child(panel)
            .into_any_element()
    }
}

/// Tailwind `text-neutral-300` — the console's default line color.
const NEUTRAL_300: gpui::Rgba = theme::rgba_const(0xD4D4D8FF);
