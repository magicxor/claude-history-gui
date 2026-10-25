//! Claude Code: one JSONL file per session in each project directory of a
//! `projects` root, with the transcripts of the session's subagents and
//! `/btw` forks beside it as sidecars:
//!
//! ```text
//! <project>/<session id>.jsonl
//! <project>/<session id>/subagents/agent-<agent id>.jsonl
//! <project>/<session id>/subagents/agent-<agent id>.meta.json
//! ```
//!
//! Records are Claude's `LogEntry` wire format, which only this module
//! decodes into events.
//!
//! # Differences from upstream
//!
//! * **Actions.** Resume plans through `resume::plan_claude` with the
//!   env and workspace (upstream's `resume_plan` and its process cwd are not
//!   ported); it resumes a sidecar's session as upstream does. `delete`
//!   returns the number of transcripts removed (1 for a sidecar).
//! * **[`meta_path`] and [`kept_in_session_dir`]** are visible to the crate
//!   so `maintenance::plan_session_delete` can list a sidecar's metadata file
//!   and the `/btw` forks a session delete keeps before the GUI confirms it;
//!   [`session_sidecars`] lists a session's sidecars for the fork's
//!   in-memory loader.

use crate::claude::{AgentContent, ContentBlock, LogEntry, TokenUsage, UserContent};
use crate::env::{HistoryEnv, WorkspaceScope};
use crate::error::Result;
use crate::history::ParseError;
use crate::history::Provenance;
use crate::history::actions::{Capabilities, Launch, Launcher, Target};
use crate::history::adapter::{Adapter, Completeness, Discovery, Located, Stamp};
use crate::history::model::{
    ConversationKey, Event, EventKind, ExecutionMode, HeadStatus, Message, Origin, Part,
    Relationship, Revision, Role, Segment, SessionMeta, SourceInstance, SourceKind, TitleKind,
    Transcript, Usage, Visibility, push_cwd,
};
use crate::history::path::decode_project_dir_name_to_path;
use crate::resume::{ResumeKind, ResumePlan};
use chrono::{DateTime, Utc};
use rayon::prelude::*;
use std::collections::VecDeque;
use std::fs;
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub struct ClaudeAdapter {
    instance: SourceInstance,
}

impl ClaudeAdapter {
    /// The adapter of a `projects` directory.
    pub fn new(root: PathBuf) -> Self {
        Self {
            instance: SourceInstance {
                kind: SourceKind::Claude,
                root,
            },
        }
    }
}

impl Adapter for ClaudeAdapter {
    fn instance(&self) -> &SourceInstance {
        &self.instance
    }

    fn discover(&self, scan_started: SystemTime) -> Discovery {
        let mut discovery = Discovery::default();
        let files = list_sessions(&self.instance.root, &mut discovery);
        discovery.found = super::locate_files(&self.instance, files, scan_started)
            .into_par_iter()
            .map(|located| with_meta_stamp(located, scan_started))
            .collect();
        discovery
    }

    fn resolve(&self, native_id: &str, scan_started: SystemTime) -> Result<Located> {
        super::resolve_file(&self.instance, native_id, scan_started)
            .map(|located| with_meta_stamp(located, scan_started))
    }

    fn decode_into(
        &self,
        located: &Located,
        events: &mut dyn FnMut(Event),
    ) -> Result<Option<Transcript>> {
        super::decode_file(located, false, events)
    }

    /// Fork: checked at every read of the session file.
    fn decode_cancellable(
        &self,
        located: &Located,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<Transcript>> {
        super::decode_file_collected(located, false, cancelled)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::ALL
    }

    /// Claude Code opens a sidecar only from within its session: resuming
    /// one resumes that session, and it cannot be forked or renamed.
    fn capabilities_for(&self, provenance: Provenance) -> Capabilities {
        if provenance.child || provenance.branch {
            Capabilities {
                resume: true,
                fork: false,
                delete: true,
                rename: false,
            }
        } else {
            Capabilities::ALL
        }
    }

    /// Claude resumes a session only from the project directory that holds
    /// it: run there, else copy the session into the workspace's project
    /// directory first. Planned by `resume::plan_claude`, with the workspace
    /// in place of upstream's process cwd (its rules are in the `resume`
    /// module docs). A sidecar resumes the session that holds it.
    fn resume(
        &self,
        env: &HistoryEnv,
        target: Target<'_>,
        workspace: Option<&WorkspaceScope>,
        fork: bool,
        launch: &Launch,
    ) -> Result<ResumePlan> {
        crate::resume::plan_claude(env, target, workspace, ResumeKind::from_fork(fork), launch)
    }

    /// Deletes every transcript of a session, in any project, with its
    /// session directory, except the `/btw` forks there, which are
    /// conversations of their own. A sidecar is deleted alone, with its
    /// metadata file.
    fn delete(&self, env: &HistoryEnv, path: &Path, _launcher: &Launcher) -> Result<usize> {
        if sidecar(path).is_some() {
            return delete_sidecar(path).map(|()| 1);
        }
        let uuid = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        crate::history::delete_session_by_uuid(env, uuid)
    }

    fn rename(&self, path: &Path, title: &str) -> Result<()> {
        crate::history::append_session_rename(path, title)
    }
}

/// Session files: `*.jsonl` directly inside each project directory, except
/// the subagent transcripts older versions kept there (`agent-*`), and the
/// sidecars in each session directory. A missing root holds none.
fn list_sessions(root: &Path, discovery: &mut Discovery) -> Vec<PathBuf> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(_) => {
            discovery.completeness = Completeness::Degraded;
            return Vec::new();
        }
    };
    let mut dirs = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => dirs.push(entry.path()),
            Err(_) => discovery.completeness = Completeness::Degraded,
        }
    }
    let listed: Vec<(PathBuf, io::Result<Listed>)> = dirs
        .into_par_iter()
        .filter(|dir| dir.is_dir())
        .map(|dir| {
            let files = list_project_dir(&dir);
            (dir, files)
        })
        .collect();
    let mut files = Vec::new();
    for (dir, listed) in listed {
        match listed {
            Ok(listed) => {
                files.extend(listed.files);
                for dir in listed.missed {
                    discovery.missed_dir(dir);
                }
            }
            Err(_) => discovery.missed_dir(dir),
        }
    }
    files
}

/// The session files of one project directory, and the sidecar directories
/// in it that could not be listed.
#[derive(Default)]
struct Listed {
    files: Vec<PathBuf>,
    missed: Vec<PathBuf>,
}

fn list_project_dir(dir: &Path) -> io::Result<Listed> {
    let mut listed = Listed::default();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if is_session_file(&path) {
            listed.files.push(path);
        } else if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            let sidecars = path.join(SIDECAR_DIR);
            match fs::read_dir(&sidecars) {
                Ok(entries) => {
                    for entry in entries {
                        match entry {
                            Ok(entry) if is_sidecar_file(&entry.path()) => {
                                listed.files.push(entry.path());
                            }
                            Ok(_) => {}
                            Err(_) => listed.missed.push(sidecars.clone()),
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) if error.kind() == io::ErrorKind::NotADirectory => {}
                Err(_) => listed.missed.push(sidecars),
            }
        }
    }
    Ok(listed)
}

fn is_session_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "jsonl")
        && !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(SIDECAR_PREFIX))
}

/// Fork: the sidecar transcripts in the session directory `dir`, as
/// discovery lists them (for the in-memory loader the GUI's tests use,
/// `history::loader::load_conversations`).
pub(crate) fn session_sidecars(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir.join(SIDECAR_DIR))
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| is_sidecar_file(path))
                .collect()
        })
        .unwrap_or_default()
}

/// Directory of a session directory holding its sidecars.
const SIDECAR_DIR: &str = "subagents";
const SIDECAR_PREFIX: &str = "agent-";

/// Agents Claude Code runs for itself rather than for the conversation:
/// prompt suggestions and compaction.
const INTERNAL_AGENTS: &[&str] = &["aprompt_suggestion-", "acompact-"];

fn is_sidecar_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "jsonl")
        && path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.strip_prefix(SIDECAR_PREFIX))
            .is_some_and(|agent| {
                !agent.is_empty() && !INTERNAL_AGENTS.iter().any(|kind| agent.starts_with(kind))
            })
}

/// Where a sidecar transcript sits: `<project>/<session id>/subagents/
/// agent-<agent id>.jsonl`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Sidecar<'a> {
    /// The project directory holding the session.
    pub project_dir: &'a Path,
    /// Id of the session holding the sidecar.
    pub session_id: &'a str,
    /// Claude Code's id of the agent that wrote it.
    pub agent_id: &'a str,
}

/// Where `path` sits, when it is a sidecar transcript.
pub fn sidecar(path: &Path) -> Option<Sidecar<'_>> {
    if !is_sidecar_file(path) {
        return None;
    }
    let agent_id = path.file_stem()?.to_str()?.strip_prefix(SIDECAR_PREFIX)?;
    let sidecars = path.parent()?;
    if sidecars.file_name()? != SIDECAR_DIR {
        return None;
    }
    let session_dir = sidecars.parent()?;
    Some(Sidecar {
        project_dir: session_dir.parent()?,
        session_id: session_dir.file_name()?.to_str()?,
        agent_id,
    })
}

/// Transcript of the session holding the sidecar at `path`.
pub fn parent_transcript(path: &Path) -> Option<PathBuf> {
    let sidecar = sidecar(path)?;
    Some(
        sidecar
            .project_dir
            .join(format!("{}.jsonl", sidecar.session_id)),
    )
}

/// The project directory holding the session file or sidecar at `path`.
pub fn project_dir(path: &Path) -> Option<&Path> {
    match sidecar(path) {
        Some(sidecar) => Some(sidecar.project_dir),
        None => path.parent(),
    }
}

/// The metadata file beside the sidecar transcript at `path`.
pub(crate) fn meta_path(path: &Path) -> PathBuf {
    path.with_extension("meta.json")
}

/// What Claude Code records about a sidecar when it starts the agent.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SidecarMeta {
    agent_type: Option<String>,
    description: Option<String>,
    /// The Agent tool call that started the agent.
    tool_use_id: Option<String>,
    #[serde(default)]
    is_fork: bool,
}

impl SidecarMeta {
    fn read(path: &Path) -> Self {
        fs::read(meta_path(path))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Whether the person forked the conversation off (`/btw`, then `f`).
    /// The Agent tool's forks carry the call that started them.
    fn is_user_fork(&self) -> bool {
        self.is_fork && self.tool_use_id.is_none()
    }

    /// What the sidecar is listed as, when its own records name nothing.
    fn summary(&self) -> Option<String> {
        let description = self.description.as_deref().map(str::trim);
        let description = description.filter(|text| !text.is_empty());
        if self.is_user_fork() {
            return description.map(str::to_owned);
        }
        match (self.agent_type.as_deref(), description) {
            (Some(kind), Some(text)) => Some(format!("{kind}: {text}")),
            (Some(kind), None) => Some(kind.to_owned()),
            (None, text) => text.map(str::to_owned),
        }
    }
}

/// `located` with a sidecar's metadata file folded into its stamp, so a
/// changed metadata file reads as a changed conversation.
fn with_meta_stamp(mut located: Located, scan_started: SystemTime) -> Located {
    if sidecar(&located.path).is_none() {
        return located;
    }
    let Ok(meta) = fs::metadata(meta_path(&located.path)) else {
        return located;
    };
    let modified = located.modified.max(meta.modified().ok());
    located.stamp = Stamp::of(located.size + meta.len(), modified, scan_started);
    located
}

/// Removes the sidecar transcript at `path` and its metadata file.
fn delete_sidecar(path: &Path) -> Result<()> {
    fs::remove_file(path)?;
    match fs::remove_file(meta_path(path)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

/// Fork: the files [`remove_session_dir`] keeps in the session directory
/// `dir`, each `/btw` fork's transcript and existing metadata file, sorted.
pub(crate) fn kept_in_session_dir(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir.join(SIDECAR_DIR)) else {
        return Vec::new();
    };
    let mut kept: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| is_sidecar_file(path) && SidecarMeta::read(path).is_user_fork())
        .flat_map(|fork| {
            let meta = meta_path(&fork);
            [Some(fork), meta.is_file().then_some(meta)]
        })
        .flatten()
        .collect();
    kept.sort();
    kept
}

/// Removes the session directory `dir`, except the `/btw` forks among its
/// sidecars, which stay with their metadata.
pub(crate) fn remove_session_dir(dir: &Path) -> io::Result<()> {
    let sidecars = dir.join(SIDECAR_DIR);
    let forks: Vec<PathBuf> = match fs::read_dir(&sidecars) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| is_sidecar_file(path) && SidecarMeta::read(path).is_user_fork())
            .collect(),
        Err(_) => Vec::new(),
    };
    if forks.is_empty() {
        return fs::remove_dir_all(dir);
    }
    let kept: Vec<PathBuf> = forks
        .iter()
        .flat_map(|fork| [fork.clone(), meta_path(fork)])
        .collect();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path == sidecars {
            continue;
        }
        if path.is_dir() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
    }
    for entry in fs::read_dir(&sidecars)? {
        let path = entry?.path();
        if kept.contains(&path) {
            continue;
        }
        if path.is_dir() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// Decodes a Claude session from `reader`, the contents of `path`, passing
/// its events to `events`. The session was last active at its latest
/// message, else when it was `modified`.
pub(crate) fn decode_reader(
    key: ConversationKey,
    path: &Path,
    reader: impl BufRead,
    modified: Option<SystemTime>,
    events: &mut dyn FnMut(Event),
) -> Result<Transcript> {
    let mut lines = Lines::new(reader);
    // The current line, plus up to 2 lines after it for parse error context.
    let mut window: VecDeque<String> = VecDeque::with_capacity(3);
    let mut context_before: VecDeque<String> = VecDeque::with_capacity(2);
    for _ in 0..2 {
        match lines.next_line()? {
            Some(line) => window.push_back(line),
            None => break,
        }
    }
    let mut records = Records::default();
    let mut activity = super::LastActivity::default();
    let mut diagnostics = Vec::new();
    let mut line_index: usize = 0;
    while let Some(line) = window.pop_front() {
        if let Some(next) = lines.next_line()? {
            window.push_back(next);
        }
        if !line.trim().is_empty() {
            match serde_json::from_str::<LogEntry>(&line) {
                Ok(entry) => {
                    if let Some(event) = records.push(entry, line_number(line_index)) {
                        activity.observe(&event);
                        events(event);
                    }
                }
                Err(error) => diagnostics.push(ParseError {
                    line_number: line_index + 1,
                    line_content: line.clone(),
                    error_message: error.to_string(),
                    context_before: context_before.iter().cloned().collect(),
                    context_after: window.iter().cloned().collect(),
                }),
            }
        }
        context_before.push_back(line);
        if context_before.len() > 2 {
            context_before.pop_front();
        }
        line_index += 1;
    }

    let segments = vec![Segment {
        artifact: path.to_path_buf(),
        start: 0,
        end: lines.bytes,
        inherited: false,
    }];
    let cwd = records.cwds.first().cloned();
    let project = cwd.clone().unwrap_or_else(|| {
        project_dir(path)
            .and_then(Path::file_name)
            .map(|dir| decode_project_dir_name_to_path(&dir.to_string_lossy()))
            .unwrap_or_default()
    });
    let (session_id, relationship, summary) = match sidecar(path) {
        Some(sidecar) => {
            let meta = SidecarMeta::read(path);
            let parent = sidecar.session_id.to_owned();
            let relationship = if meta.is_user_fork() {
                Relationship::Branch { parent }
            } else {
                Relationship::Child { parent }
            };
            let summary = records.summary.take().or_else(|| meta.summary());
            (sidecar.agent_id.to_owned(), relationship, summary)
        }
        None => (
            path.file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_owned(),
            Relationship::Root,
            records.summary.take(),
        ),
    };
    let meta = SessionMeta {
        session_id,
        cwd,
        cwds: std::mem::take(&mut records.cwds),
        project: Some(project),
        model: records.model.take(),
        summary,
        custom_title: records.custom_title.take(),
        updated_at: activity
            .at()
            .or_else(|| modified.map(DateTime::<Utc>::from)),
    };
    Ok(Transcript {
        revision: Revision::of(SourceKind::Claude, &segments, &[mtime_ns(modified)]),
        key,
        source: SourceKind::Claude,
        head: HeadStatus::Current,
        mode: ExecutionMode::Unknown,
        relationship,
        meta,
        segments,
        records: records.count,
        events: Vec::new(),
        diagnostics,
    })
}

/// Decodes `reader` as the Claude session at `path`, outside any root.
#[cfg(test)]
pub(crate) fn decode_test_reader(path: &Path, reader: impl BufRead) -> Result<Transcript> {
    let instance = SourceInstance {
        kind: SourceKind::Claude,
        root: PathBuf::new(),
    };
    let key = ConversationKey {
        instance,
        native_id: path.to_string_lossy().into_owned(),
    };
    let mut decoded = Vec::new();
    let mut transcript = decode_reader(key, path, reader, None, &mut |event| decoded.push(event))?;
    transcript.events = decoded;
    Ok(transcript)
}

fn line_number(index: usize) -> Origin {
    Origin {
        segment: 0,
        line: u32::try_from(index + 1).unwrap_or(u32::MAX),
    }
}

pub(super) fn mtime_ns(modified: Option<SystemTime>) -> i64 {
    modified
        .and_then(|modified| modified.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |since| i64::try_from(since.as_nanos()).unwrap_or(0))
}

/// Lines of a reader as `BufRead::lines` splits them, counting the bytes
/// read.
struct Lines<R> {
    reader: R,
    bytes: u64,
}

impl<R: BufRead> Lines<R> {
    fn new(reader: R) -> Self {
        Self { reader, bytes: 0 }
    }

    fn next_line(&mut self) -> io::Result<Option<String>> {
        let mut line = String::new();
        let read = self.reader.read_line(&mut line)?;
        if read == 0 {
            return Ok(None);
        }
        self.bytes += read as u64;
        if line.ends_with('\n') {
            line.pop();
            if line.ends_with('\r') {
                line.pop();
            }
        }
        Ok(Some(line))
    }
}

/// Converts Claude-format records into events, in order, collecting the
/// session metadata they define: the distinct cwds of user records, the first
/// assistant model, the first summary unless a generated title replaces
/// it, and the last custom title. A file-history snapshot, which records
/// file states for undo rather than the conversation, yields no event.
#[derive(Default)]
pub(crate) struct Records {
    count: usize,
    cwds: Vec<PathBuf>,
    model: Option<String>,
    summary: Option<String>,
    custom_title: Option<String>,
}

impl Records {
    pub(crate) fn push(&mut self, entry: LogEntry, origin: Origin) -> Option<Event> {
        self.count += 1;
        let (timestamp, kind) = match entry {
            LogEntry::User {
                message,
                timestamp,
                cwd,
                parent_tool_use_id,
                usage,
                ..
            } => {
                if let Some(cwd) = cwd {
                    push_cwd(&mut self.cwds, PathBuf::from(cwd));
                }
                let mut parts = match message.content {
                    UserContent::String(text) => vec![Part::Text(text)],
                    UserContent::Blocks(blocks) => parts(blocks),
                };
                for part in &mut parts {
                    if let Part::Text(text) = part
                        && let Some(typed) = unframed(text)
                    {
                        *text = typed;
                    }
                }
                (
                    timestamp,
                    EventKind::Message(Message {
                        role: Role::User,
                        id: None,
                        parts,
                        model: None,
                        usage: usage.map(usage_of),
                        parent_tool_use_id,
                    }),
                )
            }
            LogEntry::Assistant {
                message,
                timestamp,
                parent_tool_use_id,
                ..
            } => {
                if self.model.is_none() {
                    self.model.clone_from(&message.model);
                }
                (
                    timestamp,
                    EventKind::Message(Message {
                        role: Role::Assistant,
                        id: message.id,
                        parts: parts(message.content),
                        model: message.model,
                        usage: message.usage.map(usage_of),
                        parent_tool_use_id,
                    }),
                )
            }
            LogEntry::Summary { summary } => {
                if self.summary.is_none() {
                    self.summary = Some(summary.clone());
                }
                (None, title(TitleKind::Summary, summary))
            }
            LogEntry::AiTitle { ai_title } => {
                let trimmed = ai_title.trim();
                if !trimmed.is_empty() {
                    self.summary = Some(trimmed.to_owned());
                }
                (None, title(TitleKind::Generated, ai_title))
            }
            LogEntry::CustomTitle { custom_title } => {
                let trimmed = custom_title.trim();
                self.custom_title = (!trimmed.is_empty()).then(|| trimmed.to_owned());
                (None, title(TitleKind::Custom, custom_title))
            }
            LogEntry::Progress { data, .. } => (None, subagent(&data)),
            LogEntry::FileHistorySnapshot { .. } => return None,
            LogEntry::System { subtype, extra, .. } if subtype == "local_command" => {
                let timestamp = extra
                    .get("timestamp")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
                (timestamp, local_command(&extra))
            }
            LogEntry::System { .. }
            | LogEntry::AgentName { .. }
            | LogEntry::PermissionMode { .. }
            | LogEntry::Unknown => (None, EventKind::Other),
        };
        Some(Event {
            origin,
            timestamp: timestamp.as_deref().and_then(parse_timestamp),
            visibility: Visibility::Dialogue,
            kind,
        })
    }
}

/// Opens what Claude Code writes, ahead of the person's question, as the
/// first message of a `/btw` fork.
const FORK_BOILERPLATE: (&str, &str) = ("<fork-boilerplate>", "</fork-boilerplate>");
const FORK_DIRECTIVE: &str = "Your directive:";
/// How Claude Code hands an agent a message the person sent mid-turn.
const MID_TURN_OPEN: &str = "The user sent a new message while you were working:";
const MID_TURN_CLOSE: &str =
    "\n\nThis is how Claude Code surfaces messages the user sends mid-turn";

/// What the person wrote in a user text that Claude Code framed for an
/// agent: the question after a fork's boilerplate, or a message sent
/// mid-turn without its wrapper. `None` for other texts.
fn unframed(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    let typed = if let Some(rest) = trimmed.strip_prefix(FORK_BOILERPLATE.0) {
        let (_, rest) = rest.split_once(FORK_BOILERPLATE.1)?;
        let rest = rest.trim_start();
        rest.strip_prefix(FORK_DIRECTIVE).unwrap_or(rest)
    } else {
        let rest = trimmed.strip_prefix(MID_TURN_OPEN)?;
        rest.rfind(MID_TURN_CLOSE).map_or(rest, |end| &rest[..end])
    };
    Some(typed.trim().to_owned())
}

/// A `/btw` side question, which Claude Code records as a local command;
/// other local commands are bookkeeping.
fn local_command(extra: &serde_json::Value) -> EventKind {
    let content = extra
        .get("content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    match crate::history::extract_skill_preview(content) {
        Some(command) => match command.strip_prefix("/btw ") {
            Some(question) => EventKind::Note {
                label: "/btw".to_owned(),
                text: question.trim().to_owned(),
                searchable: true,
                usage: None,
            },
            None => EventKind::Other,
        },
        None => EventKind::Other,
    }
}

fn title(kind: TitleKind, text: String) -> EventKind {
    EventKind::Title { kind, text }
}

/// The subagent message a progress record reports, if it reports one.
fn subagent(data: &serde_json::Value) -> EventKind {
    let Some(progress) = crate::claude::parse_agent_progress(data) else {
        return EventKind::Other;
    };
    let role = match progress.message.message_type.as_str() {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => return EventKind::Other,
    };
    let AgentContent::Blocks(blocks) = progress.message.message.content;
    EventKind::Subagent {
        agent_id: progress.agent_id,
        message: Message {
            role,
            id: None,
            parts: parts(blocks),
            model: None,
            usage: None,
            parent_tool_use_id: None,
        },
    }
}

fn parts(blocks: Vec<ContentBlock>) -> Vec<Part> {
    blocks
        .into_iter()
        .map(|block| match block {
            ContentBlock::Text { text } => Part::Text(text),
            ContentBlock::ToolUse { id, name, input } => Part::ToolCall { id, name, input },
            ContentBlock::ToolResult {
                tool_use_id,
                content,
            } => Part::ToolResult {
                call_id: tool_use_id,
                output: content,
            },
            ContentBlock::Thinking { thinking, .. } => Part::Thinking(thinking),
            ContentBlock::Image { .. } => Part::Image,
            ContentBlock::Other => Part::Other,
        })
        .collect()
}

fn usage_of(usage: TokenUsage) -> Usage {
    Usage {
        input: usage.input_tokens,
        output: usage.output_tokens,
        cache_creation: usage.cache_creation_input_tokens,
        cache_read: usage.cache_read_input_tokens,
    }
}

pub(super) fn parse_timestamp(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_with_mtime(lines: &[&str], modified: SystemTime) -> Transcript {
        let path = Path::new("/projects/-work/session.jsonl");
        let key = ConversationKey {
            instance: SourceInstance {
                kind: SourceKind::Claude,
                root: PathBuf::new(),
            },
            native_id: path.to_string_lossy().into_owned(),
        };
        let contents = lines.join("\n");
        decode_reader(key, path, contents.as_bytes(), Some(modified), &mut |_| {}).unwrap()
    }

    #[test]
    fn last_activity_is_the_newest_message_not_the_file_mtime() {
        let modified = SystemTime::now();
        let transcript = decode_with_mtime(
            &[
                r#"{"type":"user","timestamp":"2026-10-15T14:50:00Z","message":{"role":"user","content":"hi"}}"#,
                r#"{"type":"assistant","timestamp":"2026-10-15T14:51:00Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"t","name":"Bash","input":{}}]}}"#,
                r#"{"type":"user","timestamp":"2026-10-15T14:51:56Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"ok"}]}}"#,
                r#"{"type":"system","subtype":"turn_duration","timestamp":"2026-10-15T14:52:30Z"}"#,
                r#"{"type":"system","subtype":"away_summary","timestamp":"2026-10-16T14:44:00Z"}"#,
            ],
            modified,
        );
        assert_eq!(
            transcript.meta.updated_at.unwrap().to_rfc3339(),
            "2026-10-15T14:51:56+00:00"
        );
    }

    #[test]
    fn records_every_distinct_cwd_with_the_first_as_cwd() {
        let transcript = decode_with_mtime(
            &[
                r#"{"type":"user","cwd":"/work","message":{"role":"user","content":"a"}}"#,
                r#"{"type":"user","cwd":"/work/repo","message":{"role":"user","content":"b"}}"#,
                r#"{"type":"user","cwd":"/work","message":{"role":"user","content":"c"}}"#,
                r#"{"type":"user","message":{"role":"user","content":"d"}}"#,
            ],
            SystemTime::now(),
        );
        assert_eq!(transcript.meta.cwd, Some(PathBuf::from("/work")));
        assert_eq!(
            transcript.meta.cwds,
            vec![PathBuf::from("/work"), PathBuf::from("/work/repo")]
        );
    }

    #[test]
    fn a_session_without_message_times_was_last_active_at_its_mtime() {
        let modified = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let transcript = decode_with_mtime(
            &[r#"{"type":"user","message":{"role":"user","content":"hi"}}"#],
            modified,
        );
        assert_eq!(
            transcript.meta.updated_at,
            Some(DateTime::<Utc>::from(modified))
        );
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    const PROMPT: &str = r#"{"type":"user","timestamp":"2026-10-15T14:50:00Z","cwd":"/work","message":{"role":"user","content":"hi"}}"#;
    const FORK_META: &str = r#"{"agentType":"fork","isFork":true,"description":"Ask the narwhal"}"#;

    #[test]
    fn sidecars_sit_in_the_subagents_directory_of_a_session() {
        let path = Path::new("/p/-work/s1/subagents/agent-a1.jsonl");
        assert_eq!(
            sidecar(path),
            Some(Sidecar {
                project_dir: Path::new("/p/-work"),
                session_id: "s1",
                agent_id: "a1",
            })
        );
        assert_eq!(
            parent_transcript(path),
            Some(PathBuf::from("/p/-work/s1.jsonl"))
        );
        assert_eq!(project_dir(path), Some(Path::new("/p/-work")));
        assert_eq!(
            project_dir(Path::new("/p/-work/s1.jsonl")),
            Some(Path::new("/p/-work"))
        );
        for other in [
            "/p/-work/agent-a1.jsonl",
            "/p/-work/s1/other/agent-a1.jsonl",
            "/p/-work/s1/subagents/agent-a1.meta.json",
            "/p/-work/s1/subagents/agent-aprompt_suggestion-1.jsonl",
            "/p/-work/s1/subagents/agent-acompact-1.jsonl",
        ] {
            assert_eq!(sidecar(Path::new(other)), None, "{other}");
        }
    }

    #[test]
    fn discovery_lists_sidecars_and_folds_their_metadata_into_the_stamp() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("-work");
        write(&project.join("s1.jsonl"), PROMPT);
        let fork = project.join("s1/subagents/agent-afork-1.jsonl");
        write(&fork, PROMPT);
        write(
            &project.join("s1/subagents/agent-aprompt_suggestion-2.jsonl"),
            PROMPT,
        );
        write(&project.join("s1/tool-results/out.txt"), "x");
        write(&project.join("agent-old.jsonl"), PROMPT);
        let old = SystemTime::now() - std::time::Duration::from_secs(3600);
        let settle = |path: &Path| {
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(old)
                .unwrap();
        };
        settle(&fork);
        let adapter = ClaudeAdapter::new(root.path().to_path_buf());
        let mut found: Vec<PathBuf> = adapter
            .discover(SystemTime::now())
            .found
            .into_iter()
            .map(|located| located.path)
            .collect();
        found.sort();
        assert_eq!(found, vec![fork.clone(), project.join("s1.jsonl")]);

        let native_id = "-work/s1/subagents/agent-afork-1.jsonl";
        let before = adapter.resolve(native_id, SystemTime::now()).unwrap().stamp;
        let meta = fork.with_extension("meta.json");
        write(&meta, FORK_META);
        settle(&meta);
        let after = adapter.resolve(native_id, SystemTime::now()).unwrap().stamp;
        assert!(after.is_known());
        assert_eq!(after.size, before.size + FORK_META.len() as u64);
    }

    #[test]
    fn a_user_fork_branches_off_its_session_and_a_subagent_is_its_child() {
        let root = tempfile::tempdir().unwrap();
        let sidecars = root.path().join("-work/s1/subagents");
        let fork = sidecars.join("agent-afork-1.jsonl");
        let agent_fork = sidecars.join("agent-a2.jsonl");
        let subagent = sidecars.join("agent-a3.jsonl");
        let contents = format!(
            "{}\n{PROMPT}\n",
            r#"{"type":"fork-context-ref","agentId":"afork-1","parentSessionId":"s1"}"#
        );
        write(&fork, &contents);
        write(&fork.with_extension("meta.json"), FORK_META);
        write(&agent_fork, PROMPT);
        write(
            &agent_fork.with_extension("meta.json"),
            r#"{"agentType":"fork","isFork":true,"toolUseId":"toolu_1"}"#,
        );
        write(&subagent, PROMPT);
        write(
            &subagent.with_extension("meta.json"),
            r#"{"agentType":"Explore","description":"Find the walrus","toolUseId":"toolu_2"}"#,
        );
        let adapter = ClaudeAdapter::new(root.path().to_path_buf());
        let decode = |path: &Path| {
            let native_id = path.strip_prefix(root.path()).unwrap().to_string_lossy();
            let located = adapter.resolve(&native_id, SystemTime::now()).unwrap();
            adapter.decode(&located).unwrap().unwrap()
        };

        let transcript = decode(&fork);
        assert_eq!(
            transcript.relationship,
            Relationship::Branch {
                parent: "s1".to_owned()
            }
        );
        assert_eq!(transcript.meta.session_id, "afork-1");
        assert_eq!(transcript.meta.summary.as_deref(), Some("Ask the narwhal"));
        assert_eq!(
            decode(&agent_fork).relationship,
            Relationship::Child {
                parent: "s1".to_owned()
            }
        );
        let transcript = decode(&subagent);
        assert_eq!(
            transcript.relationship,
            Relationship::Child {
                parent: "s1".to_owned()
            }
        );
        assert_eq!(
            transcript.meta.summary.as_deref(),
            Some("Explore: Find the walrus")
        );
    }

    #[test]
    fn framed_user_texts_read_as_what_the_person_wrote() {
        assert_eq!(
            unframed(
                "<fork-boilerplate>\nYou are a forked agent.\n</fork-boilerplate>\n\nYour directive: why is the sky blue?"
            )
            .as_deref(),
            Some("why is the sky blue?")
        );
        assert_eq!(
            unframed(
                "The user sent a new message while you were working:\nalso check tides\n\nThis is how Claude Code surfaces messages the user sends mid-turn. Address it."
            )
            .as_deref(),
            Some("also check tides")
        );
        assert_eq!(unframed("plain question"), None);
    }

    #[test]
    fn a_btw_question_is_a_searchable_note_and_other_local_commands_are_not() {
        let entry = |content: &str| -> LogEntry {
            serde_json::from_value(serde_json::json!({
                "type": "system",
                "subtype": "local_command",
                "timestamp": "2026-10-15T14:50:00Z",
                "content": content,
            }))
            .unwrap()
        };
        let mut records = Records::default();
        let event = records
            .push(
                entry("<command-name>/btw</command-name>\n<command-message>btw</command-message>\n<command-args>is the walrus fed?</command-args>"),
                Origin::default(),
            )
            .unwrap();
        assert_eq!(
            event.kind,
            EventKind::Note {
                label: "/btw".to_owned(),
                text: "is the walrus fed?".to_owned(),
                searchable: true,
                usage: None,
            }
        );
        assert!(event.timestamp.is_some());
        for content in [
            "<command-name>/model</command-name>\n<command-args>opus</command-args>",
            "<local-command-stdout>Set model</local-command-stdout>",
        ] {
            let event = records.push(entry(content), Origin::default()).unwrap();
            assert_eq!(event.kind, EventKind::Other, "{content}");
        }
    }

    #[test]
    fn sidecars_resume_their_session_and_cannot_be_forked() {
        // Fork: planned by `resume::plan_claude` with a sandbox env and a
        // workspace in place of upstream's `resume_plan` and cwd.
        let root = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(root.path());
        let projects = env.claude_projects_root().unwrap();
        let parent = projects.join("-work/s1.jsonl");
        let fork = projects.join("-work/s1/subagents/agent-afork-1.jsonl");
        write(&fork, PROMPT);
        let target = Target {
            path: &fork,
            project: None,
        };
        let elsewhere = root.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        let workspace = WorkspaceScope::new(&elsewhere).unwrap();
        let launch = Launch {
            launcher: Launcher::default_for(SourceKind::Claude),
            default_args: Vec::new(),
        };
        let adapter = ClaudeAdapter::new(projects.clone());
        let plan = || adapter.resume(&env, target, Some(&workspace), false, &launch);
        let error = plan().unwrap_err().to_string();
        assert!(error.contains("no longer exists"), "{error}");

        write(&parent, PROMPT);
        let resume = plan().unwrap();
        assert_eq!(resume.session_path, parent);
        let copy = resume.copy.unwrap();
        assert_eq!(copy.files[0].from, parent);
        assert_eq!(copy.session_id, "s1");
        assert_eq!(resume.args, ["--resume", "s1"]);

        let branch = Provenance {
            branch: true,
            ..Provenance::default()
        };
        let capabilities = adapter.capabilities_for(branch);
        assert!(capabilities.resume && capabilities.delete);
        assert!(!capabilities.fork && !capabilities.rename);
        assert_eq!(
            adapter.capabilities_for(Provenance::default()),
            Capabilities::ALL
        );
    }

    #[test]
    fn deleting_a_session_keeps_its_forks_and_deleting_a_sidecar_keeps_its_session() {
        let root = tempfile::tempdir().unwrap();
        let session_dir = root.path().join("-work/s1");
        let sidecars = session_dir.join("subagents");
        let fork = sidecars.join("agent-afork-1.jsonl");
        let subagent = sidecars.join("agent-a2.jsonl");
        write(&fork, PROMPT);
        write(&fork.with_extension("meta.json"), FORK_META);
        write(&subagent, PROMPT);
        write(
            &subagent.with_extension("meta.json"),
            r#"{"agentType":"Explore"}"#,
        );
        write(&session_dir.join("tool-results/out.txt"), "x");

        remove_session_dir(&session_dir).unwrap();
        let mut left: Vec<PathBuf> = fs::read_dir(&sidecars)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        left.sort();
        assert_eq!(left, vec![fork.clone(), fork.with_extension("meta.json")]);
        assert!(!session_dir.join("tool-results").exists());

        delete_sidecar(&fork).unwrap();
        assert!(fs::read_dir(&sidecars).unwrap().next().is_none());

        write(&subagent, PROMPT);
        remove_session_dir(&session_dir).unwrap();
        assert!(!session_dir.exists());
    }
}
