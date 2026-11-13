//! The instance settings dialog — a direct port of
//! `src/components/instances/instance-settings-dialog.tsx`, opened from the
//! instances page's card hover row and list rows (the wrench "Configure"
//! buttons). Like the other management dialogs it renders off the shell
//! root from a `ModalStore` spec (`SettingsSpec`, which carries the whole
//! instance the React dialog received as its prop), recreated per instance
//! and dropped on close.
//!
//! The two gpui-component widgets land here, themed by
//! `theme::sync_component_theme` (the obsidian palette mirrored into the
//! component layer's own global at startup): the auto-update toggle is
//! `component::Switch`, the memory allocation the `component::Slider`
//! (`SliderState` from gpui-base; its min/max are constructor-set only, so
//! the state entity is recreated once the real `get_system_memory_info`
//! total arrives — the 16384 fallback matches the React dialog's default
//! `totalMb`).
//!
//! Deferred with their reasons:
//! - the notes textarea renders as a single-line field — the shared
//!   `TextField` is single-line (IME/caret work); a multiline variant
//!   comes with the input upgrades,
//! - the group field has no datalist suggestions (the React dialog
//!   suggests the other instances' groups via `<datalist>`),
//! - "or drag an image onto it" — gpui-pre's drop events are internal
//!   element drags only; OS file-drop events are the blocker, so the hint
//!   line is left out until then,
//! - the number inputs are plain text fields parsed on save (typing a
//!   non-number there yields `null`, where the browser input refused it).
//!
//! The icon preview decodes `instance.icon` (a `data:image/…;base64,…`
//! URL written by `set_instance_icon*`) into a `gpui::Image` — the
//! instances page's own icons stay the letter fallback until the
//! card-thumbnails slice lands; this helper is what they will reuse.

use std::sync::Arc;

use gpui::{
    Context, Entity, FontWeight, Image, ImageFormat, ObjectFit, Subscription, Window, div, img,
    prelude::*, px,
};

use backend::commands;
use backend::error::AppError;
use backend::models::instance::MinecraftInstance;

use crate::bridge;
use crate::icons;
use crate::instances::drop_shadow;
use crate::modals;
use crate::text_input::{FieldStyle, TextField};
use crate::theme::{self, Theme};
use crate::toasts;
use crate::ui;

use gpui::component::slider::{Slider, SliderEvent, SliderState, SliderValue};
use gpui::component::switch::Switch;

/// The memory slider's bounds, straight from the React range input.
const MEMORY_MIN_MB: f32 = 512.0;
const MEMORY_STEP_MB: f32 = 256.0;
/// The slider's default position when the instance has no explicit
/// allocation (`value={memoryMb ?? Math.min(4096, totalMb)}`).
const MEMORY_DEFAULT_CEILING_MB: u32 = 4096;
/// `memory?.total_mb ?? 16384` — the value used until the query lands.
const MEMORY_TOTAL_FALLBACK_MB: u64 = 16384;
/// `max-h-[60vh]` of the default 1280×800 window (the window min is
/// 640px, so this stays an upper bound everywhere).
const FIELDS_AREA_MAX_H: f32 = 480.0;

pub struct InstanceSettingsDialog {
    /// The pristine instance the spec carried — the save payload's base.
    instance: MinecraftInstance,
    name: Entity<TextField>,
    group: Entity<TextField>,
    java_args: Entity<TextField>,
    notes: Entity<TextField>,
    pre_launch: Entity<TextField>,
    post_exit: Entity<TextField>,
    width: Entity<TextField>,
    height: Entity<TextField>,
    /// `javaPath` state — `None` runs the launcher default (the React
    /// state starts `""` and becomes `null` on save).
    java_path: Option<String>,
    /// `memoryMb` — `None` = "Java default" until the user touches the
    /// slider (`memory_default`), then the last slider value.
    memory_mb: Option<u32>,
    /// False once the user moved the slider (the range input sets a
    /// number on the first `onChange`).
    memory_default: bool,
    slider: Entity<SliderState>,
    /// Replaced when the slider state is recreated for the real total.
    slider_sub: Subscription,
    /// `totalMb` — refreshed from `get_system_memory_info`.
    total_mb: u64,
    auto_update: bool,
    /// `iconPreview` — the current data URL (the instance's at open,
    /// swapped on a successful upload).
    icon_preview: Option<String>,
    /// The same URL decoded once for `img()` (a data URL would otherwise
    /// go through gpui's HTTP asset loader and fail).
    icon_image: Option<Arc<Image>>,
    icon_uploading: bool,
    busy: bool,
    error: Option<String>,
    scroll: gpui::ScrollHandle,
}

impl InstanceSettingsDialog {
    pub fn new(spec: &modals::SettingsSpec, cx: &mut Context<Self>) -> Self {
        let instance = &spec.instance;

        let make_field = |cx: &mut Context<Self>, text: String, placeholder: &'static str| {
            let field = cx.new(|cx| {
                let mut field = TextField::new(cx, placeholder);
                if !text.is_empty() {
                    field.set_text(text);
                }
                field
            });
            cx.observe(&field, |_, _, cx| cx.notify()).detach();
            field
        };

        let name = make_field(cx, instance.name.clone(), "");
        let group = make_field(cx, instance.group.clone().unwrap_or_default(), "Ungrouped");
        let java_args = make_field(
            cx,
            instance.java_args.clone().unwrap_or_default(),
            "-XX:+UseG1GC",
        );
        let notes = make_field(
            cx,
            instance.notes.clone().unwrap_or_default(),
            "Anything worth remembering about this instance...",
        );
        let pre_launch = make_field(
            cx,
            instance.pre_launch_command.clone().unwrap_or_default(),
            "Runs before Minecraft starts",
        );
        let post_exit = make_field(
            cx,
            instance.post_exit_command.clone().unwrap_or_default(),
            "Runs after Minecraft closes",
        );
        let width = make_field(
            cx,
            instance
                .window_width
                .map(|v| v.to_string())
                .unwrap_or_default(),
            "Width",
        );
        let height = make_field(
            cx,
            instance
                .window_height
                .map(|v| v.to_string())
                .unwrap_or_default(),
            "Height",
        );

        let total_mb = MEMORY_TOTAL_FALLBACK_MB;
        let slider = cx.new(|_| {
            build_slider(
                total_mb,
                instance
                    .memory_mb
                    .unwrap_or(MEMORY_DEFAULT_CEILING_MB.min(total_mb as u32)),
            )
        });
        let slider_sub = cx.subscribe(&slider, Self::on_slider_event);

        // `useSystemMemory()` — the React query resolves in the same
        // open-frame window; the slider's max is constructor-set, so the
        // real total recreates it below.
        let task = bridge::command(cx, |_| async move {
            commands::system::get_system_memory_info().await
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|e| {
                Err(AppError::new(
                    "Couldn't read the system memory.",
                    e.to_string(),
                ))
            });
            let Ok(info) = result else {
                return;
            };
            this.update(cx, |this, cx| {
                this.total_mb = info.total_mb;
                // Keep whatever the user has already picked; otherwise
                // recompute the default against the real total.
                let value = this.memory_mb.unwrap_or_else(|| {
                    MEMORY_DEFAULT_CEILING_MB.min(info.total_mb.min(u64::from(u32::MAX)) as u32)
                });
                this.slider = cx.new(|_| build_slider(info.total_mb, value));
                this.slider_sub = cx.subscribe(&this.slider, Self::on_slider_event);
                cx.notify();
            })
            .ok();
        })
        .detach();

        let icon_preview = instance.icon.clone();
        let icon_image = icon_preview.as_deref().and_then(decode_data_url);

        Self {
            instance: instance.clone(),
            name,
            group,
            java_args,
            notes,
            pre_launch,
            post_exit,
            width,
            height,
            java_path: instance.java_path.clone(),
            memory_mb: instance.memory_mb,
            memory_default: instance.memory_mb.is_none(),
            slider,
            slider_sub,
            total_mb,
            auto_update: instance.auto_update_content,
            icon_preview,
            icon_image,
            icon_uploading: false,
            busy: false,
            error: None,
            scroll: gpui::ScrollHandle::new(),
        }
    }

    /// The slider's `Change` ticks — the range input's `onChange`.
    fn on_slider_event(
        &mut self,
        _state: Entity<SliderState>,
        event: &SliderEvent,
        cx: &mut Context<Self>,
    ) {
        if let SliderEvent::Change(SliderValue::Single(value)) = event {
            // The range input snaps to `step` — do the same for drags
            // between ticks.
            let snapped = (value / MEMORY_STEP_MB).round() * MEMORY_STEP_MB;
            self.memory_mb = Some(snapped.round().max(MEMORY_MIN_MB) as u32);
            self.memory_default = false;
            cx.notify();
        }
    }

    /// `openPath(instance.game_dir)` — the header's "Open folder"; the
    /// error joins the dialog's own error line.
    fn open_folder(&mut self, cx: &mut Context<Self>) {
        let dir = self.instance.game_dir.clone();
        let task = bridge::command(
            cx,
            move |_| async move { commands::system::open_path(&dir) },
        );
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|e| {
                Err(AppError::new("Couldn't open that folder.", e.to_string()))
            });
            this.update(cx, |this, cx| {
                if let Err(err) = result {
                    this.error = Some(err.message);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// "Change icon..." — `openDialog({ filters: [image extensions] })`
    /// then `api.instances.setIcon`; "Uploading..." gates the button.
    fn pick_icon(&mut self, cx: &mut Context<Self>) {
        if self.icon_uploading {
            return;
        }
        let pick = cx.background_executor().spawn(async move {
            rfd::AsyncFileDialog::new()
                .add_filter("Image", &["png", "jpg", "jpeg", "gif", "webp"])
                .pick_file()
                .await
                .map(|handle| handle.path().to_string_lossy().into_owned())
        });
        cx.spawn(async move |this, cx| {
            let Some(path) = pick.await else {
                return; // user cancelled
            };
            let task = this
                .update(cx, |this, cx| {
                    this.icon_uploading = true;
                    this.error = None;
                    cx.notify();
                    let id = this.instance.id.clone();
                    bridge::command(cx, move |state| async move {
                        commands::instances::set_instance_icon(&state, id, path).await
                    })
                })
                .ok();
            let Some(task) = task else {
                return;
            };
            let result = task
                .await
                .unwrap_or_else(|e| Err(AppError::new("Couldn't set that icon.", e.to_string())));
            this.update(cx, |this, cx| {
                this.icon_uploading = false;
                match result {
                    Ok(updated) => {
                        this.icon_preview = updated.icon;
                        this.icon_image = this.icon_preview.as_deref().and_then(decode_data_url);
                    }
                    Err(err) => this.error = Some(err.message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// "Browse..." — pick any file, keep it only if `validateJavaPath`
    /// accepts it (the React dialog discards invalid picks with an error
    /// line, never a toast).
    fn browse_java(&mut self, cx: &mut Context<Self>) {
        let pick = cx.background_executor().spawn(async move {
            rfd::AsyncFileDialog::new()
                .pick_file()
                .await
                .map(|handle| handle.path().to_string_lossy().into_owned())
        });
        cx.spawn(async move |this, cx| {
            let Some(path) = pick.await else {
                return; // user cancelled
            };
            // `bridge::command` needs the main thread; grab the task
            // handle through `AsyncApp::update`.
            let task = cx.update(|cx| {
                let candidate = path.clone();
                bridge::command(cx, move |_| async move {
                    commands::launch::validate_java_path(candidate).await
                })
            });
            let result = task
                .await
                .unwrap_or_else(|e| Err(AppError::new("Java validation failed.", e.to_string())));
            this.update(cx, |this, cx| {
                match result {
                    Ok(valid) => {
                        if valid {
                            this.java_path = Some(path);
                        } else {
                            this.error =
                                Some("That doesn't look like a valid Java executable.".to_string());
                        }
                    }
                    Err(err) => this.error = Some(err.message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `updateInstance.mutateAsync({ ...instance, ... })` — every field
    /// trimmed/null-ed like the React save; success toasts and closes
    /// (the modal-close refresh brings the pages the new data).
    fn save(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let name = self.name.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();

        let width = self.width.read(cx).value().trim().parse::<u32>().ok();
        let height = self.height.read(cx).value().trim().parse::<u32>().ok();
        let java_args = self.java_args.read(cx).value().trim().to_string();
        let group = self.group.read(cx).value().trim().to_string();
        let notes = self.notes.read(cx).value().trim().to_string();
        let pre_launch = self.pre_launch.read(cx).value().trim().to_string();
        let post_exit = self.post_exit.read(cx).value().trim().to_string();
        let memory_mb = if self.memory_default {
            None
        } else {
            self.memory_mb
        };

        let mut updated = self.instance.clone();
        updated.name = name;
        updated.java_path = self.java_path.clone();
        updated.java_args = non_empty(java_args);
        updated.memory_mb = memory_mb;
        updated.window_width = width;
        updated.window_height = height;
        updated.auto_update_content = self.auto_update;
        updated.group = non_empty(group);
        updated.notes = non_empty(notes);
        updated.pre_launch_command = non_empty(pre_launch);
        updated.post_exit_command = non_empty(post_exit);

        let task = bridge::command(cx, move |state| async move {
            commands::instances::update_instance(&state, updated).await
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|e| {
                Err(AppError::new(
                    "Couldn't save these settings.",
                    e.to_string(),
                ))
            });
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(_) => {
                        toasts::toast_success(cx, "Instance settings saved.");
                        modals::close_settings(cx);
                    }
                    Err(err) => this.error = Some(err.message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

/// `value.trim() || null` — the React save's empty-string-to-null shape.
fn non_empty(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}

/// The slider state for one total: the React range input's
/// `min={512} max={totalMb} step={256} value={…}`.
fn build_slider(total_mb: u64, value_mb: u32) -> SliderState {
    let max = (total_mb.max(MEMORY_MIN_MB as u64) as f32).min(u32::MAX as f32);
    SliderState::new()
        .min(MEMORY_MIN_MB)
        .max(max)
        .step(MEMORY_STEP_MB)
        .default_value(value_mb as f32)
}

/// `data:image/<subtype>;base64,<bytes>` → a decodable `gpui::Image` (the
/// formats `set_instance_icon*` writes).
fn decode_data_url(data_url: &str) -> Option<Arc<Image>> {
    use base64::Engine as _;
    let rest = data_url.strip_prefix("data:image/")?;
    let (subtype, data) = rest.split_once(";base64,")?;
    let format = match subtype {
        "png" => ImageFormat::Png,
        "jpeg" | "jpg" => ImageFormat::Jpeg,
        "webp" => ImageFormat::Webp,
        "gif" => ImageFormat::Gif,
        _ => return None,
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    Some(Arc::new(Image::from_bytes(format, bytes)))
}

/// A label — `text-xs font-medium text-muted`.
fn label(text: &'static str, theme: Theme) -> gpui::AnyElement {
    div()
        .text_size(px(12.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.muted)
        .child(text)
        .into_any_element()
}

impl gpui::Render for InstanceSettingsDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *Theme::get(cx);
        let name_value = self.name.read(cx).value().trim().to_string();
        let width_text = self.width.read(cx).value().trim().to_string();
        let height_text = self.height.read(cx).value().trim().to_string();

        // The header's "Open folder" (`text-xs text-muted hover:text-foreground`
        // with the 13px folder icon).
        let open_folder = div()
            .id("settings-open-folder")
            .flex()
            .items_center()
            .gap(px(6.0)) // gap-1.5
            .text_size(px(12.0))
            .text_color(theme.muted)
            .hover(move |style| style.text_color(theme.foreground))
            .on_click(cx.listener(|this, _, _, cx| this.open_folder(cx)))
            .child(icons::icon_at(icons::FOLDER_OPEN, 13.0))
            .child("Open folder");

        // The 56px icon preview — `img` when there's an icon, the
        // `bg-white/5` letter box otherwise (the React dialog's null-icon
        // branch; the cards' own icons are the thumbnails slice).
        let icon_box: gpui::AnyElement = match (&self.icon_image, &self.icon_preview) {
            (Some(image), _) => div()
                .h(px(56.0)) // h-14 w-14
                .w(px(56.0))
                .rounded(px(16.0)) // rounded-2xl
                .overflow_hidden()
                .child(img(image.clone()).size_full().object_fit(ObjectFit::Cover))
                .into_any_element(),
            (None, _) => div()
                .h(px(56.0))
                .w(px(56.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(16.0))
                .bg(theme::white(0.05))
                .text_size(px(18.0)) // text-lg
                .font_weight(FontWeight::BOLD)
                .text_color(theme.muted)
                .child(
                    self.instance
                        .name
                        .chars()
                        .next()
                        .map(|c| c.to_ascii_uppercase().to_string())
                        .unwrap_or_default(),
                )
                .into_any_element(),
        };

        let change_icon = ui::button(
            "settings-change-icon",
            theme,
            ui::ButtonVariant::Ghost,
            ui::ButtonSize::Sm,
            self.icon_uploading,
        )
        .on_click(cx.listener(|this, _, _, cx| this.pick_icon(cx)))
        .child(if self.icon_uploading {
            "Uploading..."
        } else {
            "Change icon..."
        });

        // The java executable row: a readonly display box (the React
        // `<input readOnly>`) + Browse + Clear.
        let java_display = div()
            .flex_1()
            .h(px(36.0))
            .flex()
            .items_center()
            .rounded(px(8.0)) // rounded-lg
            .px(px(12.0)) // px-3
            .text_size(px(12.0)) // text-xs
            .text_color(theme.muted)
            .bg(theme::white(0.03)) // glass-input chrome
            .border_1()
            .border_color(theme.border)
            .child(
                self.java_path
                    .clone()
                    .unwrap_or_else(|| "Use launcher default".to_string()),
            );

        // The auto-update row's switch — the controlled gpui-component
        // Switch (checked fill = our accent via the synced theme).
        let switch_entity = cx.entity().downgrade();
        let auto_update_switch = Switch::new("settings-auto-update-switch")
            .checked(self.auto_update)
            .on_change(move |checked, _, cx| {
                switch_entity
                    .update(cx, |this, cx| {
                        this.auto_update = *checked;
                        cx.notify();
                    })
                    .ok();
            });

        // The memory readout — `"4.0 GB" / "16 GB total"` (the React
        // `${(memoryMb / 1024).toFixed(1)}` labels).
        let memory_label = match self.memory_mb {
            Some(mb) => format!("{:.1} GB", f64::from(mb) / 1024.0),
            None => "Java default".to_string(),
        };
        let total_label = format!("{:.0} GB total", self.total_mb as f64 / 1024.0);

        // A labeled row wrapper keeps the `gap-3` column's shapes together.
        let mut fields = div()
            .id("settings-fields")
            .mt(px(20.0)) // mt-5
            .flex()
            .flex_col()
            .gap(px(12.0)) // gap-3
            .max_h(px(FIELDS_AREA_MAX_H))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .pr(px(4.0)); // pr-1

        // Name
        fields = fields.child(div().flex().flex_col().child(label("Name", theme)).child(
            div().mt(px(4.0)).child(self.name.update(cx, |field, cx| {
                field.render_styled(
                    window,
                    cx,
                    &FieldStyle {
                        id: "settings-name-input",
                        key_context: "SettingsNameInput",
                        height: 36.0,
                        rounded: 8.0,
                        text_size: 14.0,
                        padding_x: 12.0,
                        ..FieldStyle::default()
                    },
                )
            })),
        ));

        // Group (the datalist suggestions are deferred)
        fields = fields.child(div().flex().flex_col().child(label("Group", theme)).child(
            div().mt(px(4.0)).child(self.group.update(cx, |field, cx| {
                field.render_styled(
                    window,
                    cx,
                    &FieldStyle {
                        id: "settings-group-input",
                        key_context: "SettingsGroupInput",
                        height: 36.0,
                        rounded: 8.0,
                        text_size: 14.0,
                        padding_x: 12.0,
                        ..FieldStyle::default()
                    },
                )
            })),
        ));

        // Java executable
        fields = fields.child(
            div()
                .flex()
                .flex_col()
                .child(label("Java executable", theme))
                .child(
                    div()
                        .mt(px(4.0))
                        .flex()
                        .items_center()
                        .gap(px(8.0)) // gap-2
                        .child(java_display)
                        .child(
                            ui::button(
                                "settings-java-browse",
                                theme,
                                ui::ButtonVariant::Ghost,
                                ui::ButtonSize::Sm,
                                false,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.browse_java(cx)))
                            .child("Browse..."),
                        )
                        .when(self.java_path.is_some(), |row| {
                            row.child(
                                ui::button(
                                    "settings-java-clear",
                                    theme,
                                    ui::ButtonVariant::Ghost,
                                    ui::ButtonSize::Sm,
                                    false,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.java_path = None;
                                    cx.notify();
                                }))
                                .child("Clear"),
                            )
                        }),
                ),
        );

        // Extra JVM arguments
        fields = fields.child(
            div()
                .flex()
                .flex_col()
                .child(label("Extra JVM arguments", theme))
                .child(
                    div()
                        .mt(px(4.0))
                        .child(self.java_args.update(cx, |field, cx| {
                            field.render_styled(
                                window,
                                cx,
                                &FieldStyle {
                                    id: "settings-java-args-input",
                                    key_context: "SettingsJavaArgsInput",
                                    height: 36.0,
                                    rounded: 8.0,
                                    text_size: 14.0,
                                    padding_x: 12.0,
                                    ..FieldStyle::default()
                                },
                            )
                        })),
                ),
        );

        // Memory allocation (the gpui-component Slider)
        fields = fields.child(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(label("Memory allocation", theme))
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(theme.muted)
                                .child(format!("{memory_label} / {total_label}")),
                        ),
                )
                .child(div().mt(px(8.0)).child(Slider::new(&self.slider))),
        );

        // Window size
        fields = fields.child(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(label("Window size", theme))
                        .when(!width_text.is_empty() || !height_text.is_empty(), |head| {
                            head.child(
                                div()
                                    .id("settings-size-default")
                                    .text_size(px(11.0))
                                    .text_color(theme.muted)
                                    .hover(move |style| style.text_color(theme.foreground))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.width
                                            .update(cx, |field, _| field.set_text(String::new()));
                                        this.height
                                            .update(cx, |field, _| field.set_text(String::new()));
                                        cx.notify();
                                    }))
                                    .child("Use default"),
                            )
                        }),
                )
                .child(
                    div()
                        .mt(px(4.0))
                        .flex()
                        .items_center()
                        .gap(px(8.0)) // gap-2
                        .child(div().flex_1().child(self.width.update(cx, |field, cx| {
                            field.render_styled(
                                window,
                                cx,
                                &FieldStyle {
                                    id: "settings-width-input",
                                    key_context: "SettingsWidthInput",
                                    height: 36.0,
                                    rounded: 8.0,
                                    text_size: 14.0,
                                    padding_x: 12.0,
                                    ..FieldStyle::default()
                                },
                            )
                        })))
                        .child(div().text_size(px(12.0)).text_color(theme.muted).child("×"))
                        .child(div().flex_1().child(self.height.update(cx, |field, cx| {
                            field.render_styled(
                                window,
                                cx,
                                &FieldStyle {
                                    id: "settings-height-input",
                                    key_context: "SettingsHeightInput",
                                    height: 36.0,
                                    rounded: 8.0,
                                    text_size: 14.0,
                                    padding_x: 12.0,
                                    ..FieldStyle::default()
                                },
                            )
                        }))),
                ),
        );

        // Auto-update content (the Switch row)
        fields = fields.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .rounded(px(12.0)) // rounded-xl
                .border_1()
                .border_color(theme::white(0.08))
                .bg(theme::white(0.03))
                .px(px(12.0)) // px-3
                .py(px(10.0)) // py-2.5
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(px(12.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.foreground)
                                .child("Auto-update content"),
                        )
                        .child(div().text_size(px(11.0)).text_color(theme.muted).child(
                            "Update mods, resource packs, and shaders before every launch.",
                        )),
                )
                .child(auto_update_switch),
        );

        // Pre/post-launch commands (`font-mono text-xs` fields)
        fields = fields.child(
            div()
                .flex()
                .flex_col()
                .child(label("Pre-launch command", theme))
                .child(
                    div()
                        .mt(px(4.0))
                        .child(self.pre_launch.update(cx, |field, cx| {
                            field.render_styled(
                                window,
                                cx,
                                &FieldStyle {
                                    id: "settings-pre-launch-input",
                                    key_context: "SettingsPreLaunchInput",
                                    height: 32.0,
                                    rounded: 8.0,
                                    text_size: 12.0,
                                    padding_x: 12.0,
                                    mono: true,
                                    ..FieldStyle::default()
                                },
                            )
                        })),
                ),
        );

        fields = fields.child(
            div()
                .flex()
                .flex_col()
                .child(label("Post-exit command", theme))
                .child(
                    div().mt(px(4.0)).child(self.post_exit.update(cx, |field, cx| {
                        field.render_styled(
                            window,
                            cx,
                            &FieldStyle {
                                id: "settings-post-exit-input",
                                key_context: "SettingsPostExitInput",
                                height: 32.0,
                                rounded: 8.0,
                                text_size: 12.0,
                                padding_x: 12.0,
                                mono: true,
                                ..FieldStyle::default()
                            },
                        )
                    })),
                )
                .child(
                    div()
                        .mt(px(4.0))
                        .text_size(px(11.0))
                        .text_color(theme.muted)
                        .child(
                            "Both run in a shell with INST_NAME, INST_ID, INST_DIR, INST_JAVA, and INST_JAVA_ARGS available.",
                        ),
                ),
        );

        // Notes (single-line — the multiline textarea is deferred)
        fields = fields.child(div().flex().flex_col().child(label("Notes", theme)).child(
            div().mt(px(4.0)).child(self.notes.update(cx, |field, cx| {
                field.render_styled(
                    window,
                    cx,
                    &FieldStyle {
                        id: "settings-notes-input",
                        key_context: "SettingsNotesInput",
                        height: 36.0,
                        rounded: 8.0,
                        text_size: 14.0,
                        padding_x: 12.0,
                        ..FieldStyle::default()
                    },
                )
            })),
        ));

        let error_line = self.error.clone().map(|message| {
            div()
                .mt(px(12.0)) // mt-3
                .text_size(px(12.0))
                .text_color(theme.danger)
                .child(message)
        });

        div()
            .id("instance-settings-backdrop")
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
                    .w(px(512.0)) // max-w-lg
                    .flex()
                    .flex_col()
                    .p(px(24.0)) // p-6
                    .rounded(px(16.0)) // rounded-2xl (glass-panel)
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .shadow(vec![drop_shadow(0.25, 25.0, 50.0, -12.0)])
                    .child(
                        // Header: title + Open folder
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(16.0)) // text-base
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(format!("Configure {}", self.instance.name)),
                            )
                            .child(open_folder),
                    )
                    .child(
                        // Icon row
                        div()
                            .mt(px(16.0)) // mt-4
                            .flex()
                            .items_center()
                            .gap(px(16.0)) // gap-4
                            .child(icon_box)
                            .child(change_icon),
                    )
                    .child(fields)
                    .children(error_line)
                    .child(
                        div()
                            .mt(px(24.0)) // mt-6
                            .flex()
                            .justify_end()
                            .gap(px(8.0)) // gap-2
                            .child(
                                ui::button(
                                    "settings-cancel",
                                    theme,
                                    ui::ButtonVariant::Ghost,
                                    ui::ButtonSize::Sm,
                                    false,
                                )
                                .on_click(cx.listener(|_, _, _, cx| modals::close_settings(cx)))
                                .child("Cancel"),
                            )
                            .child(
                                ui::button(
                                    "settings-save",
                                    theme,
                                    ui::ButtonVariant::Primary,
                                    ui::ButtonSize::Sm,
                                    self.busy || name_value.is_empty(),
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.save(cx)))
                                .child(if self.busy {
                                    "Saving..."
                                } else {
                                    "Save"
                                }),
                            ),
                    ),
            )
            .into_any_element()
    }
}
