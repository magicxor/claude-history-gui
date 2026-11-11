//! An event log opened by its path rather than through a registry: a
//! single-file window, an export, or a session whose path is all a caller
//! has. A session's log (`events.jsonl`) decodes as the session of the
//! directory it is in, with the same key the adapter of that
//! `session-state` directory gives it, and reads only itself and the
//! `workspace.yaml` beside it. A copy under another name, such as a raw
//! JSONL export, is told by its content and reads only itself.

use super::decode::{self, First};
use super::{EVENTS_FILE, WORKSPACE_FILE, session_of};
use crate::error::{AppError, Result};
use crate::history::model::{ConversationKey, Event, SourceInstance, SourceKind, Transcript};
use crate::history::sources::Polled;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// More than a log's first record takes; what follows is not looked at.
const MAX_PROBED: u64 = 1 << 20;

/// The `workspace.yaml` that decoding the event log `file` also reads,
/// `None` when `file` is not named like a session's log and so reads none
/// (fork, public: the GUI's sandbox gate checks it for a file opened by
/// path).
pub fn workspace_file_of(file: &Path) -> Option<PathBuf> {
    (file.file_name()? == EVENTS_FILE).then(|| file.with_file_name(WORKSPACE_FILE))
}

/// Decodes `path` as a Copilot event log, passing its events to `events`.
/// `None` when it is not one. A file named like a session's log is one when
/// its first record is a Copilot event, as its adapter decodes it; a file
/// of another name only when it starts with a `session.start`, which is how
/// a copy of a whole log (the raw JSONL export) starts and other tools'
/// files do not. `modified` dates a log without one timestamp; `cancelled`
/// stops the decode at its next read.
pub(crate) fn decode_path_into(
    path: &Path,
    modified: Option<SystemTime>,
    events: &mut dyn FnMut(Event),
    cancelled: &dyn Fn() -> bool,
) -> Option<Result<Transcript>> {
    match first_record(path)? {
        First::Start => {}
        First::Event if workspace_file_of(path).is_some() => {}
        First::Event | First::Other => return None,
    }
    Some(decode_log(path, modified, events, cancelled))
}

/// The first record of `path`, by the decoder's own rule (lines that are no
/// record are passed over), so a log decodes the same by path as through
/// its adapter.
fn first_record(path: &Path) -> Option<First> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file.take(MAX_PROBED));
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).ok()? == 0 {
            return None;
        }
        if let Some(first) = decode::first_record(&line) {
            return Some(first);
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

/// The key the adapter of the log's `session-state` directory gives it.
/// Outside a session directory, the directory of a file named like a
/// session's log stands in for the session, and a copy under another name
/// is keyed by that name, like any loose file.
fn key_of(path: &Path) -> ConversationKey {
    let (root, native_id) = session_of(path).unwrap_or_else(|| {
        let dir = path.parent().unwrap_or(Path::new(""));
        let (root, name) = match (workspace_file_of(path), path.file_name()) {
            (None, Some(name)) => (dir, name),
            _ => (
                dir.parent().unwrap_or(dir),
                dir.file_name().unwrap_or(path.as_os_str()),
            ),
        };
        (root.to_path_buf(), name.to_string_lossy().into_owned())
    });
    ConversationKey {
        instance: SourceInstance {
            kind: SourceKind::Copilot,
            root,
        },
        native_id,
    }
}
