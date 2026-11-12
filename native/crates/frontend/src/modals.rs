//! Window-level modal state — the gpui equivalent of the React dialogs'
//! `createPortal(..., document.body)`: because gpui has no
//! `position: fixed`, every dialog renders off the shell root instead, and
//! this global carries which one is open plus the data it needs.
//!
//! The launch dialogs are owned by their originating `LaunchButton`
//! (`launch-button.tsx` kept the flow state per button); the specs hold a
//! weak handle so a dialog can hand clicks back to it after the portal
//! hop.

use gpui::{App, BorrowAppContext, Global, WeakEntity};

use crate::launch::LaunchButton;

/// `SmartLaunchDialog` — opened on every Play click.
#[derive(Clone)]
pub struct SmartLaunchSpec {
    pub instance_id: String,
    pub owner: WeakEntity<LaunchButton>,
}

/// `LaunchValidationDialog` — opened when an actual launch attempt fails
/// and the instance doesn't validate clean.
#[derive(Clone)]
pub struct ValidationSpec {
    pub instance_id: String,
    pub name: String,
    pub report: backend::models::instance::InstanceValidationReport,
    pub owner: WeakEntity<LaunchButton>,
}

/// `InstanceConsole` — opened manually and automatically on launch start.
#[derive(Clone)]
pub struct ConsoleSpec {
    pub instance_id: String,
    pub name: String,
}

/// `DeleteInstanceDialog` — the remove confirmation (the `delete_files`
/// checkbox is disabled for imported instances).
#[derive(Clone)]
pub struct DeleteSpec {
    pub instance_id: String,
    pub name: String,
    pub imported: bool,
}

/// `DuplicateInstanceDialog` — the name prompt (prefilled with
/// "`name` (copy)").
#[derive(Clone)]
pub struct DuplicateSpec {
    pub instance_id: String,
    pub name: String,
}

#[derive(Default)]
pub struct ModalStore {
    pub smart_launch: Option<SmartLaunchSpec>,
    pub validation: Option<ValidationSpec>,
    pub console: Option<ConsoleSpec>,
    pub delete: Option<DeleteSpec>,
    pub duplicate: Option<DuplicateSpec>,
}

impl Global for ModalStore {}

impl ModalStore {
    /// Nothing is open — the pages use this to know when to refresh their
    /// stat queries (the React app invalidates its react-query cache
    /// after fixes and launches; modal dismissal is where those moments
    /// land on the native side).
    pub fn all_closed(&self) -> bool {
        self.smart_launch.is_none()
            && self.validation.is_none()
            && self.console.is_none()
            && self.delete.is_none()
            && self.duplicate.is_none()
    }
}

pub fn open_smart_launch(cx: &mut App, instance_id: String, owner: WeakEntity<LaunchButton>) {
    cx.update_global(|store: &mut ModalStore, _| {
        store.smart_launch = Some(SmartLaunchSpec { instance_id, owner });
    });
}

pub fn close_smart_launch(cx: &mut App) {
    cx.update_global(|store: &mut ModalStore, _| store.smart_launch = None);
}

pub fn open_validation(
    cx: &mut App,
    instance_id: String,
    name: String,
    report: backend::models::instance::InstanceValidationReport,
    owner: WeakEntity<LaunchButton>,
) {
    cx.update_global(|store: &mut ModalStore, _| {
        store.validation = Some(ValidationSpec {
            instance_id,
            name,
            report,
            owner,
        });
    });
}

/// Replaces the report the dialog shows (the "Fix automatically" result).
pub fn update_validation_report(
    cx: &mut App,
    instance_id: &str,
    report: backend::models::instance::InstanceValidationReport,
) {
    cx.update_global(|store: &mut ModalStore, _| {
        if let Some(spec) = &mut store.validation
            && spec.instance_id == instance_id
        {
            spec.report = report;
        }
    });
}

pub fn close_validation(cx: &mut App) {
    cx.update_global(|store: &mut ModalStore, _| store.validation = None);
}

pub fn open_console(cx: &mut App, instance_id: String, name: String) {
    cx.update_global(|store: &mut ModalStore, _| {
        store.console = Some(ConsoleSpec { instance_id, name });
    });
}

pub fn close_console(cx: &mut App) {
    cx.update_global(|store: &mut ModalStore, _| store.console = None);
}

pub fn open_delete(cx: &mut App, instance_id: String, name: String, imported: bool) {
    cx.update_global(|store: &mut ModalStore, _| {
        store.delete = Some(DeleteSpec {
            instance_id,
            name,
            imported,
        });
    });
}

pub fn close_delete(cx: &mut App) {
    cx.update_global(|store: &mut ModalStore, _| store.delete = None);
}

pub fn open_duplicate(cx: &mut App, instance_id: String, name: String) {
    cx.update_global(|store: &mut ModalStore, _| {
        store.duplicate = Some(DuplicateSpec { instance_id, name });
    });
}

pub fn close_duplicate(cx: &mut App) {
    cx.update_global(|store: &mut ModalStore, _| store.duplicate = None);
}
