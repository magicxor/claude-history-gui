//! G tests of export and copy-as (A-034…A-038, A-044, A-061, A-028 export
//! races, X-003 replace prompt).

use std::path::{Path, PathBuf};

use claude_history_core::export::{ExportFormat, ExportOptions, generate_export};
use gpui_kit::{AppContext as _, TestAppContext};

use super::{dialog_id, has_dialog, render, toast};
use crate::actions::{SetToolsFull, SetToolsSummary};
use crate::ops::export::{
    COPY_DIALOG_ID, EXPORT_DIALOG_ID, LastExportMenu, REPLACE_DIALOG_ID, default_export_dir,
    with_default_extension,
};
use crate::services::ServiceCall;
use crate::test_support::TestWorld;
use crate::test_support::fixtures_ops::{OpsFixture, claude_session, tool_session};

fn saves(f: &OpsFixture) -> Vec<(PathBuf, String)> {
    f.recording
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            ServiceCall::PromptSave { dir, name } => Some((dir, name)),
            _ => None,
        })
        .collect()
}

fn out_dir(world: &TestWorld) -> PathBuf {
    let dir = world.home().join("out");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Open the viewer on `path` and its export menu with `e`.
fn export_menu(cx: &mut TestAppContext, f: &OpsFixture, path: &Path) {
    f.open_in_viewer(cx, path);
    f.press(cx, "e");
    f.settle(cx);
    assert!(has_dialog(cx, f.handle()), "`e` opens the export menu");
    assert_eq!(dialog_id(cx), EXPORT_DIALOG_ID);
    render(cx, f.handle());
}

/// A-034: `e` then `3` asks for a Markdown file name; `j j Enter` picks the
/// same; Esc records nothing.
#[gpui_kit::test]
fn export_menu_keys(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &[]);

    export_menu(cx, &f, &path);
    f.press(cx, "3");
    f.settle(cx);
    let recorded = saves(&f);
    assert_eq!(recorded.len(), 1);
    assert!(
        recorded[0].1.starts_with("conversation-") && recorded[0].1.ends_with(".md"),
        "{recorded:?}"
    );
    assert!(!has_dialog(cx, f.handle()), "the menu closed");

    f.recording.clear_calls();
    f.press(cx, "e");
    f.settle(cx);
    render(cx, f.handle());
    f.press(cx, "j j enter");
    f.settle(cx);
    let recorded = saves(&f);
    assert_eq!(recorded.len(), 1);
    assert!(
        recorded[0].1.ends_with(".md"),
        "j j Enter picks Markdown: {recorded:?}"
    );

    f.recording.clear_calls();
    f.press(cx, "e");
    f.settle(cx);
    render(cx, f.handle());
    f.press(cx, "escape");
    f.settle(cx);
    assert!(saves(&f).is_empty(), "Esc records nothing");
    assert!(!has_dialog(cx, f.handle()));
    // The default name per format.
    for (keys, ext) in [("1", ".txt"), ("2", ".txt"), ("4", ".jsonl")] {
        f.recording.clear_calls();
        f.press(cx, "e");
        f.settle(cx);
        render(cx, f.handle());
        f.press(cx, keys);
        f.settle(cx);
        assert!(saves(&f)[0].1.ends_with(ext), "{keys}: {:?}", saves(&f));
    }
}

/// A-035: the answered path gets the format's extension; an existing target
/// asks `Replace "x.md"?` (Enter ignored, `y` replaces); the toast carries the
/// full path; the next export starts in the same folder.
#[gpui_kit::test]
fn export_writes_replaces_and_remembers_the_folder(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let out = out_dir(&world);
    let f = OpsFixture::open(cx, &world, &[]);
    let expected =
        generate_export(&path, ExportFormat::Markdown, ExportOptions::default()).unwrap();

    f.recording.answer_save(Ok(Some(out.join("x"))));
    export_menu(cx, &f, &path);
    f.press(cx, "3");
    f.settle(cx);
    let written = out.join("x.md");
    assert_eq!(std::fs::read_to_string(&written).unwrap(), expected);
    assert_eq!(toast(cx), format!("Exported to {}", written.display()));
    // The toast's Reveal button reveals the exported file (not the source).
    assert_eq!(
        cx.read(crate::notify::last_toast_actions),
        vec![gpui_kit::SharedString::from("Reveal")]
    );
    f.recording.clear_calls();
    let pressed = cx
        .update_window(f.handle(), |_, window, cx| {
            crate::notify::trigger_last_toast_action("Reveal", window, cx)
        })
        .unwrap();
    assert!(pressed);
    assert_eq!(
        f.recording.calls(),
        vec![ServiceCall::Reveal(written.clone())]
    );

    // The next export's default folder is where the last one went.
    f.recording.clear_calls();
    f.press(cx, "e");
    f.settle(cx);
    render(cx, f.handle());
    f.press(cx, "3");
    f.settle(cx);
    assert_eq!(saves(&f)[0].0, out);

    // Replace prompt: the name gained an extension and the target exists.
    std::fs::write(&written, "old content").unwrap();
    f.recording.answer_save(Ok(Some(out.join("x"))));
    f.press(cx, "e");
    f.settle(cx);
    render(cx, f.handle());
    f.press(cx, "3");
    f.settle(cx);
    assert!(has_dialog(cx, f.handle()), "the Replace prompt");
    assert_eq!(dialog_id(cx), REPLACE_DIALOG_ID);
    assert_eq!(
        crate::ops::export::replace_title("x.md"),
        "Replace \"x.md\"?"
    );
    render(cx, f.handle());
    f.press(cx, "enter");
    f.settle(cx);
    assert!(has_dialog(cx, f.handle()), "Enter does not confirm");
    assert_eq!(std::fs::read_to_string(&written).unwrap(), "old content");
    f.press(cx, "y");
    f.settle(cx);
    assert!(!has_dialog(cx, f.handle()));
    assert_eq!(
        std::fs::read_to_string(&written).unwrap(),
        expected,
        "replaced"
    );

    // n cancels the replacement.
    std::fs::write(&written, "old content").unwrap();
    f.recording.answer_save(Ok(Some(out.join("x"))));
    f.press(cx, "e");
    f.settle(cx);
    render(cx, f.handle());
    f.press(cx, "3");
    f.settle(cx);
    f.press(cx, "n");
    f.settle(cx);
    assert_eq!(std::fs::read_to_string(&written).unwrap(), "old content");

    // A name that already has an extension is the native dialog's business.
    assert_eq!(
        with_default_extension(PathBuf::from("a.report"), ExportFormat::Markdown),
        PathBuf::from("a.report")
    );
    assert_eq!(
        with_default_extension(PathBuf::from("a"), ExportFormat::Jsonl),
        PathBuf::from("a.jsonl")
    );
}

/// A cancelled save prompt does nothing.
#[gpui_kit::test]
fn cancelled_save_prompt_writes_nothing(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &[]);
    let before = toast(cx);
    export_menu(cx, &f, &path);
    f.press(cx, "1");
    f.settle(cx);
    assert_eq!(saves(&f).len(), 1);
    assert_eq!(toast(cx), before);
}

/// Each format's file content is the core export (prefix per format).
#[gpui_kit::test]
fn exported_files_hold_each_formats_content(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let out = out_dir(&world);
    let f = OpsFixture::open(cx, &world, &[]);
    f.open_in_viewer(cx, &path);
    for (key, format, prefix) in [
        ("1", ExportFormat::Ledger, ""),
        ("2", ExportFormat::Plain, "You:"),
        ("3", ExportFormat::Markdown, ""),
        ("4", ExportFormat::Jsonl, "{"),
    ] {
        let name = format!("f{key}");
        f.recording.answer_save(Ok(Some(out.join(&name))));
        f.press(cx, "e");
        f.settle(cx);
        render(cx, f.handle());
        f.press(cx, key);
        f.settle(cx);
        let file = out.join(format!("{name}.{}", format.extension()));
        let content = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{file:?}: {e}"));
        let expected = generate_export(&path, format, ExportOptions::default()).unwrap();
        assert_eq!(content, expected, "{format:?}");
        assert!(content.starts_with(prefix), "{format:?}: {content:?}");
        assert!(!content.is_empty());
    }
}

/// A-036: the export follows the viewer's tool display (Summary: no tools).
#[gpui_kit::test]
fn export_options_follow_the_viewers_mode(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = tool_session(&world, "proj", 1);
    let out = out_dir(&world);
    let f = OpsFixture::open(cx, &world, &[]);
    f.open_in_viewer(cx, &path);
    let handle = f.handle();

    cx.dispatch_action(handle, SetToolsSummary);
    cx.run_until_parked();
    f.recording.answer_save(Ok(Some(out.join("summary"))));
    f.press(cx, "e");
    f.settle(cx);
    render(cx, handle);
    f.press(cx, "2");
    f.settle(cx);
    let summary = std::fs::read_to_string(out.join("summary.txt")).unwrap();
    assert!(
        !summary.contains("Tool:"),
        "Summary mode exports no tools: {summary}"
    );

    cx.dispatch_action(handle, SetToolsFull);
    cx.run_until_parked();
    f.recording.answer_save(Ok(Some(out.join("full"))));
    f.press(cx, "e");
    f.settle(cx);
    render(cx, handle);
    f.press(cx, "2");
    f.settle(cx);
    let full = std::fs::read_to_string(out.join("full.txt")).unwrap();
    assert!(full.contains("Tool:"), "Full mode exports them: {full}");
}

/// A-061: a save dialog that cannot be shown toasts with the fallback; the
/// fallback writes the default name into the default folder.
#[gpui_kit::test]
fn save_dialog_failure_offers_the_default_folder(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &[]);
    f.recording.answer_save(Err("no portal".to_string()));
    export_menu(cx, &f, &path);
    f.press(cx, "3");
    f.settle(cx);
    assert_eq!(toast(cx), "Can't open the save dialog: no portal");
    let (dir, name) = saves(&f).remove(0);
    assert_eq!(dir, cx.read(default_export_dir));
    // Click the toast's "Export to Downloads" (integrator: toast buttons are
    // observable through `notify`).
    let pressed = cx
        .update_window(f.handle(), |_, window, cx| {
            crate::notify::trigger_last_toast_action("Export to Downloads", window, cx)
        })
        .unwrap();
    assert!(pressed, "the toast offers Export to Downloads");
    f.settle(cx);
    assert!(dir.join(&name).is_file());
    assert_eq!(
        toast(cx),
        format!("Exported to {}", dir.join(&name).display())
    );
}

/// A write that fails shows core's `Failed to write: …` text.
#[gpui_kit::test]
fn export_write_failure_shows_core_text(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &[]);
    // A destination inside a folder that does not exist.
    f.recording
        .answer_save(Ok(Some(world.home().join("missing-dir").join("x.md"))));
    export_menu(cx, &f, &path);
    f.press(cx, "3");
    f.settle(cx);
    assert!(toast(cx).starts_with("Failed to write: "), "{}", toast(cx));
}

/// A-038: `y` with message navigation inactive opens the Copy menu; `2` copies
/// the Plain export.
#[gpui_kit::test]
fn copy_to_clipboard_menu(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &[]);
    f.open_in_viewer(cx, &path);
    f.press(cx, "y");
    f.settle(cx);
    assert!(has_dialog(cx, f.handle()));
    assert_eq!(dialog_id(cx), COPY_DIALOG_ID);
    render(cx, f.handle());
    f.press(cx, "2");
    f.settle(cx);
    let writes = f.clipboard_writes();
    assert_eq!(writes.len(), 1, "A-044: one clipboard write: {writes:?}");
    assert!(writes[0].starts_with("You:"), "{}", writes[0]);
    assert_eq!(toast(cx), "Copied to clipboard");
    assert_eq!(f.recording.calls().len(), 1, "A-044: no other side effect");
}

/// A-028: the menu captured its conversation and options; the list selection
/// moving before the pick changes neither the exported conversation nor the
/// copied one.
#[gpui_kit::test]
fn export_and_copy_use_the_captured_conversation(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let cwd = crate::test_support::fixtures_ops::project_dir(&world, "proj");
    let make = |number: u32, text: &str| {
        crate::test_support::fixtures_list::SessionSpec::new("proj", number)
            .cwd(cwd.clone())
            .messages(&[("user", text), ("assistant", "an answer")])
            .write(&world)
    };
    let first = make(1, "First conversation");
    let second = make(2, "Second conversation");
    let out = out_dir(&world);
    let f = OpsFixture::open(cx, &world, &[]);
    f.open_in_viewer(cx, &first);
    assert!(
        f.list_pane()
            .update(cx, |list, cx| list.select_path(&second, cx))
    );

    f.recording.answer_save(Ok(Some(out.join("captured"))));
    f.press(cx, "e");
    f.settle(cx);
    render(cx, f.handle());
    assert!(cx.read(|cx| cx.global::<LastExportMenu>().0.upgrade().is_some()));
    assert!(
        f.list_pane()
            .update(cx, |list, cx| list.select_path(&second, cx))
    );
    f.press(cx, "2");
    f.settle(cx);
    let exported = std::fs::read_to_string(out.join("captured.txt")).unwrap();
    assert!(exported.contains("First conversation"), "{exported}");
    assert!(!exported.contains("Second conversation"));

    // Copy from the list entry on the second row (the list's own target).
    f.select(cx, &second);
    cx.dispatch_action(f.handle(), crate::actions::CopyAsPlain);
    f.settle(cx);
    let writes = f.clipboard_writes();
    assert!(
        writes.last().unwrap().contains("Second conversation"),
        "{writes:?}"
    );
}

/// A-028: a Copy menu opened in the viewer copies the viewer's conversation
/// even though the list selection moved and the list refreshed before the pick.
#[gpui_kit::test]
fn the_copy_menu_uses_the_captured_conversation(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let cwd = crate::test_support::fixtures_ops::project_dir(&world, "proj");
    let make = |number: u32, text: &str| {
        crate::test_support::fixtures_list::SessionSpec::new("proj", number)
            .cwd(cwd.clone())
            .messages(&[("user", text), ("assistant", "an answer")])
            .write(&world)
    };
    let first = make(1, "First conversation");
    let second = make(2, "Second conversation");
    let f = OpsFixture::open(cx, &world, &[]);
    f.open_in_viewer(cx, &first);
    f.press(cx, "y");
    f.settle(cx);
    assert_eq!(dialog_id(cx), COPY_DIALOG_ID);
    render(cx, f.handle());
    assert!(
        f.list_pane()
            .update(cx, |list, cx| list.select_path(&second, cx))
    );
    f.list_pane().update(cx, |list, cx| list.refresh(cx));
    f.press(cx, "2");
    f.settle(cx);
    let writes = f.clipboard_writes();
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert!(writes[0].contains("First conversation"), "{}", writes[0]);
}
