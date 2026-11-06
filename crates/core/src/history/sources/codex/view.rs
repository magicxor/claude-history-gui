//! Codex rollouts opened by path, outside any configured instance.
//!
//! # Differences from upstream
//!
//! * **Streaming.** [`decode_path_into`] hands each event to a sink as it is
//!   decoded (the fork's list loaders index a file through
//!   `sources::decode_path_into`); [`decode_path`] collects them, as
//!   upstream's does.
//! * **`CODEX_SQLITE_HOME` from the env, for its own home.** Upstream decodes
//!   a rollout through [`CodexAdapter::new`] of the home holding it, whose
//!   state database falls back to the process's `CODEX_SQLITE_HOME`. The
//!   fork's adapter reads no process environment, so `decode_path_into`
//!   takes the [`HistoryEnv`] the file is opened in when its caller has one
//!   (`sources::decode_path_in`, `history::process_conversation_file_in`:
//!   the single-file browser, `Browser::new_single_file`). A rollout of that
//!   env's own Codex home ([`HistoryEnv::codex_home`]) then decodes through
//!   [`CodexAdapter::from_env`], with the env's `CODEX_SQLITE_HOME`, as the
//!   history service (the viewer, exports), the reconcile pass and the agent
//!   commands decode it. A rollout of another home, and any rollout decoded
//!   without an env ([`decode_path`], `sources::decode_path`), decodes
//!   through [`CodexAdapter::new`]: its state database is the `sqlite_home`
//!   of its home's `config.toml`, else that home.
//! * **The state database directory of a rollout, public.**
//!   [`state_db_dir_of`] names the directory decoding a rollout opened by
//!   path opens, so the GUI's QA sandbox gate checks it before a file is
//!   opened without resolving it again.

use super::{CodexAdapter, discover};
use crate::env::HistoryEnv;
use crate::error::{AppError, Result};
use crate::history::adapter::Adapter;
use crate::history::model::{Event, Transcript};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The thread of the rollout at `path`, decoded through the Codex home
/// holding it. `None` when `path` is not a rollout of a Codex home.
pub fn decode_path(path: &Path) -> Option<Result<Transcript>> {
    let mut decoded = Vec::new();
    let transcript = decode_path_into(None, path, &mut |event| decoded.push(event))?;
    Some(transcript.map(|transcript| Transcript {
        events: decoded,
        ..transcript
    }))
}

/// [`decode_path`], passing the events to `events` as they are decoded; the
/// returned transcript has none. A rollout of `env`'s own Codex home
/// decodes through the env's adapter (fork, see the module docs).
pub(crate) fn decode_path_into(
    env: Option<&HistoryEnv>,
    path: &Path,
    events: &mut dyn FnMut(Event),
) -> Option<Result<Transcript>> {
    let home = discover::home_of(path)?;
    let thread_id = discover::rollout_of(path, false)?.thread_id;
    let adapter = adapter_of(env, home);
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

/// Fork: the directory of the state database that decoding the rollout at
/// `path` with `env` ([`decode_path_into`]; `None`: [`decode_path`]) opens,
/// as [`super::state_db_dir`] resolves it for the adapter of the Codex home
/// holding the file. `None` when `path` is not a rollout of a Codex home
/// (it does not decode as one). Decoding also reads that home (its session
/// index, the listing of its rollouts).
pub fn state_db_dir_of(env: Option<&HistoryEnv>, path: &Path) -> Option<PathBuf> {
    let home = discover::home_of(path)?;
    discover::rollout_of(path, false)?;
    Some(adapter_of(env, home).sqlite_home())
}

/// The adapter of the Codex home `home`: `env`'s own, with its
/// `CODEX_SQLITE_HOME`, when `home` is `env`'s Codex home, else one without
/// (fork, see the module docs).
fn adapter_of(env: Option<&HistoryEnv>, home: &Path) -> CodexAdapter {
    env.and_then(CodexAdapter::from_env)
        .filter(|adapter| adapter.instance().root.as_path() == home)
        .unwrap_or_else(|| CodexAdapter::new(home.to_path_buf()))
}
