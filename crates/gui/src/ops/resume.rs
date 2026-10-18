//! Resume and fork (PD4, DESIGN §11.10). WP3.
//!
//! Planning is core's (`resume::plan_resume_target`), terminal commands are
//! core's (`resume::terminal`), the terminal is started only through
//! `PlatformServices::spawn_terminal`. A cross-project copy is confirmed first
//! (§5.8) and runs only through `ResumePlan::execute_copy` after that
//! confirmation. The GUI never calls `core::resume::launch` (it copies without
//! asking).
//!
//! The target is captured by the caller (PD15) and every step here uses it:
//! the conversation behind it is looked up by path, never by the selection.

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use claude_history_core::AppError;
use claude_history_core::HistoryEnv;
use claude_history_core::WorkspaceScope;
use claude_history_core::history::process_conversation_file;
use claude_history_core::resume::terminal::{
    LaunchOptions, LaunchSpec, TerminalCommand, launcher_candidates, prepare_launcher_command,
};
use claude_history_core::resume::{
    CopyPlan, ProgramOverrides, ResumePlan, ResumeTarget, plan_resume_target,
};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Global, IntoElement as _, ParentElement as _, SharedString, Styled as _, Window, div,
};

use crate::actions::ChooseWorkspace;
use crate::globals::{OpenTargets, app_state};
use crate::model::{ConversationTarget, SettingsPage};
use crate::notify::Toast;
use crate::ops::copy::{can_act, clear_dialog, live_target};
use crate::services::{Services, services};
use crate::widgets::{
    ConfirmControl, ConfirmOptions, ConfirmOutcome, PathItem, PathList, open_destructive_confirm,
};
use crate::workspace::startup::{LoginShellWaiter, login_shell_waiter, register_dialog};

/// Receives the plan (after a confirmed copy) or the error text.
pub type PlanDone = Box<dyn FnOnce(Result<ResumePlan, String>, &mut Window, &mut App)>;

/// Receives whether a terminal was started.
pub type LaunchDone = Box<dyn FnOnce(bool, &mut Window, &mut App)>;

/// The error text `then` receives when the user cancelled the copy dialog
/// (nothing was shown as an error).
pub const CANCELLED: &str = "Cancelled";

/// The error text `then` receives for a target that cannot be resumed
/// (unknown source, list still loading).
pub const UNAVAILABLE: &str = "Resume is not available for this conversation";

/// The QA / probe id of the copy confirmation.
pub const COPY_DIALOG_ID: &str = "copy-to-workspace";

/// How long a launcher gets to fail early (an early non-zero exit is an error).
pub const SPAWN_WAIT: Duration = Duration::from_secs(3);

/// `$TERMINAL` (Linux), read once by `main` and stored here
/// (`LaunchOptions::terminal_var`, DESIGN §11.10 step 6).
#[derive(Clone, Debug, Default)]
pub struct TerminalVar(pub Option<OsString>);

impl Global for TerminalVar {}

/// Store `$TERMINAL` for the launcher detection (`main` reads the process
/// environment once, §16.2, and calls this after `app::init`). Until it is
/// called the launcher detection runs without `$TERMINAL`.
pub fn set_terminal_var(cx: &mut App, value: Option<OsString>) {
    cx.set_global(TerminalVar(value));
}

/// Targets with a resume/fork between the key press and the launch outcome:
/// a double press or a key auto-repeat must not start several `claude
/// --resume` on one transcript.
#[derive(Default)]
struct ResumePending(Vec<PathBuf>);

impl Global for ResumePending {}

fn begin_pending(cx: &mut App, path: &std::path::Path) -> bool {
    let pending = cx.default_global::<ResumePending>();
    if pending.0.iter().any(|p| crate::globals::same_path(p, path)) {
        return false;
    }
    pending.0.push(path.to_path_buf());
    true
}

fn end_pending(cx: &mut App, path: &std::path::Path) {
    if let Some(pending) = cx.try_global::<ResumePending>()
        && pending.0.iter().any(|p| crate::globals::same_path(p, path))
    {
        cx.global_mut::<ResumePending>()
            .0
            .retain(|p| !crate::globals::same_path(p, path));
    }
}

/// Resume (`fork = false`) or fork the captured `target` in a new terminal.
/// A second request for a target whose first one has not finished is ignored.
pub fn resume(target: ConversationTarget, fork: bool, window: &mut Window, cx: &mut App) {
    // Not actionable (nothing to report) or already running.
    if !can_act(&target) {
        return;
    }
    // Picker mode (`--show-path`, `--show-id`, `--resume`): upstream's picker
    // resumes and returns, so the process ends with no stdout after a
    // successful launch (A-052), the same as Ctrl+O under `--resume`.
    if app_state(cx).launch.picker.is_some() {
        crate::ops::select::picker_resume(target, fork, window, cx);
        return;
    }
    if !begin_pending(cx, &target.path) {
        return;
    }
    let path = target.path.clone();
    let captured = target.clone();
    plan_and_confirm(
        target,
        fork,
        window,
        cx,
        Box::new(move |result, window, cx| {
            // Errors were toasted by `plan_and_confirm`.
            match result {
                Ok(plan) => {
                    // This runs outside the list's update: the toast names the
                    // conversation as it is now (A-009).
                    let title = live_target(captured, cx).title;
                    launch_with(
                        &plan,
                        title,
                        Some(Box::new(move |_, _, cx| end_pending(cx, &path))),
                        window,
                        cx,
                    );
                }
                Err(_) => end_pending(cx, &path),
            }
        }),
    );
}

/// Everything planning needs, captured on the UI thread.
struct PlanInput {
    env: HistoryEnv,
    workspace: Option<WorkspaceScope>,
    default_args: Vec<String>,
    programs: ProgramOverrides,
    resume_target: ResumeTarget,
    /// The target is not in the corpus and has no project path: read it.
    read_file: bool,
    fork: bool,
}

fn plan_input(target: &ConversationTarget, fork: bool, cx: &App) -> Option<PlanInput> {
    let source = target.source?;
    let state = app_state(cx);
    // Everything comes from the captured target (PD15, A2): its project path
    // is the corpus entry's at capture time (this runs inside the pane's own
    // update, which must not be read back); a conversation that is not in the
    // corpus has none and its file is read for the working directory.
    let project_path = target.project_path.clone();
    Some(PlanInput {
        env: state.env_with_workspace(),
        workspace: state.workspace.clone(),
        default_args: state.settings.resume_default_args.clone(),
        programs: ProgramOverrides::from_gui_settings(&state.config.gui),
        read_file: project_path.is_none(),
        resume_target: ResumeTarget {
            source,
            path: target.path.clone(),
            project_path,
        },
        fork,
    })
}

fn run_plan(mut input: PlanInput) -> Result<ResumePlan, AppError> {
    if input.read_file
        && let Ok(Some(conversation)) =
            process_conversation_file(input.resume_target.path.clone(), None, None)
    {
        input.resume_target.project_path = conversation.cwd.clone();
    }
    plan_resume_target(
        &input.env,
        &input.resume_target,
        input.workspace.as_ref(),
        input.fork,
        &input.default_args,
        &input.programs,
    )
}

/// Plan the resume/fork of `target` (workspace, overrides, default args) and,
/// for a cross-project copy, confirm it (§5.8); `then` receives the plan after
/// the copy ran, or the error text (already toasted; [`CANCELLED`] when the
/// user declined the copy).
pub fn plan_and_confirm(
    target: ConversationTarget,
    fork: bool,
    window: &mut Window,
    cx: &mut App,
    then: PlanDone,
) {
    let input = if can_act(&target) {
        plan_input(&target, fork, cx)
    } else {
        None
    };
    let Some(input) = input else {
        then(Err(UNAVAILABLE.to_string()), window, cx);
        return;
    };
    let task = cx
        .background_executor()
        .spawn(async move { run_plan(input) });
    window
        .spawn(&*cx, async move |cx| {
            let result = task.await;
            cx.update(|window, cx| planned(target, result, then, window, cx))
                .ok();
        })
        .detach();
}

fn planned(
    target: ConversationTarget,
    result: Result<ResumePlan, AppError>,
    then: PlanDone,
    window: &mut Window,
    cx: &mut App,
) {
    let plan = match result {
        Ok(plan) => plan,
        Err(AppError::WorkspaceRequired(message)) => {
            Toast::error(message.clone())
                .action("Choose workspace…", |window, cx| {
                    window.dispatch_action(Box::new(ChooseWorkspace), cx)
                })
                .show(window, cx);
            then(Err(message), window, cx);
            return;
        }
        Err(error) => {
            let message = error.to_string();
            Toast::error(message.clone()).show(window, cx);
            then(Err(message), window, cx);
            return;
        }
    };
    let overwrites = plan
        .copy
        .as_ref()
        .is_some_and(|copy| !copy.existing_targets.is_empty());
    for warning in &plan.warnings {
        // The overwrite warning is the confirmation's own content.
        if overwrites && warning.starts_with("Overwrites the existing copy") {
            continue;
        }
        Toast::warning(warning.clone()).show(window, cx);
    }
    if plan.copy.is_some() {
        open_copy_confirmation(target, plan, then, window, cx);
    } else {
        then(Ok(plan), window, cx);
    }
}

// ----- the copy confirmation (§5.8) ------------------------------------------------------

/// The confirmation title: names the conversation and the workspace.
pub fn copy_title(target: &ConversationTarget, plan: &ResumePlan) -> String {
    let workspace = plan
        .cwd
        .as_ref()
        .map(|dir| dir.display().to_string())
        .unwrap_or_default();
    format!("Copy \"{}\" to {workspace}?", target.title)
}

/// The rows of the copy list: from/to pairs for the transcript and the folder.
pub fn copy_items(copy: &CopyPlan) -> Vec<PathItem> {
    let mut items = Vec::new();
    for file in &copy.files {
        items.push(PathItem::file(&file.from).with_prefix("from"));
        items.push(PathItem::file(&file.to).with_prefix("to"));
    }
    for dir in &copy.dirs {
        items.push(PathItem::folder(&dir.from).with_prefix("from"));
        items.push(PathItem::folder(&dir.to).with_prefix("to"));
    }
    items
}

fn open_copy_confirmation(
    target: ConversationTarget,
    plan: ResumePlan,
    then: PlanDone,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(copy) = plan.copy.clone() else {
        then(Ok(plan), window, cx);
        return;
    };
    let handle = window.window_handle();
    let then_slot: Rc<RefCell<Option<PlanDone>>> = Rc::new(RefCell::new(Some(then)));
    let control_slot: Rc<RefCell<Option<ConfirmControl>>> = Rc::new(RefCell::new(None));
    let registration = {
        let control_slot = control_slot.clone();
        OpenTargets::register(
            cx,
            handle,
            target.path.clone(),
            target.title.clone(),
            Box::new(move |_, cx| {
                let control = control_slot.borrow().clone();
                if let Some(control) = control {
                    control.close(cx);
                }
            }),
        )
    };
    register_dialog(cx, COPY_DIALOG_ID);

    let overwrites = !copy.existing_targets.is_empty();
    let verb = if plan.kind.is_fork() {
        "Copy and fork"
    } else {
        "Copy and resume"
    };
    let cwd_text = plan
        .cwd
        .as_ref()
        .map(|dir| dir.display().to_string())
        .unwrap_or_default();
    let command_line = format!("Then runs: {}   in {cwd_text}", plan.display_command());
    let items = copy_items(&copy);
    let existing: Vec<PathItem> = copy.existing_targets.iter().map(PathItem::file).collect();
    let base = if overwrites {
        ConfirmOptions::destructive(copy_title(&target, &plan), verb)
    } else {
        ConfirmOptions::non_destructive(copy_title(&target, &plan), verb)
    };
    let options = base
        .width_rem(42.0)
        .body(move |_, cx| {
            let theme = cx.theme();
            let (danger, muted) = (theme.danger, theme.muted_foreground);
            v_flex()
                .gap_2()
                .w_full()
                .child(div().text_sm().child(
                    "Claude Code only finds sessions in the project folder of the folder it \
                     runs in, so the conversation is copied:",
                ))
                .child(PathList::new("copy-paths").items(items.clone()))
                .when(!existing.is_empty(), |this| {
                    this.child(
                        v_flex()
                            .gap_1()
                            .child(
                                h_flex()
                                    .gap_1()
                                    .text_sm()
                                    .text_color(danger)
                                    .child(Icon::new(IconName::TriangleAlert).small())
                                    .child("Already exists and will be overwritten:"),
                            )
                            .child(PathList::new("copy-existing").items(existing.iter().cloned())),
                    )
                })
                .child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child(command_line.clone()),
                )
                .into_any_element()
        })
        .on_closed({
            let then_slot = then_slot.clone();
            move |cx| {
                OpenTargets::unregister(cx, registration);
                clear_dialog(cx, COPY_DIALOG_ID);
                // Closed without confirming: the caller learns it was cancelled.
                if let Some(then) = then_slot.borrow_mut().take() {
                    cx.defer(move |cx| {
                        handle
                            .update(cx, |_, window, cx| {
                                then(Err(CANCELLED.to_string()), window, cx)
                            })
                            .ok();
                    });
                }
            }
        });

    let control = open_destructive_confirm(options, window, cx, move |control, window, cx| {
        let Some(then) = then_slot.borrow_mut().take() else {
            return ConfirmOutcome::KeepOpen;
        };
        run_copy(control, plan.clone(), then, window, cx);
        ConfirmOutcome::Busy
    });
    *control_slot.borrow_mut() = Some(control);
}

/// Run the confirmed copy in the background, then continue with `then`.
fn run_copy(
    control: &ConfirmControl,
    plan: ResumePlan,
    then: PlanDone,
    window: &mut Window,
    cx: &mut App,
) {
    let task = {
        let plan = plan.clone();
        cx.background_executor()
            .spawn(async move { plan.execute_copy() })
    };
    let control = control.clone();
    window
        .spawn(&*cx, async move |cx| {
            let result = task.await;
            cx.update(|window, cx| {
                control.close(cx);
                match result {
                    Ok(_report) => then(Ok(plan), window, cx),
                    Err(error) => {
                        let message = format!("Failed to copy the conversation: {error}");
                        Toast::error(message.clone()).show(window, cx);
                        then(Err(message), window, cx);
                    }
                }
            })
            .ok();
        })
        .detach();
}

// ----- launching the terminal (§11.10 steps 4–6) ---------------------------------------------

/// What the background launch did.
#[derive(Debug)]
pub(crate) enum LaunchOutcome {
    /// The program could not be found (or the spec could not be built).
    Spec(AppError),
    /// No terminal launcher is available.
    NoLauncher,
    /// A launcher started (`launcher` names it).
    Started { launcher: String },
    /// A launcher ran but exited early with a failure code.
    Exited { launcher: String, code: i32 },
    /// Every candidate failed; the last error text.
    Failed(String),
}

struct LaunchInput {
    env: HistoryEnv,
    plan: ResumePlan,
    options: LaunchOptions,
    waiter: Option<LoginShellWaiter>,
    services: Services,
}

fn launch_input(plan: &ResumePlan, cx: &App) -> LaunchInput {
    let state = app_state(cx);
    let options = LaunchOptions {
        terminal: state.config.gui.terminal.clone(),
        terminal_var: cx.try_global::<TerminalVar>().and_then(|var| var.0.clone()),
        ..LaunchOptions::default()
    };
    LaunchInput {
        env: state.env_with_workspace(),
        plan: plan.clone(),
        options,
        waiter: login_shell_waiter(cx),
        services: services(cx),
    }
}

fn run_launch(input: LaunchInput) -> LaunchOutcome {
    let LaunchInput {
        mut env,
        plan,
        options,
        waiter,
        services,
    } = input;
    // macOS/Linux: the login-shell PATH may still be loading (§14.2).
    if let Some(waiter) = waiter
        && !waiter.is_finished()
        && let Some(path) = waiter.wait(Duration::from_secs(3))
    {
        env.path_var = Some(path);
    }
    let spec = match plan.launch_spec(&env) {
        Ok(spec) => spec,
        Err(error) => return LaunchOutcome::Spec(error),
    };
    spawn_through_candidates(&env, spec, &options, &services)
}

/// Build the command for each launcher candidate (K17) and spawn the first
/// that starts: a spawn error tries the next candidate, an early non-zero exit
/// is final (it ran).
pub(crate) fn spawn_through_candidates(
    env: &HistoryEnv,
    spec: LaunchSpec,
    options: &LaunchOptions,
    services: &Services,
) -> LaunchOutcome {
    let candidates = launcher_candidates(env, options);
    if candidates.is_empty() {
        return LaunchOutcome::NoLauncher;
    }
    let mut last_error = String::new();
    for launcher in candidates {
        let command: TerminalCommand =
            match prepare_launcher_command(env, &launcher, &spec, options) {
                Ok(command) => command,
                Err(error) => {
                    last_error = error.to_string();
                    continue;
                }
            };
        match services.spawn_terminal(&command, SPAWN_WAIT) {
            Ok(None) | Ok(Some(0)) => {
                return LaunchOutcome::Started {
                    launcher: command.launcher.clone(),
                };
            }
            Ok(Some(code)) => {
                return LaunchOutcome::Exited {
                    launcher: command.launcher.clone(),
                    code,
                };
            }
            Err(error) => last_error = format!("Failed to launch terminal: {error}"),
        }
    }
    LaunchOutcome::Failed(last_error)
}

/// Launch `plan` through the launcher candidates (K17) and toast the result.
pub fn launch_in_new_terminal(
    plan: &ResumePlan,
    title: SharedString,
    window: &mut Window,
    cx: &mut App,
) {
    launch_with(plan, title, None, window, cx);
}

/// [`launch_in_new_terminal`] that also tells the caller whether a terminal
/// was started (the picker quits only then).
pub fn launch_with(
    plan: &ResumePlan,
    title: SharedString,
    done: Option<LaunchDone>,
    window: &mut Window,
    cx: &mut App,
) {
    let input = launch_input(plan, cx);
    let command_line = plan.display_command();
    let task = cx
        .background_executor()
        .spawn(async move { run_launch(input) });
    window
        .spawn(&*cx, async move |cx| {
            let outcome = task.await;
            cx.update(|window, cx| {
                let started = report_launch(outcome, &title, &command_line, window, cx);
                if let Some(done) = done {
                    done(started, window, cx);
                }
            })
            .ok();
        })
        .detach();
}

/// Toast the outcome; `true` when a terminal was started.
fn report_launch(
    outcome: LaunchOutcome,
    title: &str,
    command_line: &str,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    match outcome {
        LaunchOutcome::Started { launcher } => {
            Toast::info(format!("Opened \"{title}\" in {launcher}"))
                .detail(command_line.to_string())
                .show(window, cx);
            true
        }
        LaunchOutcome::Spec(error) => {
            match &error {
                AppError::ClaudeExecutionError(message)
                    if message.starts_with("Could not find '") =>
                {
                    Toast::error(format!("{message}. Set its path in Settings → General."))
                        .action("Open Settings", |window, cx| {
                            crate::settings::open_settings(Some(SettingsPage::General), window, cx)
                        })
                        .show(window, cx);
                }
                _ => Toast::error(error.to_string()).show(window, cx),
            }
            false
        }
        LaunchOutcome::NoLauncher => {
            terminal_error(
                "Failed to launch terminal: no terminal emulator found; set [gui].terminal in gui.toml"
                    .to_string(),
                window,
                cx,
            );
            false
        }
        LaunchOutcome::Exited { launcher, code } => {
            terminal_error(
                format!("Failed to launch terminal: {launcher} exited with code {code}"),
                window,
                cx,
            );
            false
        }
        LaunchOutcome::Failed(message) => {
            terminal_error(message, window, cx);
            false
        }
    }
}

fn terminal_error(message: String, window: &mut Window, cx: &mut App) {
    Toast::error(message)
        .action("Terminal settings", |window, cx| {
            crate::settings::open_settings(Some(SettingsPage::General), window, cx)
        })
        .show(window, cx);
}

/// Whether the picker resumes by replacing the launching terminal's process
/// (macOS/Linux with a TTY on stdin and stdout) instead of opening a window.
pub fn picker_uses_exec(unix: bool, stdin_is_tty: bool, stdout: crate::model::StdoutKind) -> bool {
    unix && stdin_is_tty && stdout == crate::model::StdoutKind::Terminal
}

/// The launch spec of `plan` for `exec_in_current_terminal` (resolved with the
/// current environment). Used by the picker's Unix path.
pub fn exec_spec(plan: &ResumePlan, cx: &App) -> Result<LaunchSpec, AppError> {
    plan.launch_spec(&app_state(cx).env_with_workspace())
}
