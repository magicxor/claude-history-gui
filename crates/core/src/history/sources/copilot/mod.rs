//! GitHub Copilot CLI: sessions kept as one directory each under
//! `<COPILOT_HOME>/session-state/<uuid>/`, with an append-only event log
//! (`events.jsonl`) and a `workspace.yaml` of session metadata.
//!
//! A session's native id is its directory name, the session id the CLI's
//! `--resume=<id>` takes. The instance root is the `session-state`
//! directory.
//!
//! # Differences from upstream
//!
//! * **The whole source is fork-only.** Upstream has no Copilot support.
//! * **Resume only.** The CLI has no fork flag and no delete command, and
//!   the fork leaves renaming out, so [`Adapter::capabilities`] allows only
//!   resuming (`resume::plan_copilot`, which sets `COPILOT_HOME` in the
//!   plan's `env`).

mod decode;
mod discover;
pub mod view;
mod workspace;

use crate::env::{HistoryEnv, WorkspaceScope};
use crate::error::{AppError, Result};
use crate::history::actions::{Capabilities, Launch, Launcher, Target};
use crate::history::adapter::{Adapter, Discovery, Located, cancelled_error};
use crate::history::model::{Event, SourceInstance, SourceKind, Transcript};
use crate::history::sources::Polled;
use crate::resume::{ResumeKind, ResumePlan};
use std::fs::File;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Directory of a Copilot home that holds the sessions.
pub const SESSION_STATE_DIR: &str = "session-state";
/// A session's event log, inside its directory.
pub const EVENTS_FILE: &str = "events.jsonl";
/// A session's metadata, beside its event log.
pub const WORKSPACE_FILE: &str = "workspace.yaml";

/// Fork: the adapter of a Copilot `session-state` directory.
pub struct CopilotAdapter {
    instance: SourceInstance,
}

impl CopilotAdapter {
    /// The adapter of the `session-state` directory `root`.
    pub fn new(root: PathBuf) -> Self {
        Self {
            instance: SourceInstance {
                kind: SourceKind::Copilot,
                root,
            },
        }
    }

    /// The adapter of the Copilot home `env` selects
    /// ([`HistoryEnv::copilot_home`]). `None` without a home directory or
    /// `COPILOT_HOME`.
    pub fn from_env(env: &HistoryEnv) -> Option<Self> {
        Some(Self::new(env.copilot_home()?.join(SESSION_STATE_DIR)))
    }
}

/// The session root (as spelled) and the session id of an `events.jsonl`
/// whose directory is named by a UUID, else `None`.
pub fn session_of(path: &Path) -> Option<(PathBuf, String)> {
    if path.file_name()? != EVENTS_FILE {
        return None;
    }
    let dir = path.parent()?;
    let id = dir.file_name()?.to_str()?;
    if !is_uuid(id) {
        return None;
    }
    Some((dir.parent()?.to_path_buf(), id.to_owned()))
}

/// `8-4-4-4-12` hexadecimal digits, in either case.
fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

/// Decodes the log of `located`, giving up at its next read once
/// `cancelled()` returns true.
fn decode_located(
    located: &Located,
    events: &mut dyn FnMut(Event),
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<Transcript>> {
    let reader = BufReader::new(Polled::new(File::open(&located.path)?, cancelled));
    decode::decode(
        located.key.clone(),
        &located.path,
        reader,
        located.modified,
        events,
    )
}

impl Adapter for CopilotAdapter {
    fn instance(&self) -> &SourceInstance {
        &self.instance
    }

    fn native_id_of(&self, path: &Path) -> Option<String> {
        let (root, id) = session_of(path)?;
        (root == self.instance.root).then_some(id)
    }

    fn discover(&self, scan_started: SystemTime) -> Discovery {
        discover::discover(&self.instance, scan_started)
    }

    fn resolve(&self, native_id: &str, scan_started: SystemTime) -> Result<Located> {
        let located = if is_uuid(native_id) {
            discover::locate(
                &self.instance,
                &self.instance.root.join(native_id),
                scan_started,
            )?
        } else {
            None
        };
        located.ok_or_else(|| {
            AppError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no Copilot session {native_id}"),
            ))
        })
    }

    fn decode_into(
        &self,
        located: &Located,
        events: &mut dyn FnMut(Event),
    ) -> Result<Option<Transcript>> {
        decode_located(located, events, &|| false)
    }

    fn decode_cancellable(
        &self,
        located: &Located,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<Transcript>> {
        if cancelled() {
            return Err(cancelled_error());
        }
        let mut decoded = Vec::new();
        let transcript = decode_located(located, &mut |event| decoded.push(event), cancelled)?;
        Ok(transcript.map(|transcript| Transcript {
            events: decoded,
            ..transcript
        }))
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            resume: true,
            fork: false,
            delete: false,
            rename: false,
        }
    }

    /// `copilot [default_args] --resume=<session id>` in the session's
    /// project (planned by `resume::plan_copilot`).
    fn resume(
        &self,
        _env: &HistoryEnv,
        target: Target<'_>,
        workspace: Option<&WorkspaceScope>,
        fork: bool,
        launch: &Launch,
    ) -> Result<ResumePlan> {
        crate::resume::plan_copilot(target, workspace, ResumeKind::from_fork(fork), launch)
    }

    fn delete(&self, _env: &HistoryEnv, _path: &Path, _launcher: &Launcher) -> Result<usize> {
        Err(AppError::Io(io::Error::new(
            io::ErrorKind::Unsupported,
            "Copilot sessions cannot be deleted from here",
        )))
    }

    fn rename(&self, _path: &Path, _title: &str) -> Result<()> {
        Err(AppError::Io(io::Error::new(
            io::ErrorKind::Unsupported,
            "Copilot sessions cannot be renamed from here",
        )))
    }
}

#[cfg(test)]
pub(crate) mod tests;
