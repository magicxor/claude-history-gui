//! Pi and OMP: one JSONL session file per conversation, in per-project
//! directories of a session root or directly in a flat one.

use crate::env::{HistoryEnv, WorkspaceScope};
use crate::error::Result;
use crate::history::ParseError;
use crate::history::actions::{Capabilities, Target};
use crate::history::adapter::{Adapter, Completeness, Discovery, Located};
use crate::history::model::{
    ConversationKey, Event, ExecutionMode, HeadStatus, Origin, Relationship, Revision, Segment,
    SessionMeta, SourceInstance, SourceKind, Transcript,
};
use crate::history::omp_loader::{self, OmpSessionRoot};
use crate::history::pi::PiProjection;
use crate::history::pi_loader::{self, PiSessionRoot};
use crate::resume::{ResumeKind, ResumePlan};
use chrono::{DateTime, Utc};
use std::path::Path;
use std::time::SystemTime;

/// The adapter of a Pi or an OMP session root.
pub struct PiAdapter {
    instance: SourceInstance,
    flat: bool,
}

impl PiAdapter {
    pub fn pi(root: PiSessionRoot) -> Self {
        Self::new(SourceKind::Pi, root.path, root.flat)
    }

    pub fn omp(root: OmpSessionRoot) -> Self {
        Self::new(SourceKind::Omp, root.path, root.flat)
    }

    fn new(kind: SourceKind, root: std::path::PathBuf, flat: bool) -> Self {
        Self {
            instance: SourceInstance { kind, root },
            flat,
        }
    }
}

impl Adapter for PiAdapter {
    fn instance(&self) -> &SourceInstance {
        &self.instance
    }

    fn discover(&self, scan_started: SystemTime) -> Discovery {
        let root = self.instance.root.clone();
        let files = match self.instance.kind {
            SourceKind::Omp => omp_loader::discover_files(&OmpSessionRoot {
                path: root,
                flat: self.flat,
            }),
            _ => pi_loader::discover_files(&PiSessionRoot {
                path: root,
                flat: self.flat,
            }),
        };
        match files {
            Ok(files) => Discovery {
                found: super::locate_files(&self.instance, files, scan_started),
                completeness: Completeness::Complete,
            },
            Err(_) => Discovery {
                found: Vec::new(),
                completeness: Completeness::Degraded,
            },
        }
    }

    fn resolve(&self, native_id: &str, scan_started: SystemTime) -> Result<Located> {
        super::resolve_file(&self.instance, native_id, scan_started)
    }

    fn decode_into(
        &self,
        located: &Located,
        events: &mut dyn FnMut(Event),
    ) -> Result<Option<Transcript>> {
        let omp_owned = self.instance.kind == SourceKind::Omp && omp_owned(&self.instance.root);
        super::decode_file(located, omp_owned, events)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::ALL
    }

    /// Pi resumes a session file in place (`pi --session`, `omp --resume`)
    /// or forks it into a new one (`--fork`). Planned by
    /// `resume::plan_native`, which runs a fork in the workspace (upstream's
    /// cwd).
    fn resume(
        &self,
        _env: &HistoryEnv,
        target: Target<'_>,
        workspace: Option<&WorkspaceScope>,
        fork: bool,
        _default_args: &[String],
        program_override: Option<&Path>,
    ) -> Result<ResumePlan> {
        Ok(crate::resume::plan_native(
            self.instance.kind,
            target,
            workspace,
            ResumeKind::from_fork(fork),
            program_override,
        ))
    }

    fn delete(&self, _env: &HistoryEnv, path: &Path) -> Result<usize> {
        match self.instance.kind {
            SourceKind::Omp => omp_loader::delete_session(path),
            _ => pi_loader::delete_session(path),
        }
        .map(|()| 1)
    }

    fn rename(&self, path: &Path, title: &str) -> Result<()> {
        match self.instance.kind {
            SourceKind::Omp => crate::history::pi::append_omp_session_rename(path, title),
            _ => crate::history::pi::append_session_rename(path, title),
        }
    }
}

/// Whether OMP sessions under `root` are OMP's own format rather than Pi's.
pub(crate) fn omp_owned(root: &Path) -> bool {
    root.components()
        .any(|component| matches!(component.as_os_str().to_str(), Some(".omp" | "omp")))
}

/// The transcript of a Pi or OMP session: the records of its active branch,
/// projected into Claude's format. An event's origin is its position in the
/// projected branch.
pub(super) fn transcript_of(
    key: ConversationKey,
    path: &Path,
    projection: PiProjection,
    bytes: u64,
    modified: Option<SystemTime>,
    events: &mut dyn FnMut(Event),
) -> Transcript {
    let source = projection.source;
    let mut records = super::claude::Records::default();
    for (position, (_, entry)) in projection.entries.into_iter().enumerate() {
        let line = u32::try_from(position + 1).unwrap_or(u32::MAX);
        events(records.push(entry, Origin { segment: 0, line }));
    }
    let updated_at = records
        .latest_activity
        .or_else(|| super::claude::parse_timestamp(&projection.header.timestamp))
        .or_else(|| modified.map(DateTime::<Utc>::from));
    let diagnostics = projection
        .malformed_lines
        .into_iter()
        .map(|line_number| ParseError {
            line_number,
            line_content: String::new(),
            error_message: format!("malformed {} JSONL record", source.label()),
            context_before: Vec::new(),
            context_after: Vec::new(),
        })
        .collect();
    let segments = vec![Segment {
        artifact: path.to_path_buf(),
        start: 0,
        end: bytes,
        inherited: false,
    }];
    let cwd = projection.header.cwd;
    Transcript {
        revision: Revision::of(source, &segments, &[super::claude::mtime_ns(modified)]),
        key,
        source,
        head: HeadStatus::Current,
        mode: ExecutionMode::Unknown,
        relationship: Relationship::Root,
        meta: SessionMeta {
            session_id: projection.header.id,
            cwd: Some(cwd.clone()),
            project: Some(cwd),
            model: records.model,
            summary: records.summary,
            custom_title: records.custom_title,
            updated_at,
        },
        segments,
        // The session header is a record too.
        records: records.count + 1,
        events: Vec::new(),
        diagnostics,
    }
}
