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
//! * **No actions yet.** Upstream plans `codex [args] resume|fork <id>` and
//!   runs `codex delete --force <id>` with `std::process::Command`. Core
//!   spawns no process outside `program` and `resume::terminal`, the fork's
//!   resume plans carry no environment variables (`CODEX_HOME`) yet, and a
//!   GUI deletes through its own services after a confirmation. Until that
//!   is ported (with upstream eea6d37, which wires these actions into the
//!   TUI), [`CodexAdapter`]'s capabilities are none and `resume` and
//!   `delete` refuse; rename is refused as upstream refuses it.

mod decode;
pub mod discover;
mod head;
mod state_db;

use crate::env::{HistoryEnv, WorkspaceScope};
use crate::error::{AppError, Result};
use crate::history::actions::{Capabilities, Target, refusal};
use crate::history::adapter::{Adapter, Completeness, Discovery, Located, Native};
use crate::history::model::{Event, SourceInstance, SourceKind, Transcript};
use crate::resume::ResumePlan;
use head::{Scan, Thread};
use rayon::prelude::*;
use state_db::StateDb;
use std::collections::HashMap;
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
    /// home's `config.toml`, else `CODEX_SQLITE_HOME`, else the home.
    fn sqlite_home(&self) -> PathBuf {
        let home = &self.instance.root;
        self.sqlite_home
            .clone()
            .or_else(|| configured_sqlite_home(home))
            .or_else(|| self.env_sqlite_home.clone())
            .unwrap_or_else(|| home.clone())
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

/// `sqlite_home` of the home's `config.toml`.
fn configured_sqlite_home(home: &Path) -> Option<PathBuf> {
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

    /// Fork: none until the Codex CLI's actions are ported (see the module
    /// docs); upstream supports everything but rename.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            resume: false,
            fork: false,
            delete: false,
            rename: false,
        }
    }

    /// Fork: refused until the Codex resume plan is ported (see the module
    /// docs).
    fn resume(
        &self,
        _env: &HistoryEnv,
        _target: Target<'_>,
        _workspace: Option<&WorkspaceScope>,
        fork: bool,
        _default_args: &[String],
        _program_override: Option<&Path>,
    ) -> Result<ResumePlan> {
        let doing = if fork { "Forking" } else { "Resuming" };
        Err(AppError::Unsupported(refusal(SourceKind::Codex, doing)))
    }

    /// Fork: refused until `codex delete` is ported (see the module docs).
    fn delete(&self, _env: &HistoryEnv, _path: &Path) -> Result<usize> {
        Err(AppError::Unsupported(refusal(
            SourceKind::Codex,
            "Deleting",
        )))
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
