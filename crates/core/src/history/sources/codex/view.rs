//! Codex rollouts opened by path, outside any configured instance.
//!
//! # Differences from upstream
//!
//! * **Streaming.** [`decode_path_into`] hands each event to a sink as it is
//!   decoded (the fork's list loaders index a file through
//!   `sources::decode_path_into`); [`decode_path`] collects them, as
//!   upstream's does.
//! * **No `CODEX_SQLITE_HOME` here.** A rollout is decoded through
//!   [`CodexAdapter::new`] of the home holding it, as upstream, but the
//!   fork's adapter reads no process environment: its state database is the
//!   `sqlite_home` of the home's `config.toml`, else the home. A rollout of
//!   the env's own Codex home never comes here: the history service opens
//!   it through the env's adapter (`CodexAdapter::from_env`), as the
//!   reconcile pass and the agent commands decode it.

use super::{CodexAdapter, discover};
use crate::error::{AppError, Result};
use crate::history::adapter::Adapter;
use crate::history::model::{Event, Transcript};
use std::path::Path;
use std::time::SystemTime;

/// The thread of the rollout at `path`, decoded through the Codex home
/// holding it. `None` when `path` is not a rollout of a Codex home.
pub fn decode_path(path: &Path) -> Option<Result<Transcript>> {
    let mut decoded = Vec::new();
    let transcript = decode_path_into(path, &mut |event| decoded.push(event))?;
    Some(transcript.map(|transcript| Transcript {
        events: decoded,
        ..transcript
    }))
}

/// [`decode_path`], passing the events to `events` as they are decoded; the
/// returned transcript has none (fork, see the module docs).
pub(crate) fn decode_path_into(
    path: &Path,
    events: &mut dyn FnMut(Event),
) -> Option<Result<Transcript>> {
    let home = discover::home_of(path)?;
    let thread_id = discover::rollout_of(path, false)?.thread_id;
    let adapter = CodexAdapter::new(home.to_path_buf());
    Some(
        adapter
            .resolve(&thread_id, SystemTime::now())
            .and_then(|located| adapter.decode_into(&located, events))
            .and_then(|transcript| {
                transcript.ok_or_else(|| {
                    AppError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("not a Codex rollout: {}", path.display()),
                    ))
                })
            }),
    )
}
