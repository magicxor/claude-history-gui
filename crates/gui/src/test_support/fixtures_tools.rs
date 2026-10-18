//! Tools fixtures (DESIGN §6, §15.2): empty transcripts, two-copy sessions,
//! hashes of the Pi/OMP trees and small wait helpers. Owned by WP5.
//!
//! Builders use `super::TestWorld` and `super::synth`; GUI-only fixture
//! files go into `crates/gui/tests/fixtures/tools/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, App, AppContext as _, TestAppContext};

use super::{TestWorld, synth};

/// A Claude transcript with one user message and no assistant reply
/// (`delete-empty` lists it). `text = None` writes no user message at all.
pub fn write_empty_transcript(
    world: &TestWorld,
    cwd: &Path,
    number: u32,
    text: Option<&str>,
) -> PathBuf {
    let uuid = synth::uuid(number);
    let cwd_json = serde_json::to_string(&cwd.display().to_string()).expect("json");
    let mut lines = Vec::new();
    if let Some(text) = text {
        let text = serde_json::to_string(text).expect("json");
        lines.push(format!(
            r#"{{"type":"user","sessionId":"{uuid}","cwd":{cwd_json},"timestamp":"2026-09-01T10:00:00Z","message":{{"role":"user","content":{text}}}}}"#
        ));
    }
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    world.add_claude_session(&synth::encoded_project_dir(cwd), &uuid, &refs)
}

/// Make `path` look `days` old.
pub fn age_file(path: &Path, days: i64) {
    super::fixtures_list::set_mtime(path, chrono::Utc::now() - chrono::Duration::days(days));
}

/// Two copies of one session in two projects, the first with a session
/// directory holding a tool result. Returns `(first, second, session_dir)`.
pub fn two_copy_session(world: &TestWorld, number: u32) -> (PathBuf, PathBuf, PathBuf) {
    let uuid = synth::uuid(number);
    let base = synth::cwd_base();
    let mut paths = Vec::new();
    for project in ["copy-a", "copy-b"] {
        let cwd = base.join(project);
        let lines = synth::claude_conversation(&uuid, &cwd, "Two copies", 3, 1);
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        paths.push(world.add_claude_session(&synth::encoded_project_dir(&cwd), &uuid, &refs));
    }
    let session_dir = paths[0].parent().expect("project dir").join(&uuid);
    std::fs::create_dir_all(session_dir.join("tool-results")).expect("session dir");
    std::fs::write(session_dir.join("tool-results").join("r.txt"), "result").expect("result");
    let second = paths.pop().expect("second");
    let first = paths.pop().expect("first");
    (first, second, session_dir)
}

/// Every file under `root` with its size and a cheap content digest (the
/// Pi/OMP "untouched" checks).
pub fn tree_digest(root: &Path) -> BTreeMap<PathBuf, (u64, u64)> {
    fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, (u64, u64)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if let Ok(bytes) = std::fs::read(&path) {
                let digest = bytes.iter().fold(1469598103934665603u64, |hash, byte| {
                    (hash ^ u64::from(*byte)).wrapping_mul(1099511628211)
                });
                out.insert(path, (bytes.len() as u64, digest));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, &mut out);
    out
}

/// Wait (real time, ≤ 30 s) until `done` holds, pumping the executor.
pub fn wait_for(cx: &mut TestAppContext, what: &str, done: impl Fn(&App) -> bool) {
    cx.executor().allow_parking();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| done(cx)) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(3));
    }
}

/// Draw `handle` once.
pub fn render(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("the window is open");
}

/// Let the dialog's open animation finish and draw it at rest.
pub fn settle(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.run_until_parked();
    render(cx, handle);
    std::thread::sleep(Duration::from_millis(350));
    render(cx, handle);
    cx.run_until_parked();
}

/// Whether `handle` shows a dialog.
pub fn has_dialog(cx: &mut TestAppContext, handle: AnyWindowHandle) -> bool {
    cx.update_window(handle, |_, window, cx| window.has_active_dialog(cx))
        .expect("the window is open")
}
