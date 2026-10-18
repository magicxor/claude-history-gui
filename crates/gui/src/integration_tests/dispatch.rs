//! Dispatch paths across the panes: A-055, X-001, X-002, V-103.

use std::path::{Path, PathBuf};

use claude_history_core::history::Source;
use gpui_kit::{Action, AnyWindowHandle, AppContext as _, MenuItem, TestAppContext, point, px};

use super::{click_row, has_dialog, probe, render, settle, shown_path, viewer_of, wait_for};
use crate::globals::OpenTargets;
use crate::ops::export::LastExportMenu;
use crate::test_support::fixtures_ops::{OpsFixture, claude_session, write_gui_config};
use crate::test_support::fixtures_viewer::{draw, wait_viewer_ready};
use crate::test_support::{TestWorld, recording};

/// The action of the Session menu item `name` (from the one menu tree).
fn session_item(cx: &TestAppContext, name: &str) -> Box<dyn Action> {
    cx.read(|cx| {
        let tree = crate::menus::menu_tree(cx);
        let session = tree
            .into_iter()
            .find(|menu| menu.name.as_ref() == "Session")
            .expect("the Session menu");
        session
            .items
            .into_iter()
            .find_map(|item| match item {
                MenuItem::Action {
                    name: label,
                    action,
                    ..
                } if label.as_ref() == name => Some(action),
                _ => None,
            })
            .unwrap_or_else(|| panic!("Session › {name}"))
    })
}

/// The AppMenuBar path (gpui-kit 0.7.3 `AppMenuBar` + `PopupMenu`): opening
/// the bar remembers the focused element as the action context, the popup
/// takes the focus, and confirming an item focuses the context again and
/// dispatches the item's action there.
fn menu_bar_dispatch(cx: &mut TestAppContext, handle: AnyWindowHandle, action: Box<dyn Action>) {
    cx.update_window(handle, |_, window, cx| {
        let context = window.focused(cx);
        let popup = cx.focus_handle();
        popup.focus(window, cx);
        if let Some(context) = context {
            context.focus(window, cx);
        }
        window.dispatch_action(action, cx);
    })
    .expect("the window is open");
    cx.run_until_parked();
}

/// The palette path: open it from the current focus, type, confirm.
fn palette_dispatch(cx: &mut TestAppContext, handle: AnyWindowHandle, query: &str) {
    cx.update_window(handle, |_, window, cx| crate::palette::open(window, cx))
        .expect("the window is open");
    render(cx, handle);
    cx.simulate_input(handle, query);
    render(cx, handle);
    cx.simulate_keystrokes(handle, "enter");
    cx.run_until_parked();
}

fn export_target(cx: &TestAppContext) -> Option<PathBuf> {
    cx.read(|cx| {
        cx.try_global::<LastExportMenu>()
            .and_then(|last| last.0.upgrade())
            .filter(|menu| !menu.read(cx).is_closed())
            .map(|menu| menu.read(cx).target().path.clone())
    })
}

fn held(cx: &TestAppContext) -> Vec<(PathBuf, String)> {
    cx.read(|cx| {
        OpenTargets::entries(cx)
            .into_iter()
            .map(|(path, title)| (path, title.to_string()))
            .collect()
    })
}

fn wait_dialog(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    for _ in 0..200 {
        settle(cx);
        render(cx, handle);
        if has_dialog(cx, handle) {
            return;
        }
    }
    panic!("no dialog opened");
}

fn close_dialog(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.simulate_keystrokes(handle, "escape");
    settle(cx);
    render(cx, handle);
    assert!(!has_dialog(cx, handle));
}

fn stem(path: &Path) -> String {
    path.file_stem().unwrap().to_string_lossy().into_owned()
}

/// A-055: with the viewer focused, the Session menu's Resume, Copy path and
/// Export… record the same effects as `ctrl-r`, `Y` and `e`.
#[gpui_kit::test]
fn a055_session_menu_items_do_what_their_keys_do(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let path = claude_session(&world, "proj", 1, "Menu equals keys");
    claude_session(&world, "proj", 2, "Another conversation");
    let f = OpsFixture::open(cx, &world, &[]);
    let handle = f.handle();
    f.open_in_viewer(cx, &path);
    assert_eq!(probe(cx, handle, f.ws(), "focus"), "viewer");

    // The keys.
    f.press(cx, "ctrl-r");
    let services = f.recording.clone();
    wait_for(cx, "the keyed resume", move |_| {
        services.terminal_commands().len() == 1
    });
    f.press(cx, "shift-y");
    let keyed_copy = f.clipboard_writes();
    f.press(cx, "e");
    wait_dialog(cx, handle);
    let keyed_export = export_target(cx);
    close_dialog(cx, handle);

    // The same items from the Session menu, dispatched like the menu bar does.
    menu_bar_dispatch(cx, handle, session_item(cx, "Resume"));
    let services = f.recording.clone();
    wait_for(cx, "the menu resume", move |_| {
        services.terminal_commands().len() == 2
    });
    menu_bar_dispatch(cx, handle, session_item(cx, "Copy path"));
    let menu_copy = f.clipboard_writes();
    menu_bar_dispatch(cx, handle, session_item(cx, "Export…"));
    wait_dialog(cx, handle);
    let menu_export = export_target(cx);
    close_dialog(cx, handle);

    let commands = f.terminal_commands();
    assert_eq!(commands[0], commands[1], "the same launch");
    assert!(commands[0].contains(&stem(&path)));
    assert_eq!(keyed_copy, vec![path.display().to_string()]);
    assert_eq!(
        menu_copy,
        vec![path.display().to_string(), path.display().to_string()]
    );
    assert_eq!(keyed_export, Some(path.clone()));
    assert_eq!(menu_export, keyed_export);
}

/// X-001 (A1): the viewer shows X; a new result set selects Y in the list;
/// Delete, Resume and Export through the menu bar and through the palette
/// all act on X, the viewer's conversation.
#[gpui_kit::test]
fn x001_menus_and_the_palette_act_on_the_focused_panes_target(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let x = claude_session(&world, "proj", 1, "Target X");
    let y = claude_session(&world, "proj", 2, "Other Y");
    let f = OpsFixture::open(cx, &world, &[]);
    let handle = f.handle();
    f.open_in_viewer(cx, &x);
    // A result set that selects Y while the viewer keeps the focus.
    let moved = f
        .list_pane()
        .update(cx, |list, cx| list.select_path(&y, cx));
    assert!(moved);
    settle(cx);
    assert_eq!(f.list.selected_path(cx), Some(y.clone()));
    assert_eq!(shown_path(cx, f.ws()), Some(x.clone()));

    for via_palette in [false, true] {
        let how = if via_palette { "palette" } else { "menu bar" };
        // Delete: the confirmation holds X (and only X).
        if via_palette {
            palette_dispatch(cx, handle, "delete");
        } else {
            menu_bar_dispatch(cx, handle, session_item(cx, "Delete…"));
        }
        wait_dialog(cx, handle);
        assert_eq!(
            cx.read(|cx| crate::workspace::startup::last_dialog(cx).to_string()),
            "delete",
            "{how}"
        );
        let held = held(cx);
        assert_eq!(held, vec![(x.clone(), "Target X".to_string())], "{how}");
        close_dialog(cx, handle);

        // Resume: the recorded launch resumes X.
        let before = f.terminal_commands().len();
        if via_palette {
            palette_dispatch(cx, handle, "resume");
        } else {
            menu_bar_dispatch(cx, handle, session_item(cx, "Resume"));
        }
        let services = f.recording.clone();
        wait_for(cx, "the launch", move |_| {
            services.terminal_commands().len() == before + 1
        });
        let launched = f.terminal_commands().pop().unwrap();
        assert!(launched.contains(&stem(&x)), "{how}: {launched}");
        assert!(!launched.contains(&stem(&y)), "{how}: {launched}");

        // Export: the menu captured X.
        if via_palette {
            palette_dispatch(cx, handle, "export");
        } else {
            menu_bar_dispatch(cx, handle, session_item(cx, "Export…"));
        }
        wait_dialog(cx, handle);
        assert_eq!(export_target(cx), Some(x.clone()), "{how}");
        close_dialog(cx, handle);
    }
    assert_eq!(
        f.list.selected_path(cx),
        Some(y),
        "the list kept its selection"
    );
}

/// X-002 (A2): a Pi conversation opened from a Deep search hit deletes with
/// the Pi plan — one file, the Pi transcript — from the hit's captured target.
#[gpui_kit::test]
fn x002_a_pi_conversation_from_a_hit_deletes_with_the_pi_plan(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = world.standard_corpus();
    let pi = corpus
        .iter()
        .find(|path| path.to_string_lossy().contains(".pi"))
        .unwrap()
        .clone();
    let f = OpsFixture::open(cx, &world, &[]);
    let (handle, ws) = (f.handle(), f.ws().clone());
    let deep = cx.read(|cx| ws.read(cx).deep().clone());
    f.press(cx, "secondary-2");
    cx.update_window(handle, |_, window, cx| {
        deep.update(cx, |deep, cx| {
            deep.set_query("caching", window, cx);
            deep.run(cx);
        })
    })
    .unwrap();
    let pane = deep.clone();
    wait_for(cx, "the search", move |cx| !pane.read(cx).is_running(cx));
    let target = cx
        .read(|cx| deep.read(cx).selected_hit_target(cx))
        .expect("a hit");
    assert_eq!(target.path, pi);
    assert_eq!(
        target.source,
        Some(Source::Pi),
        "from AgentConversationInfo"
    );
    let results = cx.read(|cx| deep.read(cx).results().clone());
    results.update(cx, |results, cx| results.open_selected(true, cx));
    let viewer = viewer_of(cx, &ws);
    wait_viewer_ready(cx, handle, &viewer);
    assert_eq!(shown_path(cx, &ws), Some(pi.clone()));
    assert_eq!(probe(cx, handle, &ws, "focus"), "viewer");

    f.press(cx, "ctrl-x");
    wait_dialog(cx, handle);
    let held = held(cx);
    assert_eq!(held.len(), 1, "the Pi plan has one file: {held:?}");
    assert_eq!(held[0].0, pi);
    f.press(cx, "y");
    let gone = pi.clone();
    wait_for(cx, "the delete", move |_| !gone.exists());
    settle(cx);
    for other in corpus.iter().filter(|path| **path != pi) {
        assert!(other.exists(), "{} was kept", other.display());
    }
    assert!(
        cx.read(|cx| viewer.read(cx).target().is_none()),
        "viewer cleared"
    );
}

/// V-103: with the focus in the list's search input (nothing selected
/// there), Ctrl+C copies the transcript selection; text selected in the
/// input wins. The focus moves by keyboard (Ctrl+1): in gpui-kit 0.7.3 any
/// left mouse-down clears the window's text selection in its capture phase
/// (`TextSelection`), so a click on a row would drop the selection first
/// (integrator, Phase C: FEATURES V-103 acceptance adapted).
#[gpui_kit::test]
fn v103_ctrl_c_in_the_list_copies_the_transcript_selection(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let paths = crate::test_support::fixtures_list::listed_corpus(&world);
    let f = OpsFixture::open(cx, &world, &[]);
    let (handle, ws) = (f.handle(), f.ws().clone());
    let shown = paths[0].clone();
    let viewer = f.open_in_viewer(cx, &shown);
    let position = |cx: &mut TestAppContext, item: usize, byte: usize| {
        viewer.update(cx, |v, cx| v.request_text_probe(item, 0, byte, cx));
        draw(cx, handle);
        cx.read(|cx| viewer.read(cx).text_probe())
            .expect("the probed segment painted")
    };
    let start = position(cx, 0, 0);
    let end = position(cx, 1, 6);
    cx.update_window(handle, |_, window, cx| {
        use gpui_kit::test::TestWindowExt as _;
        window.drag(
            start + point(px(1.), px(4.)),
            end + point(px(1.), px(4.)),
            cx,
        )
    })
    .unwrap();
    cx.run_until_parked();
    // The selection exists (Ctrl+C in the transcript copies it).
    cx.simulate_keystrokes(handle, "secondary-c");
    cx.run_until_parked();
    let in_viewer = recording(cx).clipboard().unwrap_or_default();
    assert!(
        in_viewer.contains("question 1"),
        "in the viewer: {in_viewer:?}"
    );

    // Ctrl+1: the search input takes the focus; the viewer keeps its
    // conversation and its selection.
    cx.simulate_keystrokes(handle, "secondary-1");
    render(cx, handle);
    assert_eq!(probe(cx, handle, &ws, "focus"), "list");
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(200));
    cx.run_until_parked();
    assert_eq!(shown_path(cx, &ws), Some(shown.clone()));

    recording(cx).set_clipboard(None);
    cx.simulate_keystrokes(handle, "secondary-c");
    cx.run_until_parked();
    let copied = recording(cx).clipboard().unwrap_or_default();
    assert!(copied.contains("question 1"), "{copied:?}");
    assert_eq!(
        f.list.read(cx, |list, cx| list.query(cx)).as_ref(),
        "",
        "the search input had nothing selected"
    );
    // With text selected in the search input, Ctrl+C copies that instead.
    f.list.type_text(cx, "login");
    cx.simulate_keystrokes(handle, "secondary-a secondary-c");
    cx.run_until_parked();
    assert_eq!(recording(cx).clipboard().as_deref(), Some("login"));
    // A click on a row: the search input keeps (takes) the focus and the
    // click clears the window's text selection (gpui-kit), as anywhere else.
    f.press(cx, "escape");
    f.list.wait_idle(cx);
    click_row(cx, handle, &paths[1]);
    assert_eq!(probe(cx, handle, &ws, "focus"), "list");
    assert_eq!(f.list.selected_path(cx), Some(paths[1].clone()));
}
