//! Maintenance actions (upstream `main.rs` one-shot flags and the
//! `delete-empty` subcommand) as data-returning operations:
//!
//! * delete-empty: [`find_empty_candidates`] (dry run with size, mtime and
//!   artifact directories), [`delete_empty_candidates`] (deletes the
//!   confirmed list, skipping candidates that changed since the scan),
//!   [`format_delete_empty_report`] (the CLI's stdout text);
//! * `--delete <uuid>`: [`find_session_copies`] (what would be removed) and
//!   [`delete_session_by_uuid`];
//! * deleting a listed conversation (any source) in two steps around the
//!   confirmation: [`plan_session_delete`] and [`execute_session_delete`];
//! * `--show-dir`: [`show_dir`];
//! * `--debug-search`: [`debug_search_report`] (scoring breakdown per result
//!   plus the CLI's stderr text);
//! * `--debug` parse-error log: [`parse_error_report`] and
//!   [`log_parse_errors`].
//!
//! All of them write only where upstream wrote (Claude transcripts for the
//! deletions, the debug log); every location comes from the
//! [`HistoryEnv`].

use crate::debug::DebugLevel;
use crate::env::{HistoryEnv, WorkspaceScope};
use crate::error::{AppError, Result};
use crate::history::{self, Conversation, ParseError, Source};
use crate::search;
use crate::search::literal::CaseMode;
use crate::time_filter::TimeFilter;
use chrono::{DateTime, Local};
use std::path::{Path, PathBuf};

pub use crate::history::{DeleteEmptyScope, DeleteEmptySummary, EmptyTranscript};

/// Number of results upstream's `--debug-search` prints.
pub const DEBUG_SEARCH_TOP: usize = 30;

/// Load every conversation (Claude, Pi, OMP), keep those inside
/// `time_filter`, newest first: the corpus upstream's one-shot flags
/// (`--debug-search`, the semantic flags) worked on.
pub fn load_sorted_conversations(
    env: &HistoryEnv,
    show_last: bool,
    debug: Option<DebugLevel>,
    time_filter: &TimeFilter,
) -> Result<Vec<Conversation>> {
    let mut conversations = history::load_all_conversations(env, show_last, debug)?;
    conversations.retain(|conversation| time_filter.matches(conversation.timestamp));
    conversations.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    Ok(conversations)
}

// ---------------------------------------------------------------------------
// delete-empty
// ---------------------------------------------------------------------------

/// A transcript `delete-empty` would delete, with what the GUI needs to show
/// before confirming.
#[derive(Clone, Debug)]
pub struct EmptyCandidate {
    /// Path, session id, project display name, user message and line counts,
    /// preview (upstream's dry-run data).
    pub transcript: EmptyTranscript,
    /// Size of the transcript in bytes.
    pub size: u64,
    /// Last modification time of the transcript.
    pub modified: Option<DateTime<Local>>,
    /// The `<project>/<session id>/` directory deleted together with the
    /// transcript, when it exists ([`history::session_artifact_dir`]: never
    /// for a file whose stem is not a plain session id, such as `...jsonl`).
    pub artifact_dirs: Vec<PathBuf>,
}

impl EmptyCandidate {
    pub fn path(&self) -> &Path {
        &self.transcript.path
    }

    pub fn session_id(&self) -> &str {
        &self.transcript.session_id
    }

    pub fn project(&self) -> &str {
        &self.transcript.project_name
    }

    /// Whether the transcript changed within `window` before `now`. A brand
    /// new session that has a prompt but no reply yet looks empty; GUIs may
    /// want to exclude such files (upstream did not).
    pub fn modified_within(&self, now: DateTime<Local>, window: chrono::Duration) -> bool {
        self.modified
            .is_some_and(|modified| now - modified < window)
    }

    /// Upstream's two stdout lines for this candidate (without newlines):
    /// `"{id}\t{project}\tusers={n}\tlines={n}\t{preview}"` and
    /// `"  {path}"`.
    pub fn cli_lines(&self) -> [String; 2] {
        candidate_lines(&self.transcript)
    }
}

fn candidate_lines(transcript: &EmptyTranscript) -> [String; 2] {
    let preview = transcript
        .preview
        .as_deref()
        .map(truncate_delete_empty_preview)
        .unwrap_or_else(|| "(no user messages)".to_string());
    [
        format!(
            "{}\t{}\tusers={}\tlines={}\t{}",
            transcript.session_id,
            transcript.project_name,
            transcript.user_messages,
            transcript.line_count,
            preview
        ),
        format!("  {}", transcript.path.display()),
    ]
}

/// Find Claude transcripts without any assistant message (upstream
/// `delete-empty` dry run, `loader.rs:403-525`). `scope` is
/// [`DeleteEmptyScope::All`] or the workspace's Claude project directory
/// ([`DeleteEmptyScope::Local`]). Sorted by path.
pub fn find_empty_candidates(
    env: &HistoryEnv,
    scope: &DeleteEmptyScope,
) -> Result<Vec<EmptyCandidate>> {
    Ok(history::find_empty_transcripts(env, scope)?
        .into_iter()
        .map(empty_candidate)
        .collect())
}

fn empty_candidate(transcript: EmptyTranscript) -> EmptyCandidate {
    let metadata = std::fs::metadata(&transcript.path).ok();
    let size = metadata
        .as_ref()
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let modified = metadata
        .and_then(|metadata| metadata.modified().ok())
        .map(DateTime::<Local>::from);
    let artifact_dirs = history::session_artifact_dir(&transcript.path, &transcript.session_id)
        .filter(|dir| dir.is_dir())
        .into_iter()
        .collect();
    EmptyCandidate {
        transcript,
        size,
        modified,
        artifact_dirs,
    }
}

/// A deletion that failed.
#[derive(Debug)]
pub struct DeleteFailure {
    pub path: PathBuf,
    pub error: AppError,
}

/// Why [`delete_empty_candidates`] left a candidate alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// The transcript's size or modification time differs from the scan
    /// (scan again to see what it holds now).
    Changed,
    /// The transcript has an assistant message now.
    NoLongerEmpty,
    /// The transcript does not exist any more.
    Missing,
}

/// A candidate that was not deleted because it changed after the scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedCandidate {
    pub path: PathBuf,
    pub reason: SkipReason,
}

/// Result of [`delete_empty_candidates`].
#[derive(Debug, Default)]
pub struct DeleteEmptyOutcome {
    /// Transcripts deleted (with their session directories), in order.
    pub deleted: Vec<PathBuf>,
    /// Candidates kept because they changed since the scan, in order.
    pub skipped: Vec<SkippedCandidate>,
    /// The first failure; like upstream, deletion stops there.
    pub failure: Option<DeleteFailure>,
}

impl DeleteEmptyOutcome {
    pub fn deleted_count(&self) -> usize {
        self.deleted.len()
    }

    /// The transcripts of `candidates` that were deleted, in order (the
    /// list for [`format_delete_empty_report`] after deleting).
    pub fn deleted_transcripts(&self, candidates: &[EmptyCandidate]) -> Vec<EmptyTranscript> {
        candidates
            .iter()
            .filter(|candidate| self.deleted.contains(&candidate.transcript.path))
            .map(|candidate| candidate.transcript.clone())
            .collect()
    }
}

/// Delete the given (confirmed) candidates: each transcript and its
/// `<project>/<session id>/` directory, in order, stopping at the first
/// error (upstream `delete-empty --yes`). Unlike upstream, the transcripts
/// deleted before a failure are reported.
///
/// Upstream deleted right after scanning; here the list may have been shown
/// for minutes, so each candidate is checked again right before it is
/// deleted. A candidate whose size or modification time changed since the
/// scan, that has an assistant message now (e.g. a new Claude Code session
/// whose reply arrived meanwhile) or that is gone is kept and reported in
/// [`DeleteEmptyOutcome::skipped`].
pub fn delete_empty_candidates(candidates: &[EmptyCandidate]) -> DeleteEmptyOutcome {
    let mut outcome = DeleteEmptyOutcome::default();
    for candidate in candidates {
        let path = candidate.transcript.path.clone();
        match delete_unchanged_candidate(candidate) {
            Ok(None) => outcome.deleted.push(path),
            Ok(Some(reason)) => outcome.skipped.push(SkippedCandidate { path, reason }),
            Err(error) => {
                outcome.failure = Some(DeleteFailure { path, error });
                break;
            }
        }
    }
    outcome
}

/// Delete one candidate unless it changed since the scan (`Some(reason)`).
fn delete_unchanged_candidate(candidate: &EmptyCandidate) -> Result<Option<SkipReason>> {
    let metadata = match std::fs::metadata(&candidate.transcript.path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Some(SkipReason::Missing));
        }
        Err(error) => return Err(error.into()),
    };
    let modified = metadata.modified().ok().map(DateTime::<Local>::from);
    if metadata.len() != candidate.size || modified != candidate.modified {
        return Ok(Some(SkipReason::Changed));
    }
    Ok(
        match history::delete_empty_transcript(&candidate.transcript)? {
            history::EmptyTranscriptDeletion::Deleted => None,
            history::EmptyTranscriptDeletion::NoLongerEmpty => Some(SkipReason::NoLongerEmpty),
            history::EmptyTranscriptDeletion::Missing => Some(SkipReason::Missing),
        },
    )
}

/// Upstream's summary line (`main.rs:121-127`).
pub fn delete_empty_summary_line(delete: bool, count: usize) -> String {
    if delete {
        format!("Summary: deleted {count} empty transcript(s).")
    } else {
        format!("Summary: {count} empty transcript(s) would be deleted.")
    }
}

/// Upstream's preview truncation (`main.rs:129-138`): 120 characters, then
/// `...`.
pub fn truncate_delete_empty_preview(preview: &str) -> String {
    const MAX_CHARS: usize = 120;
    let mut chars = preview.chars();
    let truncated = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{}...", truncated)
    } else {
        truncated
    }
}

/// The exact stdout of upstream `delete-empty` (`main.rs:73-119`), with a
/// trailing newline after every line.
///
/// `deleted` is `None` for a dry run and `Some(count)` after deleting: the
/// header and the summary line both report `count`. Upstream either
/// deleted every candidate or printed nothing, so it listed the whole
/// candidate list; [`delete_empty_candidates`] may skip candidates or stop
/// early, so after deleting pass the transcripts actually deleted
/// ([`DeleteEmptyOutcome::deleted_transcripts`]) with
/// `Some(outcome.deleted_count())`.
pub fn format_delete_empty_report(
    transcripts: &[EmptyTranscript],
    deleted: Option<usize>,
) -> String {
    let delete = deleted.is_some();
    let mut out = String::new();
    if transcripts.is_empty() {
        out.push_str("No empty transcripts found.\n");
        out.push_str(&delete_empty_summary_line(delete, 0));
        out.push('\n');
        return out;
    }
    match deleted {
        Some(count) => out.push_str(&format!("Deleted {count} empty transcript(s):\n")),
        None => out.push_str(&format!(
            "Found {} empty transcript(s). Re-run with --yes to delete:\n",
            transcripts.len()
        )),
    }
    for transcript in transcripts {
        for line in candidate_lines(transcript) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    // The same count as the header: after a partial delete the list length
    // is not the number deleted.
    out.push_str(&delete_empty_summary_line(
        delete,
        deleted.unwrap_or(transcripts.len()),
    ));
    out.push('\n');
    out
}

// ---------------------------------------------------------------------------
// delete by UUID
// ---------------------------------------------------------------------------

/// One copy of a Claude session (a session forked across projects exists
/// once per project directory).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionCopyFiles {
    /// `<projects root>/<project>/<uuid>.jsonl`
    pub jsonl: PathBuf,
    /// The encoded project directory name.
    pub project_dir_name: String,
    /// `<project>/<uuid>/` (tool results, subagents) when it exists.
    pub session_dir: Option<PathBuf>,
}

fn validate_uuid(uuid: &str) -> Result<()> {
    if uuid.is_empty() || !uuid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(AppError::SessionNotFound(uuid.to_owned()));
    }
    Ok(())
}

/// Every copy of the Claude session `uuid` and the files deleting it would
/// remove. Errors like upstream `--delete`: `SessionNotFound` for an invalid
/// id (only `[A-Za-z0-9-]`) or when no copy exists.
pub fn find_session_copies(env: &HistoryEnv, uuid: &str) -> Result<Vec<SessionCopyFiles>> {
    validate_uuid(uuid)?;
    let copies = history::find_all_jsonl_by_uuid(env, uuid)?
        .into_iter()
        .map(|jsonl| {
            let project_dir = jsonl.parent().map(Path::to_path_buf).unwrap_or_default();
            let session_dir = Some(project_dir.join(uuid)).filter(|dir| dir.is_dir());
            SessionCopyFiles {
                project_dir_name: project_dir
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                jsonl,
                session_dir,
            }
        })
        .collect::<Vec<_>>();
    if copies.is_empty() {
        return Err(AppError::SessionNotFound(uuid.to_owned()));
    }
    Ok(copies)
}

/// Result of [`delete_session_by_uuid`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteByUuidReport {
    pub session_id: String,
    /// Number of copies deleted.
    pub deleted: usize,
    /// Upstream's stderr line ([`delete_by_uuid_message`]).
    pub message: String,
}

/// Delete every copy of the Claude session `uuid` and its session
/// directories (upstream `--delete`, `loader.rs:350-375`).
pub fn delete_session_by_uuid(env: &HistoryEnv, uuid: &str) -> Result<DeleteByUuidReport> {
    let deleted = history::delete_session_by_uuid(env, uuid)?;
    Ok(DeleteByUuidReport {
        session_id: uuid.to_string(),
        deleted,
        message: delete_by_uuid_message(uuid, deleted),
    })
}

// ---------------------------------------------------------------------------
// delete a listed conversation in two steps (K4)
// ---------------------------------------------------------------------------

/// What deleting one listed conversation removes, computed before the
/// confirmation ([`plan_session_delete`]) and executed only after it
/// ([`execute_session_delete`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeletePlan {
    /// The source of the conversation the plan was made for (addendum A2:
    /// never the live selection's).
    pub source: Source,
    /// The listed conversation the plan was made for: the path of the
    /// upstream debug-log line when the delete fails, whichever item failed.
    pub path: PathBuf,
    /// Claude: the transcript's file stem; Pi/OMP: the session header id
    /// (the file stem when the header has none).
    pub session_id: String,
    /// Transcripts to remove: every copy of a Claude session across project
    /// directories (cross-project forks), else the one file.
    pub files: Vec<PathBuf>,
    /// Directories to remove: the existing `<project>/<uuid>/` session
    /// directories (Claude) or the artifact directory (OMP).
    pub dirs: Vec<PathBuf>,
}

/// What [`execute_session_delete`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeleteReport {
    pub removed_files: Vec<PathBuf>,
    pub removed_dirs: Vec<PathBuf>,
    /// Planned items that no longer existed (not an error).
    pub missing: Vec<PathBuf>,
}

/// A failed [`execute_session_delete`]: the error together with what was
/// removed before it (empty when the plan was refused up front).
#[derive(Debug)]
pub struct SessionDeleteFailure {
    pub error: AppError,
    /// Items removed or found missing before the failure.
    pub partial: DeleteReport,
}

impl std::fmt::Display for SessionDeleteFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for SessionDeleteFailure {}

impl From<SessionDeleteFailure> for AppError {
    fn from(failure: SessionDeleteFailure) -> Self {
        failure.error
    }
}

impl From<Box<SessionDeleteFailure>> for AppError {
    fn from(failure: Box<SessionDeleteFailure>) -> Self {
        failure.error
    }
}

/// Plan the deletion of the listed conversation at `path` (blocking file
/// system reads; run it off the UI thread):
///
/// * Claude: every `<uuid>.jsonl` with `path`'s stem in all Claude project
///   directories ([`find_session_copies`]) plus their existing session
///   directories. An invalid id or no copy is `SessionNotFound`, like
///   upstream `--delete`;
/// * Pi: `path`, if it is a `.jsonl` Pi session, else `SessionNotFound`;
/// * OMP: `path`, if it is a `.jsonl` OMP session, plus its artifact
///   directory when it exists.
///
/// Nothing is removed.
pub fn plan_session_delete(env: &HistoryEnv, path: &Path, source: Source) -> Result<DeletePlan> {
    let not_found = || AppError::SessionNotFound(path.display().to_string());
    let is_jsonl = path.extension().and_then(|extension| extension.to_str()) == Some("jsonl");
    match source {
        Source::Claude => {
            let session_id = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default()
                .to_string();
            let copies = find_session_copies(env, &session_id)?;
            Ok(DeletePlan {
                source,
                path: path.to_path_buf(),
                files: copies.iter().map(|copy| copy.jsonl.clone()).collect(),
                dirs: copies
                    .into_iter()
                    .filter_map(|copy| copy.session_dir)
                    .collect(),
                session_id,
            })
        }
        Source::Pi => {
            if !is_jsonl || !history::pi::is_pi_file(path) {
                return Err(not_found());
            }
            let header = history::pi::parse_file(path)?.map(|projection| projection.header.id);
            Ok(DeletePlan {
                source,
                path: path.to_path_buf(),
                session_id: plan_session_id(header, path),
                files: vec![path.to_path_buf()],
                dirs: Vec::new(),
            })
        }
        Source::Omp => {
            if !is_jsonl || !history::pi::is_omp_file(path) {
                return Err(not_found());
            }
            let header = history::pi::parse_omp_file(path)?.map(|projection| projection.header.id);
            Ok(DeletePlan {
                source,
                path: path.to_path_buf(),
                session_id: plan_session_id(header, path),
                files: vec![path.to_path_buf()],
                dirs: history::omp_loader::session_artifact_dir(path)
                    .filter(|dir| dir.is_dir())
                    .into_iter()
                    .collect(),
            })
        }
    }
}

fn plan_session_id(header_id: Option<String>, path: &Path) -> String {
    header_id.filter(|id| !id.is_empty()).unwrap_or_else(|| {
        path.file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default()
    })
}

/// Remove exactly the items of `plan` (blocking; run it off the UI thread),
/// transcripts first. Items that are already gone are reported in
/// [`DeleteReport::missing`]. The first failure stops the deletion: the
/// upstream debug-log line `Failed to delete session <path>: <error>` (the
/// listed conversation's [`DeletePlan::path`], as upstream) is appended to
/// the debug log and the error returned together with the partial
/// [`DeleteReport`] (items removed before it stay removed).
///
/// The plan is checked again first; anything [`plan_session_delete`] could
/// not have planned is refused with `SessionNotFound` before something is
/// removed:
///
/// * Claude: only `<projects root>/<project>/<session_id>.jsonl` files and
///   `<session_id>` directories beside one of them;
/// * Pi: exactly the listed file, a `.jsonl` Pi session (when it still
///   exists), and no directory;
/// * OMP: exactly the listed file, a `.jsonl` OMP session (when it still
///   exists), and at most its artifact directory.
pub fn execute_session_delete(
    env: &HistoryEnv,
    plan: &DeletePlan,
) -> std::result::Result<DeleteReport, Box<SessionDeleteFailure>> {
    if let Err(error) = validate_delete_plan(env, plan) {
        return Err(Box::new(SessionDeleteFailure {
            error,
            partial: DeleteReport::default(),
        }));
    }
    let mut report = DeleteReport::default();
    for file in &plan.files {
        match std::fs::remove_file(file) {
            Ok(()) => report.removed_files.push(file.clone()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                report.missing.push(file.clone());
            }
            Err(error) => {
                return Err(Box::new(SessionDeleteFailure {
                    error: log_delete_failure(env, &plan.path, error.into()),
                    partial: report,
                }));
            }
        }
    }
    for dir in &plan.dirs {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => report.removed_dirs.push(dir.clone()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                report.missing.push(dir.clone());
            }
            Err(error) => {
                return Err(Box::new(SessionDeleteFailure {
                    error: log_delete_failure(env, &plan.path, error.into()),
                    partial: report,
                }));
            }
        }
    }
    Ok(report)
}

fn validate_delete_plan(env: &HistoryEnv, plan: &DeletePlan) -> Result<()> {
    let refuse = || Err(AppError::SessionNotFound(plan.session_id.clone()));
    let jsonl =
        |path: &PathBuf| path.extension().and_then(|extension| extension.to_str()) == Some("jsonl");
    if !plan.files.iter().all(jsonl) {
        return refuse();
    }
    match plan.source {
        Source::Claude => {
            validate_uuid(&plan.session_id)?;
            let id = std::ffi::OsStr::new(&plan.session_id);
            let root = env.claude_projects_root()?;
            // `<projects root>/<project>/<id>.jsonl`, `<project>` a plain name.
            let in_a_project = |file: &Path| {
                file.parent().is_some_and(|project| {
                    project.file_name().is_some() && project.parent() == Some(root.as_path())
                })
            };
            let files_ok = plan
                .files
                .iter()
                .all(|file| file.file_stem() == Some(id) && in_a_project(file));
            // `<projects root>/<project>/<id>/`, beside one of the files.
            let dirs_ok = plan.dirs.iter().all(|dir| {
                dir.file_name() == Some(id)
                    && plan.files.iter().any(|file| file.parent() == dir.parent())
            });
            if !files_ok || !dirs_ok {
                return refuse();
            }
        }
        Source::Pi | Source::Omp => {
            let [file] = plan.files.as_slice() else {
                return refuse();
            };
            if *file != plan.path {
                return refuse();
            }
            // A file deleted meanwhile is reported missing, not refused.
            let exists = std::fs::symlink_metadata(file).is_ok();
            let dirs_ok = if plan.source == Source::Pi {
                if exists && !history::pi::is_pi_file(file) {
                    return refuse();
                }
                plan.dirs.is_empty()
            } else {
                if exists && !history::pi::is_omp_file(file) {
                    return refuse();
                }
                let artifacts = history::omp_loader::session_artifact_dir(file);
                plan.dirs.len() <= 1 && plan.dirs.iter().all(|dir| Some(dir) == artifacts.as_ref())
            };
            if !dirs_ok {
                return refuse();
            }
        }
    }
    Ok(())
}

fn log_delete_failure(env: &HistoryEnv, path: &Path, error: AppError) -> AppError {
    if let Some(log_path) = env.debug_log_file() {
        let _ = crate::debug_log::log_debug(
            &log_path,
            &format!("Failed to delete session {}: {}", path.display(), error),
        );
    }
    error
}

/// `Deleted session {id}` or `Deleted session {id} ({n} copies across
/// projects)` (`main.rs:224-232`).
pub fn delete_by_uuid_message(session_id: &str, count: usize) -> String {
    if count == 1 {
        format!("Deleted session {}", session_id)
    } else {
        format!(
            "Deleted session {} ({} copies across projects)",
            session_id, count
        )
    }
}

// ---------------------------------------------------------------------------
// show-dir
// ---------------------------------------------------------------------------

/// Error text of [`show_dir`] without a workspace.
pub const WORKSPACE_REQUIRED_SHOW_DIR: &str =
    "Choose a workspace folder to show its Claude projects directory";

/// The Claude projects directory of a directory
/// (`<projects root>/<encoded dir>`; it need not exist).
pub fn projects_dir_for(env: &HistoryEnv, dir: &Path) -> Result<PathBuf> {
    env.claude_projects_dir_for(dir)
}

/// Upstream `--show-dir`: the workspace's Claude projects directory.
pub fn show_dir(env: &HistoryEnv, workspace: Option<&WorkspaceScope>) -> Result<PathBuf> {
    let workspace = workspace
        .ok_or_else(|| AppError::WorkspaceRequired(WORKSPACE_REQUIRED_SHOW_DIR.to_string()))?;
    Ok(env
        .claude_projects_root()?
        .join(workspace.project_dir_name()))
}

// ---------------------------------------------------------------------------
// debug-search
// ---------------------------------------------------------------------------

/// A quoted literal of the query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugLiteral {
    pub text: String,
    pub case_mode: CaseMode,
}

/// Per-word term frequency of one field.
#[derive(Clone, Debug, PartialEq)]
pub struct DebugSearchWord {
    pub word: String,
    pub tf: usize,
    pub ln_score: f64,
}

/// Score contribution of one searchable field.
#[derive(Clone, Debug, PartialEq)]
pub struct DebugSearchField {
    pub name: String,
    pub weight: f64,
    pub tf_score: f64,
    pub adjacency_score: f64,
    pub words: Vec<DebugSearchWord>,
}

impl DebugSearchField {
    /// Whether the field contributed (upstream printed only these).
    pub fn contributes(&self) -> bool {
        self.tf_score > 0.0 || self.adjacency_score > 0.0
    }
}

/// One ranked conversation.
#[derive(Clone, Debug, PartialEq)]
pub struct DebugSearchRow {
    /// 1-based rank.
    pub rank: usize,
    /// Index into the conversations passed to [`debug_search_report`].
    pub conversation_index: usize,
    pub path: PathBuf,
    pub score: f64,
    pub freshness: f64,
    /// Project name, or `(none)`.
    pub project: String,
    /// File stem, or `?`.
    pub session: String,
    /// Age at `now`.
    pub age: chrono::Duration,
    /// `Nh` below a day, else `Nd` ([`format_debug_age`]).
    pub age_label: String,
    pub fields: Vec<DebugSearchField>,
}

/// Upstream `--debug-search` as data.
#[derive(Clone, Debug, PartialEq)]
pub struct DebugSearchReport {
    /// The unquoted part of the query.
    pub intent: String,
    pub literals: Vec<DebugLiteral>,
    /// The top [`DEBUG_SEARCH_TOP`] results.
    pub rows: Vec<DebugSearchRow>,
    /// Number of matching conversations before the top-30 cut.
    pub total_results: usize,
}

impl DebugSearchReport {
    /// The exact stderr text upstream printed (`main.rs:291-332`).
    pub fn to_text(&self) -> String {
        let mut out = format!("intent: {:?}\n", self.intent);
        if self.literals.is_empty() {
            out.push_str("literals: none\n");
        } else {
            out.push_str("literals:\n");
            for literal in &self.literals {
                out.push_str(&format!("  {:?} ({:?})\n", literal.text, literal.case_mode));
            }
        }
        out.push('\n');
        for row in &self.rows {
            out.push_str(&format!(
                "#{:2} score={:.2} freshness={:.2} | {} | {} | {} ago\n",
                row.rank, row.score, row.freshness, row.project, row.session, row.age_label
            ));
            for field in row.fields.iter().filter(|field| field.contributes()) {
                out.push_str(&format!(
                    "     {}: tf={:.2} adj={:.2} (w={:.1})\n",
                    field.name, field.tf_score, field.adjacency_score, field.weight
                ));
                for word in field.words.iter().filter(|word| word.tf > 0) {
                    out.push_str(&format!(
                        "       \"{}\" tf={} ln={:.2}\n",
                        word.word, word.tf, word.ln_score
                    ));
                }
            }
            out.push('\n');
        }
        out
    }
}

/// Upstream `format_debug_age` (`main.rs:1989-1996`).
pub fn format_debug_age(age: chrono::Duration) -> String {
    let hours = age.num_hours();
    if hours < 24 {
        format!("{}h", hours)
    } else {
        format!("{}d", hours / 24)
    }
}

/// Whether a conversation belongs to the workspace, as upstream's
/// `--debug-search --local` decided: Claude by worktree-aware project
/// directory name, Pi/OMP by canonical working directory.
pub fn conversation_in_workspace(conversation: &Conversation, workspace: &WorkspaceScope) -> bool {
    if conversation.source != Source::Claude {
        let current = workspace.canonical_dir();
        return conversation
            .project_path
            .as_ref()
            .or(conversation.cwd.as_ref())
            .is_some_and(|path| path.canonicalize().unwrap_or_else(|_| path.clone()) == current);
    }
    conversation
        .path
        .parent()
        .and_then(|p| p.file_name())
        .is_some_and(|name| {
            history::is_same_project(&name.to_string_lossy(), workspace.project_dir_name())
        })
}

/// Lexical scoring breakdown for `query` (upstream `--debug-search`,
/// `main.rs:244-335`). `conversations` should be newest first (see
/// [`load_sorted_conversations`]); `workspace` restricts to the workspace
/// (`--local`).
pub fn debug_search_report(
    conversations: &[Conversation],
    query: &str,
    now: DateTime<Local>,
    workspace: Option<&WorkspaceScope>,
) -> DebugSearchReport {
    let searchable = search::precompute_search_text(conversations);
    let debug = search::debug_search(conversations, &searchable, query, now, |index| {
        workspace
            .is_none_or(|workspace| conversation_in_workspace(&conversations[index], workspace))
    });
    let total_results = debug.results.len();
    let rows = debug
        .results
        .iter()
        .take(DEBUG_SEARCH_TOP)
        .enumerate()
        .map(|(rank, (index, score))| {
            let conversation = &conversations[*index];
            let age = now.signed_duration_since(conversation.timestamp);
            DebugSearchRow {
                rank: rank + 1,
                conversation_index: *index,
                path: conversation.path.clone(),
                score: score.total,
                freshness: score.freshness,
                project: conversation
                    .project_name
                    .clone()
                    .unwrap_or_else(|| "(none)".to_string()),
                session: conversation
                    .path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
                    .to_string(),
                age_label: format_debug_age(age),
                age,
                fields: score
                    .fields
                    .iter()
                    .map(|field| DebugSearchField {
                        name: field.name.to_string(),
                        weight: field.weight,
                        tf_score: field.tf_score,
                        adjacency_score: field.adjacency_score,
                        words: field
                            .word_details
                            .iter()
                            .map(|(word, tf, ln_score)| DebugSearchWord {
                                word: word.clone(),
                                tf: *tf,
                                ln_score: *ln_score,
                            })
                            .collect(),
                    })
                    .collect(),
            }
        })
        .collect();
    DebugSearchReport {
        intent: debug.parsed.unquoted().to_string(),
        literals: debug
            .parsed
            .literals()
            .iter()
            .map(|literal| DebugLiteral {
                text: literal.text().to_string(),
                case_mode: literal.case_mode(),
            })
            .collect(),
        rows,
        total_results,
    }
}

// ---------------------------------------------------------------------------
// parse errors
// ---------------------------------------------------------------------------

/// One line of a parse error's context window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseErrorContextLine {
    /// 1-based line number in the JSONL file (as upstream printed it).
    pub line_number: usize,
    /// The full line.
    pub text: String,
    /// Whether this is the failing line.
    pub is_error: bool,
}

/// One malformed JSONL record with its context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseErrorEntry {
    pub line_number: usize,
    pub message: String,
    /// Up to two lines before, the failing line, up to two lines after.
    pub context: Vec<ParseErrorContextLine>,
}

/// The parse errors of one conversation (upstream wrote them to the debug
/// log after a `--debug` selection).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseErrorReport {
    pub file: PathBuf,
    pub errors: Vec<ParseErrorEntry>,
}

impl ParseErrorReport {
    /// The block upstream appended to `debug.log`
    /// (`debug_log::log_parse_errors`), with `timestamp` in its
    /// `%Y-%m-%d %H:%M:%S` format.
    pub fn to_log_block(&self, timestamp: &str) -> String {
        let mut out = format!("=== Parse Errors: {} ===\n", timestamp);
        out.push_str(&format!("File: {}\n", self.file.display()));
        out.push_str(&format!("Errors: {}\n\n", self.errors.len()));
        for error in &self.errors {
            out.push_str(&format!(
                "Line {}: {}\n\n",
                error.line_number, error.message
            ));
            for line in &error.context {
                let marker = if line.is_error { ">" } else { " " };
                out.push_str(&format!(
                    "{marker} {:>4} | {}\n",
                    line.line_number,
                    truncate_log_line(&line.text, 200)
                ));
            }
            out.push('\n');
        }
        out.push_str("---\n\n");
        out
    }
}

fn truncate_log_line(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        text.chars().take(max_chars).collect::<String>() + "..."
    }
}

fn parse_error_entry(error: &ParseError) -> ParseErrorEntry {
    let mut context = Vec::new();
    let before_start = error.line_number.saturating_sub(error.context_before.len());
    for (offset, text) in error.context_before.iter().enumerate() {
        context.push(ParseErrorContextLine {
            line_number: before_start + offset,
            text: text.clone(),
            is_error: false,
        });
    }
    context.push(ParseErrorContextLine {
        line_number: error.line_number,
        text: error.line_content.clone(),
        is_error: true,
    });
    for (offset, text) in error.context_after.iter().enumerate() {
        context.push(ParseErrorContextLine {
            line_number: error.line_number + 1 + offset,
            text: text.clone(),
            is_error: false,
        });
    }
    ParseErrorEntry {
        line_number: error.line_number,
        message: error.error_message.clone(),
        context,
    }
}

/// The conversation's parse errors, or `None` when it has none.
pub fn parse_error_report(conversation: &Conversation) -> Option<ParseErrorReport> {
    if conversation.parse_errors.is_empty() {
        return None;
    }
    Some(ParseErrorReport {
        file: conversation.path.clone(),
        errors: conversation
            .parse_errors
            .iter()
            .map(parse_error_entry)
            .collect(),
    })
}

/// Append the conversation's parse errors to the debug log
/// ([`HistoryEnv::debug_log_file`]) like upstream's `--debug` selection, and
/// return the info message (`Logged N parse error(s) to <path>`), or `None`
/// when there was nothing to log.
pub fn log_parse_errors(env: &HistoryEnv, conversation: &Conversation) -> Result<Option<String>> {
    if conversation.parse_errors.is_empty() {
        return Ok(None);
    }
    let log = env.debug_log_file().ok_or_else(|| {
        AppError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Could not determine home directory",
        ))
    })?;
    crate::debug_log::log_parse_errors(&log, conversation)?;
    Ok(Some(format!(
        "Logged {} parse error(s) to {}",
        conversation.parse_errors.len(),
        log.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::test_support::{assistant_jsonl_line, user_jsonl_line};

    struct Sandbox {
        root: tempfile::TempDir,
        env: HistoryEnv,
    }

    impl Sandbox {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let env = HistoryEnv::sandbox(root.path());
            Self { root, env }
        }

        fn project(&self, name: &str) -> PathBuf {
            let dir = self.env.claude_projects_root().unwrap().join(name);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }
    }

    fn write(path: &Path, lines: &[String]) {
        std::fs::write(path, lines.join("\n") + "\n").unwrap();
    }

    // --- upstream main.rs tests ---

    #[test]
    fn delete_empty_summary_line_reports_dry_run_count() {
        assert_eq!(
            delete_empty_summary_line(false, 7),
            "Summary: 7 empty transcript(s) would be deleted."
        );
    }

    #[test]
    fn delete_empty_summary_line_reports_deleted_count() {
        assert_eq!(
            delete_empty_summary_line(true, 7),
            "Summary: deleted 7 empty transcript(s)."
        );
    }

    #[test]
    fn preview_truncation_matches_upstream() {
        let long = "é".repeat(121);
        assert_eq!(
            truncate_delete_empty_preview(&long),
            format!("{}...", "é".repeat(120))
        );
        assert_eq!(truncate_delete_empty_preview("short"), "short");
        assert_eq!(
            truncate_delete_empty_preview(&"x".repeat(120)),
            "x".repeat(120)
        );
    }

    #[test]
    fn delete_empty_candidates_report_and_delete() {
        let sandbox = Sandbox::new();
        let project = sandbox.project("-tmp-proj");
        let empty = project.join("aaaa-empty.jsonl");
        write(&empty, &[user_jsonl_line("/status   please")]);
        std::fs::create_dir_all(project.join("aaaa-empty").join("tool-results")).unwrap();
        let metadata_only = project.join("bbbb-meta.jsonl");
        write(
            &metadata_only,
            &[r#"{"type":"summary","summary":"x"}"#.to_string()],
        );
        let full = project.join("cccc-full.jsonl");
        write(&full, &[user_jsonl_line("q"), assistant_jsonl_line("a")]);
        std::fs::write(project.join("agent-x.jsonl"), user_jsonl_line("q")).unwrap();

        let candidates = find_empty_candidates(&sandbox.env, &DeleteEmptyScope::All).unwrap();
        assert_eq!(
            candidates
                .iter()
                .map(|c| c.path().to_path_buf())
                .collect::<Vec<_>>(),
            vec![empty.clone(), metadata_only.clone()]
        );
        let first = &candidates[0];
        assert_eq!(first.session_id(), "aaaa-empty");
        assert_eq!(first.size, std::fs::metadata(&empty).unwrap().len());
        assert!(first.modified.is_some());
        assert!(first.modified_within(Local::now(), chrono::Duration::minutes(5)));
        assert!(!first.modified_within(
            Local::now() + chrono::Duration::hours(1),
            chrono::Duration::minutes(5)
        ));
        assert_eq!(first.artifact_dirs, vec![project.join("aaaa-empty")]);
        assert!(candidates[1].artifact_dirs.is_empty());

        let transcripts: Vec<_> = candidates.iter().map(|c| c.transcript.clone()).collect();
        let project_name = first.project().to_string();
        let dry_run = format_delete_empty_report(&transcripts, None);
        assert_eq!(
            dry_run,
            format!(
                "Found 2 empty transcript(s). Re-run with --yes to delete:\n\
                 aaaa-empty\t{project_name}\tusers=1\tlines=1\t/status please\n  {}\n\
                 bbbb-meta\t{project_name}\tusers=0\tlines=1\t(no user messages)\n  {}\n\
                 Summary: 2 empty transcript(s) would be deleted.\n",
                empty.display(),
                metadata_only.display()
            )
        );
        assert_eq!(first.cli_lines()[1], format!("  {}", empty.display()));

        let outcome = delete_empty_candidates(&candidates);
        assert!(outcome.failure.is_none());
        assert_eq!(outcome.deleted, vec![empty.clone(), metadata_only.clone()]);
        assert!(!empty.exists() && !metadata_only.exists());
        assert!(!project.join("aaaa-empty").exists());
        assert!(full.exists());
        assert_eq!(
            outcome
                .deleted_transcripts(&candidates)
                .into_iter()
                .map(|transcript| transcript.path)
                .collect::<Vec<_>>(),
            vec![empty.clone(), metadata_only.clone()]
        );
        let report = format_delete_empty_report(
            &outcome.deleted_transcripts(&candidates),
            Some(outcome.deleted_count()),
        );
        assert!(
            report.starts_with("Deleted 2 empty transcript(s):\n"),
            "{report}"
        );
        assert!(
            report.ends_with("Summary: deleted 2 empty transcript(s).\n"),
            "{report}"
        );

        // Deleting again skips the (now missing) files.
        let outcome = delete_empty_candidates(&candidates);
        assert!(outcome.deleted.is_empty());
        assert!(outcome.failure.is_none());
        assert_eq!(
            outcome.skipped,
            vec![
                SkippedCandidate {
                    path: empty.clone(),
                    reason: SkipReason::Missing
                },
                SkippedCandidate {
                    path: metadata_only.clone(),
                    reason: SkipReason::Missing
                },
            ]
        );
        // The summary agrees with the header after a partial (here: empty)
        // delete; it used to count the list ("Deleted 0 ... deleted 2").
        let report = format_delete_empty_report(&transcripts, Some(outcome.deleted_count()));
        assert!(
            report.starts_with("Deleted 0 empty transcript(s):\n"),
            "{report}"
        );
        assert!(
            report.ends_with("Summary: deleted 0 empty transcript(s).\n"),
            "{report}"
        );
        assert!(outcome.deleted_transcripts(&candidates).is_empty());

        assert_eq!(
            format_delete_empty_report(&[], None),
            "No empty transcripts found.\nSummary: 0 empty transcript(s) would be deleted.\n"
        );
        assert_eq!(
            format_delete_empty_report(&[], Some(0)),
            "No empty transcripts found.\nSummary: deleted 0 empty transcript(s).\n"
        );
    }

    #[test]
    fn delete_empty_keeps_transcripts_that_changed_since_the_scan() {
        let sandbox = Sandbox::new();
        let project = sandbox.project("-tmp-proj");
        // A brand-new session: the prompt is written, the reply is not.
        let live = project.join("aaaa-live.jsonl");
        write(&live, &[user_jsonl_line("new prompt")]);
        let live_dir = project.join("aaaa-live").join("tool-results");
        std::fs::create_dir_all(&live_dir).unwrap();
        let stale = project.join("bbbb-stale.jsonl");
        write(&stale, &[user_jsonl_line("abandoned")]);

        let candidates = find_empty_candidates(&sandbox.env, &DeleteEmptyScope::All).unwrap();
        assert_eq!(candidates.len(), 2);

        // The reply arrives while the confirmation dialog is open.
        write(
            &live,
            &[user_jsonl_line("new prompt"), assistant_jsonl_line("reply")],
        );
        assert!(
            find_empty_candidates(&sandbox.env, &DeleteEmptyScope::All)
                .unwrap()
                .iter()
                .all(|candidate| candidate.path() != live)
        );

        let outcome = delete_empty_candidates(&candidates);
        assert!(outcome.failure.is_none());
        assert_eq!(outcome.deleted, vec![stale.clone()]);
        assert_eq!(
            outcome.skipped,
            vec![SkippedCandidate {
                path: live.clone(),
                reason: SkipReason::Changed
            }]
        );
        assert!(live.exists() && live_dir.exists());
        assert!(!stale.exists());

        // The history-level delete re-reads the file even when size and
        // mtime were not compared.
        let transcript = candidates[0].transcript.clone();
        assert_eq!(transcript.path, live);
        assert_eq!(
            history::delete_empty_transcript(&transcript).unwrap(),
            history::EmptyTranscriptDeletion::NoLongerEmpty
        );
        assert_eq!(
            history::delete_transcripts(std::slice::from_ref(&transcript)).unwrap(),
            0
        );
        assert!(live.exists() && live_dir.exists());
    }

    #[test]
    fn delete_empty_never_deletes_directories_named_by_dot_stems() {
        let sandbox = Sandbox::new();
        let root = sandbox.env.claude_projects_root().unwrap();
        let project = sandbox.project("C--work-a");
        let other = sandbox.project("C--work-b");
        // `...jsonl` has the stem `..` and `..jsonl` the stem `.`.
        let dots3 = project.join("...jsonl");
        let dots2 = project.join("..jsonl");
        std::fs::write(&dots3, "").unwrap();
        std::fs::write(&dots2, "").unwrap();
        let kept = project.join("aaaa-kept.jsonl");
        write(&kept, &[user_jsonl_line("q"), assistant_jsonl_line("a")]);
        let other_session = other.join("2222-other.jsonl");
        write(
            &other_session,
            &[user_jsonl_line("q"), assistant_jsonl_line("a")],
        );
        std::fs::create_dir_all(other.join("2222-other").join("subagents")).unwrap();

        let candidates = find_empty_candidates(&sandbox.env, &DeleteEmptyScope::All).unwrap();
        let ids: Vec<&str> = candidates.iter().map(|c| c.session_id()).collect();
        assert_eq!(ids.len(), 2, "{ids:?}");
        assert!(ids.contains(&"..") && ids.contains(&"."), "{ids:?}");
        assert!(
            candidates.iter().all(|c| c.artifact_dirs.is_empty()),
            "{candidates:?}"
        );

        let outcome = delete_empty_candidates(&candidates);
        assert!(outcome.failure.is_none(), "{:?}", outcome.failure);
        assert_eq!(outcome.deleted_count(), 2);
        assert!(!dots3.exists() && !dots2.exists());
        assert!(root.is_dir() && project.is_dir() && other.is_dir());
        assert!(kept.exists() && other_session.exists());
        assert!(other.join("2222-other").join("subagents").is_dir());

        // The rule also guards arbitrary transcripts passed to the history API.
        assert_eq!(history::session_artifact_dir(&dots3, ".."), None);
        assert_eq!(history::session_artifact_dir(&dots2, "."), None);
        assert_eq!(history::session_artifact_dir(&kept, "bbbb-other"), None);
        assert_eq!(history::session_artifact_dir(&kept, ""), None);
        assert_eq!(
            history::session_artifact_dir(&kept, "aaaa-kept"),
            Some(project.join("aaaa-kept"))
        );
        let forged = project.join("forged.jsonl");
        std::fs::write(&forged, "").unwrap();
        let transcript = EmptyTranscript {
            path: forged.clone(),
            session_id: "..".to_string(),
            project_name: "p".to_string(),
            user_messages: 0,
            line_count: 0,
            preview: None,
        };
        assert_eq!(history::delete_transcripts(&[transcript]).unwrap(), 1);
        assert!(!forged.exists());
        assert!(root.is_dir() && project.is_dir() && other.is_dir());
    }

    #[test]
    fn delete_empty_local_scope_uses_the_workspace_project() {
        let sandbox = Sandbox::new();
        let workspace = sandbox.root.path().join("work space");
        let scope = WorkspaceScope::new(&workspace).unwrap();
        let local = sandbox.project(scope.project_dir_name());
        let other = sandbox.project("-other");
        write(&local.join("l.jsonl"), &[user_jsonl_line("q")]);
        write(&other.join("o.jsonl"), &[user_jsonl_line("q")]);

        let candidates =
            find_empty_candidates(&sandbox.env, &DeleteEmptyScope::Local(scope.clone())).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].path(), local.join("l.jsonl"));
        assert_eq!(candidates[0].project(), workspace.display().to_string());

        let missing = WorkspaceScope::new(sandbox.root.path().join("nothing-here")).unwrap();
        assert!(
            find_empty_candidates(&sandbox.env, &DeleteEmptyScope::Local(missing))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            find_empty_candidates(&sandbox.env, &DeleteEmptyScope::All)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn delete_empty_without_projects_dir_is_an_error() {
        let sandbox = Sandbox::new();
        assert!(matches!(
            find_empty_candidates(&sandbox.env, &DeleteEmptyScope::All),
            Err(AppError::ProjectsDirNotFound(_))
        ));
    }

    #[test]
    fn delete_by_uuid_lists_and_removes_every_copy() {
        let sandbox = Sandbox::new();
        let uuid = "12345678-1234-4234-9234-123456789abc";
        let first = sandbox.project("-a");
        let second = sandbox.project("-b");
        for project in [&first, &second] {
            write(
                &project.join(format!("{uuid}.jsonl")),
                &[user_jsonl_line("q")],
            );
        }
        std::fs::create_dir_all(first.join(uuid).join("subagents")).unwrap();

        let mut copies = find_session_copies(&sandbox.env, uuid).unwrap();
        copies.sort_by(|a, b| a.jsonl.cmp(&b.jsonl));
        assert_eq!(
            copies,
            vec![
                SessionCopyFiles {
                    jsonl: first.join(format!("{uuid}.jsonl")),
                    project_dir_name: "-a".to_string(),
                    session_dir: Some(first.join(uuid)),
                },
                SessionCopyFiles {
                    jsonl: second.join(format!("{uuid}.jsonl")),
                    project_dir_name: "-b".to_string(),
                    session_dir: None,
                },
            ]
        );

        let report = delete_session_by_uuid(&sandbox.env, uuid).unwrap();
        assert_eq!(report.deleted, 2);
        assert_eq!(
            report.message,
            format!("Deleted session {uuid} (2 copies across projects)")
        );
        assert!(!first.join(uuid).exists());
        assert!(matches!(
            find_session_copies(&sandbox.env, uuid),
            Err(AppError::SessionNotFound(_))
        ));
        assert!(matches!(
            delete_session_by_uuid(&sandbox.env, uuid),
            Err(AppError::SessionNotFound(_))
        ));
        for invalid in ["", "../x", "a b", "x/y"] {
            assert!(matches!(
                find_session_copies(&sandbox.env, invalid),
                Err(AppError::SessionNotFound(_))
            ));
        }
        assert_eq!(delete_by_uuid_message("id", 1), "Deleted session id");
    }

    // --- K4: plan, then execute ---

    #[test]
    fn claude_delete_plan_lists_every_copy_and_session_dir_then_executes_it() {
        let sandbox = Sandbox::new();
        let uuid = "12345678-1234-4234-9234-123456789abc";
        let other = "87654321-4321-4321-8321-cba987654321";
        let first = sandbox.project("-a");
        let second = sandbox.project("-b");
        for project in [&first, &second] {
            write(
                &project.join(format!("{uuid}.jsonl")),
                &[user_jsonl_line("q")],
            );
        }
        write(
            &first.join(format!("{other}.jsonl")),
            &[user_jsonl_line("q")],
        );
        std::fs::create_dir_all(first.join(uuid).join("subagents")).unwrap();
        let listed = second.join(format!("{uuid}.jsonl"));

        let mut plan = plan_session_delete(&sandbox.env, &listed, Source::Claude).unwrap();
        plan.files.sort();
        assert_eq!(plan.source, Source::Claude);
        assert_eq!(plan.path, listed);
        assert_eq!(plan.session_id, uuid);
        assert_eq!(
            plan.files,
            vec![
                first.join(format!("{uuid}.jsonl")),
                second.join(format!("{uuid}.jsonl"))
            ]
        );
        assert_eq!(plan.dirs, vec![first.join(uuid)]);
        // Planning removes nothing.
        assert!(listed.exists() && first.join(uuid).exists());

        // A copy that vanished meanwhile is reported, not an error.
        std::fs::remove_file(first.join(format!("{uuid}.jsonl"))).unwrap();
        let report = execute_session_delete(&sandbox.env, &plan).unwrap();
        assert_eq!(report.removed_files, vec![listed.clone()]);
        assert_eq!(report.missing, vec![first.join(format!("{uuid}.jsonl"))]);
        assert_eq!(report.removed_dirs, vec![first.join(uuid)]);
        assert!(!listed.exists() && !first.join(uuid).exists());
        assert!(
            first.join(format!("{other}.jsonl")).exists(),
            "only plan items"
        );

        assert!(matches!(
            plan_session_delete(&sandbox.env, &listed, Source::Claude),
            Err(AppError::SessionNotFound(_))
        ));
        for invalid in ["a b.jsonl", "..jsonl", "x$.jsonl"] {
            assert!(
                matches!(
                    plan_session_delete(&sandbox.env, &first.join(invalid), Source::Claude),
                    Err(AppError::SessionNotFound(_))
                ),
                "{invalid}"
            );
        }
    }

    #[test]
    fn a_tampered_claude_plan_is_refused_before_anything_is_removed() {
        let sandbox = Sandbox::new();
        let uuid = "12345678-1234-4234-9234-123456789abc";
        let project = sandbox.project("-a");
        let transcript = project.join(format!("{uuid}.jsonl"));
        let bystander = project.join("notes.jsonl");
        write(&transcript, &[user_jsonl_line("q")]);
        write(&bystander, &[user_jsonl_line("q")]);
        // Same names outside the projects root.
        let outside = sandbox.root.path().join("elsewhere");
        let outside_transcript = outside.join(format!("{uuid}.jsonl"));
        let outside_dir = outside.join(uuid);
        std::fs::create_dir_all(&outside_dir).unwrap();
        write(&outside_transcript, &[user_jsonl_line("q")]);
        let projects_root = sandbox.env.claude_projects_root().unwrap();
        let in_root = projects_root.join(format!("{uuid}.jsonl"));
        write(&in_root, &[user_jsonl_line("q")]);
        let other_project_dir = sandbox.project("-other").join(uuid);
        std::fs::create_dir_all(&other_project_dir).unwrap();
        let plan = |files: Vec<PathBuf>, dirs: Vec<PathBuf>| DeletePlan {
            source: Source::Claude,
            path: transcript.clone(),
            session_id: uuid.to_string(),
            files,
            dirs,
        };
        for plan in [
            plan(vec![transcript.clone(), bystander.clone()], Vec::new()),
            plan(vec![transcript.clone()], vec![project.clone()]),
            DeletePlan {
                session_id: "../x".to_string(),
                ..plan(Vec::new(), Vec::new())
            },
            plan(vec![outside_transcript.clone()], Vec::new()),
            plan(vec![transcript.clone()], vec![outside_dir.clone()]),
            // Directly in the projects root, or a `..` project.
            plan(vec![in_root.clone()], Vec::new()),
            plan(
                vec![
                    projects_root
                        .join("-a")
                        .join("..")
                        .join(format!("{uuid}.jsonl")),
                ],
                Vec::new(),
            ),
            // A session directory that is not beside a planned transcript.
            plan(vec![transcript.clone()], vec![other_project_dir.clone()]),
            // A Pi plan for a Claude transcript.
            DeletePlan {
                source: Source::Pi,
                ..plan(vec![transcript.clone()], Vec::new())
            },
        ] {
            assert!(
                matches!(
                    execute_session_delete(&sandbox.env, &plan),
                    Err(failure) if matches!(failure.error, AppError::SessionNotFound(_))
                ),
                "{plan:?}"
            );
        }
        assert!(transcript.exists() && bystander.exists());
        assert!(outside_transcript.exists() && outside_dir.exists());
        assert!(in_root.exists() && other_project_dir.exists());
    }

    #[test]
    fn a_tampered_pi_or_omp_plan_is_refused_before_anything_is_removed() {
        let sandbox = Sandbox::new();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let dir = sandbox.root.path().join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        let pi = dir.join("pi.jsonl");
        std::fs::copy(fixtures.join("pi/v1.jsonl"), &pi).unwrap();
        let omp = dir.join("omp.jsonl");
        std::fs::copy(fixtures.join("omp/v3.jsonl"), &omp).unwrap();
        let artifacts = dir.join("omp");
        std::fs::create_dir_all(&artifacts).unwrap();
        let unrelated = sandbox.root.path().join("unrelated");
        std::fs::create_dir_all(unrelated.join("inner")).unwrap();
        std::fs::write(unrelated.join("inner").join("keep.txt"), b"keep").unwrap();
        let notes = dir.join("notes.jsonl");
        std::fs::write(&notes, "{}\n").unwrap();

        let pi_plan = plan_session_delete(&sandbox.env, &pi, Source::Pi).unwrap();
        let omp_plan = plan_session_delete(&sandbox.env, &omp, Source::Omp).unwrap();
        assert_eq!(pi_plan.path, pi);
        assert_eq!(omp_plan.path, omp);
        for plan in [
            // An arbitrary directory in an OMP plan without files.
            DeletePlan {
                source: Source::Omp,
                path: omp.clone(),
                session_id: "whatever".to_string(),
                files: Vec::new(),
                dirs: vec![unrelated.clone()],
            },
            DeletePlan {
                dirs: vec![unrelated.clone()],
                ..omp_plan.clone()
            },
            DeletePlan {
                dirs: vec![artifacts.clone(), artifacts.clone()],
                ..omp_plan.clone()
            },
            DeletePlan {
                dirs: vec![artifacts.clone()],
                ..pi_plan.clone()
            },
            // Not the listed file, not a session, more than one file.
            DeletePlan {
                files: vec![notes.clone()],
                ..pi_plan.clone()
            },
            DeletePlan {
                path: notes.clone(),
                files: vec![notes.clone()],
                ..pi_plan.clone()
            },
            DeletePlan {
                files: vec![pi.clone(), notes.clone()],
                ..pi_plan.clone()
            },
            // An OMP session in a Pi plan.
            DeletePlan {
                source: Source::Pi,
                dirs: Vec::new(),
                ..omp_plan.clone()
            },
        ] {
            assert!(
                matches!(
                    execute_session_delete(&sandbox.env, &plan),
                    Err(failure) if matches!(failure.error, AppError::SessionNotFound(_))
                ),
                "{plan:?}"
            );
        }
        assert!(unrelated.join("inner").join("keep.txt").exists());
        assert!(pi.exists() && omp.exists() && artifacts.exists() && notes.exists());

        // The plans as made still run; a file gone meanwhile is missing.
        std::fs::remove_file(&pi).unwrap();
        let report = execute_session_delete(&sandbox.env, &pi_plan).unwrap();
        assert_eq!(report.missing, vec![pi]);
        let report = execute_session_delete(&sandbox.env, &omp_plan).unwrap();
        assert_eq!(report.removed_files, vec![omp]);
        assert_eq!(report.removed_dirs, vec![artifacts]);
    }

    #[test]
    fn pi_and_omp_delete_plans_validate_the_file_and_include_omp_artifacts() {
        let sandbox = Sandbox::new();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let dir = sandbox.root.path().join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        let pi = dir.join("pi.jsonl");
        std::fs::copy(fixtures.join("pi/v1.jsonl"), &pi).unwrap();
        let omp = dir.join("omp.jsonl");
        std::fs::copy(fixtures.join("omp/v3.jsonl"), &omp).unwrap();
        let artifacts = dir.join("omp");
        std::fs::create_dir_all(artifacts.join("tool")).unwrap();

        let pi_plan = plan_session_delete(&sandbox.env, &pi, Source::Pi).unwrap();
        assert_eq!(pi_plan.session_id, "custom_v1_id");
        assert_eq!(pi_plan.files, vec![pi.clone()]);
        assert!(pi_plan.dirs.is_empty());
        let omp_plan = plan_session_delete(&sandbox.env, &omp, Source::Omp).unwrap();
        assert_eq!(omp_plan.session_id, "omp_session_custom_id");
        assert_eq!(omp_plan.files, vec![omp.clone()]);
        assert_eq!(omp_plan.dirs, vec![artifacts.clone()]);
        let not_a_session = dir.join("notes.jsonl");
        std::fs::write(&not_a_session, "{}\n").unwrap();
        assert!(matches!(
            plan_session_delete(&sandbox.env, &not_a_session, Source::Pi),
            Err(AppError::SessionNotFound(_))
        ));

        let report = execute_session_delete(&sandbox.env, &pi_plan).unwrap();
        assert_eq!(report.removed_files, vec![pi.clone()]);
        let report = execute_session_delete(&sandbox.env, &omp_plan).unwrap();
        assert_eq!(report.removed_files, vec![omp.clone()]);
        assert_eq!(report.removed_dirs, vec![artifacts.clone()]);
        assert!(!pi.exists() && !omp.exists() && !artifacts.exists());
        assert!(not_a_session.exists());
        // Executing again: everything is missing, nothing fails.
        let report = execute_session_delete(&sandbox.env, &omp_plan).unwrap();
        assert_eq!(report.missing, vec![omp, artifacts]);
    }

    #[test]
    fn a_refused_plan_reports_an_empty_partial_report() {
        let sandbox = Sandbox::new();
        let uuid = "12345678-1234-4234-9234-123456789abc";
        let listed = sandbox.project("-a").join(format!("{uuid}.jsonl"));
        write(&listed, &[user_jsonl_line("q")]);
        let mut plan = plan_session_delete(&sandbox.env, &listed, Source::Claude).unwrap();
        plan.session_id = "not-a-uuid".to_string();
        let failure = execute_session_delete(&sandbox.env, &plan).unwrap_err();
        assert_eq!(failure.partial, DeleteReport::default());
        assert!(listed.exists());
        let _: AppError = failure.into();
    }

    #[test]
    fn a_failure_in_the_dirs_loop_keeps_files_dirs_and_missing_in_the_partial_report() {
        let sandbox = Sandbox::new();
        let uuid = "12345678-1234-4234-9234-123456789abc";
        let listed = sandbox.project("-a").join(format!("{uuid}.jsonl"));
        write(&listed, &[user_jsonl_line("q")]);
        // A copy that vanished meanwhile.
        let gone = sandbox.project("-c").join(format!("{uuid}.jsonl"));
        let removable = sandbox.project("-a").join(uuid);
        std::fs::create_dir_all(&removable).unwrap();
        write(&removable.join("tool.txt"), &["x".to_string()]);
        // A file where a session directory should be: remove_dir_all fails.
        let blocked = sandbox.project("-b").join(uuid);
        write(
            &sandbox.project("-b").join(format!("{uuid}.jsonl")),
            &[user_jsonl_line("q")],
        );
        write(&blocked, &["not a directory".to_string()]);
        let mut plan = plan_session_delete(&sandbox.env, &listed, Source::Claude).unwrap();
        plan.files.push(gone.clone());
        plan.dirs = vec![removable.clone(), blocked.clone()];

        let failure = execute_session_delete(&sandbox.env, &plan).unwrap_err();
        assert!(
            plan.files
                .iter()
                .all(|file| failure.partial.removed_files.contains(file) || *file == gone)
        );
        assert_eq!(failure.partial.removed_dirs, vec![removable.clone()]);
        assert_eq!(failure.partial.missing, vec![gone]);
        assert!(!removable.exists() && blocked.exists());
    }

    #[test]
    fn a_failed_delete_writes_the_upstream_debug_log_line() {
        // Upstream (tui/runtime.rs) logs the listed conversation's path,
        // also when another copy is the item that cannot be removed.
        let sandbox = Sandbox::new();
        let uuid = "12345678-1234-4234-9234-123456789abc";
        let listed = sandbox.project("-a").join(format!("{uuid}.jsonl"));
        write(&listed, &[user_jsonl_line("q")]);
        // A directory where a copy should be cannot be removed with
        // remove_file.
        let blocked = sandbox.project("-b").join(format!("{uuid}.jsonl"));
        std::fs::create_dir_all(&blocked).unwrap();
        let mut plan = plan_session_delete(&sandbox.env, &listed, Source::Claude).unwrap();
        plan.files.sort();
        assert_eq!(plan.files, vec![listed.clone(), blocked.clone()]);

        let failure = execute_session_delete(&sandbox.env, &plan).unwrap_err();
        // The copy removed before the failure is reported with the error.
        assert_eq!(failure.partial.removed_files, vec![listed.clone()]);
        assert!(failure.partial.removed_dirs.is_empty() && failure.partial.missing.is_empty());
        assert!(!listed.exists());
        let error = failure.error;
        let log = std::fs::read_to_string(sandbox.env.debug_log_file().unwrap()).unwrap();
        let line = format!("Failed to delete session {}: {}", listed.display(), error);
        assert!(log.contains(&line), "{log}");
        assert!(!log.contains(&blocked.display().to_string()), "{log}");
        // The text the GUI shows for the same failure (DESIGN 11.9).
        let shown = crate::browser::DeleteError::Failed {
            path: listed.clone(),
            error,
        };
        assert_eq!(shown.to_string(), line);
        assert!(blocked.exists());
    }

    #[test]
    fn show_dir_needs_a_workspace() {
        let sandbox = Sandbox::new();
        let (dir, with_trailing_separator, encoded) = if cfg!(windows) {
            (
                r"C:\Users\me\code\proj",
                r"C:\Users\me\code\proj\",
                "C--Users-me-code-proj",
            )
        } else {
            (
                "/Users/me/code/proj",
                "/Users/me/code/proj/",
                "-Users-me-code-proj",
            )
        };
        let workspace = WorkspaceScope::new(dir).unwrap();
        assert_eq!(
            show_dir(&sandbox.env, Some(&workspace)).unwrap(),
            sandbox.env.claude_projects_root().unwrap().join(encoded)
        );
        assert_eq!(
            projects_dir_for(&sandbox.env, Path::new(dir)).unwrap(),
            show_dir(&sandbox.env, Some(&workspace)).unwrap()
        );
        // A trailing separator names the same projects directory.
        let trailing = WorkspaceScope::new(with_trailing_separator).unwrap();
        assert_eq!(
            show_dir(&sandbox.env, Some(&trailing)).unwrap(),
            show_dir(&sandbox.env, Some(&workspace)).unwrap()
        );
        assert_eq!(
            projects_dir_for(&sandbox.env, Path::new(with_trailing_separator)).unwrap(),
            show_dir(&sandbox.env, Some(&workspace)).unwrap()
        );
        let error = show_dir(&sandbox.env, None).unwrap_err();
        assert_eq!(error.to_string(), WORKSPACE_REQUIRED_SHOW_DIR);
    }

    fn conversation(path: &str, text: &str) -> Conversation {
        crate::semantic::test_fixtures::SemanticConversationFixture::new(path, Vec::<String>::new())
            .with_title(text)
            .with_project("proj", "/projects/proj")
            .with_normalized_search_text_lower()
            .build()
    }

    #[test]
    fn debug_search_report_matches_upstream_format() {
        let now = Local::now();
        let mut older = conversation("/p/-proj/aaa.jsonl", "alpha beta gamma");
        older.timestamp = now - chrono::Duration::days(3);
        let mut newer = conversation("/p/-proj/bbb.jsonl", "alpha only");
        newer.timestamp = now - chrono::Duration::hours(2);
        let miss = conversation("/p/-other/ccc.jsonl", "nothing relevant");
        let conversations = vec![newer, older, miss];

        let report = debug_search_report(&conversations, "alpha \"Beta\"", now, None);
        assert_eq!(report.intent, "alpha");
        assert_eq!(
            report.literals,
            vec![DebugLiteral {
                text: "Beta".to_string(),
                case_mode: CaseMode::Sensitive
            }]
        );
        let text = report.to_text();
        assert!(
            text.starts_with("intent: \"alpha\"\nliterals:\n  \"Beta\" (Sensitive)\n\n"),
            "{text}"
        );
        for row in &report.rows {
            assert_eq!(conversations[row.conversation_index].path, row.path);
            assert!(text.contains(&format!(
                "#{:2} score={:.2} freshness={:.2} | proj | {} | {} ago\n",
                row.rank, row.score, row.freshness, row.session, row.age_label
            )));
        }

        let plain = debug_search_report(&conversations, "alpha", now, None);
        assert_eq!(plain.total_results, 2);
        assert_eq!(plain.rows.len(), 2);
        assert!(
            plain
                .to_text()
                .starts_with("intent: \"alpha\"\nliterals: none\n\n")
        );
        let ages: Vec<&str> = plain
            .rows
            .iter()
            .map(|row| row.age_label.as_str())
            .collect();
        assert!(ages.contains(&"2h") && ages.contains(&"3d"), "{ages:?}");
        let contributing = plain.rows[0]
            .fields
            .iter()
            .filter(|field| field.contributes())
            .count();
        assert!(contributing > 0);
        assert!(plain.to_text().contains("\"alpha\" tf="));

        // The workspace restricts to its (worktree-aware) project.
        let workspace = WorkspaceScope::from_parts("/nowhere/proj", "-other");
        let local = debug_search_report(&conversations, "nothing", now, Some(&workspace));
        assert_eq!(local.rows.len(), 1);
        assert_eq!(local.rows[0].session, "ccc");
        assert!(
            debug_search_report(&conversations, "alpha", now, Some(&workspace))
                .rows
                .is_empty()
        );
    }

    #[test]
    fn debug_age_labels() {
        assert_eq!(format_debug_age(chrono::Duration::hours(23)), "23h");
        assert_eq!(format_debug_age(chrono::Duration::hours(24)), "1d");
        assert_eq!(format_debug_age(chrono::Duration::hours(50)), "2d");
    }

    #[test]
    fn pi_conversations_match_the_workspace_by_directory() {
        let root = tempfile::tempdir().unwrap();
        let mut pi = conversation("/sessions/x.jsonl", "pi");
        pi.source = Source::Pi;
        pi.project_path = Some(root.path().to_path_buf());
        assert!(conversation_in_workspace(
            &pi,
            &WorkspaceScope::new(root.path()).unwrap()
        ));
        assert!(!conversation_in_workspace(
            &pi,
            &WorkspaceScope::new(root.path().join("sub")).unwrap()
        ));
    }

    #[test]
    fn parse_error_report_matches_the_debug_log_block() {
        let sandbox = Sandbox::new();
        let mut conversation = conversation("session.jsonl", "x");
        conversation.parse_errors = vec![ParseError {
            line_number: 3,
            line_content: "x".repeat(250),
            error_message: "expected value".to_string(),
            context_before: vec!["a".to_string(), "b".to_string()],
            context_after: vec!["d".to_string()],
        }];

        let report = parse_error_report(&conversation).unwrap();
        assert_eq!(report.errors.len(), 1);
        let context: Vec<(usize, bool)> = report.errors[0]
            .context
            .iter()
            .map(|line| (line.line_number, line.is_error))
            .collect();
        assert_eq!(context, vec![(1, false), (2, false), (3, true), (4, false)]);

        let message = log_parse_errors(&sandbox.env, &conversation)
            .unwrap()
            .unwrap();
        let log = sandbox.env.debug_log_file().unwrap();
        assert_eq!(
            message,
            format!("Logged 1 parse error(s) to {}", log.display())
        );
        let written = std::fs::read_to_string(&log).unwrap();
        let timestamp = written
            .lines()
            .next()
            .unwrap()
            .trim_start_matches("=== Parse Errors: ")
            .trim_end_matches(" ===");
        assert_eq!(written, report.to_log_block(timestamp));

        conversation.parse_errors.clear();
        assert!(parse_error_report(&conversation).is_none());
        assert_eq!(log_parse_errors(&sandbox.env, &conversation).unwrap(), None);
    }

    #[test]
    fn load_sorted_conversations_filters_and_sorts() {
        let sandbox = Sandbox::new();
        let project = sandbox.project("-tmp-p");
        write(
            &project.join("old.jsonl"),
            &[user_jsonl_line("old"), assistant_jsonl_line("a")],
        );
        write(
            &project.join("new.jsonl"),
            &[user_jsonl_line("new"), assistant_jsonl_line("a")],
        );
        let old = std::fs::File::options()
            .write(true)
            .open(project.join("old.jsonl"))
            .unwrap();
        old.set_modified(
            std::time::SystemTime::now() - std::time::Duration::from_secs(86_400 * 10),
        )
        .unwrap();
        drop(old);

        let all =
            load_sorted_conversations(&sandbox.env, true, None, &TimeFilter::default()).unwrap();
        let stems: Vec<_> = all
            .iter()
            .map(|c| c.path.file_stem().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(stems, vec!["new", "old"]);

        let recent = crate::agent::args::TimeRangeArgs {
            since: Some("2d".parse().unwrap()),
            ..Default::default()
        }
        .resolve()
        .unwrap();
        let filtered = load_sorted_conversations(&sandbox.env, true, None, &recent).unwrap();
        assert_eq!(filtered.len(), 1);
    }
}
