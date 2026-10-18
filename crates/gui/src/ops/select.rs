//! Ctrl+O: picker output or Print and exit (PD5, DESIGN §11.13). WP3.
//!
//! * `--show-path` / `--show-id`: core `settings::picker_output` + `\n` to
//!   stdout, exit 0; an error goes to stderr (`Error: <e>`), exit 1.
//! * `--resume [--fork-session]`: the PD4 plan and confirmations, then Unix +
//!   TTY: `exec_in_current_terminal`; otherwise a new terminal window; then
//!   exit 0 with no output.
//! * No picker flag: "Print and exit" by stdout kind: redirected prints at
//!   once, a terminal asks first, no stdout answers with a toast.
//!
//! The printed text is the Ledger export (no ANSI) or, with `--plain` /
//! `[display] plain`, the CLI plain format, with the options resolved at
//! startup (never the live viewer toggles, A-060). With `--debug` the
//! diagnostics go to stderr first, in upstream's order.

use std::path::Path;

use claude_history_core::HistoryEnv;
use claude_history_core::debug::{self, DebugLevel};
use claude_history_core::debug_log::log_display_error;
use claude_history_core::export::{
    ExportFormat, ExportOptions, display_parse_errors, generate_export, plain_display,
};
use claude_history_core::history::Conversation;
use claude_history_core::maintenance::log_parse_errors;
use claude_history_core::settings::{PickerAction, picker_output, session_id_from_path};
use gpui_kit::{App, Window};

use crate::debug_sink::capture_level;
use crate::exit;
use crate::globals::{OpenTargets, app_state};
use crate::model::{ConversationTarget, StdoutKind};
use crate::notify::Toast;
use crate::ops::copy::{clear_dialog, main_list};
use crate::ops::resume::{exec_spec, launch_with, picker_uses_exec, plan_and_confirm};
use crate::services::services;
use crate::widgets::{ConfirmControl, ConfirmOptions, ConfirmOutcome, open_destructive_confirm};
use crate::workspace::startup::register_dialog;

/// The info toast when there is no stdout to print to.
pub const NOTHING_TO_PRINT: &str =
    "Nothing to print to: start the app from a terminal, or use Export…";

/// The QA / probe id of the Print and exit confirmation.
pub const PRINT_DIALOG_ID: &str = "print-and-exit";

/// Picker select / Print and exit with the captured target (`None`: nothing selected).
pub fn select_and_exit(target: Option<ConversationTarget>, window: &mut Window, cx: &mut App) {
    let Some(target) = target else {
        return;
    };
    // The list calls this inside its own update; the steps below read it.
    window.defer(cx, move |window, cx| select_now(target, window, cx));
}

fn select_now(target: ConversationTarget, window: &mut Window, cx: &mut App) {
    match app_state(cx).launch.picker {
        Some(action @ (PickerAction::ShowPath | PickerAction::ShowId)) => {
            print_picker_line(action, target, window, cx);
        }
        Some(PickerAction::Resume { fork }) => picker_resume(target, fork, window, cx),
        None => print_and_exit(target, window, cx),
    }
}

// ----- --show-path / --show-id ---------------------------------------------------------

/// The picker's output line (without the newline) for `action`, or the error
/// text (`Failed to run Claude CLI: Conversation filename is not valid
/// Unicode` for a non-UTF-8 stem, A-047).
pub fn picker_line(action: PickerAction, conversation: &Conversation) -> Result<String, String> {
    picker_output(action, conversation)
        .map(Option::unwrap_or_default)
        .map_err(|error| error.to_string())
}

/// [`picker_line`] for a target that is not in the corpus (only the captured
/// values are known): the path, the header id, or the stem.
fn picker_line_for_target(
    action: PickerAction,
    target: &ConversationTarget,
) -> Result<String, String> {
    match action {
        PickerAction::ShowId if target.session_id == target.file_stem => {
            session_id_from_path(&target.path).map_err(|error| error.to_string())
        }
        PickerAction::ShowId => Ok(target.session_id.clone()),
        _ => Ok(target.path.display().to_string()),
    }
}

fn print_picker_line(
    action: PickerAction,
    target: ConversationTarget,
    window: &mut Window,
    cx: &mut App,
) {
    let conversation = main_list(cx).and_then(|list| list.read(cx).conversation(&target.path));
    let line = match conversation {
        Some(conversation) => picker_line(action, &conversation),
        None => picker_line_for_target(action, &target),
    };
    finish_picker_line(line, &target, window, cx);
}

/// Print the picker's `line` (plus `\n`) and quit 0, or write `Error: <e>` to
/// stderr and quit 1.
pub fn finish_picker_line(
    line: Result<String, String>,
    target: &ConversationTarget,
    window: &mut Window,
    cx: &mut App,
) {
    match line {
        Ok(line) => print_and_quit(line + "\n", target, window, cx),
        Err(error) => {
            services(cx).write_stderr(&format!("Error: {error}\n"));
            exit::quit_with_code(exit::EXIT_ERROR, cx);
        }
    }
}

/// Write `text` to stdout and quit, or say there is nothing to print to.
fn print_and_quit(text: String, target: &ConversationTarget, window: &mut Window, cx: &mut App) {
    if services(cx).stdout_kind() == StdoutKind::None {
        nothing_to_print(target, window, cx);
        return;
    }
    exit::quit_with_output(text, cx);
}

fn nothing_to_print(target: &ConversationTarget, window: &mut Window, cx: &mut App) {
    let target = target.clone();
    Toast::info(NOTHING_TO_PRINT)
        .action("Export…", move |window, cx| {
            let options = crate::workspace::routing::default_export_options(cx);
            crate::ops::export::open_export_menu(target.clone(), options, window, cx);
        })
        .show(window, cx);
}

// ----- --resume --------------------------------------------------------------------------

pub(crate) fn picker_resume(
    target: ConversationTarget,
    fork: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let title = target.title.clone();
    plan_and_confirm(
        target,
        fork,
        window,
        cx,
        Box::new(move |result, window, cx| {
            // Errors were toasted; the picker stays open.
            let Ok(plan) = result else {
                return;
            };
            let services = services(cx);
            if picker_uses_exec(cfg!(unix), services.stdin_is_tty(), services.stdout_kind()) {
                match exec_spec(&plan, cx) {
                    Ok(spec) => match services.exec_in_current_terminal(&spec) {
                        // The launching terminal took over (the real call never returns).
                        Ok(()) => exit::quit_without_output(cx),
                        Err(error) => {
                            Toast::error(format!("Failed to run Claude CLI: {error}"))
                                .show(window, cx);
                        }
                    },
                    Err(error) => Toast::error(error.to_string()).show(window, cx),
                }
            } else {
                launch_with(
                    &plan,
                    title,
                    Some(Box::new(|started, _, cx| {
                        if started {
                            exit::quit_without_output(cx);
                        }
                    })),
                    window,
                    cx,
                );
            }
        }),
    );
}

// ----- Print and exit ----------------------------------------------------------------------

fn print_and_exit(target: ConversationTarget, window: &mut Window, cx: &mut App) {
    match services(cx).stdout_kind() {
        StdoutKind::Redirected => print_export(target, window, cx),
        StdoutKind::Terminal => confirm_print(target, window, cx),
        StdoutKind::None => nothing_to_print(&target, window, cx),
    }
}

/// The title of the print confirmation (A-063).
pub fn print_title(target: &ConversationTarget) -> String {
    format!("Print \"{}\" to the terminal and quit?", target.title)
}

fn confirm_print(target: ConversationTarget, window: &mut Window, cx: &mut App) {
    let slot: std::rc::Rc<std::cell::RefCell<Option<ConfirmControl>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let id = {
        let slot = slot.clone();
        OpenTargets::register(
            cx,
            window.window_handle(),
            target.path.clone(),
            target.title.clone(),
            Box::new(move |_, cx| {
                let control = slot.borrow().clone();
                if let Some(control) = control {
                    control.close(cx);
                }
            }),
        )
    };
    register_dialog(cx, PRINT_DIALOG_ID);
    let options = ConfirmOptions::non_destructive(print_title(&target), "Print and quit")
        .on_closed(move |cx| {
            OpenTargets::unregister(cx, id);
            clear_dialog(cx, PRINT_DIALOG_ID);
        });
    let control = open_destructive_confirm(options, window, cx, move |control, window, cx| {
        control.close(cx);
        print_export(target.clone(), window, cx);
        ConfirmOutcome::Close
    });
    *slot.borrow_mut() = Some(control);
}

/// The text Ctrl+O prints: the Ledger export, or the plain CLI format.
pub fn print_text(path: &Path, plain: bool, options: ExportOptions) -> Result<String, String> {
    let text = if plain {
        plain_display(path, options)
    } else {
        generate_export(path, ExportFormat::Ledger, options)
    };
    text.map_err(|error| error.to_string())
}

/// `--debug` diagnostics before the output, in upstream's order (PD5): the
/// parse errors logged to debug.log, then the display parse errors.
pub fn print_diagnostics(
    env: &HistoryEnv,
    conversation: Option<&Conversation>,
    path: &Path,
    level: Option<DebugLevel>,
) {
    if let Some(conversation) = conversation {
        match log_parse_errors(env, conversation) {
            Ok(Some(message)) => debug::info(level, &message),
            Ok(None) => {}
            Err(error) => debug::warn(
                level,
                &format!("Failed to write parse errors to log: {error}"),
            ),
        }
    }
    if let Ok(errors) = display_parse_errors(path) {
        for error in errors {
            debug::error(
                level,
                &format!(
                    "Failed to parse line {}: {}",
                    error.line_number, error.error
                ),
            );
            if let Some(log) = env.debug_log_file() {
                let _ = log_display_error(&log, path, error.line_number, &error.error, &error.line);
            }
        }
    }
}

/// Generate the text in the background, print it and quit.
fn print_export(target: ConversationTarget, window: &mut Window, cx: &mut App) {
    let state = app_state(cx);
    let options = ExportOptions {
        show_tools: state.settings.show_tools,
        show_thinking: state.settings.show_thinking,
    };
    let config_plain = state
        .config
        .effective
        .display
        .as_ref()
        .and_then(|display| display.plain)
        .unwrap_or(false);
    let plain = state.launch.args.plain || config_plain;
    let debug_requested = state.launch.args.debug.is_some();
    let env = state.env_with_workspace();
    let conversation = main_list(cx).and_then(|list| list.read(cx).conversation(&target.path));
    let level = Some(capture_level(cx));
    let path = target.path.clone();
    let task = cx.background_executor().spawn(async move {
        if debug_requested {
            print_diagnostics(&env, conversation.as_deref(), &path, level);
        }
        print_text(&path, plain, options)
    });
    window
        .spawn(&*cx, async move |cx| {
            let result = task.await;
            cx.update(|_, cx| match result {
                Ok(text) => exit::quit_with_output(text, cx),
                Err(error) => {
                    services(cx).write_stderr(&format!("Error: {error}\n"));
                    exit::quit_with_code(exit::EXIT_ERROR, cx);
                }
            })
            .ok();
        })
        .detach();
}
