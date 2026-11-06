//! Codex: threads whose history is one or more JSONL rollouts under a Codex
//! home (`CODEX_HOME`, else `~/.codex`), chosen with the help of the state
//! database (see `state_db`).
//!
//! A thread's native id is its thread id. Codex's own CLI acts on threads
//! by id, with `CODEX_HOME` selecting the home.
//!
//! # Differences from upstream
//!
//! * **The home and the state database from a [`HistoryEnv`].** Upstream
//!   reads `CODEX_HOME` and `CODEX_SQLITE_HOME` from the process
//!   environment (`home_from_env`) and resolves relative values against its
//!   cwd. Here they are [`HistoryEnv`] fields
//!   ([`HistoryEnv::codex_home`], [`HistoryEnv::codex_sqlite_home`]: relative
//!   values resolve against the launch directory, else the workspace, like
//!   the `PI_*` paths), and [`CodexAdapter::from_env`] builds the adapter of
//!   the env's home. The state database directory is resolved when the home
//!   is scanned, not when the adapter is built (upstream does the same from
//!   eea6d37 on), so building a registry reads no file.
//! * **The state database directory, public.** [`state_db_dir`] (the
//!   adapter's resolution: `config.toml`'s `sqlite_home`, else
//!   `CODEX_SQLITE_HOME`, else the home) and [`configured_sqlite_home`] are
//!   public (upstream: private to the adapter), so the GUI's QA sandbox gate
//!   checks the directory the adapter opens without resolving it again.
//! * **Actions as the fork's plans.** Upstream plans `codex [args]
//!   resume|fork <id>` as a `std::process::Command` with `CODEX_HOME` set
//!   and runs `codex delete --force <id>` itself. Here
//!   [`CodexAdapter::resume`] plans a [`ResumePlan`] (`resume::plan_codex`)
//!   whose `env` carries `CODEX_HOME` into the terminal window, and the
//!   deletion is a [`delete_spec`] run by [`run_to_completion`] (core spawns
//!   processes only in `program` and `resume::terminal`; refused in tests and
//!   hermetic builds). A GUI plans the deletion first and runs the same
//!   command through its own services after a confirmation
//!   (`maintenance::plan_session_delete`,
//!   `maintenance::execute_session_delete_with`). The CLI is the
//!   [`Launcher`] the caller passes (upstream 8a99ccc: `[resume.codex]
//!   command`, here also `[gui] codex_path`, see `config::agent_launcher`),
//!   else `codex` on the env's `PATH`.

mod decode;
pub mod discover;
mod head;
mod state_db;
pub mod view;

use crate::env::{HistoryEnv, WorkspaceScope};
use crate::error::{AppError, Result};
use crate::history::actions::{Capabilities, Launch, Launcher, Target};
use crate::history::adapter::{Adapter, Completeness, Discovery, Located, Native};
use crate::history::model::{Event, SourceInstance, SourceKind, Transcript};
use crate::resume::terminal::{LaunchSpec, RunOutput, run_to_completion};
use crate::resume::{ResumeKind, ResumePlan};
use head::{Scan, Thread};
use rayon::prelude::*;
use state_db::StateDb;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

/// The adapter of a Codex home.
pub struct CodexAdapter {
    instance: SourceInstance,
    /// Directory of the state database, when not where Codex would look.
    sqlite_home: Option<PathBuf>,
    /// Fork: `CODEX_SQLITE_HOME` of the [`HistoryEnv`] the adapter was built
    /// from.
    env_sqlite_home: Option<PathBuf>,
}

impl CodexAdapter {
    /// The adapter of the Codex home at `home`, with its state database
    /// where Codex would look for it (see [`Self::from_env`] for
    /// `CODEX_SQLITE_HOME`).
    pub fn new(home: PathBuf) -> Self {
        Self {
            instance: SourceInstance {
                kind: SourceKind::Codex,
                root: home,
            },
            sqlite_home: None,
            env_sqlite_home: None,
        }
    }

    pub fn with_sqlite_home(home: PathBuf, sqlite_home: PathBuf) -> Self {
        Self {
            sqlite_home: Some(sqlite_home),
            ..Self::new(home)
        }
    }

    /// Fork: the adapter of the Codex home `env` selects
    /// ([`HistoryEnv::codex_home`]), with `env`'s `CODEX_SQLITE_HOME`.
    /// `None` without a home directory or `CODEX_HOME`.
    pub fn from_env(env: &HistoryEnv) -> Option<Self> {
        Some(Self {
            env_sqlite_home: env.codex_sqlite_home(),
            ..Self::new(env.codex_home()?)
        })
    }

    /// The directory of the state database: the `sqlite_home` of the
    /// home's `config.toml`, else `CODEX_SQLITE_HOME`, else the home
    /// ([`state_db_dir`]).
    fn sqlite_home(&self) -> PathBuf {
        let home = &self.instance.root;
        self.sqlite_home
            .clone()
            .unwrap_or_else(|| state_db_dir(home, self.env_sqlite_home.as_deref()))
    }

    /// Every rollout of the home with the state that chooses among them,
    /// limited to the state database row of `only` when given. An error
    /// means the listing or the database could not be read now.
    fn scan(&self, only: Option<&str>) -> std::result::Result<Scan, AppError> {
        let rollouts = discover::list(&self.instance.root)?;
        let rows = match StateDb::open(&self.sqlite_home()).map_err(sqlite_error)? {
            None => HashMap::new(),
            Some(db) => match only {
                None => db.rows().map_err(sqlite_error)?,
                Some(id) => db
                    .row(id)
                    .map_err(sqlite_error)?
                    .map(|row| (id.to_owned(), row))
                    .into_iter()
                    .collect(),
            },
        };
        let names = discover::session_names(&self.instance.root);
        Ok(Scan::new(rollouts, rows, names))
    }

    fn located(&self, thread: Thread, scan_started: SystemTime) -> Located {
        let head = thread.head_link();
        Located {
            key: crate::history::model::ConversationKey {
                instance: self.instance.clone(),
                native_id: thread.id.clone(),
            },
            path: head.path.clone(),
            size: head.size,
            modified: head.modified,
            file_id: head.file_id,
            stamp: thread.stamp(scan_started),
            native: Some(Native(Arc::new(thread))),
        }
    }

    /// The thread `located` resolved, resolving it again when it carries
    /// none.
    fn thread_of(&self, located: &Located) -> Result<Arc<Thread>> {
        if let Some(thread) = located
            .native
            .as_ref()
            .and_then(|native| native.0.clone().downcast::<Thread>().ok())
        {
            return Ok(thread);
        }
        let located = self.resolve(&located.key.native_id, SystemTime::now())?;
        Ok(located
            .native
            .and_then(|native| native.0.downcast::<Thread>().ok())
            .expect("resolve carries the thread"))
    }
}

/// Fork (public): the directory of the state database of the Codex home
/// `home`, as the adapter opens it: the `sqlite_home` of the home's
/// `config.toml` ([`configured_sqlite_home`]), else `env_sqlite_home` (the
/// resolved `CODEX_SQLITE_HOME`, [`HistoryEnv::codex_sqlite_home`]), else
/// the home.
pub fn state_db_dir(home: &Path, env_sqlite_home: Option<&Path>) -> PathBuf {
    configured_sqlite_home(home)
        .or_else(|| env_sqlite_home.map(Path::to_path_buf))
        .unwrap_or_else(|| home.to_path_buf())
}

/// `sqlite_home` of the home's `config.toml` (a relative value resolves
/// against the home); `None` when the file is missing or unreadable, is not
/// TOML or names no `sqlite_home` string. Fork: public.
pub fn configured_sqlite_home(home: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(home.join("config.toml")).ok()?;
    let config: toml::Table = toml::from_str(&text).ok()?;
    let path = PathBuf::from(config.get("sqlite_home")?.as_str()?);
    Some(if path.is_absolute() {
        path
    } else {
        home.join(path)
    })
}

fn sqlite_error(error: rusqlite::Error) -> AppError {
    AppError::Io(std::io::Error::other(format!(
        "Codex state database unavailable: {error}"
    )))
}

/// The thread id of the rollout at `path`.
pub(crate) fn thread_id_of(path: &Path) -> Result<String> {
    discover::rollout_of(path, false)
        .map(|rollout| rollout.thread_id)
        .ok_or_else(|| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("not a Codex rollout: {}", path.display()),
            ))
        })
}

/// The environment of the Codex CLI acting on the home of the rollout at
/// `path`, else on `home`: `CODEX_HOME` (upstream's `codex_command`).
pub(crate) fn codex_env(path: &Path, home: &Path) -> Vec<(OsString, OsString)> {
    let home = discover::home_of(path).unwrap_or(home);
    if home.as_os_str().is_empty() {
        return Vec::new();
    }
    vec![(
        OsString::from("CODEX_HOME"),
        home.as_os_str().to_os_string(),
    )]
}

/// Fork: every rollout of the thread of the rollout at `path`, in the Codex
/// home holding it: what `codex delete` removes.
pub fn thread_rollouts(path: &Path) -> Result<Vec<PathBuf>> {
    let thread_id = thread_id_of(path)?;
    let home = discover::home_of(path).ok_or_else(|| {
        AppError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a rollout of a Codex home: {}", path.display()),
        ))
    })?;
    Ok(discover::list(home)?
        .into_iter()
        .filter(|rollout| rollout.thread_id == thread_id)
        .map(|rollout| rollout.path)
        .collect())
}

/// Fork: upstream's `codex delete --force <thread id>` for the thread of the
/// rollout at `path`, as a run of the CLI `launcher` starts (upstream
/// 8a99ccc: the `[resume.codex] command` with its leading arguments, a
/// `[gui] codex_path`, else `codex` on the env's `PATH`; checked first, an
/// [`AppError::AgentLaunch`] naming it when it is missing) with
/// `CODEX_HOME` set to the rollout's home, else `home`.
pub fn delete_spec(
    env: &HistoryEnv,
    path: &Path,
    home: &Path,
    launcher: &Launcher,
) -> Result<LaunchSpec> {
    let thread_id = thread_id_of(path)?;
    let program = launcher.resolve(env, SourceKind::Codex)?;
    Ok(LaunchSpec {
        program,
        args: launcher
            .args
            .iter()
            .map(OsString::from)
            .chain(
                ["delete", "--force", thread_id.as_str()]
                    .into_iter()
                    .map(OsString::from),
            )
            .collect(),
        cwd: None,
        env: codex_env(path, home),
    })
}

/// Fork: upstream's outcome of a `codex delete` run: its stderr when it
/// failed.
pub fn delete_outcome(output: &RunOutput) -> Result<()> {
    if output.success {
        return Ok(());
    }
    Err(AppError::Io(std::io::Error::other(format!(
        "codex delete failed ({}): {}",
        output.status,
        output.stderr.trim()
    ))))
}

impl Adapter for CodexAdapter {
    fn instance(&self) -> &SourceInstance {
        &self.instance
    }

    fn native_id_of(&self, path: &Path) -> Option<String> {
        discover::rollout_of(path, false).map(|rollout| rollout.thread_id)
    }

    fn discover(&self, scan_started: SystemTime) -> Discovery {
        let scan = match self.scan(None) {
            Ok(scan) => scan,
            Err(_) => {
                return Discovery {
                    found: Vec::new(),
                    completeness: Completeness::Degraded,
                };
            }
        };
        let found = scan
            .thread_ids()
            .into_par_iter()
            .filter_map(|id| scan.thread(id))
            .map(|thread| self.located(thread, scan_started))
            .collect();
        Discovery {
            found,
            completeness: Completeness::Complete,
        }
    }

    fn resolve(&self, native_id: &str, scan_started: SystemTime) -> Result<Located> {
        let thread = self
            .scan(Some(native_id))?
            .thread(native_id)
            .ok_or_else(|| {
                AppError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("no Codex rollout of thread {native_id}"),
                ))
            })?;
        Ok(self.located(thread, scan_started))
    }

    fn decode_into(
        &self,
        located: &Located,
        events: &mut dyn FnMut(Event),
    ) -> Result<Option<Transcript>> {
        let thread = self.thread_of(located)?;
        decode::decode(located.key.clone(), &thread, events)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            rename: false,
            ..Capabilities::ALL
        }
    }

    /// `codex [default_args] resume|fork <thread id>` in the conversation's
    /// project (fork: planned by `resume::plan_codex`, see the module docs).
    fn resume(
        &self,
        _env: &HistoryEnv,
        target: Target<'_>,
        workspace: Option<&WorkspaceScope>,
        fork: bool,
        launch: &Launch,
    ) -> Result<ResumePlan> {
        crate::resume::plan_codex(
            target,
            &self.instance.root,
            workspace,
            ResumeKind::from_fork(fork),
            launch,
        )
    }

    /// `codex delete --force <thread id>`, which removes every rollout of
    /// the thread and its state, run by the CLI `launcher` starts (checked
    /// first). Fork: run by [`run_to_completion`] (refused in tests and
    /// hermetic builds); returns the number of rollouts the thread had.
    fn delete(&self, env: &HistoryEnv, path: &Path, launcher: &Launcher) -> Result<usize> {
        let spec = delete_spec(env, path, &self.instance.root, launcher)?;
        let rollouts = thread_rollouts(path).map_or(1, |rollouts| rollouts.len());
        let output = run_to_completion(&spec).map_err(AppError::Io)?;
        delete_outcome(&output)?;
        Ok(rollouts)
    }

    fn rename(&self, _path: &Path, _title: &str) -> Result<()> {
        Err(AppError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Codex threads cannot be renamed from here",
        )))
    }
}

#[cfg(test)]
pub(crate) mod tests;
