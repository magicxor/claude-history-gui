//! The command palette (`secondary-shift-p`, `f10`; DESIGN §8.9). WP0 → WP6.
//!
//! Built from [`menus::palette_tree`] on every open: groups = top-level menus,
//! items = every action leaf with its live shortcut and the menu path as
//! keywords. The palette stores the element that had focus when it opened;
//! confirming closes the palette, restores that focus and dispatches the
//! action there, so the focused pane resolves its own target (A1).
//! WP6 adds the disabled state (`is_action_available_in` the stored focus)
//! and keeps check marks in sync.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IndexPath, WindowExt as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Action, App, AppContext as _, FocusHandle, Focusable as _, Global, Keystroke, Menu, MenuItem,
    ParentElement as _, SharedString, Styled as _, Window, div, rems,
};

use crate::menus;

/// One command of the palette.
pub struct PaletteEntry {
    pub label: SharedString,
    /// The menu path (`View › Theme`), searched as keywords.
    pub path: SharedString,
    pub checked: bool,
    /// No handler for the action on the dispatch path of the focus the
    /// palette returns to (§8.9): shown muted, never dispatched.
    pub disabled: bool,
    /// The live shortcut of the action for the focus the palette returns to
    /// (`None`: unbound there).
    pub shortcut: Option<Keystroke>,
    pub action: Box<dyn Action>,
}

impl PaletteEntry {
    /// The shortcut as displayed (`Ctrl+R`, `F2`).
    pub fn shortcut_label(&self) -> Option<String> {
        self.shortcut.as_ref().map(Kbd::format)
    }
}

/// A group of the palette (one top-level menu).
pub struct PaletteGroup {
    pub label: SharedString,
    pub entries: Vec<PaletteEntry>,
}

/// The focus to return to when the palette closes.
#[derive(Clone, Default)]
pub struct PaletteReturnFocus(pub Option<FocusHandle>);

impl Global for PaletteReturnFocus {}

/// Register the palette's globals (§7.3 step 8). Also starts the UI state
/// persistence (`persist::install`): this is the last WP6 install step the
/// app's init (frozen) calls, in production and in tests.
pub fn install(cx: &mut App) {
    cx.set_global(PaletteReturnFocus::default());
    crate::persist::install(cx);
}

/// Nothing is cached across keymap/theme changes: the palette is built on
/// every open.
pub fn rebuild(_cx: &mut App) {}

fn collect(items: Vec<MenuItem>, path: &str, out: &mut Vec<PaletteEntry>) {
    for item in items {
        match item {
            MenuItem::Action {
                name,
                action,
                checked,
                ..
            } => out.push(PaletteEntry {
                label: name,
                path: path.to_string().into(),
                checked,
                disabled: false,
                shortcut: None,
                action,
            }),
            MenuItem::Submenu(menu) => {
                let sub_path = format!("{path} › {}", menu.name);
                collect(menu.items, &sub_path, out);
            }
            MenuItem::Separator | MenuItem::SystemMenu(_) => {}
        }
    }
}

/// The palette's groups from a menu tree.
pub fn groups_from_menus(tree: Vec<Menu>) -> Vec<PaletteGroup> {
    tree.into_iter()
        .map(|menu| {
            let mut entries = Vec::new();
            collect(menu.items, menu.name.as_ref(), &mut entries);
            PaletteGroup {
                label: menu.name,
                entries,
            }
        })
        .collect()
}

/// The palette's groups for the current state: every menu command, on macOS
/// including the app menu's Settings… and Quit (PD12).
pub fn palette_groups(cx: &App) -> Vec<PaletteGroup> {
    groups_from_menus(menus::palette_tree(cx))
}

/// [`palette_groups`] with the disabled state evaluated against the dispatch
/// path of `focus` (the element the palette returns to; the window's current
/// focus when `None`): an entry whose action no element on that path handles
/// is disabled (§8.9).
pub fn palette_groups_for(
    focus: Option<&FocusHandle>,
    window: &Window,
    cx: &App,
) -> Vec<PaletteGroup> {
    let mut groups = palette_groups(cx);
    for entry in groups.iter_mut().flat_map(|group| group.entries.iter_mut()) {
        entry.disabled = !match focus {
            Some(focus) => window.is_action_available_in(&*entry.action, focus),
            None => window.is_action_available(&*entry.action, cx),
        };
        let binding = match focus {
            Some(focus) => window.highest_precedence_binding_for_action_in(&*entry.action, focus),
            None => window.highest_precedence_binding_for_action(&*entry.action),
        };
        entry.shortcut =
            binding.and_then(|binding| binding.keystrokes().first().map(|k| k.inner().clone()));
    }
    groups
}

/// The open palette's state (weak: gone once the dialog closed).
#[derive(Default)]
struct OpenPalette(Option<gpui_kit::WeakEntity<CommandState>>);

impl Global for OpenPalette {}

/// Whether a palette dialog is open.
pub fn is_open(cx: &App) -> bool {
    cx.try_global::<OpenPalette>()
        .and_then(|open| open.0.as_ref())
        .is_some_and(|state| state.upgrade().is_some())
}

/// Open the palette in `window`.
pub fn open(window: &mut Window, cx: &mut App) {
    let return_focus = window.focused(cx);
    cx.set_global(PaletteReturnFocus(return_focus.clone()));
    let groups = Rc::new(palette_groups_for(return_focus.as_ref(), window, cx));
    let state = cx.new(|cx| CommandState::new(window, cx));
    cx.set_global(OpenPalette(Some(state.downgrade())));
    let first_render = Rc::new(Cell::new(true));
    window.open_dialog(cx, move |dialog, window, cx| {
        if first_render.replace(false) {
            let focus = state.read(cx).focus_handle(cx);
            focus.focus(window, cx);
        }
        let muted = cx.theme().muted_foreground;
        let mut command = Command::new(&state)
            .placeholder("Type a command…")
            .bordered(false)
            .max_h(rems(26.));
        for group in groups.iter() {
            let mut command_group = CommandGroup::new().label(group.label.clone());
            for entry in &group.entries {
                let label = entry.label.clone();
                let checked = entry.checked;
                let disabled = entry.disabled;
                let shortcut = entry.shortcut.clone();
                command_group = command_group.item(
                    CommandItem::new()
                        .label(entry.label.clone())
                        .keywords([entry.path.clone()])
                        // The check mark is drawn by the custom content below; the
                        // kit's own trailing check would double it.
                        .disabled(disabled)
                        .child(move |_window, _cx| {
                            let kbd = shortcut.clone().map(Kbd::new);
                            h_flex()
                                .w_full()
                                .gap_2()
                                .when(disabled, |this| this.text_color(muted))
                                .child(div().w(rems(1.)).when(checked, |this| this.child("✓")))
                                .child(div().flex_1().min_w_0().child(label.clone()))
                                .when_some(kbd, |this, kbd| this.child(kbd))
                        }),
                );
            }
            command = command.group(command_group);
        }
        let confirm_groups = groups.clone();
        let confirm_focus = return_focus.clone();
        dialog.w(window.rem_size() * 36.).close_button(false).child(
            command
                .on_confirm(move |index: IndexPath, window, cx| {
                    let Some(entry) = confirm_groups
                        .get(index.section)
                        .and_then(|group| group.entries.get(index.row))
                        .filter(|entry| !entry.disabled)
                    else {
                        return;
                    };
                    dispatch_to_return_focus(
                        entry.action.boxed_clone(),
                        confirm_focus.clone(),
                        window,
                        cx,
                    );
                })
                .empty(move |_, _, _| {
                    div()
                        .p_3()
                        .text_sm()
                        .text_color(muted)
                        .child("No matching commands")
                }),
        )
    });
}

/// Close the palette, restore the stored focus and dispatch `action` there.
fn dispatch_to_return_focus(
    action: Box<dyn Action>,
    focus: Option<FocusHandle>,
    window: &mut Window,
    cx: &mut App,
) {
    window.close_dialog(cx);
    if let Some(focus) = focus {
        focus.focus(window, cx);
    }
    window.dispatch_action(action, cx);
    cx.set_global(PaletteReturnFocus::default());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menus::{MenuChecks, build_tree};

    #[test]
    fn every_menu_leaf_becomes_a_palette_entry() {
        let groups = groups_from_menus(build_tree(&MenuChecks::default()));
        assert_eq!(groups.len(), 6);
        let labels: Vec<&str> = groups
            .iter()
            .flat_map(|group| group.entries.iter().map(|entry| entry.label.as_ref()))
            .collect();
        assert!(labels.contains(&"Resume"));
        assert!(labels.contains(&"Export…"));
        assert!(labels.contains(&"Dark"));
        let dark = groups
            .iter()
            .flat_map(|group| group.entries.iter())
            .find(|entry| entry.label.as_ref() == "Dark")
            .unwrap();
        assert_eq!(dark.path.as_ref(), "View › Theme");
    }
}
