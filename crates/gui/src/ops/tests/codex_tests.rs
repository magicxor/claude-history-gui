//! G tests of the Codex actions (upstream eea6d37; V-104, A-065…A-067): resume and
//! fork through the Codex CLI with `CODEX_HOME`, delete through the platform
//! services, rename refused up front, orphaned threads refuse everything.
//! Nothing ever runs: the terminal launch and the `codex delete` run are
//! recorded.

use std::ffi::OsString;
use std::path::PathBuf;

use claude_history_core::history::model::SourceKind;
use claude_history_core::maintenance::plan_session_delete;
use gpui_kit::{AppContext as _, TestAppContext};

use super::{dialog_id, has_dialog, render, toast};
use crate::actions::{Delete, Fork, Rename, Resume};
use crate::list::context_menu;
use crate::ops::delete::{self, DIALOG_ID};
use crate::test_support::TestWorld;
use crate::test_support::fixtures_ops::{OpsFixture, fake_program, toml_path, write_gui_config};
use crate::workspace::routing::{self, SessionRoute};

const THREAD: &str = "01a00000-0000-7000-8000-000000000002";

/// The paginated fixture thread (cwd `/work/demo`) in the world's Codex home.
fn codex_thread(world: &TestWorld) -> PathBuf {
    world.add_fixture(
        &format!(".codex/sessions/2026/09/01/rollout-2026-09-01T10-00-00-{THREAD}.jsonl"),
        include_bytes!(
            "../../../../core/tests/fixtures/codex/sessions/2026/09/01/rollout-2026-09-01T10-00-00-01a00000-0000-7000-8000-000000000002.jsonl"
        ),
    )
}

/// `gui.toml` with every agent, `codex` included, a fake program.
fn codex_config(world: &TestWorld) -> PathBuf {
    let codex = fake_program(world, "codex");
    write_gui_config(world, &format!("codex_path = {}\n", toml_path(&codex)), "");
    codex
}

/// V-104, A-065, A-066, A-067: a Codex thread opens in the viewer, Rename is refused
/// before its dialog opens, Resume and Fork run `codex resume|fork <id>`
/// with `CODEX_HOME`, and Delete runs `codex delete --force <id>`.
#[gpui_kit::test]
fn codex_threads_resume_fork_and_delete_but_do_not_rename(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let codex = codex_config(&world);
    let path = codex_thread(&world);
    let home = world.home().join(".codex");
    let f = OpsFixture::open(cx, &world, &[]);
    let target = f.target(cx, &path);
    assert_eq!(target.source, Some(SourceKind::Codex));

    // The viewer shows the thread's dialogue (no error).
    let viewer = f.open_in_viewer(cx, &path);
    let (error, items) = cx.read(|cx| {
        let viewer = viewer.read(cx);
        (
            viewer.file_error().map(str::to_string),
            viewer.items().map_or(0, |items| items.len()),
        )
    });
    assert_eq!(error, None);
    assert!(items > 0, "the thread's messages are shown");

    // The row menu: everything but Rename.
    let menu = cx.read(|cx| context_menu::build(1, &target, false, true, cx));
    assert!(menu.is_disabled("Rename…"));
    for label in ["Resume", "Fork", "Delete…"] {
        assert!(!menu.is_disabled(label), "{label}");
    }

    // Rename: refused with core's text, no dialog.
    f.select(cx, &path);
    f.press(cx, "f2");
    f.settle(cx);
    assert!(!has_dialog(cx, f.handle()));
    assert_eq!(toast(cx), "Renaming CX sessions is not supported");

    // Resume and Fork: the Codex CLI with the thread's home.
    f.recording.clear_calls();
    f.select(cx, &path);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    f.select(cx, &path);
    f.press(cx, "ctrl-f");
    f.settle(cx);
    let commands = f.terminal_commands();
    assert_eq!(commands.len(), 2, "{commands:?}");
    for (command, verb) in commands.iter().zip(["resume", "fork"]) {
        assert!(
            command.contains(&format!("{verb} {THREAD}")),
            "{verb}: {command}"
        );
        assert!(command.contains("CODEX_HOME="), "{command}");
        assert!(command.contains(&home.display().to_string()), "{command}");
        assert!(command.contains(&codex.display().to_string()), "{command}");
    }

    // Delete: the plan lists the thread's rollouts and says what Codex does.
    let env = cx.read(|cx| crate::globals::app_state(cx).env_with_workspace());
    let plan = plan_session_delete(&env, &path, SourceKind::Codex).unwrap();
    assert_eq!(plan.files, vec![path.clone()]);
    assert_eq!(plan.session_id, THREAD);
    assert!(
        delete::consequence_text(&plan).starts_with(&format!("Runs codex delete --force {THREAD}")),
        "{}",
        delete::consequence_text(&plan)
    );
    f.recording.clear_calls();
    f.select(cx, &path);
    f.press(cx, "ctrl-x");
    f.settle(cx);
    assert!(has_dialog(cx, f.handle()), "Ctrl+X opens the confirmation");
    assert_eq!(dialog_id(cx), DIALOG_ID);
    render(cx, f.handle());
    f.press(cx, "shift-y");
    f.settle(cx);
    let runs = f.recording.program_runs();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0].program.path, codex);
    assert_eq!(
        runs[0].args,
        ["delete", "--force", THREAD].map(OsString::from).to_vec()
    );
    assert_eq!(
        runs[0].env,
        vec![(OsString::from("CODEX_HOME"), home.into_os_string())]
    );
    assert!(f.recording.terminal_commands().is_empty());
    assert!(path.is_file(), "the recorder deletes nothing");
    assert!(!f.list.paths(cx).contains(&path), "the row left the list");
    assert!(toast(cx).starts_with("Deleted \""), "{}", toast(cx));
}

/// A failed `codex delete` keeps the row and shows Codex's error.
#[gpui_kit::test]
fn a_failed_codex_delete_keeps_the_thread(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    codex_config(&world);
    let path = codex_thread(&world);
    let f = OpsFixture::open(cx, &world, &[]);
    f.recording
        .set_run_result(Ok(claude_history_core::resume::terminal::RunOutput {
            success: false,
            status: "exit status: 1".to_string(),
            stderr: "thread is busy\n".to_string(),
        }));
    f.select(cx, &path);
    f.press(cx, "ctrl-x");
    f.settle(cx);
    render(cx, f.handle());
    f.press(cx, "shift-y");
    f.settle(cx);
    assert_eq!(f.recording.program_runs().len(), 1);
    assert!(f.list.paths(cx).contains(&path), "the row stays");
    let text = toast(cx);
    assert!(
        text.contains("codex delete failed (exit status: 1): thread is busy"),
        "{text}"
    );
}

/// A-067: an orphaned thread (its current rollout is gone) refuses every
/// action with core's text, in the row menu and through the router.
#[gpui_kit::test]
fn orphaned_threads_refuse_every_action(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    codex_config(&world);
    let path = codex_thread(&world);
    let f = OpsFixture::open(cx, &world, &[]);
    let mut target = f.target(cx, &path);
    target.provenance.orphaned = true;

    let menu = cx.read(|cx| context_menu::build(1, &target, false, true, cx));
    for label in ["Resume", "Fork", "Rename…", "Delete…"] {
        assert!(menu.is_disabled(label), "{label}");
    }
    assert!(!menu.is_disabled("Open"));

    f.recording.clear_calls();
    let actions: [Box<dyn gpui_kit::Action>; 4] = [
        Box::new(Resume),
        Box::new(Fork),
        Box::new(Rename),
        Box::new(Delete),
    ];
    for action in actions {
        let route = cx
            .update_window(f.handle(), |_, window, cx| {
                let options = routing::default_export_options(cx);
                routing::run_session_action(action.as_ref(), target.clone(), options, window, cx)
            })
            .unwrap();
        assert_eq!(route, SessionRoute::Done);
        f.settle(cx);
        assert!(!has_dialog(cx, f.handle()));
        let text = toast(cx);
        assert!(
            text.contains("is unavailable: CX no longer has this conversation"),
            "{text}"
        );
    }
    assert!(f.recording.calls().is_empty(), "nothing ran");
}
