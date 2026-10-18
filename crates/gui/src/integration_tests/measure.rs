//! Measurements and live text: the X-038 UI-thread spans, the X-007 frame
//! counter, the Debug log keeping its scroll position (WP5 request).

use claude_history_core::debug::DebugLevel;
use gpui_kit::{AppContext as _, TestAppContext, point, px};

use super::{has_dialog, render, settle, wait_for};
use crate::globals::{BusySpan, UiBusy, debug_buffer};
use crate::model::DebugLogTab;
use crate::ops::rename::LastRename;
use crate::test_support::fixtures_ops::{OpsFixture, claude_session};
use crate::test_support::{TestWorld, global_lock};

fn record(cx: &TestAppContext, span: BusySpan) -> Option<crate::globals::BusyRecord> {
    cx.read(|cx| UiBusy::of(cx, span))
}

/// X-038 (structural): the load end, a refresh end, a rename apply and a
/// delete removal are each timed once, around the whole update that ran
/// them (the probes `busy.*` of cross-pane.qa read these).
#[gpui_kit::test]
fn x038_the_ui_spans_are_measured_end_to_end(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Measured");
    let doomed = claude_session(&world, "proj", 2, "Measured too");
    let f = OpsFixture::open(cx, &world, &[]);
    let handle = f.handle();
    let load = record(cx, BusySpan::LoadEnd).expect("the load end was measured");
    assert_eq!(load.count, 1);
    assert!(record(cx, BusySpan::RefreshEnd).is_none());

    // Refresh (F5).
    f.focus_list(cx);
    f.press(cx, "f5");
    wait_for(cx, "the refresh span", |cx| {
        UiBusy::of(cx, BusySpan::RefreshEnd).is_some()
    });
    f.list.wait_idle(cx);
    assert_eq!(record(cx, BusySpan::RefreshEnd).unwrap().count, 1);
    assert_eq!(
        record(cx, BusySpan::LoadEnd).unwrap().count,
        1,
        "a refresh is not a load"
    );

    // Rename apply.
    f.select(cx, &path);
    f.press(cx, "f2");
    settle(cx);
    let dialog = cx
        .read(|cx| {
            cx.try_global::<LastRename>()
                .and_then(|last| last.0.upgrade())
        })
        .expect("the rename dialog");
    cx.update_window(handle, |_, window, cx| {
        let input = dialog.read(cx).input();
        input.update(cx, |input, cx| {
            input.set_value("Measured again", window, cx)
        });
        dialog.update(cx, |dialog, cx| dialog.save(window, cx));
    })
    .unwrap();
    wait_for(cx, "the rename span", |cx| {
        UiBusy::of(cx, BusySpan::RenameApply).is_some()
    });

    // Delete removal.
    f.select(cx, &doomed);
    f.press(cx, "ctrl-x");
    for _ in 0..100 {
        settle(cx);
        render(cx, handle);
        if has_dialog(cx, handle) {
            break;
        }
    }
    f.press(cx, "y");
    wait_for(cx, "the delete span", |cx| {
        UiBusy::of(cx, BusySpan::DeleteRemoval).is_some()
    });
    for span in [
        BusySpan::LoadEnd,
        BusySpan::RefreshEnd,
        BusySpan::RenameApply,
        BusySpan::DeleteRemoval,
    ] {
        let record = record(cx, span).unwrap();
        assert!(record.max >= record.last && record.count >= 1, "{span:?}");
        // Debug build, test platform: generous, the release targets are QA's.
        assert!(record.max.as_secs_f64() < 1.0, "{span:?}: {:?}", record.max);
    }
    assert!(record(cx, BusySpan::Paste).is_none(), "QA records pastes");
}

/// X-007 (structural): the main window counts its frames and their UI time;
/// nothing is drawn while nothing changes.
#[gpui_kit::test]
fn x007_the_main_window_counts_its_frames(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    claude_session(&world, "proj", 1, "Frames");
    let f = OpsFixture::open(cx, &world, &[]);
    let handle = f.handle();
    let ws = f.ws().clone();
    let stats = |cx: &TestAppContext| {
        cx.read(|cx| {
            let stats = ws.read(cx).frame_stats();
            (stats.frames(), stats.total())
        })
    };
    render(cx, handle);
    let (before, time_before) = stats(cx);
    render(cx, handle);
    render(cx, handle);
    let (after, time_after) = stats(cx);
    assert_eq!(after, before + 2, "one count per drawn frame");
    assert!(time_after >= time_before);
    // Idle: parked with nothing to do draws nothing.
    settle(cx);
    let (idle, _) = stats(cx);
    settle(cx);
    assert_eq!(stats(cx).0, idle, "no frame while idle");
    // The ticker's count is a probe of its own.
    assert_eq!(
        super::probe(cx, handle, &ws, "list.ticks"),
        "0",
        "the 30 s ticker has not run"
    );
}

/// X-007: the 30 s ticker repaints only when a painted row's time label
/// changes — `just now` rows turn into `1 min ago` at the second tick; a tick
/// that changes nothing draws nothing.
#[gpui_kit::test]
fn x007_the_ticker_repaints_only_when_a_label_changes(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    crate::test_support::fixtures_list::listed_corpus(&world);
    let f = crate::test_support::fixtures_list::ListFixture::open_loaded(cx, &world, &[]);
    render(cx, f.handle);
    let probe =
        |cx: &TestAppContext, key: &str| f.read(cx, |list, cx| list.qa_probe(key, cx)).unwrap();
    let label = |cx: &TestAppContext| f.titles(cx).len();
    assert!(label(cx) > 0);
    assert_eq!(probe(cx, "list.ticks"), "0");
    cx.executor().advance_clock(crate::list::TICK_INTERVAL);
    cx.run_until_parked();
    assert_eq!(probe(cx, "list.ticks"), "1");
    assert_eq!(probe(cx, "list.tick_repaints"), "0", "still `just now`");
    cx.executor().advance_clock(crate::list::TICK_INTERVAL);
    cx.run_until_parked();
    assert_eq!(probe(cx, "list.ticks"), "2");
    assert_eq!(probe(cx, "list.tick_repaints"), "1", "`1 min ago` now");
}

/// WP5 request (T-022): the live Debug log appends new lines without moving
/// the view — the scroll position and the selection stay.
#[gpui_kit::test]
fn the_live_debug_log_keeps_scroll_and_selection(cx: &mut TestAppContext) {
    let _lock = global_lock();
    let world = TestWorld::new();
    claude_session(&world, "proj", 1, "Debug log");
    let f = OpsFixture::open(cx, &world, &[]);
    let handle = f.handle();
    let buffer = cx.read(debug_buffer);
    buffer.set_min_level(DebugLevel::Debug);
    for n in 0..200 {
        buffer.push(DebugLevel::Info, format!("line {n} of the live log"));
    }
    cx.update_window(handle, |_, window, cx| {
        crate::tools::open_debug_log(DebugLogTab::Messages, window, cx)
    })
    .unwrap();
    settle(cx);
    render(cx, handle);
    let dialog = cx
        .read(crate::tools::debug_log::current_dialog)
        .expect("the Debug log");
    let output = cx.read(|cx| dialog.read(cx).output().clone());
    assert!(
        cx.read(|cx| output.text(cx))
            .contains("line 199 of the live log")
    );

    // Scroll down and select some text.
    cx.update(|cx| {
        output.state().update(cx, |state, cx| {
            state.set_scroll_offset(point(px(0.), px(-600.)), cx);
            state.set_selected_range(40..60, cx);
            state.set_scroll_offset(point(px(0.), px(-600.)), cx);
        })
    });
    render(cx, handle);
    render(cx, handle);
    let scroll = cx.read(|cx| output.scroll_offset(cx));
    assert!(f32::from(scroll.y) < -100.0, "scrolled: {scroll:?}");
    let selection = cx.read(|cx| output.selected_range(cx));
    assert_eq!(selection, 40..60);

    // New lines arrive; the 1 s tick appends them.
    for n in 200..230 {
        buffer.push(DebugLevel::Warn, format!("line {n} of the live log"));
    }
    cx.update_window(handle, |_, window, cx| {
        dialog.update(cx, |dialog, cx| dialog.tick(window, cx))
    })
    .unwrap();
    render(cx, handle);
    render(cx, handle);
    let text = cx.read(|cx| output.text(cx).to_string());
    assert!(text.contains("line 229 of the live log"), "appended");
    assert!(text.contains("line 0 of the live log"), "nothing replaced");
    assert_eq!(
        cx.read(|cx| output.selected_range(cx)),
        40..60,
        "selection kept"
    );
    assert_eq!(
        cx.read(|cx| output.scroll_offset(cx)),
        scroll,
        "scroll kept"
    );

    // A different text (the ring dropped old lines, the level changed)
    // replaces it but keeps the view where it fits.
    cx.update_window(handle, |_, window, cx| {
        output.update_text("a short\nreplacement", window, cx)
    })
    .unwrap();
    render(cx, handle);
    assert_eq!(
        cx.read(|cx| output.text(cx)).as_ref(),
        "a short\nreplacement"
    );
    let clamped = cx.read(|cx| output.selected_range(cx));
    assert!(clamped.end <= "a short\nreplacement".len());
    // An explicit tab switch starts at the top again.
    cx.update_window(handle, |_, window, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.set_tab(DebugLogTab::Messages, window, cx)
        })
    })
    .unwrap();
    render(cx, handle);
    render(cx, handle);
    assert_eq!(f32::from(cx.read(|cx| output.scroll_offset(cx)).y), 0.0);
}
