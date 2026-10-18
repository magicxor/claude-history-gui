//! G tests of resume and fork (A-001…A-016, C-026, X-003 copy dialog, A-028
//! resume/fork races). Terminal commands are only ever recorded
//! (`RecordingServices`); nothing is spawned.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use claude_history_core::resume::ResumePlan;
use gpui_kit::{AppContext as _, TestAppContext};

use super::{dialog_id, has_dialog, render, toast};
use crate::model::ConversationTarget;
use crate::ops::resume::{self, CANCELLED, plan_and_confirm};
use crate::test_support::fixtures_ops::{
    OpsFixture, claude_session, claude_session_gone, claude_session_in, omp_session, pi_session,
    project_dir, write_gui_config,
};
use crate::test_support::{TestWorld, synth};

fn stem(path: &Path) -> String {
    path.file_stem().unwrap().to_string_lossy().into_owned()
}

/// Whether the last recorded terminal command runs in `dir` (as its process
/// directory, or as the `/D` / `--directory` argument the Windows launchers
/// use).
fn assert_runs_in(f: &OpsFixture, dir: &Path) {
    let command = f
        .recording
        .terminal_commands()
        .pop()
        .expect("a terminal command was recorded");
    let norm = |text: String| text.replace('/', "\\").to_lowercase();
    let want = norm(dir.display().to_string());
    let in_cwd = command
        .cwd
        .as_ref()
        .is_some_and(|cwd| norm(cwd.display().to_string()) == want);
    assert!(
        in_cwd || norm(command.display()).contains(&want),
        "runs in {dir:?}: {} (cwd {:?})",
        command.display(),
        command.cwd
    );
}

fn only_command(f: &OpsFixture) -> String {
    let commands = f.terminal_commands();
    assert_eq!(commands.len(), 1, "one terminal command: {commands:?}");
    commands[0].clone()
}

/// A-001, A-004, A-009, A-011, C-026: Ctrl+R on a list row runs the resume
/// arguments core builds (default args appended) through the fake program.
#[gpui_kit::test]
fn resume_from_the_list_records_one_terminal_command(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(
        &world,
        "",
        "[resume]\ndefault_args = [\"--model\", \"opus\"]",
    );
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &[]);
    f.select(cx, &path);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    let command = only_command(&f);
    assert!(
        command.contains(&format!("--resume {} --model opus", stem(&path))),
        "{command}"
    );
    assert!(command.contains("fake-claude"), "A-011: {command}");
    assert!(
        toast(cx).starts_with("Opened \"Fix login bug\" in "),
        "A-009: {}",
        toast(cx)
    );
    assert!(
        !has_dialog(cx, f.handle()),
        "the window stays open, no dialog"
    );
}

/// A-002: a same-project fork keeps `--fork-session`.
#[gpui_kit::test]
fn fork_in_the_same_project_adds_fork_session(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &[]);
    f.select(cx, &path);
    f.press(cx, "ctrl-f");
    f.settle(cx);
    let command = only_command(&f);
    assert!(
        command.contains(&format!("--resume {} --fork-session", stem(&path))),
        "{command}"
    );
}

/// A-001 from the viewer, A-028: the viewer's conversation is resumed even
/// though the list selection moved to another row.
#[gpui_kit::test]
fn resume_and_fork_from_the_viewer_use_the_viewers_conversation(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let shown = claude_session(&world, "proj", 1, "Shown in the viewer");
    let other = claude_session(&world, "proj", 2, "Selected in the list");
    let f = OpsFixture::open(cx, &world, &[]);
    f.open_in_viewer(cx, &shown);
    // The selection moves away (a refresh / result set would do the same).
    assert!(
        f.list_pane()
            .update(cx, |list, cx| list.select_path(&other, cx))
    );
    f.press(cx, "ctrl-r");
    f.settle(cx);
    let command = only_command(&f);
    assert!(command.contains(&stem(&shown)), "{command}");
    assert!(!command.contains(&stem(&other)), "{command}");
    f.recording.clear_calls();
    f.press(cx, "ctrl-f");
    f.settle(cx);
    let command = only_command(&f);
    assert!(
        command.contains(&format!("{} --fork-session", stem(&shown))),
        "{command}"
    );
}

/// A-003 (1): the project directory exists → run in it, no dialog.
/// (2): it is gone but the workspace maps to the directory holding the
/// session → run in the workspace, no dialog. (3): the workspace maps
/// elsewhere → the copy dialog.
#[gpui_kit::test]
fn claude_resume_decisions_follow_the_project_and_the_workspace(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    // (1) project exists.
    let exists = claude_session(&world, "proj", 1, "Project exists");
    // (2) project gone, session stored in the workspace's projects directory.
    let workspace = project_dir(&world, "ws-maps");
    let gone_cwd = world.home().join("work").join("gone").join("p2");
    let lines =
        synth::claude_conversation(&synth::uuid(2), &gone_cwd, "Maps to the workspace", 2, 1);
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let in_workspace = world.add_claude_session(
        &synth::encoded_project_dir(&workspace),
        &synth::uuid(2),
        &refs,
    );
    // (3) project gone, stored elsewhere.
    let elsewhere = claude_session_gone(&world, 3, "Needs a copy");
    let f = OpsFixture::open(
        cx,
        &world,
        &["--workspace", &workspace.display().to_string()],
    );

    f.select(cx, &exists);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    let command = only_command(&f);
    assert!(command.contains(&stem(&exists)), "{command}");
    assert_runs_in(&f, &project_dir(&world, "proj"));
    assert!(!has_dialog(cx, f.handle()), "(1) no dialog");

    f.recording.clear_calls();
    f.select(cx, &in_workspace);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    let command = only_command(&f);
    assert!(command.contains(&stem(&in_workspace)), "{command}");
    assert_runs_in(&f, &workspace);
    assert!(!has_dialog(cx, f.handle()), "(2) no dialog");

    f.recording.clear_calls();
    f.select(cx, &elsewhere);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    assert!(has_dialog(cx, f.handle()), "(3) the copy dialog opens");
    assert_eq!(dialog_id(cx), resume::COPY_DIALOG_ID);
    assert!(
        f.terminal_commands().is_empty(),
        "nothing runs before the confirmation"
    );
}

/// A-005: a cross-project fork lists from/to for the transcript and the
/// session folder; confirming (Enter, no overwrite) copies both and runs the
/// copy without `--fork-session`.
#[gpui_kit::test]
fn cross_project_fork_copies_after_confirmation(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let session_dir = path.parent().unwrap().join(stem(&path));
    std::fs::create_dir_all(session_dir.join("subagents")).unwrap();
    std::fs::write(session_dir.join("subagents").join("agent.jsonl"), "{}\n").unwrap();
    let workspace = project_dir(&world, "newproj");
    let f = OpsFixture::open(
        cx,
        &world,
        &["--workspace", &workspace.display().to_string()],
    );
    f.select(cx, &path);
    f.press(cx, "ctrl-f");
    f.settle(cx);
    assert!(has_dialog(cx, f.handle()));
    assert!(f.terminal_commands().is_empty());

    // The dialog's content is core's plan: from/to pairs for both items.
    let target = f.target(cx, &path);
    let plan = plan_for(cx, &f, &target, true);
    let copy = plan.copy.clone().expect("a copy plan");
    assert_eq!(
        resume::copy_title(&target, &plan),
        format!("Copy \"Fix login bug\" to {}?", workspace.display())
    );
    let items = resume::copy_items(&copy);
    assert_eq!(items.len(), 4, "from/to for the transcript and the folder");
    assert!(items[0].prefix.as_deref() == Some("from") && items[1].prefix.as_deref() == Some("to"));
    assert!(items[2].kind == crate::widgets::PathKind::Folder);

    render(cx, f.handle());
    f.press(cx, "enter");
    f.settle(cx);
    assert!(!has_dialog(cx, f.handle()));
    let target_dir = copy.target_dir.clone();
    assert!(target_dir.join(format!("{}.jsonl", stem(&path))).is_file());
    assert!(
        target_dir
            .join(stem(&path))
            .join("subagents")
            .join("agent.jsonl")
            .is_file()
    );
    let command = only_command(&f);
    assert!(
        command.contains(&format!("--resume {}", stem(&path))),
        "{command}"
    );
    assert!(
        !command.contains("--fork-session"),
        "the copy is the fork: {command}"
    );
}

/// A-013, X-003: an existing target is listed, the button is the danger one,
/// Enter does nothing, `y` confirms.
#[gpui_kit::test]
fn copy_dialog_with_overwrites_ignores_enter_and_confirms_with_y(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let workspace = project_dir(&world, "newproj");
    // A previous copy exists in the workspace's projects directory.
    let existing_dir = world
        .projects_root()
        .join(synth::encoded_project_dir(&workspace));
    std::fs::create_dir_all(&existing_dir).unwrap();
    let existing = existing_dir.join(format!("{}.jsonl", stem(&path)));
    std::fs::write(&existing, "previous copy\n").unwrap();
    let f = OpsFixture::open(
        cx,
        &world,
        &["--workspace", &workspace.display().to_string()],
    );
    f.select(cx, &path);
    f.press(cx, "ctrl-f");
    f.settle(cx);
    assert!(has_dialog(cx, f.handle()));
    let target = f.target(cx, &path);
    let plan = plan_for(cx, &f, &target, true);
    let copy = plan.copy.clone().unwrap();
    assert_eq!(copy.existing_targets, vec![existing.clone()]);
    assert!(
        plan.warnings
            .iter()
            .any(|w| w.contains("Overwrites the existing copy")),
        "core warns; the dialog carries it"
    );

    render(cx, f.handle());
    f.press(cx, "enter");
    f.settle(cx);
    assert!(has_dialog(cx, f.handle()), "Enter leaves the dialog open");
    assert_eq!(
        std::fs::read_to_string(&existing).unwrap(),
        "previous copy\n",
        "nothing copied"
    );
    assert!(f.terminal_commands().is_empty());

    f.press(cx, "y");
    f.settle(cx);
    assert!(!has_dialog(cx, f.handle()));
    assert_ne!(
        std::fs::read_to_string(&existing).unwrap(),
        "previous copy\n",
        "overwritten"
    );
    only_command(&f);
}

/// `n` and Esc cancel the copy; nothing is copied or run, and the caller
/// hears `CANCELLED`.
#[gpui_kit::test]
fn copy_dialog_cancel_copies_nothing(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let workspace = project_dir(&world, "newproj");
    let f = OpsFixture::open(
        cx,
        &world,
        &["--workspace", &workspace.display().to_string()],
    );
    let target = f.target(cx, &path);
    let answers: Rc<RefCell<Vec<Result<ResumePlan, String>>>> = Rc::new(RefCell::new(Vec::new()));
    for keys in ["n", "escape"] {
        let sink = answers.clone();
        cx.update_window(f.handle(), |_, window, cx| {
            plan_and_confirm(
                target.clone(),
                true,
                window,
                cx,
                Box::new(move |result, _, _| sink.borrow_mut().push(result)),
            )
        })
        .unwrap();
        f.settle(cx);
        assert!(has_dialog(cx, f.handle()));
        render(cx, f.handle());
        f.press(cx, keys);
        f.settle(cx);
        assert!(!has_dialog(cx, f.handle()), "{keys} closes the dialog");
    }
    let answers = answers.borrow();
    assert_eq!(answers.len(), 2);
    assert!(
        answers
            .iter()
            .all(|a| a.as_ref().err().map(String::as_str) == Some(CANCELLED))
    );
    let projects = world
        .projects_root()
        .join(synth::encoded_project_dir(&workspace));
    assert!(!projects.exists(), "nothing was copied");
    assert!(f.terminal_commands().is_empty());
}

/// A fork whose workspace maps to the session's own projects directory never
/// plans a copy onto itself.
#[gpui_kit::test]
fn fork_into_the_sessions_own_project_never_copies(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let workspace = project_dir(&world, "proj");
    let f = OpsFixture::open(
        cx,
        &world,
        &["--workspace", &workspace.display().to_string()],
    );
    f.select(cx, &path);
    f.press(cx, "ctrl-f");
    f.settle(cx);
    assert!(!has_dialog(cx, f.handle()), "no copy dialog");
    let command = only_command(&f);
    assert!(command.contains("--fork-session"), "{command}");
}

/// A-028: the copy confirmation captured its conversation; moving the list
/// selection before confirming still copies and runs the captured one.
#[gpui_kit::test]
fn copy_confirmation_acts_on_the_captured_conversation(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let first = claude_session(&world, "proj", 1, "First");
    let second = claude_session(&world, "other", 2, "Second");
    let workspace = project_dir(&world, "newproj");
    let f = OpsFixture::open(
        cx,
        &world,
        &["--workspace", &workspace.display().to_string()],
    );
    f.select(cx, &first);
    f.press(cx, "ctrl-f");
    f.settle(cx);
    assert!(has_dialog(cx, f.handle()));
    assert!(
        f.list_pane()
            .update(cx, |list, cx| list.select_path(&second, cx))
    );
    render(cx, f.handle());
    f.press(cx, "enter");
    f.settle(cx);
    let command = only_command(&f);
    assert!(command.contains(&stem(&first)), "{command}");
    assert!(!command.contains(&stem(&second)), "{command}");
}

fn plan_for(
    cx: &mut TestAppContext,
    f: &OpsFixture,
    target: &ConversationTarget,
    fork: bool,
) -> ResumePlan {
    let plan: Rc<RefCell<Option<ResumePlan>>> = Rc::new(RefCell::new(None));
    let sink = plan.clone();
    // Plan without confirming: read the dialog's plan from a copy-free call
    // path by planning with core directly.
    let _ = (&sink, fork);
    let result = cx.read(|cx| {
        let state = crate::globals::app_state(cx);
        let conversation = f
            .list_pane()
            .read(cx)
            .conversation(&target.path)
            .expect("listed");
        claude_history_core::resume::plan_resume(
            &state.env_with_workspace(),
            &conversation,
            state.workspace.as_ref(),
            fork,
            &state.settings.resume_default_args,
            &claude_history_core::resume::ProgramOverrides::from_gui_settings(&state.config.gui),
        )
    });
    result.expect("a plan")
}

/// A-006, A-007, A-008: Pi and OMP run their native commands, without the
/// Claude default arguments.
#[gpui_kit::test]
fn pi_and_omp_resume_and_fork_use_their_native_commands(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(
        &world,
        "",
        "[resume]\ndefault_args = [\"--model\", \"opus\"]",
    );
    let project = project_dir(&world, "native");
    let workspace = project_dir(&world, "ws");
    let pi = pi_session(&world, &project);
    let omp = omp_session(&world, &project);
    let f = OpsFixture::open(
        cx,
        &world,
        &["--workspace", &workspace.display().to_string()],
    );

    // The list's own spelling of the paths (separators).
    let (pi_path, omp_path) = (f.target(cx, &pi).path, f.target(cx, &omp).path);
    f.select(cx, &pi);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    let command = only_command(&f);
    assert!(command.contains("fake-pi"), "{command}");
    assert!(
        command.contains(&format!("--session {}", pi_path.display())),
        "{command}"
    );
    assert!(!command.contains("--model"), "A-008: {command}");

    f.recording.clear_calls();
    f.press(cx, "ctrl-f");
    f.settle(cx);
    let command = only_command(&f);
    assert!(
        command.contains(&format!("--fork {}", pi_path.display())),
        "{command}"
    );

    f.recording.clear_calls();
    f.select(cx, &omp);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    let command = only_command(&f);
    assert!(command.contains("fake-omp"), "{command}");
    assert!(
        command.contains(&format!("--resume {}", omp_path.display())),
        "{command}"
    );
    assert!(!command.contains("--model"), "A-008: {command}");

    f.recording.clear_calls();
    f.press(cx, "ctrl-f");
    f.settle(cx);
    let command = only_command(&f);
    assert!(
        command.contains(&format!("--fork {}", omp_path.display())),
        "{command}"
    );
}

/// A-010: without a workspace a session whose project is gone offers
/// "Choose workspace…" with core's exact text; no terminal call.
#[gpui_kit::test]
fn missing_workspace_toasts_core_text(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let path = claude_session_gone(&world, 1, "Gone project");
    let f = OpsFixture::open(cx, &world, &[]);
    f.select(cx, &path);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    assert_eq!(
        toast(cx),
        "Choose a workspace folder to resume this session"
    );
    assert!(f.terminal_commands().is_empty());
    // The toast's "Choose workspace…" button opens the folder prompt
    // (integrator: toast buttons are observable through `notify`).
    assert_eq!(
        cx.read(crate::notify::last_toast_actions),
        vec![gpui_kit::SharedString::from("Choose workspace…")]
    );
    f.recording.clear_calls();
    let pressed = cx
        .update_window(f.handle(), |_, window, cx| {
            crate::notify::trigger_last_toast_action("Choose workspace…", window, cx)
        })
        .unwrap();
    assert!(pressed);
    f.settle(cx);
    assert!(
        f.recording
            .calls()
            .iter()
            .any(|call| matches!(call, crate::services::ServiceCall::PromptOpen(_))),
        "Choose workspace… asks for a folder"
    );
    f.press(cx, "ctrl-f");
    f.settle(cx);
    assert_eq!(toast(cx), "Choose a workspace folder to fork this session");
    assert!(f.terminal_commands().is_empty());
}

/// A-014: a Pi session whose recorded directory is missing warns and runs in
/// the workspace.
#[gpui_kit::test]
fn pi_session_with_a_missing_directory_warns(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let missing = world.home().join("work").join("nowhere");
    let pi = pi_session(&world, &missing);
    let workspace = project_dir(&world, "ws");
    let f = OpsFixture::open(
        cx,
        &world,
        &["--workspace", &workspace.display().to_string()],
    );
    let target = f.target(cx, &pi);
    let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    cx.update_window(f.handle(), |_, window, cx| {
        plan_and_confirm(
            target,
            false,
            window,
            cx,
            Box::new(move |result, _, cx| {
                // The warning toast is up when the caller continues.
                sink.borrow_mut()
                    .push(crate::globals::last_toast(cx).to_string());
                assert!(result.is_ok());
            }),
        )
    })
    .unwrap();
    f.settle(cx);
    assert_eq!(
        seen.borrow().as_slice(),
        [format!(
            "Session directory {} does not exist",
            missing.display()
        )]
    );
}

/// A program that cannot be found: the documented toast, no terminal call.
#[gpui_kit::test]
fn missing_program_toasts_with_a_settings_hint(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    // No claude_path and no PATH in the sandbox environment.
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &[]);
    f.select(cx, &path);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    assert_eq!(
        toast(cx),
        "Could not find 'claude' on PATH. Set its path in Settings → General."
    );
    assert!(f.terminal_commands().is_empty());
}

/// An early non-zero exit is an error (no retry: it ran).
#[gpui_kit::test]
fn early_nonzero_exit_is_an_error_toast(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &[]);
    f.recording.set_spawn_result(Ok(Some(3)));
    f.select(cx, &path);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    let text = toast(cx);
    assert!(text.starts_with("Failed to launch terminal: "), "{text}");
    assert!(text.ends_with("exited with code 3"), "{text}");
    assert_eq!(f.terminal_commands().len(), 1, "no retry");
}

/// A spawn error with one candidate: the last error is the toast.
#[gpui_kit::test]
fn spawn_error_is_reported_after_the_last_candidate(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &[]);
    f.recording.set_spawn_result(Err("boom".to_string()));
    f.select(cx, &path);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    assert_eq!(toast(cx), "Failed to launch terminal: boom");
    assert!(!f.terminal_commands().is_empty());
}

/// A-015: `[gui] terminal` selects the launcher (a Linux emulator name here).
#[gpui_kit::test]
fn configured_terminal_selects_the_launcher(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "terminal = \"kitty\"", "");
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &[]);
    f.select(cx, &path);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    let commands = f.recording.terminal_commands();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].program.to_string_lossy(), "kitty");
    assert!(
        toast(cx).starts_with("Opened \"Fix login bug\" in kitty"),
        "{}",
        toast(cx)
    );
}

/// A-016: arguments `cmd.exe` cannot pass are rejected with core's error and
/// no terminal call (Windows batch programs).
#[cfg(windows)]
#[gpui_kit::test]
fn arguments_cmd_cannot_pass_are_rejected(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "[resume]\ndefault_args = [\"line\\nbreak\"]");
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &[]);
    f.select(cx, &path);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    assert!(f.terminal_commands().is_empty());
    let text = toast(cx);
    assert!(
        text.contains("argument contains a line break, which cannot be passed through cmd.exe"),
        "core's error text: {text}"
    );
}

/// A-062: a target of unknown source records nothing; the entry points are
/// no-ops.
#[gpui_kit::test]
fn unknown_source_targets_are_unavailable(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &[]);
    let known = f.target(cx, &path);
    let unknown = ConversationTarget {
        source: None,
        ..known
    };
    let answers: Rc<RefCell<Vec<Result<ResumePlan, String>>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = answers.clone();
    cx.update_window(f.handle(), |_, window, cx| {
        resume::resume(unknown.clone(), false, window, cx);
        plan_and_confirm(
            unknown.clone(),
            true,
            window,
            cx,
            Box::new(move |result, _, _| sink.borrow_mut().push(result)),
        );
        crate::ops::rename::open_rename_dialog(unknown.clone(), window, cx);
        crate::ops::delete::open_delete_dialog(unknown, window, cx);
    })
    .unwrap();
    f.settle(cx);
    assert!(
        f.recording.calls().is_empty(),
        "no side effect: {:?}",
        f.recording.calls()
    );
    assert!(!has_dialog(cx, f.handle()));
    assert_eq!(
        answers
            .borrow()
            .iter()
            .map(|a| a.as_ref().err().cloned())
            .collect::<Vec<_>>(),
        vec![Some(resume::UNAVAILABLE.to_string())]
    );
    assert!(path.is_file(), "nothing deleted");
}

/// Unused import guard for helpers other modules may use.
#[allow(dead_code)]
fn _paths(_: PathBuf) {
    let _ = claude_session_in;
}

/// A-062: a single-file window on an unparsable file has an unknown source:
/// dispatching Resume, Fork, Rename and Delete records nothing and opens no
/// dialog (the window handles none of them, TUI parity).
#[gpui_kit::test]
fn single_file_window_with_an_unknown_source_does_nothing(cx: &mut TestAppContext) {
    use crate::actions::{Delete, Fork, Rename, Resume};
    use crate::test_support::fixtures_ops::unparsable_file;
    use crate::test_support::{open_single_file, recording};

    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let path = unparsable_file(&world);
    let handle = open_single_file(cx, &world, &path);
    let recorded = recording(cx);
    cx.dispatch_action(handle, Resume);
    cx.dispatch_action(handle, Fork);
    cx.dispatch_action(handle, Rename);
    cx.dispatch_action(handle, Delete);
    for _ in 0..10 {
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(recorded.calls().is_empty(), "{:?}", recorded.calls());
    assert!(!has_dialog(cx, handle));
    assert!(path.is_file(), "nothing deleted");
}

/// A-014: a Pi session whose recorded directory is missing runs in the
/// workspace (the recorded command's directory).
#[gpui_kit::test]
fn pi_session_with_a_missing_directory_runs_in_the_workspace(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let missing = world.home().join("work").join("nowhere");
    let pi = pi_session(&world, &missing);
    let workspace = project_dir(&world, "ws");
    let f = OpsFixture::open(
        cx,
        &world,
        &["--workspace", &workspace.display().to_string()],
    );
    f.select(cx, &pi);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    only_command(&f);
    assert_runs_in(&f, &workspace);
}

/// A-015: a command template expands as core builds it (`{cwd}`, `{args}`).
#[gpui_kit::test]
fn a_command_template_launches_through_the_given_program(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "terminal = \"kitty --directory {cwd} {args}\"", "");
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &[]);
    f.select(cx, &path);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    let commands = f.recording.terminal_commands();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].program.to_string_lossy(), "kitty");
    let text = commands[0].display();
    let project = project_dir(&world, "proj").display().to_string();
    assert!(text.contains("--directory"), "{text}");
    assert!(text.contains(&project), "{text}");
    assert!(text.contains("fake-claude"), "{text}");
    assert!(
        text.contains(&format!("--resume {}", stem(&path))),
        "{text}"
    );
    assert_eq!(toast(cx), "Opened \"Fix login bug\" in custom terminal");
}

/// A-001/A-002: a double press (or key auto-repeat) starts one terminal; the
/// guard is released afterwards, so a later press starts another.
#[gpui_kit::test]
fn a_double_press_starts_one_terminal(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &[]);
    f.select(cx, &path);
    cx.simulate_keystrokes(f.handle(), "ctrl-r ctrl-r");
    f.settle(cx);
    assert_eq!(f.terminal_commands().len(), 1);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    assert_eq!(f.terminal_commands().len(), 2, "released after the launch");
}

/// The guard is also released when the launch fails or planning refuses.
#[gpui_kit::test]
fn the_resume_guard_is_released_after_a_failure(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let gone = claude_session_gone(&world, 2, "Gone project");
    let f = OpsFixture::open(cx, &world, &[]);
    f.recording.set_spawn_result(Err("boom".to_string()));
    f.select(cx, &path);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    assert_eq!(
        f.terminal_commands().len(),
        2,
        "the failed launch released it"
    );
    f.select(cx, &gone);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    assert_eq!(
        toast(cx),
        "Choose a workspace folder to resume this session"
    );
}
