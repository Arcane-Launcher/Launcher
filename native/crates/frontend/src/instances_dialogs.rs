//! The create / import instance dialogs — direct ports of
//! `src/components/instances/create-instance-dialog.tsx` and
//! `src/components/instances/import-instances-dialog.tsx`, opened from
//! the Instances page's header and empty state. Like the other dialogs
//! they render off the shell root from `ModalStore` flags; both are
//! spec-less (the create flow holds its own form state, the import flow
//! its scan results), created fresh every time they open and dropped on
//! close.
//!
//! Native `<select>` elements (Minecraft version / loader / loader
//! version) become custom menus like the Instances page's sort control —
//! they open upward for the paint-order reason documented there and close
//! on selection or re-clicking the trigger (outside-click close needs the
//! portal machinery — same deferral). The version menu scrolls, like the
//! native list would. The folder pickers use `rfd::AsyncFileDialog`
//! where the Tauri app used `plugin-dialog` — the future runs
//! off-thread, so the UI never blocks while the OS dialog is up.

use std::collections::HashSet;

use gpui::{
    Context, Div, Entity, FontWeight, SharedString, Stateful, Window, div, prelude::*, px, relative,
};

use backend::commands;
use backend::error::AppError;
use backend::models::common::ModLoaderKind;
use backend::models::content::{DiscoveredInstance, ImportSource};
use backend::models::download::{DownloadStatus, DownloadTask};
use backend::models::instance::{CreateInstanceRequest, MinecraftInstance};
use backend::models::minecraft::VersionManifestEntry;

use crate::bridge::{self, DownloadsStore, InstallStageStore};
use crate::format;
use crate::icons;
use crate::instances::drop_shadow;
use crate::modals;
use crate::text_input::{FieldStyle, TextField};
use crate::theme::{self, Theme};
use crate::toasts;
use crate::ui;

/// `loaderLabel` — the confirm step's sentence names the loaders.
fn loader_label(kind: ModLoaderKind) -> &'static str {
    match kind {
        ModLoaderKind::Vanilla => "Vanilla",
        ModLoaderKind::Fabric => "Fabric",
        ModLoaderKind::Quilt => "Quilt",
        ModLoaderKind::Forge => "Forge",
        ModLoaderKind::NeoForge => "NeoForge",
    }
}

/// The `<select>`'s option labels — the React options print the raw ids
/// (`{l}`) under CSS `text-transform: capitalize`, so "neoforge"
/// displays as "Neoforge".
fn loader_option_label(kind: ModLoaderKind) -> &'static str {
    match kind {
        ModLoaderKind::Vanilla => "Vanilla",
        ModLoaderKind::Fabric => "Fabric",
        ModLoaderKind::Quilt => "Quilt",
        ModLoaderKind::Forge => "Forge",
        ModLoaderKind::NeoForge => "Neoforge",
    }
}

/// The dropdown panel — the sort menu's chrome (`ui/dropdown-menu.tsx`:
/// `rounded-xl border-white/10 p-1 shadow-xl`), widened to the select
/// and scrollable for the long version lists.
fn menu_panel(key: &'static str, theme: &Theme) -> Stateful<Div> {
    div()
        .id(SharedString::from(format!("{key}-menu")))
        .absolute()
        .bottom(px(40.0)) // the 36px trigger's top edge + the mt-1 gap
        .left(px(0.0))
        .right(px(0.0))
        .max_h(px(240.0))
        .flex()
        .flex_col()
        .rounded(px(12.0)) // rounded-xl
        .bg(theme.surface)
        .border_1()
        .border_color(theme::white(0.10))
        .p(px(4.0)) // p-1
        .shadow(vec![drop_shadow(0.35, 10.0, 30.0, -10.0)])
        .overflow_y_scroll()
}

/// One dropdown option (the sort menu's item look: `rounded-lg px-2.5
/// py-1.5 text-xs`, selected in accent).
#[allow(clippy::too_many_arguments)]
fn menu_item(
    key: impl Into<SharedString>,
    label: String,
    selected: bool,
    theme: Theme,
    cx: &mut Context<CreateInstanceDialog>,
    on_click: impl Fn(&mut CreateInstanceDialog, &mut Window, &mut Context<CreateInstanceDialog>)
    + 'static,
) -> Stateful<Div> {
    let mut item = div()
        .id(key.into())
        .flex()
        .w_full()
        .items_center()
        .rounded(px(8.0)) // rounded-lg
        .px(px(10.0)) // px-2.5
        .py(px(6.0)) // py-1.5
        .text_size(px(12.0)) // text-xs
        .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)));
    if selected {
        item = item.text_color(theme.accent).bg(theme::white(0.03));
    } else {
        item = item
            .text_color(theme.foreground)
            .hover(move |style| style.bg(theme::white(0.06)));
    }
    item.child(label)
}

/// The versions' error line — `<Icon warning-circle/> message Retry`
/// (`text-[11px] text-danger`, retry `font-semibold underline`).
fn version_error_row(
    theme: &Theme,
    message: String,
    cx: &mut Context<CreateInstanceDialog>,
) -> Div {
    div()
        .mt(px(4.0)) // mt-1
        .flex()
        .items_center()
        .gap(px(6.0)) // gap-1.5
        .text_size(px(11.0)) // text-[11px]
        .text_color(theme.danger)
        .child(icons::icon_at(icons::WARNING_CIRCLE, 12.0))
        .child(message)
        .child(
            div()
                .id("retry-versions")
                .font_weight(FontWeight::SEMIBOLD)
                .underline()
                .on_click(cx.listener(|this, _, _, cx| this.fetch_versions(cx)))
                .child("Retry"),
        )
}

/// The loader-versions' error line (same look, retry refetches with the
/// current loader + version — the React `loaderVersions.refetch()`).
fn loader_version_error_row(
    theme: &Theme,
    message: String,
    cx: &mut Context<CreateInstanceDialog>,
) -> Div {
    div()
        .mt(px(4.0))
        .flex()
        .items_center()
        .gap(px(6.0))
        .text_size(px(11.0))
        .text_color(theme.danger)
        .child(icons::icon_at(icons::WARNING_CIRCLE, 12.0))
        .child(message)
        .child(
            div()
                .id("retry-loader-versions")
                .font_weight(FontWeight::SEMIBOLD)
                .underline()
                .on_click(cx.listener(|this, _, _, cx| {
                    if this.loader != ModLoaderKind::Vanilla && !this.minecraft_version.is_empty() {
                        this.fetch_loader_versions(cx);
                    }
                }))
                .child("Retry"),
        )
}

pub struct CreateInstanceDialog {
    name: Entity<TextField>,
    minecraft_version: String,
    loader: ModLoaderKind,
    loader_version: Option<String>,
    existing_dir: Option<String>,
    error: Option<String>,
    created: Option<MinecraftInstance>,
    creating: bool,
    // The confirm step's "Download now" flow.
    downloading: bool,
    // `useMinecraftVersions()` + `useLoaderVersions(...)`.
    versions: Option<Vec<VersionManifestEntry>>,
    versions_error: Option<String>,
    loader_versions: Option<Vec<String>>,
    loader_versions_error: Option<String>,
    loader_versions_loading: bool,
    open_menu: Option<SelectId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectId {
    Minecraft,
    Loader,
    LoaderVersion,
}

impl CreateInstanceDialog {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| TextField::new(cx, "Arcane Survival"));
        cx.observe(&name, |_, _, cx| cx.notify()).detach();
        // The confirm step tracks the backend's download tasks and the
        // install stages (its `useDownloads()` + `install://stage`
        // subscription).
        cx.observe_global::<DownloadsStore>(|_, cx| cx.notify())
            .detach();
        cx.observe_global::<InstallStageStore>(|_, cx| cx.notify())
            .detach();
        let mut this = Self {
            name,
            minecraft_version: String::new(),
            loader: ModLoaderKind::Fabric,
            loader_version: None,
            existing_dir: None,
            error: None,
            created: None,
            creating: false,
            downloading: false,
            versions: None,
            versions_error: None,
            loader_versions: None,
            loader_versions_error: None,
            loader_versions_loading: false,
            open_menu: None,
        };
        this.fetch_versions(cx);
        this
    }

    /// `useMinecraftVersions()` — fetched fresh each time the dialog
    /// opens (the React query's staleTime caches across dialog mounts;
    /// the native port pays the refetch).
    fn fetch_versions(&mut self, cx: &mut Context<Self>) {
        self.versions_error = None;
        cx.notify();
        let task = bridge::command(cx, |state| async move {
            commands::instances::list_minecraft_versions(&state).await
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|e| {
                Err(AppError::new(
                    "Couldn't load Minecraft versions.",
                    e.to_string(),
                ))
            });
            this.update(cx, |this, cx| {
                match result {
                    Ok(list) => this.versions = Some(list),
                    Err(err) => this.versions_error = Some(err.message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `useLoaderVersions(loader, minecraftVersion, loader !== "vanilla")`
    /// — `enabled` also requires a chosen Minecraft version
    /// (`enabled && Boolean(minecraftVersion)`).
    fn fetch_loader_versions(&mut self, cx: &mut Context<Self>) {
        self.loader_versions_loading = true;
        self.loader_versions = None;
        self.loader_versions_error = None;
        cx.notify();
        let loader = self.loader;
        let minecraft_version = self.minecraft_version.clone();
        let task = bridge::command(cx, move |state| async move {
            commands::instances::list_loader_versions(&state, loader, minecraft_version).await
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|e| {
                Err(AppError::new(
                    "Couldn't load loader versions.",
                    e.to_string(),
                ))
            });
            this.update(cx, |this, cx| {
                this.loader_versions_loading = false;
                match result {
                    Ok(list) => this.loader_versions = Some(list),
                    Err(err) => this.loader_versions_error = Some(err.message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `create.mutateAsync(...)` — on success the dialog switches to the
    /// DownloadConfirmStep (the React mutation's cache invalidation lands
    /// with the page's all-closed refetch when the dialog closes).
    fn create(&mut self, cx: &mut Context<Self>) {
        if self.creating {
            return;
        }
        let name = self.name.read(cx).value().trim().to_string();
        let minecraft_version = self.minecraft_version.clone();
        if name.is_empty() || minecraft_version.is_empty() {
            return;
        }
        self.creating = true;
        self.error = None;
        cx.notify();
        let request = CreateInstanceRequest {
            name,
            minecraft_version,
            loader: self.loader,
            loader_version: self.loader_version.clone(),
            existing_game_dir: self.existing_dir.clone(),
            java_path: None,
        };
        let task = bridge::command(cx, move |state| async move {
            commands::instances::create_instance(&state, request).await
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .unwrap_or_else(|e| Err(AppError::new("Something went wrong.", e.to_string())));
            this.update(cx, |this, cx| {
                this.creating = false;
                match result {
                    Ok(instance) => this.created = Some(instance),
                    Err(err) => this.error = Some(err.message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The confirm step's "Download now" — `ensureInstalled`, with the
    /// progress read off the download tasks while it runs.
    fn download_now(&mut self, cx: &mut Context<Self>) {
        if self.downloading {
            return;
        }
        let Some(created) = self.created.clone() else {
            return;
        };
        self.downloading = true;
        self.error = None;
        cx.notify();
        let id = created.id.clone();
        let task = bridge::command(cx, move |state| async move {
            commands::launch::ensure_instance_installed(&state, id).await
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|e| {
                Err(AppError::new(
                    "Couldn't download this instance.",
                    e.to_string(),
                ))
            });
            this.update(cx, |this, cx| {
                this.downloading = false;
                match result {
                    Ok(()) => {
                        if let Some(created) = &this.created {
                            toasts::toast_success(
                                cx,
                                format!("{} is ready to play.", created.name),
                            );
                        }
                        modals::close_create(cx);
                    }
                    Err(err) => this.error = Some(err.message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The folder row's "Import..." — `openDialog({ directory: true })`.
    fn pick_folder(&mut self, cx: &mut Context<Self>) {
        let task = cx.background_executor().spawn(async move {
            rfd::AsyncFileDialog::new()
                .pick_folder()
                .await
                .map(|handle| handle.path().to_string_lossy().into_owned())
        });
        cx.spawn(async move |this, cx| {
            let Some(dir) = task.await else {
                return; // user cancelled
            };
            this.update(cx, |this, cx| {
                this.existing_dir = Some(dir);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *Theme::get(cx);
        let mut panel = div()
            .w(px(448.0)) // max-w-md
            .flex()
            .flex_col()
            .p(px(24.0)) // p-6
            .rounded(px(16.0)) // rounded-2xl (glass-panel)
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .shadow(vec![drop_shadow(0.25, 25.0, 50.0, -12.0)]);

        if let Some(created) = self.created.clone() {
            // The dialog's second act — the created instance's download
            // prompt (the `created` early return in the React dialog).
            panel = panel
                .child(
                    div()
                        .text_size(px(16.0)) // text-base
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Instance created"),
                )
                .child(self.confirm_step(&created, cx));
        } else {
            panel = panel
                .child(
                    div()
                        .text_size(px(16.0)) // text-base
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("New instance"),
                )
                .child(self.form(window, cx));
        }

        div()
            .id("create-instance-backdrop")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme::rgba_const(0x00000099)) // bg-black/60
            .p(px(16.0))
            .occlude()
            .child(panel)
    }

    /// The form — name, Minecraft version, loader, (loader version),
    /// folder, then the shared error line and the footer buttons.
    fn form(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        let theme = *Theme::get(cx);

        // `<label className="text-xs font-medium text-muted">`.
        let label = |key: &'static str, text: &'static str| {
            div()
                .id(key)
                .text_size(px(12.0)) // text-xs
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.muted)
                .child(text)
        };

        let mut form = div()
            .id("create-form")
            .mt(px(16.0)) // mt-4
            .flex()
            .flex_col()
            .gap(px(12.0)); // gap-3

        form = form.child(
            div()
                .flex()
                .flex_col()
                .child(label("name-label", "Name"))
                .child(
                    div()
                        .mt(px(4.0))
                        .w_full()
                        .child(self.name.update(cx, |field, cx| {
                            field.render_styled(
                                window,
                                cx,
                                &FieldStyle {
                                    id: "create-name-input",
                                    key_context: "CreateInput",
                                    height: 36.0,
                                    rounded: 8.0,
                                    text_size: 14.0,
                                    padding_x: 12.0,
                                    transparent: false,
                                    mono: false,
                                },
                            )
                        })),
                ),
        );

        // --- Minecraft version select -----------------------------------
        let ready = self.versions.is_some();
        let loading = self.versions.is_none() && self.versions_error.is_none();
        let version_label = if !self.minecraft_version.is_empty() {
            self.minecraft_version.clone()
        } else if loading {
            "Loading versions...".to_string()
        } else {
            "Select a version".to_string()
        };
        let mut version_block = div()
            .flex()
            .flex_col()
            .child(label("version-label", "Minecraft version"))
            .child(self.select_field(
                SelectId::Minecraft,
                "mc-version",
                version_label,
                ready,
                !ready && loading,
                cx,
            ));
        if let Some(message) = self.versions_error.clone() {
            version_block = version_block.child(version_error_row(&theme, message, cx));
        }
        form = form.child(version_block);

        // --- loader select (never disabled in the React form) ------------
        form = form.child(
            div()
                .flex()
                .flex_col()
                .child(label("loader-label", "Loader"))
                .child(self.select_field(
                    SelectId::Loader,
                    "loader",
                    loader_option_label(self.loader).to_string(),
                    true,
                    false,
                    cx,
                )),
        );

        // --- loader version select (hidden for vanilla) ------------------
        if self.loader != ModLoaderKind::Vanilla {
            let ready = !self.loader_versions_loading && self.loader_versions.is_some();
            let label_text = if self.loader_versions_loading {
                "Loading...".to_string()
            } else {
                self.loader_version
                    .clone()
                    .unwrap_or_else(|| "Recommended".to_string())
            };
            let mut block = div()
                .flex()
                .flex_col()
                .child(label("loader-version-label", "Loader version"))
                .child(self.select_field(
                    SelectId::LoaderVersion,
                    "loader-version",
                    label_text,
                    ready,
                    !ready && self.loader_versions_loading,
                    cx,
                ));
            if let Some(message) = self.loader_versions_error.clone() {
                block = block.child(loader_version_error_row(&theme, message, cx));
            }
            form = form.child(block);
        }

        // --- folder row --------------------------------------------------
        form = form.child(
            div()
                .flex()
                .flex_col()
                .child(label("folder-label", "Folder"))
                .child(
                    div().mt(px(4.0)).flex().gap(px(8.0)).child(
                        // The read-only input: `value={existingDir ?? "New
                        // folder (managed by Arcane Launcher)"}`.
                        div()
                            .flex_1()
                            .h(px(36.0))
                            .flex()
                            .items_center()
                            .rounded(px(8.0))
                            .px(px(12.0))
                            .bg(theme::white(0.03))
                            .border_1()
                            .border_color(theme.border)
                            .text_size(px(12.0)) // text-xs
                            .text_color(theme.muted)
                            .child(div().truncate().child(
                                self.existing_dir.clone().unwrap_or_else(|| {
                                    "New folder (managed by Arcane Launcher)".to_string()
                                }),
                            )),
                    ),
                )
                .child(div().mt(px(4.0)).flex().gap(px(8.0)).child({
                    let mut row = div().flex().gap(px(8.0)).child(
                        ui::button(
                            "pick-folder",
                            theme,
                            ui::ButtonVariant::Ghost,
                            ui::ButtonSize::Sm,
                            false,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.pick_folder(cx)))
                        .child("Import..."),
                    );
                    if self.existing_dir.is_some() {
                        row = row.child(
                            ui::button(
                                "clear-folder",
                                theme,
                                ui::ButtonVariant::Ghost,
                                ui::ButtonSize::Sm,
                                false,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.existing_dir = None;
                                cx.notify();
                            }))
                            .child("Clear"),
                        );
                    }
                    row
                })),
        );

        // The shared error line + footer buttons (`mt-6 flex justify-end
        // gap-2`).
        if let Some(message) = self.error.clone() {
            form = form.child(
                div()
                    .mt(px(12.0)) // mt-3
                    .text_size(px(12.0)) // text-xs
                    .text_color(theme.danger)
                    .child(message),
            );
        }

        let name_empty = self.name.read(cx).value().trim().is_empty();
        let version_empty = self.minecraft_version.is_empty();
        form = form.child(
            div()
                .mt(px(24.0)) // mt-6
                .flex()
                .justify_end()
                .gap(px(8.0)) // gap-2
                .child(
                    ui::button(
                        "create-cancel",
                        theme,
                        ui::ButtonVariant::Ghost,
                        ui::ButtonSize::Sm,
                        false,
                    )
                    .on_click(cx.listener(|_, _, _, cx| modals::close_create(cx)))
                    .child("Cancel"),
                )
                .child(
                    ui::button(
                        "create-confirm",
                        theme,
                        ui::ButtonVariant::Primary,
                        ui::ButtonSize::Sm,
                        name_empty || version_empty || self.creating,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.create(cx)))
                    .child(if self.creating {
                        "Creating..."
                    } else {
                        "Create"
                    }),
                ),
        );

        form
    }

    /// One `<select>` — the trigger (`glass-input rounded-lg px-3 py-2
    /// text-sm` with the caret) plus its open menu. The React select
    /// opens its list downward; the menu opens upward for the paint-order
    /// reason in the module docs.
    fn select_field(
        &mut self,
        id: SelectId,
        key: &'static str,
        label_text: String,
        enabled: bool,
        dimmed: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = *Theme::get(cx);
        let mut trigger = div()
            .id(SharedString::from(format!("{key}-select")))
            .h(px(36.0))
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .rounded(px(8.0)) // rounded-lg
            .px(px(12.0)) // px-3
            .bg(theme::white(0.03)) // glass-input
            .border_1()
            .border_color(theme.border)
            .text_size(px(14.0)) // text-sm
            .text_color(if dimmed {
                theme.muted
            } else {
                theme.foreground
            });
        if enabled {
            trigger = trigger.on_click(cx.listener(move |this, _, _, cx| {
                this.open_menu = if this.open_menu == Some(id) {
                    None
                } else {
                    Some(id)
                };
                cx.notify();
            }));
        } else if dimmed {
            // `disabled:opacity-60` on the React select.
            trigger = trigger.opacity(0.6);
        }
        let mut wrapper = div().relative().w_full().child(
            trigger
                .child(label_text)
                .child(icons::icon_at(icons::CARET_DOWN, 9.0).text_color(theme.muted)),
        );
        if enabled && self.open_menu == Some(id) {
            wrapper = match id {
                SelectId::Minecraft => wrapper.child(self.version_menu(cx)),
                SelectId::Loader => wrapper.child(self.loader_menu(cx)),
                SelectId::LoaderVersion => wrapper.child(self.loader_version_menu(cx)),
            };
        }
        wrapper.into_any_element()
    }

    /// The Minecraft version menu — release versions only
    /// (`versions.data?.filter((v) => v.type === "release")`).
    fn version_menu(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = *Theme::get(cx);
        let mut panel = menu_panel("mc-version", &theme);
        if let Some(versions) = self.versions.clone() {
            for id in versions
                .iter()
                .filter(|v| v.kind == "release")
                .map(|v| v.id.clone())
            {
                let selected = id == self.minecraft_version;
                let key = SharedString::from(format!("mc-version-{id}"));
                panel = panel.child(menu_item(
                    key,
                    id.clone(),
                    selected,
                    theme,
                    cx,
                    move |this, _, cx| {
                        this.minecraft_version = id.clone();
                        this.open_menu = None;
                        if this.loader != ModLoaderKind::Vanilla {
                            this.fetch_loader_versions(cx);
                        }
                        cx.notify();
                    },
                ));
            }
        }
        panel.into_any_element()
    }

    fn loader_menu(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = *Theme::get(cx);
        let mut panel = menu_panel("loader", &theme);
        for kind in [
            ModLoaderKind::Vanilla,
            ModLoaderKind::Fabric,
            ModLoaderKind::Quilt,
            ModLoaderKind::Forge,
            ModLoaderKind::NeoForge,
        ] {
            let selected = kind == self.loader;
            let key = SharedString::from(format!("loader-{kind:?}"));
            panel = panel.child(menu_item(
                key,
                loader_option_label(kind).to_string(),
                selected,
                theme,
                cx,
                move |this, _, cx| {
                    this.loader = kind;
                    this.open_menu = None;
                    if kind != ModLoaderKind::Vanilla && !this.minecraft_version.is_empty() {
                        this.fetch_loader_versions(cx);
                    }
                    cx.notify();
                },
            ));
        }
        panel.into_any_element()
    }

    fn loader_version_menu(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = *Theme::get(cx);
        let mut panel = menu_panel("loader-version", &theme);
        // The `<option value="">Recommended</option>` entry.
        panel = panel.child(menu_item(
            "loader-version-recommended",
            "Recommended".to_string(),
            self.loader_version.is_none(),
            theme,
            cx,
            |this, _, cx| {
                this.loader_version = None;
                this.open_menu = None;
                cx.notify();
            },
        ));
        if let Some(versions) = self.loader_versions.clone() {
            for version in versions {
                let selected = self.loader_version.as_deref() == Some(version.as_str());
                let key = SharedString::from(format!("loader-version-{version}"));
                panel = panel.child(menu_item(
                    key,
                    version.clone(),
                    selected,
                    theme,
                    cx,
                    move |this, _, cx| {
                        this.loader_version = Some(version.clone());
                        this.open_menu = None;
                        cx.notify();
                    },
                ));
            }
        }
        panel.into_any_element()
    }

    /// `DownloadConfirmStep` — the created instance's download prompt,
    /// tracking `download://progress` tasks and `install://stage` while
    /// the install runs.
    fn confirm_step(
        &mut self,
        instance: &MinecraftInstance,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let theme = *Theme::get(cx);

        let tasks: Vec<DownloadTask> = cx
            .global::<DownloadsStore>()
            .tasks()
            .into_iter()
            .filter(|t| t.instance_id.as_deref() == Some(instance.id.as_str()))
            .collect();
        let active_count = tasks
            .iter()
            .filter(|t| {
                matches!(
                    t.status,
                    DownloadStatus::Downloading | DownloadStatus::Queued | DownloadStatus::Paused
                )
            })
            .count();
        let total_downloaded: u64 = tasks.iter().map(|t| t.downloaded_bytes).sum();
        let total_size: u64 = tasks.iter().map(|t| t.total_bytes).sum();
        let overall_pct = if total_size > 0 {
            ((total_downloaded as f64 / total_size as f64) * 100.0)
                .round()
                .clamp(0.0, 100.0) as u64
        } else {
            0
        };
        let speed: u64 = tasks
            .iter()
            .filter(|t| {
                matches!(
                    t.status,
                    DownloadStatus::Downloading | DownloadStatus::Queued | DownloadStatus::Paused
                )
            })
            .map(|t| t.speed_bps)
            .sum();
        let eta = if active_count > 0 {
            format::format_eta(total_size.saturating_sub(total_downloaded), speed)
        } else {
            None
        };
        let stage = cx
            .global::<InstallStageStore>()
            .stage_for(&instance.id)
            .cloned();
        let headline = if active_count > 0 {
            "Downloading game files".to_string()
        } else if !tasks.is_empty() {
            "Finishing up...".to_string()
        } else {
            stage.unwrap_or_else(|| "Preparing...".to_string())
        };

        // The intro row — the download icon box + the ask.
        let mut step = div()
            .id("download-confirm")
            .mt(px(4.0)) // mt-1
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(12.0)) // gap-3
                    .child(
                        div()
                            .mt(px(2.0)) // mt-0.5
                            .h(px(36.0)) // h-9
                            .w(px(36.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(12.0)) // rounded-xl
                            .border_1()
                            .border_color(theme::white(0.08))
                            .bg(theme::white(0.05))
                            .child(
                                icons::icon_at(icons::DOWNLOAD_SIMPLE, 16.0)
                                    .text_color(theme.accent),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(14.0)) // text-sm
                            .text_color(theme.muted)
                            .child(format!(
                                "\"{}\" needs Minecraft {}{} downloaded before it can launch. \
                                 Download it now?",
                                instance.name,
                                instance.minecraft_version,
                                if instance.loader != ModLoaderKind::Vanilla {
                                    format!(" and {}", loader_label(instance.loader))
                                } else {
                                    String::new()
                                },
                            )),
                    ),
            );

        if self.downloading {
            // The progress box (`rounded-xl border-white/8 bg-white/3
            // px-6 py-7`, icon + headline + bar + per-task rows).
            let mut progress = div()
                .id("download-progress")
                .mt(px(16.0)) // mt-4
                .flex()
                .flex_col()
                .items_center()
                .gap(px(12.0)) // gap-3
                .rounded(px(12.0)) // rounded-xl
                .border_1()
                .border_color(theme::white(0.08))
                .bg(theme::white(0.03))
                .px(px(24.0)) // px-6
                .py(px(28.0)); // py-7

            progress = progress.child(
                div()
                    .h(px(48.0)) // h-12
                    .w(px(48.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(12.0))
                    .bg(theme::white(0.06))
                    .child(icons::icon_at(icons::CUBE, 24.0).text_color(theme.muted)),
            );
            progress = progress.child(
                div()
                    .text_size(px(14.0))
                    .font_weight(FontWeight::MEDIUM)
                    .child(headline),
            );

            // The bar — `h-2 w-full max-w-xs overflow-hidden rounded-full
            // bg-white/10`, the fill `from-accent to-accent-strong`. With
            // no known total it sits at w-1/3 (the React pulse renders
            // statically, like every other pulse).
            let mut fill = div().h_full().rounded_full().bg(gpui::linear_gradient(
                90.0,
                gpui::linear_color_stop(theme.accent, 0.0),
                gpui::linear_color_stop(theme.accent_strong, 1.0),
            ));
            fill = if total_size > 0 {
                fill.w(relative(overall_pct as f32 / 100.0))
            } else {
                fill.w(relative(1.0 / 3.0))
            };
            progress = progress.child(
                div()
                    .h(px(8.0)) // h-2
                    .w_full()
                    .max_w(px(320.0)) // max-w-xs
                    .overflow_hidden()
                    .rounded_full()
                    .bg(theme::white(0.10))
                    .child(fill),
            );

            if total_size > 0 {
                let mut line = format!(
                    "{}% · {} / {}",
                    overall_pct,
                    format::format_bytes(total_downloaded),
                    format::format_bytes(total_size)
                );
                if speed > 0 {
                    line.push_str(&format!(" · {}", format::format_speed(speed)));
                }
                if let Some(eta) = &eta {
                    line.push_str(&format!(" · {eta} left"));
                }
                progress = progress.child(
                    div()
                        .text_size(px(11.0)) // text-[11px]
                        .text_color(theme.muted)
                        .child(line),
                );
            }

            if !tasks.is_empty() {
                let mut list = div()
                    .w_full()
                    .max_w(px(320.0))
                    .flex()
                    .flex_col()
                    .gap(px(6.0)); // gap-1.5
                for (index, task) in tasks.iter().enumerate() {
                    let done = task.status == DownloadStatus::Completed;
                    let pct = if task.total_bytes > 0 {
                        ((task.downloaded_bytes as f64 / task.total_bytes as f64) * 100.0)
                            .round()
                            .clamp(0.0, 100.0) as u64
                    } else {
                        0
                    };
                    let mut row = div()
                        .id(SharedString::from(format!("download-task-{index}")))
                        .flex()
                        .items_center()
                        .gap(px(8.0)) // gap-2
                        .text_size(px(11.0)) // text-[11px]
                        .text_color(theme.muted)
                        .child(div().flex_1().truncate().child(task.name.clone()));
                    if task.status == DownloadStatus::Failed {
                        row = row.child(div().text_color(theme.danger).child("Failed"));
                    } else if done {
                        row = row.child(
                            icons::icon_at(icons::CHECK_CIRCLE, 12.0).text_color(theme.success),
                        );
                    } else {
                        row = row.child(div().child(format!("{pct}%")));
                    }
                    list = list.child(row);
                }
                progress = progress.child(list);
            }

            step = step.child(progress);
        }

        if let Some(message) = self.error.clone() {
            step = step.child(
                div()
                    .mt(px(12.0)) // mt-3
                    .text_size(px(12.0)) // text-xs
                    .text_color(theme.danger)
                    .child(message),
            );
        }

        step = step.child(
            div()
                .mt(px(24.0)) // mt-6
                .flex()
                .justify_end()
                .gap(px(8.0)) // gap-2
                .child(
                    ui::button(
                        "download-skip",
                        theme,
                        ui::ButtonVariant::Ghost,
                        ui::ButtonSize::Sm,
                        self.downloading,
                    )
                    .on_click(cx.listener(|_, _, _, cx| modals::close_create(cx)))
                    .child("Skip for now"),
                )
                .child(
                    ui::button(
                        "download-now",
                        theme,
                        ui::ButtonVariant::Primary,
                        ui::ButtonSize::Sm,
                        self.downloading,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.download_now(cx)))
                    .child(if self.downloading {
                        "Downloading..."
                    } else {
                        "Download now"
                    }),
                ),
        );

        step
    }
}

impl gpui::Render for CreateInstanceDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_dialog(window, cx)
    }
}

// ---------------------------------------------------------------------------
// ImportInstancesDialog
// ---------------------------------------------------------------------------

/// The source chips — `sources` in `import-instances-dialog.tsx`.
const SOURCES: [(ImportSource, &str); 5] = [
    (ImportSource::PrismLauncher, "PrismLauncher / MultiMC"),
    (ImportSource::CurseForge, "CurseForge App"),
    (ImportSource::TLauncher, "Minecraft Launcher / TLauncher"),
    (ImportSource::SKLauncher, "SKlauncher"),
    (ImportSource::ModrinthApp, "Modrinth App"),
];

/// How to import from each launcher — the `guides` table. Folder names
/// match exactly what the backend's import scanners look for.
fn guide(source: ImportSource) -> (&'static str, &'static str, &'static [&'static str]) {
    match source {
        ImportSource::PrismLauncher => (
            "%APPDATA%\\PrismLauncher (MultiMC: the folder MultiMC.exe is in)",
            "the folder that contains the instances folder",
            &[
                "Close PrismLauncher / MultiMC so no instance is running.",
                "Click Scan default locations. PrismLauncher installed normally is found automatically.",
                "Portable install, MultiMC, or a custom data folder? Click Browse for folder and pick the folder that contains the instances folder.",
            ],
        ),
        ImportSource::CurseForge => (
            "Documents\\curseforge\\minecraft or %USERPROFILE%\\curseforge\\minecraft",
            "the minecraft folder, or the Instances folder inside it",
            &[
                "Close the CurseForge app so no modpack is running.",
                "Click Scan default locations.",
                "Moved your modding folder? In CurseForge open Settings > Minecraft to see where it is, then Browse for folder and pick that minecraft folder (or its Instances folder).",
            ],
        ),
        ImportSource::TLauncher => (
            "%APPDATA%\\.minecraft",
            "your .minecraft folder",
            &[
                "Close the launcher (and the game).",
                "Click Scan default locations. Every profile and every installed version shows up as an instance.",
                "Changed the game folder in the launcher's settings? Browse for folder and pick that .minecraft folder.",
                "Versions you never installed or played don't have files yet, so they won't appear. Launch them once in the other launcher first.",
            ],
        ),
        ImportSource::SKLauncher => (
            "%APPDATA%\\.minecraft",
            "your .minecraft folder",
            &[
                "Close SKlauncher (and the game).",
                "Click Scan default locations. Your SKlauncher profiles and installed versions show up as instances.",
                "Using a different game folder? Browse for folder and pick that .minecraft folder.",
            ],
        ),
        ImportSource::ModrinthApp => (
            "%APPDATA%\\ModrinthApp",
            "the folder that contains app.db and the profiles folder",
            &[
                "Close the Modrinth App completely (also from the system tray).",
                "Click Scan default locations.",
                "Changed the app directory in Modrinth App's settings? Browse for folder and pick the folder that contains app.db and profiles.",
            ],
        ),
    }
}

pub struct ImportInstancesDialog {
    source: ImportSource,
    root_path: Option<String>,
    discovered: Option<Vec<DiscoveredInstance>>,
    selected: HashSet<String>,
    error: Option<String>,
    scanning: bool,
    importing: bool,
}

impl ImportInstancesDialog {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            source: ImportSource::PrismLauncher,
            root_path: None,
            discovered: None,
            selected: HashSet::new(),
            error: None,
            scanning: false,
            importing: false,
        }
    }

    /// `runScan(path)` — the backend scan (synchronous service call on
    /// the runtime, like every command); on an empty result the React
    /// dialog shows one of two hints depending on whether a folder was
    /// given.
    fn run_scan(&mut self, path: Option<String>, cx: &mut Context<Self>) {
        self.error = None;
        self.discovered = None;
        self.selected.clear();
        self.scanning = true;
        cx.notify();
        let source = self.source;
        let had_path = path.is_some();
        let task = bridge::command(cx, move |_state| async move {
            commands::import::scan_for_instances(source, path).await
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .unwrap_or_else(|e| Err(AppError::new("Scan failed.", e.to_string())));
            this.update(cx, |this, cx| {
                this.scanning = false;
                match result {
                    Ok(results) => {
                        if results.is_empty() {
                            this.error = Some(if had_path {
                                "No instances found in that folder.".to_string()
                            } else {
                                "Nothing found in the usual default locations — point me at the launcher's data folder instead.".to_string()
                            });
                        } else {
                            this.selected = results
                                .iter()
                                .map(|r| r.root_path.clone())
                                .collect();
                        }
                        this.discovered = Some(results);
                    }
                    Err(err) => this.error = Some(err.message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// "Browse for folder..." — `openDialog({ directory: true })` then a
    /// scan rooted at the pick.
    fn browse(&mut self, cx: &mut Context<Self>) {
        if self.scanning {
            return;
        }
        let task = cx.background_executor().spawn(async move {
            rfd::AsyncFileDialog::new()
                .pick_folder()
                .await
                .map(|handle| handle.path().to_string_lossy().into_owned())
        });
        cx.spawn(async move |this, cx| {
            let Some(dir) = task.await else {
                return; // user cancelled
            };
            this.update(cx, |this, cx| {
                this.root_path = Some(dir.clone());
                this.run_scan(Some(dir), cx);
            })
            .ok();
        })
        .detach();
    }

    /// "Import N selected" — one backend task imports them in order and
    /// reports successes/failures for the toasts (`importInstance`
    /// mutation loop in the React dialog).
    fn import_selected(&mut self, cx: &mut Context<Self>) {
        if self.importing {
            return;
        }
        let Some(discovered) = self.discovered.clone() else {
            return;
        };
        let to_import: Vec<DiscoveredInstance> = discovered
            .into_iter()
            .filter(|d| self.selected.contains(&d.root_path))
            .collect();
        if to_import.is_empty() {
            return;
        }
        self.importing = true;
        cx.notify();
        let task = bridge::command(cx, move |state| async move {
            let mut succeeded = 0usize;
            let mut failures: Vec<String> = Vec::new();
            for d in to_import {
                let name = d.name.clone();
                match commands::import::import_discovered_instance(&state, d).await {
                    Ok(_) => succeeded += 1,
                    Err(_) => failures.push(name),
                }
            }
            (succeeded, failures)
        });
        cx.spawn(async move |this, cx| {
            let (succeeded, failures) = task.await.unwrap_or((0, Vec::new()));
            this.update(cx, |this, cx| {
                this.importing = false;
                if succeeded > 0 {
                    toasts::toast_success(
                        cx,
                        format!(
                            "Imported {succeeded} instance{}.",
                            if succeeded == 1 { "" } else { "s" }
                        ),
                    );
                }
                if !failures.is_empty() {
                    toasts::toast_error(cx, format!("Failed to import: {}", failures.join(", ")));
                }
                modals::close_import(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_dialog(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *Theme::get(cx);

        let mut panel = div()
            .id("import-dialog-panel")
            .w(px(576.0)) // max-w-xl
            .max_h(relative(0.9)) // max-h-[90vh]
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .p(px(24.0)) // p-6
            .rounded(px(16.0)) // rounded-2xl (glass-panel)
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .shadow(vec![drop_shadow(0.25, 25.0, 50.0, -12.0)])
            .child(
                div()
                    .text_size(px(16.0)) // text-base
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Import instances"),
            )
            .child(
                div()
                    .mt(px(4.0)) // mt-1
                    .text_size(px(12.0)) // text-xs
                    .text_color(theme.muted)
                    .child(
                        "Bring in instances you already have in another launcher — their mods, \
                         worlds, and configs come along as-is.",
                    ),
            );

        // --- source chips ------------------------------------------------
        let mut chips = div()
            .id("import-sources")
            .mt(px(16.0)) // mt-4
            .flex()
            .flex_wrap()
            .gap(px(8.0)); // gap-2
        for (source, label) in SOURCES {
            let active = source == self.source;
            let mut chip = div()
                .id(SharedString::from(format!("import-source-{:?}", source)))
                .flex_none()
                .rounded(px(8.0)) // rounded-lg
                .border_1()
                .px(px(12.0)) // px-3
                .py(px(6.0)) // py-1.5
                .text_size(px(12.0)) // text-xs
                .font_weight(FontWeight::MEDIUM)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.source = source;
                    this.discovered = None;
                    this.root_path = None;
                    this.selected.clear();
                    cx.notify();
                }));
            chip = if active {
                chip.border_color(theme::accent_alpha(0.40))
                    .bg(theme::accent_alpha(0.15))
                    .text_color(theme.accent)
            } else {
                chip.border_color(theme.border)
                    .text_color(theme.muted)
                    .hover(move |style| style.bg(theme::white(0.05)))
            };
            chips = chips.child(chip.child(label));
        }
        panel = panel.child(chips);

        // --- the guide panel ---------------------------------------------
        let (location, pick, steps) = guide(self.source);
        let source_label = SOURCES
            .iter()
            .find(|(s, _)| *s == self.source)
            .map(|(_, l)| *l)
            .unwrap_or_default();
        let mut guide_panel = div()
            .id("import-guide")
            .mt(px(16.0)) // mt-4
            .rounded(px(12.0)) // rounded-xl
            .border_1()
            .border_color(theme::white(0.06))
            .bg(theme::white(0.02))
            .p(px(16.0)); // p-4
        guide_panel = guide_panel
            .child(
                div()
                    .text_size(px(12.0)) // text-xs
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child(format!("How to import from {source_label}")),
            )
            // The `<ol className="...list-decimal pl-4">` — manual numbers,
            // the same content.
            .child(
                div()
                    .mt(px(8.0)) // mt-2
                    .flex()
                    .flex_col()
                    .gap(px(6.0)) // gap-1.5
                    .pl(px(16.0)) // pl-4
                    .text_size(px(12.0)) // text-xs
                    .text_color(theme.muted)
                    .children(
                        steps
                            .iter()
                            .enumerate()
                            .map(|(index, step)| div().child(format!("{}. {}", index + 1, step))),
                    )
                    .child(div().child(format!(
                        "{}. Tick the instances you want and click Import.",
                        steps.len() + 1
                    ))),
            )
            .child(
                div()
                    .mt(px(12.0)) // mt-3
                    .flex()
                    .flex_col()
                    .gap(px(4.0)) // gap-1
                    .text_size(px(11.0)) // text-[11px]
                    .text_color(theme.muted)
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.foreground)
                                    .child("Usual location:"),
                            )
                            .child(
                                div()
                                    .rounded(px(4.0))
                                    .bg(theme::white(0.06))
                                    .px(px(6.0)) // px-1.5
                                    .py(px(2.0)) // py-0.5
                                    .child(location),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.foreground)
                                    .child("When browsing, pick:"),
                            )
                            .child(pick),
                    ),
            )
            .child(
                div()
                    .mt(px(12.0)) // mt-3
                    .border_t_1()
                    .border_color(theme::white(0.06))
                    .pt(px(12.0)) // pt-3
                    .text_size(px(11.0)) // text-[11px]
                    .text_color(theme.muted)
                    .child(
                        "Importing makes a copy, so the other launcher keeps working as before. \
                         From a shared .minecraft only your game content is copied (worlds, mods, \
                         settings, servers), never the other launcher's files or logins. Mods, \
                         worlds, configs, resource packs and shaders all come along. Imported \
                         mods show as not linked in Installed Mods, so they can't be checked for \
                         updates until you reinstall them from the browser.",
                    ),
            );
        panel = panel.child(guide_panel);

        // --- scan row + root path line -----------------------------------
        let scan_label = if self.scanning && self.root_path.is_none() {
            "Scanning..."
        } else {
            "Scan default locations"
        };
        panel = panel
            .child(
                div()
                    .mt(px(12.0)) // mt-3
                    .flex()
                    .gap(px(8.0)) // gap-2
                    .child(
                        ui::button(
                            "import-scan",
                            theme,
                            ui::ButtonVariant::Ghost,
                            ui::ButtonSize::Sm,
                            self.scanning,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.run_scan(None, cx)))
                        .child(scan_label),
                    )
                    .child(
                        ui::button(
                            "import-browse",
                            theme,
                            ui::ButtonVariant::Ghost,
                            ui::ButtonSize::Sm,
                            self.scanning,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.browse(cx)))
                        .child("Browse for folder..."),
                    ),
            )
            .children(self.root_path.clone().map(|path| {
                div()
                    .mt(px(4.0)) // mt-1
                    .text_size(px(11.0)) // text-[11px]
                    .text_color(theme.muted)
                    .child(div().truncate().child(format!("Scanning: {path}")))
            }));

        if let Some(message) = self.error.clone() {
            panel = panel.child(
                div()
                    .mt(px(12.0)) // mt-3
                    .text_size(px(12.0)) // text-xs
                    .text_color(theme.danger)
                    .child(message),
            );
        }

        // --- discovered list ----------------------------------------------
        if let Some(discovered) = self.discovered.clone().filter(|d| !d.is_empty()) {
            let mut list = div()
                .id("import-discovered")
                .mt(px(12.0)) // mt-3
                .max_h(px(256.0)) // max-h-64
                .flex()
                .flex_col()
                .overflow_y_scroll()
                .rounded(px(8.0)) // rounded-lg
                .border_1()
                .border_color(theme::white(0.06));
            let count = discovered.len();
            for (index, d) in discovered.iter().enumerate() {
                let was_selected = self.selected.contains(&d.root_path);
                let root = d.root_path.clone();
                let sub = if d.loader != ModLoaderKind::Vanilla {
                    format!(
                        "Minecraft {} · {}{}",
                        d.minecraft_version,
                        d.loader.id(),
                        d.loader_version
                            .as_deref()
                            .map(|v| format!(" {v}"))
                            .unwrap_or_default()
                    )
                } else {
                    format!("Minecraft {}", d.minecraft_version)
                };
                let mut row = div()
                    .id(SharedString::from(format!("import-row-{index}")))
                    .flex()
                    .items_center()
                    .gap(px(10.0)) // gap-2.5
                    .px(px(12.0)) // px-3
                    .py(px(8.0)) // py-2
                    .text_size(px(14.0)) // text-sm
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if was_selected {
                            this.selected.remove(&root);
                        } else {
                            this.selected.insert(root.clone());
                        }
                        cx.notify();
                    }));
                if index + 1 < count {
                    row = row.border_b_1().border_color(theme::white(0.04));
                }
                list = list.child(
                    row.child(ui::dark_checkbox(
                        SharedString::from(format!("import-check-{index}")),
                        theme,
                        was_selected,
                        false,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .child(
                                div()
                                    .truncate()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(d.name.clone()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(11.0)) // text-[11px]
                                    .text_color(theme.muted)
                                    .child(sub),
                            ),
                    ),
                );
            }
            panel = panel.child(list);
        }

        // --- footer -------------------------------------------------------
        let selected_count = self.selected.len();
        let mut footer = div()
            .mt(px(24.0)) // mt-6
            .flex()
            .justify_end()
            .gap(px(8.0)) // gap-2
            .child(
                ui::button(
                    "import-close",
                    theme,
                    ui::ButtonVariant::Ghost,
                    ui::ButtonSize::Sm,
                    false,
                )
                .on_click(cx.listener(|_, _, _, cx| modals::close_import(cx)))
                .child("Close"),
            );
        if self
            .discovered
            .as_ref()
            .is_some_and(|discovered| !discovered.is_empty())
        {
            footer = footer.child(
                ui::button(
                    "import-confirm",
                    theme,
                    ui::ButtonVariant::Primary,
                    ui::ButtonSize::Sm,
                    selected_count == 0 || self.importing,
                )
                .on_click(cx.listener(|this, _, _, cx| this.import_selected(cx)))
                .child(if self.importing {
                    "Importing...".to_string()
                } else {
                    format!("Import {selected_count} selected")
                }),
            );
        }
        panel = panel.child(footer);

        div()
            .id("import-instances-backdrop")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme::rgba_const(0x00000099)) // bg-black/60
            .p(px(16.0))
            .occlude()
            .child(panel)
    }
}

impl gpui::Render for ImportInstancesDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_dialog(cx)
    }
}
