//! The result of one agent request: structured data for rich rendering plus
//! the exact text the CLI printed.
//!
//! [`crate::agent::execute`] returns an [`AgentResponse`]:
//!
//! * `stdout` / `stderr` / `exit_code` reproduce what
//!   `claude-history agent ...` printed and returned (the protocol text on
//!   stdout; semantic notices and the `protocol agent-error` envelope on
//!   stderr), so a GUI can show or copy the agent text verbatim;
//! * `result` holds the same answer as data: ranked conversations and hits
//!   with evidence previews, anchors and message ranges
//!   ([`AgentSearchResult`]), the rendered messages of a read
//!   ([`AgentReadResult`]) or the outline records ([`AgentOutlineResult`]),
//!   each with the transcript paths of the conversations involved;
//! * `warnings`, `error` and `notices` are the diagnostics.
//!
//! The structured data is computed before the output budget (`--budget`)
//! trims the text, so it can contain more than `stdout`.

use crate::agent::diagnostic::{AgentError, AgentWarning};
use crate::agent::protocol::{OutlineItem, ProtocolFocus, ProtocolOptions, ReadMessage, ReadSlice};
use crate::agent::refs::ResolvedConversation;
use crate::agent::search::{AgentOutputHit, AgentProtocolKind, AgentSearchOutput};
use crate::agent::service::AgentNotice;
use crate::history::Source;
use std::path::PathBuf;

/// A conversation referenced by a result, with where it lives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentConversationInfo {
    /// The reference the protocol text uses (`ch_...`).
    pub reference: String,
    /// The full (unabbreviated) reference.
    pub full_ref: String,
    /// The session UUID as the protocol reports it.
    pub uuid: String,
    /// The protocol's project id.
    pub project_id: String,
    pub source: Source,
    /// Session id (Claude: file stem; Pi/OMP: header id).
    pub session_id: String,
    /// Transcript file name.
    pub session_filename: String,
    /// Transcript path (open it in the viewer).
    pub path: PathBuf,
}

impl From<&ResolvedConversation> for AgentConversationInfo {
    fn from(resolved: &ResolvedConversation) -> Self {
        Self {
            reference: resolved.reference.canonical(),
            full_ref: resolved.reference.full_ref(),
            uuid: resolved.reference.uuid(),
            project_id: resolved.key.project_id(),
            source: resolved.key.source,
            session_id: resolved.key.session_id.clone(),
            session_filename: resolved.key.session_filename.clone(),
            path: resolved.key.path.clone(),
        }
    }
}

/// `agent search` / `agent within` as data.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentSearchResult {
    /// Mode, query, target (within), grouped or flat hits with scores,
    /// anchors, focus/read ranges and evidence previews, statistics.
    pub output: AgentSearchOutput,
    /// Every conversation a hit (or the within target) refers to, in order
    /// of first appearance.
    pub conversations: Vec<AgentConversationInfo>,
}

impl AgentSearchResult {
    /// The hits in output order (grouped output: group by group).
    pub fn hits(&self) -> Vec<&AgentOutputHit> {
        if self.output.protocol == AgentProtocolKind::Search
            && !self.output.flat
            && !self.output.groups.is_empty()
        {
            self.output
                .groups
                .iter()
                .flat_map(|group| group.hits.iter())
                .collect()
        } else {
            self.output.hits.iter().collect()
        }
    }

    /// The conversation a hit or group refers to.
    pub fn conversation(&self, reference: &str) -> Option<&AgentConversationInfo> {
        self.conversations
            .iter()
            .find(|conversation| conversation.reference == reference)
    }
}

/// `agent read` as data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentReadResult {
    /// The conversations read, in request order.
    pub conversations: Vec<AgentConversationInfo>,
    /// Every rendered message, unbudgeted.
    pub messages: Vec<ReadMessage>,
    /// `--focus` (or none).
    pub focus: Option<ProtocolFocus>,
    /// `--lines` / `--match`.
    pub slice: Option<ReadSlice>,
    /// Budget and visibility policy applied.
    pub options: ProtocolOptions,
}

/// `agent outline` as data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentOutlineResult {
    pub conversation: AgentConversationInfo,
    /// Message records (up to 20 visible messages) or 10-message segments,
    /// unbudgeted.
    pub items: Vec<OutlineItem>,
    /// Messages in the transcript.
    pub total_messages: usize,
    /// Messages visible under the policy.
    pub visible_messages: usize,
    pub options: ProtocolOptions,
}

/// The structured answer of a request.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentResult {
    Search(AgentSearchResult),
    Within(AgentSearchResult),
    Read(AgentReadResult),
    Outline(AgentOutlineResult),
}

/// Everything one agent request produced.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentResponse {
    /// What the CLI printed to stdout: the protocol text (empty on failure).
    pub stdout: String,
    /// What the CLI printed to stderr: one line per notice, then the error
    /// envelope on failure.
    pub stderr: String,
    /// The CLI's exit code: 0 on success, 1 on failure.
    pub exit_code: i32,
    /// The structured answer (`None` on failure).
    pub result: Option<AgentResult>,
    /// Warnings attached to the answer (also rendered as `warning` records
    /// in `stdout`).
    pub warnings: Vec<AgentWarning>,
    /// The structured error on failure.
    pub error: Option<AgentError>,
    /// Side-channel notices (semantic progress, uncached passages).
    pub notices: Vec<AgentNotice>,
}

impl AgentResponse {
    pub fn is_success(&self) -> bool {
        self.exit_code == 0
    }

    /// The protocol text to show verbatim: `stdout` on success, the error
    /// envelope on failure.
    pub fn text(&self) -> &str {
        if self.is_success() {
            &self.stdout
        } else {
            self.error_text()
        }
    }

    /// The error envelope part of `stderr` (empty on success).
    pub fn error_text(&self) -> &str {
        if self.is_success() {
            return "";
        }
        let notices: usize = self
            .notices
            .iter()
            .map(|notice| notice.to_string().len() + 1)
            .sum();
        &self.stderr[notices.min(self.stderr.len())..]
    }

    pub(crate) fn success(
        stdout: String,
        result: AgentResult,
        warnings: Vec<AgentWarning>,
        notices: Vec<AgentNotice>,
    ) -> Self {
        Self {
            stdout,
            stderr: notice_lines(&notices),
            exit_code: 0,
            result: Some(result),
            warnings,
            error: None,
            notices,
        }
    }

    pub(crate) fn failure(
        envelope: String,
        error: Option<AgentError>,
        notices: Vec<AgentNotice>,
    ) -> Self {
        let mut stderr = notice_lines(&notices);
        stderr.push_str(&envelope);
        Self {
            stdout: String::new(),
            stderr,
            exit_code: 1,
            result: None,
            warnings: Vec::new(),
            error,
            notices,
        }
    }
}

impl AgentResponse {
    /// The error text of a request cancelled through
    /// [`crate::agent::service::AgentService::with_cancellation`] (K7): it
    /// fails with exit code 1, no structured error, and this text after
    /// the notices produced before the cancel (the whole `stderr` when
    /// there were none).
    pub const CANCELLED_TEXT: &'static str = "Error: Semantic search cancelled\n";

    /// Whether the request was cancelled (see [`Self::CANCELLED_TEXT`]).
    pub fn is_cancelled(&self) -> bool {
        !self.is_success() && self.error.is_none() && self.error_text() == Self::CANCELLED_TEXT
    }
}

fn notice_lines(notices: &[AgentNotice]) -> String {
    notices.iter().map(|notice| format!("{notice}\n")).collect()
}

/// The conversations of `output`, in order of first appearance, resolved
/// against `resolved`.
pub(crate) fn conversations_for_output(
    output: &AgentSearchOutput,
    resolved: &[ResolvedConversation],
) -> Vec<AgentConversationInfo> {
    let references = output
        .target
        .iter()
        .map(|target| target.conversation_ref.as_str())
        .chain(
            output
                .groups
                .iter()
                .map(|group| group.conversation_ref.as_str()),
        )
        .chain(
            output
                .groups
                .iter()
                .flat_map(|group| group.hits.iter())
                .chain(output.hits.iter())
                .map(|hit| hit.conversation_ref.as_str()),
        );
    let mut seen = std::collections::HashSet::new();
    let mut conversations = Vec::new();
    for reference in references {
        if !seen.insert(reference) {
            continue;
        }
        if let Some(conversation) = resolved
            .iter()
            .find(|resolved| resolved.reference.canonical() == reference)
        {
            conversations.push(AgentConversationInfo::from(conversation));
        }
    }
    conversations
}
