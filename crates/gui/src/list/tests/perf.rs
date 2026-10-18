//! The 20k-row performance probes of DESIGN §15.8 as an ignored test (the Q
//! scenario `perf-list.qa` needs the QA runner, WP7). Run in release:
//!
//! ```text
//! cargo test --release -p claude-history-gui --lib list::tests::perf -- --ignored --nocapture
//! ```
//!
//! It prints the measurements and asserts the §15.8 list targets (UI-thread
//! time per handler). The structural preconditions (nothing O(corpus) on the
//! UI thread) are asserted by the regular tests (`uuid_jump_*`, K1 paths).

use std::time::Instant;

use gpui_kit::AppContext as _;
use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt as _;

use crate::test_support::fixtures_list::ListFixture;
use crate::test_support::{TestWorld, synth};

const ROWS: usize = 20_000;

fn probe(f: &ListFixture, cx: &TestAppContext, key: &str) -> f64 {
    f.read(cx, |list, cx| list.qa_probe(key, cx))
        .and_then(|value| value.parse().ok())
        .unwrap_or(f64::NAN)
}

#[gpui_kit::test]
#[ignore = "20k rows; run in release with --ignored"]
fn perf_20k_rows(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let started = Instant::now();
    synth::corpus(&world, ROWS);
    eprintln!("wrote {ROWS} sessions in {:.1?}", started.elapsed());

    let f = ListFixture::open(cx, &world, &[]);
    let load_started = Instant::now();
    f.wait_loaded(cx);
    eprintln!("loaded in {:.1?}", load_started.elapsed());
    assert_eq!(f.paths(cx).len(), ROWS);
    let install = probe(&f, cx, "list.poll_busy_max");
    eprintln!("load: slowest poll (batch append or install) {install:.3} ms");

    eprintln!(
        "load: slowest install poll {:.3} ms, of which core's loader poll {:.3} ms",
        probe(&f, cx, "list.install_busy_max"),
        probe(&f, cx, "list.loader_install_max")
    );

    // Typing: every key.
    for ch in "synthetic conversation 19".chars() {
        let key = if ch == ' ' {
            "space".to_string()
        } else {
            ch.to_string()
        };
        cx.dispatch_keystroke(f.handle, gpui_kit::Keystroke::parse(&key).unwrap());
        eprintln!("  key {ch:?}: {:.3} ms", probe(&f, cx, "list.ui_busy_last"));
        f.wait_idle(cx);
    }
    let per_key = probe(&f, cx, "list.ui_busy_max");
    eprintln!("typing: slowest key handler {per_key:.3} ms");

    // A pasted UUID: pending lookup, no disk on the UI thread.
    crate::test_support::recording(cx).set_clipboard(Some(synth::uuid(1_007)));
    f.press(cx, "escape");
    f.press(cx, "secondary-v");
    f.wait_idle(cx);
    let paste = probe(&f, cx, "list.ui_busy_max");
    eprintln!("paste: slowest key/paste handler {paste:.3} ms");
    f.press(cx, "escape");
    f.wait_idle(cx);

    // Scroll through the list: build frames while moving the selection.
    for _ in 0..200 {
        f.press(cx, "pagedown");
        cx.update_window(f.handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
    }
    let frame_p95 = probe(&f, cx, "list.frame_p95");
    let frame_max = probe(&f, cx, "list.frame_max");
    eprintln!("frames: p95 {frame_p95:.3} ms, max {frame_max:.3} ms (render + visible rows)");

    // Refresh end: install of the prepared corpus.
    f.with_window(cx, |list, _, cx| list.refresh(cx));
    f.wait_loaded(cx);
    eprintln!(
        "refresh: slowest install poll {:.3} ms, of which core's loader poll {:.3} ms",
        probe(&f, cx, "list.install_busy_max"),
        probe(&f, cx, "list.loader_install_max")
    );

    // The core calls alone (what the pane's methods spend inside `Browser`).
    {
        let probe_target = f
            .read(cx, |list, _| list.conversation(&list.filtered_paths()[3]))
            .unwrap();
        let mut probe_renamed = (*probe_target).clone();
        probe_renamed.custom_title = Some("Core rename".to_string());
        let probe_path = probe_target.path.clone();
        let victim = f.paths(cx)[12].clone();
        let (core_rename, core_remove) = f.with_window(cx, |list, _, _| {
            let t = Instant::now();
            list.browser
                .apply_renamed(&probe_path, probe_renamed)
                .unwrap();
            let core_rename = t.elapsed();
            let t = Instant::now();
            list.browser.remove_paths(std::slice::from_ref(&victim));
            (core_rename, t.elapsed())
        });
        eprintln!("core alone: apply_renamed {core_rename:.3?}, remove_paths {core_remove:.3?}");
    }

    // Rename apply and delete removal: one element each.
    let target = f
        .read(cx, |list, _| list.conversation(&list.filtered_paths()[5]))
        .unwrap();
    let mut renamed = (*target).clone();
    renamed.custom_title = Some("Renamed".to_string());
    let path = target.path.clone();
    let t = Instant::now();
    f.with_window(cx, |list, _, cx| {
        list.apply_renamed(&path, renamed, cx).unwrap();
    });
    let rename = t.elapsed();
    let rename_inside = probe(&f, cx, "list.edit_busy_last");
    let victim = f.paths(cx)[10].clone();
    let t = Instant::now();
    f.with_window(cx, |list, _, cx| {
        list.remove_paths(std::slice::from_ref(&victim), cx)
    });
    let remove = t.elapsed();
    let remove_inside = probe(&f, cx, "list.edit_busy_last");
    eprintln!(
        "rename apply {rename:.3?} (pane {rename_inside:.3} ms, rest = event subscribers), \
         delete removal {remove:.3?} (pane {remove_inside:.3} ms)"
    );

    // Within the pane's control: asserted.
    assert!(per_key <= 2.0, "per typed key {per_key} ms > 2 ms");
    // Measured 0.8-2.2 ms across runs (target 2 ms; a linear scan of 20k
    // session ids inside core): asserted at the 2 ms target (X-009).
    assert!(paste <= 2.0, "paste {paste} ms > 2 ms");
    assert!(frame_p95 <= 16.0, "frame p95 {frame_p95} ms > 16 ms");
    // Spent inside core (CCR in the hand-off): reported, not asserted.
    let install = probe(&f, cx, "list.loader_install_max");
    for (what, took, limit) in [
        ("install of a load/refresh", install, 16.0),
        ("rename apply", rename.as_secs_f64() * 1000.0, 5.0),
        ("delete removal", remove.as_secs_f64() * 1000.0, 5.0),
    ] {
        if took > limit {
            eprintln!("TARGET MISSED: {what} {took:.1} ms > {limit} ms");
        }
    }
}
