//! An event log opened by its path rather than through a registry: a
//! single-file window, an export, or a session whose path is all a caller
//! has. Such a log decodes as the session of the directory it is in, with
//! the same key the adapter of that `session-state` directory gives it, and
//! reads only itself and the `workspace.yaml` beside it.

use super::{EVENTS_FILE, WORKSPACE_FILE, decode, session_of};
use crate::error::{AppError, Result};
use crate::history::model::{ConversationKey, Event, SourceInstance, SourceKind, Transcript};
use crate::history::sources::Polled;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// More than a first record takes; a longer first line is not looked at.
const MAX_FIRST_RECORD: u64 = 1 << 20;

/// The `workspace.yaml` that decoding the event log `file` also reads,
/// `None` when `file` is not named like an event log (fork, public: the
/// GUI's sandbox gate checks it for a file opened by path).
pub fn workspace_file_of(file: &Path) -> Option<PathBuf> {
    (file.file_name()? == EVENTS_FILE).then(|| file.with_file_name(WORKSPACE_FILE))
}

/// Decodes `path` as a Copilot event log, passing its events to `events`.
/// `None` when it is not one: a file of another name, or one whose first
/// record is not a Copilot event. `modified` dates a log without one
/// timestamp; `cancelled` stops the decode at its next read.
pub(crate) fn decode_path_into(
    path: &Path,
    modified: Option<SystemTime>,
    events: &mut dyn FnMut(Event),
    cancelled: &dyn Fn() -> bool,
) -> Option<Result<Transcript>> {
    workspace_file_of(path)?;
    if !starts_event_log(path) {
        return None;
    }
    Some(decode_log(path, modified, events, cancelled))
}

fn starts_event_log(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let mut reader = BufReader::new(file.take(MAX_FIRST_RECORD));
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return false,
            Ok(_) if line.trim_ascii().is_empty() => {}
            Ok(_) => return decode::starts_event_log(&line),
        }
    }
}

fn decode_log(
    path: &Path,
    modified: Option<SystemTime>,
    events: &mut dyn FnMut(Event),
    cancelled: &dyn Fn() -> bool,
) -> Result<Transcript> {
    let file = File::open(path)?;
    let modified = modified.or_else(|| file.metadata().ok()?.modified().ok());
    let reader = BufReader::new(Polled::new(file, cancelled));
    decode::decode(key_of(path), path, reader, modified, events)?.ok_or_else(|| {
        AppError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("not a Copilot event log: {}", path.display()),
        ))
    })
}

/// The key the adapter of the log's `session-state` directory gives it;
/// for a log outside a session directory, its directory stands in for the
/// session.
fn key_of(path: &Path) -> ConversationKey {
    let (root, native_id) = session_of(path).unwrap_or_else(|| {
        let dir = path.parent().unwrap_or(Path::new(""));
        let name = dir.file_name().unwrap_or(path.as_os_str());
        (
            dir.parent().unwrap_or(dir).to_path_buf(),
            name.to_string_lossy().into_owned(),
        )
    });
    ConversationKey {
        instance: SourceInstance {
            kind: SourceKind::Copilot,
            root,
        },
        native_id,
    }
}
