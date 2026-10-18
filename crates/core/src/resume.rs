//! Resume / fork planning and cross-project session copy (upstream
//! `main.rs:1788-2020`).
//!
//! Upstream resolved what to run from the process cwd and then `exec`ed
//! `claude`/`pi`/`omp` in the terminal it was running in. A GUI has neither a
//! meaningful cwd nor a terminal, so core splits that into data:
//!
//! 1. [`plan_resume`] decides the program, its arguments, the working
//!    directory and an optional [`CopyPlan`] (a cross-project copy of a
//!    Claude session), from the conversation and the explicitly chosen
//!    workspace ([`WorkspaceScope`], which replaces upstream's cwd).
//! 2. [`execute_copy`] performs that copy (after the GUI confirmed it).
//! 3. [`terminal`] turns the plan into a command that opens a terminal window
//!    running it, and [`launch`] spawns that.
//!
//! # Claude semantics (GUI)
//!
//! `claude --resume <id>` only finds sessions stored in the projects
//! directory of its own working directory (`<projects root>/<encoded cwd>`).
//!
//! * Resume runs in the conversation's own project directory when it exists
//!   and maps to the directory that holds the session. Otherwise it uses the
//!   workspace: in place when the workspace maps to that directory, else by
//!   copying the session into the workspace's projects directory first (the
//!   TUI's `CopyToCurrent`). Without a workspace this is an error
//!   ([`WORKSPACE_REQUIRED_RESUME`]).
//! * Fork with a workspace whose projects directory differs from the
//!   session's copies the session there and resumes the copy (no
//!   `--fork-session`: the copy is the fork). Otherwise it runs
//!   `claude --resume <id> --fork-session` in the session's project
//!   directory (the conversation's project directory, or the workspace when
//!   it maps to the same projects directory).
//! * `[resume].default_args` are appended for Claude only.
//!
//! Upstream copied a session onto itself when the project directory was gone
//! but the cwd already mapped to the session's directory (which truncates
//! the transcript on some systems); the GUI semantics above never plan that,
//! and [`execute_copy`] additionally skips any copy whose source and target
//! are the same file. Upstream's exact decision function is kept as
//! [`resolve_claude_resume_action`].
//!
//! # Pi / OMP
//!
//! `pi --session <path>` / `omp --resume <path>` run in the session's
//! recorded working directory; `pi --fork <path>` / `omp --fork <path>`
//! inherited upstream's cwd, which is the workspace here. Missing
//! directories fall back (workspace, then project directory, then none) with
//! a warning instead of failing to spawn.

pub mod terminal;

use crate::config::GuiSettings;
use crate::env::{HistoryEnv, WorkspaceScope};
use crate::error::{AppError, Result};
use crate::history::{Conversation, Source};
use crate::program::ResolvedProgram;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub use terminal::{
    LaunchOptions, LaunchSpec, Platform, TerminalArgs, TerminalCommand, TerminalLauncher,
};

/// Error text when a Claude session can only be resumed through a workspace
/// and none is selected.
pub const WORKSPACE_REQUIRED_RESUME: &str = "Choose a workspace folder to resume this session";
/// Error text when a Claude session can only be forked through a workspace
/// and none is selected.
pub const WORKSPACE_REQUIRED_FORK: &str = "Choose a workspace folder to fork this session";

/// Upstream's decision for Claude sessions (`main.rs:1795-1799`), kept for
/// parity and tests. The GUI uses [`plan_resume`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaudeResumeAction {
    Run { current_dir: PathBuf },
    CopyToCurrent { cwd_projects_dir: PathBuf },
}

/// Upstream `resolve_claude_resume_action` (`main.rs:1801-1834`), verbatim
/// except that `cwd` is explicit and paths come from `env`.
///
/// * the project path is missing or not a directory, or (`fork_session` and
///   the cwd's projects dir differs from the session's): copy into the cwd's
///   projects dir;
/// * else the cwd maps to the session's projects dir: run in the cwd;
/// * else the project path maps to it: run in the project path;
/// * else: copy into the cwd's projects dir.
pub fn resolve_claude_resume_action(
    env: &HistoryEnv,
    selected_path: &Path,
    project_path: Option<&PathBuf>,
    cwd: &Path,
    fork_session: bool,
) -> Result<ClaudeResumeAction> {
    let conv_projects_dir = selected_path.parent().ok_or_else(no_project_dir_error)?;
    let cwd_projects_dir = env.claude_projects_dir_for(cwd)?;
    let project_dir = project_path.filter(|p| p.exists() && p.is_dir());

    if project_dir.is_none() || (fork_session && cwd_projects_dir != conv_projects_dir) {
        return Ok(ClaudeResumeAction::CopyToCurrent { cwd_projects_dir });
    }

    if cwd_projects_dir == conv_projects_dir {
        return Ok(ClaudeResumeAction::Run {
            current_dir: cwd.to_path_buf(),
        });
    }

    let project_dir = project_dir.unwrap();
    let project_projects_dir = env.claude_projects_dir_for(project_dir)?;
    if project_projects_dir == conv_projects_dir {
        Ok(ClaudeResumeAction::Run {
            current_dir: project_dir.clone(),
        })
    } else {
        Ok(ClaudeResumeAction::CopyToCurrent { cwd_projects_dir })
    }
}

fn no_project_dir_error() -> AppError {
    AppError::ClaudeExecutionError("Cannot determine conversation's project directory".to_string())
}

/// Resume an existing session or fork it into a new one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResumeKind {
    Resume,
    Fork,
}

impl ResumeKind {
    pub fn from_fork(fork: bool) -> Self {
        if fork { Self::Fork } else { Self::Resume }
    }

    pub fn is_fork(self) -> bool {
        self == Self::Fork
    }
}

/// How a [`ResumePlan`] runs the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResumeStrategy {
    /// Claude: run in the conversation's own project directory.
    InProject,
    /// Claude: run in the workspace, whose projects directory already holds
    /// the session.
    InWorkspace,
    /// Claude: copy the session into the workspace's projects directory,
    /// then resume the copy there (never with `--fork-session`).
    CopyToWorkspace,
    /// Pi / OMP: the agent's native resume or fork command.
    Native,
}

/// Explicit executable paths (`gui.toml`: `claude_path`, `pi_path`,
/// `omp_path`). `None` resolves the program through `PATH`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProgramOverrides {
    pub claude: Option<PathBuf>,
    pub pi: Option<PathBuf>,
    pub omp: Option<PathBuf>,
}

impl ProgramOverrides {
    pub fn from_gui_settings(settings: &GuiSettings) -> Self {
        Self {
            claude: settings.claude_path.clone(),
            pi: settings.pi_path.clone(),
            omp: settings.omp_path.clone(),
        }
    }

    pub fn for_source(&self, source: Source) -> Option<&Path> {
        match source {
            Source::Claude => self.claude.as_deref(),
            Source::Pi => self.pi.as_deref(),
            Source::Omp => self.omp.as_deref(),
        }
    }
}

/// The program name each source resumes with.
pub fn program_name(source: Source) -> &'static str {
    match source {
        Source::Claude => "claude",
        Source::Pi => "pi",
        Source::Omp => "omp",
    }
}

/// Resolve `name` (`claude`, `pi`, `omp`, ...) to a full path: the override
/// when given (it must exist), else a `PATH` search with `PATHEXT` on
/// Windows (W9). [`ResolvedProgram::kind`] tells whether it is a batch
/// script that must run through `cmd.exe`; [`ResolvedProgram::command`]
/// spawns either kind with its arguments intact.
pub fn resolve_program(
    env: &HistoryEnv,
    name: &str,
    override_path: Option<&Path>,
) -> Result<ResolvedProgram> {
    env.resolve_program(name, override_path)
}

/// What a session to resume is: its source, transcript path and recorded
/// project directory (normally built from a [`Conversation`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeTarget {
    pub source: Source,
    pub path: PathBuf,
    pub project_path: Option<PathBuf>,
}

impl From<&Conversation> for ResumeTarget {
    fn from(conversation: &Conversation) -> Self {
        Self {
            source: conversation.source,
            path: conversation.path.clone(),
            project_path: conversation.project_path.clone(),
        }
    }
}

/// One file or directory to copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyItem {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// Cross-project copy of a Claude session (upstream `copy_session_files`):
/// the `<id>.jsonl` transcript and the `<id>/` session directory
/// (`tool-results/`, `subagents/`). `~/.claude/file-history/<id>` is global
/// and keyed by session id, so it is not copied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyPlan {
    pub session_id: String,
    /// Projects directory that holds the session now.
    pub source_projects_dir: PathBuf,
    /// Projects directory the session is copied into (created if missing).
    pub target_dir: PathBuf,
    /// Files to copy (the transcript).
    pub files: Vec<CopyItem>,
    /// Directories to copy recursively (the session directory, when it
    /// existed at planning time).
    pub dirs: Vec<CopyItem>,
    /// Targets that already exist and will be overwritten (a previous copy
    /// of the same session, possibly continued since). The GUI should warn.
    pub existing_targets: Vec<PathBuf>,
}

/// What [`execute_copy`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CopyReport {
    pub files_copied: usize,
    pub dirs_copied: usize,
    /// Items skipped because source and target are the same file.
    pub skipped_same_path: Vec<PathBuf>,
}

/// A resume or fork, ready to launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumePlan {
    pub source: Source,
    pub kind: ResumeKind,
    pub strategy: ResumeStrategy,
    /// The session transcript.
    pub session_path: PathBuf,
    /// Program name: `claude`, `pi` or `omp`.
    pub program: String,
    /// Explicit executable path (from [`ProgramOverrides`]).
    pub program_override: Option<PathBuf>,
    /// Arguments after the program.
    pub args: Vec<OsString>,
    /// Working directory to run in (`None`: the terminal's default).
    pub cwd: Option<PathBuf>,
    /// Copy to perform before launching.
    pub copy: Option<CopyPlan>,
    /// Non-fatal notes for the user (fallback directories, overwrites).
    pub warnings: Vec<String>,
}

impl ResumePlan {
    /// Resolve the program (override or `PATH`) and build what a terminal
    /// has to run.
    pub fn launch_spec(&self, env: &HistoryEnv) -> Result<LaunchSpec> {
        let program = env.resolve_program(&self.program, self.program_override.as_deref())?;
        Ok(LaunchSpec {
            program,
            args: self.args.clone(),
            cwd: self.cwd.clone(),
        })
    }

    /// Perform the planned copy, if any.
    pub fn execute_copy(&self) -> Result<Option<CopyReport>> {
        self.copy.as_ref().map(execute_copy).transpose()
    }

    /// The command for display (`claude --resume <id> --fork-session`),
    /// POSIX-quoted. Not meant to be executed.
    pub fn display_command(&self) -> String {
        std::iter::once(self.program.clone())
            .chain(
                self.args
                    .iter()
                    .map(|arg| terminal::quote_posix(&arg.to_string_lossy())),
            )
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Plan resuming (`fork = false`) or forking `conversation`.
///
/// `workspace` replaces upstream's process cwd; `default_args` are
/// `[resume].default_args` (Claude only); `programs` holds explicit
/// executable paths. See the module docs for the decision rules.
pub fn plan_resume(
    env: &HistoryEnv,
    conversation: &Conversation,
    workspace: Option<&WorkspaceScope>,
    fork: bool,
    default_args: &[String],
    programs: &ProgramOverrides,
) -> Result<ResumePlan> {
    plan_resume_target(
        env,
        &ResumeTarget::from(conversation),
        workspace,
        fork,
        default_args,
        programs,
    )
}

/// [`plan_resume`] for a session given by path (for example a JSONL file
/// that is not part of the loaded list).
pub fn plan_resume_target(
    env: &HistoryEnv,
    target: &ResumeTarget,
    workspace: Option<&WorkspaceScope>,
    fork: bool,
    default_args: &[String],
    programs: &ProgramOverrides,
) -> Result<ResumePlan> {
    let kind = ResumeKind::from_fork(fork);
    let program_override = programs.for_source(target.source).map(Path::to_path_buf);
    match target.source {
        Source::Claude => plan_claude(env, target, workspace, kind, default_args, program_override),
        Source::Pi | Source::Omp => Ok(plan_native(target, workspace, kind, program_override)),
    }
}

fn plan_claude(
    env: &HistoryEnv,
    target: &ResumeTarget,
    workspace: Option<&WorkspaceScope>,
    kind: ResumeKind,
    default_args: &[String],
    program_override: Option<PathBuf>,
) -> Result<ResumePlan> {
    let conversation_id = target
        .path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| {
            AppError::ClaudeExecutionError("Conversation filename is not valid Unicode".to_string())
        })?
        .to_owned();
    // A stray `..jsonl` (stem `.`) or `abc..jsonl` (stem `abc.`, a Windows
    // alias of `abc`) is not a session: `--resume .` is meaningless and the
    // copy below would treat the whole project directory as the session's.
    if !is_session_transcript(&target.path, &conversation_id) {
        return Err(not_a_session_transcript(&target.path));
    }
    let conv_projects_dir = target.path.parent().ok_or_else(no_project_dir_error)?;

    let project_dir = target
        .project_path
        .as_deref()
        .filter(|path| path.is_dir())
        .map(Path::to_path_buf);
    let project_maps = match &project_dir {
        Some(dir) => same_dir(&env.claude_projects_dir_for(dir)?, conv_projects_dir),
        None => false,
    };
    let workspace_projects_dir = workspace
        .map(|workspace| {
            env.claude_projects_root()
                .map(|root| root.join(workspace.project_dir_name()))
        })
        .transpose()?;
    let workspace_maps = workspace_projects_dir
        .as_deref()
        .is_some_and(|dir| same_dir(dir, conv_projects_dir));

    let usable_project = project_dir.filter(|_| project_maps);
    let strategy = match kind {
        ResumeKind::Fork => match workspace {
            Some(_) if !workspace_maps => ResumeStrategy::CopyToWorkspace,
            _ if usable_project.is_some() => ResumeStrategy::InProject,
            Some(_) => ResumeStrategy::InWorkspace,
            None => {
                return Err(AppError::WorkspaceRequired(
                    WORKSPACE_REQUIRED_FORK.to_string(),
                ));
            }
        },
        ResumeKind::Resume => match workspace {
            _ if usable_project.is_some() => ResumeStrategy::InProject,
            Some(_) if workspace_maps => ResumeStrategy::InWorkspace,
            Some(_) => ResumeStrategy::CopyToWorkspace,
            None => {
                return Err(AppError::WorkspaceRequired(
                    WORKSPACE_REQUIRED_RESUME.to_string(),
                ));
            }
        },
    };

    let cwd = match strategy {
        ResumeStrategy::InProject => usable_project.expect("usable project dir"),
        _ => {
            let workspace = workspace.expect("strategy implies a workspace");
            if !workspace.dir().is_dir() {
                return Err(AppError::WorkspaceRequired(format!(
                    "Workspace folder does not exist: {}",
                    workspace.dir().display()
                )));
            }
            workspace.dir().to_path_buf()
        }
    };

    let mut warnings = Vec::new();
    let copy = if strategy == ResumeStrategy::CopyToWorkspace {
        let target_dir = workspace_projects_dir.expect("strategy implies a workspace");
        let plan = plan_session_copy(
            &target.path,
            &conversation_id,
            conv_projects_dir,
            &target_dir,
        )?;
        for existing in &plan.existing_targets {
            warnings.push(format!(
                "Overwrites the existing copy at {}",
                existing.display()
            ));
        }
        Some(plan)
    } else {
        None
    };

    let mut args = vec![OsString::from("--resume"), OsString::from(&conversation_id)];
    // The copy is the fork: never pass --fork-session after copying.
    if kind == ResumeKind::Fork && strategy != ResumeStrategy::CopyToWorkspace {
        args.push(OsString::from("--fork-session"));
    }
    args.extend(default_args.iter().map(OsString::from));

    Ok(ResumePlan {
        source: Source::Claude,
        kind,
        strategy,
        session_path: target.path.clone(),
        program: program_name(Source::Claude).to_string(),
        program_override,
        args,
        cwd: Some(cwd),
        copy,
        warnings,
    })
}

/// Upstream `build_pi_resume_command` / `build_omp_resume_command`
/// (`main.rs:1860-1886`) as data. Resume runs in the session's project
/// directory; fork inherited upstream's cwd, which is the workspace here.
fn plan_native(
    target: &ResumeTarget,
    workspace: Option<&WorkspaceScope>,
    kind: ResumeKind,
    program_override: Option<PathBuf>,
) -> ResumePlan {
    let flag = match (target.source, kind) {
        (_, ResumeKind::Fork) => "--fork",
        (Source::Omp, ResumeKind::Resume) => "--resume",
        (_, ResumeKind::Resume) => "--session",
    };
    let existing_project = target.project_path.as_deref().filter(|path| path.is_dir());
    let existing_workspace = workspace
        .map(WorkspaceScope::dir)
        .filter(|dir| dir.is_dir());
    let mut warnings = Vec::new();
    let cwd = match kind {
        ResumeKind::Resume => {
            if let Some(project) = existing_project {
                Some(project.to_path_buf())
            } else {
                if let Some(project) = &target.project_path {
                    warnings.push(format!(
                        "Session directory {} does not exist",
                        project.display()
                    ));
                }
                existing_workspace.map(Path::to_path_buf)
            }
        }
        ResumeKind::Fork => existing_workspace
            .or(existing_project)
            .map(Path::to_path_buf),
    };
    ResumePlan {
        source: target.source,
        kind,
        strategy: ResumeStrategy::Native,
        session_path: target.path.clone(),
        program: program_name(target.source).to_string(),
        program_override,
        args: vec![OsString::from(flag), target.path.clone().into_os_string()],
        cwd,
        copy: None,
        warnings,
    }
}

/// Whether `path` is the transcript `<session id>.jsonl` of `session_id`
/// under the session-id rule of the deletes
/// ([`crate::history::session_artifact_dir`]).
fn is_session_transcript(path: &Path, session_id: &str) -> bool {
    crate::history::session_artifact_dir(path, session_id).is_some()
}

fn not_a_session_transcript(path: &Path) -> AppError {
    AppError::ClaudeExecutionError(format!(
        "Not a Claude session transcript: {}",
        path.display()
    ))
}

/// Plan the cross-project copy of `<id>.jsonl` (+ `<id>/`) from
/// `source_projects_dir` into `target_projects_dir`.
///
/// `session_id` must be the file stem of `jsonl_path` and a plain session
/// id (ASCII letters, digits and `-`); anything else is refused, so a
/// stray `..jsonl` can never plan the project directory itself as the
/// session directory (upstream copied the whole project).
pub fn plan_session_copy(
    jsonl_path: &Path,
    session_id: &str,
    source_projects_dir: &Path,
    target_projects_dir: &Path,
) -> Result<CopyPlan> {
    if !is_session_transcript(jsonl_path, session_id) {
        return Err(not_a_session_transcript(jsonl_path));
    }
    let file_name = jsonl_path.file_name().ok_or_else(|| {
        AppError::ClaudeExecutionError(format!("Not a transcript file: {}", jsonl_path.display()))
    })?;
    let files = vec![CopyItem {
        from: jsonl_path.to_path_buf(),
        to: target_projects_dir.join(file_name),
    }];
    let session_dir = source_projects_dir.join(session_id);
    let dirs = if session_dir.is_dir() {
        vec![CopyItem {
            from: session_dir,
            to: target_projects_dir.join(session_id),
        }]
    } else {
        Vec::new()
    };
    let existing_targets = files
        .iter()
        .chain(dirs.iter())
        .filter(|item| item.to.exists() && !same_file(&item.from, &item.to))
        .map(|item| item.to.clone())
        .collect();
    Ok(CopyPlan {
        session_id: session_id.to_string(),
        source_projects_dir: source_projects_dir.to_path_buf(),
        target_dir: target_projects_dir.to_path_buf(),
        files,
        dirs,
        existing_targets,
    })
}

/// Perform a [`CopyPlan`]: create the target projects directory, copy the
/// transcript (overwriting, as upstream) and the session directory
/// recursively. Copies whose source and target are the same file are
/// skipped (upstream would have truncated the transcript).
pub fn execute_copy(plan: &CopyPlan) -> Result<CopyReport> {
    std::fs::create_dir_all(&plan.target_dir).map_err(AppError::Io)?;
    let mut report = CopyReport::default();
    for item in &plan.files {
        if same_file(&item.from, &item.to) {
            report.skipped_same_path.push(item.to.clone());
            continue;
        }
        std::fs::copy(&item.from, &item.to).map_err(AppError::Io)?;
        report.files_copied += 1;
    }
    for item in &plan.dirs {
        if !item.from.is_dir() {
            continue;
        }
        if same_file(&item.from, &item.to) {
            report.skipped_same_path.push(item.to.clone());
            continue;
        }
        copy_dir_recursive(&item.from, &item.to)?;
        report.dirs_copied += 1;
    }
    Ok(report)
}

/// Upstream `copy_dir_recursive` (`main.rs:1974-1987`): follows symlinks
/// and overwrites files.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst).map_err(AppError::Io)?;
    for entry in std::fs::read_dir(src).map_err(AppError::Io)? {
        let entry = entry.map_err(AppError::Io)?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path).map_err(AppError::Io)?;
        }
    }
    Ok(())
}

/// Whether two directory paths name the same directory: equal as given, or
/// equal once canonicalized (both must exist for the latter).
fn same_dir(left: &Path, right: &Path) -> bool {
    left == right || same_file(left, right)
}

/// Whether two existing paths resolve to the same file system object.
fn same_file(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// Plan, copy (if planned) and open a terminal window running the session.
///
/// This performs the copy without asking; GUIs that want to confirm
/// overwrites check [`CopyPlan::existing_targets`] first.
pub fn launch(
    env: &HistoryEnv,
    plan: &ResumePlan,
    options: &LaunchOptions,
) -> Result<LaunchReport> {
    let spec = plan.launch_spec(env)?;
    let command = terminal::prepare_terminal_command(env, &spec, options)?;
    let copy = plan.execute_copy()?;
    terminal::spawn_terminal(&command)
        .map_err(|error| AppError::TerminalLaunch(error.to_string()))?;
    Ok(LaunchReport { command, copy })
}

/// What [`launch`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchReport {
    pub command: TerminalCommand,
    pub copy: Option<CopyReport>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    const UUID: &str = "12345678-1234-4234-9234-123456789abc";

    struct Sandbox {
        root: tempfile::TempDir,
        env: HistoryEnv,
    }

    impl Sandbox {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let env = HistoryEnv::sandbox(root.path());
            Self { root, env }
        }

        fn dir(&self, name: &str) -> PathBuf {
            let dir = self.root.path().join(name);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        /// A Claude transcript stored under the projects dir of `project`.
        fn session_in(&self, project: &Path) -> PathBuf {
            let dir = self.env.claude_projects_dir_for(project).unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!("{UUID}.jsonl"));
            std::fs::write(&path, transcript()).unwrap();
            path
        }

        fn target(&self, path: &Path, project: Option<&Path>) -> ResumeTarget {
            ResumeTarget {
                source: Source::Claude,
                path: path.to_path_buf(),
                project_path: project.map(Path::to_path_buf),
            }
        }

        fn plan(
            &self,
            target: &ResumeTarget,
            workspace: Option<&Path>,
            fork: bool,
            default_args: &[String],
        ) -> Result<ResumePlan> {
            let workspace = workspace.map(|dir| WorkspaceScope::new(dir).unwrap());
            plan_resume_target(
                &self.env,
                target,
                workspace.as_ref(),
                fork,
                default_args,
                &ProgramOverrides::default(),
            )
        }
    }

    fn transcript() -> String {
        format!(
            "{}
{}
",
            crate::agent::test_support::user_jsonl_line("question"),
            crate::agent::test_support::assistant_jsonl_line("answer")
        )
    }

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    // --- Upstream tests (main.rs:1716-1785), with a sandbox env. ---

    #[test]
    fn resume_action_uses_cwd_when_it_maps_to_selected_project_dir() {
        let sandbox = Sandbox::new();
        let cwd = sandbox.dir("cwd");
        let stale_project_path = sandbox.dir("stale");
        let selected_path = sandbox
            .env
            .claude_projects_dir_for(&cwd)
            .unwrap()
            .join(format!("{UUID}.jsonl"));

        let action = resolve_claude_resume_action(
            &sandbox.env,
            &selected_path,
            Some(&stale_project_path),
            &cwd,
            false,
        )
        .unwrap();

        assert_eq!(action, ClaudeResumeAction::Run { current_dir: cwd });
    }

    #[test]
    fn resume_action_uses_project_path_when_it_maps_to_selected_project_dir() {
        let sandbox = Sandbox::new();
        let cwd = sandbox.dir("cwd");
        let project = sandbox.dir("project");
        let selected_path = sandbox
            .env
            .claude_projects_dir_for(&project)
            .unwrap()
            .join(format!("{UUID}.jsonl"));

        let action =
            resolve_claude_resume_action(&sandbox.env, &selected_path, Some(&project), &cwd, false)
                .unwrap();

        assert_eq!(
            action,
            ClaudeResumeAction::Run {
                current_dir: project
            }
        );
    }

    #[test]
    fn resume_action_copies_selected_transcript_when_project_path_maps_elsewhere() {
        let sandbox = Sandbox::new();
        let cwd = sandbox.dir("cwd");
        let selected_project = sandbox.dir("selected");
        let stale_project_path = sandbox.dir("stale");
        let selected_path = sandbox
            .env
            .claude_projects_dir_for(&selected_project)
            .unwrap()
            .join(format!("{UUID}.jsonl"));
        let cwd_projects_dir = sandbox.env.claude_projects_dir_for(&cwd).unwrap();

        let action = resolve_claude_resume_action(
            &sandbox.env,
            &selected_path,
            Some(&stale_project_path),
            &cwd,
            false,
        )
        .unwrap();

        assert_eq!(
            action,
            ClaudeResumeAction::CopyToCurrent { cwd_projects_dir }
        );
    }

    // --- Upstream Pi/OMP command tests (main.rs:568-605) as plans. ---

    #[test]
    fn pi_resume_and_fork_commands_use_native_paths_without_claude_defaults() {
        let sandbox = Sandbox::new();
        let cwd = sandbox.dir("project");
        let path = sandbox
            .root
            .path()
            .join("pi sessions")
            .join("session.jsonl");
        let target = ResumeTarget {
            source: Source::Pi,
            path: path.clone(),
            project_path: Some(cwd.clone()),
        };
        let defaults = vec!["--model".to_string(), "opus".to_string()];

        let resume = sandbox.plan(&target, None, false, &defaults).unwrap();
        assert_eq!(resume.program, "pi");
        assert_eq!(
            resume.args,
            vec![OsString::from("--session"), path.clone().into_os_string()]
        );
        assert_eq!(resume.cwd, Some(cwd.clone()));
        assert_eq!(resume.strategy, ResumeStrategy::Native);
        assert!(resume.copy.is_none());

        let fork = sandbox.plan(&target, None, true, &defaults).unwrap();
        assert_eq!(
            fork.args,
            vec![OsString::from("--fork"), path.into_os_string()]
        );
        // Upstream inherited its cwd; without a workspace the session's own
        // directory is the closest equivalent.
        assert_eq!(fork.cwd, Some(cwd));
    }

    #[test]
    fn omp_resume_and_fork_commands_use_native_paths() {
        let sandbox = Sandbox::new();
        let cwd = sandbox.dir("project");
        let workspace = sandbox.dir("workspace");
        let path = sandbox
            .root
            .path()
            .join("omp sessions")
            .join("session.jsonl");
        let target = ResumeTarget {
            source: Source::Omp,
            path: path.clone(),
            project_path: Some(cwd.clone()),
        };

        let resume = sandbox.plan(&target, Some(&workspace), false, &[]).unwrap();
        assert_eq!(resume.program, "omp");
        assert_eq!(
            resume.args,
            vec![OsString::from("--resume"), path.clone().into_os_string()]
        );
        assert_eq!(resume.cwd, Some(cwd));

        let fork = sandbox.plan(&target, Some(&workspace), true, &[]).unwrap();
        assert_eq!(
            fork.args,
            vec![OsString::from("--fork"), path.into_os_string()]
        );
        // Fork inherits the (workspace) cwd, like upstream's process cwd.
        assert_eq!(fork.cwd, Some(workspace));
    }

    #[test]
    fn native_resume_falls_back_when_the_session_directory_is_missing() {
        let sandbox = Sandbox::new();
        let workspace = sandbox.dir("workspace");
        let gone = sandbox.root.path().join("gone");
        let target = ResumeTarget {
            source: Source::Pi,
            path: sandbox.root.path().join("s.jsonl"),
            project_path: Some(gone.clone()),
        };

        let plan = sandbox.plan(&target, Some(&workspace), false, &[]).unwrap();
        assert_eq!(plan.cwd, Some(workspace));
        assert_eq!(plan.warnings.len(), 1);
        assert!(plan.warnings[0].contains(&gone.display().to_string()));

        let plan = sandbox.plan(&target, None, false, &[]).unwrap();
        assert_eq!(plan.cwd, None);
    }

    // --- GUI semantics for Claude. ---

    #[test]
    fn resume_runs_in_the_project_dir_that_maps_to_the_session() {
        let sandbox = Sandbox::new();
        let project = sandbox.dir("project");
        let workspace = sandbox.dir("workspace");
        let session = sandbox.session_in(&project);
        let defaults = vec!["--model".to_string(), "opus".to_string()];

        for workspace in [None, Some(workspace.as_path())] {
            let plan = sandbox
                .plan(
                    &sandbox.target(&session, Some(&project)),
                    workspace,
                    false,
                    &defaults,
                )
                .unwrap();
            assert_eq!(plan.strategy, ResumeStrategy::InProject);
            assert_eq!(plan.program, "claude");
            assert_eq!(plan.cwd, Some(project.clone()));
            assert_eq!(plan.args, os(&["--resume", UUID, "--model", "opus"]));
            assert!(plan.copy.is_none());
        }
    }

    #[test]
    fn resume_without_a_usable_project_dir_needs_a_workspace() {
        let sandbox = Sandbox::new();
        let elsewhere = sandbox.dir("elsewhere");
        let original = sandbox.root.path().join("deleted-worktree");
        let session = sandbox.session_in(&original);

        // Missing project dir, and a project dir that maps elsewhere.
        for project in [Some(original.as_path()), Some(elsewhere.as_path()), None] {
            let error = sandbox
                .plan(&sandbox.target(&session, project), None, false, &[])
                .unwrap_err();
            assert!(matches!(error, AppError::WorkspaceRequired(_)));
            assert_eq!(error.to_string(), WORKSPACE_REQUIRED_RESUME);
        }
    }

    #[test]
    fn resume_copies_into_the_workspace_when_the_project_dir_is_gone() {
        let sandbox = Sandbox::new();
        let original = sandbox.root.path().join("deleted-worktree");
        let workspace = sandbox.dir("workspace");
        let session = sandbox.session_in(&original);
        let source_dir = session.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(source_dir.join(UUID).join("tool-results")).unwrap();
        std::fs::write(
            source_dir.join(UUID).join("tool-results").join("r.txt"),
            "out",
        )
        .unwrap();

        let plan = sandbox
            .plan(
                &sandbox.target(&session, Some(&original)),
                Some(&workspace),
                false,
                &["--verbose".to_string()],
            )
            .unwrap();
        let target_dir = sandbox.env.claude_projects_dir_for(&workspace).unwrap();
        assert_eq!(plan.strategy, ResumeStrategy::CopyToWorkspace);
        assert_eq!(plan.cwd, Some(workspace.clone()));
        assert_eq!(plan.args, os(&["--resume", UUID, "--verbose"]));
        let copy = plan.copy.clone().unwrap();
        assert_eq!(copy.target_dir, target_dir);
        assert_eq!(copy.source_projects_dir, source_dir);
        assert_eq!(
            copy.files,
            vec![CopyItem {
                from: session.clone(),
                to: target_dir.join(format!("{UUID}.jsonl")),
            }]
        );
        assert_eq!(
            copy.dirs,
            vec![CopyItem {
                from: source_dir.join(UUID),
                to: target_dir.join(UUID),
            }]
        );
        assert!(copy.existing_targets.is_empty());
        assert!(plan.warnings.is_empty());

        let report = plan.execute_copy().unwrap().unwrap();
        assert_eq!(report.files_copied, 1);
        assert_eq!(report.dirs_copied, 1);
        assert_eq!(
            std::fs::read_to_string(target_dir.join(format!("{UUID}.jsonl"))).unwrap(),
            transcript()
        );
        assert_eq!(
            std::fs::read_to_string(target_dir.join(UUID).join("tool-results").join("r.txt"))
                .unwrap(),
            "out"
        );
        // The original is untouched.
        assert!(session.is_file());

        // Planning again reports the overwrite.
        let again = sandbox
            .plan(
                &sandbox.target(&session, Some(&original)),
                Some(&workspace),
                false,
                &[],
            )
            .unwrap();
        assert_eq!(again.copy.unwrap().existing_targets.len(), 2);
        assert_eq!(again.warnings.len(), 2);
    }

    #[test]
    fn resume_runs_in_place_when_the_workspace_already_holds_the_session() {
        // Upstream planned a copy onto the session itself here (project dir
        // gone, cwd mapping to the session's dir), truncating it on Linux.
        let sandbox = Sandbox::new();
        let workspace = sandbox.dir("workspace");
        let session = sandbox.session_in(&workspace);
        let gone = sandbox.root.path().join("gone");

        for fork in [false, true] {
            let plan = sandbox
                .plan(
                    &sandbox.target(&session, Some(&gone)),
                    Some(&workspace),
                    fork,
                    &[],
                )
                .unwrap();
            assert_eq!(plan.strategy, ResumeStrategy::InWorkspace);
            assert_eq!(plan.cwd, Some(workspace.clone()));
            assert!(plan.copy.is_none());
            let expected: &[&str] = if fork {
                &["--resume", UUID, "--fork-session"]
            } else {
                &["--resume", UUID]
            };
            assert_eq!(plan.args, os(expected));
        }
        // Upstream's own decision for the same inputs is the self-copy.
        assert_eq!(
            resolve_claude_resume_action(&sandbox.env, &session, Some(&gone), &workspace, false)
                .unwrap(),
            ClaudeResumeAction::CopyToCurrent {
                cwd_projects_dir: session.parent().unwrap().to_path_buf()
            }
        );
    }

    #[test]
    fn fork_into_another_workspace_copies_without_fork_session() {
        let sandbox = Sandbox::new();
        let project = sandbox.dir("project");
        let workspace = sandbox.dir("workspace");
        let session = sandbox.session_in(&project);

        let plan = sandbox
            .plan(
                &sandbox.target(&session, Some(&project)),
                Some(&workspace),
                true,
                &["--model".to_string(), "opus".to_string()],
            )
            .unwrap();
        assert_eq!(plan.kind, ResumeKind::Fork);
        assert_eq!(plan.strategy, ResumeStrategy::CopyToWorkspace);
        assert_eq!(plan.cwd, Some(workspace.clone()));
        assert_eq!(plan.args, os(&["--resume", UUID, "--model", "opus"]));
        assert_eq!(
            plan.copy.as_ref().unwrap().target_dir,
            sandbox.env.claude_projects_dir_for(&workspace).unwrap()
        );
        // No session directory existed, so only the transcript is copied.
        assert!(plan.copy.as_ref().unwrap().dirs.is_empty());
    }

    #[test]
    fn fork_in_place_uses_fork_session_in_the_project_dir() {
        let sandbox = Sandbox::new();
        let project = sandbox.dir("project");
        let session = sandbox.session_in(&project);

        // No workspace, or a workspace mapping to the same projects dir.
        for workspace in [None, Some(project.as_path())] {
            let plan = sandbox
                .plan(
                    &sandbox.target(&session, Some(&project)),
                    workspace,
                    true,
                    &["--verbose".to_string()],
                )
                .unwrap();
            assert_eq!(plan.strategy, ResumeStrategy::InProject);
            assert_eq!(plan.cwd, Some(project.clone()));
            assert_eq!(
                plan.args,
                os(&["--resume", UUID, "--fork-session", "--verbose"])
            );
            assert!(plan.copy.is_none());
        }
    }

    #[test]
    fn workspace_spelling_never_turns_an_in_place_fork_into_a_copy() {
        // A workspace typed as `C:\proj\` (trailing separator), `proj\.` or
        // `x\..\proj` used to be encoded verbatim (`C--proj-`), so it never
        // mapped to the session's projects dir: fork planned a copy into a
        // bogus projects dir and then ran `claude --resume` without
        // `--fork-session` where Claude Code found the ORIGINAL session.
        let sandbox = Sandbox::new();
        let project = sandbox.dir("project");
        let session = sandbox.session_in(&project);
        let sep = std::path::MAIN_SEPARATOR_STR;
        let dir = project.display().to_string();
        for spelling in [
            format!("{dir}{sep}"),
            format!("{dir}{sep}{sep}"),
            format!("{dir}{sep}."),
            format!("{dir}{sep}..{sep}project{sep}"),
        ] {
            let workspace = Path::new(&spelling);
            for project_path in [None, Some(project.as_path())] {
                let plan = sandbox
                    .plan(
                        &sandbox.target(&session, project_path),
                        Some(workspace),
                        true,
                        &[],
                    )
                    .unwrap();
                assert_ne!(plan.strategy, ResumeStrategy::CopyToWorkspace, "{spelling}");
                assert!(plan.copy.is_none(), "{spelling}");
                assert_eq!(plan.args, os(&["--resume", UUID, "--fork-session"]));
                assert_eq!(plan.cwd, Some(project.clone()), "{spelling}");
            }
            let plan = sandbox
                .plan(&sandbox.target(&session, None), Some(workspace), false, &[])
                .unwrap();
            assert_eq!(plan.strategy, ResumeStrategy::InWorkspace, "{spelling}");
            assert_eq!(plan.cwd, Some(project.clone()), "{spelling}");
            assert!(plan.copy.is_none(), "{spelling}");
        }
        // Upstream's decision function agrees once the cwd is normalized.
        assert_eq!(
            resolve_claude_resume_action(
                &sandbox.env,
                &session,
                Some(&project),
                Path::new(&format!("{dir}{sep}")),
                true,
            )
            .unwrap(),
            ClaudeResumeAction::Run {
                current_dir: PathBuf::from(format!("{dir}{sep}"))
            }
        );
        // A relative workspace cannot be encoded and is refused.
        assert!(matches!(
            WorkspaceScope::new("project"),
            Err(AppError::InvalidWorkspace(_))
        ));
    }

    #[test]
    fn fork_without_workspace_or_project_dir_is_an_error() {
        let sandbox = Sandbox::new();
        let session = sandbox.session_in(&sandbox.root.path().join("gone"));
        let error = sandbox
            .plan(&sandbox.target(&session, None), None, true, &[])
            .unwrap_err();
        assert_eq!(error.to_string(), WORKSPACE_REQUIRED_FORK);
    }

    #[test]
    fn missing_workspace_folder_is_reported() {
        let sandbox = Sandbox::new();
        let session = sandbox.session_in(&sandbox.root.path().join("gone"));
        let missing = sandbox.root.path().join("no-such-workspace");
        let error = sandbox
            .plan(&sandbox.target(&session, None), Some(&missing), false, &[])
            .unwrap_err();
        assert!(matches!(error, AppError::WorkspaceRequired(_)));
        assert!(error.to_string().contains("does not exist"), "{error}");
    }

    #[test]
    fn paths_without_a_file_name_fail_like_upstream() {
        let sandbox = Sandbox::new();
        let workspace = sandbox.dir("workspace");
        let root_only = ResumeTarget {
            source: Source::Claude,
            path: PathBuf::from("/"),
            project_path: None,
        };
        let error = sandbox
            .plan(&root_only, Some(&workspace), false, &[])
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Failed to run Claude CLI: Conversation filename is not valid Unicode"
        );
    }

    #[test]
    fn execute_copy_skips_a_copy_onto_itself() {
        let sandbox = Sandbox::new();
        let project = sandbox.dir("project");
        let session = sandbox.session_in(&project);
        let dir = session.parent().unwrap().to_path_buf();
        let plan = plan_session_copy(&session, UUID, &dir, &dir).unwrap();
        assert!(plan.existing_targets.is_empty());

        let report = execute_copy(&plan).unwrap();
        assert_eq!(report.files_copied, 0);
        assert_eq!(
            report.skipped_same_path,
            vec![dir.join(format!("{UUID}.jsonl"))]
        );
        assert_eq!(std::fs::read_to_string(&session).unwrap(), transcript());
    }

    #[test]
    fn dot_stem_transcripts_are_never_planned_or_copied() {
        // A stray `..jsonl` has the stem `.`: upstream planned
        // `<src project>/.` as the session directory, copying the whole
        // source project over the workspace project (and `--resume .`).
        let sandbox = Sandbox::new();
        let source = sandbox.dir("source");
        let workspace = sandbox.dir("workspace");
        let source_dir = sandbox.session_in(&source).parent().unwrap().to_path_buf();
        std::fs::create_dir_all(source_dir.join("other-session")).unwrap();
        std::fs::write(source_dir.join("other-session").join("tool.txt"), "x").unwrap();
        let workspace_dir = sandbox
            .session_in(&workspace)
            .parent()
            .unwrap()
            .to_path_buf();
        let continued = workspace_dir.join(format!("{UUID}.jsonl"));
        std::fs::write(&continued, "CONTINUED IN WORKSPACE").unwrap();

        for name in ["..jsonl", "abc..jsonl", "a b.jsonl"] {
            let stray = source_dir.join(name);
            std::fs::write(&stray, transcript()).unwrap();
            for fork in [false, true] {
                let error = sandbox
                    .plan(&sandbox.target(&stray, None), Some(&workspace), fork, &[])
                    .unwrap_err();
                assert_eq!(
                    error.to_string(),
                    format!(
                        "Failed to run Claude CLI: Not a Claude session transcript: {}",
                        stray.display()
                    ),
                    "{name}"
                );
            }
            // Also refused in the session's own project (no `--resume .`).
            assert!(
                sandbox
                    .plan(&sandbox.target(&stray, Some(&source)), None, false, &[])
                    .is_err(),
                "{name}"
            );
            let stem = stray.file_stem().unwrap().to_str().unwrap();
            assert!(
                plan_session_copy(&stray, stem, &source_dir, &workspace_dir).is_err(),
                "{name}"
            );
        }
        // A session id that is not the transcript's own stem is refused too.
        let session = source_dir.join(format!("{UUID}.jsonl"));
        assert!(plan_session_copy(&session, ".", &source_dir, &workspace_dir).is_err());

        assert_eq!(
            std::fs::read_to_string(&continued).unwrap(),
            "CONTINUED IN WORKSPACE"
        );
        assert!(!workspace_dir.join("other-session").exists());
    }

    #[test]
    fn program_overrides_flow_into_the_plan_and_launch_spec() {
        let sandbox = Sandbox::new();
        let project = sandbox.dir("project");
        let session = sandbox.session_in(&project);
        let bin = sandbox.dir("bin");
        let claude = bin.join("claude.cmd");
        std::fs::write(&claude, "@echo off\n").unwrap();
        let programs = ProgramOverrides {
            claude: Some(claude.clone()),
            ..ProgramOverrides::default()
        };
        let plan = plan_resume_target(
            &sandbox.env,
            &sandbox.target(&session, Some(&project)),
            None,
            false,
            &[],
            &programs,
        )
        .unwrap();
        assert_eq!(plan.program_override, Some(claude.clone()));
        let spec = plan.launch_spec(&sandbox.env).unwrap();
        assert_eq!(spec.program.path, claude);
        assert_eq!(spec.program.kind, crate::program::ProgramKind::BatchScript);
        assert_eq!(spec.cwd, Some(project));

        // Without an override and with no PATH, resolution fails cleanly.
        let plain = ResumePlan {
            program_override: None,
            ..plan
        };
        let error = plain.launch_spec(&sandbox.env).unwrap_err();
        assert!(
            error.to_string().contains("Could not find 'claude'"),
            "{error}"
        );
    }

    #[test]
    fn plan_resume_reads_the_conversation() {
        let sandbox = Sandbox::new();
        let project = sandbox.dir("project");
        let session = sandbox.session_in(&project);
        let conversation = crate::history::process_conversation_file(session.clone(), None, None)
            .unwrap()
            .expect("conversation");
        let conversation = Conversation {
            project_path: Some(project.clone()),
            ..conversation
        };
        let plan = plan_resume(
            &sandbox.env,
            &conversation,
            None,
            false,
            &[],
            &ProgramOverrides::default(),
        )
        .unwrap();
        assert_eq!(plan.session_path, session);
        assert_eq!(plan.cwd, Some(project));
        assert_eq!(plan.display_command(), format!("claude --resume {UUID}"));
    }

    #[test]
    fn display_command_quotes_arguments() {
        let plan = ResumePlan {
            source: Source::Pi,
            kind: ResumeKind::Resume,
            strategy: ResumeStrategy::Native,
            session_path: PathBuf::from("/tmp/pi sessions/s.jsonl"),
            program: "pi".to_string(),
            program_override: None,
            args: vec![
                OsString::from("--session"),
                OsString::from("/tmp/pi sessions/it's.jsonl"),
            ],
            cwd: None,
            copy: None,
            warnings: Vec::new(),
        };
        assert_eq!(
            plan.display_command(),
            "pi --session '/tmp/pi sessions/it'\\''s.jsonl'"
        );
        assert_eq!(
            program_name(Source::Omp),
            OsStr::new("omp").to_str().unwrap()
        );
    }
}
