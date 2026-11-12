//! Listing the sessions of a Copilot `session-state` directory: the
//! directories named by a UUID that hold an event log. The CLI creates a
//! session's directory before its first event, so many have no log yet (or
//! never get one); other entries of the directory are not sessions.

use super::{EVENTS_FILE, WORKSPACE_FILE, is_uuid};
use crate::history::adapter::{Completeness, Discovery, Located, Stamp};
use crate::history::model::{ConversationKey, SourceInstance};
use rayon::prelude::*;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Every session of `instance`. A missing root holds none; a session
/// directory that cannot be read is reported as missed.
pub(super) fn discover(instance: &SourceInstance, scan_started: SystemTime) -> Discovery {
    let mut discovery = Discovery::default();
    let entries = match fs::read_dir(&instance.root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return discovery,
        Err(_) => {
            discovery.completeness = Completeness::Degraded;
            return discovery;
        }
    };
    let mut dirs = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) if entry.file_name().to_str().is_some_and(is_uuid) => dirs.push(entry.path()),
            Ok(_) => {}
            Err(_) => discovery.completeness = Completeness::Degraded,
        }
    }
    let located: Vec<(PathBuf, io::Result<Option<Located>>)> = dirs
        .into_par_iter()
        .map(|dir| {
            let located = locate(instance, &dir, scan_started);
            (dir, located)
        })
        .collect();
    for (dir, located) in located {
        match located {
            Ok(Some(located)) => discovery.found.push(located),
            Ok(None) => {}
            Err(_) => discovery.missed_dir(dir),
        }
    }
    discovery
}

/// The session whose directory is `dir`, `None` when it has no event log.
pub(super) fn locate(
    instance: &SourceInstance,
    dir: &Path,
    scan_started: SystemTime,
) -> io::Result<Option<Located>> {
    let path = dir.join(EVENTS_FILE);
    let metadata = match fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return Ok(None),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let size = metadata.len();
    let modified = metadata.modified().ok();
    // The titles are in `workspace.yaml`: a change there changes the
    // conversation too.
    let stamp = match fs::metadata(dir.join(WORKSPACE_FILE)) {
        Ok(workspace) => Stamp::of(
            size + workspace.len(),
            modified.max(workspace.modified().ok()),
            scan_started,
        ),
        Err(_) => Stamp::of(size, modified, scan_started),
    };
    let native_id = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(Some(Located {
        key: ConversationKey {
            instance: instance.clone(),
            native_id,
        },
        file_id: crate::history::sources::file_id(&metadata),
        path,
        size,
        modified,
        stamp,
        native: None,
    }))
}
