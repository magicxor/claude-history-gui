//! Focus, previews and layout across the panes: L-085, L-098, C-058, X-043,
//! X-024, X-040.

use std::time::Duration;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, Focusable as _, TestAppContext, px, size};

use super::{has_dialog, probe, render, settle, shown_path, viewer_of, wait_for};
use crate::actions::*;
use crate::globals::app_state;
use crate::list::ListPaneEvent;
use crate::model::{LeftTab, PaneFocus};
use crate::test_support::fixtures_list::{ListFixture, add_session, listed_corpus};
use crate::test_support::fixtures_ops::{OpsFixture, claude_session, write_gui_config};
use crate::test_support::fixtures_viewer::wait_viewer_ready;
use crate::test_support::{TestWorld, recording};

/// L-085: keyboard selection previews in the viewer after the 150 ms
/// debounce, only while the list has focus; a list action within the
/// debounce shows the new row first and acts on it.
#[gpui_kit::test]
fn l085_previews_are_debounced_and_flushed_by_list_actions(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    for (number, title) in [(1, "First"), (2, "Second"), (3, "Third"), (4, "Fourth")] {
        claude_session(&world, "proj", number, title);
    }
    let f = OpsFixture::open(cx, &world, &[]);
    let (handle, ws) = (f.handle(), f.ws().clone());
    let rows = f.list.paths(cx);
    assert_eq!(rows.len(), 4);
    f.focus_list(cx);
    cx.executor().advance_clock(Duration::from_millis(200));
    cx.run_until_parked();

    // `down`: debounced, then shown.
    f.press(cx, "down");
    assert_eq!(f.list.selected_path(cx).as_ref(), Some(&rows[1]));
    assert!(cx.read(|cx| ws.read(cx).has_pending_preview()));
    assert_ne!(shown_path(cx, &ws).as_ref(), Some(&rows[1]), "debounced");
    cx.executor().advance_clock(Duration::from_millis(200));
    cx.run_until_parked();
    assert_eq!(shown_path(cx, &ws).as_ref(), Some(&rows[1]), "previewed");
    assert_eq!(
        probe(cx, handle, &ws, "focus"),
        "list",
        "the list keeps focus"
    );

    // With the viewer focused a new selection (a result set) never replaces it.
    let viewer = viewer_of(cx, &ws);
    cx.update_window(handle, |_, window, cx| {
        viewer.update(cx, |viewer, cx| viewer.focus(window, cx))
    })
    .unwrap();
    render(cx, handle);
    let events = f.list.record_events(cx);
    let moved = f
        .list_pane()
        .update(cx, |list, cx| list.select_path(&rows[2], cx));
    assert!(moved);
    cx.run_until_parked();
    assert_eq!(
        events.count_matching(|event| matches!(event, ListPaneEvent::SelectionChanged { .. })),
        1,
        "the selection really changed"
    );
    cx.executor().advance_clock(Duration::from_millis(200));
    cx.run_until_parked();
    assert_eq!(shown_path(cx, &ws).as_ref(), Some(&rows[1]));
    assert!(!cx.read(|cx| ws.read(cx).has_pending_preview()));

    // `down` then Ctrl+R within 50 ms (no clock advance): the viewer shows
    // the new row and the recorded launch resumes it.
    f.focus_list(cx);
    f.press(cx, "down");
    assert_eq!(f.list.selected_path(cx).as_ref(), Some(&rows[3]));
    f.press(cx, "ctrl-r");
    assert_eq!(
        shown_path(cx, &ws).as_ref(),
        Some(&rows[3]),
        "flushed first"
    );
    let services = f.recording.clone();
    wait_for(cx, "the recorded resume", move |_| {
        !services.terminal_commands().is_empty()
    });
    let stem = rows[3].file_stem().unwrap().to_string_lossy().to_string();
    let commands = f.terminal_commands();
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert!(commands[0].contains(&stem), "{commands:?}");
}

/// L-098: leaving the viewer (`q`) returns to the search input with the
/// query, the rows and the selection unchanged.
#[gpui_kit::test]
fn l098_leaving_the_viewer_keeps_the_list_state(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    for (number, title) in [
        (1, "alpha one"),
        (2, "alpha two"),
        (3, "alpha three"),
        (4, "beta four"),
    ] {
        add_session(&world, "proj", number, title, number);
    }
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let (handle, ws) = (f.handle, f.ws.clone());
    f.type_text(cx, "alpha");
    f.wait_idle(cx);
    let rows = f.paths(cx);
    assert_eq!(rows.len(), 3);
    f.press(cx, "down down");
    assert_eq!(f.selected_position(cx), Some(2));
    let selected = f.selected_path(cx).unwrap();

    f.press(cx, "enter");
    let viewer = viewer_of(cx, &ws);
    wait_viewer_ready(cx, handle, &viewer);
    assert_eq!(shown_path(cx, &ws), Some(selected.clone()));
    assert_eq!(probe(cx, handle, &ws, "focus"), "viewer");

    f.press(cx, "q");
    render(cx, handle);
    assert_eq!(probe(cx, handle, &ws, "focus"), "list");
    let search_focused = cx
        .update_window(handle, |_, window, cx| {
            f.list.focus_handle(cx).contains_focused(window, cx)
        })
        .unwrap();
    assert!(search_focused, "the search input has the focus");
    assert_eq!(f.read(cx, |list, cx| list.query(cx)).as_ref(), "alpha");
    assert_eq!(f.paths(cx), rows, "rows unchanged");
    assert_eq!(f.selected_position(cx), Some(2));
    assert_eq!(f.selected_path(cx), Some(selected));
}

/// C-058 (AD16): F6 cycles search → toolbar → viewer → agent panel → back;
/// Shift+F6 reverses; with the Deep search tab the query joins the cycle;
/// a hidden left pane is skipped.
#[gpui_kit::test]
fn c058_f6_cycles_the_regions(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let paths = listed_corpus(&world);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let (handle, ws) = (f.handle, f.ws.clone());
    let region = |cx: &mut TestAppContext| {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            ws.read(cx).focused_region(window, cx)
        })
        .unwrap()
    };
    f.press(cx, "secondary-1");
    assert_eq!(region(cx), PaneFocus::List);
    let forward = [PaneFocus::ListToolbar, PaneFocus::Viewer, PaneFocus::List];
    for expected in forward {
        f.press(cx, "f6");
        assert_eq!(region(cx), expected);
    }
    // The agent panel joins once it is open (opened from the list's row).
    f.list.update(cx, |_, cx| {
        cx.emit(ListPaneEvent::OpenRequested {
            target: crate::model::ConversationTarget::from_parsed_file(&paths[0], None),
            focus_viewer: false,
            agent_tab: Some(crate::model::AgentTab::Outline),
        })
    });
    settle(cx);
    assert_eq!(region(cx), PaneFocus::AgentPanel);
    let cycle = [
        PaneFocus::List,
        PaneFocus::ListToolbar,
        PaneFocus::Viewer,
        PaneFocus::AgentPanel,
    ];
    for expected in cycle {
        f.press(cx, "f6");
        assert_eq!(region(cx), expected, "F6");
    }
    for expected in [PaneFocus::Viewer, PaneFocus::ListToolbar, PaneFocus::List] {
        f.press(cx, "shift-f6");
        assert_eq!(region(cx), expected, "Shift+F6");
    }
    // Deep search tab: viewer → agent panel → deep search query.
    f.press(cx, "secondary-2");
    assert_eq!(region(cx), PaneFocus::DeepSearch);
    for expected in [
        PaneFocus::Viewer,
        PaneFocus::AgentPanel,
        PaneFocus::DeepSearch,
    ] {
        f.press(cx, "f6");
        assert_eq!(region(cx), expected, "F6 with Deep search");
    }
    // A hidden left pane is skipped.
    cx.dispatch_action(handle, ToggleListPane);
    render(cx, handle);
    assert!(cx.read(|cx| ws.read(cx).left_collapsed()));
    let regions = cx.read(|cx| ws.read(cx).regions(cx));
    assert_eq!(regions, [PaneFocus::Viewer, PaneFocus::AgentPanel]);
    f.press(cx, "f6");
    let first = region(cx);
    f.press(cx, "f6");
    let second = region(cx);
    assert_ne!(first, second);
    assert!(regions.contains(&first) && regions.contains(&second));
}

/// X-043: when the focused element disappears and nothing focusable is left
/// above it, the focus goes to the visible left tab's primary element — the
/// Deep search query on that tab, never the hidden list input.
#[gpui_kit::test]
fn x043_focus_lost_follows_the_visible_left_tab(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let paths = listed_corpus(&world);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let (handle, ws) = (f.handle, f.ws.clone());
    f.list.update(cx, |_, cx| {
        cx.emit(ListPaneEvent::OpenRequested {
            target: crate::model::ConversationTarget::from_parsed_file(&paths[0], None),
            focus_viewer: false,
            agent_tab: Some(crate::model::AgentTab::Outline),
        })
    });
    settle(cx);
    f.press(cx, "secondary-2");
    assert_eq!(cx.read(|cx| ws.read(cx).left_tab()), LeftTab::DeepSearch);
    // Focus the agent panel, then take the panel away under the focus.
    let panel = cx.read(|cx| viewer_of_window(&ws, cx).read(cx).agent_panel().clone());
    cx.update_window(handle, |_, window, cx| {
        panel.focus_handle(cx).focus(window, cx);
    })
    .unwrap();
    render(cx, handle);
    assert_eq!(probe(cx, handle, &ws, "focus"), "agent");
    panel.update(cx, |panel, cx| panel.hide(cx));
    render(cx, handle);
    render(cx, handle);
    assert_eq!(
        probe(cx, handle, &ws, "focus"),
        "deep",
        "the visible tab's query"
    );
    let deep_query = cx
        .update_window(handle, |_, window, cx| {
            ws.read(cx).deep().read(cx).contains_focus(window, cx)
                && !ws.read(cx).list().read(cx).contains_focus(window, cx)
        })
        .unwrap();
    assert!(deep_query);

    // On the Conversations tab the same loss lands in the list's search.
    f.press(cx, "secondary-1");
    cx.update_window(handle, |_, window, cx| {
        viewer_of_window(&ws, cx).update(cx, |viewer, cx| {
            viewer.open_agent_panel(crate::model::AgentTab::Outline, window, cx)
        });
    })
    .unwrap();
    settle(cx);
    cx.update_window(handle, |_, window, cx| {
        panel.focus_handle(cx).focus(window, cx);
    })
    .unwrap();
    render(cx, handle);
    panel.update(cx, |panel, cx| panel.hide(cx));
    render(cx, handle);
    render(cx, handle);
    assert_eq!(probe(cx, handle, &ws, "focus"), "list");
}

fn viewer_of_window(
    ws: &gpui_kit::Entity<crate::workspace::Workspace>,
    cx: &gpui_kit::App,
) -> gpui_kit::Entity<crate::viewer::ViewerPane> {
    ws.read(cx).viewer().clone()
}

/// X-024: closing a dialog (Esc) returns the focus to the element that had
/// it before the dialog opened — from the list's search input and from the
/// transcript, for the session dialogs, the tools and the help overlays.
#[gpui_kit::test]
fn x024_dialogs_restore_the_focus_they_took(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let path = claude_session(&world, "proj", 1, "Restore focus");
    claude_session(&world, "proj", 2, "Another one");
    let f = OpsFixture::open(cx, &world, &[]);
    let handle = f.handle();
    recording(cx).set_release(Err("offline".to_string()));
    cx.update(|cx| cx.set_reduce_motion(true));

    let dialogs: Vec<(&str, Box<dyn gpui_kit::Action>)> = vec![
        ("rename", Box::new(Rename)),
        ("delete", Box::new(Delete)),
        ("export menu", Box::new(ExportConversation)),
        ("copy menu", Box::new(YankOrCopyMessage)),
        ("palette", Box::new(CommandPalette)),
        ("shortcuts", Box::new(ShowShortcuts)),
        ("settings", Box::new(OpenSettings)),
        ("debug log", Box::new(ShowDebugLog)),
        ("delete by id", Box::new(DeleteSessionById)),
        ("delete empty", Box::new(DeleteEmptyTranscripts)),
        ("about", Box::new(ShowAbout)),
        ("updates", Box::new(CheckForUpdates)),
    ];
    for in_viewer in [false, true] {
        if in_viewer {
            f.open_in_viewer(cx, &path);
        } else {
            f.select(cx, &path);
        }
        for (name, action) in &dialogs {
            // The viewer's copy menu needs message navigation off (it is).
            let before = cx
                .update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    window.focused(cx)
                })
                .unwrap()
                .expect("something has focus");
            let action = action.boxed_clone();
            cx.update_window(handle, |_, window, cx| window.dispatch_action(action, cx))
                .unwrap();
            wait_for_dialog(cx, handle, name);
            cx.simulate_keystrokes(handle, "escape");
            settle(cx);
            render(cx, handle);
            assert!(!has_dialog(cx, handle), "{name}: Esc closes it");
            let after = cx
                .update_window(handle, |_, window, cx| window.focused(cx))
                .unwrap();
            assert_eq!(
                after.as_ref(),
                Some(&before),
                "{name} ({}): the focus came back",
                if in_viewer { "viewer" } else { "list" }
            );
        }
    }
}

fn wait_for_dialog(cx: &mut TestAppContext, handle: gpui_kit::AnyWindowHandle, name: &str) {
    cx.executor().allow_parking();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        settle(cx);
        render(cx, handle);
        if has_dialog(cx, handle) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{name}: the dialog did not open"
        );
    }
}

/// X-040 (PD1): on an 880 px window the agent panel takes the left pane's
/// room ("auto", not remembered) and gives it back when it closes, with the
/// width clamped to the window; a wide window only narrows the left pane.
#[gpui_kit::test]
fn x040_the_agent_panel_makes_room(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let paths = listed_corpus(&world);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let (handle, ws) = (f.handle, f.ws.clone());
    let viewer_width_rem = |cx: &mut TestAppContext| {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let rem = f32::from(window.rem_size());
            let area = f32::from(window.find("viewer-area").bounds().size.width) / rem;
            let panel = viewer_of_window(&ws, cx).read(cx).agent_panel().read(cx);
            let panel = if panel.is_visible() {
                crate::deep_search::panel::panel_width_rem(cx)
            } else {
                0.0
            };
            area - panel
        })
        .unwrap()
    };
    let layout = |cx: &mut TestAppContext| {
        cx.read(|cx| (ws.read(cx).left_collapsed(), ws.read(cx).auto_collapsed()))
    };

    cx.simulate_window_resize(handle, size(px(880.), px(600.)));
    render(cx, handle);
    f.list.update(cx, |_, cx| {
        cx.emit(ListPaneEvent::OpenRequested {
            target: crate::model::ConversationTarget::from_parsed_file(&paths[0], None),
            focus_viewer: true,
            agent_tab: None,
        })
    });
    let viewer = viewer_of(cx, &ws);
    wait_viewer_ready(cx, handle, &viewer);
    assert_eq!(layout(cx), (false, false));
    assert!(viewer_width_rem(cx) >= 32.0 - 0.05);

    // Open the panel (`o` in the transcript): the list goes, marked auto.
    f.press(cx, "o");
    settle(cx);
    assert!(cx.read(|cx| viewer.read(cx).agent_panel().read(cx).is_visible()));
    assert_eq!(layout(cx), (true, true), "collapsed automatically");
    assert!(
        !cx.read(|cx| app_state(cx).ui.list_collapsed),
        "not remembered"
    );
    assert!(
        viewer_width_rem(cx) >= 32.0 - 0.05,
        "the transcript keeps 32rem"
    );
    assert_eq!(probe(cx, handle, &ws, "layout.auto_collapsed"), "true");

    // Close it (`o` again): the list comes back, clamped to the window.
    f.press(cx, "o");
    settle(cx);
    assert!(!cx.read(|cx| viewer.read(cx).agent_panel().read(cx).is_visible()));
    assert_eq!(layout(cx), (false, false), "restored");
    let left = cx
        .update_window(handle, |_, window, cx| {
            ws.read(cx).effective_left_width_rem(window)
        })
        .unwrap();
    assert!(left <= 55.0 - 32.0, "clamped to the window: {left}");
    assert!(viewer_width_rem(cx) >= 32.0 - 0.05);

    // A wide window: the panel fits next to a narrower list.
    cx.simulate_window_resize(handle, size(px(1280.), px(820.)));
    render(cx, handle);
    f.press(cx, "o");
    settle(cx);
    assert_eq!(layout(cx), (false, false), "no collapse at 80rem");
    let left = cx
        .update_window(handle, |_, window, cx| {
            ws.read(cx).effective_left_width_rem(window)
        })
        .unwrap();
    assert!((18.0..30.0).contains(&left), "narrowed, not hidden: {left}");
    assert!(viewer_width_rem(cx) >= 32.0 - 0.05);
    // Shrinking the window with the panel open hides the list (auto).
    cx.simulate_window_resize(handle, size(px(880.), px(600.)));
    settle(cx);
    render(cx, handle);
    assert_eq!(layout(cx), (true, true));
    // View > Focus viewer reads checked while the list is hidden automatically.
    assert!(
        cx.read(|cx| crate::menus::current_checks(cx).list_collapsed),
        "the auto-hidden list shows Focus viewer checked"
    );
    // An explicit Focus viewer toggle ends the automatic state.
    cx.dispatch_action(handle, ToggleListPane);
    render(cx, handle);
    assert_eq!(layout(cx), (false, false));
    // The list that comes back keeps its minimum width (it never renders as a sliver).
    let left = cx
        .update_window(handle, |_, window, cx| {
            ws.read(cx).effective_left_width_rem(window)
        })
        .unwrap();
    assert!(
        left >= 18.0,
        "the shown list is at least its minimum: {left}"
    );
    assert!(
        !cx.read(|cx| crate::menus::current_checks(cx).list_collapsed),
        "an explicit show clears the check"
    );
}
