//! Tests of `title_bar.rs` (DESIGN §17.5, PD1): window titles and the title
//! bar of both window roots.

use gpui_kit::TestAppContext;

use super::*;
use crate::test_support::{TestWorld, open_main, open_single_file};

#[test]
fn single_file_titles_put_the_file_first() {
    assert_eq!(
        single_file_title("session.jsonl"),
        "session.jsonl — Claude History"
    );
}

/// PD1: the title names the scope (`All projects` or the workspace's short
/// name) and marks picker mode.
#[gpui_kit::test]
fn the_main_title_names_the_scope(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    assert_eq!(cx.read(window_title), "Claude History — All projects");
}

#[gpui_kit::test]
fn the_title_names_the_workspace(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let dir = world.home().join("my-project");
    std::fs::create_dir_all(&dir).unwrap();
    let (_handle, _ws) = open_main(cx, &world, &["--workspace", &dir.display().to_string()]);
    assert_eq!(cx.read(window_title), "Claude History — my-project");
}

#[gpui_kit::test]
fn picker_mode_is_marked_in_the_title(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &["--show-path"]);
    assert_eq!(
        cx.read(window_title),
        "Claude History — All projects — Select"
    );
}

/// Both window roots render the app title bar (with the menu bar on
/// Windows/Linux) without panicking.
#[gpui_kit::test]
fn both_roots_render_the_title_bar(cx: &mut TestAppContext) {
    use gpui_kit::AppContext as _;
    use gpui_kit::test::TestWindowExt as _;
    let world = TestWorld::new();
    let path = world.add_claude_session(
        "-synthetic-proj",
        &crate::test_support::synth::uuid(3),
        &[
            r#"{"type":"user","sessionId":"x","timestamp":"2026-09-01T10:00:00Z","message":{"role":"user","content":"hello"}}"#,
        ],
    );
    let (main, _ws) = open_main(cx, &world, &[]);
    let single = open_single_file(cx, &world, &path);
    for handle in [main, single] {
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
    }
}
