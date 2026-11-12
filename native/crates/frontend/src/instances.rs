//! The Instances page — direct port of `src/app/instances/page.tsx`
//! (`InstancesPage`, `InstanceCard`, `InstanceListRow`,
//! `InstanceMoreMenu`, `InterruptedMigrationBanner`,
//! `DeleteInstanceDialog`, `DuplicateInstanceDialog`), plus the second
//! routed screen behind the sidebar's Instances link.
//!
//! Deferred pieces are documented inline:
//! - "Manage mods" (routes to the Mods page — not ported yet), so the
//!   selected card's action stays "Select" like its unselected state,
//! - the card thumbnails and instance icons (`convertFileSrc`/data-URL
//!   `<img>`s) — the gradient header and placeholder icon are exactly
//!   the React app's null-icon fallback,
//! - the pack-update banner (`useModpackUpdateCheck`) — it needs the
//!   network-status plumbing that lands with the settings slice,
//! - the ⋮ menu's Performance boost / Change version / Version history /
//!   Export items (their dialogs ride on screens that aren't ported
//!   yet), so it currently holds Console / Duplicate / Remove only,
//! - the hover lift (`hover:-translate-y-0.5`, the hover shadow) and the
//!   hover accent hairline above the cards — gpui 0.2.2 has no
//!   transforms and no hover-conditioned children,
//! - the pulse animations (running dots, skeletons) render statically.
//!
//! The create / import dialogs (the header's and empty state's buttons)
//! live in `instances_dialogs.rs`.
//!
//! Both custom dropdowns open **upward** where React opens downward:
//! gpui paints in document order, so a downward menu would be covered by
//! the siblings painted after its own card/toolbar; opening upward keeps
//! it above everything painted earlier. Panels and items are
//! pixel-matched to `ui/dropdown-menu.tsx`.

use std::cmp::{Ordering, Reverse};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use backend::commands;
use backend::error::AppError;
use backend::models::common::ModLoaderKind;
use backend::models::instance::{InstanceValidationReport, MinecraftInstance};
use backend::services::instance::evolution::InProgressMarker;
use gpui::{
    BoxShadow, Context, Entity, FontWeight, Hsla, SharedString, Window, div, prelude::*, px,
};

use crate::background::glow_orb;
use crate::bridge::{self, RunningStore, SelectionStore};
use crate::format;
use crate::icons;
use crate::launch::LaunchButton;
use crate::modals;
use crate::text_input::{FieldStyle, TextField};
use crate::theme::{self, Theme};
use crate::toasts::{self, ToastVariant};
use crate::ui;

/// The header stats' separator dots (`text-neutral-700`).
const NEUTRAL_700: gpui::Rgba = theme::rgba_const(0x404040FF);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortMode {
    Recent,
    Name,
    Created,
}

impl SortMode {
    /// The `<select>`'s option labels (page.tsx).
    fn label(self) -> &'static str {
        match self {
            SortMode::Recent => "Sort: Recently played",
            SortMode::Name => "Sort: Name",
            SortMode::Created => "Sort: Newest",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Grid,
    List,
}

/// `a.name.localeCompare(b.name)` — JS's collation sorts ASCII names
/// case-insensitively first; a lowercase compare with a case-sensitive
/// tiebreak matches it for the names instances carry.
fn locale_cmp(a: &str, b: &str) -> Ordering {
    match a.to_lowercase().cmp(&b.to_lowercase()) {
        Ordering::Equal => a.cmp(b),
        other => other,
    }
}

/// CSS `capitalize` over `instance.loader` (the lowercase serde id).
fn capitalize(id: &'static str) -> String {
    let mut chars = id.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// The soft `shadow-xl` / `shadow-2xl` the dropdown panels and dialogs
/// carry (a plain dark drop, like `add_account_modal`'s).
pub(crate) fn drop_shadow(alpha: f32, y: f32, blur: f32, spread: f32) -> BoxShadow {
    BoxShadow {
        color: Hsla {
            a: alpha,
            ..Hsla::from(theme::rgba_const(0x00000000))
        },
        offset: gpui::point(px(0.0), px(y)),
        blur_radius: px(blur),
        spread_radius: px(spread),
    }
}

/// One ⋮-menu item (`ui/dropdown-menu.tsx`'s `DropdownMenuItem` — icon
/// 13, text-xs; `danger` items read `text-danger hover:bg-danger/10`).
/// A module-level fn because closures can't take `impl Trait` params.
#[allow(clippy::too_many_arguments)]
fn more_menu_item(
    key: SharedString,
    icon: &'static str,
    danger: bool,
    label: &'static str,
    cx: &mut Context<InstancesPage>,
    on_click: impl Fn(&mut InstancesPage, &mut Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let theme = *Theme::get(cx);
    let mut entry = div()
        .id(key)
        .flex()
        .w_full()
        .items_center()
        .gap_2()
        .rounded(px(8.0)) // rounded-lg
        .px(px(10.0)) // px-2.5
        .py(px(6.0)) // py-1.5
        .text_size(px(12.0)) // text-xs
        .text_color(if danger {
            theme.danger
        } else {
            theme.foreground
        })
        .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)));
    if danger {
        entry = entry.hover(move |style| style.bg(theme::tinted_rgba(0xE6455A, 0.10)));
    } else {
        entry = entry.hover(move |style| style.bg(theme::white(0.06)));
    }
    entry.child(icons::icon_at(icon, 13.0)).child(label)
}

/// One of the interrupted-migration banner's ghost buttons.
fn resolve_button(
    key: SharedString,
    theme: Theme,
    busy: bool,
    id: &str,
    label: &'static str,
    restore: bool,
    cx: &mut Context<InstancesPage>,
) -> gpui::Stateful<gpui::Div> {
    ui::button(
        key,
        theme,
        ui::ButtonVariant::Ghost,
        ui::ButtonSize::Sm,
        busy,
    )
    .on_click(cx.listener({
        let id = id.to_string();
        move |this, _, _, cx| this.resolve_interrupted(&id, restore, cx)
    }))
    .child(label)
}

pub struct InstancesPage {
    instances: Option<Vec<MinecraftInstance>>,
    /// The search bar (`query` state) — an entity because it owns IME
    /// focus; the page re-renders on its changes.
    query: Entity<TextField>,
    sort: SortMode,
    view: ViewMode,
    /// The sort `<select>`'s open state (no `PopupMenu` in gpui 0.2.2 —
    /// a hand-built menu, see the module docs).
    sort_open: bool,
    /// The ⋮ overflow menu's open instance, if any.
    open_menu: Option<String>,
    collapsed_groups: HashSet<String>,
    /// The per-card query caches (`useInstanceValidation` and the
    /// interrupted-migration query), keyed by instance id.
    validation: HashMap<String, InstanceValidationReport>,
    interrupted: HashMap<String, InProgressMarker>,
    /// Banner buttons mid-`resolve_interrupted_migration`.
    resolving: HashSet<String>,
    /// The folder button's inline error line, per card (`folderError`).
    folder_errors: HashMap<String, String>,
    /// One LaunchButton per card — the React page renders a separate
    /// button component per card (grid: default primary/md; list rows:
    /// `size="sm"`), so the native page keeps both maps.
    grid_buttons: HashMap<String, Entity<LaunchButton>>,
    list_buttons: HashMap<String, Entity<LaunchButton>>,
    /// In-flight fetch guards (`validation:<id>`, `interrupted:<id>`).
    pending: HashSet<String>,
    /// `useCountUp`'s eased animations — the header total (keyed
    /// `"total"`) and each card's playtime (keyed by instance id).
    anim_targets: HashMap<String, u64>,
    anim_values: HashMap<String, u64>,
}

impl InstancesPage {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| TextField::new(cx, "Search instances..."));

        let mut this = Self {
            instances: None,
            query,
            sort: SortMode::Recent,
            view: ViewMode::Grid,
            sort_open: false,
            open_menu: None,
            collapsed_groups: HashSet::new(),
            validation: HashMap::new(),
            interrupted: HashMap::new(),
            resolving: HashSet::new(),
            folder_errors: HashMap::new(),
            grid_buttons: HashMap::new(),
            list_buttons: HashMap::new(),
            pending: HashSet::new(),
            anim_targets: HashMap::new(),
            anim_values: HashMap::new(),
        };

        cx.observe(&this.query, |_, _, cx| cx.notify()).detach();
        cx.observe_global::<RunningStore>(|this, cx| {
            // `launch://exit` updates last_played/playtime — the same
            // trigger the Play page uses for its list refetch.
            this.refetch(cx);
            cx.notify();
        })
        .detach();
        cx.observe_global::<SelectionStore>(|_, cx| cx.notify())
            .detach();
        cx.observe_global::<modals::ModalStore>(|this, cx| {
            // The React app invalidates its instance queries after the
            // delete/duplicate dialogs; modal dismissal is where those
            // moments land here. The per-card caches reset so the
            // validation lines re-resolve against the fresh list.
            if cx.global::<modals::ModalStore>().all_closed() {
                this.validation.clear();
                this.interrupted.clear();
                this.pending.clear();
                this.refetch(cx);
            }
            cx.notify();
        })
        .detach();

        this.refetch(cx);
        this
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

    /// Runs after the instance list changes: rebuilds the LaunchButton
    /// entities (every card's button opens the console on launch start —
    /// the React page's `onLaunchStart`), refetches the missing per-card
    /// queries and restarts the count-up animations.
    fn after_instances_changed(&mut self, cx: &mut Context<Self>) {
        let instances = self.instances.clone().unwrap_or_default();

        for buttons in [&mut self.grid_buttons, &mut self.list_buttons] {
            buttons.retain(|id, _| instances.iter().any(|i| &i.id == id));
        }
        for instance in &instances {
            let id = instance.id.clone();
            self.grid_buttons.entry(id.clone()).or_insert_with(|| {
                let instance = instance.clone();
                cx.new(|_| {
                    LaunchButton::new(
                        instance,
                        true,
                        ui::ButtonSize::Md,
                        ui::ButtonVariant::Primary,
                    )
                })
            });
            self.list_buttons.entry(id).or_insert_with(|| {
                let instance = instance.clone();
                cx.new(|_| {
                    LaunchButton::new(
                        instance,
                        true,
                        ui::ButtonSize::Sm,
                        ui::ButtonVariant::Primary,
                    )
                })
            });
        }
        for instance in &instances {
            if let Some(button) = self.grid_buttons.get(&instance.id) {
                button.update(cx, |b, _| b.set_instance(instance.clone()));
            }
            if let Some(button) = self.list_buttons.get(&instance.id) {
                button.update(cx, |b, _| b.set_instance(instance.clone()));
            }
        }

        self.fetch_missing(cx);

        // `useCountUp`: the header total counts up from 0 on mount, and
        // each card's playtime from 0 (or its previous value) whenever
        // the query brings a new number.
        let total = instances.len() as u64;
        let total_from = self.anim_value("total");
        self.animate_value("total".into(), total_from, total, cx);
        for instance in &instances {
            let key = instance.id.clone();
            let target = instance.total_playtime_secs;
            if self.anim_targets.get(&key) != Some(&target) {
                let from = self.anim_value(&key);
                self.animate_value(key, from, target, cx);
            }
        }
    }

    fn anim_value(&self, key: &str) -> u64 {
        self.anim_values.get(key).copied().unwrap_or(0)
    }

    /// `useCountUp` — ease the displayed value toward the target over
    /// 500ms (cubic ease-out). Superseded when a newer target lands for
    /// the same key (play.rs's `animate_playtime`, generalized).
    fn animate_value(&mut self, key: String, from: u64, to: u64, cx: &mut Context<Self>) {
        if self.anim_targets.get(&key) == Some(&to) {
            return;
        }
        self.anim_targets.insert(key.clone(), to);
        cx.spawn(async move |this, cx| {
            let start = Instant::now();
            loop {
                let t = ((Instant::now() - start).as_secs_f32() / 0.5).min(1.0);
                let eased = 1.0 - (1.0 - t).powi(3);
                let value = (from as f32 + (to as f32 - from as f32) * eased).round() as u64;
                let done = t >= 1.0;
                let stop = this
                    .update(cx, |this, cx| {
                        if this.anim_targets.get(&key) != Some(&to) {
                            return true; // superseded by a newer animation
                        }
                        this.anim_values
                            .insert(key.clone(), if done { to } else { value });
                        cx.notify();
                        done
                    })
                    .unwrap_or(true);
                if stop {
                    return;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
            }
        })
        .detach();
    }

    /// The react-query hooks' initial fetches — the validation report
    /// and the interrupted-migration marker per card, guarded so each id
    /// is only in flight once (`staleTime` means they don't re-resolve
    /// until invalidated, which here is the modal-all-closed reset).
    fn fetch_missing(&mut self, cx: &mut Context<Self>) {
        let instances = self.instances.clone().unwrap_or_default();
        for instance in instances {
            let id = instance.id.clone();
            if !self.validation.contains_key(&id) {
                let key = format!("validation:{id}");
                if self.pending.insert(key.clone()) {
                    let task = {
                        let id = id.clone();
                        bridge::command(cx, move |state| async move {
                            commands::instances::validate_instance(&state, id).await
                        })
                    };
                    cx.spawn(async move |this, cx| {
                        let report = task.await.ok().and_then(|r| r.ok());
                        this.update(cx, |this, cx| {
                            this.pending.remove(&key);
                            if let Some(report) = report {
                                this.validation.insert(id, report);
                            }
                            cx.notify();
                        })
                        .ok();
                    })
                    .detach();
                }
            }

            let id = instance.id.clone();
            if !self.interrupted.contains_key(&id) {
                let key = format!("interrupted:{id}");
                if self.pending.insert(key.clone()) {
                    let task = {
                        let id = id.clone();
                        bridge::command(cx, move |state| async move {
                            commands::evolution::check_interrupted_migration(&state, id).await
                        })
                    };
                    cx.spawn(async move |this, cx| {
                        let marker = task.await.ok().and_then(|r| r.ok()).flatten();
                        this.update(cx, |this, cx| {
                            this.pending.remove(&key);
                            if let Some(marker) = marker {
                                this.interrupted.insert(id, marker);
                            }
                            cx.notify();
                        })
                        .ok();
                    })
                    .detach();
                }
            }
        }
    }

    // ---------------------------------------------------------------------
    // Card actions
    // ---------------------------------------------------------------------

    /// `updateInstance.mutate({ ...instance, favorite: !instance.favorite })`
    /// — flips the flag and lets the invalidation bring the new state
    /// back (no optimistic update in the React app either).
    fn toggle_favorite(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(instance) = self
            .instances
            .as_ref()
            .and_then(|list| list.iter().find(|i| i.id == id))
            .cloned()
        else {
            return;
        };
        let mut updated = instance.clone();
        updated.favorite = !updated.favorite;
        let task = bridge::command(cx, move |state| async move {
            commands::instances::update_instance(&state, updated).await
        });
        cx.spawn(async move |this, cx| {
            let updated = task.await.ok().and_then(|r| r.ok());
            this.update(cx, |this, cx| {
                if let Some(updated) = updated {
                    if let Some(slot) = this
                        .instances
                        .as_mut()
                        .and_then(|list| list.iter_mut().find(|i| i.id == updated.id))
                    {
                        *slot = updated.clone();
                    }
                    if let Some(button) = this.grid_buttons.get(&updated.id) {
                        let instance = updated.clone();
                        button.update(cx, |b, _| b.set_instance(instance));
                    }
                    if let Some(button) = this.list_buttons.get(&updated.id) {
                        let instance = updated.clone();
                        button.update(cx, |b, _| b.set_instance(instance));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `openPath(instance.game_dir)` — the folder button; the error
    /// renders as the card's inline red line (`folderError` state).
    fn open_folder(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(dir) = self
            .instances
            .as_ref()
            .and_then(|list| list.iter().find(|i| i.id == id))
            .map(|i| i.game_dir.clone())
        else {
            return;
        };
        let task = bridge::command(
            cx,
            move |_| async move { commands::system::open_path(&dir) },
        );
        let id = id.to_string();
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|e| {
                Err(AppError::new("Couldn't open that folder.", e.to_string()))
            });
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.folder_errors.remove(&id);
                    }
                    Err(err) => {
                        this.folder_errors.insert(id, err.message);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The interrupted-migration banner's two buttons
    /// (`api.evolution.resolveInterrupted(instance.id, restore)`).
    /// "Restore Original" also invalidates the instances list.
    fn resolve_interrupted(&mut self, id: &str, restore: bool, cx: &mut Context<Self>) {
        if !self.resolving.insert(id.to_string()) {
            return;
        }
        cx.notify();
        let id_owned = id.to_string();
        let task = bridge::command(cx, move |state| async move {
            commands::evolution::resolve_interrupted_migration(&state, id_owned, restore).await
        });
        let id = id.to_string();
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|e| {
                Err(AppError::new(
                    if restore {
                        "Couldn't restore."
                    } else {
                        "Couldn't dismiss this."
                    },
                    e.to_string(),
                ))
            });
            this.update(cx, |this, cx| {
                this.resolving.remove(&id);
                match result {
                    Ok(_) => {
                        this.interrupted.remove(&id);
                        if restore {
                            toasts::toast_success(cx, "Restored the original instance.");
                            this.refetch(cx);
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

    // ---------------------------------------------------------------------
    // Derived state — `visible` / `grouped` (page.tsx's useMemo hooks)
    // ---------------------------------------------------------------------

    fn query_text(&self, cx: &Context<Self>) -> String {
        self.query.read(cx).value().to_string()
    }

    /// The search + sort pipeline: filter (name / Minecraft version /
    /// loader, case-insensitive), sort by mode, then float favorites to
    /// the top (JS's stable sort keeps the mode order within each
    /// favorite group).
    fn visible(&self, cx: &Context<Self>) -> Vec<MinecraftInstance> {
        let Some(instances) = &self.instances else {
            return Vec::new();
        };
        let needle = self.query_text(cx).trim().to_lowercase();

        let mut filtered: Vec<MinecraftInstance> = instances
            .iter()
            .filter(|i| {
                if needle.is_empty() {
                    return true;
                }
                i.name.to_lowercase().contains(&needle)
                    || i.minecraft_version.to_lowercase().contains(&needle)
                    || i.loader.id().contains(&needle)
            })
            .cloned()
            .collect();

        match self.sort {
            SortMode::Name => filtered.sort_by(|a, b| locale_cmp(&a.name, &b.name)),
            SortMode::Created => filtered.sort_by_key(|i| Reverse(i.created_at)),
            SortMode::Recent => {
                filtered.sort_by_key(|i| Reverse(i.last_played.unwrap_or(i.created_at)))
            }
        }
        // Favorites always float to the top, ahead of whatever sort is
        // active (the stable sort preserves the order above).
        filtered.sort_by_key(|i| Reverse(i.favorite));
        filtered
    }

    /// Grouped view of `visible`: groups keep their first-appearance
    /// order, the ungrouped bucket always last.
    fn grouped(&self, cx: &Context<Self>) -> Vec<(String, Vec<MinecraftInstance>)> {
        let mut map: Vec<(String, Vec<MinecraftInstance>)> = Vec::new();
        for instance in self.visible(cx) {
            let key = instance
                .group
                .as_deref()
                .map(str::trim)
                .filter(|g| !g.is_empty())
                .unwrap_or("")
                .to_string();
            match map.iter_mut().find(|(k, _)| *k == key) {
                Some((_, items)) => items.push(instance),
                None => map.push((key, vec![instance])),
            }
        }
        map.sort_by(|(a, _), (b, _)| match (a.as_str(), b.as_str()) {
            ("", "") => Ordering::Equal,
            ("", _) => Ordering::Greater,
            (_, "") => Ordering::Less,
            (a, b) => locale_cmp(a, b),
        });
        map
    }

    /// `hasGroups` — computed off the full list, not the filtered one.
    fn has_groups(&self) -> bool {
        self.instances.as_ref().is_some_and(|list| {
            list.iter()
                .any(|i| i.group.as_deref().is_some_and(|g| !g.trim().is_empty()))
        })
    }

    fn running_count(&self, cx: &Context<Self>) -> usize {
        self.instances
            .as_ref()
            .map(|list| {
                list.iter()
                    .filter(|i| cx.global::<RunningStore>().is_running(&i.id))
                    .count()
            })
            .unwrap_or(0)
    }

    // ---------------------------------------------------------------------
    // Render pieces (page.tsx's InstancesPage / InstanceCard /
    // InstanceListRow / InterruptedMigrationBanner)
    // ---------------------------------------------------------------------

    fn header(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = *Theme::get(cx);
        let instances = self.instances.as_deref().unwrap_or(&[]);
        let favorite_count = instances.iter().filter(|i| i.favorite).count();
        let total_running = self.running_count(cx);

        // The header's blurred accent disc (`-top-20 left-16 h-48 w-72
        // rounded-full bg-accent/8 blur-[90px]`) — a pre-rasterized glow
        // orb like the Play banner's (gpui has no CSS blur).
        let orb = glow_orb(200.0, 45.0, 0.08, theme.accent);

        let mut stats = div().flex().items_center().gap_2();
        stats = stats.child(div().child(format!("{} total", self.anim_value("total"))));
        if favorite_count > 0 {
            stats = stats
                .child(div().text_color(NEUTRAL_700).child("·"))
                .child(div().child(format!("{favorite_count} favorited")));
        }
        if total_running > 0 {
            stats = stats.child(div().text_color(NEUTRAL_700).child("·")).child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_color(theme.success)
                    .child(
                        div()
                            .h(px(6.0)) // h-1.5
                            .w(px(6.0))
                            .flex_none()
                            .rounded_full()
                            .bg(theme.success), // animate-pulse renders statically
                    )
                    .child(format!("{total_running} running")),
            );
        }

        div()
            .id("instances-header")
            .relative()
            .flex()
            .items_center()
            .justify_between()
            .overflow_hidden()
            .rounded(px(16.0)) // rounded-2xl
            .border_1()
            .border_color(theme.border) // border-white/7
            .bg(theme::white(0.02))
            .p(px(20.0)) // p-5
            .child(orb.render().top(px(-80.0)).left(px(64.0)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(14.0)) // gap-3.5
                    .child(
                        // The 44px icon box (`h-11 w-11 rounded-xl border
                        // border-white/8 bg-linear-to-br from-white/8
                        // to-transparent`).
                        div()
                            .h(px(44.0))
                            .w(px(44.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(12.0))
                            .border_1()
                            .border_color(theme::white(0.08))
                            .bg(gpui::linear_gradient(
                                135.0,
                                gpui::linear_color_stop(theme::white(0.08), 0.0),
                                gpui::linear_color_stop(theme::rgba_const(0x00000000), 1.0),
                            ))
                            .child(icons::icon_at(icons::STACK, 19.0).text_color(theme.muted)),
                    )
                    .child(
                        div()
                            .child(
                                div()
                                    .text_size(px(18.0)) // text-lg
                                    .font_weight(FontWeight::EXTRA_BOLD)
                                    .text_color(theme::rgba_const(0xFFFFFFFF))
                                    .child("Instances"),
                            )
                            .child(
                                div()
                                    .mt(px(2.0)) // mt-0.5
                                    .flex()
                                    .items_center()
                                    .text_size(px(12.0)) // text-xs
                                    .text_color(theme.muted)
                                    .child(stats),
                            ),
                    ),
            )
            .child(
                // `relative flex gap-2` — "Import from launcher" opens the
                // import dialog, "New instance" the create dialog
                // (`setShowImport` / `setShowCreate`).
                div()
                    .flex()
                    .flex_none()
                    .gap(px(8.0)) // gap-2
                    .child(
                        ui::button(
                            "open-import",
                            theme,
                            ui::ButtonVariant::Ghost,
                            ui::ButtonSize::Sm,
                            false,
                        )
                        .on_click(cx.listener(|_, _, _, cx| modals::open_import(cx)))
                        .child(icons::icon_at(icons::DOWNLOAD_SIMPLE, 13.0))
                        .child("Import from launcher"),
                    )
                    .child(
                        ui::button(
                            "open-create",
                            theme,
                            ui::ButtonVariant::Primary,
                            ui::ButtonSize::Sm,
                            false,
                        )
                        .on_click(cx.listener(|_, _, _, cx| modals::open_create(cx)))
                        .child(icons::icon_at(icons::PLUS, 14.0))
                        .child("New instance"),
                    ),
            )
            .into_any_element()
    }

    /// The toolbar — search, view toggle and the sort select, shown once
    /// the list resolves with at least one instance.
    fn toolbar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = *Theme::get(cx);

        // The view toggle's buttons (`view === "grid" ? "bg-accent/20
        // text-accent" : "text-muted hover:bg-white/6"`).
        let view_button = |key: &'static str, icon: &'static str, active: bool| {
            let mut button = div()
                .id(key)
                .flex()
                .rounded(px(8.0)) // rounded-lg
                .p(px(6.0)); // p-1.5
            if active {
                button = button
                    .bg(theme::accent_alpha(0.20))
                    .text_color(theme.accent);
            } else {
                button = button
                    .text_color(theme.muted)
                    .hover(move |style| style.bg(theme::white(0.06)));
            }
            button.child(icons::icon_at(icon, 15.0))
        };

        div()
            .id("instances-toolbar")
            .flex()
            .items_center()
            .gap_2()
            .rounded(px(16.0)) // rounded-2xl
            .border_1()
            .border_color(theme.border) // border-white/6
            .bg(theme::white(0.02))
            .p(px(8.0)) // p-2
            .child(
                // `glass-input flex flex-1 items-center gap-2 rounded-xl
                // px-4 py-2.5` with the input transparent inside it.
                div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .gap_2()
                    .rounded(px(12.0)) // rounded-xl
                    .px(px(16.0)) // px-4
                    .py(px(10.0)) // py-2.5
                    .bg(theme::white(0.03))
                    .border_1()
                    .border_color(theme.border)
                    .child(icons::icon_at(icons::MAGNIFYING_GLASS, 15.0).text_color(theme.muted))
                    .child(self.query.update(cx, |field, cx| {
                        field.render_styled(
                            window,
                            cx,
                            &FieldStyle {
                                id: "instances-search-input",
                                key_context: "InstancesSearchInput",
                                height: 20.0,
                                rounded: 0.0,
                                text_size: 14.0,
                                padding_x: 0.0,
                                transparent: true,
                            },
                        )
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap_1()
                    .rounded(px(12.0)) // rounded-xl
                    .border_1()
                    .border_color(theme.border) // border-white/6
                    .bg(theme::white(0.03))
                    .p(px(4.0)) // p-1
                    .child(view_button(
                        "view-grid",
                        icons::SQUARES_FOUR,
                        self.view == ViewMode::Grid,
                    ))
                    .child(view_button(
                        "view-list",
                        icons::LIST,
                        self.view == ViewMode::List,
                    )),
            )
            .child(self.sort_control(cx))
            .into_any_element()
    }

    /// The sort `<select>` — `glass-input rounded-lg py-2.5 pl-3 pr-7
    /// text-xs` with the caret overlay, plus its open menu. A custom
    /// menu stands in for the native `<select>` dropdown (gpui has no
    /// PopupMenu); it opens upward, see the module docs. It closes on
    /// selection or re-clicking the trigger — the React menu also closes
    /// on any outside click, which needs the portal machinery the modals
    /// use.
    fn sort_control(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = *Theme::get(cx);

        let mut wrapper = div().relative().flex_none().child(
            div()
                .id("sort-select")
                .flex()
                .items_center()
                .rounded(px(8.0)) // rounded-lg
                .pl(px(12.0)) // pl-3
                .pr(px(28.0)) // pr-7
                .py(px(10.0)) // py-2.5
                .bg(theme::white(0.03))
                .border_1()
                .border_color(theme.border)
                .text_size(px(12.0)) // text-xs
                .text_color(theme.foreground)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.sort_open = !this.sort_open;
                    cx.notify();
                }))
                .child(self.sort.label()),
        );

        if self.sort_open {
            let mut menu = div()
                .id("sort-menu")
                .absolute()
                .bottom(px(42.0)) // the trigger's top edge + the mt-1.5 gap
                .left(px(0.0))
                .w(px(176.0)) // min-w-[168px]
                .rounded(px(12.0)) // rounded-xl
                .bg(theme.surface)
                .border_1()
                .border_color(theme::white(0.10))
                .p(px(4.0)) // p-1
                .shadow(vec![drop_shadow(0.35, 10.0, 30.0, -10.0)]);
            for mode in [SortMode::Recent, SortMode::Name, SortMode::Created] {
                let selected = mode == self.sort;
                let mut item = div()
                    .id(SharedString::from(format!("sort-{:?}", mode)))
                    .flex()
                    .w_full()
                    .items_center()
                    .rounded(px(8.0)) // rounded-lg
                    .px(px(10.0)) // px-2.5
                    .py(px(6.0)) // py-1.5
                    .text_size(px(12.0)) // text-xs
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.sort = mode;
                        this.sort_open = false;
                        cx.notify();
                    }));
                if selected {
                    item = item.text_color(theme.accent).bg(theme::white(0.03));
                } else {
                    item = item
                        .text_color(theme.foreground)
                        .hover(move |style| style.bg(theme::white(0.06)));
                }
                menu = menu.child(item.child(mode.label()));
            }
            wrapper = wrapper.child(menu);
        }

        wrapper.into_any_element()
    }

    /// `InstanceCardSkeleton` — 6 while the list loads (`.skeleton` =
    /// white/6, pulse renders statically).
    fn skeleton(&self, cx: &Context<Self>) -> gpui::AnyElement {
        let theme = *Theme::get(cx);
        let block = |key: &'static str, w: f32, h: f32, radius: f32| {
            div()
                .id(key)
                .w(px(w))
                .h(px(h))
                .flex_none()
                .rounded(px(radius))
                .bg(theme::white(0.06))
        };
        let mut grid = div().grid().grid_cols(3).gap(px(16.0)); // gap-4
        for i in 0..6 {
            grid = grid.child(
                div()
                    .id(SharedString::from(format!("skeleton-{i}")))
                    .rounded(px(16.0)) // rounded-2xl
                    .p(px(20.0)) // p-5
                    .flex()
                    .flex_col()
                    .gap(px(12.0)) // gap-3
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.0)) // gap-3
                            .child(block("skel-icon", 44.0, 44.0, 12.0))
                            .child(
                                div()
                                    .flex()
                                    .flex_1()
                                    .flex_col()
                                    .gap(px(8.0)) // gap-2
                                    .child(block("skel-line-1", 96.0, 16.0, 8.0))
                                    .child(block("skel-line-2", 64.0, 12.0, 8.0)),
                            ),
                    )
                    .child(block("skel-line-3", 128.0, 12.0, 8.0))
                    .child(
                        div()
                            .mt_auto()
                            .flex()
                            .items_center()
                            .gap(px(6.0)) // gap-1.5
                            .pt(px(8.0)) // pt-2
                            .child(block("skel-btn-1", 80.0, 32.0, 8.0))
                            .child(block("skel-btn-2", 64.0, 32.0, 8.0)),
                    ),
            );
        }
        grid.into_any_element()
    }

    /// The empty state's "New instance" button opens the create dialog.
    fn empty_state(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = *Theme::get(cx);
        div()
            .id("instances-empty")
            .flex()
            .flex_col()
            .items_center()
            .rounded(px(16.0)) // rounded-2xl
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .p(px(40.0)) // p-10
            .child(icons::icon_at(icons::STACK, 28.0).text_color(theme.muted))
            .child(
                div()
                    .mt(px(12.0)) // gap-3
                    .text_size(px(14.0)) // text-sm
                    .font_weight(FontWeight::MEDIUM)
                    .child("No instances yet"),
            )
            .child(
                div()
                    .text_size(px(12.0)) // text-xs
                    .text_color(theme.muted)
                    .child("Create one to get started, or install a modpack from the Mods page."),
            )
            .child(
                ui::button(
                    "empty-new-instance",
                    theme,
                    ui::ButtonVariant::Primary,
                    ui::ButtonSize::Sm,
                    false,
                )
                .mt(px(8.0)) // mt-2
                .on_click(cx.listener(|_, _, _, cx| modals::open_create(cx)))
                .child(icons::icon_at(icons::PLUS, 14.0))
                .child("New instance"),
            )
            .into_any_element()
    }

    fn no_match(&self, cx: &Context<Self>, query: &str) -> gpui::AnyElement {
        let theme = *Theme::get(cx);
        // gpui has no text-align — the single line centers via flex.
        div()
            .id("instances-no-match")
            .flex()
            .justify_center()
            .rounded(px(16.0)) // rounded-2xl
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .p(px(32.0)) // p-8
            .text_size(px(14.0)) // text-sm
            .text_color(theme.muted)
            .child(format!("No instances match \"{query}\"."))
            .into_any_element()
    }

    /// The grid container (`grid grid-cols-1 gap-4 sm:grid-cols-2
    /// lg:grid-cols-3` at the native window's desktop width → 3 cols).
    fn grid(&mut self, items: &[MinecraftInstance], cx: &mut Context<Self>) -> gpui::AnyElement {
        let selected = cx.global::<SelectionStore>().selected.clone();
        let mut grid = div().grid().grid_cols(3).gap(px(16.0)); // gap-4
        for instance in items {
            let selected = selected.as_deref() == Some(instance.id.as_str());
            grid = grid.child(self.card(instance, selected, cx));
        }
        grid.into_any_element()
    }

    /// The list container (`flex flex-col gap-2`).
    fn list(&mut self, items: &[MinecraftInstance], cx: &mut Context<Self>) -> gpui::AnyElement {
        let selected = cx.global::<SelectionStore>().selected.clone();
        let mut column = div().flex().flex_col().gap(px(8.0)); // gap-2
        for instance in items {
            let selected = selected.as_deref() == Some(instance.id.as_str());
            column = column.child(self.list_row(instance, selected, cx));
        }
        column.into_any_element()
    }

    /// `InstanceCard` — the glass card with the gradient header, the
    /// favorite/loader badges, the icon + name row, the validation and
    /// playtime lines, and the action row.
    fn card(
        &mut self,
        instance: &MinecraftInstance,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = *Theme::get(cx);
        let id = instance.id.clone();
        let running = cx.global::<RunningStore>().is_running(&id);
        let animated_playtime = self.anim_value(&id);

        // The 135° gradient header (the React card's thumbnail
        // fallback), the bottom shade over it, and the two badges.
        let favorite = instance.favorite;
        let mut header = div()
            .relative()
            .h(px(128.0)) // h-32
            .w_full()
            .flex_none()
            .overflow_hidden()
            .rounded(px(16.0)) // rounded-t-2xl
            .bg(gpui::linear_gradient(
                135.0,
                gpui::linear_color_stop(theme::accent_alpha(0.35), 0.0),
                gpui::linear_color_stop(theme::white(0.03), 1.0),
            ))
            .child(
                // `bg-linear-to-t from-black/50 via-black/0 to-black/10`
                // — gpui's two-stop gradient blends the endpoints.
                div().absolute().inset_0().bg(gpui::linear_gradient(
                    0.0,
                    gpui::linear_color_stop(theme::tinted_rgba(0x000000, 0.50), 0.0),
                    gpui::linear_color_stop(theme::tinted_rgba(0x000000, 0.10), 1.0),
                )),
            )
            .child(
                div()
                    .id(SharedString::from(format!("favorite-{id}")))
                    .absolute()
                    .left(px(8.0)) // left-2
                    .top(px(8.0)) // top-2
                    .h(px(24.0)) // h-6
                    .w(px(24.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(theme::rgba_const(0x00000066)) // bg-black/40
                    .hover(move |style| style.bg(theme::rgba_const(0x00000099))) // hover:bg-black/60
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, _, cx| this.toggle_favorite(&id, cx)
                    }))
                    .child(
                        icons::icon_at(
                            if favorite {
                                icons::STAR_FILL
                            } else {
                                icons::STAR
                            },
                            12.0,
                        )
                        .text_color(if favorite {
                            theme.warning
                        } else {
                            theme::white(0.80)
                        }),
                    ),
            );
        if instance.loader != ModLoaderKind::Vanilla {
            header = header.child(
                div()
                    .absolute()
                    .right(px(8.0)) // right-2
                    .top(px(8.0))
                    .child(
                        ui::badge(theme)
                            .border_color(theme::white(0.20))
                            .bg(theme::rgba_const(0x00000066)) // bg-black/40
                            .text_color(theme::white(1.0))
                            .child(capitalize(instance.loader.id())),
                    ),
            );
        }

        // The icon + name row. The 40px icon is the React app's
        // null-icon fallback (the `img` icon is a data: URL — deferred).
        let mut badges = div().flex().flex_wrap().items_center().gap(px(6.0)); // gap-1.5
        badges = badges.child(
            div()
                .truncate()
                .text_size(px(15.0)) // text-[15px]
                .font_weight(FontWeight::BOLD)
                .child(instance.name.clone()),
        );
        if selected {
            badges = badges.child(
                ui::badge(theme)
                    .border_color(theme::accent_alpha(0.40))
                    .bg(theme::accent_alpha(0.15))
                    .text_color(theme.accent)
                    .child("Selected"),
            );
        }
        if running {
            badges = badges.child(
                ui::badge(theme)
                    .border_color(theme::tinted_rgba(0x4ADE80, 0.40))
                    .bg(theme::tinted_rgba(0x4ADE80, 0.15))
                    .text_color(theme.success)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .mr(px(4.0)) // mr-1
                                    .h(px(6.0))
                                    .w(px(6.0))
                                    .rounded_full()
                                    .bg(theme.success), // animate-pulse renders statically
                            )
                            .child("Running"),
                    ),
            );
        }
        if let Some(group) = instance
            .group
            .as_deref()
            .map(str::trim)
            .filter(|g| !g.is_empty())
        {
            badges = badges.child(ui::badge(theme).child(group.to_string()));
        }

        let mut card = ui::glass_card(theme)
            .id(SharedString::from(format!("instance-card-{id}")))
            .relative()
            .flex()
            .flex_col()
            .rounded(px(16.0)); // rounded-2xl
        if selected {
            // `border-accent/60` (+ the `ring-1 ring-accent/40` as an
            // inset outline child below — gpui has no box-shadow rings
            // on arbitrary elements, so the ring is a border one pixel
            // in).
            card = card.border_color(theme::accent_alpha(0.60));
        }

        card = card.child(header).child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .gap(px(12.0)) // gap-3
                .p(px(16.0)) // p-4
                .children(self.interrupted_banner(instance, cx))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.0)) // gap-3
                        .child(
                            div()
                                .h(px(40.0)) // h-10
                                .w(px(40.0))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(12.0)) // rounded-xl
                                .bg(theme::white(0.05))
                                .child(icons::icon_at(icons::STACK, 17.0).text_color(theme.muted)),
                        )
                        .child(
                            div().min_w_0().flex_1().child(badges).child(
                                div()
                                    .truncate()
                                    .text_size(px(12.0)) // text-xs
                                    .text_color(theme.muted)
                                    .child(match instance.loader {
                                        ModLoaderKind::Vanilla => {
                                            format!("Minecraft {}", instance.minecraft_version)
                                        }
                                        _ => format!(
                                            "Minecraft {} · {}",
                                            instance.minecraft_version,
                                            instance.loader.id()
                                        ),
                                    }),
                            ),
                        ),
                )
                .children(self.validation_line(instance, cx))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0)) // gap-1.5
                        .text_size(px(11.0)) // text-[11px]
                        .text_color(theme.muted)
                        .child(icons::icon_at(icons::CLOCK, 11.0).flex_none())
                        .child(div().truncate().child(match instance.last_played {
                            Some(_) => format!(
                                "Last played {} · {}",
                                format::time_ago(instance.last_played),
                                format::format_playtime(animated_playtime)
                            ),
                            None => "Never played".to_string(),
                        })),
                )
                // The pack-update banner lands with the settings
                // slice's network plumbing (`useModpackUpdateCheck`).
                .child(self.action_row(instance, selected, cx))
                .children(self.folder_errors.get(&id).map(|message| {
                    div()
                        .text_size(px(12.0)) // text-xs
                        .text_color(theme.danger)
                        .child(message.clone())
                })),
        );

        if selected {
            // The `ring-1 ring-accent/40` — an inset outline painted
            // above the card's content (a plain div hit-tests nothing).
            card = card.child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(px(16.0))
                    .border_1()
                    .border_color(theme::accent_alpha(0.40)),
            );
        }

        if self.open_menu.as_deref() == Some(id.as_str()) {
            // The ⋮ menu, opening upward from the action row's right end
            // (see the module docs for the paint-order reasoning). A
            // card-covering backdrop closes it on click.
            card = card
                .child(
                    div()
                        .id(SharedString::from(format!("menu-backdrop-{id}")))
                        .absolute()
                        .inset_0()
                        .occlude()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_menu = None;
                            cx.notify();
                        })),
                )
                .child(self.more_menu(instance, 48.0, 16.0, cx));
        }

        card.into_any_element()
    }

    /// The action row: Select, the LaunchButton, and (right) the folder
    /// button + ⋮ menu trigger. "Manage mods" (the selected variant) is
    /// deferred with the Mods page; Configure / Change version (the
    /// wrench / arrows icon buttons) are deferred with their dialogs.
    fn action_row(
        &mut self,
        instance: &MinecraftInstance,
        _selected: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = *Theme::get(cx);
        let id = instance.id.clone();

        let icon_button = |key: SharedString, icon: &'static str| {
            div()
                .id(key)
                .rounded(px(8.0)) // rounded-lg
                .p(px(6.0)) // p-1.5
                .flex()
                .text_color(theme.muted)
                .hover(move |style| style.bg(theme::white(0.06)).text_color(theme.foreground))
                .child(icons::icon_at(icon, 14.0))
        };

        div()
            .mt_auto()
            .flex()
            .items_center()
            .gap(px(6.0)) // gap-1.5
            .pt(px(4.0)) // pt-1
            .child(
                ui::button(
                    SharedString::from(format!("select-{id}")),
                    theme,
                    ui::ButtonVariant::Ghost,
                    ui::ButtonSize::Sm,
                    false,
                )
                .on_click(cx.listener({
                    let id = id.clone();
                    move |_, _, _, cx| {
                        bridge::set_selected_instance(cx, Some(id.clone()));
                    }
                }))
                .child("Select"),
            )
            .children(self.grid_buttons.get(&id).cloned())
            .child(
                div()
                    .ml_auto()
                    .flex()
                    .items_center()
                    .gap(px(2.0)) // gap-0.5
                    .border_l_1()
                    .border_color(theme.border) // border-white/6
                    .pl(px(6.0)) // pl-1.5
                    .child(
                        icon_button(
                            SharedString::from(format!("folder-{id}")),
                            icons::FOLDER_OPEN,
                        )
                        .on_click(cx.listener({
                            let id = id.clone();
                            move |this, _, _, cx| this.open_folder(&id, cx)
                        })),
                    )
                    .child(
                        icon_button(
                            SharedString::from(format!("more-{id}")),
                            icons::DOTS_THREE_VERTICAL,
                        )
                        .on_click(cx.listener({
                            let id = id.clone();
                            move |this, _, _, cx| {
                                this.open_menu = Some(id.clone());
                                cx.notify();
                            }
                        })),
                    ),
            )
            .into_any_element()
    }

    /// The ⋮ overflow menu — `ui/dropdown-menu.tsx`'s panel
    /// (`glass-panel min-w-[168px] rounded-xl border border-white/10
    /// p-1 shadow-xl`) with its items (icon 13, text-xs). Opens upward
    /// from the trigger: `bottom` = the trigger's top edge (card
    /// padding + button + gap), `right` = the trigger's right edge.
    /// Performance boost / Change version / Version history / Export
    /// are deferred with their dialogs; Console / Duplicate / Remove
    /// remain.
    fn more_menu(
        &mut self,
        instance: &MinecraftInstance,
        bottom: f32,
        right: f32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = *Theme::get(cx);
        let id = instance.id.clone();

        let mut menu = div()
            .id(SharedString::from(format!("more-menu-{id}")))
            .absolute()
            .bottom(px(bottom))
            .right(px(right))
            .w(px(176.0)) // min-w-[168px]
            .rounded(px(12.0)) // rounded-xl
            .bg(theme.surface)
            .border_1()
            .border_color(theme::white(0.10))
            .p(px(4.0)) // p-1
            .shadow(vec![drop_shadow(0.35, 10.0, 30.0, -10.0)]);

        menu = menu.child(more_menu_item(
            SharedString::from(format!("menu-console-{id}")),
            icons::TERMINAL_WINDOW,
            false,
            "Console",
            cx,
            {
                let instance = instance.clone();
                move |this, _, cx| {
                    this.open_menu = None;
                    modals::open_console(cx, instance.id.clone(), instance.name.clone());
                }
            },
        ));
        menu = menu.child(more_menu_item(
            SharedString::from(format!("menu-duplicate-{id}")),
            icons::STACK,
            false,
            "Duplicate",
            cx,
            {
                let instance = instance.clone();
                move |this, _, cx| {
                    this.open_menu = None;
                    modals::open_duplicate(cx, instance.id.clone(), instance.name.clone());
                }
            },
        ));
        menu = menu.child(
            div()
                .mt(px(4.0)) // my-1
                .mb(px(4.0))
                .h(px(1.0))
                .bg(theme::white(0.08)),
        );
        menu = menu.child(more_menu_item(
            SharedString::from(format!("menu-remove-{id}")),
            icons::TRASH,
            true,
            "Remove",
            cx,
            {
                let instance = instance.clone();
                move |this, _, cx| {
                    this.open_menu = None;
                    modals::open_delete(
                        cx,
                        instance.id.clone(),
                        instance.name.clone(),
                        instance.imported,
                    )
                }
            },
        ));

        menu.into_any_element()
    }

    /// `InterruptedMigrationBanner` — the two ghost buttons resolve the
    /// marker (`restore` = "Restore Original").
    fn interrupted_banner(
        &mut self,
        instance: &MinecraftInstance,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let theme = *Theme::get(cx);
        let marker = self.interrupted.get(&instance.id)?;
        let id = instance.id.clone();
        let busy = self.resolving.contains(&id);

        Some(
            div()
                .rounded(px(8.0)) // rounded-lg
                .border_1()
                .border_color(theme::tinted_rgba(0xFBBF24, 0.30)) // border-warning/30
                .bg(theme::tinted_rgba(0xFBBF24, 0.10)) // bg-warning/10
                .p(px(10.0)) // p-2.5
                .text_size(px(12.0)) // text-xs
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0)) // gap-1.5
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.warning)
                        .child(icons::icon_at(icons::WARNING_CIRCLE, 13.0))
                        .child("Incomplete migration detected"),
                )
                .child(
                    div()
                        .mt(px(4.0)) // mt-1
                        .text_color(theme.muted)
                        .child(format!(
                            "{} → {} didn't finish (Arcane closed mid-migration).",
                            marker.source_version, marker.target_version
                        )),
                )
                .child(
                    div()
                        .mt(px(8.0)) // mt-2
                        .flex()
                        .gap(px(6.0)) // gap-1.5
                        .child(resolve_button(
                            SharedString::from(format!("restore-{id}")),
                            theme,
                            busy,
                            &id,
                            "Restore Original",
                            true,
                            cx,
                        ))
                        .child(resolve_button(
                            SharedString::from(format!("dismiss-{id}")),
                            theme,
                            busy,
                            &id,
                            "Dismiss",
                            false,
                            cx,
                        )),
                )
                .into_any_element(),
        )
    }

    /// The validation line — the warning box when the instance needs
    /// attention, the glowing green dot when it's ready.
    fn validation_line(
        &self,
        instance: &MinecraftInstance,
        cx: &Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let theme = *Theme::get(cx);
        let report = self.validation.get(&instance.id)?;

        let element = if report.ready {
            div()
                .flex()
                .items_center()
                .gap(px(6.0)) // gap-1.5
                .text_size(px(12.0)) // text-xs
                .text_color(theme.success)
                .child(
                    div()
                        .h(px(6.0)) // h-1.5
                        .w(px(6.0))
                        .flex_none()
                        .rounded_full()
                        .bg(theme.success)
                        .shadow(vec![drop_shadow(0.5, 0.0, 6.0, 1.0)]),
                )
                .child("Ready to play offline")
                .into_any_element()
        } else {
            div()
                .flex()
                .items_center()
                .gap(px(6.0)) // gap-1.5
                .rounded(px(8.0)) // rounded-lg
                .border_1()
                .border_color(theme::tinted_rgba(0xFBBF24, 0.25)) // border-warning/25
                .bg(theme::tinted_rgba(0xFBBF24, 0.08)) // bg-warning/8
                .px(px(10.0)) // px-2.5
                .py(px(6.0)) // py-1.5
                .text_size(px(12.0)) // text-xs
                .text_color(theme.warning)
                .child(icons::icon_at(icons::WARNING_CIRCLE, 12.0).flex_none())
                .child(
                    div().truncate().child(
                        report
                            .issues
                            .first()
                            .map(|i| i.message.clone())
                            .unwrap_or_else(|| "Needs attention.".to_string()),
                    ),
                )
                .into_any_element()
        };
        Some(element)
    }

    /// `InstanceListRow` — the compact row variant.
    fn list_row(
        &mut self,
        instance: &MinecraftInstance,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = *Theme::get(cx);
        let id = instance.id.clone();
        let running = cx.global::<RunningStore>().is_running(&id);
        let validation = self.validation.get(&id);
        let animated_playtime = self.anim_value(&id);

        let mut badges = div().flex().flex_wrap().items_center().gap(px(6.0));
        badges = badges.child(
            div()
                .truncate()
                .text_size(px(14.0)) // text-sm
                .font_weight(FontWeight::SEMIBOLD)
                .child(instance.name.clone()),
        );
        if selected {
            badges = badges.child(
                ui::badge(theme)
                    .border_color(theme::accent_alpha(0.40))
                    .bg(theme::accent_alpha(0.15))
                    .text_color(theme.accent)
                    .child("Selected"),
            );
        }
        if running {
            badges = badges.child(
                ui::badge(theme)
                    .border_color(theme::tinted_rgba(0x4ADE80, 0.40))
                    .bg(theme::tinted_rgba(0x4ADE80, 0.15))
                    .text_color(theme.success)
                    .child("Running"),
            );
        }

        let needs_attention = validation.is_some_and(|v| !v.ready);

        let mut row = ui::glass_card(theme)
            .id(SharedString::from(format!("instance-row-{id}")))
            .relative()
            .flex()
            .items_center()
            .gap(px(12.0)) // gap-3
            .rounded(px(16.0)); // rounded-2xl (GlassCard's base)
        if selected {
            row = row.border_color(theme::accent_alpha(0.60));
        }

        row = row
            .child(
                div()
                    .id(SharedString::from(format!("row-favorite-{id}")))
                    .flex_none()
                    .rounded_full()
                    .p(px(4.0)) // p-1
                    .text_color(if instance.favorite {
                        theme.warning
                    } else {
                        theme.muted
                    })
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, _, cx| this.toggle_favorite(&id, cx)
                    }))
                    .hover(move |style| style.text_color(theme.warning))
                    .child(icons::icon_at(
                        if instance.favorite {
                            icons::STAR_FILL
                        } else {
                            icons::STAR
                        },
                        14.0,
                    )),
            )
            .child(
                // The 36px icon — the null-icon fallback (deferred `img`).
                div()
                    .h(px(36.0)) // h-9
                    .w(px(36.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.0)) // rounded-lg
                    .bg(theme::white(0.05))
                    .child(icons::icon_at(icons::STACK, 15.0).text_color(theme.muted)),
            )
            .child(
                div().min_w_0().flex_1().child(badges).child(
                    div()
                        .truncate()
                        .text_size(px(12.0)) // text-xs
                        .text_color(theme.muted)
                        .child({
                            let mut line = format!("Minecraft {}", instance.minecraft_version);
                            if instance.loader != ModLoaderKind::Vanilla {
                                line.push_str(&format!(" · {}", instance.loader.id()));
                            }
                            if needs_attention {
                                line.push_str(" · Needs attention");
                            }
                            line
                        }),
                ),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(12.0)) // text-xs
                    .text_color(theme.muted)
                    .child(match instance.last_played {
                        Some(_) => format!(
                            "{} · {}",
                            format::time_ago(instance.last_played),
                            format::format_playtime(animated_playtime)
                        ),
                        None => "Never played".to_string(),
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.0)) // gap-1.5
                    .child(
                        ui::button(
                            SharedString::from(format!("row-select-{id}")),
                            theme,
                            ui::ButtonVariant::Ghost,
                            ui::ButtonSize::Sm,
                            false,
                        )
                        .on_click(cx.listener({
                            let id = id.clone();
                            move |_, _, _, cx| {
                                bridge::set_selected_instance(cx, Some(id.clone()));
                            }
                        }))
                        .child("Select"),
                    )
                    .child(div().flex().children(self.list_buttons.get(&id).cloned()))
                    .child(
                        div()
                            .id(SharedString::from(format!("row-more-{id}")))
                            .rounded(px(8.0)) // rounded-lg
                            .p(px(6.0)) // p-1.5
                            .flex()
                            .text_color(theme.muted)
                            .hover(move |style| {
                                style.bg(theme::white(0.06)).text_color(theme.foreground)
                            })
                            .on_click(cx.listener({
                                let id = id.clone();
                                move |this, _, _, cx| {
                                    this.open_menu = Some(id.clone());
                                    cx.notify();
                                }
                            }))
                            .child(icons::icon_at(icons::DOTS_THREE_VERTICAL, 14.0)),
                    ),
            );

        if selected {
            row = row.child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(px(16.0))
                    .border_1()
                    .border_color(theme::accent_alpha(0.40)),
            );
        }

        if self.open_menu.as_deref() == Some(id.as_str()) {
            row = row
                .child(
                    div()
                        .id(SharedString::from(format!("menu-backdrop-{id}")))
                        .absolute()
                        .inset_0()
                        .occlude()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_menu = None;
                            cx.notify();
                        })),
                )
                .child(self.more_menu(instance, 46.0, 12.0, cx));
        }

        row.into_any_element()
    }
}

impl gpui::Render for InstancesPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *Theme::get(cx);

        let mut root = div()
            .id("instances-page")
            .flex()
            .flex_col()
            .gap(px(24.0)) // gap-6
            .child(self.header(cx));

        // The toolbar shows once the list resolves non-empty.
        if self.instances.as_ref().is_some_and(|list| !list.is_empty()) {
            root = root.child(self.toolbar(window, cx));
        }

        // The cards section (or the loading/empty/no-match state).
        let loaded = self.instances.as_ref().map(|list| !list.is_empty());
        let cards: Option<gpui::AnyElement> = match loaded {
            // The list hasn't resolved yet — the six skeletons.
            None => Some(self.skeleton(cx)),
            Some(false) => Some(self.empty_state(cx)),
            Some(true) => {
                let visible = self.visible(cx);
                if visible.is_empty() {
                    let query = self.query_text(cx);
                    Some(self.no_match(cx, &query))
                } else if self.has_groups() {
                    // Grouped sections (`hasGroups`), the ungrouped
                    // bucket last.
                    let mut sections = div().flex().flex_col().gap(px(24.0)); // gap-6
                    for (name, items) in self.grouped(cx) {
                        let collapsed = self.collapsed_groups.contains(&name);
                        let count = items.len();
                        let toggle_key = SharedString::from(format!(
                            "group-{}",
                            if name.is_empty() { "ungrouped" } else { &name }
                        ));
                        // gpui has no text-transform — the group name
                        // uppercases directly (the `({count})` suffix
                        // keeps its normal case).
                        let label = if name.is_empty() {
                            "UNGROUPED".to_string()
                        } else {
                            name.to_uppercase()
                        };
                        sections = sections.child(
                            div()
                                .id(toggle_key)
                                .mb(px(12.0)) // mb-3
                                .flex()
                                .w_full()
                                .items_center()
                                .gap(px(6.0)) // gap-1.5
                                .border_b_1()
                                .border_color(theme.border) // border-white/6
                                .pb(px(8.0)) // pb-2
                                .text_size(px(12.0)) // text-xs
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.muted)
                                .on_click(cx.listener({
                                    let name = name.clone();
                                    move |this, _, _, cx| {
                                        if !this.collapsed_groups.remove(&name) {
                                            this.collapsed_groups.insert(name.clone());
                                        }
                                        cx.notify();
                                    }
                                }))
                                .hover(move |style| style.text_color(theme.foreground))
                                .child(icons::icon_at(
                                    if collapsed {
                                        icons::CARET_RIGHT
                                    } else {
                                        icons::CARET_DOWN
                                    },
                                    11.0,
                                ))
                                .child(label)
                                .child(
                                    div()
                                        .font_weight(FontWeight::NORMAL)
                                        .text_color(theme::tinted_rgba(0x96969D, 0.70))
                                        .child(format!("({count})")),
                                ),
                        );
                        if !collapsed {
                            sections = sections.child(match self.view {
                                ViewMode::Grid => self.grid(&items, cx),
                                ViewMode::List => self.list(&items, cx),
                            });
                        }
                    }
                    Some(sections.into_any_element())
                } else {
                    Some(match self.view {
                        ViewMode::Grid => self.grid(&visible, cx),
                        ViewMode::List => self.list(&visible, cx),
                    })
                }
            }
        };
        if let Some(cards) = cards {
            root = root.child(cards);
        }

        root.into_any_element()
    }
}

// ---------------------------------------------------------------------------
// The delete / duplicate dialogs — page.tsx's `DeleteInstanceDialog` and
// `DuplicateInstanceDialog`. The shell creates them from their `ModalStore`
// specs so they paint above everything else (like the launch dialogs).
// ---------------------------------------------------------------------------

pub struct DeleteInstanceDialog {
    spec: modals::DeleteSpec,
    delete_files: bool,
    busy: bool,
}

impl DeleteInstanceDialog {
    pub fn new(spec: &modals::DeleteSpec) -> Self {
        Self {
            spec: spec.clone(),
            delete_files: false,
            busy: false,
        }
    }

    /// `del.mutateAsync({ id, deleteFiles })` — the React mutation has no
    /// error UI here (its onSuccess invalidates the list; a failure
    /// surfaces as a toast like the other screens' mutations).
    fn confirm(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        cx.notify();
        let id = self.spec.instance_id.clone();
        let delete_files = self.delete_files;
        let task = bridge::command(cx, move |state| async move {
            commands::instances::delete_instance(&state, id, delete_files).await
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .unwrap_or_else(|e| Err(AppError::new("Removal failed.", e.to_string())));
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        toasts::toast(
                            cx,
                            ToastVariant::Info,
                            format!("Removed {}.", this.spec.name),
                        );
                        modals::close_delete(cx);
                    }
                    Err(err) => toasts::toast_error(cx, err.message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl gpui::Render for DeleteInstanceDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *Theme::get(cx);
        let imported = self.spec.imported;

        // The native `<input type="checkbox">` under `color-scheme: dark`
        // — see `ui::dark_checkbox`. The disabled (imported) checkbox is
        // inert; the label row has no click of its own (React's `<label>`
        // wraps only the checkbox+text).
        let checkbox =
            ui::dark_checkbox("delete-files-checkbox", theme, self.delete_files, imported)
                .mt(px(2.0)); // mt-0.5
        let checkbox = if imported {
            checkbox
        } else {
            checkbox.on_click(cx.listener(|this, _, _, cx| {
                this.delete_files = !this.delete_files;
                cx.notify();
            }))
        };

        div()
            .id("delete-instance-backdrop")
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
                    .w(px(384.0)) // max-w-sm
                    .flex()
                    .flex_col()
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
                            .child(format!("Remove {}?", self.spec.name)),
                    )
                    .child(
                        div()
                            .mt(px(4.0)) // mt-1
                            .text_size(px(12.0)) // text-xs
                            .text_color(theme.muted)
                            .child("This removes it from Arcane Launcher's instance list."),
                    )
                    .child(
                        // The checkbox label row — the second sentence
                        // explains the disabled state for imported
                        // instances.
                        div()
                            .mt(px(16.0)) // mt-4
                            .flex()
                            .items_start()
                            .gap(px(8.0)) // gap-2
                            .text_size(px(12.0)) // text-xs
                            .text_color(theme.muted)
                            .child(checkbox)
                            .child(
                                div()
                                    .flex_1()
                                    .child(match imported {
                                        false => {
                                            "Also delete its files from disk (mods, worlds, config — everything)."
                                                .to_string()
                                        }
                                        true => "Also delete its files from disk (mods, worlds, config — everything). Not available for imported instances — you'll need to delete that folder yourself."
                                            .to_string(),
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .mt(px(24.0)) // mt-6
                            .flex()
                            .justify_end()
                            .gap(px(8.0)) // gap-2
                            .child(
                                ui::button(
                                    "delete-cancel",
                                    theme,
                                    ui::ButtonVariant::Ghost,
                                    ui::ButtonSize::Sm,
                                    self.busy,
                                )
                                .on_click(cx.listener(|_, _, _, cx| modals::close_delete(cx)))
                                .child("Cancel"),
                            )
                            .child(
                                ui::button(
                                    "delete-confirm",
                                    theme,
                                    ui::ButtonVariant::Danger,
                                    ui::ButtonSize::Sm,
                                    self.busy,
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.confirm(cx)))
                                .child(if self.busy { "Removing..." } else { "Remove" }),
                            ),
                    ),
            )
            .into_any_element()
    }
}

pub struct DuplicateInstanceDialog {
    spec: modals::DuplicateSpec,
    /// The name prompt, prefilled `"{name} (copy)"`.
    name: Entity<TextField>,
    busy: bool,
    error: Option<String>,
    /// The React input's `autoFocus` — focused on the first paint.
    focused: bool,
}

impl DuplicateInstanceDialog {
    pub fn new(spec: &modals::DuplicateSpec, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| {
            let mut field = TextField::new(cx, "");
            field.set_text(format!("{} (copy)", spec.name));
            field
        });
        cx.observe(&name, |_, _, cx| cx.notify()).detach();
        Self {
            spec: spec.clone(),
            name,
            busy: false,
            error: None,
            focused: false,
        }
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let new_name = self.name.read(cx).value().trim().to_string();
        if new_name.is_empty() {
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let id = self.spec.instance_id.clone();
        let task = {
            let new_name = new_name.clone();
            bridge::command(cx, move |state| async move {
                commands::instances::duplicate_instance(&state, id, new_name).await
            })
        };
        let label = new_name;
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|e| {
                Err(AppError::new(
                    "Couldn't duplicate this instance.",
                    e.to_string(),
                ))
            });
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(_) => {
                        toasts::toast_success(cx, format!("Created \"{label}\"."));
                        modals::close_duplicate(cx);
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

impl gpui::Render for DuplicateInstanceDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *Theme::get(cx);
        let name = self.name.read(cx).value().to_string();

        if !self.focused {
            // `autoFocus` — grab focus on the first paint.
            self.focused = true;
            self.name
                .update(cx, |field, _| window.focus(&field.focus.clone()));
        }

        div()
            .id("duplicate-instance-backdrop")
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
                    .w(px(384.0)) // max-w-sm
                    .flex()
                    .flex_col()
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
                            .child(format!("Duplicate {}", self.spec.name)),
                    )
                    .child(
                        div()
                            .mt(px(4.0)) // mt-1
                            .text_size(px(12.0)) // text-xs
                            .text_color(theme.muted)
                            .child(
                                "Copies its mods, config, saves, and resource packs into a new instance. Shared libraries aren't re-downloaded, so this is fast.",
                            ),
                    )
                    .child(
                        div().mt(px(16.0)).w_full().child(
                            self.name.update(cx, |field, cx| {
                                field.render_styled(
                                    window,
                                    cx,
                                    &FieldStyle {
                                        id: "duplicate-name-input",
                                        key_context: "DuplicateInput",
                                        height: 36.0,
                                        rounded: 8.0,
                                        text_size: 14.0,
                                        padding_x: 12.0,
                                        transparent: false,
                                    },
                                )
                            }),
                        ),
                    )
                    .children(self.error.clone().map(|message| {
                        div()
                            .mt(px(8.0)) // mt-2
                            .text_size(px(12.0)) // text-xs
                            .text_color(theme.danger)
                            .child(message)
                    }))
                    .child(
                        div()
                            .mt(px(24.0)) // mt-6
                            .flex()
                            .justify_end()
                            .gap(px(8.0)) // gap-2
                            .child(
                                ui::button(
                                    "duplicate-cancel",
                                    theme,
                                    ui::ButtonVariant::Ghost,
                                    ui::ButtonSize::Sm,
                                    self.busy,
                                )
                                .on_click(cx.listener(|_, _, _, cx| {
                                    modals::close_duplicate(cx)
                                }))
                                .child("Cancel"),
                            )
                            .child(
                                ui::button(
                                    "duplicate-confirm",
                                    theme,
                                    ui::ButtonVariant::Primary,
                                    ui::ButtonSize::Sm,
                                    self.busy || name.trim().is_empty(),
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.confirm(cx)))
                                .child(if self.busy {
                                    "Duplicating..."
                                } else {
                                    "Duplicate"
                                }),
                            ),
                    ),
            )
            .into_any_element()
    }
}
