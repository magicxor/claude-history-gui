//! Conversation loading and project discovery.
//!
//! This module handles loading conversations from Claude project directories,
//! both synchronously and via streaming for the TUI.

use super::messages::parts_count_as_message;
use super::model::{EventKind, Part, Role, SourceKind};
use super::parser::process_conversation_file;
use super::path::{
    decode_project_dir_name, decode_project_dir_name_to_path, format_short_name_from_path,
};
use super::sources::Registry;
use super::{Conversation, LoaderMessage, Project};
use crate::agent::transcript::bounded_tool_result_text;
use crate::debug;
use crate::debug::DebugLevel;
use crate::env::{HistoryEnv, WorkspaceScope};
use crate::error::{AppError, Result};
use crate::text_match::nfc;
use crate::time_filter::TimeFilter;
use rayon::prelude::*;
use std::fs::read_dir;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::SystemTime;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum DeleteEmptyScope {
    All,
    /// Only the Claude project directory of this workspace (upstream used
    /// the process cwd).
    Local(WorkspaceScope),
}

#[derive(Debug, Clone)]
pub struct EmptyTranscript {
    pub path: PathBuf,
    pub session_id: String,
    pub project_name: String,
    pub user_messages: usize,
    pub line_count: usize,
    pub preview: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DeleteEmptySummary {
    pub candidates: Vec<EmptyTranscript>,
    pub deleted: usize,
}

/// Start loading all conversations in the background
/// Returns a receiver that will receive LoaderMessage updates
///
/// Fork note: kept for the in-memory fallback (no store), which upstream
/// 358becb removed together with [`LoaderMessage`]. Claude projects stream
/// one batch per project, as upstream's loader did; every other instance
/// the env configures (Pi, OMP and, since upstream 9597669, Codex) is
/// parsed through the registry like upstream's `store::load_in_memory`
/// and sent in batches of [`AUXILIARY_BATCH`], newest first.
pub fn load_all_conversations_streaming(
    env: Arc<HistoryEnv>,
    show_last: bool,
    debug_level: Option<DebugLevel>,
    time: TimeFilter,
) -> Receiver<LoaderMessage> {
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        load_all_streaming_inner(&env, tx, show_last, debug_level, time);
    });

    rx
}

/// How many conversations of the instances other than Claude's one
/// [`LoaderMessage::Batch`] holds.
const AUXILIARY_BATCH: usize = 32;

fn load_all_streaming_inner(
    env: &HistoryEnv,
    tx: Sender<LoaderMessage>,
    show_last: bool,
    debug_level: Option<DebugLevel>,
    time: TimeFilter,
) {
    thread::scope(|scope| {
        let claude = scope.spawn(|| load_claude_streaming(env, &tx, show_last, debug_level, time));
        let mut paths = std::collections::HashSet::new();
        let mut emit = |mut conversations: Vec<Conversation>| {
            conversations.retain(|conversation| {
                let path = conversation
                    .path
                    .canonicalize()
                    .unwrap_or_else(|_| conversation.path.clone());
                paths.insert(path) && (!time.is_active() || time.matches(conversation.timestamp))
            });
            if !conversations.is_empty() {
                let _ = tx.send(LoaderMessage::Batch(conversations));
            }
        };
        let auxiliary = Registry::from_env(env).filtered(|kind| kind != SourceKind::Claude);
        let auxiliary_usable = auxiliary
            .adapters()
            .iter()
            .any(|adapter| adapter.instance().root.exists());
        let mut conversations = crate::store::reconcile::parse_everything(&auxiliary);
        conversations.sort_by_key(|conversation| std::cmp::Reverse(conversation.timestamp));
        for conversation in &mut conversations {
            conversation.preview = if show_last {
                conversation.preview_last.clone()
            } else {
                conversation.preview_first.clone()
            };
        }
        let mut conversations = conversations.into_iter().peekable();
        while conversations.peek().is_some() {
            emit(conversations.by_ref().take(AUXILIARY_BATCH).collect());
        }

        let claude_result = claude.join().expect("Claude loader thread");
        if let Err(error) = claude_result
            && !auxiliary_usable
        {
            let _ = tx.send(LoaderMessage::Fatal(error));
            return;
        }
        let _ = tx.send(LoaderMessage::Done);
    });
}

fn load_claude_streaming(
    env: &HistoryEnv,
    tx: &Sender<LoaderMessage>,
    show_last: bool,
    debug_level: Option<DebugLevel>,
    time: TimeFilter,
) -> Result<()> {
    let root = env.claude_projects_root()?;
    if !root.exists() {
        return Err(AppError::ProjectsDirNotFound(root.display().to_string()));
    }
    let projects = list_projects(&root)?;
    debug::info(
        debug_level,
        &format!("Loading global history from {} projects", projects.len()),
    );

    // Process projects in parallel and send batches as they complete
    projects.par_iter().for_each(|project| {
        let project_dir = root.join(&project.name);

        match load_conversations(&project_dir, show_last, debug_level) {
            Ok(mut convs) => {
                if convs.is_empty() {
                    return;
                }

                let fallback_path = decode_project_dir_name_to_path(&project.name);

                for conv in &mut convs {
                    let project_path = conv.cwd.clone().unwrap_or_else(|| fallback_path.clone());
                    conv.project_name =
                        Some(nfc(&format_short_name_from_path(&project_path)).into_owned());
                    conv.project_path = Some(project_path);
                }

                // Only conversations in the time range are sent.
                if time.is_active() {
                    convs.retain(|conv| time.matches(conv.timestamp));
                    if convs.is_empty() {
                        return;
                    }
                }

                // Send batch, ignore error if receiver dropped
                let _ = tx.send(LoaderMessage::Batch(convs));
            }
            Err(e) => {
                debug::warn(
                    debug_level,
                    &format!("Failed to load project {}: {}", project.display_name, e),
                );
                let _ = tx.send(LoaderMessage::ProjectError);
            }
        }
    });

    Ok(())
}

/// Find a session JSONL file by UUID across all projects.
/// Returns the path to the `.jsonl` file if found.
pub fn find_jsonl_by_uuid(env: &HistoryEnv, uuid: &str) -> Result<Option<PathBuf>> {
    let matches = find_all_jsonl_by_uuid(env, uuid)?;
    Ok(matches.into_iter().next())
}

/// Find all session JSONL files by UUID across all projects.
/// A session may exist in multiple project directories due to cross-project forking.
pub fn find_all_jsonl_by_uuid(env: &HistoryEnv, uuid: &str) -> Result<Vec<PathBuf>> {
    let root = env.claude_projects_root()?;
    if !root.exists() {
        return Ok(Vec::new());
    }

    let filename = format!("{}.jsonl", uuid);
    let mut matches = Vec::new();

    for entry in read_dir(&root)? {
        let entry = entry?;
        let project_dir = entry.path();
        if !project_dir.is_dir() {
            continue;
        }
        let candidate = project_dir.join(&filename);
        if candidate.exists() {
            matches.push(candidate);
        }
    }

    Ok(matches)
}

/// Delete a session by UUID across all projects.
/// Removes both the .jsonl file and the session subdirectory (tool-results/,
/// subagents/), keeping the `/btw` forks among its subagents.
/// Returns the number of files deleted.
pub fn delete_session_by_uuid(env: &HistoryEnv, uuid: &str) -> Result<usize> {
    // Validate format to prevent path traversal
    if !is_safe_session_id(uuid) {
        return Err(AppError::SessionNotFound(uuid.to_owned()));
    }

    let matches = find_all_jsonl_by_uuid(env, uuid)?;
    if matches.is_empty() {
        return Err(AppError::SessionNotFound(uuid.to_owned()));
    }

    let count = matches.len();
    for jsonl_path in &matches {
        std::fs::remove_file(jsonl_path)?;

        // Also remove the session subdirectory if it exists
        if let Some(session_dir) = session_artifact_dir(jsonl_path, uuid)
            && session_dir.is_dir()
        {
            super::sources::claude::remove_session_dir(&session_dir)?;
        }
    }

    Ok(count)
}

pub fn delete_empty_transcripts(
    env: &HistoryEnv,
    scope: &DeleteEmptyScope,
    delete: bool,
) -> Result<DeleteEmptySummary> {
    let candidates = find_empty_transcripts(env, scope)?;
    let deleted = if delete {
        delete_transcripts(&candidates)?
    } else {
        0
    };

    Ok(DeleteEmptySummary {
        candidates,
        deleted,
    })
}

/// Delete the given transcripts (a list previously returned by
/// [`find_empty_transcripts`] and confirmed by the user) with
/// [`delete_empty_transcript`]: each one is re-checked first, and those that
/// are gone or no longer empty are kept and not counted. Returns the number
/// deleted; stops at the first error, like upstream.
pub fn delete_transcripts(transcripts: &[EmptyTranscript]) -> Result<usize> {
    let mut deleted = 0;
    for transcript in transcripts {
        if delete_empty_transcript(transcript)? == EmptyTranscriptDeletion::Deleted {
            deleted += 1;
        }
    }
    Ok(deleted)
}

/// What [`delete_empty_transcript`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmptyTranscriptDeletion {
    /// The transcript and its session directory were deleted.
    Deleted,
    /// The transcript has an assistant message now (for example a new
    /// session whose reply arrived after the scan); it was kept.
    NoLongerEmpty,
    /// The transcript does not exist any more.
    Missing,
}

/// Delete one empty transcript and its `<project>/<session id>/` directory
/// ([`session_artifact_dir`]; the `/btw` forks there are kept, upstream
/// 3c2ffeb), after re-reading it: a transcript that gained
/// an assistant message since the scan is kept. Upstream scanned and deleted
/// in one pass; a GUI shows the scan and deletes after a confirmation.
pub fn delete_empty_transcript(transcript: &EmptyTranscript) -> Result<EmptyTranscriptDeletion> {
    match empty_transcript_from_path(&transcript.path, &transcript.project_name) {
        Ok(Some(_)) => {}
        Ok(None) => return Ok(EmptyTranscriptDeletion::NoLongerEmpty),
        Err(AppError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(EmptyTranscriptDeletion::Missing);
        }
        Err(error) => return Err(error),
    }
    std::fs::remove_file(&transcript.path)?;
    if let Some(session_dir) = session_artifact_dir(&transcript.path, &transcript.session_id)
        && session_dir.is_dir()
    {
        super::sources::claude::remove_session_dir(&session_dir)?;
    }
    Ok(EmptyTranscriptDeletion::Deleted)
}

/// Whether `session_id` can name a session directory: non-empty ASCII
/// letters, digits and `-` (Claude session ids are UUIDs).
fn is_safe_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The `<project>/<session id>/` directory (tool results, subagents) that
/// belongs to the Claude transcript `<project>/<session id>.jsonl`.
///
/// `None` unless `session_id` is the transcript's file stem and passes the
/// session-id rule of [`delete_session_by_uuid`]: a stray file such as
/// `...jsonl` (stem `..`) or `..jsonl` (stem `.`) must never name the
/// projects root or the project directory.
pub fn session_artifact_dir(transcript: &Path, session_id: &str) -> Option<PathBuf> {
    if !is_safe_session_id(session_id)
        || transcript.file_stem() != Some(std::ffi::OsStr::new(session_id))
    {
        return None;
    }
    Some(transcript.parent()?.join(session_id))
}

/// Find Claude transcripts without any assistant message (dry run of
/// `delete-empty`). Sorted by path.
pub fn find_empty_transcripts(
    env: &HistoryEnv,
    scope: &DeleteEmptyScope,
) -> Result<Vec<EmptyTranscript>> {
    let root = env.claude_projects_root()?;
    if !root.exists() {
        return Err(AppError::ProjectsDirNotFound(root.display().to_string()));
    }

    let projects = match scope {
        DeleteEmptyScope::All => list_projects(&root)?,
        DeleteEmptyScope::Local(workspace) => {
            let project_dir_name = workspace.project_dir_name().to_string();
            let project_dir = root.join(&project_dir_name);
            if !project_dir.exists() {
                return Ok(Vec::new());
            }
            vec![Project {
                name: project_dir_name,
                display_name: workspace.dir().display().to_string(),
                modified: SystemTime::UNIX_EPOCH,
            }]
        }
    };

    let mut candidates: Vec<EmptyTranscript> = projects
        .par_iter()
        .flat_map(|project| {
            let project_dir = root.join(&project.name);
            let entries = match read_dir(project_dir) {
                Ok(entries) => entries,
                Err(_) => return Vec::new(),
            };

            entries
                .filter_map(|entry| {
                    let path = entry.ok()?.path();
                    let filename = path.file_name()?.to_str()?;
                    if path.extension().and_then(|s| s.to_str()) != Some("jsonl")
                        || filename.starts_with("agent-")
                    {
                        return None;
                    }

                    empty_transcript_from_path(&path, &project.display_name)
                        .ok()
                        .flatten()
                })
                .collect::<Vec<_>>()
        })
        .collect();

    candidates.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(candidates)
}

/// The text a user message is found by: its text and the bounded text of
/// its tool results.
fn user_search_text(parts: &[Part]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            Part::Text(text) => Some(text.clone()),
            Part::ToolResult {
                output: Some(output),
                ..
            } => bounded_tool_result_text(output),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn empty_transcript_from_path(path: &Path, project_name: &str) -> Result<Option<EmptyTranscript>> {
    let transcript = super::sources::decode_path(path, None, false)?;
    if transcript.source != SourceKind::Claude {
        return Ok(None);
    }
    let line_count = transcript.records + transcript.diagnostics.len();
    let mut user_messages = 0;
    let mut assistant_messages = 0;
    let mut preview = None;

    for event in &transcript.events {
        let message = match &event.kind {
            EventKind::Message(message) | EventKind::Subagent { message, .. } => message,
            _ => continue,
        };
        match (&event.kind, message.role) {
            (EventKind::Message(_), Role::User) => {
                let text = user_search_text(&message.parts);
                if !text.trim().is_empty() {
                    user_messages += 1;
                    if preview.is_none() {
                        preview = Some(super::parser::normalize_whitespace(&text));
                    }
                }
            }
            (_, Role::Assistant) if parts_count_as_message(&message.parts) => {
                assistant_messages += 1;
            }
            _ => {}
        }
    }

    if assistant_messages > 0 {
        return Ok(None);
    }

    let session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_owned();

    Ok(Some(EmptyTranscript {
        path: path.to_owned(),
        session_id,
        project_name: project_name.to_owned(),
        user_messages,
        line_count,
        preview,
    }))
}

/// List all projects that contain conversation files
pub fn list_projects(root: &Path) -> Result<Vec<Project>> {
    let entries = read_dir(root)?;

    let mut projects: Vec<Project> = entries
        .par_bridge()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();

            if !path.is_dir() {
                return None;
            }

            // Check if project has any non-agent .jsonl files
            let has_conversations = read_dir(&path).ok()?.any(|e| {
                e.ok()
                    .map(|e| {
                        let path = e.path();
                        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                        path.extension().map(|s| s == "jsonl").unwrap_or(false)
                            && !name.starts_with("agent-")
                    })
                    .unwrap_or(false)
            });

            if !has_conversations {
                return None;
            }

            let name = path.file_name()?.to_string_lossy().to_string();
            // Heuristic decode: convert encoded directory name back to readable path
            // The encoding replaces non-alphanumeric chars (except -) with -
            // So / becomes -, but _ also becomes -, and __ becomes --
            // We convert single dashes to / but preserve double dashes as _
            let display_name = decode_project_dir_name(&name);
            let modified = entry
                .metadata()
                .ok()?
                .modified()
                .ok()
                .unwrap_or(SystemTime::UNIX_EPOCH);

            Some(Project {
                name,
                display_name,
                modified,
            })
        })
        .collect();

    // Sort by recently modified
    projects.sort_by_key(|a| std::cmp::Reverse(a.modified));

    Ok(projects)
}

/// Find and process all conversation files of a project directory, with
/// the sidecars of its sessions (upstream 3c2ffeb).
///
/// Fork: the Claude half of [`load_all_conversations_streaming`]; upstream
/// 9597669 removed it with its in-memory loader.
pub(crate) fn load_conversations(
    projects_dir: &Path,
    show_last: bool,
    debug_level: Option<DebugLevel>,
) -> Result<Vec<Conversation>> {
    let mut files = Vec::new();
    let mut skipped_agent_files = 0;

    for entry in read_dir(projects_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            if let Some(filename) = path.file_name().and_then(|f| f.to_str())
                && filename.starts_with("agent-")
            {
                skipped_agent_files += 1;
                debug::debug(debug_level, &format!("Skipping agent file: {}", filename));
                continue;
            }

            let modified = entry.metadata().ok().and_then(|m| m.modified().ok());
            files.push((path, modified));
        } else if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            // The session's subagent and `/btw` fork transcripts (upstream
            // 3c2ffeb), listed like the Claude adapter's discovery does.
            for sidecar in super::sources::claude::session_sidecars(&path) {
                let modified = std::fs::metadata(&sidecar)
                    .ok()
                    .and_then(|m| m.modified().ok());
                files.push((sidecar, modified));
            }
        }
    }

    debug::info(
        debug_level,
        &format!(
            "Found {} conversation files ({} agent files skipped)",
            files.len(),
            skipped_agent_files
        ),
    );

    let mut conversations: Vec<Conversation> = files
        .into_par_iter()
        .filter_map(|(path, modified)| {
            let filename = path
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("unknown")
                .to_owned();

            match process_conversation_file(path, modified, debug_level) {
                Ok(Some(mut conversation)) => {
                    conversation.preview = if show_last {
                        conversation.preview_last.clone()
                    } else {
                        conversation.preview_first.clone()
                    };
                    debug::debug(
                        debug_level,
                        &format!("Parsed {}: {}", filename, conversation.preview),
                    );
                    Some(conversation)
                }
                Ok(None) => None,
                Err(e) => {
                    debug::warn(
                        debug_level,
                        &format!("Error processing {}: {}", filename, e),
                    );
                    None
                }
            }
        })
        .collect();

    // Ensure deterministic ordering after parallel processing
    conversations.sort_by_key(|a| std::cmp::Reverse(a.timestamp));

    // Inject project info into each conversation
    let fallback_path = projects_dir
        .file_name()
        .map(|n| decode_project_dir_name_to_path(&n.to_string_lossy()))
        .unwrap_or_default();

    for (idx, conv) in conversations.iter_mut().enumerate() {
        conv.index = idx;

        // Prefer the cwd extracted from the JSONL file, fall back to decoded
        // path. The name is NFC like the indexer's (upstream a76d5df).
        let project_path = conv.cwd.clone().unwrap_or_else(|| fallback_path.clone());
        conv.project_name = Some(nfc(&format_short_name_from_path(&project_path)).into_owned());
        conv.project_path = Some(project_path);
    }

    debug::info(
        debug_level,
        &format!("Total conversations loaded: {}", conversations.len()),
    );

    Ok(conversations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    /// T-026: delete by ID and delete-empty remove a session's own files and
    /// leave its `/btw` forks (3c2ffeb).
    #[test]
    fn deleting_a_session_by_id_or_as_empty_keeps_its_forks() {
        let user = r#"{"type":"user","cwd":"/work","timestamp":"2026-01-01T00:00:00Z","message":{"role":"user","content":"hello"}}"#;
        let put = |path: &Path| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                path,
                format!(
                    "{user}
"
                ),
            )
            .unwrap();
        };
        let root = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(root.path());
        let projects = env.claude_projects_root().unwrap();
        let project = projects.join("-work");
        let layout = |id: &str| {
            let session = project.join(format!("{id}.jsonl"));
            let sidecars = project.join(id).join("subagents");
            let fork = sidecars.join("agent-afork-1.jsonl");
            let subagent = sidecars.join("agent-a2.jsonl");
            put(&session);
            put(&fork);
            std::fs::write(
                fork.with_extension("meta.json"),
                r#"{"agentType":"fork","isFork":true,"description":"btw"}"#,
            )
            .unwrap();
            put(&subagent);
            std::fs::write(
                subagent.with_extension("meta.json"),
                r#"{"agentType":"Explore"}"#,
            )
            .unwrap();
            std::fs::create_dir_all(project.join(id).join("tool-results")).unwrap();
            std::fs::write(project.join(id).join("tool-results/out.txt"), "x").unwrap();
            (session, fork, subagent)
        };
        let check = |session: &Path, fork: &Path, subagent: &Path| {
            assert!(!session.exists());
            assert!(fork.is_file() && fork.with_extension("meta.json").is_file());
            assert!(!subagent.exists() && !subagent.with_extension("meta.json").exists());
            assert!(!session.with_extension("").join("tool-results").exists());
        };

        let (session, fork, subagent) = layout("s1");
        assert_eq!(delete_session_by_uuid(&env, "s1").unwrap(), 1);
        check(&session, &fork, &subagent);

        // A session with no assistant message is empty.
        let (session, fork, subagent) = layout("s2");
        let empty = EmptyTranscript {
            path: session.clone(),
            session_id: "s2".into(),
            project_name: "work".into(),
            user_messages: 1,
            line_count: 1,
            preview: None,
        };
        assert_eq!(
            delete_empty_transcript(&empty).unwrap(),
            EmptyTranscriptDeletion::Deleted
        );
        check(&session, &fork, &subagent);
    }

    fn result(output: Option<serde_json::Value>) -> Part {
        Part::ToolResult {
            call_id: "id".into(),
            output,
        }
    }

    #[test]
    fn user_search_text_includes_tool_result_string() {
        let parts = [
            Part::Text("hello".into()),
            result(Some(json!("tool output here"))),
        ];
        let text = user_search_text(&parts);
        assert!(text.contains("hello"));
        assert!(text.contains("tool output here"));
    }

    #[test]
    fn user_search_text_includes_tool_result_array() {
        let parts = [result(Some(json!([
            {"type": "text", "text": "line one"},
            {"type": "text", "text": "line two"}
        ])))];
        let text = user_search_text(&parts);
        assert!(text.contains("line one"));
        assert!(text.contains("line two"));
    }

    #[test]
    fn user_search_text_ignores_non_text_blocks_in_array() {
        let parts = [result(Some(json!([
            {"type": "text", "text": "visible"},
            {"type": "image", "source": {"data": "base64..."}}
        ])))];
        let text = user_search_text(&parts);
        assert!(text.contains("visible"));
        assert!(!text.contains("base64"));
    }

    #[test]
    fn user_search_text_handles_missing_and_empty_results() {
        assert_eq!(user_search_text(&[result(None)]), "");
        assert_eq!(user_search_text(&[result(Some(json!("")))]), "");
    }

    #[test]
    fn project_names_and_texts_are_nfc() {
        // The indexer keeps texts and project names in NFC (upstream
        // a76d5df); this loader names projects itself, the same way.
        let dir = tempfile::tempdir().unwrap();
        let lines = [
            json!({
                "type": "user",
                "cwd": "/work/cafe\u{301}",
                "timestamp": "2024-01-01T00:00:00Z",
                "message": {"role": "user", "content": "Cafe\u{301} menu"}
            }),
            json!({
                "type": "assistant",
                "timestamp": "2024-01-01T00:01:00Z",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "ok"}]}
            }),
        ];
        let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
        std::fs::write(
            dir.path()
                .join("11111111-1111-4111-8111-111111111111.jsonl"),
            text,
        )
        .unwrap();

        let conversations = load_conversations(dir.path(), false, None).unwrap();
        assert_eq!(conversations.len(), 1);
        let conversation = &conversations[0];
        assert_eq!(conversation.project_name.as_deref(), Some("caf\u{e9}"));
        assert!(
            conversation.full_text.contains("Caf\u{e9} menu"),
            "{}",
            conversation.full_text
        );
        // The recorded working directory stays as recorded.
        assert_eq!(
            conversation.cwd.as_deref(),
            Some(Path::new("/work/cafe\u{301}"))
        );
    }

    fn write_transcript(lines: &[&str]) -> tempfile::NamedTempFile {
        let mut file = tempfile::Builder::new()
            .suffix(".jsonl")
            .tempfile()
            .unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
        file
    }

    #[test]
    fn empty_transcript_detects_user_only_command_session() {
        let file = write_transcript(&[
            r#"{"type":"user","message":{"role":"user","content":"<command-name>/status</command-name>"}}"#,
        ]);

        let transcript = empty_transcript_from_path(file.path(), "project")
            .unwrap()
            .expect("user-only transcript should be empty");

        assert_eq!(transcript.user_messages, 1);
        assert_eq!(transcript.line_count, 1);
        assert_eq!(transcript.project_name, "project");
        assert_eq!(
            transcript.preview.as_deref(),
            Some("<command-name>/status</command-name>")
        );
    }

    #[test]
    fn empty_transcript_ignores_transcript_with_assistant_message() {
        let file = write_transcript(&[
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hello"}]}}"#,
        ]);

        let transcript = empty_transcript_from_path(file.path(), "project").unwrap();

        assert!(transcript.is_none());
    }

    #[test]
    fn empty_transcript_includes_metadata_only_file() {
        let file = write_transcript(&[r#"{"type":"summary","summary":"Only metadata"}"#]);

        let transcript = empty_transcript_from_path(file.path(), "project")
            .unwrap()
            .expect("metadata-only transcript should be empty");

        assert_eq!(transcript.user_messages, 0);
        assert_eq!(transcript.line_count, 1);
        assert_eq!(transcript.preview, None);
    }
}
