//! Corpus changes and results across the panes: V-007, T-023, D-011 (and
//! WP4's D-024 click), X-023.

use std::path::Path;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, SharedString, TestAppContext};

use super::{has_dialog, render, settle, shown_path, viewer_of, wait_for};
use crate::actions::*;
use crate::deep_search::results::{Body, hit_element_id};
use crate::globals::last_toast;
use crate::notify::{last_toast_actions, toast_log};
use crate::ops::rename::LastRename;
use crate::test_support::fixtures_agent::{agent_corpus, outline_session};
use crate::test_support::fixtures_list::{ListFixture, install_failing_embedder};
use crate::test_support::fixtures_ops::{OpsFixture, claude_session, write_gui_config};
use crate::test_support::fixtures_viewer::{single_file_viewer, wait_viewer_ready};
use crate::test_support::{TestWorld, fault, open_main, open_single_file, synth};

fn rename_dialog(
    cx: &TestAppContext,
) -> Option<gpui_kit::Entity<crate::ops::rename::RenameDialog>> {
    cx.read(|cx| {
        cx.try_global::<LastRename>()
            .and_then(|last| last.0.upgrade())
            .filter(|dialog| !dialog.read(cx).is_closed())
    })
}

fn wait_dialog(cx: &mut TestAppContext, handle: gpui_kit::AnyWindowHandle) {
    for _ in 0..200 {
        settle(cx);
        render(cx, handle);
        if has_dialog(cx, handle) {
            return;
        }
    }
    panic!("no dialog opened");
}

fn header_text(
    cx: &TestAppContext,
    viewer: &gpui_kit::Entity<crate::viewer::ViewerPane>,
) -> String {
    cx.read(|cx| {
        viewer
            .read(cx)
            .header_layout()
            .map(|layout| layout.text())
            .unwrap_or_default()
    })
}

fn stem(path: &Path) -> String {
    path.file_stem().unwrap().to_string_lossy().into_owned()
}

/// V-007: renaming the open conversation updates the viewer header in place
/// (no reopen: the same build stays installed).
#[gpui_kit::test]
fn v007_a_rename_updates_the_open_header(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Old title");
    claude_session(&world, "proj", 2, "Another one");
    let f = OpsFixture::open(cx, &world, &[]);
    let handle = f.handle();
    let viewer = f.open_in_viewer(cx, &path);
    assert!(
        header_text(cx, &viewer).contains("Old title"),
        "{}",
        header_text(cx, &viewer)
    );
    let builds = cx.read(|cx| viewer.read(cx).completed_builds());
    let items = cx.read(|cx| viewer.read(cx).items().unwrap());

    f.press(cx, "f2");
    wait_dialog(cx, handle);
    let dialog = rename_dialog(cx).expect("the rename dialog");
    assert_eq!(cx.read(|cx| dialog.read(cx).target().path.clone()), path);
    cx.update_window(handle, |_, window, cx| {
        let input = dialog.read(cx).input();
        input.update(cx, |input, cx| input.set_value("New title", window, cx));
        dialog.update(cx, |dialog, cx| dialog.save(window, cx));
    })
    .unwrap();
    let shown = viewer.clone();
    wait_for(cx, "the header", move |cx| {
        shown
            .read(cx)
            .header_layout()
            .is_some_and(|layout| layout.text().contains("New title"))
    });
    render(cx, handle);
    assert!(!has_dialog(cx, handle), "the dialog closed");
    assert_eq!(shown_path(cx, f.ws()), Some(path.clone()));
    assert_eq!(
        cx.read(|cx| viewer.read(cx).completed_builds()),
        builds,
        "not reopened"
    );
    let same_items =
        cx.read(|cx| std::sync::Arc::ptr_eq(&viewer.read(cx).items().unwrap(), &items));
    assert!(same_items, "the installed items stayed");
    assert!(f.list.titles(cx).iter().any(|title| title == "New title"));
}

/// T-023 (PD15): the viewer shows X and a rename dialog for X is open; X is
/// deleted by session ID from another window → the viewer is cleared and the
/// rename dialog closes with `Closed: "<title>" was deleted` in its window.
#[gpui_kit::test]
fn t023_a_tool_deletion_clears_the_viewer_and_closes_dialogs(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let doomed = claude_session(&world, "proj", 1, "Doomed conversation");
    let survivor = claude_session(&world, "proj", 2, "Survivor");
    let f = OpsFixture::open(cx, &world, &[]);
    let main = f.handle();
    let viewer = f.open_in_viewer(cx, &doomed);
    f.press(cx, "f2");
    wait_dialog(cx, main);
    assert!(rename_dialog(cx).is_some());

    // Tools → Delete conversation by session ID in a single-file window.
    let single = open_single_file(cx, &world, &survivor);
    let single_viewer = single_file_viewer(cx, single);
    wait_viewer_ready(cx, single, &single_viewer);
    cx.update_window(single, |_, window, cx| {
        crate::tools::open_delete_by_id(Some(stem(&doomed)), window, cx)
    })
    .unwrap();
    settle(cx);
    let dialog = cx
        .read(crate::tools::delete_by_id::current_dialog)
        .expect("the Delete by ID dialog");
    let finder = dialog.clone();
    wait_for(cx, "the copies", move |cx| {
        !finder.read(cx).copies().is_empty() && !finder.read(cx).is_busy()
    });
    cx.update_window(single, |_, window, cx| {
        dialog.update(cx, |dialog, cx| dialog.request_delete(window, cx))
    })
    .unwrap();
    // The plan ran: the confirmation is stacked on the dialog.
    let planner = dialog.clone();
    wait_for(cx, "the plan", move |cx| !planner.read(cx).is_busy());
    settle(cx);
    render(cx, single);
    cx.simulate_keystrokes(single, "y");
    let gone = doomed.clone();
    wait_for(cx, "the deletion", move |_| !gone.exists());
    settle(cx);
    render(cx, main);

    assert!(
        cx.read(|cx| viewer.read(cx).target().is_none()),
        "viewer cleared"
    );
    assert!(rename_dialog(cx).is_none(), "the rename dialog closed");
    assert!(
        !has_dialog(cx, main),
        "nothing left open in the main window"
    );
    let log = cx.read(toast_log);
    assert!(
        log.iter().any(|(window, text)| *window == main
            && text.as_ref() == "Closed: \"Doomed conversation\" was deleted"),
        "{log:?}"
    );
    assert!(survivor.exists());
    assert!(!f.list.paths(cx).contains(&doomed), "the row left the list");
}

/// D-011: clicking the hit of message m12 opens its conversation with the
/// navigation focus on the message of ordinal 12, scrolled to the top.
#[gpui_kit::test]
fn d011_a_hit_opens_at_its_message(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    agent_corpus(&world);
    // Long enough that m12 can scroll to the top (80 messages).
    let long = outline_session(&world, 7, "Long outline", 80);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let (handle, ws) = (f.handle, f.ws.clone());
    let deep = cx.read(|cx| ws.read(cx).deep().clone());
    f.press(cx, "secondary-2");
    cx.update_window(handle, |_, window, cx| {
        deep.update(cx, |deep, cx| {
            deep.set_query("\"outline topic 12 answer\"", window, cx);
            deep.run(cx);
        })
    })
    .unwrap();
    let pane = deep.clone();
    wait_for(cx, "the search", move |cx| !pane.read(cx).is_running(cx));
    render(cx, handle);
    let results = cx.read(|cx| deep.read(cx).results().clone());
    // Every outline conversation has an m12; take the long one.
    let hit = cx.read(|cx| match results.read(cx).body() {
        Body::Search(model) => (0..model.hits)
            .filter_map(|ix| model.hit(ix))
            .find(|hit| hit.title == "Long outline")
            .cloned(),
        _ => None,
    });
    let hit = hit.expect("the hit in Long outline");
    assert_eq!(hit.ordinal(), 12, "the hit of m12");
    let id = hit_element_id("deep-results", &hit);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(id.clone(), cx);
    })
    .unwrap();
    let viewer = viewer_of(cx, &ws);
    wait_viewer_ready(cx, handle, &viewer);
    settle(cx);
    wait_viewer_ready(cx, handle, &viewer);
    assert_eq!(shown_path(cx, &ws), Some(long.clone()));
    let (focused, top, text) = cx.read(|cx| {
        let viewer = viewer.read(cx);
        let items = viewer.items().unwrap();
        let focused = viewer.nav().focused();
        let top = viewer.list_state().logical_scroll_top();
        let text = items
            .iter()
            .find(|item| item.nav_position == focused)
            .map(crate::test_support::fixtures_viewer::item_text)
            .unwrap_or_default();
        (focused, (top.item_ix, f32::from(top.offset_in_item)), text)
    });
    assert_eq!(
        focused,
        Some(11),
        "ordinal 12 is the 12th navigable message"
    );
    assert!(text.contains("outline topic 12 answer"), "{text}");
    let item_of_focus = cx.read(|cx| {
        viewer
            .read(cx)
            .items()
            .unwrap()
            .iter()
            .position(|item| item.nav_position == Some(11))
            .unwrap()
    });
    assert_eq!(top.0, item_of_focus, "scrolled to the top");
    assert!(top.1.abs() < 1.0, "at the message's start: {}", top.1);

    // D-024 (a WP4 deviation closed here): a real click on the hit's anchor
    // button puts its `ma_` anchors on the clipboard.
    f.press(cx, "secondary-2");
    render(cx, handle);
    assert!(hit.anchors.iter().all(|anchor| anchor.starts_with("ma_")));
    assert!(!hit.anchors.is_empty());
    let button = format!(
        "deep-results-copy-anchor-{}-m{}-m{}",
        hit.conversation_ref, hit.focus.0, hit.focus.1
    );
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(SharedString::from(button), cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        crate::test_support::recording(cx).clipboard(),
        Some(hit.anchors.join(","))
    );
}

/// X-023: errors are surfaced with their §12 texts and recovery actions —
/// a terminal spawn failure, an export write failure, an unknown session ID,
/// a rename failure and a delete failure in one session.
#[gpui_kit::test]
fn x023_errors_show_their_texts_and_recovery(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let path = claude_session(&world, "proj", 1, "Error table");
    claude_session(&world, "proj", 2, "Second");
    let f = OpsFixture::open(cx, &world, &[]);
    let handle = f.handle();

    // Terminal spawn failure: + [Terminal settings].
    f.recording.set_spawn_result(Err("boom".to_string()));
    f.select(cx, &path);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    wait_for(cx, "the launch error", |cx| {
        last_toast(cx).as_ref() == "Failed to launch terminal: boom"
    });
    assert_eq!(cx.read(last_toast_actions), ["Terminal settings"]);
    f.recording.set_spawn_result(Ok(None));

    // Export write failure: `Failed to write: <e>`.
    f.recording
        .answer_save(Ok(Some(world.home().join("missing-dir").join("x.md"))));
    f.select(cx, &path);
    cx.dispatch_action(handle, ExportConversation);
    wait_dialog(cx, handle);
    f.press(cx, "3");
    f.settle(cx);
    wait_for(cx, "the write error", |cx| {
        last_toast(cx).starts_with("Failed to write: ")
    });

    // Unknown session ID: `Session not found: <uuid>`, the rows stay.
    f.focus_list(cx);
    let rows = f.list.paths(cx);
    let unknown = synth::uuid(999_999);
    f.list.type_text(cx, &unknown);
    let text = format!("Session not found: {unknown}");
    let wanted = text.clone();
    wait_for(cx, "the not-found toast", move |cx| {
        last_toast(cx).as_ref() == wanted
    });
    f.list.wait_idle(cx);
    assert_eq!(f.list.paths(cx), rows);
    f.press(cx, "escape");
    f.list.wait_idle(cx);

    // The rename and delete failures below rely on file permissions.
    if fault::running_as_root() {
        eprintln!("skipped the permission cases: root (DESIGN 15.10)");
        return;
    }

    // Rename failure: inline `Failed to rename: <e>`, the dialog stays for a retry.
    let guard = fault::unwritable(&path);
    f.select(cx, &path);
    f.press(cx, "f2");
    wait_dialog(cx, handle);
    let dialog = rename_dialog(cx).expect("the rename dialog");
    cx.update_window(handle, |_, window, cx| {
        let input = dialog.read(cx).input();
        input.update(cx, |input, cx| input.set_value("Renamed", window, cx));
        dialog.update(cx, |dialog, cx| dialog.save(window, cx));
    })
    .unwrap();
    let pending = dialog.clone();
    wait_for(cx, "the rename error", move |cx| {
        pending.read(cx).error().is_some()
    });
    let error = cx
        .read(|cx| dialog.read(cx).error().map(str::to_string))
        .unwrap();
    assert!(error.starts_with("Failed to rename: "), "{error}");
    assert!(has_dialog(cx, handle), "still open for a retry");
    assert!(
        !cx.read(|cx| dialog.read(cx).is_saving()),
        "Save is available again"
    );
    drop(guard);
    cx.simulate_keystrokes(handle, "escape");
    f.settle(cx);

    // Delete failure: `Failed to delete session <path>: <e>`, the row stays.
    let guard = fault::undeletable(&path);
    f.select(cx, &path);
    f.press(cx, "ctrl-x");
    wait_dialog(cx, handle);
    f.press(cx, "y");
    let prefix = format!("Failed to delete session {}: ", path.display());
    wait_for(cx, "the delete error", move |cx| {
        last_toast(cx).starts_with(&prefix)
    });
    drop(guard);
    assert!(path.exists());
    assert!(f.list.paths(cx).contains(&path), "still listed");
}

/// X-023: a failing semantic model shows `Semantic search failed` with
/// Switch to lexical and Retry (empty state and toast).
#[gpui_kit::test]
fn x023_a_semantic_error_offers_lexical_and_retry(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    synth::corpus(&world, 6);
    ListFixture::init_app(cx, &world, &[]);
    install_failing_embedder(cx, "model missing");
    let f = ListFixture::open_loaded(cx, &world, &[]);
    cx.dispatch_action(f.handle, SetModeSemantic);
    f.type_text(cx, "synthetic");
    f.wait_until(cx, "the semantic error", |list, _| {
        list.browser().semantic_error().is_some()
    });
    f.wait_idle(cx);
    render(cx, f.handle);
    let toast = f
        .read(cx, |list, _| list.toasts().last().cloned())
        .expect("a toast");
    assert!(
        toast.text.starts_with("Semantic search failed: "),
        "{toast:?}"
    );
    assert!(toast.text.contains("model missing"), "{toast:?}");
    assert_eq!(toast.buttons, ["Switch to lexical", "Retry"]);
    let status = f.read(cx, |list, _| list.search_status().text);
    assert!(status.contains("sem failed"), "{status}");
    // The list area: the error state with the same two recovery actions.
    let state = f
        .read(cx, |list, cx| list.empty_state(cx))
        .expect("an empty state");
    assert_eq!(state.title, "Semantic search failed");
    assert!(
        state
            .description
            .as_deref()
            .is_some_and(|text| text.contains("model missing"))
    );
    let labels: Vec<&str> = state.actions.iter().map(|action| action.label()).collect();
    assert_eq!(labels, ["Switch to lexical", "Retry"]);
    // Switch to lexical recovers the rows.
    cx.dispatch_action(f.handle, SetModeLexical);
    f.wait_idle(cx);
    assert_eq!(f.paths(cx).len(), 6);
}

/// X-023: a config.toml parse error opens `Couldn't read config.toml` with
/// core's text and the recovery buttons.
#[gpui_kit::test]
fn x023_a_config_parse_error_opens_its_dialog(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    world.write_config(Some("this is = not toml ["), None);
    let (handle, _ws) = open_main(cx, &world, &[]);
    cx.update_window(handle, |_, window, cx| {
        crate::workspace::startup::show_config_errors(window, cx)
    })
    .unwrap();
    render(cx, handle);
    assert!(has_dialog(cx, handle));
    assert_eq!(
        cx.read(|cx| crate::workspace::startup::last_dialog(cx).to_string()),
        "Couldn't read config.toml"
    );
    let error = cx.read(|cx| {
        crate::globals::app_state(cx)
            .config_errors
            .cli
            .clone()
            .unwrap()
    });
    assert!(
        error.starts_with("Configuration error: Failed to parse config file at '"),
        "{error}"
    );
    cx.update_window(handle, |_, window, _| {
        for id in [
            "config-open",
            "config-retry",
            "config-continue",
            "config-quit",
        ] {
            assert!(window.try_find(id).is_some(), "{id}");
        }
    })
    .unwrap();
}
