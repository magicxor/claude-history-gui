//! What the tool that wrote a conversation can do with it, as its adapter
//! plans it: resume or fork it in that tool, delete it, rename it.
//!
//! # Differences from upstream
//!
//! * **The fork's plans.** Upstream's `Resume` (a `std::process::Command`
//!   plus an optional `SessionCopy` that the CLI applies before it runs the
//!   command in its own terminal) is not ported. A resume plans into
//!   [`crate::resume::ResumePlan`] (program, arguments, working directory, a
//!   [`crate::resume::CopyPlan`] the GUI confirms before it runs, warnings),
//!   which the GUI launches in a new terminal window; `resume::execute_copy`
//!   and its tests cover `SessionCopy::apply` and its test. Planning takes the
//!   [`crate::env::HistoryEnv`] and the workspace that replaces upstream's
//!   process cwd (see [`super::adapter::Adapter::resume`]).
//! * **[`refusal`]** also formats the texts of the core errors that carry a
//!   refusal (`RenameError::Unsupported`, `DeleteError::Unsupported`,
//!   [`crate::error::AppError::Unsupported`]); upstream shows them in the
//!   TUI's status line only.
//! * **[`Launcher`] with an explicit env (upstream 8a99ccc).** Upstream
//!   expands `~/` with the process's home and resolves a relative program
//!   path against its cwd when it builds the launcher, and looks the program
//!   up on the process's `PATH` (`Launcher::check`). Here
//!   [`Launcher::configured`] takes the [`HistoryEnv`]: `~/` (also `~\` on
//!   Windows) is its home and a relative path resolves against
//!   [`HistoryEnv::env_value_base_dir`] (the GUI's launch directory, else the
//!   workspace, as the `PI_*` paths do); without either it stays relative
//!   and is never found (core reads no process cwd). [`Launcher::resolve`]
//!   (and [`Launcher::check`]) find the program with the fork's resolver
//!   ([`crate::program::find_program_path`]: the env's `PATH`, `PATHEXT` and
//!   `.cmd`/`.bat` scripts run through `cmd.exe` on Windows, the executable
//!   bit elsewhere). Upstream's `Launcher::command` (a
//!   `std::process::Command`) is not ported: a plan holds the launcher, and
//!   [`crate::resume::ResumePlan::launch_spec`] puts its leading arguments
//!   before the agent's. [`Launcher::configured_at`] is a [`ConfiguredAt`]
//!   naming the file and key, because the GUI reads two files and also takes
//!   a program path from `[gui] claude_path` ... (decision 17,
//!   [`crate::config::agent_launcher`]); the error names them
//!   (``check `command` under [resume.pi] in gui.toml``), where upstream says
//!   `in the config file`, and for the agent's own binary suggests setting
//!   `command` `in gui.toml or config.toml`.

use crate::env::HistoryEnv;
use crate::error::{AppError, Result};
use crate::history::Provenance;
use crate::history::model::SourceKind;
use crate::history::sources::Registry;
use crate::program::ResolvedProgram;
use std::path::{Path, PathBuf};

/// The actions an adapter supports for its conversations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Capabilities {
    pub resume: bool,
    pub fork: bool,
    pub delete: bool,
    pub rename: bool,
}

impl Capabilities {
    pub const ALL: Self = Self {
        resume: true,
        fork: true,
        delete: true,
        rename: true,
    };

    pub const NONE: Self = Self {
        resume: false,
        fork: false,
        delete: false,
        rename: false,
    };
}

/// The actions the adapter of `source` supports for a conversation of
/// `provenance`: none for an orphaned one, which its tool no longer has.
pub fn capabilities_of(source: SourceKind, provenance: Provenance) -> Capabilities {
    if provenance.orphaned {
        return Capabilities::NONE;
    }
    // Capabilities belong to the kind of source, not to an instance.
    Registry::with_roots(None, None, None)
        .for_source(source)
        .capabilities_for(provenance)
}

/// Why `doing` ("Deleting", ...) is unavailable for a conversation of
/// `source` and `provenance`.
pub fn refusal(source: SourceKind, provenance: Provenance, doing: &str) -> String {
    if provenance.orphaned {
        format!(
            "{doing} is unavailable: {} no longer has this conversation's current history",
            source.list_label()
        )
    } else if provenance.child || provenance.branch {
        format!(
            "{doing} {} subagents and forks is not supported",
            source.list_label()
        )
    } else {
        format!("{doing} {} sessions is not supported", source.list_label())
    }
}

/// A conversation to act on.
#[derive(Clone, Copy, Debug)]
pub struct Target<'a> {
    /// The conversation's own artifact.
    pub path: &'a Path,
    /// The project directory the conversation is listed under.
    pub project: Option<&'a Path>,
}

/// Where the user configured a [`Launcher`]. Fork: upstream names only the
/// table, of its one config file; the GUI reads `config.toml` and
/// `gui.toml`, and also takes a program path from `[gui]`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfiguredAt {
    /// `command` under the table (`[resume]`, `[resume.pi]`, ...) of
    /// `config.toml`.
    Config(&'static str),
    /// `command` under the table of `gui.toml`.
    Gui(&'static str),
    /// The program path under `[gui]` of `gui.toml`: the key
    /// (`claude_path`, `pi_path`, `omp_path`, `codex_path`).
    GuiPath(&'static str),
}

impl ConfiguredAt {
    /// The table: `[resume]`, `[resume.pi]`, ..., or `[gui]`.
    pub fn table(self) -> &'static str {
        match self {
            Self::Config(table) | Self::Gui(table) => table,
            Self::GuiPath(_) => "[gui]",
        }
    }

    /// The key in [`Self::table`]: `command`, or a `[gui]` program path key.
    pub fn key(self) -> &'static str {
        match self {
            Self::Config(_) | Self::Gui(_) => "command",
            Self::GuiPath(key) => key,
        }
    }

    /// The file: `config.toml` or `gui.toml`.
    pub fn file(self) -> &'static str {
        match self {
            Self::Config(_) => crate::env::CLI_CONFIG_FILE_NAME,
            Self::Gui(_) | Self::GuiPath(_) => crate::env::GUI_CONFIG_FILE_NAME,
        }
    }
}

/// `` `command` under [resume.pi] in gui.toml ``.
impl std::fmt::Display for ConfiguredAt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "`{}` under {} in {}",
            self.key(),
            self.table(),
            self.file()
        )
    }
}

/// The program an agent is started with, and the arguments that precede the
/// agent's own: the agent's binary by default, or the user's wrapper.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Launcher {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Where the user configured it, for errors; `None` for the default.
    pub configured_at: Option<ConfiguredAt>,
}

impl Launcher {
    /// The agent's own binary, found on `PATH`.
    pub fn default_for(source: SourceKind) -> Self {
        Self {
            program: PathBuf::from(source.label()),
            args: Vec::new(),
            configured_at: None,
        }
    }

    /// The configured `command`, its first element the program: a name
    /// found on `PATH`, or a path. `~/` starts a path in the home
    /// directory, and a relative path is relative to the directory the GUI
    /// was started in (fork: `env`'s, see the module docs).
    pub fn configured(env: &HistoryEnv, command: &[String], configured_at: ConfiguredAt) -> Self {
        let (program, args) = command
            .split_first()
            .expect("configured commands are not empty");
        Self {
            program: absolute_program(env, Path::new(program)),
            args: args.to_vec(),
            configured_at: Some(configured_at),
        }
    }

    /// Fork: a program path from `[gui]` (`claude_path`, ...): the program
    /// alone, resolved like a configured command's.
    pub fn program_path(env: &HistoryEnv, path: &Path, configured_at: ConfiguredAt) -> Self {
        Self {
            program: absolute_program(env, path),
            args: Vec::new(),
            configured_at: Some(configured_at),
        }
    }

    /// Fork: the program to run, found on the env's `PATH` (with `PATHEXT`
    /// on Windows) or by its path; the error of [`Self::check`] when there
    /// is none.
    pub fn resolve(&self, env: &HistoryEnv, source: SourceKind) -> Result<ResolvedProgram> {
        crate::program::find_program_path(
            &self.program,
            env.path_var.as_deref(),
            env.pathext.as_deref(),
            cfg!(windows),
        )
        .ok_or_else(|| self.missing(source))
    }

    /// Fails, naming the program and where it was configured, when it does
    /// not exist or is not executable.
    pub fn check(&self, env: &HistoryEnv, source: SourceKind) -> Result<()> {
        self.resolve(env, source).map(|_| ())
    }

    fn missing(&self, source: SourceKind) -> AppError {
        let program = self.program.display();
        let found = if has_separator(&self.program) {
            "is not an executable file"
        } else {
            "was not found on PATH"
        };
        let hint = match self.configured_at {
            Some(at) => format!("check {at}"),
            None => format!(
                "if you start {} through a wrapper, set `command` under {} in {} or {}",
                source.assistant_name(),
                config_table(source),
                crate::env::GUI_CONFIG_FILE_NAME,
                crate::env::CLI_CONFIG_FILE_NAME,
            ),
        };
        AppError::AgentLaunch(format!("`{program}` {found}; {hint}"))
    }

    /// Fork: the program as configured and its leading arguments, for
    /// display (`mise exec -- claude`).
    pub fn words(&self) -> Vec<String> {
        std::iter::once(self.program.to_string_lossy().into_owned())
            .chain(self.args.iter().cloned())
            .collect()
    }
}

/// The config table whose `command` launches `source`.
pub fn config_table(source: SourceKind) -> &'static str {
    match source {
        SourceKind::Claude => "[resume]",
        SourceKind::Pi => "[resume.pi]",
        SourceKind::Omp => "[resume.omp]",
        SourceKind::Codex => "[resume.codex]",
    }
}

/// How to start the agent of a conversation: its launcher, and the user's
/// extra arguments for agents that take them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Launch {
    pub launcher: Launcher,
    pub default_args: Vec<String>,
}

impl Launch {
    /// Fork: the agent's own binary and no extra arguments.
    pub fn default_for(source: SourceKind) -> Self {
        Self {
            launcher: Launcher::default_for(source),
            default_args: Vec::new(),
        }
    }
}

/// Whether `program` names a path rather than a program on `PATH`.
fn has_separator(program: &Path) -> bool {
    crate::program::names_a_path(program, cfg!(windows))
}

/// Upstream `absolute_program`, with the home and the base of relative
/// paths from `env` (see the module docs).
fn absolute_program(env: &HistoryEnv, program: &Path) -> PathBuf {
    let path = match home_relative(program) {
        Some(rest) => match &env.home_dir {
            Some(home) => home.join(rest),
            None => program.to_path_buf(),
        },
        None => program.to_path_buf(),
    };
    if path.is_relative() && has_separator(&path) {
        // The agent runs in the conversation's project, not here.
        if let Some(base) = env.env_value_base_dir() {
            return base.join(path);
        }
    }
    path
}

/// The rest of `program` after a leading `~/` (also `~\` on Windows).
fn home_relative(program: &Path) -> Option<&Path> {
    let text = program.to_str()?;
    text.strip_prefix("~/")
        .or_else(|| {
            if cfg!(windows) {
                text.strip_prefix("~\\")
            } else {
                None
            }
        })
        .map(Path::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::model::SourceKind;
    use crate::history::omp_loader::OmpSessionRoot;
    use crate::history::pi_loader::PiSessionRoot;
    use crate::history::sources::Registry;
    use std::fs;
    use std::path::PathBuf;

    fn executable(path: &Path) {
        fs::write(path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn launchers_default_to_the_agent_binary() {
        for (source, program) in [
            (SourceKind::Claude, "claude"),
            (SourceKind::Pi, "pi"),
            (SourceKind::Omp, "omp"),
            (SourceKind::Codex, "codex"),
        ] {
            let launcher = Launcher::default_for(source);
            assert_eq!(launcher.program, Path::new(program));
            assert!(launcher.args.is_empty());
            assert_eq!(launcher.configured_at, None);
        }
    }

    /// Fork: the leading arguments reach the command through
    /// `ResumePlan::launch_spec` (`resume::tests`); here the launcher keeps
    /// them, before the agent's own.
    #[test]
    fn configured_launchers_lead_with_their_arguments() {
        let env = HistoryEnv::sandbox(Path::new("/home/me"));
        let command = ["mise", "exec", "--", "claude"].map(String::from);
        let launcher = Launcher::configured(&env, &command, ConfiguredAt::Gui("[resume]"));
        assert_eq!(launcher.program, Path::new("mise"));
        assert_eq!(launcher.args, ["exec", "--", "claude"]);
        assert_eq!(launcher.words(), ["mise", "exec", "--", "claude"]);
        assert_eq!(launcher.configured_at, Some(ConfiguredAt::Gui("[resume]")));
    }

    /// Fork: `~/` is the env's home and a relative path resolves against the
    /// launch directory (else the workspace), never the process's.
    #[test]
    fn configured_programs_resolve_home_and_relative_paths() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let launch = root.path().join("launch");
        let env = HistoryEnv::sandbox(&home).with_env_path_base(&launch);
        let at = ConfiguredAt::Config("[resume]");
        let launcher = Launcher::configured(&env, &["~/bin/claude".to_owned()], at);
        assert_eq!(launcher.program, home.join("bin/claude"));
        let launcher = Launcher::configured(&env, &["bin/claude".to_owned()], at);
        assert_eq!(launcher.program, launch.join("bin/claude"));
        let launcher = Launcher::configured(&env, &["my-claude".to_owned()], at);
        assert_eq!(launcher.program, Path::new("my-claude"));

        // Without a launch directory: the workspace; without either, the
        // path stays relative and is never looked up.
        let workspace = root.path().join("work");
        let env = HistoryEnv::sandbox(&home).with_workspace(&workspace);
        let launcher = Launcher::configured(&env, &["bin/claude".to_owned()], at);
        assert_eq!(launcher.program, workspace.join("bin/claude"));
        let env = HistoryEnv::sandbox(&home);
        let launcher = Launcher::configured(&env, &["bin/claude".to_owned()], at);
        assert_eq!(launcher.program, Path::new("bin/claude"));
        let error = launcher.check(&env, SourceKind::Claude).unwrap_err();
        assert!(
            error.to_string().contains("is not an executable file"),
            "{error}"
        );
    }

    #[test]
    fn programs_are_found_on_path_or_by_path() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir(&bin).unwrap();
        // Windows finds a program by its PATHEXT extensions.
        let name = if cfg!(windows) {
            "my-claude.cmd"
        } else {
            "my-claude"
        };
        executable(&bin.join(name));
        fs::write(bin.join("data"), "").unwrap();
        let mut env = HistoryEnv::sandbox(dir.path());
        env.path_var = Some(std::env::join_paths([dir.path(), &bin]).unwrap());
        let at = ConfiguredAt::Gui("[resume]");
        let found = |env: &HistoryEnv, program: &Path| {
            Launcher::program_path(env, program, at)
                .resolve(env, SourceKind::Claude)
                .ok()
                .map(|program| program.path)
        };

        assert_eq!(found(&env, Path::new("my-claude")), Some(bin.join(name)));
        assert_eq!(found(&env, Path::new("missing")), None);
        assert_eq!(found(&env, &bin.join(name)), Some(bin.join(name)));
        assert_eq!(found(&env, &bin.join("missing")), None);
        #[cfg(unix)]
        assert_eq!(found(&env, Path::new("data")), None);
        // Without PATH, only a path is found.
        env.path_var = None;
        assert_eq!(found(&env, Path::new("my-claude")), None);
        assert_eq!(found(&env, &bin.join(name)), Some(bin.join(name)));
    }

    #[test]
    fn missing_programs_name_their_config_key() {
        let env = HistoryEnv::sandbox(Path::new("/nonexistent/claude-history-test"));
        let missing = Launcher::configured(
            &env,
            &["/nonexistent/claude-history-test/agent".to_owned()],
            ConfiguredAt::Config("[resume.pi]"),
        );
        let message = missing.check(&env, SourceKind::Pi).unwrap_err().to_string();
        assert!(
            message.contains("/nonexistent/claude-history-test/agent"),
            "{message}"
        );
        assert!(message.contains("is not an executable file"), "{message}");
        assert!(
            message.ends_with("check `command` under [resume.pi] in config.toml"),
            "{message}"
        );

        let default = Launcher {
            program: PathBuf::from("claude-history-test-missing-agent"),
            ..Launcher::default_for(SourceKind::Codex)
        };
        let message = default
            .check(&env, SourceKind::Codex)
            .unwrap_err()
            .to_string();
        assert_eq!(
            message,
            "Cannot start agent: `claude-history-test-missing-agent` was not found on PATH; if \
             you start Codex through a wrapper, set `command` under [resume.codex] in gui.toml \
             or config.toml"
        );

        // Fork: a `[gui]` program path names its key.
        let path = Launcher::program_path(
            &env,
            Path::new("/nonexistent/claude-history-test/claude.exe"),
            ConfiguredAt::GuiPath("claude_path"),
        );
        let message = path
            .check(&env, SourceKind::Claude)
            .unwrap_err()
            .to_string();
        assert!(
            message.ends_with("check `claude_path` under [gui] in gui.toml"),
            "{message}"
        );
    }

    #[test]
    fn every_source_has_an_acting_adapter() {
        let registry = Registry::with_roots(None, None, None);
        for source in [
            SourceKind::Claude,
            SourceKind::Pi,
            SourceKind::Omp,
            SourceKind::Codex,
        ] {
            let adapter = registry.for_source(source);
            assert_eq!(adapter.instance().kind, source);
            let capabilities = adapter.capabilities();
            assert_eq!(capabilities.rename, source != SourceKind::Codex);
            assert!(capabilities.resume && capabilities.fork && capabilities.delete);
        }
    }

    /// Fork: the capabilities of a conversation, as the GUI gates its actions.
    #[test]
    fn orphaned_conversations_support_nothing() {
        let orphaned = Provenance {
            orphaned: true,
            ..Provenance::default()
        };
        for source in [
            SourceKind::Claude,
            SourceKind::Pi,
            SourceKind::Omp,
            SourceKind::Codex,
        ] {
            assert_eq!(capabilities_of(source, orphaned), Capabilities::NONE);
        }
        assert_eq!(
            capabilities_of(SourceKind::Claude, Provenance::default()),
            Capabilities::ALL
        );
        assert_eq!(
            capabilities_of(SourceKind::Codex, Provenance::default()),
            Capabilities {
                rename: false,
                ..Capabilities::ALL
            }
        );
        // Hidden threads still have their tool's actions.
        let child = Provenance {
            child: true,
            ..Provenance::default()
        };
        assert!(capabilities_of(SourceKind::Codex, child).resume);
    }

    #[test]
    fn the_configured_instance_acts_on_its_sources_conversations() {
        let root = |name: &str| PathBuf::from("roots").join(name);
        let registry = Registry::with_roots(
            Some(root("claude")),
            Some(PiSessionRoot {
                path: root("pi"),
                flat: false,
            }),
            Some(OmpSessionRoot {
                path: root("omp"),
                flat: true,
            }),
        );
        for (source, name) in [
            (SourceKind::Claude, "claude"),
            (SourceKind::Pi, "pi"),
            (SourceKind::Omp, "omp"),
        ] {
            let adapter = registry.for_source(source);
            assert_eq!(adapter.instance().kind, source);
            assert_eq!(adapter.instance().root, root(name));
        }
    }

    #[test]
    fn refusals_name_the_source_as_the_list_does() {
        let plain = Provenance::default();
        assert_eq!(
            refusal(SourceKind::Pi, plain, "Deleting"),
            "Deleting Pi sessions is not supported"
        );
        assert_eq!(
            refusal(SourceKind::Codex, plain, "Renaming"),
            "Renaming CX sessions is not supported"
        );
        let orphaned = Provenance {
            orphaned: true,
            ..plain
        };
        assert_eq!(
            refusal(SourceKind::Codex, orphaned, "Resuming"),
            "Resuming is unavailable: CX no longer has this conversation's current history"
        );
    }
}
