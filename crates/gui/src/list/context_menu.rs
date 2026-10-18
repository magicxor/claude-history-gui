//! The conversation row context menu (DESIGN §5.2): `NativeMenu` at the
//! clicked point or at the selected row's recorded position (Shift+F10, Menu
//! key). Every item dispatches `RowMenu { item, token }` with the target
//! captured when the menu was built (A1). WP1.
//!
//! The menu is first built as a plain model ([`RowMenuModel`]: labels, live
//! shortcut labels, disabled state) and then converted into the native menu.
//! Tests and recording runs (QA) never show a native menu: it would open a
//! real OS popup.

use std::path::PathBuf;

use claude_history_core::export::ExportFormat;
use gpui_kit::component::native_menu::NativeMenu;
use gpui_kit::{App, Pixels, Point, SharedString, Window};

use crate::actions::RowMenu;
use crate::keymap::binding_labels;
use crate::model::{ConversationTarget, RowMenuItem};
use crate::services::services;
use crate::workspace::routing::row_menu_action;

/// The context stack the shortcut labels are looked up in.
const MENU_STACK: &str = "Workspace ConversationList";

/// One entry of the menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuEntry {
    Item {
        label: SharedString,
        /// The live shortcut (`Ctrl+R`), when the action is bound.
        shortcut: Option<SharedString>,
        item: RowMenuItem,
        disabled: bool,
    },
    Submenu {
        label: SharedString,
        entries: Vec<MenuEntry>,
    },
    Separator,
}

impl MenuEntry {
    /// The label of an item or submenu.
    pub fn label(&self) -> Option<&str> {
        match self {
            MenuEntry::Item { label, .. } | MenuEntry::Submenu { label, .. } => Some(label),
            MenuEntry::Separator => None,
        }
    }
}

/// A built row menu: the token under which the target was captured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowMenuModel {
    pub token: u64,
    /// The captured target's path.
    pub path: PathBuf,
    pub entries: Vec<MenuEntry>,
}

impl RowMenuModel {
    /// The labels of the top-level items and submenus, in order.
    pub fn labels(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter_map(|entry| entry.label().map(str::to_string))
            .collect()
    }

    /// The item with `label` (top level).
    pub fn item(&self, label: &str) -> Option<&MenuEntry> {
        self.entries
            .iter()
            .find(|entry| entry.label() == Some(label))
    }

    /// The shortcut of the top-level item `label`.
    pub fn shortcut(&self, label: &str) -> Option<&str> {
        match self.item(label)? {
            MenuEntry::Item { shortcut, .. } => shortcut.as_deref(),
            _ => None,
        }
    }

    /// Whether the top-level item `label` is disabled.
    pub fn is_disabled(&self, label: &str) -> bool {
        matches!(
            self.item(label),
            Some(MenuEntry::Item { disabled: true, .. })
        )
    }
}

/// The menu for `target` (§5.2). Resume, Fork, Rename and Delete need a known
/// source and are disabled while the initial load runs (AD6).
pub fn build(token: u64, target: &ConversationTarget, loading: bool, cx: &App) -> RowMenuModel {
    let modify_disabled = loading || !target.can_modify();
    let item = |label: &'static str, item: RowMenuItem, disabled: bool| {
        let action = row_menu_action(item);
        let shortcut = binding_labels(action.as_ref(), MENU_STACK, cx)
            .into_iter()
            .next();
        MenuEntry::Item {
            label: label.into(),
            shortcut,
            item,
            disabled,
        }
    };
    let mut entries = vec![
        item("Open", RowMenuItem::Open, false),
        item("Resume", RowMenuItem::Resume, modify_disabled),
        item("Fork", RowMenuItem::Fork, modify_disabled),
        item("Rename…", RowMenuItem::Rename, modify_disabled),
        item("Export…", RowMenuItem::Export, false),
        MenuEntry::Submenu {
            label: "Copy to clipboard".into(),
            entries: vec![
                item(
                    "Ledger (formatted)",
                    RowMenuItem::CopyAs(ExportFormat::Ledger),
                    false,
                ),
                item(
                    "Plain text",
                    RowMenuItem::CopyAs(ExportFormat::Plain),
                    false,
                ),
                item(
                    "Markdown",
                    RowMenuItem::CopyAs(ExportFormat::Markdown),
                    false,
                ),
                item(
                    "JSONL (raw)",
                    RowMenuItem::CopyAs(ExportFormat::Jsonl),
                    false,
                ),
            ],
        },
        item("Copy path", RowMenuItem::CopyPath, false),
        item("Copy session ID", RowMenuItem::CopySessionId, false),
    ];
    if target.header_id_differs() {
        entries.push(item(
            "Copy header session ID",
            RowMenuItem::CopyHeaderSessionId,
            false,
        ));
    }
    entries.push(item("Reveal in file manager", RowMenuItem::Reveal, false));
    entries.push(MenuEntry::Separator);
    entries.push(item(
        "Search within conversation…",
        RowMenuItem::SearchWithin,
        false,
    ));
    entries.push(item("Outline", RowMenuItem::Outline, false));
    entries.push(MenuEntry::Separator);
    entries.push(item("Delete…", RowMenuItem::Delete, modify_disabled));
    RowMenuModel {
        token,
        path: target.path.clone(),
        entries,
    }
}

/// The text of a native menu item: Windows right-aligns text after a tab as
/// the accelerator; other platforms get it in parentheses.
fn native_label(label: &str, shortcut: Option<&str>) -> SharedString {
    match shortcut {
        Some(shortcut) if cfg!(windows) => format!("{label}\t{shortcut}").into(),
        Some(shortcut) => format!("{label}  ({shortcut})").into(),
        None => label.to_string().into(),
    }
}

fn native_menu(entries: &[MenuEntry], token: u64) -> NativeMenu {
    let mut menu = NativeMenu::new();
    for entry in entries {
        menu = match entry {
            MenuEntry::Separator => menu.separator(),
            MenuEntry::Item {
                label,
                shortcut,
                item,
                disabled,
            } => menu.menu_with_disabled(
                native_label(label, shortcut.as_deref()),
                *disabled,
                Box::new(RowMenu { item: *item, token }),
            ),
            MenuEntry::Submenu { label, entries } => {
                menu.submenu(label.clone(), native_menu(entries, token))
            }
        };
    }
    menu
}

/// Pop the menu up at `at` (window coordinates). Not in tests or recording
/// runs: a native menu would be a real OS popup.
pub fn show(model: &RowMenuModel, at: Point<Pixels>, window: &mut Window, cx: &mut App) {
    if cfg!(test) || services(cx).is_recording() {
        return;
    }
    native_menu(&model.entries, model.token).show(at, window, cx);
}
