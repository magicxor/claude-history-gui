//! G tests of the search input, the list keys, the selection and the
//! pointer (L-031…L-036, L-046, L-051…L-060, L-062, L-064…L-067, L-080,
//! S-004…S-009, S-020, X-010, A-049, C-003).

use claude_history_core::config::{KeyBindings, parse_key_binding};
use gpui_kit::AppContext as _;
use gpui_kit::component::input::Position;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{Focusable as _, Keystroke, ScrollDelta, TestAppContext, point, px};

use crate::exit;
use crate::keymap;
use crate::list::ListPaneEvent;
use crate::test_support::fixtures_list::{ListFixture, listed_corpus, workspace_corpus};
use crate::test_support::{TestWorld, recording, synth};

fn corpus(n: usize) -> TestWorld {
    let world = TestWorld::new();
    synth::corpus(&world, n);
    world
}

fn row_id(path: &std::path::Path) -> String {
    format!("row:{}", path.display())
}

fn cursor(f: &ListFixture, cx: &TestAppContext) -> usize {
    f.read(cx, |list, cx| list.search.read(cx).cursor())
}

fn put_caret(f: &ListFixture, cx: &mut TestAppContext, character: u32) {
    f.with_window(cx, |list, window, cx| {
        list.search.update(cx, |search, cx| {
            search.set_cursor_position(Position::new(0, character), window, cx)
        });
    });
}

/// S-004: typing `abc` after the load makes three `set_query` calls; S-009:
/// clearing lists all rows newest first.
#[gpui_kit::test]
fn typing_dispatches_each_edit(cx: &mut TestAppContext) {
    let world = corpus(30);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let before = f.read(cx, |list, cx| list.qa_probe("list.query_calls", cx));
    assert_eq!(before.as_deref(), Some("0"));
    f.type_text(cx, "abc");
    assert_eq!(
        f.read(cx, |list, cx| list.qa_probe("list.query_calls", cx))
            .as_deref(),
        Some("3")
    );
    f.press(cx, "escape");
    f.wait_idle(cx);
    let times: Vec<_> = f.read(cx, |list, _| {
        (0..30)
            .map(|pos| list.browser().row(pos).unwrap().timestamp)
            .collect()
    });
    assert!(times.windows(2).all(|pair| pair[0] >= pair[1]));
    assert_eq!(f.paths(cx).len(), 30);
}

/// S-005, S-007, L-080: typing ten characters without parking ends with the
/// rows of the final query only; the old rows stay until the answer; the
/// selection resets to row 0 when results arrive.
#[gpui_kit::test]
fn fast_typing_ends_with_the_final_results(cx: &mut TestAppContext) {
    let world = corpus(40);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.with_window(cx, |list, _, cx| list.select_by(5, cx));
    assert_eq!(f.selected_position(cx), Some(5));
    let before = f.paths(cx);
    for ch in "synthetic ".chars() {
        cx.dispatch_keystroke(
            f.handle,
            Keystroke::parse(&ch.to_string().replace(' ', "space")).unwrap(),
        );
    }
    // S-007: nothing was applied yet; the previous rows are still listed.
    assert_eq!(f.paths(cx), before);
    for ch in "conversation".chars() {
        cx.dispatch_keystroke(f.handle, Keystroke::parse(&ch.to_string()).unwrap());
    }
    f.wait_idle(cx);
    assert_eq!(
        f.read(cx, |list, cx| list.query(cx)).as_ref(),
        "synthetic conversation"
    );
    assert_eq!(f.paths(cx).len(), 40, "every row matches the final query");
    // S-005: no later poll changes the applied results.
    let applied = f.paths(cx);
    for _ in 0..5 {
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(f.paths(cx), applied);
    assert_eq!(
        f.selected_position(cx),
        Some(0),
        "L-080: results reset the selection"
    );
    // A query matching nothing leaves no rows.
    f.type_text(cx, "qqqq");
    f.wait_idle(cx);
    assert!(f.paths(cx).is_empty());
}

/// S-005, S-007: with rows that differ per prefix, typing leaves the old rows
/// until the answer (no synchronous search) and ends with the rows of the
/// final query only (an intermediate answer is never left applied).
#[gpui_kit::test]
fn typing_with_distinct_prefix_results_ends_with_the_final_rows(cx: &mut TestAppContext) {
    use crate::test_support::fixtures_list::SessionSpec;
    let world = TestWorld::new();
    SessionSpec::new("proj", 1)
        .summary("alpha one")
        .write(&world);
    SessionSpec::new("proj", 2)
        .summary("alphabet two")
        .write(&world);
    SessionSpec::new("proj", 3)
        .summary("beta three")
        .write(&world);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let before = f.paths(cx);
    assert_eq!(before.len(), 3);
    for ch in "beta".chars() {
        cx.dispatch_keystroke(f.handle, Keystroke::parse(&ch.to_string()).unwrap());
        // "b" alone would already hide "alpha one": the old rows stay.
        assert_eq!(f.paths(cx), before, "after {ch:?} nothing was applied yet");
    }
    f.wait_idle(cx);
    assert_eq!(f.titles(cx), vec!["beta three".to_string()]);
    for _ in 0..5 {
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        f.titles(cx),
        vec!["beta three".to_string()],
        "no intermediate answer is applied later"
    );
}

/// L-034, X-010: pasted line breaks and tabs become spaces and the text is
/// inserted as one edit; a paste is one `set_query`; no action runs.
#[gpui_kit::test]
fn paste_inserts_text_once(cx: &mut TestAppContext) {
    let world = corpus(10);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let events = f.record_events(cx);
    assert_eq!(crate::list::sanitize_pasted("a\nb\tc"), "a b c");
    assert_eq!(crate::list::sanitize_pasted("a\r\nb"), "a b");
    let taken = f.with_window(cx, |list, window, cx| {
        list.handle_paste("a\nb\tc", window, cx)
    });
    assert!(taken, "a text with line breaks is taken by the list");
    cx.run_until_parked();
    assert_eq!(f.read(cx, |list, cx| list.query(cx)).as_ref(), "a b c");
    assert_eq!(
        events.count_matching(|e| matches!(e, ListPaneEvent::OpenRequested { .. })),
        0
    );
    assert!(!f.read(cx, |list, _| list.scope_is_workspace()));
    // Plain text is left to the input.
    assert!(!f.with_window(cx, |list, window, cx| {
        list.handle_paste("plain", window, cx)
    }));

    f.press(cx, "escape");
    let calls = f
        .read(cx, |list, cx| list.qa_probe("list.query_calls", cx))
        .unwrap();
    let long = "x".repeat(5_000);
    recording(cx).set_clipboard(Some(long));
    f.press(cx, "ctrl-v");
    let after = f
        .read(cx, |list, cx| list.qa_probe("list.query_calls", cx))
        .unwrap();
    assert_eq!(
        after.parse::<usize>().unwrap() - calls.parse::<usize>().unwrap(),
        1,
        "one set_query for the whole paste"
    );
    assert_eq!(f.read(cx, |list, cx| list.query(cx)).len(), 5_000);
}

/// L-035, L-036: `?` is typed, unmatched Ctrl/Alt chords insert nothing and
/// open nothing.
#[gpui_kit::test]
fn only_text_input_edits_the_query(cx: &mut TestAppContext) {
    let world = corpus(5);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.type_text(cx, "?");
    assert_eq!(f.read(cx, |list, cx| list.query(cx)).as_ref(), "?");
    assert!(
        !cx.update_window(f.handle, |_, window, cx| {
            use gpui_kit::component::WindowExt as _;
            window.has_active_dialog(cx)
        })
        .unwrap(),
        "no Shortcuts dialog"
    );
    f.press(cx, "escape");
    // Lexical mode: ctrl-s has no semantic result, alt-x is unbound.
    f.press(cx, "ctrl-s");
    f.press(cx, "alt-x");
    assert_eq!(f.read(cx, |list, cx| list.query(cx)).as_ref(), "");
    assert!(
        !cx.update_window(f.handle, |_, window, cx| {
            use gpui_kit::component::WindowExt as _;
            window.has_active_dialog(cx)
        })
        .unwrap()
    );
}

/// L-052: Home/End select the first/last row and leave the caret alone.
#[gpui_kit::test]
fn home_and_end_select_rows_not_the_caret(cx: &mut TestAppContext) {
    let world = corpus(25);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.type_text(cx, "synthetic");
    f.wait_idle(cx);
    let caret = cursor(&f, cx);
    f.press(cx, "end");
    assert_eq!(f.selected_position(cx), Some(24));
    assert_eq!(cursor(&f, cx), caret);
    f.press(cx, "home");
    assert_eq!(f.selected_position(cx), Some(0));
    assert_eq!(cursor(&f, cx), caret);
}

/// L-053, L-054, L-055: paging moves by the visible rows; Ctrl+D by half;
/// the ends stop.
#[gpui_kit::test]
fn paging_follows_the_visible_rows(cx: &mut TestAppContext) {
    let world = corpus(200);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.with_window(cx, |list, _, _| list.set_visible_rows(20));
    f.press(cx, "pagedown");
    assert_eq!(f.selected_position(cx), Some(20));
    f.press(cx, "pageup");
    assert_eq!(f.selected_position(cx), Some(0));
    f.press(cx, "ctrl-d");
    assert_eq!(f.selected_position(cx), Some(10));
    f.press(cx, "up");
    f.press(cx, "ctrl-p");
    assert_eq!(f.selected_position(cx), Some(8));
    f.press(cx, "home");
    f.press(cx, "up");
    assert_eq!(f.selected_position(cx), Some(0), "stops at the first row");
    f.press(cx, "end");
    f.press(cx, "ctrl-n");
    f.press(cx, "down");
    assert_eq!(f.selected_position(cx), Some(199), "stops at the last row");
}

/// L-067: selecting the last of 200 rows makes that row painted.
#[gpui_kit::test]
fn the_selected_row_is_painted(cx: &mut TestAppContext) {
    let world = corpus(200);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.press(cx, "end");
    let last = f.selected_path(cx).unwrap();
    cx.update_window(f.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(
            window.try_find(row_id(&last)).is_some(),
            "the last row is painted"
        );
    })
    .unwrap();
    // Virtualized: far fewer rows than 200 exist as elements.
    let first = f.paths(cx)[0].clone();
    cx.update_window(f.handle, |_, window, _| {
        assert!(
            window.try_find(row_id(&first)).is_none(),
            "the first row scrolled out"
        );
    })
    .unwrap();
}

/// L-064: the wheel scrolls the viewport; the selection stays.
#[gpui_kit::test]
fn the_wheel_scrolls_without_selecting(cx: &mut TestAppContext) {
    let world = corpus(100);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    cx.update_window(f.handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    let first = f.paths(cx)[0].clone();
    let before = f.read(cx, |list, _| list.scroll_offset());
    let selected = f.selected_position(cx);
    cx.update_window(f.handle, |_, window, cx| {
        window.scroll(
            row_id(&first),
            ScrollDelta::Pixels(point(px(0.0), px(-240.0))),
            cx,
        );
    })
    .unwrap();
    let after = f.read(cx, |list, _| list.scroll_offset());
    assert_ne!(before, after, "the viewport moved");
    assert_eq!(f.selected_position(cx), selected, "the selection did not");
}

/// L-065: a click selects + previews and the search keeps focus; a double
/// click opens in the viewer.
#[gpui_kit::test]
fn clicks_select_and_double_clicks_open(cx: &mut TestAppContext) {
    let world = corpus(12);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let paths = f.paths(cx);
    let events = f.record_events(cx);
    cx.update_window(f.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(row_id(&paths[3]), cx);
    })
    .unwrap();
    assert_eq!(f.selected_position(cx), Some(3));
    let selections = events.selections();
    assert_eq!(selections.len(), 1);
    assert!(selections[0].1, "user = true");
    assert_eq!(
        selections[0].0.as_ref().map(|t| t.path.clone()),
        Some(paths[3].clone())
    );
    cx.update_window(f.handle, |_, window, cx| {
        assert!(
            f.list.focus_handle(cx).is_focused(window),
            "the search input keeps focus"
        );
    })
    .unwrap();

    events.clear();
    cx.update_window(f.handle, |_, window, cx| {
        window.double_click(row_id(&paths[5]), cx);
    })
    .unwrap();
    let open = events.all();
    assert!(
        open.iter().any(|event| matches!(
            event,
            ListPaneEvent::OpenRequested { target, focus_viewer: true, agent_tab: None }
                if target.path == paths[5]
        )),
        "{open:?}"
    );
}

/// L-062 control: the same Ctrl+O on a loaded list does open the print
/// confirmation, so the loading test above is not vacuous.
#[gpui_kit::test]
fn ctrl_o_opens_the_print_dialog_once_loaded(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    recording(cx).set_stdout_kind(crate::model::StdoutKind::Terminal);
    f.press(cx, "ctrl-o");
    cx.run_until_parked();
    assert!(
        cx.update_window(f.handle, |_, window, cx| {
            use gpui_kit::component::WindowExt as _;
            window.has_active_dialog(cx)
        })
        .unwrap(),
        "Ctrl+O asks before printing to a terminal"
    );
}

/// L-065: clicking the row that is already selected previews it again (the
/// viewer may show another conversation).
#[gpui_kit::test]
fn clicking_the_selected_row_previews_it_again(cx: &mut TestAppContext) {
    let world = corpus(12);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let paths = f.paths(cx);
    let events = f.record_events(cx);
    for expected in [1, 1] {
        events.clear();
        cx.update_window(f.handle, |_, window, cx| {
            window.render_frame(cx);
            window.click(row_id(&paths[3]), cx);
        })
        .unwrap();
        assert_eq!(f.selected_position(cx), Some(3));
        let selections = events.selections();
        assert_eq!(selections.len(), expected, "{selections:?}");
        assert!(selections[0].1, "user = true");
        assert_eq!(
            selections[0].0.as_ref().map(|t| t.path.clone()),
            Some(paths[3].clone())
        );
    }
}

/// L-062, L-066: while loading Resume/Fork/Delete/Select do nothing and a row
/// click still previews; a modal dialog blocks row clicks.
#[gpui_kit::test]
fn loading_gates_the_modifying_actions(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let f = ListFixture::open_paused(cx, &world, &[]);
    f.step_until(cx, "the first batch", |list| {
        !list.loading_snapshots().is_empty()
    });
    assert!(f.read(cx, |list, _| list.is_loading()));
    // A terminal stdout makes an ungated Ctrl+O open the print confirmation
    // (see `ctrl_o_opens_the_print_dialog_once_loaded`), so the dialog check
    // below detects a missing SelectAndExit gate.
    recording(cx).set_stdout_kind(crate::model::StdoutKind::Terminal);
    f.press(cx, "ctrl-x");
    f.press(cx, "ctrl-r");
    f.press(cx, "ctrl-f");
    f.press(cx, "ctrl-o");
    f.press(cx, "f2");
    cx.run_until_parked();
    assert!(
        !cx.update_window(f.handle, |_, window, cx| {
            use gpui_kit::component::WindowExt as _;
            window.has_active_dialog(cx)
        })
        .unwrap(),
        "no dialog opened"
    );
    assert!(recording(cx).terminal_commands().is_empty());
    assert_eq!(recording(cx).stdout(), "");
    let events = f.record_events(cx);
    let paths = f.paths(cx);
    assert!(paths.len() >= 2);
    cx.update_window(f.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(row_id(&paths[1]), cx);
    })
    .unwrap();
    assert!(
        events
            .selections()
            .iter()
            .any(|(target, user)| *user && target.as_ref().map(|t| &t.path) == Some(&paths[1])),
        "a click during loading previews"
    );
    f.resume(cx);
    f.wait_loaded(cx);

    // A dialog is modal: a row click changes nothing.
    let rows = f.paths(cx);
    let selected = f.selected_position(cx);
    cx.update_window(f.handle, |_, window, cx| {
        use gpui_kit::component::WindowExt as _;
        window.open_dialog(cx, |dialog, _, _| dialog.title("Modal"));
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(f.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(row_id(&rows[4]), cx);
    })
    .unwrap();
    assert_eq!(
        f.selected_position(cx),
        selected,
        "the dialog blocked the click"
    );
}

/// L-056: Esc clears the query and browses; a second Esc does nothing.
#[gpui_kit::test]
fn escape_clears_then_does_nothing(cx: &mut TestAppContext) {
    let world = corpus(8);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.type_text(cx, "abc");
    f.wait_idle(cx);
    assert!(f.paths(cx).is_empty());
    f.press(cx, "escape");
    f.wait_idle(cx);
    assert_eq!(f.read(cx, |list, cx| list.query(cx)).as_ref(), "");
    assert_eq!(f.paths(cx).len(), 8);
    f.press(cx, "escape");
    assert_eq!(cx.read(exit::quit_requested), None);
    assert!(cx.update_window(f.handle, |_, _, _| ()).is_ok());
}

/// A-049: in picker mode Esc on an empty query quits without output.
#[gpui_kit::test]
fn picker_escape_quits_without_output(cx: &mut TestAppContext) {
    let world = corpus(3);
    let f = ListFixture::open_loaded(cx, &world, &["--show-id"]);
    f.press(cx, "escape");
    assert_eq!(cx.read(exit::quit_requested), Some(exit::EXIT_OK));
    assert!(recording(cx).stdout().is_empty());
    assert!(
        !recording(cx)
            .calls()
            .iter()
            .any(|call| matches!(call, crate::services::ServiceCall::Stdout(_)))
    );
}

/// L-057: Ctrl+C copies the input's selection; the app keeps running.
#[gpui_kit::test]
fn ctrl_c_copies_and_never_quits(cx: &mut TestAppContext) {
    let world = corpus(3);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.type_text(cx, "abc");
    f.press(cx, "ctrl-a");
    f.press(cx, "ctrl-c");
    assert_eq!(recording(cx).clipboard().as_deref(), Some("abc"));
    assert_eq!(cx.read(exit::quit_requested), None);
}

/// L-051: Enter emits `OpenRequested` for the selected row with focus_viewer.
#[gpui_kit::test]
fn enter_opens_the_selected_row(cx: &mut TestAppContext) {
    let world = corpus(6);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let events = f.record_events(cx);
    f.press(cx, "down down");
    let target = f.read(cx, |list, _| list.selected_target()).unwrap();
    events.clear();
    f.press(cx, "enter");
    assert_eq!(
        events.all(),
        vec![ListPaneEvent::OpenRequested {
            target,
            focus_viewer: true,
            agent_tab: None
        }]
    );
}

/// L-059, L-060: caret and emacs editing keys in the search input.
#[gpui_kit::test]
fn emacs_editing_keys_work_in_the_search_input(cx: &mut TestAppContext) {
    let world = corpus(4);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.type_text(cx, "foo bar");
    f.press(cx, "ctrl-left");
    assert_eq!(cursor(&f, cx), 4, "ctrl-left from the end of `foo bar`");
    f.press(cx, "left");
    assert_eq!(cursor(&f, cx), 3);
    f.press(cx, "escape");

    let query = |f: &ListFixture, cx: &TestAppContext| f.read(cx, |l, cx| l.query(cx)).to_string();
    let reset = |f: &ListFixture, cx: &mut TestAppContext| {
        f.press(cx, "escape");
        f.type_text(cx, "foo bar baz");
        put_caret(f, cx, 7);
    };
    reset(&f, cx);
    f.press(cx, "ctrl-k");
    assert_eq!(query(&f, cx), "foo bar");
    reset(&f, cx);
    f.press(cx, "ctrl-u");
    assert_eq!(query(&f, cx), " baz");
    reset(&f, cx);
    f.press(cx, "ctrl-w");
    assert_eq!(query(&f, cx), "foo  baz");
    reset(&f, cx);
    f.press(cx, "ctrl-e");
    assert_eq!(cursor(&f, cx), 11);
    f.press(cx, "alt-b");
    assert_eq!(cursor(&f, cx), 8, "alt-b moves to the start of `baz`");
    f.press(cx, "alt-f");
    assert_eq!(cursor(&f, cx), 11);
    f.press(cx, "ctrl-b");
    assert_eq!(cursor(&f, cx), 10);
}

/// L-060: with Fork rebound, Ctrl+F moves the caret right by one.
#[gpui_kit::test]
fn ctrl_f_moves_right_when_fork_is_rebound(cx: &mut TestAppContext) {
    let world = corpus(4);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    cx.update(|cx| {
        let keys = KeyBindings {
            fork: parse_key_binding("alt+f").unwrap(),
            ..KeyBindings::default()
        };
        keymap::install_user_bindings(cx, &keys).unwrap();
    });
    cx.run_until_parked();
    f.type_text(cx, "foo bar baz");
    put_caret(&f, cx, 2);
    f.press(cx, "ctrl-f");
    assert_eq!(cursor(&f, cx), 3);
}

/// L-031, L-046, C-003, L-044: `-L` starts with the workspace scope showing
/// only the project and its worktree; Tab toggles the scope with a workspace.
#[gpui_kit::test]
fn scope_toggles_with_tab_and_a_workspace(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (proj, _worktree, _api) = workspace_corpus(&world);
    let f = ListFixture::open_loaded(
        cx,
        &world,
        &["--workspace", &proj.display().to_string(), "-L"],
    );
    assert!(f.read(cx, |list, _| list.has_workspace()));
    assert!(f.read(cx, |list, _| list.scope_is_workspace()), "C-003");
    assert_eq!(f.read(cx, |list, _| list.prompt_text()), "Project ❯");
    f.wait_idle(cx);
    let titles = f.titles(cx);
    assert_eq!(titles.len(), 4, "{titles:?}: proj and its worktree only");
    assert!(
        titles
            .iter()
            .all(|t| t.starts_with("proj") || t.starts_with("worktree"))
    );
    assert_eq!(
        f.read(cx, |list, _| list.toolbar_model().workspace_label),
        "proj"
    );
    f.press(cx, "tab");
    f.wait_idle(cx);
    assert!(!f.read(cx, |list, _| list.scope_is_workspace()));
    assert_eq!(f.paths(cx).len(), 6);
    f.press(cx, "tab");
    f.wait_idle(cx);
    assert!(f.read(cx, |list, _| list.scope_is_workspace()));
    assert_eq!(f.titles(cx), titles);
}

/// L-046: without a workspace Tab does not toggle the scope and moves focus on.
#[gpui_kit::test]
fn tab_without_a_workspace_moves_focus_on(cx: &mut TestAppContext) {
    let world = corpus(5);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    assert!(!f.read(cx, |list, _| list.has_workspace()));
    f.press(cx, "tab");
    assert!(!f.read(cx, |list, _| list.scope_is_workspace()));
    let on_toolbar = cx
        .update_window(f.handle, |_, window, cx| {
            f.list.read(cx).toolbar_contains_focus(window, cx)
        })
        .unwrap();
    assert!(on_toolbar, "focus moved to the toolbar");
}

/// L-042: `[tui] exclude_projects` hides the exact project and its worktrees.
#[gpui_kit::test]
fn excluded_projects_are_hidden(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    world.write_config(None, Some("[tui]\nexclude_projects = [\"proj\"]\n"));
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let names: Vec<Option<String>> = f.read(cx, |list, _| {
        (0..list.browser().filtered().len())
            .map(|pos| list.browser().row(pos).unwrap().project_name.clone())
            .collect()
    });
    assert!(
        !names.iter().any(|name| name
            .as_deref()
            .is_some_and(|n| n == "proj" || n.starts_with("proj/"))),
        "{names:?}"
    );
    assert!(names.len() < 8 && !names.is_empty());
    assert!(
        names.iter().flatten().any(|name| name == "api"),
        "other projects stay listed: {names:?}"
    );
}

/// L-023: the preview shows the last messages first, or the first with `--first`.
#[gpui_kit::test]
fn previews_follow_first_and_last(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    crate::test_support::fixtures_list::SessionSpec::new("proj", 1)
        .messages(&[
            ("user", "opening question"),
            ("assistant", "middle answer"),
            ("user", "closing question"),
            ("assistant", "closing answer"),
        ])
        .write(&world);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let row = f.read(cx, |list, _| list.row_models(0..1, 480.0)).remove(0);
    assert!(
        row.preview.text.starts_with("closing"),
        "{}",
        row.preview.text
    );

    let mut cx2 = cx.new_app();
    let f2 = ListFixture::open_loaded(&mut cx2, &world, &["--first"]);
    let row = f2
        .read(&cx2, |list, _| list.row_models(0..1, 480.0))
        .remove(0);
    assert!(
        row.preview.text.starts_with("opening"),
        "{}",
        row.preview.text
    );
}

/// Mixed corpora: lanes.
#[gpui_kit::test]
fn mixed_corpora_have_lanes(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let labels: Vec<_> = f
        .read(cx, |list, _| list.row_models(0..8, 480.0))
        .into_iter()
        .filter_map(|row| row.source_label)
        .collect();
    assert!(labels.contains(&"CC") && labels.contains(&"Pi") && labels.contains(&"OMP"));
}

/// 17.3 (preview-related events): keyboard navigation emits user selections,
/// a new result set resets the selection with a non-user event.
#[gpui_kit::test]
fn selection_events_flag_their_cause(cx: &mut TestAppContext) {
    let world = corpus(20);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let paths = f.paths(cx);
    let events = f.record_events(cx);
    f.press(cx, "down");
    let selections = events.selections();
    assert_eq!(selections.len(), 1);
    assert!(selections[0].1, "keyboard navigation is a user selection");
    assert_eq!(
        selections[0].0.as_ref().map(|t| t.path.clone()),
        Some(paths[1].clone())
    );

    events.clear();
    f.type_text(cx, "synthetic");
    f.wait_idle(cx);
    let selections = events.selections();
    let last = selections
        .last()
        .expect("the result set reset the selection");
    assert!(!last.1, "an automatic selection is not a user selection");
    assert_eq!(f.selected_position(cx), Some(0));
    assert_eq!(last.0.as_ref().map(|t| t.path.clone()), f.selected_path(cx));
}

/// L-067, L-080: results that reset the selection to row 0 scroll it into
/// view (the selection was at the end of 200 rows, the new results are the
/// same 200 rows).
#[gpui_kit::test]
fn new_results_scroll_the_selection_into_view(cx: &mut TestAppContext) {
    let world = corpus(200);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    f.press(cx, "end");
    f.type_text(cx, "synthetic");
    f.wait_idle(cx);
    assert_eq!(f.selected_position(cx), Some(0));
    let first = f.paths(cx)[0].clone();
    cx.update_window(f.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(
            window.try_find(row_id(&first)).is_some(),
            "the selected row is painted"
        );
    })
    .unwrap();
}

/// L-065: a click while focus is outside the list (after Enter or a double
/// click the viewer holds it) gives the focus back to the search input, so
/// the Workspace previews the selection.
#[gpui_kit::test]
fn a_row_click_takes_the_focus_back(cx: &mut TestAppContext) {
    let world = corpus(12);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let paths = f.paths(cx);
    cx.update_window(f.handle, |_, window, cx| {
        window.blur(cx);
        window.render_frame(cx);
        assert!(!f.list.focus_handle(cx).is_focused(window));
        window.click(row_id(&paths[4]), cx);
        assert!(f.list.focus_handle(cx).is_focused(window));
        window.blur(cx);
        window.right_click(row_id(&paths[2]), cx);
        assert!(
            f.list.focus_handle(cx).is_focused(window),
            "RowMenu is dispatched along the focused path"
        );
    })
    .unwrap();
}
