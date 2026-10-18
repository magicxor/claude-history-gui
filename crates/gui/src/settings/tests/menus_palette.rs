//! G tests of the menu tree and the command palette (DESIGN §8.9, §8.10):
//! C-055, C-057, X-025, and the check marks that follow the viewer.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::{GlobalState, WindowExt as _};
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, Menu, MenuItem, OwnedMenu, OwnedMenuItem, Render, Styled as _, TestAppContext,
    Window, WindowOptions, div,
};

use crate::actions::*;
use crate::globals::app_state;
use crate::list::ListPaneEvent;
use crate::menus;
use crate::model::ConversationTarget;
use crate::palette;
use crate::test_support::{TestWorld, activate, open_main, open_single_file, synth};

fn render(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    use gpui_kit::test::TestWindowExt as _;
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
}

fn has_dialog(cx: &mut TestAppContext, handle: AnyWindowHandle) -> bool {
    cx.update_window(handle, |_, window, cx| window.has_active_dialog(cx))
        .unwrap()
}

/// Every action leaf of a menu: (menu path, name, checked).
fn leaves(menu: &Menu, path: &str, out: &mut Vec<(String, String, bool)>) {
    for item in &menu.items {
        match item {
            MenuItem::Action { name, checked, .. } => {
                out.push((path.to_string(), name.to_string(), *checked))
            }
            MenuItem::Submenu(sub) => leaves(sub, &format!("{path} › {}", sub.name), out),
            _ => {}
        }
    }
}

fn tree_leaves(tree: &[Menu]) -> Vec<(String, String, bool)> {
    let mut out = Vec::new();
    for menu in tree {
        leaves(menu, menu.name.as_ref(), &mut out);
    }
    out
}

/// The same for the installed (owned) tree the in-window menu bar reads.
fn owned_leaves(menu: &OwnedMenu, path: &str, out: &mut Vec<(String, String, bool)>) {
    for item in &menu.items {
        match item {
            OwnedMenuItem::Action { name, checked, .. } => {
                out.push((path.to_string(), name.clone(), *checked))
            }
            OwnedMenuItem::Submenu(sub) => {
                owned_leaves(sub, &format!("{path} › {}", sub.name), out)
            }
            _ => {}
        }
    }
}

fn installed(cx: &mut TestAppContext) -> Vec<(String, String, bool)> {
    cx.read(|cx| {
        let mut out = Vec::new();
        for menu in GlobalState::global(cx).app_menus() {
            owned_leaves(menu, menu.name.as_ref(), &mut out);
        }
        out
    })
}

fn checked(leaves: &[(String, String, bool)], path: &str, name: &str) -> bool {
    leaves
        .iter()
        .find(|(p, n, _)| p == path && n == name)
        .unwrap_or_else(|| panic!("menu item {path} / {name}"))
        .2
}

/// C-055: the tree has File/Edit/View/Session/Tools/Help, and after
/// SetThemeDark the installed Theme ▸ check moves to Dark.
#[gpui_kit::test]
fn the_tree_has_six_menus_and_the_theme_check_follows_the_choice(cx: &mut TestAppContext) {
    let _guard = crate::test_support::global_lock();
    let world = TestWorld::new();
    let (handle, _ws) = open_main(cx, &world, &[]);
    let names: Vec<String> = cx.read(|cx| {
        GlobalState::global(cx)
            .app_menus()
            .iter()
            .map(|menu| menu.name.to_string())
            .collect()
    });
    assert_eq!(
        names,
        ["File", "Edit", "View", "Session", "Tools", "Help"],
        "one tree for every window"
    );
    let leaves = installed(cx);
    assert!(checked(&leaves, "View › Theme", "System"));
    assert!(!checked(&leaves, "View › Theme", "Dark"));
    cx.dispatch_action(handle, SetThemeDark);
    cx.run_until_parked();
    let leaves = installed(cx);
    assert!(checked(&leaves, "View › Theme", "Dark"), "{leaves:#?}");
    assert!(!checked(&leaves, "View › Theme", "System"));
    cx.dispatch_action(handle, SetThemeLight);
    cx.run_until_parked();
    assert!(checked(&installed(cx), "View › Theme", "Light"));
    cx.dispatch_action(handle, SetThemeSystem);
    cx.run_until_parked();
}

fn session_path(world: &TestWorld) -> std::path::PathBuf {
    world.add_claude_session(
        "-synthetic-proj",
        &synth::uuid(5),
        &[
            r#"{"type":"user","sessionId":"x","timestamp":"2026-09-01T10:00:00Z","message":{"role":"user","content":"hello menus"}}"#,
            r#"{"type":"assistant","sessionId":"x","timestamp":"2026-09-01T10:00:05Z","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}"#,
        ],
    )
}

fn open_in_viewer(
    cx: &mut TestAppContext,
    ws: &Entity<crate::workspace::Workspace>,
    path: &std::path::Path,
) {
    let list = cx.read(|cx| ws.read(cx).list().clone());
    let target = ConversationTarget::from_parsed_file(path, None);
    list.update(cx, |_, cx| {
        cx.emit(ListPaneEvent::OpenRequested {
            target,
            focus_viewer: true,
            agent_tab: None,
        })
    });
    cx.run_until_parked();
}

/// The View menu's check marks follow the open conversation's toggles
/// without any cooperation of the viewer (it just notifies).
#[gpui_kit::test]
fn view_checks_follow_the_viewer_and_the_collapsed_state(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = session_path(&world);
    let (handle, ws) = open_main(cx, &world, &[]);
    let leaves = installed(cx);
    assert!(
        !leaves
            .iter()
            .any(|(path, _, on)| path == "View › Tool display" && *on),
        "no conversation open: no tool display checked"
    );
    open_in_viewer(cx, &ws, &path);
    render(cx, handle);
    let leaves = installed(cx);
    assert!(checked(&leaves, "View › Tool display", "Summary"));
    assert!(!checked(&leaves, "View", "Show thinking"));
    cx.dispatch_action(handle, ToggleThinking);
    cx.dispatch_action(handle, ToggleTiming);
    cx.dispatch_action(handle, SetToolsFull);
    cx.run_until_parked();
    let leaves = installed(cx);
    assert!(checked(&leaves, "View", "Show thinking"), "{leaves:#?}");
    assert!(checked(&leaves, "View", "Show timestamps"));
    assert!(checked(&leaves, "View › Tool display", "Full"));
    assert!(!checked(&leaves, "View › Tool display", "Summary"));
    // Focus viewer mode.
    assert!(!checked(&leaves, "View", "Focus viewer"));
    cx.dispatch_action(handle, ToggleListPane);
    cx.run_until_parked();
    assert!(checked(&installed(cx), "View", "Focus viewer"));
    assert!(cx.read(|cx| app_state(cx).ui.list_collapsed));
    cx.dispatch_action(handle, ToggleListPane);
    cx.run_until_parked();
    assert!(!checked(&installed(cx), "View", "Focus viewer"));
}

/// The tree builder maps every check input to its menu item (pure).
#[test]
fn the_tree_marks_each_state() {
    use claude_history_core::theme::ThemePreference;
    let tree = menus::build_tree(&menus::MenuChecks {
        theme: ThemePreference::Dark,
        list_collapsed: true,
        tool_display: Some(2),
        show_thinking: true,
        show_timing: true,
        semantic_mode: true,
        workspace_scope: true,
        picker: false,
        header_session_id: false,
    });
    let leaves = tree_leaves(&tree);
    for (path, name) in [
        ("View › Theme", "Dark"),
        ("View", "Focus viewer"),
        ("View › Tool display", "Full"),
        ("View", "Show thinking"),
        ("View", "Show timestamps"),
        ("View › Search mode", "Semantic"),
        ("View › Scope", "Workspace"),
    ] {
        assert!(checked(&leaves, path, name), "{path} / {name}");
    }
    for (path, name) in [
        ("View › Theme", "System"),
        ("View › Tool display", "Summary"),
        ("View › Search mode", "Lexical"),
        ("View › Scope", "All"),
    ] {
        assert!(!checked(&leaves, path, name), "{path} / {name}");
    }
}

/// C-055: in a single-file window Session → Resume (and Fork, Rename, Delete)
/// is unavailable (the palette shows them disabled), while viewer items stay
/// available; the check marks describe that window's viewer.
#[gpui_kit::test]
fn a_single_file_window_disables_the_session_items(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = session_path(&world);
    let (_main, _ws) = open_main(cx, &world, &[]);
    let handle = open_single_file(cx, &world, &path);
    render(cx, handle);
    let groups = cx
        .update_window(handle, |_, window, cx| {
            palette::palette_groups_for(None, window, cx)
        })
        .unwrap();
    let find = |group: &str, label: &str| {
        let group = groups
            .iter()
            .find(|g| g.label.as_ref() == group)
            .unwrap_or_else(|| panic!("group {group}"));
        group
            .entries
            .iter()
            .find(|e| e.label.as_ref() == label)
            .unwrap_or_else(|| panic!("entry {label}"))
            .disabled
    };
    for label in ["Resume", "Fork", "Rename…", "Delete…"] {
        assert!(find("Session", label), "{label} is unavailable here");
    }
    assert!(!find("Session", "Outline"), "viewer items stay available");
    assert!(!find("View", "Show thinking"));
    assert!(!find("Help", "Command palette…"));
    // The single-file viewer's own toggles drive the checks.
    cx.dispatch_action(handle, ToggleThinking);
    cx.run_until_parked();
    assert!(checked(&installed(cx), "View", "Show thinking"));
}

/// A key-context probe window: handles ExportConversation and Resume only.
struct Probe {
    focus: FocusHandle,
    seen: Rc<RefCell<Vec<&'static str>>>,
}

impl Render for Probe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let seen = self.seen.clone();
        let seen2 = self.seen.clone();
        div()
            .id("probe")
            .key_context("Probe")
            .track_focus(&self.focus)
            .size_full()
            .on_action(
                cx.listener(move |_, _: &ExportConversation, _, _| {
                    seen.borrow_mut().push("export")
                }),
            )
            .on_action(cx.listener(move |_, _: &Resume, _, _| seen2.borrow_mut().push("resume")))
    }
}

fn open_probe(
    cx: &mut TestAppContext,
) -> (
    AnyWindowHandle,
    Entity<Probe>,
    Rc<RefCell<Vec<&'static str>>>,
) {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let result = {
        let seen = seen.clone();
        cx.update(move |cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, move |_, cx| {
                cx.new(|cx| Probe {
                    focus: cx.focus_handle(),
                    seen,
                })
            })
            .expect("open the probe window")
        })
    };
    let (handle, probe) = result;
    activate(cx, handle);
    let focus = cx.read(|cx| probe.read(cx).focus.clone());
    cx.update_window(handle, |_, window, cx| focus.focus(window, cx))
        .unwrap();
    render(cx, handle);
    (handle, probe, seen)
}

/// C-057: the palette lists every menu leaf (with its live shortcut when the
/// stored focus has one), disables items nothing handles at the stored focus,
/// filters by typing, and dispatches the confirmed item to the element focused
/// before it opened, restoring that focus.
#[gpui_kit::test]
fn the_palette_lists_every_leaf_and_dispatches_to_the_stored_focus(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_main, _ws) = open_main(cx, &world, &[]);
    let (handle, probe, seen) = open_probe(cx);
    let focus = cx.read(|cx| probe.read(cx).focus.clone());

    // The model: one entry per leaf of the palette tree, same order.
    let (groups, leaves_expected) = cx
        .update_window(handle, |_, window, cx| {
            (
                palette::palette_groups_for(Some(&focus), window, cx),
                tree_leaves(&menus::palette_tree(cx)),
            )
        })
        .unwrap();
    let listed: Vec<(String, String)> = groups
        .iter()
        .flat_map(|group| {
            group
                .entries
                .iter()
                .map(|entry| (entry.path.to_string(), entry.label.to_string()))
        })
        .collect();
    let expected: Vec<(String, String)> = leaves_expected
        .iter()
        .map(|(path, name, _)| (path.clone(), name.clone()))
        .collect();
    assert_eq!(listed, expected, "one palette item per menu leaf");
    let entry = |group: &str, label: &str| {
        groups
            .iter()
            .find(|g| g.label.as_ref() == group)
            .and_then(|g| g.entries.iter().find(|e| e.label.as_ref() == label))
            .unwrap_or_else(|| panic!("{group} / {label}"))
    };
    // Handled by the probe: enabled; everything else is disabled there.
    assert!(!entry("Session", "Resume").disabled);
    assert!(!entry("Session", "Export…").disabled);
    assert!(entry("Session", "Fork").disabled);
    assert!(entry("View", "Show thinking").disabled);
    // Live shortcuts: Resume has none at the probe (list keys are not on its
    // path); app-wide shortcuts are context-free or `Workspace`-scoped.
    assert_eq!(
        entry("File", "Quit").shortcut_label().as_deref(),
        Some("Ctrl+Q")
    );
    assert_eq!(entry("Session", "Resume").shortcut, None);

    // Open it for real from the probe window.
    cx.update_window(handle, |_, window, cx| palette::open(window, cx))
        .unwrap();
    cx.run_until_parked();
    render(cx, handle);
    assert!(has_dialog(cx, handle), "the palette is open");
    cx.simulate_input(handle, "exp");
    cx.run_until_parked();
    render(cx, handle);
    cx.simulate_keystrokes(handle, "enter");
    cx.run_until_parked();
    assert!(!has_dialog(cx, handle), "confirming closes the palette");
    assert_eq!(
        seen.borrow().as_slice(),
        ["export"],
        "dispatched to the element focused before the palette opened"
    );
    let focused = cx
        .update_window(handle, |_, window, _| focus.is_focused(window))
        .unwrap();
    assert!(focused, "the stored focus is restored");

    // A disabled item does nothing.
    seen.borrow_mut().clear();
    cx.update_window(handle, |_, window, cx| palette::open(window, cx))
        .unwrap();
    cx.run_until_parked();
    render(cx, handle);
    cx.simulate_input(handle, "fork");
    cx.run_until_parked();
    render(cx, handle);
    cx.simulate_keystrokes(handle, "enter");
    cx.run_until_parked();
    assert!(seen.borrow().is_empty());
    cx.simulate_keystrokes(handle, "escape");
    cx.run_until_parked();
}

/// X-025: the palette contains one item per menu leaf in the main window, every
/// Tools item is reachable from it, and F10 opens it.
#[gpui_kit::test]
fn every_tools_item_is_in_the_palette_and_f10_opens_it(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (handle, _ws) = open_main(cx, &world, &[]);
    let (groups, tree) = cx
        .update_window(handle, |_, window, cx| {
            (
                palette::palette_groups_for(None, window, cx),
                menus::palette_tree(cx),
            )
        })
        .unwrap();
    let tools = groups
        .iter()
        .find(|group| group.label.as_ref() == "Tools")
        .expect("Tools group");
    let tools_names: Vec<String> = tools.entries.iter().map(|e| e.label.to_string()).collect();
    for expected in [
        "Delete empty transcripts…",
        "Generate semantic cache…",
        "Clear semantic cache…",
        "Debug search…",
        "Semantic debug search…",
        "Debug log…",
        "Show projects directory",
        "Delete conversation by session ID…",
        "Open JSONL file…",
        "Check for updates…",
        "Settings…",
    ] {
        assert!(tools_names.contains(&expected.to_string()), "{expected}");
    }
    let leaf_count = tree_leaves(&tree).len();
    assert_eq!(
        groups.iter().map(|g| g.entries.len()).sum::<usize>(),
        leaf_count
    );
    cx.simulate_keystrokes(handle, "f10");
    cx.run_until_parked();
    render(cx, handle);
    assert!(has_dialog(cx, handle), "F10 opens the palette");
    cx.simulate_keystrokes(handle, "escape");
    cx.run_until_parked();
    assert!(!has_dialog(cx, handle));
    // The shortcut opens it too.
    cx.simulate_keystrokes(handle, "secondary-shift-p");
    cx.run_until_parked();
    assert!(has_dialog(cx, handle));
}

/// The palette lists Settings… and Quit on every platform (PD12).
#[gpui_kit::test]
fn settings_and_quit_are_in_the_palette(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    let labels: Vec<String> = cx.read(|cx| {
        palette::palette_groups(cx)
            .iter()
            .flat_map(|group| group.entries.iter().map(|e| e.label.to_string()))
            .collect()
    });
    assert!(labels.contains(&"Settings…".to_string()));
    assert!(labels.contains(&"Quit".to_string()));
}

/// Every action leaf of the installed tree: (name, disabled).
fn installed_disabled(cx: &mut TestAppContext) -> Vec<(String, bool)> {
    fn walk(items: &[OwnedMenuItem], out: &mut Vec<(String, bool)>) {
        for item in items {
            match item {
                OwnedMenuItem::Action { name, disabled, .. } => out.push((name.clone(), *disabled)),
                OwnedMenuItem::Submenu(sub) => walk(&sub.items, out),
                _ => {}
            }
        }
    }
    cx.read(|cx| {
        let mut out = Vec::new();
        for menu in GlobalState::global(cx).app_menus() {
            walk(&menu.items, &mut out);
        }
        out
    })
}

/// S-033 / V-090: the in-window menu bar (gpui-component's PopupMenu greys an
/// item only by its `disabled` flag) disables the commands the active window
/// cannot handle: in a single-file window Resume, Fork, Rename, Delete and
/// the search modes; in the main window Close window.
#[cfg(not(target_os = "macos"))]
#[gpui_kit::test]
fn the_in_window_menu_bar_disables_unavailable_items(cx: &mut TestAppContext) {
    let _guard = crate::test_support::global_lock();
    let world = TestWorld::new();
    let path = session_path(&world);
    let (main, _ws) = open_main(cx, &world, &[]);
    activate(cx, main);
    render(cx, main);
    cx.run_until_parked();
    let flags = installed_disabled(cx);
    let off = |name: &str, flags: &[(String, bool)]| {
        flags
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, d)| *d)
            .collect::<Vec<_>>()
    };
    assert_eq!(off("Close window", &flags), [true], "no handler in main");
    assert_eq!(off("Lexical", &flags), [false], "the list handles modes");

    let single = open_single_file(cx, &world, &path);
    activate(cx, single);
    render(cx, single);
    cx.run_until_parked();
    let flags = installed_disabled(cx);
    for name in [
        "Resume",
        "Fork",
        "Rename…",
        "Delete…",
        "Lexical",
        "Semantic",
    ] {
        assert_eq!(off(name, &flags), [true], "{name} is greyed out");
    }
    assert_eq!(off("Close window", &flags), [false]);
    assert_eq!(off("Outline", &flags), [false]);
    assert_eq!(off("Keyboard shortcuts", &flags), [false]);
}
