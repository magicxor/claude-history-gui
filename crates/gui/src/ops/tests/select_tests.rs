//! G and P tests of Ctrl+O: picker output and Print and exit (A-046…A-052,
//! A-058…A-060, A-063). Output only ever goes to `RecordingServices`.

use std::path::PathBuf;

use claude_history_core::debug::{self, DebugLevel};
use claude_history_core::export::{ExportFormat, ExportOptions, generate_export, plain_display};
use claude_history_core::history::process_conversation_file;
use claude_history_core::settings::PickerAction;
use gpui_kit::{AppContext as _, TestAppContext};

use super::{dialog_id, has_dialog, render, toast};
use crate::exit::quit_requested;
use crate::model::StdoutKind;
use crate::ops::resume::picker_uses_exec;
use crate::ops::select::{self, NOTHING_TO_PRINT, PRINT_DIALOG_ID, picker_line};
use crate::services::ServiceCall;
use crate::test_support::fixtures_ops::{
    OpsFixture, claude_session, pi_session, project_dir, tool_session, write_gui_config,
};
use crate::test_support::{TestWorld, global_lock};

fn stdouts(f: &OpsFixture) -> Vec<String> {
    f.recording
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            ServiceCall::Stdout(text) => Some(text),
            _ => None,
        })
        .collect()
}

fn quit(cx: &TestAppContext) -> Option<i32> {
    cx.read(quit_requested)
}

fn ctrl_o(cx: &mut TestAppContext, f: &OpsFixture, path: &std::path::Path) {
    f.select(cx, path);
    f.press(cx, "ctrl-o");
    f.settle(cx);
}

/// Removes the process-wide debug sink when the test ends.
struct ResetSink;

impl Drop for ResetSink {
    fn drop(&mut self) {
        debug::reset_sink();
    }
}

/// A-046: `--show-path` prints the path and quits 0; `--show-id` prints the
/// session ID (the header id for Pi).
#[gpui_kit::test]
fn show_path_prints_the_path_and_quits(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &["--show-path"]);
    f.recording.set_stdout_kind(StdoutKind::Redirected);
    let target = f.target(cx, &path);
    ctrl_o(cx, &f, &path);
    assert_eq!(stdouts(&f), vec![format!("{}\n", target.path.display())]);
    assert_eq!(quit(cx), Some(0));
}

#[gpui_kit::test]
fn show_id_prints_the_session_id(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let claude = claude_session(&world, "proj", 1, "Fix login bug");
    let pi = pi_session(&world, &project_dir(&world, "native"));
    let f = OpsFixture::open(cx, &world, &["--show-id"]);
    f.recording.set_stdout_kind(StdoutKind::Redirected);
    ctrl_o(cx, &f, &pi);
    assert_eq!(
        stdouts(&f),
        vec!["01912345-6789-7abc-8def-0123456789ab\n".to_string()]
    );
    assert_eq!(quit(cx), Some(0));
    // A Claude row prints its UUID.
    f.recording.clear_calls();
    ctrl_o(cx, &f, &claude);
    let uuid = claude.file_stem().unwrap().to_string_lossy().into_owned();
    assert_eq!(stdouts(&f), vec![format!("{uuid}\n")]);
}

/// A-047 (P): the picker mapping of a non-UTF-8 file stem is the error core
/// gives; the GUI prints `Error: <e>` on stderr and exits 1.
#[test]
fn picker_line_reports_a_non_unicode_stem() {
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let mut conversation = process_conversation_file(path, None, None)
        .unwrap()
        .expect("a conversation");
    conversation.session_id.clear();
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt as _;
        let name: Vec<u16> = vec![0x61, 0xD800, 0x2E, 0x6A, 0x73, 0x6F, 0x6E, 0x6C];
        conversation.path = PathBuf::from(std::ffi::OsString::from_wide(&name));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        let name = b"a\xFF.jsonl".to_vec();
        conversation.path = PathBuf::from(std::ffi::OsString::from_vec(name));
    }
    assert_eq!(
        picker_line(PickerAction::ShowId, &conversation),
        Err("Failed to run Claude CLI: Conversation filename is not valid Unicode".to_string())
    );
    // The path itself needs no Unicode.
    assert!(picker_line(PickerAction::ShowPath, &conversation).is_ok());
}

/// A-047 (G): the error goes to stderr as `Error: <e>` and the exit code is 1.
/// (The non-Unicode row cannot be listed, so the shared helper is driven.)
#[gpui_kit::test]
fn picker_errors_exit_with_code_one(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &["--show-id"]);
    f.recording.set_stdout_kind(StdoutKind::Redirected);
    let target = f.target(cx, &path);
    cx.update_window(f.handle(), |_, window, cx| {
        select::finish_picker_line(
            Err("Failed to run Claude CLI: Conversation filename is not valid Unicode".to_string()),
            &target,
            window,
            cx,
        )
    })
    .unwrap();
    assert_eq!(
        f.recording.stderr(),
        "Error: Failed to run Claude CLI: Conversation filename is not valid Unicode\n"
    );
    assert!(stdouts(&f).is_empty());
    assert_eq!(quit(cx), Some(1));
}

/// A-063: with no stdout the picker and Print and exit answer with a toast and
/// stay open.
#[gpui_kit::test]
fn no_stdout_toasts_and_stays_open(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &[]);
    ctrl_o(cx, &f, &path);
    assert_eq!(toast(cx), NOTHING_TO_PRINT);
    assert_eq!(quit(cx), None);
    assert!(stdouts(&f).is_empty());
    // The picker flavour too.
}

#[gpui_kit::test]
fn picker_without_stdout_toasts_and_stays_open(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &["--show-path"]);
    ctrl_o(cx, &f, &path);
    assert_eq!(toast(cx), NOTHING_TO_PRINT);
    assert_eq!(quit(cx), None);
}

/// A-058, A-046: redirected stdout prints the Ledger export at once, without
/// ANSI, and quits.
#[gpui_kit::test]
fn redirected_stdout_prints_the_ledger_export(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &[]);
    f.recording.set_stdout_kind(StdoutKind::Redirected);
    let options = cx.read(|cx| {
        let settings = &crate::globals::app_state(cx).settings;
        ExportOptions {
            show_tools: settings.show_tools,
            show_thinking: settings.show_thinking,
        }
    });
    let expected = generate_export(&path, ExportFormat::Ledger, options).unwrap();
    ctrl_o(cx, &f, &path);
    assert_eq!(stdouts(&f), vec![expected.clone()]);
    assert!(!expected.contains("\u{1b}["), "no ANSI");
    assert_eq!(quit(cx), Some(0));
}

/// A-063: a terminal asks first; Enter (not destructive) prints and quits;
/// Esc cancels.
#[gpui_kit::test]
fn terminal_stdout_asks_before_printing(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &[]);
    f.recording.set_stdout_kind(StdoutKind::Terminal);
    ctrl_o(cx, &f, &path);
    assert!(has_dialog(cx, f.handle()));
    assert_eq!(dialog_id(cx), PRINT_DIALOG_ID);
    assert_eq!(
        crate::ops::select::print_title(&f.target(cx, &path)),
        "Print \"Fix login bug\" to the terminal and quit?"
    );
    assert!(stdouts(&f).is_empty() && quit(cx).is_none());
    render(cx, f.handle());
    f.press(cx, "escape");
    f.settle(cx);
    assert!(!has_dialog(cx, f.handle()) && stdouts(&f).is_empty() && quit(cx).is_none());

    ctrl_o(cx, &f, &path);
    render(cx, f.handle());
    f.press(cx, "enter");
    f.settle(cx);
    assert_eq!(stdouts(&f).len(), 1, "Print and quit printed");
    assert_eq!(quit(cx), Some(0));
}

/// A-060: the output uses the options resolved at startup, never the live
/// viewer toggles.
#[gpui_kit::test]
fn output_options_are_the_startup_ones(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = tool_session(&world, "proj", 1);
    let f = OpsFixture::open(cx, &world, &[]);
    f.recording.set_stdout_kind(StdoutKind::Redirected);
    let with_tools = generate_export(
        &path,
        ExportFormat::Ledger,
        ExportOptions {
            show_tools: true,
            show_thinking: false,
        },
    )
    .unwrap();
    let without_tools = generate_export(
        &path,
        ExportFormat::Ledger,
        ExportOptions {
            show_tools: false,
            show_thinking: false,
        },
    )
    .unwrap();
    assert_ne!(with_tools, without_tools, "the fixture discriminates");
    // The viewer is switched to full tool display before Ctrl+O.
    f.open_in_viewer(cx, &path);
    cx.dispatch_action(f.handle(), crate::actions::SetToolsFull);
    cx.run_until_parked();
    f.select(cx, &path);
    f.press(cx, "ctrl-o");
    f.settle(cx);
    assert_eq!(stdouts(&f), vec![without_tools]);
}

#[gpui_kit::test]
fn show_tools_flag_prints_the_tools(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = tool_session(&world, "proj", 1);
    let f = OpsFixture::open(cx, &world, &["--show-tools"]);
    f.recording.set_stdout_kind(StdoutKind::Redirected);
    let with_tools = generate_export(
        &path,
        ExportFormat::Ledger,
        ExportOptions {
            show_tools: true,
            show_thinking: false,
        },
    )
    .unwrap();
    ctrl_o(cx, &f, &path);
    assert_eq!(stdouts(&f), vec![with_tools]);
}

/// `--plain` and `[display] plain = true` print the CLI plain format.
#[gpui_kit::test]
fn plain_flag_prints_the_cli_plain_format(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &["--plain"]);
    f.recording.set_stdout_kind(StdoutKind::Redirected);
    let options = cx.read(|cx| {
        let settings = &crate::globals::app_state(cx).settings;
        ExportOptions {
            show_tools: settings.show_tools,
            show_thinking: settings.show_thinking,
        }
    });
    let expected = plain_display(&path, options).unwrap();
    ctrl_o(cx, &f, &path);
    assert_eq!(stdouts(&f), vec![expected.clone()]);
    assert!(expected.starts_with("You:"));
}

#[gpui_kit::test]
fn display_plain_config_prints_the_cli_plain_format(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    world.write_config(Some("[display]\nplain = true\n"), None);
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &[]);
    f.recording.set_stdout_kind(StdoutKind::Redirected);
    ctrl_o(cx, &f, &path);
    let printed = stdouts(&f);
    assert_eq!(printed.len(), 1);
    assert!(printed[0].starts_with("You:"), "{printed:?}");
}

/// A session with `bad` malformed lines.
fn malformed_session(world: &TestWorld, bad: usize) -> PathBuf {
    let cwd = project_dir(world, "proj");
    let uuid = crate::test_support::synth::uuid(7);
    let mut lines = crate::test_support::synth::claude_conversation(&uuid, &cwd, "Malformed", 3, 1);
    for ix in 0..bad {
        lines.insert(1 + ix, format!("{{this is not json {ix}"));
    }
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    world.add_claude_session(
        &crate::test_support::synth::encoded_project_dir(&cwd),
        &uuid,
        &refs,
    )
}

fn install_sink(cx: &mut TestAppContext, level: DebugLevel) {
    cx.update(|cx| crate::debug_sink::install(cx, Some(level)));
}

/// A-048: with `--debug info` the parse errors are logged and announced on
/// stderr before the one stdout write.
#[gpui_kit::test]
fn debug_info_logs_parse_errors_before_printing(cx: &mut TestAppContext) {
    let _lock = global_lock();
    let _reset = ResetSink;
    let world = TestWorld::new();
    let path = malformed_session(&world, 2);
    let f = OpsFixture::open(cx, &world, &["--debug=info"]);
    install_sink(cx, DebugLevel::Info);
    f.recording.set_stdout_kind(StdoutKind::Redirected);
    f.recording.clear_calls();
    ctrl_o(cx, &f, &path);
    let calls = f.recording.calls();
    let stdout_at = calls
        .iter()
        .position(|call| matches!(call, ServiceCall::Stdout(_)))
        .expect("the output was printed");
    let logged = calls
        .iter()
        .position(|call| matches!(call, ServiceCall::Stderr(text) if text.starts_with("[INFO] Logged 2 parse error(s) to ")))
        .unwrap_or_else(|| panic!("no INFO line in {calls:?}"));
    assert!(logged < stdout_at, "stderr diagnostics come first");
    assert!(
        calls[stdout_at + 1..]
            .iter()
            // (The core debug sink is process-wide: other tests' loader lines
            // may arrive too, so only this conversation's diagnostics count.)
            .all(|call| !matches!(call, ServiceCall::Stderr(text) if text.contains("parse"))),
        "the diagnostics all precede the output: {calls:?}"
    );
    let log = std::fs::read_to_string(world.env.debug_log_file().unwrap()).unwrap();
    assert!(log.contains("=== Parse Errors:"), "{log}");
}

/// A-059: with `--debug error` the display parse errors are reported at error
/// level (stderr + debug.log block) and no INFO line appears.
#[gpui_kit::test]
fn debug_error_reports_display_parse_errors_only(cx: &mut TestAppContext) {
    let _lock = global_lock();
    let _reset = ResetSink;
    let world = TestWorld::new();
    let path = malformed_session(&world, 1);
    let f = OpsFixture::open(cx, &world, &["--debug=error"]);
    install_sink(cx, DebugLevel::Error);
    f.recording.set_stdout_kind(StdoutKind::Redirected);
    f.recording.clear_calls();
    ctrl_o(cx, &f, &path);
    let calls = f.recording.calls();
    let stderr: Vec<String> = calls
        .iter()
        .filter_map(|call| match call {
            ServiceCall::Stderr(text) => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        stderr
            .iter()
            .any(|line| line.starts_with("[ERROR] Failed to parse line 2: ")),
        "{stderr:?}"
    );
    assert!(
        stderr.iter().all(|line| !line.starts_with("[INFO]")),
        "{stderr:?}"
    );
    let log = std::fs::read_to_string(world.env.debug_log_file().unwrap()).unwrap();
    assert!(log.contains("=== Display Parse Error:"), "{log}");
}

/// A-050: with `--debug debug` the picker's stdout is exactly the result line;
/// every debug line is a stderr call.
#[gpui_kit::test]
fn picker_stdout_stays_clean_with_debug(cx: &mut TestAppContext) {
    let _lock = global_lock();
    let _reset = ResetSink;
    let world = TestWorld::new();
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &["--show-path", "--debug=debug"]);
    install_sink(cx, DebugLevel::Debug);
    f.recording.set_stdout_kind(StdoutKind::Redirected);
    f.recording.clear_calls();
    let target = f.target(cx, &path);
    ctrl_o(cx, &f, &path);
    let calls = f.recording.calls();
    let outs = stdouts(&f);
    assert_eq!(outs, vec![format!("{}\n", target.path.display())]);
    assert!(
        calls
            .iter()
            .all(|call| matches!(call, ServiceCall::Stdout(_) | ServiceCall::Stderr(_))),
        "{calls:?}"
    );
}

/// A-052, picker `--resume` (Windows and non-TTY: a new terminal, then quit 0
/// without output).
#[cfg(windows)]
#[gpui_kit::test]
fn picker_resume_opens_a_terminal_and_quits(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &["--resume"]);
    f.recording.set_stdout_kind(StdoutKind::Terminal);
    ctrl_o(cx, &f, &path);
    assert_eq!(f.terminal_commands().len(), 1);
    assert!(stdouts(&f).is_empty(), "nothing on stdout");
    assert_eq!(quit(cx), Some(0));
}

/// A-001 (G): Ctrl+R / Ctrl+F in any picker mode launch and then quit with no
/// output (upstream's picker returns after `Resume`/`ForkResume`).
#[cfg(windows)]
fn picker_key_resumes_and_quits(cx: &mut TestAppContext, flag: &str, key: &str) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &[flag]);
    f.recording.set_stdout_kind(StdoutKind::Redirected);
    f.select(cx, &path);
    f.press(cx, key);
    f.settle(cx);
    assert_eq!(f.terminal_commands().len(), 1, "{flag} {key}");
    assert!(stdouts(&f).is_empty(), "{flag} {key}: no stdout");
    assert_eq!(quit(cx), Some(0), "{flag} {key}");
}

#[cfg(windows)]
#[gpui_kit::test]
fn picker_show_id_ctrl_r_resumes_and_quits(cx: &mut TestAppContext) {
    picker_key_resumes_and_quits(cx, "--show-id", "ctrl-r");
}

#[cfg(windows)]
#[gpui_kit::test]
fn picker_show_path_ctrl_f_forks_and_quits(cx: &mut TestAppContext) {
    picker_key_resumes_and_quits(cx, "--show-path", "ctrl-f");
}

#[cfg(windows)]
#[gpui_kit::test]
fn picker_resume_mode_ctrl_r_resumes_and_quits(cx: &mut TestAppContext) {
    picker_key_resumes_and_quits(cx, "--resume", "ctrl-r");
}

/// A-001: a failed launch in picker mode keeps the picker open.
#[gpui_kit::test]
fn picker_ctrl_r_failure_stays_open(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &["--show-id"]);
    f.recording.set_spawn_result(Err("boom".to_string()));
    f.select(cx, &path);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    assert_eq!(quit(cx), None);
    assert_eq!(toast(cx), "Failed to launch terminal: boom");
}

/// A failed launch keeps the picker open (the toast explains).
#[gpui_kit::test]
fn picker_resume_failure_stays_open(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &["--resume"]);
    f.recording.set_spawn_result(Err("boom".to_string()));
    ctrl_o(cx, &f, &path);
    assert_eq!(quit(cx), None);
    assert_eq!(toast(cx), "Failed to launch terminal: boom");
}

/// A-052: Unix with a TTY on stdin and stdout execs in the launching terminal.
#[cfg(unix)]
#[gpui_kit::test]
fn picker_resume_execs_in_the_launching_terminal(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let path = claude_session(&world, "proj", 1, "Fix login bug");
    let f = OpsFixture::open(cx, &world, &["--resume"]);
    f.recording.set_stdin_is_tty(true);
    f.recording.set_stdout_kind(StdoutKind::Terminal);
    ctrl_o(cx, &f, &path);
    let execs: Vec<_> = f
        .recording
        .calls()
        .into_iter()
        .filter(|call| matches!(call, ServiceCall::Exec(_)))
        .collect();
    assert_eq!(execs.len(), 1);
    assert!(f.terminal_commands().is_empty());
    assert_eq!(quit(cx), Some(0));
}

/// The exec decision (P): only Unix with a TTY on stdin and stdout.
#[test]
fn exec_decision_needs_unix_and_two_ttys() {
    assert!(picker_uses_exec(true, true, StdoutKind::Terminal));
    assert!(!picker_uses_exec(false, true, StdoutKind::Terminal));
    assert!(!picker_uses_exec(true, false, StdoutKind::Terminal));
    assert!(!picker_uses_exec(true, true, StdoutKind::Redirected));
    assert!(!picker_uses_exec(true, true, StdoutKind::None));
}
