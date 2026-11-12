//! G tests of the Copilot actions (fork-only source): the session lists and
//! opens, Resume runs `copilot --resume=<id>` with `COPILOT_HOME`, Fork,
//! Rename and Delete are refused up front with core's text. Nothing ever
//! runs: the terminal launch is recorded.

use std::path::PathBuf;

use claude_history_core::history::model::SourceKind;
use gpui_kit::TestAppContext;

use super::{has_dialog, toast};
use crate::list::context_menu;
use crate::test_support::TestWorld;
use crate::test_support::fixtures_ops::{OpsFixture, fake_program, toml_path, write_gui_config};
use crate::viewer::toolbar::Refusals;

const SESSION: &str = "0c0a0000-0000-4000-8000-000000000001";

/// The fixture session (cwd `/work/demo`) in the world's Copilot home.
fn copilot_session(world: &TestWorld) -> PathBuf {
    world.add_fixture(
        &format!(".copilot/session-state/{SESSION}/workspace.yaml"),
        include_bytes!(
            "../../../../core/tests/fixtures/copilot/session-state/0c0a0000-0000-4000-8000-000000000001/workspace.yaml"
        ),
    );
    world.add_fixture(
        &format!(".copilot/session-state/{SESSION}/events.jsonl"),
        include_bytes!(
            "../../../../core/tests/fixtures/copilot/session-state/0c0a0000-0000-4000-8000-000000000001/events.jsonl"
        ),
    )
}

/// `gui.toml` with `copilot_path` a fake program and `extra` appended.
fn copilot_config(world: &TestWorld, extra: &str) -> PathBuf {
    let copilot = fake_program(world, "copilot");
    write_gui_config(
        world,
        &format!("copilot_path = {}\n", toml_path(&copilot)),
        extra,
    );
    copilot
}

/// The session is listed as a Copilot row and opens in the viewer.
#[gpui_kit::test]
fn a_copilot_session_lists_and_opens(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    copilot_config(&world, "");
    let path = copilot_session(&world);
    let f = OpsFixture::open(cx, &world, &[]);
    let target = f.target(cx, &path);
    assert_eq!(target.source, Some(SourceKind::Copilot));
    assert_eq!(target.title.as_ref(), "Ocelot cache eviction");
    assert_eq!(target.session_id, SESSION);
    assert_eq!(SourceKind::Copilot.list_label(), "CP");

    let viewer = f.open_in_viewer(cx, &path);
    let (error, items) = cx.read(|cx| {
        let viewer = viewer.read(cx);
        (
            viewer.file_error().map(str::to_string),
            viewer.items().map_or(0, |items| items.len()),
        )
    });
    assert_eq!(error, None);
    assert!(items > 0, "the session's messages are shown");
}

/// Resume starts the Copilot CLI with `--resume=<id>` and the session's
/// `COPILOT_HOME`.
#[gpui_kit::test]
fn resume_runs_copilot_with_the_session_id_and_home(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let copilot = copilot_config(&world, "");
    let path = copilot_session(&world);
    let home = world.home().join(".copilot");
    let f = OpsFixture::open(cx, &world, &[]);

    f.recording.clear_calls();
    f.select(cx, &path);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    let commands = f.terminal_commands();
    assert_eq!(commands.len(), 1, "{commands:?}");
    let command = &commands[0];
    assert!(
        command.contains(&format!("--resume={SESSION}")),
        "{command}"
    );
    assert!(command.contains("COPILOT_HOME="), "{command}");
    assert!(command.contains(&home.display().to_string()), "{command}");
    assert!(
        command.contains(&copilot.display().to_string()),
        "{command}"
    );
}

/// `[resume.copilot] default_args` come before `--resume=`.
#[gpui_kit::test]
fn default_arguments_precede_the_resume_argument(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    copilot_config(
        &world,
        "[resume.copilot]\ndefault_args = [\"--allow-all-tools\"]",
    );
    let path = copilot_session(&world);
    let f = OpsFixture::open(cx, &world, &[]);

    f.recording.clear_calls();
    f.select(cx, &path);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    let commands = f.terminal_commands();
    assert_eq!(commands.len(), 1, "{commands:?}");
    // The launcher may quote the `--resume=` argument: compare positions.
    let command = &commands[0];
    let default_arg = command.find("--allow-all-tools").expect(command);
    let resume = command.find(&format!("--resume={SESSION}")).expect(command);
    assert!(default_arg < resume, "{command}");
}

/// Fork, Rename and Delete are refused with core's text and no dialog; the
/// row menu and the viewer toolbar say so, and nothing is launched.
#[gpui_kit::test]
fn fork_rename_and_delete_are_refused(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    copilot_config(&world, "");
    let path = copilot_session(&world);
    let f = OpsFixture::open(cx, &world, &[]);
    let target = f.target(cx, &path);

    let menu = cx.read(|cx| context_menu::build(1, &target, false, true, cx));
    for label in ["Fork", "Rename…", "Delete…"] {
        assert!(menu.is_disabled(label), "{label}");
    }
    assert!(!menu.is_disabled("Resume"));

    let refusals = Refusals::of(Some(&target));
    assert_eq!(refusals.resume, None);
    let text = |refusal: &Option<gpui_kit::SharedString>| refusal.as_ref().map(|s| s.to_string());
    assert_eq!(
        text(&refusals.fork).as_deref(),
        Some("Forking CP sessions is not supported")
    );
    assert_eq!(
        text(&refusals.rename).as_deref(),
        Some("Renaming CP sessions is not supported")
    );
    assert_eq!(
        text(&refusals.delete).as_deref(),
        Some("Deleting CP sessions is not supported")
    );

    f.recording.clear_calls();
    for (keys, expected) in [
        ("ctrl-f", "Forking CP sessions is not supported"),
        ("f2", "Renaming CP sessions is not supported"),
        ("ctrl-x", "Deleting CP sessions is not supported"),
    ] {
        f.select(cx, &path);
        f.press(cx, keys);
        f.settle(cx);
        assert!(!has_dialog(cx, f.handle()), "{keys}");
        assert_eq!(toast(cx), expected, "{keys}");
    }
    assert!(f.recording.calls().is_empty(), "nothing ran");
    assert!(path.is_file(), "the session is untouched");
}

/// "Copy session ID" copies the uuid, not the file stem `events`.
#[gpui_kit::test]
fn copy_session_id_copies_the_uuid(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    copilot_config(&world, "");
    let path = copilot_session(&world);
    let f = OpsFixture::open(cx, &world, &[]);
    f.open_in_viewer(cx, &path);
    f.recording.clear_calls();
    f.press(cx, "shift-i");
    assert_eq!(f.clipboard_writes(), vec![SESSION.to_string()]);
}
