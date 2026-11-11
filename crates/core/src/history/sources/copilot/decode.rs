//! Event log records into transcript events.
//!
//! Every line of `events.jsonl` is `{type, data, id, timestamp, parentId}`,
//! the keys in any order; the first is the session's `session.start`. The
//! dialogue is `user.message` and `assistant.message` (text, reasoning and
//! the tool calls it requests) and `tool.execution_complete` (a call's
//! result). A subagent's records are in the same log, marked with the tool
//! call that started it (`data.parentToolCallId`) and, by newer CLI
//! versions, with a top-level `agentId`.
//!
//! The format is not documented and grows with every CLI version: a record
//! of another type, or one whose data does not have the shape read here,
//! yields no event. Most of a log is such records (hooks, turn markers,
//! tool starts), so `data` is only parsed for the types that are read.

use super::WORKSPACE_FILE;
use super::workspace::Workspace;
use crate::error::Result;
use crate::history::ParseError;
use crate::history::model::{
    ConversationKey, Event, EventKind, ExecutionMode, HeadStatus, Message, Origin, Part,
    Relationship, Revision, Role, Segment, SessionMeta, SourceKind, Transcript, Usage, Visibility,
    push_cwd,
};
use crate::history::sources::LastActivity;
use crate::history::sources::claude::{mtime_ns, parse_timestamp};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::Value;
use serde_json::value::RawValue;
use std::collections::HashMap;
use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The label of a subagent whose start the log does not record.
const UNNAMED_AGENT: &str = "agent";

#[derive(Deserialize)]
struct Record<'a> {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    id: Option<IgnoredAny>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default, rename = "agentId")]
    agent_id: Option<String>,
    #[serde(default, borrow)]
    data: Option<&'a RawValue>,
}

impl Record<'_> {
    /// The record's data in the shape `T`, else `T`'s default.
    fn data<'de, T: Deserialize<'de> + Default>(&'de self) -> T {
        self.data
            .and_then(|data| serde_json::from_str(data.get()).ok())
            .unwrap_or_default()
    }
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Start {
    session_id: Option<String>,
    selected_model: Option<String>,
    context: Context,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Context {
    cwd: Option<String>,
}

/// `session.context_changed` names the directory itself, `session.resume`
/// in its `context`.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Moved {
    cwd: Option<String>,
    context: Context,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ModelChange {
    new_model: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct UserMessage {
    content: Option<String>,
    source: Option<String>,
    parent_tool_call_id: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct AssistantMessage {
    message_id: Option<String>,
    content: Option<String>,
    reasoning_text: Option<String>,
    model: Option<String>,
    tool_requests: Option<Vec<ToolRequest>>,
    parent_tool_call_id: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ToolRequest {
    tool_call_id: Option<String>,
    name: Option<String>,
    arguments: Value,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ToolComplete {
    tool_call_id: Option<String>,
    success: Option<bool>,
    result: Option<Value>,
    error: Option<Value>,
    parent_tool_call_id: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct SubagentStarted {
    tool_call_id: Option<String>,
    agent_name: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Compaction {
    summary_content: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct UsageRecord {
    usage: TokenCounts,
}

/// The tokens of one model call. The input count includes the cached ones.
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct TokenCounts {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
}

impl TokenCounts {
    fn usage(&self) -> Usage {
        let cache_read = self.cache_read_tokens.unwrap_or(0);
        let cache_creation = self.cache_write_tokens.unwrap_or(0);
        Usage {
            input: self
                .input_tokens
                .unwrap_or(0)
                .saturating_sub(cache_read + cache_creation),
            output: self.output_tokens.unwrap_or(0),
            cache_creation,
            cache_read,
        }
    }
}

/// Whether `line`, the first record of a file, is a Copilot event: a typed
/// record with an id and data (as every event has; a log normally starts
/// with its `session.start`).
pub(super) fn starts_event_log(line: &[u8]) -> bool {
    serde_json::from_slice::<Record<'_>>(line.trim_ascii())
        .is_ok_and(|record| record.id.is_some() && record.data.is_some())
}

/// Decodes the event log `path`, read from `reader`, passing its events to
/// `events`. `None` when its first record is not a Copilot event. The
/// titles come from the `workspace.yaml` beside the log. The session was
/// last active at its latest message, else at its last record, the time
/// `workspace.yaml` names, or the log's mtime (`modified`).
pub(super) fn decode(
    key: ConversationKey,
    path: &Path,
    mut reader: impl BufRead,
    modified: Option<SystemTime>,
    events: &mut dyn FnMut(Event),
) -> Result<Option<Transcript>> {
    let mut decoder = Decoder {
        events,
        diagnostics: Vec::new(),
        records: 0,
        session_id: None,
        model: None,
        cwds: Vec::new(),
        agents: HashMap::new(),
        last_timestamp: None,
        activity: LastActivity::default(),
    };
    let mut line = Vec::new();
    let mut consumed: u64 = 0;
    let mut line_number: u32 = 0;
    let mut started = false;
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        consumed += read as u64;
        line_number += 1;
        let complete = line.last() == Some(&b'\n');
        let text = line.trim_ascii();
        if text.is_empty() {
            continue;
        }
        decoder.records += 1;
        let record: Record<'_> = match serde_json::from_slice(text) {
            Ok(record) => record,
            Err(error) => {
                let message = if complete {
                    format!("malformed Copilot event: {error}")
                } else {
                    "incomplete last record, still being written".to_owned()
                };
                decoder.diagnostics.push(ParseError {
                    line_number: line_number as usize,
                    line_content: String::from_utf8_lossy(text).into_owned(),
                    error_message: message,
                    context_before: Vec::new(),
                    context_after: Vec::new(),
                });
                continue;
            }
        };
        if !started {
            if record.id.is_none() || record.data.is_none() {
                return Ok(None);
            }
            started = true;
        }
        decoder.record(&record, line_number);
    }
    if !started {
        return Ok(None);
    }

    let workspace_path = path.with_file_name(WORKSPACE_FILE);
    let workspace = Workspace::read(&workspace_path);
    let workspace_modified = fs::metadata(&workspace_path)
        .ok()
        .and_then(|metadata| metadata.modified().ok());
    let mut cwds = std::mem::take(&mut decoder.cwds);
    if let Some(cwd) = workspace.cwd.as_deref().filter(|_| cwds.is_empty()) {
        cwds.push(PathBuf::from(cwd));
    }
    let cwd = cwds.first().cloned();
    let (custom_title, summary) = titles(&workspace);
    let updated_at = decoder
        .activity
        .at()
        .or(decoder.last_timestamp)
        .or_else(|| workspace.updated_at.as_deref().and_then(parse_timestamp))
        .or_else(|| modified.map(DateTime::<Utc>::from));
    let session_id = decoder
        .session_id
        .take()
        .unwrap_or_else(|| key.native_id.clone());
    let segments = vec![Segment {
        artifact: path.to_path_buf(),
        start: 0,
        end: consumed,
        inherited: false,
    }];
    // The titles are part of what was decoded: a renamed session is another
    // revision of the same log.
    let mtime = mtime_ns(modified).max(mtime_ns(workspace_modified));
    Ok(Some(Transcript {
        revision: Revision::of(SourceKind::Copilot, &segments, &[mtime]),
        key,
        source: SourceKind::Copilot,
        head: HeadStatus::Current,
        mode: ExecutionMode::Unknown,
        relationship: Relationship::Root,
        meta: SessionMeta {
            session_id,
            cwd: cwd.clone(),
            cwds,
            project: cwd,
            model: decoder.model,
            summary,
            custom_title,
            updated_at,
        },
        segments,
        events: Vec::new(),
        records: decoder.records,
        diagnostics: decoder.diagnostics,
    }))
}

/// The custom title and the summary `workspace` gives a session: a name the
/// user chose is the custom title, a generated one the summary, as is the
/// `summary` older CLI versions wrote (on one line).
fn titles(workspace: &Workspace) -> (Option<String>, Option<String>) {
    let one_line = |text: &str| {
        let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
        (!line.is_empty()).then_some(line)
    };
    let name = workspace.name.as_deref().and_then(one_line);
    let summary = workspace.summary.as_deref().and_then(one_line);
    if workspace.user_named {
        (name, summary)
    } else {
        (None, name.or(summary))
    }
}

struct Decoder<'a> {
    events: &'a mut dyn FnMut(Event),
    diagnostics: Vec<ParseError>,
    records: usize,
    session_id: Option<String>,
    /// The model the session started with, then the one it last changed to.
    model: Option<String>,
    /// Distinct working directories, in order of first appearance.
    cwds: Vec<PathBuf>,
    /// Names of the subagents started so far, by agent id and by the id of
    /// the tool call that started them.
    agents: HashMap<String, String>,
    /// Timestamp of the last record that has one.
    last_timestamp: Option<DateTime<Utc>>,
    activity: LastActivity,
}

impl Decoder<'_> {
    fn record(&mut self, record: &Record<'_>, line: u32) {
        let timestamp = record.timestamp.as_deref().and_then(parse_timestamp);
        if timestamp.is_some() {
            self.last_timestamp = timestamp;
        }
        let Some((visibility, kind)) = self.kind(record) else {
            return;
        };
        let event = Event {
            origin: Origin { segment: 0, line },
            timestamp,
            visibility,
            kind,
        };
        self.activity.observe(&event);
        (self.events)(event);
    }

    /// The event of one record, if it has one, with its visibility.
    fn kind(&mut self, record: &Record<'_>) -> Option<(Visibility, EventKind)> {
        match record.kind.as_str() {
            "session.start" => {
                let start: Start = record.data();
                self.session_id = start.session_id.filter(|id| !id.is_empty());
                self.model = start.selected_model.filter(|model| !model.is_empty());
                self.moved_to(start.context.cwd);
                None
            }
            "session.resume" | "session.context_changed" => {
                let moved: Moved = record.data();
                self.moved_to(moved.cwd.or(moved.context.cwd));
                None
            }
            "session.model_change" => {
                let model = record
                    .data::<ModelChange>()
                    .new_model
                    .filter(|model| !model.is_empty())?;
                self.model = Some(model.clone());
                Some((Visibility::Dialogue, note("Model", model, None)))
            }
            "subagent.started" => {
                let started: SubagentStarted = record.data();
                let name = started.agent_name.filter(|name| !name.is_empty())?;
                for id in [record.agent_id.clone(), started.tool_call_id]
                    .into_iter()
                    .flatten()
                {
                    self.agents.insert(id, name.clone());
                }
                None
            }
            "user.message" => {
                let data: UserMessage = record.data();
                let content = data.content.filter(|content| !content.trim().is_empty())?;
                // What the CLI itself tells the model as a user message.
                let visibility = if data.source.as_deref() == Some("system") {
                    Visibility::Injected
                } else {
                    Visibility::Dialogue
                };
                let message = plain_message(Role::User, None, vec![Part::Text(content)]);
                Some(self.attribute(record, data.parent_tool_call_id, message, visibility))
            }
            "assistant.message" => {
                let message: AssistantMessage = record.data();
                let mut parts = Vec::new();
                if let Some(reasoning) = message
                    .reasoning_text
                    .filter(|text| !text.trim().is_empty())
                {
                    parts.push(Part::Thinking(reasoning));
                }
                if let Some(content) = message.content.filter(|text| !text.trim().is_empty()) {
                    parts.push(Part::Text(content));
                }
                for request in message.tool_requests.unwrap_or_default() {
                    let Some(name) = request.name.filter(|name| !name.is_empty()) else {
                        continue;
                    };
                    parts.push(Part::ToolCall {
                        id: request.tool_call_id.unwrap_or_default(),
                        name,
                        input: request.arguments,
                    });
                }
                if parts.is_empty() {
                    return None;
                }
                let mut decoded = plain_message(Role::Assistant, message.message_id, parts);
                decoded.model = message.model.filter(|model| !model.is_empty());
                Some(self.attribute(
                    record,
                    message.parent_tool_call_id,
                    decoded,
                    Visibility::Dialogue,
                ))
            }
            "tool.execution_complete" => {
                let complete: ToolComplete = record.data();
                let call_id = complete.tool_call_id.filter(|id| !id.is_empty())?;
                let output = tool_output(complete.success, complete.result, complete.error);
                let message =
                    plain_message(Role::User, None, vec![Part::ToolResult { call_id, output }]);
                Some(self.attribute(
                    record,
                    complete.parent_tool_call_id,
                    message,
                    Visibility::Dialogue,
                ))
            }
            "session.compaction_complete" => {
                let summary = record
                    .data::<Compaction>()
                    .summary_content
                    .filter(|summary| !summary.trim().is_empty())?;
                Some((Visibility::Dialogue, note("Compaction", summary, None)))
            }
            "session.usage_record" => {
                let usage = record.data::<UsageRecord>().usage.usage();
                (usage.total() > 0).then(|| {
                    (
                        Visibility::Dialogue,
                        note("Token count", String::new(), Some(usage)),
                    )
                })
            }
            _ => None,
        }
    }

    fn moved_to(&mut self, cwd: Option<String>) {
        if let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty()) {
            push_cwd(&mut self.cwds, PathBuf::from(cwd));
        }
    }

    /// `message` as the main agent's, or as a subagent's when the record
    /// names one: by its own agent id or by the tool call that started it.
    fn attribute(
        &self,
        record: &Record<'_>,
        parent_tool_call: Option<String>,
        message: Message,
        visibility: Visibility,
    ) -> (Visibility, EventKind) {
        let marks = [record.agent_id.as_deref(), parent_tool_call.as_deref()];
        if marks.iter().all(Option::is_none) {
            return (visibility, EventKind::Message(message));
        }
        let agent_id = marks
            .iter()
            .flatten()
            .find_map(|id| self.agents.get(*id))
            .map_or(UNNAMED_AGENT, String::as_str)
            .to_owned();
        (
            Visibility::Dialogue,
            EventKind::Subagent { agent_id, message },
        )
    }
}

/// What a finished tool call returned: its result's text, or its error.
fn tool_output(
    success: Option<bool>,
    result: Option<Value>,
    error: Option<Value>,
) -> Option<Value> {
    let error = error.and_then(|error| match error {
        Value::String(message) => Some(message),
        Value::Object(mut error) => match error.remove("message") {
            Some(Value::String(message)) => Some(message),
            _ => None,
        },
        _ => None,
    });
    if success == Some(false)
        && let Some(message) = error
    {
        return Some(Value::String(format!("Error: {message}")));
    }
    match result? {
        Value::Object(mut result) => match result.remove("content") {
            Some(content) => Some(content),
            None => Some(Value::Object(result)),
        },
        Value::Null => None,
        other => Some(other),
    }
}

fn note(label: &str, text: String, usage: Option<Usage>) -> EventKind {
    EventKind::Note {
        label: label.to_owned(),
        text,
        searchable: false,
        usage,
    }
}

fn plain_message(role: Role, id: Option<String>, parts: Vec<Part>) -> Message {
    Message {
        role,
        id,
        parts,
        model: None,
        usage: None,
        parent_tool_use_id: None,
    }
}
