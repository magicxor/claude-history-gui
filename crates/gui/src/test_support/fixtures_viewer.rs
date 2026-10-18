//! Viewer fixtures (DESIGN §6, §15.2): synthetic 3k/10k-message transcripts
//! with 5k-line tool outputs, markdown samples. Owned by WP2.
//!
//! Builders use `super::TestWorld` and `super::synth`; GUI-only fixture
//! files go into `crates/gui/tests/fixtures/viewer/`.
//!
//! * JSONL line builders for every block kind the viewer renders (user,
//!   assistant text, thinking, tool calls and results, subagent entries,
//!   agent progress, Pi metadata);
//! * [`write_transcript`] / [`items_for`]: pure item builds (P tests);
//! * [`open_viewer`] / [`open_main_viewer`]: a viewer in a real test window,
//!   [`wait_viewer_ready`] waits for its build, [`draw`] renders a frame.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use claude_history_core::view::{ToolDisplayMode, ViewPrefs, ViewerModel};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext};

use super::TestWorld;
use crate::list::ListPaneEvent;
use crate::model::ConversationTarget;
use crate::viewer::ViewerPane;
use crate::viewer::items::{self, ViewerItem};

/// A deterministic RFC 3339 stamp `n` minutes after the fixture epoch.
pub fn stamp(n: u32) -> String {
    format!("2026-09-04T{:02}:{:02}:00Z", 10 + n / 60, n % 60)
}

fn json(text: &str) -> String {
    serde_json::to_string(text).expect("strings serialize")
}

/// A user message (string content).
pub fn user(text: &str) -> String {
    user_at(text, 0)
}

/// A user message at `stamp(minute)`.
pub fn user_at(text: &str, minute: u32) -> String {
    format!(
        r#"{{"type":"user","sessionId":"fixture","timestamp":"{}","message":{{"role":"user","content":{}}}}}"#,
        stamp(minute),
        json(text)
    )
}

/// An assistant message with one text block.
pub fn assistant(text: &str) -> String {
    assistant_at(text, 1)
}

/// An assistant text message at `stamp(minute)`.
pub fn assistant_at(text: &str, minute: u32) -> String {
    assistant_blocks(
        &format!(r#"[{{"type":"text","text":{}}}]"#, json(text)),
        minute,
    )
}

/// An assistant message with raw content blocks (`[...]` JSON).
pub fn assistant_blocks(blocks: &str, minute: u32) -> String {
    format!(
        r#"{{"type":"assistant","sessionId":"fixture","timestamp":"{}","message":{{"role":"assistant","model":"claude-opus-4-5","content":{blocks}}}}}"#,
        stamp(minute)
    )
}

/// An assistant message with a thinking block and a text block.
pub fn thinking_and_text(thinking: &str, text: &str, minute: u32) -> String {
    assistant_blocks(
        &format!(
            r#"[{{"type":"thinking","thinking":{},"signature":""}},{{"type":"text","text":{}}}]"#,
            json(thinking),
            json(text)
        ),
        minute,
    )
}

/// An assistant message calling one tool (`input` is JSON).
pub fn tool_call(id: &str, name: &str, input: &str, minute: u32) -> String {
    assistant_blocks(
        &format!(r#"[{{"type":"tool_use","id":"{id}","name":"{name}","input":{input}}}]"#),
        minute,
    )
}

/// A Bash call running `command`.
pub fn bash_call(id: &str, command: &str, minute: u32) -> String {
    tool_call(
        id,
        "Bash",
        &format!(r#"{{"command":{}}}"#, json(command)),
        minute,
    )
}

/// A user message with a tool result.
pub fn tool_result(id: &str, content: &str, minute: u32) -> String {
    format!(
        r#"{{"type":"user","sessionId":"fixture","timestamp":"{}","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"{id}","content":{}}}]}}}}"#,
        stamp(minute),
        json(content)
    )
}

/// A subagent assistant text message under `parent`.
pub fn subagent_text(parent: &str, text: &str, minute: u32) -> String {
    format!(
        r#"{{"type":"assistant","parent_tool_use_id":"{parent}","sessionId":"fixture","timestamp":"{}","message":{{"role":"assistant","content":[{{"type":"text","text":{}}}]}}}}"#,
        stamp(minute),
        json(text)
    )
}

/// A subagent tool result under `parent`.
pub fn subagent_result(parent: &str, id: &str, content: &str, minute: u32) -> String {
    format!(
        r#"{{"type":"user","parent_tool_use_id":"{parent}","sessionId":"fixture","timestamp":"{}","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"{id}","content":{}}}]}}}}"#,
        stamp(minute),
        json(content)
    )
}

/// An agent-progress assistant record of `agent_id`.
pub fn agent_progress(agent_id: &str, text: &str) -> String {
    format!(
        r#"{{"type":"progress","data":{{"type":"agent_progress","agentId":"{agent_id}","message":{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":{}}}]}}}}}}}}"#,
        json(text)
    )
}

/// A searchable Pi metadata record.
pub fn pi_metadata(label: &str, text: &str) -> String {
    format!(
        r#"{{"type":"pi-metadata","label":"{label}","text":{},"timestamp":"{}"}}"#,
        json(text),
        stamp(3)
    )
}

/// A skill-expansion user message.
pub fn skill_user(text: &str) -> String {
    user(&format!("Base directory for this skill: /x\n\n{text}"))
}

/// `n` numbered lines (`line 1` … `line n`).
pub fn numbered_lines(n: usize) -> String {
    (1..=n)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Write a transcript under `dir`.
pub fn write_transcript(dir: &Path, name: &str, lines: &[String]) -> PathBuf {
    std::fs::create_dir_all(dir).expect("create the fixture dir");
    let path = dir.join(name);
    let mut text = lines.join("\n");
    text.push('\n');
    std::fs::write(&path, text).expect("write the transcript");
    path
}

/// View preferences.
pub fn prefs(tool_display: ToolDisplayMode, thinking: bool, timing: bool) -> ViewPrefs {
    ViewPrefs {
        tool_display,
        show_thinking: thinking,
        show_timing: timing,
    }
}

/// The model of `lines` (written to a temp file).
pub fn model_for(lines: &[String], prefs: ViewPrefs) -> (tempfile::TempDir, ViewerModel) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = write_transcript(dir.path(), "fixture.jsonl", lines);
    let model = ViewerModel::open(&path, prefs).expect("the fixture parses");
    (dir, model)
}

/// The items of `lines` at `cols` content cells.
pub fn items_for(lines: &[String], prefs: ViewPrefs, cols: usize) -> Vec<ViewerItem> {
    let (_dir, model) = model_for(lines, prefs);
    items_of(&model, cols)
}

/// The items of `model` at `cols` content cells.
pub fn items_of(model: &ViewerModel, cols: usize) -> Vec<ViewerItem> {
    let doc = model.document();
    items::build(&doc, model, cols, &|| false).expect("never cancelled")
}

/// The text of every segment of an item.
pub fn item_text(item: &ViewerItem) -> String {
    items::item_text(item)
}

/// Draw a frame of `handle`.
pub fn draw(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("the window is open");
}

/// Wait (≤ 30 s) until the viewer has no build in flight, then draw.
pub fn wait_viewer_ready(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    viewer: &Entity<ViewerPane>,
) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        cx.run_until_parked();
        if !cx.read(|cx| viewer.read(cx).is_loading()) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the viewer did not finish its build within 30 s"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    draw(cx, handle);
    cx.run_until_parked();
    draw(cx, handle);
}

/// The viewer of a single-file window.
pub fn single_file_viewer(cx: &mut TestAppContext, handle: AnyWindowHandle) -> Entity<ViewerPane> {
    cx.update_window(handle, |root, _, cx| {
        let root = root
            .downcast::<gpui_kit::base::Root>()
            .expect("a gpui-kit root");
        let view = root.read(cx).view().clone();
        view.downcast::<crate::single_file::SingleFileWindow>()
            .expect("a single-file window")
            .read(cx)
            .viewer()
            .clone()
    })
    .expect("the window is open")
}

/// A transcript in `world` opened in a single-file window (built and drawn).
pub fn open_viewer(
    cx: &mut TestAppContext,
    world: &TestWorld,
    lines: &[String],
    args: &[&str],
) -> (AnyWindowHandle, Entity<ViewerPane>, PathBuf) {
    let path = write_transcript(&world.home().join("viewer"), "fixture.jsonl", lines);
    let handle = super::open_single_file_with_args(cx, world, &path, args);
    let viewer = single_file_viewer(cx, handle);
    wait_viewer_ready(cx, handle, &viewer);
    (handle, viewer, path)
}

/// Open `path` in the main window's viewer (what Enter on a row does).
pub fn open_in_main(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    ws: &Entity<crate::workspace::Workspace>,
    path: &Path,
    focus: bool,
) -> Entity<ViewerPane> {
    let list = cx.read(|cx| ws.read(cx).list().clone());
    list.update(cx, |_, cx| {
        cx.emit(ListPaneEvent::OpenRequested {
            target: ConversationTarget::from_parsed_file(path, None),
            focus_viewer: focus,
            agent_tab: None,
        })
    });
    let viewer = cx.read(|cx| ws.read(cx).viewer().clone());
    wait_viewer_ready(cx, handle, &viewer);
    viewer
}

/// A transcript in `world` opened in the main window's viewer.
pub fn open_main_viewer(
    cx: &mut TestAppContext,
    world: &TestWorld,
    lines: &[String],
    args: &[&str],
) -> (
    AnyWindowHandle,
    Entity<crate::workspace::Workspace>,
    Entity<ViewerPane>,
    PathBuf,
) {
    let path = write_transcript(&world.home().join("viewer"), "main-fixture.jsonl", lines);
    let (handle, ws) = super::open_main(cx, world, args);
    let viewer = open_in_main(cx, handle, &ws, &path, true);
    (handle, ws, viewer, path)
}

/// `messages` alternating user/assistant messages, each a few lines, so the
/// transcript is taller than the viewport.
pub fn long_conversation(messages: usize) -> Vec<String> {
    (0..messages)
        .map(|ix| {
            let text = format!(
                "Message {} body.\n\nSecond paragraph of message {}.\n\nThird paragraph.",
                ix + 1,
                ix + 1
            );
            if ix % 2 == 0 {
                user_at(&text, ix as u32)
            } else {
                assistant_at(&text, ix as u32)
            }
        })
        .collect()
}

/// The synthetic perf transcript (§15.8): `messages` messages with
/// `big_outputs` Bash results of `lines_per_output` lines.
pub fn perf_transcript(
    world: &TestWorld,
    messages: usize,
    big_outputs: usize,
    lines_per_output: usize,
) -> PathBuf {
    let path = world
        .home()
        .join("viewer")
        .join(format!("perf-{messages}.jsonl"));
    super::synth::transcript(&path, messages, big_outputs, lines_per_output);
    path
}
