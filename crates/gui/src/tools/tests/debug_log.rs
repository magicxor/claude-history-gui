//! G tests of the Debug log dialog (T-015, T-022).

use std::path::Path;

use claude_history_core::debug::DebugLevel;
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext};

use super::main_window;
use crate::globals::debug_buffer;
use crate::model::{ConversationTarget, DebugLogTab, OpenRequest};
use crate::services::ServiceCall;
use crate::test_support::fixtures_list::ListFixture;
use crate::test_support::fixtures_tools::{settle, wait_for};
use crate::test_support::{TestWorld, global_lock, recording};
use crate::tools::debug_log::{
    DebugLogDialog, ParseTab, current_dialog, open_debug_log, tail_lines,
};

fn open(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    tab: DebugLogTab,
) -> Entity<DebugLogDialog> {
    cx.update_window(handle, |_, window, cx| open_debug_log(tab, window, cx))
        .unwrap();
    settle(cx, handle);
    cx.read(current_dialog).expect("the Debug log is open")
}

fn show_in_viewer(cx: &mut TestAppContext, f: &ListFixture, path: &Path) {
    let viewer = cx.read(|cx| f.ws.read(cx).viewer().clone());
    let target = ConversationTarget::from_parsed_file(path, None);
    cx.update_window(f.handle, |_, window, cx| {
        viewer.update(cx, |viewer, cx| {
            viewer.open(OpenRequest::new(target, true), window, cx)
        })
    })
    .unwrap();
    cx.run_until_parked();
}

/// T-015: the Parse errors tab shows the viewer's conversation and Write to
/// debug.log appends core's block.
#[gpui_kit::test]
fn parse_errors_are_shown_and_written_to_debug_log(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = world.standard_corpus();
    let broken = corpus[7].clone();
    let f = ListFixture::open_loaded(cx, &world, &[]);
    show_in_viewer(cx, &f, &broken);
    let dialog = open(cx, f.handle, DebugLogTab::ParseErrors);
    wait_for(cx, "the parse", |cx| {
        matches!(dialog.read(cx).parse_tab(), ParseTab::Errors(_))
    });
    let text = cx.read(|cx| dialog.read(cx).text(cx));
    assert!(
        text.contains(&format!("File: {}", broken.display())),
        "{text}"
    );
    assert!(
        text.contains("Errors: 1") && text.contains("Line 2:"),
        "{text}"
    );
    assert!(text.contains("{this is not json"), "{text}");

    let log = world.env.debug_log_file().expect("debug log path");
    assert!(!log.exists());
    cx.update_window(f.handle, |_, window, cx| {
        dialog.update(cx, |d, cx| d.write_parse_errors(window, cx))
    })
    .unwrap();
    wait_for(cx, "the write", |cx| dialog.read(cx).status().is_some());
    let written = std::fs::read_to_string(&log).expect("debug.log was written");
    assert!(written.contains("=== Parse Errors:"), "{written}");
    let status = cx
        .read(|cx| dialog.read(cx).status().map(str::to_string))
        .unwrap();
    assert!(
        status.starts_with("Logged 1 parse error(s) to "),
        "{status}"
    );
}

/// Without an open conversation the Parse errors tab says so.
#[gpui_kit::test]
fn parse_errors_without_a_conversation(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_f, handle) = main_window(cx, &world, &[]);
    let dialog = open(cx, handle, DebugLogTab::ParseErrors);
    assert_eq!(
        cx.read(|cx| dialog.read(cx).parse_tab().clone()),
        ParseTab::NoConversation
    );
    assert_eq!(
        cx.read(|cx| dialog.read(cx).text(cx)),
        "No conversation is open."
    );
}

/// Opening the Messages tab with a conversation in the viewer does not show
/// the Parse errors tab's loading text (the parse runs in the background).
#[gpui_kit::test]
fn the_messages_tab_is_not_the_parse_loading_text(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = world.standard_corpus();
    let f = ListFixture::open_loaded(cx, &world, &[]);
    show_in_viewer(cx, &f, &corpus[7]);
    let dialog = open(cx, f.handle, DebugLogTab::Messages);
    wait_for(cx, "the parse", |cx| {
        matches!(dialog.read(cx).parse_tab(), ParseTab::Errors(_))
    });
    let text = cx.read(|cx| dialog.read(cx).text(cx));
    assert!(!text.contains("Reading the conversation"), "{text}");
    // Switching to the tab shows the finished report at once.
    cx.update_window(f.handle, |_, window, cx| {
        dialog.update(cx, |d, cx| d.set_tab(DebugLogTab::ParseErrors, window, cx))
    })
    .unwrap();
    let text = cx.read(|cx| dialog.read(cx).text(cx));
    assert!(text.contains("Errors: 1"), "{text}");
}

/// A conversation without errors says so.
#[gpui_kit::test]
fn a_clean_conversation_has_no_parse_errors(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let corpus = world.standard_corpus();
    let f = ListFixture::open_loaded(cx, &world, &[]);
    show_in_viewer(cx, &f, &corpus[0]);
    let dialog = open(cx, f.handle, DebugLogTab::ParseErrors);
    wait_for(cx, "the parse", |cx| {
        matches!(dialog.read(cx).parse_tab(), ParseTab::Clean(_))
    });
    let text = cx.read(|cx| dialog.read(cx).text(cx));
    assert!(text.starts_with("No parse errors in "), "{text}");
}

/// T-015: the level selector filters the Messages tab.
#[gpui_kit::test]
fn the_level_filters_the_messages(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_f, handle) = main_window(cx, &world, &[]);
    cx.update(|cx| {
        let buffer = debug_buffer(cx);
        buffer.clear();
        buffer.push(DebugLevel::Debug, "a debug line");
        buffer.push(DebugLevel::Info, "an info line");
        buffer.push(DebugLevel::Warn, "a warning line");
    });
    let dialog = open(cx, handle, DebugLogTab::Messages);
    // Default level: Info.
    let text = cx.read(|cx| dialog.read(cx).text(cx));
    assert!(!text.contains("a debug line"), "{text}");
    assert!(
        text.contains("[INFO] an info line") && text.contains("[WARN] a warning line"),
        "{text}"
    );
    cx.update_window(handle, |_, window, cx| {
        dialog.update(cx, |d, cx| d.set_level(DebugLevel::Debug, window, cx))
    })
    .unwrap();
    let text = cx.read(|cx| dialog.read(cx).text(cx));
    assert!(text.contains("[DEBUG] a debug line"), "{text}");
    cx.update_window(handle, |_, window, cx| {
        dialog.update(cx, |d, cx| d.set_level(DebugLevel::Warn, window, cx))
    })
    .unwrap();
    let text = cx.read(|cx| dialog.read(cx).text(cx));
    assert!(
        !text.contains("an info line") && text.contains("a warning line"),
        "{text}"
    );
    // Clear empties the buffer.
    cx.update_window(handle, |_, window, cx| {
        dialog.update(cx, |d, cx| d.clear_messages(window, cx))
    })
    .unwrap();
    assert_eq!(cx.read(|cx| dialog.read(cx).text(cx)), "");
    cx.update(|cx| debug_buffer(cx).set_min_level(DebugLevel::Info));
}

/// T-022: Debug in the dialog applies to the next load.
#[gpui_kit::test]
fn the_level_applies_to_the_next_load(cx: &mut TestAppContext) {
    let _lock = global_lock();
    let world = TestWorld::new();
    world.standard_corpus();
    let f = ListFixture::open_loaded(cx, &world, &[]);
    cx.update(|cx| crate::debug_sink::install(cx, None));
    cx.update(|cx| debug_buffer(cx).clear());
    let dialog = open(cx, f.handle, DebugLogTab::Messages);
    cx.update_window(f.handle, |_, window, cx| {
        dialog.update(cx, |d, cx| d.set_level(DebugLevel::Debug, window, cx))
    })
    .unwrap();
    assert_eq!(cx.read(crate::debug_sink::capture_level), DebugLevel::Debug);
    // The Tools' own fresh loads (here: the Debug search corpus) pass it on.
    let project = world.projects_root().join("agent-project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("agent-x.jsonl"), "{}\n").unwrap();
    cx.update_window(f.handle, |_, window, cx| {
        crate::tools::open_debug_search(None, false, window, cx)
    })
    .unwrap();
    wait_for(cx, "the corpus", |cx| {
        crate::tools::debug_search::current_dialog(cx).is_some_and(|d| {
            matches!(
                d.read(cx).corpus_state(),
                crate::tools::debug_search::CorpusState::Ready(_)
            )
        })
    });
    let debug_lines = cx.read(|cx| {
        debug_buffer(cx)
            .lines()
            .iter()
            .filter(|line| line.level == DebugLevel::Debug)
            .count()
    });
    assert!(
        debug_lines > 0,
        "the refresh captured Debug-level loader lines"
    );
    cx.update(|cx| {
        debug_buffer(cx).set_min_level(DebugLevel::Info);
    });
}

/// The debug.log tab shows the last 2,000 lines; Open file and Reveal go
/// through the services.
#[gpui_kit::test]
fn the_file_tab_shows_the_tail(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let log = world.env.debug_log_file().expect("debug log path");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    let body: String = (0..2500).map(|n| format!("line {n}\n")).collect();
    std::fs::write(&log, body).unwrap();
    let (_f, handle) = main_window(cx, &world, &[]);
    let dialog = open(cx, handle, DebugLogTab::File);
    wait_for(cx, "the tail", |cx| {
        dialog.read(cx).text(cx).starts_with("line 500")
    });
    let text = cx.read(|cx| dialog.read(cx).text(cx));
    assert_eq!(text.lines().count(), 2000);
    assert!(text.ends_with("line 2499"));
    cx.update(|cx| {
        dialog.update(cx, |d, cx| {
            d.open_file(cx);
            d.reveal_file(cx);
        })
    });
    let calls = recording(cx).calls();
    assert!(
        calls.contains(&ServiceCall::OpenWithSystem(log.clone())),
        "{calls:?}"
    );
    assert!(calls.contains(&ServiceCall::Reveal(log)), "{calls:?}");
}

#[gpui_kit::test]
fn a_missing_debug_log_says_so(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_f, handle) = main_window(cx, &world, &[]);
    let dialog = open(cx, handle, DebugLogTab::File);
    wait_for(cx, "the tab", |cx| {
        dialog.read(cx).text(cx) == "debug.log does not exist yet."
    });
}

#[test]
fn tail_lines_keeps_the_last_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.log");
    std::fs::write(&path, "a\nb\nc\nd\n").unwrap();
    assert_eq!(tail_lines(&path, 2).as_deref(), Some("c\nd"));
    assert_eq!(tail_lines(&path, 10).as_deref(), Some("a\nb\nc\nd"));
    assert_eq!(tail_lines(&dir.path().join("none.log"), 2), None);
}
