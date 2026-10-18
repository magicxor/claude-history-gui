//! Agent result sets (DESIGN §5.4, §11.14): the view models built from
//! `AgentResponse` data — compact group cards and hit rows, outline rows,
//! read cards, banners (exact-forced, warnings, uncached passages), the
//! stats footer — and [`ResultsView`], the entity both the Deep search tab
//! and the agent panel's tabs embed. WP4.
//!
//! The models are gpui-free and unit-tested; the view only renders them and
//! turns clicks and keys into [`ResultsEvent`]s. A result set owns the
//! conversations it was computed for: every row carries the
//! `AgentConversationInfo` of its own conversation, so opening a row always
//! reveals the message in *that* conversation.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use claude_history_core::agent::diagnostic::{AgentErrorKind, AgentWarning};
use claude_history_core::agent::protocol::{OutlineItem, ReadMessage};
use claude_history_core::agent::retrieval::AgentHitSource;
use claude_history_core::agent::search::{AgentHitKind, AgentOutputHit, AgentProtocolKind};
use claude_history_core::agent::transcript::AgentMessageRole;
use claude_history_core::agent::{
    AgentConversationInfo, AgentNotice, AgentOutlineResult, AgentReadResult, AgentResponse,
    AgentResult, AgentSearchResult,
};
use claude_history_core::search::mode::SearchMode;
use claude_history_core::search::query::ParsedQuery;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::scroll::{ScrollableElement as _, Scrollbar};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ListAlignment, ListState, ParentElement as _, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _,
    Window, div, list, px,
};

use crate::actions::{
    AgentOpenSelected, AgentSelectNext, AgentSelectPrev, DeepOpenHit, DeepSelectNext,
    DeepSelectPrev, GenerateSemanticCache, ToggleAgentText,
};
use crate::deep_search::agent_text::{AgentTextView, toggle_bar};
use crate::model::{
    AgentTab, ContentVisibilityHint, ConversationTarget, RevealKind, RevealRequest,
};
use crate::notify;
use crate::qa_ext::QaExt as _;
use crate::services::services;
use crate::theme_tokens;

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

/// Lines of a read card body that are rendered; the agent text view has all.
pub const READ_BODY_LINES: usize = 400;

/// `1.2k` for 1,200 characters, `312` below a thousand.
pub fn format_chars(chars: usize) -> String {
    if chars >= 1000 {
        let tenths = (chars + 50) / 100;
        if tenths.is_multiple_of(10) {
            format!("{}k", tenths / 10)
        } else {
            format!("{}.{}k", tenths / 10, tenths % 10)
        }
    } else {
        chars.to_string()
    }
}

/// The message label of a range: `m12` or `m12..m14`.
pub fn range_label(start: usize, end: usize) -> String {
    if start == end {
        format!("m{start}")
    } else {
        format!("m{start}..m{end}")
    }
}

/// The protocol's source atom of a hit (`lexical`, `tool`, …).
fn source_atom(hit: &AgentOutputHit) -> &'static str {
    match hit.evidence_source {
        AgentHitSource::Dialogue => match hit.source {
            AgentHitKind::Exact => "exact",
            AgentHitKind::Lexical => "lexical",
            AgentHitKind::Semantic => "semantic",
            AgentHitKind::Hybrid => "hybrid",
        },
        AgentHitSource::Tool => "tool",
        AgentHitSource::Thinking => "thinking",
    }
}

/// Resolves the project name the conversation list shows for a transcript
/// (core's `project_name`, taken from the cwd the transcript records).
pub type ProjectLookup<'a> = &'a dyn Fn(&std::path::Path) -> Option<String>;

/// The project the way the list shows it (`proj`, `proj/feature`): the
/// list's own name of the transcript (D-010), else for a Claude transcript
/// the decoded project folder, else the protocol's `pr_...` id. Pi and OMP
/// session folders are not in Claude's encoding and are never decoded.
pub fn project_label(
    info: Option<&AgentConversationInfo>,
    project_id: &str,
    lookup: Option<ProjectLookup<'_>>,
) -> String {
    let from_list = info
        .and_then(|info| lookup.and_then(|lookup| lookup(&info.path)))
        .filter(|label| !label.is_empty());
    let decoded = || {
        let info =
            info.filter(|info| info.source == claude_history_core::history::Source::Claude)?;
        let dir = info.path.parent()?.file_name()?.to_str()?;
        Some(claude_history_core::history::format_short_name_from_path(
            std::path::Path::new(&claude_history_core::history::path::decode_project_dir_name(dir)),
        ))
        .filter(|label| !label.is_empty())
    };
    from_list
        .or_else(decoded)
        .unwrap_or_else(|| project_id.to_string())
}

/// A message of some conversation a row reveals.
#[derive(Clone, Debug, PartialEq)]
pub struct RevealSpec {
    pub info: AgentConversationInfo,
    pub title: Option<String>,
    pub ordinal: usize,
    pub hint: ContentVisibilityHint,
}

impl RevealSpec {
    /// The conversation target and the reveal request of the message.
    pub fn request(&self) -> (ConversationTarget, RevealRequest) {
        let target = ConversationTarget::from_agent_info(&self.info, self.title.as_deref());
        let reveal = RevealRequest {
            path: self.info.path.clone(),
            full_ref: Some(self.info.full_ref.clone()),
            kind: RevealKind::AgentOrdinal {
                ordinal: self.ordinal,
                hint: self.hint,
            },
        };
        (target, reveal)
    }
}

/// One evidence hit.
#[derive(Clone, Debug, PartialEq)]
pub struct HitModel {
    /// The reference the protocol text uses (`ch_` + 12 hex).
    pub conversation_ref: String,
    pub full_ref: String,
    pub title: String,
    pub project_id: String,
    /// The project as the list shows it.
    pub project: String,
    pub anchors: Vec<String>,
    pub source: &'static str,
    pub score: f64,
    /// `(semantic, lexical)` of a semantic or hybrid hit.
    pub breakdown: Option<(f32, f32)>,
    pub focus: (usize, usize),
    pub read: (usize, usize),
    pub preview: String,
    pub hint: ContentVisibilityHint,
    pub info: Option<AgentConversationInfo>,
}

impl HitModel {
    fn new(
        hit: &AgentOutputHit,
        info: Option<&AgentConversationInfo>,
        title: &str,
        lookup: Option<ProjectLookup<'_>>,
    ) -> HitModel {
        HitModel {
            conversation_ref: hit.conversation_ref.clone(),
            full_ref: info
                .map(|info| info.full_ref.clone())
                .unwrap_or_else(|| hit.conversation_ref.clone()),
            title: title.to_string(),
            project_id: hit.project_id.clone(),
            project: project_label(info, &hit.project_id, lookup),
            anchors: hit.anchors.clone(),
            source: source_atom(hit),
            score: hit.score,
            breakdown: hit
                .semantic_score_breakdown
                .map(|score| (score.semantic, score.lexical)),
            focus: (hit.focus_range.start, hit.focus_range.end),
            read: (hit.read_range.start, hit.read_range.end),
            preview: hit.preview.clone(),
            hint: ContentVisibilityHint {
                tools: hit.render_options.tools,
                tool_results: hit.render_options.tool_results,
                thinking: hit.render_options.thinking,
                subagents: hit.render_options.subagents,
            },
            info: info.cloned(),
        }
    }

    /// The message `Open at message` reveals: the first message of the focus.
    pub fn ordinal(&self) -> usize {
        self.focus.0
    }

    /// `m12` or `m12..m14` (the focus range).
    pub fn focus_label(&self) -> String {
        range_label(self.focus.0, self.focus.1)
    }

    /// The hit's score and, of a semantic or hybrid hit, its breakdown, as
    /// the protocol prints them (`0.123456 sem 0.100 lex 0.200`).
    pub fn score_text(&self) -> String {
        match self.breakdown {
            Some((semantic, lexical)) => {
                format!("{:.6} sem {semantic:.3} lex {lexical:.3}", self.score)
            }
            None => format!("{:.6}", self.score),
        }
    }

    /// The row's tooltip: the full ref handle and the score.
    pub fn tooltip(&self) -> String {
        format!("{}\nscore {}", self.full_ref, self.score_text())
    }

    /// The row's accessible name.
    pub fn accessibility_label(&self) -> String {
        format!(
            "{} \u{b7} {} \u{b7} {} {} \u{b7} score {}",
            self.title,
            self.project,
            self.source,
            self.focus_label(),
            self.score_text()
        )
    }

    pub fn reveal(&self) -> Option<RevealSpec> {
        Some(RevealSpec {
            info: self.info.clone()?,
            title: Some(self.title.clone()),
            ordinal: self.ordinal(),
            hint: self.hint,
        })
    }

    /// D-012: `claude-history agent read <ref>:mC..mD --focus mA..mB
    /// [--tools] [--tool-results] [--thinking] [--subagents]`.
    pub fn read_command(&self) -> String {
        let mut command = format!(
            "claude-history agent read {}:m{}..m{} --focus m{}..m{}",
            self.full_ref, self.read.0, self.read.1, self.focus.0, self.focus.1
        );
        for (on, flag) in [
            (self.hint.tools, "--tools"),
            (self.hint.tool_results, "--tool-results"),
            (self.hint.thinking, "--thinking"),
            (self.hint.subagents, "--subagents"),
        ] {
            if on {
                command.push(' ');
                command.push_str(flag);
            }
        }
        command
    }

    /// The lines of the agent text that belong to this hit: its `hit`
    /// record and the `read` recipe that follows. `None` when the budget
    /// cut it from the text.
    pub fn agent_records(&self, text: &str) -> Option<String> {
        let ref_atom = format!(" ref={} ", self.conversation_ref);
        let anchors_atom = format!(" anchors={} ", self.anchors.join(","));
        let focus_atom = format!(" focus=m{}..m{} ", self.focus.0, self.focus.1);
        let lines: Vec<&str> = text.lines().collect();
        let at = lines.iter().position(|line| {
            line.starts_with("hit ")
                && line.contains(&ref_atom)
                && line.contains(&anchors_atom)
                && line.contains(&focus_atom)
        })?;
        let mut records = format!("{}\n", lines[at]);
        if let Some(next) = lines.get(at + 1)
            && next.starts_with("read ")
        {
            records.push_str(next);
            records.push('\n');
        }
        Some(records)
    }
}

/// A conversation card of grouped output.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupModel {
    pub rank: usize,
    pub title: String,
    pub project_id: String,
    /// The project as the list shows it.
    pub project: String,
    pub uuid: String,
    pub conversation_ref: String,
    pub full_ref: String,
    pub score: f64,
    pub breakdown: Option<(f32, f32)>,
    /// Hits shown / total hits of the conversation.
    pub shown: usize,
    pub total: usize,
    pub info: Option<AgentConversationInfo>,
}

impl GroupModel {
    /// `proj · 3f2a19c0 · sem 0.812 lex 0.100 · 2/5 hits`
    pub fn meta(&self) -> String {
        let mut parts = vec![self.project.clone(), self.uuid.chars().take(8).collect()];
        if let Some((semantic, lexical)) = self.breakdown {
            parts.push(format!("sem {semantic:.3} lex {lexical:.3}"));
        }
        parts.push(format!("{}/{} hits", self.shown, self.total));
        parts.join(" · ")
    }

    /// The score as the protocol prints it.
    pub fn score_text(&self) -> String {
        format!("{:.6}", self.score)
    }

    /// The card's accessible name.
    pub fn accessibility_label(&self) -> String {
        format!(
            "{} \u{b7} {} \u{b7} score {} \u{b7} {}/{} hits",
            self.title,
            self.project,
            self.score_text(),
            self.shown,
            self.total
        )
    }
}

/// A row of a search result list.
#[derive(Clone, Debug, PartialEq)]
pub enum SearchRow {
    Group(GroupModel),
    Hit(HitModel),
}

/// A search or within result as rows.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchModel {
    pub rows: Vec<SearchRow>,
    pub mode: SearchMode,
    pub flat: bool,
    pub within: bool,
    /// Within: `conversation project=… uuid=… ref=…` (also with 0 hits).
    pub target_line: Option<String>,
    pub query: String,
    /// Quoted-only query: the exact mode was forced.
    pub exact_forced: bool,
    pub groups: usize,
    pub hits: usize,
    pub shortlisted: usize,
    pub loaded: usize,
}

impl SearchModel {
    pub fn from_result(result: &AgentSearchResult) -> SearchModel {
        Self::from_result_with(result, None)
    }

    /// `lookup` names the projects the way the conversation list does.
    pub fn from_result_with(
        result: &AgentSearchResult,
        lookup: Option<ProjectLookup<'_>>,
    ) -> SearchModel {
        let output = &result.output;
        let within = output.protocol == AgentProtocolKind::Within;
        let mut rows = Vec::new();
        let grouped = !within && !output.flat && !output.groups.is_empty();
        if grouped {
            for (index, group) in output.groups.iter().enumerate() {
                let info = result.conversation(&group.conversation_ref);
                rows.push(SearchRow::Group(GroupModel {
                    rank: index + 1,
                    title: group.title.clone(),
                    project_id: group.project_id.clone(),
                    project: project_label(info, &group.project_id, lookup),
                    uuid: group.conversation_uuid.clone(),
                    conversation_ref: group.conversation_ref.clone(),
                    full_ref: info
                        .map(|info| info.full_ref.clone())
                        .unwrap_or_else(|| group.conversation_ref.clone()),
                    score: group.score,
                    breakdown: group
                        .hits
                        .first()
                        .and_then(|hit| hit.semantic_score_breakdown)
                        .map(|score| (score.semantic, score.lexical)),
                    shown: group.hits.len(),
                    total: group.total_hits,
                    info: info.cloned(),
                }));
                for hit in &group.hits {
                    rows.push(SearchRow::Hit(HitModel::new(
                        hit,
                        info,
                        &group.title,
                        lookup,
                    )));
                }
            }
        } else {
            for hit in &output.hits {
                let info = result.conversation(&hit.conversation_ref);
                rows.push(SearchRow::Hit(HitModel::new(hit, info, &hit.title, lookup)));
            }
        }
        let hits = rows
            .iter()
            .filter(|row| matches!(row, SearchRow::Hit(_)))
            .count();
        let exact_forced =
            output.mode == SearchMode::Exact && ParsedQuery::parse(&output.query).is_quoted_only();
        SearchModel {
            rows,
            mode: output.mode,
            flat: output.flat,
            within,
            target_line: output.target.as_ref().map(|target| {
                format!(
                    "conversation project={} uuid={} ref={}",
                    target.project_id, target.conversation_uuid, target.conversation_ref
                )
            }),
            query: output.query.clone(),
            exact_forced,
            groups: if grouped { output.groups.len() } else { 0 },
            hits,
            shortlisted: output.stats.shortlisted,
            loaded: output.stats.transcripts_loaded,
        }
    }

    /// The footer: `<g> groups · <h> hits · shortlisted <s> · loaded <l>`
    /// (flat and within output has no group count).
    pub fn stats_line(&self) -> String {
        let hits = if self.flat || self.within {
            format!("{} hits", self.hits)
        } else {
            format!("{} groups · {} hits", self.groups, self.hits)
        };
        format!(
            "{hits} · shortlisted {} · loaded {}",
            self.shortlisted, self.loaded
        )
    }

    pub fn hit(&self, hit_index: usize) -> Option<&HitModel> {
        self.rows
            .iter()
            .filter_map(|row| match row {
                SearchRow::Hit(hit) => Some(hit),
                SearchRow::Group(_) => None,
            })
            .nth(hit_index)
    }
}

/// An outline row.
#[derive(Clone, Debug, PartialEq)]
pub enum OutlineRow {
    Message {
        ordinal: usize,
        role: &'static str,
        chars: usize,
        anchor: String,
        snippet: String,
    },
    Segment {
        start: usize,
        end: usize,
        chars: usize,
        first_anchor: String,
        last_anchor: String,
        first_snippet: String,
        last_snippet: String,
    },
}

/// The Outline tab's result.
#[derive(Clone, Debug, PartialEq)]
pub struct OutlineModel {
    pub info: AgentConversationInfo,
    pub rows: Vec<OutlineRow>,
    pub total: usize,
    pub visible: usize,
    pub hint: ContentVisibilityHint,
}

fn role_label(role: AgentMessageRole) -> &'static str {
    match role {
        AgentMessageRole::User => "user",
        AgentMessageRole::Assistant => "assistant",
    }
}

impl OutlineModel {
    pub fn from_result(result: &AgentOutlineResult) -> OutlineModel {
        let rows = result
            .items
            .iter()
            .map(|item| match item {
                OutlineItem::Message {
                    ordinal,
                    role,
                    chars,
                    anchor,
                    snippet,
                } => OutlineRow::Message {
                    ordinal: *ordinal,
                    role: role_label(*role),
                    chars: *chars,
                    anchor: anchor.clone(),
                    snippet: snippet.clone(),
                },
                OutlineItem::Segment {
                    start,
                    end,
                    chars,
                    first_anchor,
                    last_anchor,
                    first_snippet,
                    last_snippet,
                } => OutlineRow::Segment {
                    start: *start,
                    end: *end,
                    chars: *chars,
                    first_anchor: first_anchor.clone(),
                    last_anchor: last_anchor.clone(),
                    first_snippet: first_snippet.clone(),
                    last_snippet: last_snippet.clone(),
                },
            })
            .collect();
        OutlineModel {
            info: result.conversation.clone(),
            rows,
            total: result.total_messages,
            visible: result.visible_messages,
            hint: ContentVisibilityHint {
                tools: result.options.tools,
                tool_results: result.options.tool_results,
                thinking: result.options.thinking,
                subagents: result.options.subagents,
            },
        }
    }

    /// `42 visible messages` (and the total when it differs).
    pub fn summary(&self) -> String {
        if self.visible == self.total {
            format!("{} visible messages", self.visible)
        } else {
            format!("{} visible messages of {}", self.visible, self.total)
        }
    }

    /// The row's segment text `seg m1..m10 · 8.1k chars`.
    pub fn segment_label(start: usize, end: usize, chars: usize) -> String {
        format!("seg m{start}..m{end} · {} chars", format_chars(chars))
    }
}

/// One rendered message of a read.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadCard {
    pub ordinal: usize,
    pub role: &'static str,
    pub line: usize,
    pub anchor: String,
    pub slice: Option<String>,
    pub body: String,
    pub focused: bool,
    pub subagent: bool,
    pub info: Option<AgentConversationInfo>,
}

impl ReadCard {
    /// `m12 · assistant · line 88 · ma_…`
    pub fn header(&self) -> String {
        format!(
            "m{} · {} · line {} · {}",
            self.ordinal, self.role, self.line, self.anchor
        )
    }
}

/// Cards of one conversation.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadGroup {
    pub info: Option<AgentConversationInfo>,
    pub conversation_ref: String,
    pub title: Option<String>,
    pub cards: Vec<ReadCard>,
}

/// The Read tab's result: cards grouped by conversation.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadModel {
    pub groups: Vec<ReadGroup>,
    pub hint: ContentVisibilityHint,
    /// Single-ref read: the range the cards cover (paging extends it).
    pub range: Option<(usize, usize)>,
    /// The `continue read` lines of the agent text (cut by the output
    /// budget), as runnable commands (D-021).
    pub continue_commands: Vec<String>,
}

impl ReadModel {
    pub fn from_result(result: &AgentReadResult, titles: &BTreeMap<String, String>) -> ReadModel {
        let mut groups: Vec<ReadGroup> = Vec::new();
        for message in &result.messages {
            let info = result
                .conversations
                .iter()
                .find(|conversation| conversation.full_ref == message.conversation_full_ref);
            let card = card_of(message, info);
            match groups
                .iter_mut()
                .find(|group| group.conversation_ref == message.conversation_ref)
            {
                Some(group) => group.cards.push(card),
                None => groups.push(ReadGroup {
                    info: info.cloned(),
                    conversation_ref: message.conversation_ref.clone(),
                    title: titles.get(&message.conversation_full_ref).cloned(),
                    cards: vec![card],
                }),
            }
        }
        let range = (groups.len() == 1)
            .then(|| {
                let ordinals = groups[0].cards.iter().map(|card| card.ordinal);
                Some((ordinals.clone().min()?, ordinals.max()?))
            })
            .flatten();
        ReadModel {
            groups,
            hint: ContentVisibilityHint {
                tools: result.options.tools,
                tool_results: result.options.tool_results,
                thinking: result.options.thinking,
                subagents: result.options.subagents,
            },
            range,
            continue_commands: Vec::new(),
        }
    }
}

/// The commands behind the `continue read ref=…` / `continue read refs=a;b`
/// lines of an agent text: `claude-history agent read a b`.
pub fn continue_commands(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("continue read ")?;
            let (_, refs) = rest.split_once('=')?;
            let refs = refs.trim_matches('"').replace("%3B", ";");
            let refs: Vec<String> = refs
                .split(';')
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect();
            (!refs.is_empty()).then(|| format!("claude-history agent read {}", refs.join(" ")))
        })
        .collect()
}

fn card_of(message: &ReadMessage, info: Option<&AgentConversationInfo>) -> ReadCard {
    ReadCard {
        ordinal: message.ordinal,
        role: role_label(message.role),
        line: message.jsonl_line,
        anchor: message.anchor.clone(),
        slice: message.slice.clone(),
        body: message.body.clone(),
        focused: message.focused,
        subagent: message.subagent,
        info: info.cloned(),
    }
}

/// The warnings banner text: `3 warnings: malformed-transcript 2, io 1`,
/// deduplicated by (kind, reference) in the order Skipped,
/// MalformedTranscript, Io, SemanticUnavailable (D-013).
pub fn warnings_summary(warnings: &[AgentWarning]) -> Option<(String, Vec<String>)> {
    let mut seen = HashSet::new();
    let mut by_kind: BTreeMap<_, Vec<&AgentWarning>> = BTreeMap::new();
    for warning in warnings {
        if seen.insert((warning.kind, warning.reference.clone())) {
            by_kind.entry(warning.kind).or_default().push(warning);
        }
    }
    let total: usize = by_kind.values().map(Vec::len).sum();
    if total == 0 {
        return None;
    }
    let counts: Vec<String> = by_kind
        .iter()
        .map(|(kind, list)| format!("{} {}", kind.as_str(), list.len()))
        .collect();
    let noun = if total == 1 { "warning" } else { "warnings" };
    let details = by_kind
        .iter()
        .flat_map(|(kind, list)| {
            list.iter().map(move |warning| {
                let reference = warning
                    .reference
                    .as_deref()
                    .map(|reference| format!(" {reference}"))
                    .unwrap_or_default();
                format!("{}{}: {}", kind.as_str(), reference, warning.detail)
            })
        })
        .collect();
    Some((format!("{total} {noun}: {}", counts.join(", ")), details))
}

/// The uncached-passages banner (D-006) of a response, if any.
pub fn uncached_banner(notices: &[AgentNotice]) -> Option<String> {
    notices.iter().rev().find_map(|notice| match notice {
        AgentNotice::UncachedPassages {
            count,
            bounded_top_up: false,
        } => Some(format!(
            "{count} passage(s) are not cached. Generate the semantic cache to include them."
        )),
        AgentNotice::UncachedPassages {
            count,
            bounded_top_up: true,
        } => Some(format!(
            "{count} passage(s) remain uncached after the bounded top-up. Run the search again to cache more."
        )),
        AgentNotice::SemanticProgress(_) => None,
    })
}

/// The status line of a progress notice (`Embedding 12/30`).
pub fn progress_text(notice: &AgentNotice) -> Option<String> {
    use claude_history_core::semantic::index::SemanticIndexProgress as Progress;
    match notice {
        AgentNotice::SemanticProgress(progress) => Some(match progress {
            Progress::Embedding { completed, total } => format!("Embedding {completed}/{total}"),
            Progress::CacheReady => "Cache ready".to_string(),
            Progress::Ranking => "Ranking".to_string(),
            Progress::Complete => "Complete".to_string(),
            Progress::EmptyCorpus => "Nothing to search".to_string(),
        }),
        AgentNotice::UncachedPassages { .. } => None,
    }
}

/// The ambiguous-ref candidates: the `ch_…` handles at the start of each
/// candidate line of the error detail.
pub fn ambiguous_candidates(detail: &str) -> Vec<String> {
    let list = detail
        .split_once("candidates:")
        .map(|(_, rest)| rest)
        .unwrap_or_default();
    list.lines()
        .filter_map(|line| {
            let handle = line.split_whitespace().next()?;
            handle.starts_with("ch_").then(|| handle.to_string())
        })
        .collect()
}

/// How a failed response is shown.
#[derive(Clone, Debug, PartialEq)]
pub struct ErrorModel {
    /// `invalid-ref: <detail>` or the CLI text of an untyped error.
    pub text: String,
    pub kind: Option<AgentErrorKind>,
    pub candidates: Vec<String>,
}

/// The text of a cancelled search shown instead of an error.
pub const CANCELLED_TEXT: &str = "Search cancelled";

impl ErrorModel {
    pub fn from_response(response: &AgentResponse) -> Option<ErrorModel> {
        if response.is_success() {
            return None;
        }
        Some(match &response.error {
            Some(error) => ErrorModel {
                text: format!("{}: {}", error.kind.as_str(), error.detail),
                kind: Some(error.kind),
                candidates: if error.kind == AgentErrorKind::AmbiguousRef {
                    ambiguous_candidates(&error.detail)
                } else {
                    Vec::new()
                },
            },
            None => ErrorModel {
                text: response
                    .error_text()
                    .trim()
                    .trim_start_matches("Error: ")
                    .to_string(),
                kind: None,
                candidates: Vec::new(),
            },
        })
    }
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/// What the results area shows besides the rows.
#[derive(Clone, Debug, PartialEq)]
pub enum RunState {
    Idle,
    /// `Searching…` / `Reading…` with the latest progress line.
    Running {
        label: &'static str,
        progress: Option<String>,
    },
    Cancelled,
    Done,
}

/// The body of a result set.
#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    None,
    Search(SearchModel),
    Outline(OutlineModel),
    Read(Rc<ReadModel>),
}

/// An action of the hit context menu (§5.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitAction {
    Open,
    SearchWithin,
    Outline,
    ReadRange,
    CopyReadCommand,
    CopyAgentRecords,
    CopyRef,
    CopyAnchors,
}

impl HitAction {
    pub const ALL: [HitAction; 8] = [
        HitAction::Open,
        HitAction::SearchWithin,
        HitAction::Outline,
        HitAction::ReadRange,
        HitAction::CopyReadCommand,
        HitAction::CopyAgentRecords,
        HitAction::CopyRef,
        HitAction::CopyAnchors,
    ];

    pub fn label(self) -> &'static str {
        match self {
            HitAction::Open => "Open at message",
            HitAction::SearchWithin => "Search within conversation…",
            HitAction::Outline => "Outline",
            HitAction::ReadRange => "Read range…",
            HitAction::CopyReadCommand => "Copy read command",
            HitAction::CopyAgentRecords => "Copy as agent records",
            HitAction::CopyRef => "Copy ref",
            HitAction::CopyAnchors => "Copy anchors",
        }
    }
}

/// Events of a results view.
#[derive(Clone, Debug, PartialEq)]
pub enum ResultsEvent {
    /// Reveal a message in its conversation (with `tab`, the viewer's agent
    /// panel opens on that tab).
    Open {
        target: ConversationTarget,
        reveal: RevealRequest,
        focus_viewer: bool,
        tab: Option<AgentTab>,
    },
    /// An ambiguous-ref candidate was clicked: put its handle in the Refs
    /// field.
    FillRefs(String),
    /// Read paging: extend the range by 5 messages before / after.
    LoadMore { previous: bool },
    /// The selected row changed: the conversation of its result set (what
    /// session actions act on while the panel has focus, §7.8).
    Selection(Option<ConversationTarget>),
}

/// One row of the virtualized Read list. The element index of an item is
/// its `Selectable::child`.
#[derive(Clone, Debug, PartialEq)]
pub enum ReadItem {
    /// The header of conversation group `group`.
    Header(usize),
    Card {
        group: usize,
        card: usize,
        /// Index among the selectable rows.
        selectable: usize,
    },
    /// A `continue read` line (index into `continue_commands`).
    Continue(usize),
    /// Load previous / Load next.
    Paging,
}

/// Selectable item of a body.
#[derive(Clone, Debug, PartialEq)]
struct Selectable {
    /// The element index of the row inside the scrolling list.
    child: usize,
    reveal: Option<RevealSpec>,
    /// The hit behind a search row.
    hit: Option<usize>,
}

/// Rows, banners and the agent-text toggle of one result set.
pub struct ResultsView {
    id: &'static str,
    /// `DeepResults` or `AgentRows`.
    key_context: &'static str,
    deep: bool,
    focus: FocusHandle,
    scroll: ScrollHandle,
    state: RunState,
    response: Option<AgentResponse>,
    body: Body,
    error: Option<ErrorModel>,
    warnings: Option<(String, Vec<String>)>,
    warnings_open: bool,
    uncached: Option<String>,
    selectables: Vec<Selectable>,
    selected: Option<usize>,
    expanded: HashSet<usize>,
    deleted: Vec<PathBuf>,
    text: AgentTextView,
    /// Paging buttons of a single-ref read: the range they extend.
    paging: Option<(usize, usize)>,
    /// The Read body's virtualized rows (rule 18): a read has one card per
    /// message and an unbudgeted result can have thousands.
    read_list: ListState,
    read_items: Rc<Vec<ReadItem>>,
    /// The latest keystroke was `secondary-enter` (the window's modifier
    /// state lags behind keystrokes that are not preceded by a modifier
    /// event, e.g. scripted ones).
    enter_secondary: bool,
    _keystrokes: Option<gpui_kit::Subscription>,
}

impl EventEmitter<ResultsEvent> for ResultsView {}

impl Focusable for ResultsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ResultsView {
    /// `deep`: the Deep search tab's list (`DeepResults` keys); otherwise
    /// the agent panel's (`AgentRows`).
    pub fn new(id: &'static str, deep: bool, cx: &mut Context<Self>) -> Self {
        let keystrokes = deep.then(|| {
            let view = cx.entity().downgrade();
            cx.intercept_keystrokes(move |event, _, cx| {
                let secondary =
                    event.keystroke.key == "enter" && event.keystroke.modifiers.secondary();
                view.update(cx, |this, _| this.enter_secondary = secondary)
                    .ok();
            })
        });
        Self {
            id,
            key_context: if deep {
                crate::keymap::CTX_DEEP_RESULTS
            } else {
                crate::keymap::CTX_AGENT_ROWS
            },
            deep,
            focus: cx.focus_handle().tab_stop(true),
            scroll: ScrollHandle::new(),
            state: RunState::Idle,
            response: None,
            body: Body::None,
            error: None,
            warnings: None,
            warnings_open: false,
            uncached: None,
            selectables: Vec::new(),
            selected: None,
            expanded: HashSet::new(),
            deleted: Vec::new(),
            text: AgentTextView::new(),
            paging: None,
            read_list: ListState::new(0, ListAlignment::Top, px(400.)),
            read_items: Rc::new(Vec::new()),
            enter_secondary: false,
            _keystrokes: keystrokes,
        }
    }

    // ----- state ------------------------------------------------------------------

    pub fn state(&self) -> &RunState {
        &self.state
    }

    pub fn is_running(&self) -> bool {
        matches!(self.state, RunState::Running { .. })
    }

    pub fn body(&self) -> &Body {
        &self.body
    }

    pub fn response(&self) -> Option<&AgentResponse> {
        self.response.as_ref()
    }

    pub fn error(&self) -> Option<&ErrorModel> {
        self.error.as_ref()
    }

    pub fn warnings_banner(&self) -> Option<&str> {
        self.warnings.as_ref().map(|(summary, _)| summary.as_str())
    }

    /// Whether the warnings list is expanded (the toggle's state).
    pub fn warnings_expanded(&self) -> bool {
        self.warnings_open
    }

    pub fn warning_details(&self) -> Vec<String> {
        self.warnings
            .as_ref()
            .map(|(_, details)| details.clone())
            .unwrap_or_default()
    }

    pub fn uncached_text(&self) -> Option<&str> {
        self.uncached.as_deref()
    }

    pub fn text_view(&self) -> &AgentTextView {
        &self.text
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// The effective mode of the shown search or within response (`exact`,
    /// `lexical`, `semantic`, `hybrid`): the text of the mode badge.
    pub fn mode_badge_text(&self) -> Option<String> {
        match &self.body {
            Body::Search(model) => Some(model.mode.as_str().to_string()),
            _ => None,
        }
    }

    /// The status line (`Searching…`, progress, `Search cancelled`).
    pub fn status_text(&self) -> String {
        match &self.state {
            RunState::Idle | RunState::Done => String::new(),
            RunState::Running { label, progress } => match progress {
                Some(progress) => format!("{label} {progress}"),
                None => (*label).to_string(),
            },
            RunState::Cancelled => CANCELLED_TEXT.to_string(),
        }
    }

    /// A run started: the old rows stay until the answer replaces them.
    pub fn set_running(&mut self, label: &'static str, cx: &mut Context<Self>) {
        self.state = RunState::Running {
            label,
            progress: None,
        };
        self.error = None;
        cx.notify();
    }

    pub fn set_progress(&mut self, text: String, cx: &mut Context<Self>) {
        if let RunState::Running { progress, .. } = &mut self.state {
            *progress = Some(text);
            cx.notify();
        }
    }

    pub fn set_cancelled(&mut self, cx: &mut Context<Self>) {
        self.state = RunState::Cancelled;
        self.error = None;
        cx.notify();
    }

    /// Forget the result set.
    pub fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state = RunState::Idle;
        self.response = None;
        self.body = Body::None;
        self.error = None;
        self.warnings = None;
        self.uncached = None;
        self.selectables.clear();
        self.selected = None;
        self.expanded.clear();
        self.paging = None;
        self.rebuild_read_items();
        self.text.set_response(None, window, cx);
        self.announce_selection(cx);
        cx.notify();
    }

    /// The answer of the run. `titles` names the conversations of a read
    /// (full ref → title).
    pub fn set_response(
        &mut self,
        response: AgentResponse,
        titles: &BTreeMap<String, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.text.set_response(Some(&response), window, cx);
        self.warnings = warnings_summary(&response.warnings);
        self.warnings_open = false;
        self.uncached = uncached_banner(&response.notices);
        self.expanded.clear();
        self.paging = None;
        if response.is_cancelled() {
            self.state = RunState::Cancelled;
            self.error = None;
            self.response = Some(response);
            cx.notify();
            return;
        }
        self.state = RunState::Done;
        self.error = ErrorModel::from_response(&response);
        match &response.result {
            Some(AgentResult::Search(result)) | Some(AgentResult::Within(result)) => {
                // D-010: the project names of the conversation list.
                let list = crate::globals::main_panes(cx).list();
                let lookup = |path: &std::path::Path| {
                    let list = list.as_ref()?;
                    list.read(cx).conversation(path)?.project_name.clone()
                };
                self.body = Body::Search(SearchModel::from_result_with(result, Some(&lookup)));
            }
            Some(AgentResult::Outline(result)) => {
                self.body = Body::Outline(OutlineModel::from_result(result));
            }
            Some(AgentResult::Read(result)) => {
                let mut model = ReadModel::from_result(result, titles);
                model.continue_commands = continue_commands(response.text());
                self.body = Body::Read(Rc::new(model));
            }
            None => self.body = Body::None,
        }
        self.response = Some(response);
        self.rebuild_selectables();
        self.rebuild_read_items();
        self.selected = (!self.selectables.is_empty()).then_some(0);
        self.scroll.scroll_to_top_of_item(0);
        self.read_list.scroll_to(gpui_kit::ListOffset::default());
        self.announce_selection(cx);
        cx.notify();
    }

    /// The range the Load previous / Load next buttons extend (single-ref
    /// reads), or `None` for no paging row.
    pub fn set_paging(&mut self, paging: Option<(usize, usize)>, cx: &mut Context<Self>) {
        self.paging = paging;
        self.rebuild_read_items();
        cx.notify();
    }

    /// The rows of the Read list: group headers and cards, then the
    /// `continue read` lines and the paging row.
    fn rebuild_read_items(&mut self) {
        let mut items = Vec::new();
        if let Body::Read(model) = &self.body {
            let mut selectable = 0;
            for (group_ix, group) in model.groups.iter().enumerate() {
                items.push(ReadItem::Header(group_ix));
                for card_ix in 0..group.cards.len() {
                    items.push(ReadItem::Card {
                        group: group_ix,
                        card: card_ix,
                        selectable,
                    });
                    selectable += 1;
                }
            }
            items.extend((0..model.continue_commands.len()).map(ReadItem::Continue));
            if self.paging.is_some() {
                items.push(ReadItem::Paging);
            }
        }
        self.read_list.reset(items.len());
        self.read_items = Rc::new(items);
    }

    /// Result sets of deleted conversations are marked and their rows no
    /// longer open anything.
    pub fn mark_deleted(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        self.deleted.extend(paths.iter().cloned());
        cx.notify();
    }

    /// Whether `path` was deleted while this result set was shown.
    pub fn is_path_deleted(&self, path: &std::path::Path) -> bool {
        self.deleted
            .iter()
            .any(|deleted| crate::globals::same_path(deleted, path))
    }

    fn is_deleted(&self, info: Option<&AgentConversationInfo>) -> bool {
        info.is_some_and(|info| {
            self.deleted
                .iter()
                .any(|path| crate::globals::same_path(path, &info.path))
        })
    }

    /// Every conversation path of the result.
    pub fn conversation_paths(&self) -> Vec<PathBuf> {
        match &self.body {
            Body::None => Vec::new(),
            Body::Search(model) => model
                .rows
                .iter()
                .filter_map(|row| match row {
                    SearchRow::Group(group) => group.info.as_ref(),
                    SearchRow::Hit(hit) => hit.info.as_ref(),
                })
                .map(|info| info.path.clone())
                .collect(),
            Body::Outline(model) => vec![model.info.path.clone()],
            Body::Read(model) => model
                .groups
                .iter()
                .filter_map(|group| group.info.as_ref())
                .map(|info| info.path.clone())
                .collect(),
        }
    }

    fn rebuild_selectables(&mut self) {
        let mut selectables = Vec::new();
        match &self.body {
            Body::None => {}
            Body::Search(model) => {
                let mut hit_index = 0;
                // The within target line comes before the rows.
                let offset = usize::from(model.target_line.is_some());
                for (child, row) in model.rows.iter().enumerate() {
                    if let SearchRow::Hit(hit) = row {
                        selectables.push(Selectable {
                            child: child + offset,
                            reveal: hit.reveal(),
                            hit: Some(hit_index),
                        });
                        hit_index += 1;
                    }
                }
            }
            Body::Outline(model) => {
                for (child, row) in model.rows.iter().enumerate() {
                    let ordinal = match row {
                        OutlineRow::Message { ordinal, .. } => *ordinal,
                        OutlineRow::Segment { start, .. } => *start,
                    };
                    selectables.push(Selectable {
                        child,
                        reveal: Some(RevealSpec {
                            info: model.info.clone(),
                            title: None,
                            ordinal,
                            hint: model.hint,
                        }),
                        hit: None,
                    });
                }
            }
            Body::Read(model) => {
                let mut child = 0;
                for group in &model.groups {
                    child += 1; // the conversation header
                    for card in &group.cards {
                        selectables.push(Selectable {
                            child,
                            reveal: card.info.clone().map(|info| RevealSpec {
                                info,
                                title: group.title.clone(),
                                ordinal: card.ordinal,
                                hint: model.hint,
                            }),
                            hit: None,
                        });
                        child += 1;
                    }
                }
            }
        }
        self.selectables = selectables;
    }

    // ----- selection and opening -----------------------------------------------------

    /// Number of selectable rows (hits, outline rows, read cards).
    pub fn selectable_count(&self) -> usize {
        self.selectables.len()
    }

    pub fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.selectables.len() {
            self.selected = Some(index);
            let child = self.selectables[index].child;
            if matches!(self.body, Body::Read(_)) {
                self.read_list.scroll_to_reveal_item(child);
            } else {
                self.scroll.scroll_to_item(child);
            }
            self.announce_selection(cx);
            cx.notify();
        }
    }

    fn announce_selection(&mut self, cx: &mut Context<Self>) {
        let target = self.selected_reveal().map(|reveal| reveal.request().0);
        cx.emit(ResultsEvent::Selection(target));
    }

    pub fn select_next(&mut self, cx: &mut Context<Self>) {
        let next = self.selected.map_or(0, |ix| ix + 1);
        self.select(next.min(self.selectables.len().saturating_sub(1)), cx);
    }

    pub fn select_prev(&mut self, cx: &mut Context<Self>) {
        let prev = self.selected.map_or(0, |ix| ix.saturating_sub(1));
        self.select(prev, cx);
    }

    /// The reveal spec of the selected row.
    pub fn selected_reveal(&self) -> Option<RevealSpec> {
        self.selectables
            .get(self.selected?)
            .and_then(|item| item.reveal.clone())
    }

    /// The selected search hit.
    pub fn selected_hit(&self) -> Option<HitModel> {
        let index = self.selectables.get(self.selected?)?.hit?;
        match &self.body {
            Body::Search(model) => model.hit(index).cloned(),
            _ => None,
        }
    }

    /// Open the selected row (Enter; with `focus_viewer` Ctrl+Enter).
    pub fn open_selected(&mut self, focus_viewer: bool, cx: &mut Context<Self>) {
        let Some(index) = self.selected else { return };
        self.open_index(index, focus_viewer, None, cx);
    }

    /// Open selectable `index`.
    pub fn open_index(
        &mut self,
        index: usize,
        focus_viewer: bool,
        tab: Option<AgentTab>,
        cx: &mut Context<Self>,
    ) {
        let Some(reveal) = self
            .selectables
            .get(index)
            .and_then(|item| item.reveal.clone())
        else {
            return;
        };
        if self.is_deleted(Some(&reveal.info)) {
            return;
        }
        let (target, request) = reveal.request();
        cx.emit(ResultsEvent::Open {
            target,
            reveal: request,
            focus_viewer,
            tab,
        });
    }

    /// A context-menu or button action on search hit `hit_index`.
    pub fn hit_action(
        &mut self,
        hit_index: usize,
        action: HitAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Body::Search(model) = &self.body else {
            return;
        };
        let Some(hit) = model.hit(hit_index).cloned() else {
            return;
        };
        let selectable = self
            .selectables
            .iter()
            .position(|item| item.hit == Some(hit_index));
        if let Some(selectable) = selectable {
            self.selected = Some(selectable);
        }
        let copy = |text: String, what: &str, window: &mut Window, cx: &mut Context<Self>| {
            services(cx).clipboard_write(text, cx);
            notify::info(window, cx, format!("Copied {what}"));
        };
        match action {
            HitAction::Open => {
                if let Some(selectable) = selectable {
                    self.open_index(selectable, false, None, cx);
                }
            }
            HitAction::SearchWithin | HitAction::Outline | HitAction::ReadRange => {
                if let Some(selectable) = selectable {
                    let tab = match action {
                        HitAction::SearchWithin => AgentTab::Within,
                        HitAction::Outline => AgentTab::Outline,
                        _ => AgentTab::Read,
                    };
                    self.open_index(selectable, false, Some(tab), cx);
                }
            }
            HitAction::CopyReadCommand => copy(hit.read_command(), "read command", window, cx),
            HitAction::CopyAgentRecords => {
                let text = self
                    .response
                    .as_ref()
                    .map(AgentResponse::text)
                    .unwrap_or_default();
                match hit.agent_records(text) {
                    Some(records) => copy(records, "agent records", window, cx),
                    None => notify::warning(
                        window,
                        cx,
                        "That hit is not in the agent text (cut by the output budget)",
                    ),
                }
            }
            HitAction::CopyRef => copy(hit.full_ref.clone(), "ref", window, cx),
            HitAction::CopyAnchors => copy(hit.anchors.join(","), "anchors", window, cx),
        }
    }

    /// Copy the full `ch_` ref of a group or hit conversation.
    pub fn copy_ref(&mut self, full_ref: String, window: &mut Window, cx: &mut Context<Self>) {
        services(cx).clipboard_write(full_ref, cx);
        notify::info(window, cx, "Copied ref");
    }

    /// Copy a read command (the `continue read` line, D-021).
    pub fn copy_command(&mut self, command: String, window: &mut Window, cx: &mut Context<Self>) {
        services(cx).clipboard_write(command, cx);
        notify::info(window, cx, "Copied read command");
    }

    /// Copy one anchor (`ma_…`).
    pub fn copy_anchor(&mut self, anchor: String, window: &mut Window, cx: &mut Context<Self>) {
        services(cx).clipboard_write(anchor, cx);
        notify::info(window, cx, "Copied anchor");
    }

    pub fn set_text_shown(&mut self, shown: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.text.set_shown(shown, window, cx);
        cx.notify();
    }

    pub fn toggle_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = !self.text.is_shown();
        self.set_text_shown(shown, window, cx);
    }

    pub fn copy_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.text.copy(cx);
        notify::info(window, cx, "Copied agent text output");
    }

    /// Focus the rows (Tab stop of the list).
    pub fn focus_rows(&self, window: &mut Window, cx: &mut App) {
        self.focus.focus(window, cx);
    }

    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
    }

    /// Probes (§15.3): counts of the current body.
    pub fn counts(&self) -> (usize, usize) {
        match &self.body {
            Body::Search(model) => (model.groups, model.hits),
            _ => (0, 0),
        }
    }

    // ----- actions ------------------------------------------------------------------

    fn on_next(&mut self, _: &DeepSelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.select_next(cx);
    }

    fn on_prev(&mut self, _: &DeepSelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        self.select_prev(cx);
    }

    fn on_open(&mut self, _: &DeepOpenHit, window: &mut Window, cx: &mut Context<Self>) {
        // `secondary-enter` also opens the hit and moves focus to the viewer.
        let secondary = window.modifiers().secondary() || self.enter_secondary;
        self.open_selected(secondary, cx);
    }

    fn on_agent_next(&mut self, _: &AgentSelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.select_next(cx);
    }

    fn on_agent_prev(&mut self, _: &AgentSelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        self.select_prev(cx);
    }

    fn on_agent_open(&mut self, _: &AgentOpenSelected, _: &mut Window, cx: &mut Context<Self>) {
        // The agent panel's Enter always focuses the viewer (the message is
        // shown there).
        self.open_selected(true, cx);
    }

    fn on_toggle_text(&mut self, _: &ToggleAgentText, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_text(window, cx);
    }

    fn click_row(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.select(index, cx);
        self.focus.focus(window, cx);
        self.open_index(index, false, None, cx);
    }

    fn toggle_segment(&mut self, child: usize, cx: &mut Context<Self>) {
        if !self.expanded.remove(&child) {
            self.expanded.insert(child);
        }
        cx.notify();
    }
}

// ----- rendering ----------------------------------------------------------------------

impl Render for ResultsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let border = theme.border;
        let muted = theme.muted_foreground;
        let roles = theme_tokens::roles(cx);
        let id = self.id;
        let view = cx.entity();

        let mut column = v_flex().id(id).size_full().min_h_0();

        // Status line while running or after a cancel.
        let status = self.status_text();
        if !status.is_empty() {
            column = column.child(
                h_flex()
                    .id(SharedString::from(format!("{id}-status")))
                    .gap_2()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(muted)
                    .child(Icon::new(IconName::Info).xsmall())
                    .child(status),
            );
        }

        // The effective mode badge (D-003/D-004): the mode the response ran
        // in, which is not the form's choice (quoted-only queries force
        // exact, Default resolves to the config).
        if let Some(text) = self.mode_badge_text() {
            column = column.child(
                h_flex()
                    .id(SharedString::from(format!("{id}-mode")))
                    .qa_id(format!("{id}:mode"))
                    .px_2()
                    .py_0p5()
                    .gap_1()
                    .items_center()
                    .text_xs()
                    .text_color(muted)
                    .child("Mode")
                    .child(
                        div()
                            .px_1()
                            .rounded_sm()
                            .border_1()
                            .border_color(border)
                            .font_family(cx.theme().mono_font_family.clone())
                            .child(text),
                    ),
            );
        }
        // Hints and banners.
        if let Body::Search(model) = &self.body
            && model.exact_forced
        {
            column = column.child(banner(
                format!("{id}-exact"),
                IconName::Info,
                "Exact match forced (quoted-only query)".to_string(),
                muted,
                border,
            ));
        }
        if let Some((summary, details)) = &self.warnings {
            let open = self.warnings_open;
            let mut block = v_flex()
                .id(SharedString::from(format!("{id}-warnings")))
                .px_2()
                .py_1()
                .gap_0p5()
                .border_b_1()
                .border_color(border)
                .text_xs()
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .text_color(roles.warning)
                        .child(Icon::new(IconName::TriangleAlert).xsmall())
                        .child(div().flex_1().child(summary.clone()))
                        .child(
                            Button::new(SharedString::from(format!("{id}-warnings-toggle")))
                                .icon(if open {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .ghost()
                                .xsmall()
                                .accessibility_label("Show warnings")
                                .tooltip("Show warnings")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.warnings_open = !this.warnings_open;
                                    cx.notify();
                                })),
                        ),
                );
            if open {
                for detail in details {
                    block = block.child(div().text_color(muted).child(detail.clone()));
                }
            }
            column = column.child(block);
        }
        if let Some(text) = &self.uncached {
            let generate = !text.contains("remain uncached");
            column = column.child(
                h_flex()
                    .id(SharedString::from(format!("{id}-uncached")))
                    .px_2()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(muted)
                    .child(Icon::new(IconName::Info).xsmall())
                    .child(div().flex_1().child(text.clone()))
                    .when(generate, |this| {
                        this.child(
                            Button::new(SharedString::from(format!("{id}-generate")))
                                .label("Generate semantic cache…")
                                .xsmall()
                                .outline()
                                .on_click(|_, window, cx| request_generate_cache(window, cx)),
                        )
                    }),
            );
        }
        if let Some(error) = &self.error {
            let candidates = error.candidates.clone();
            let mut block = v_flex()
                .id(SharedString::from(format!("{id}-error")))
                .px_2()
                .py_1()
                .gap_1()
                .border_b_1()
                .border_color(border)
                .text_xs()
                .text_color(roles.danger)
                .child(
                    h_flex()
                        .gap_1()
                        .items_start()
                        .child(Icon::new(IconName::CircleAlert).xsmall())
                        .child(div().flex_1().child(error.text.clone())),
                );
            for (ix, candidate) in candidates.into_iter().enumerate() {
                let handle = candidate.clone();
                block = block.child(
                    Button::new(SharedString::from(format!("{id}-candidate-{ix}")))
                        .label(candidate)
                        .link()
                        .xsmall()
                        .tooltip("Use this conversation")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(ResultsEvent::FillRefs(handle.clone()));
                        })),
                );
            }
            column = column.child(block);
        }

        // Body: the text view or the rows.
        if self.text.is_shown() {
            column = column.child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px_2()
                    .child(self.text.body(self.static_id("text"), cx)),
            );
        } else {
            // Always rendered (even empty) so the row list can take focus.
            column = column.child(self.render_rows(&view, window, cx));
        }

        // Footer: stats and the text toggle.
        let footer_text = match &self.body {
            Body::Search(model) => Some(model.stats_line()),
            Body::Outline(model) => Some(model.summary()),
            _ => None,
        };
        let has_response = self.response.is_some();
        column = column.child(
            v_flex()
                .flex_none()
                .gap_1()
                .px_2()
                .py_1()
                .border_t_1()
                .border_color(border)
                .when_some(footer_text, |this, text| {
                    this.child(
                        div()
                            .id(SharedString::from(format!("{id}-stats")))
                            .text_xs()
                            .text_color(muted)
                            .child(text),
                    )
                })
                .child(toggle_bar(
                    self.static_id("toggle"),
                    self.text.is_shown(),
                    has_response,
                    |this: &mut ResultsView, shown, window, cx| {
                        this.set_text_shown(shown, window, cx)
                    },
                    |this: &mut ResultsView, window, cx| this.copy_text(window, cx),
                    cx,
                )),
        );
        column
    }
}

/// The uncached banner's button: Tools → Generate semantic cache….
pub fn request_generate_cache(window: &mut Window, cx: &mut App) {
    window.dispatch_action(Box::new(GenerateSemanticCache), cx);
}

/// An info banner line.
fn banner(
    id: String,
    icon: IconName,
    text: String,
    muted: gpui_kit::Hsla,
    border: gpui_kit::Hsla,
) -> impl IntoElement {
    h_flex()
        .id(SharedString::from(id))
        .px_2()
        .py_1()
        .gap_1()
        .items_center()
        .border_b_1()
        .border_color(border)
        .text_xs()
        .text_color(muted)
        .child(Icon::new(icon).xsmall())
        .child(text)
}

impl ResultsView {
    /// Element ids must be `'static` strings for the shared helpers.
    fn static_id(&self, part: &'static str) -> &'static str {
        match (self.deep, part) {
            (true, "text") => "deep-text",
            (true, _) => "deep-toggle",
            (false, "text") => "agent-text",
            (false, _) => "agent-toggle",
        }
    }

    fn render_rows(
        &self,
        view: &Entity<ResultsView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let border = theme.border;
        let roles = theme_tokens::roles(cx);
        let mono = theme.mono_font_family.clone();
        let selected_child = self
            .selected
            .and_then(|ix| self.selectables.get(ix))
            .map(|item| item.child);
        let focused = self.focus.is_focused(window);
        let mut rows: Vec<gpui_kit::AnyElement> = Vec::new();
        let mut read_list: Option<gpui_kit::AnyElement> = None;
        let ctx = self.key_context;
        let id = self.id;
        match &self.body {
            Body::None => {}
            Body::Search(model) => {
                if let Some(line) = &model.target_line {
                    rows.push(
                        div()
                            .id(SharedString::from(format!("{id}-target")))
                            .px_2()
                            .py_1()
                            .text_xs()
                            .font_family(mono.clone())
                            .text_color(roles.muted_foreground)
                            .child(line.clone())
                            .into_any_element(),
                    );
                }
                let offset = rows.len();
                if model.rows.is_empty() {
                    rows.push(
                        div()
                            .id(SharedString::from(format!("{id}-empty")))
                            .px_2()
                            .py_2()
                            .text_sm()
                            .text_color(roles.muted_foreground)
                            .child("No hits")
                            .into_any_element(),
                    );
                }
                let mut hit_index = 0;
                let mut selectable = 0;
                for (child, row) in model.rows.iter().enumerate() {
                    match row {
                        SearchRow::Group(group) => rows.push(group_card(
                            id,
                            selectable,
                            group,
                            self.is_deleted(group.info.as_ref()),
                            view,
                            &roles,
                            &mono,
                            border,
                        )),
                        SearchRow::Hit(hit) => {
                            let is_selected = selected_child == Some(child + offset);
                            rows.push(hit_row(
                                id,
                                hit_index,
                                selectable,
                                hit,
                                model.flat || model.within,
                                self.deep,
                                is_selected,
                                focused,
                                self.is_deleted(hit.info.as_ref()),
                                view,
                                &roles,
                                &mono,
                            ));
                            hit_index += 1;
                            selectable += 1;
                        }
                    }
                }
            }
            Body::Outline(model) => {
                for (child, row) in model.rows.iter().enumerate() {
                    rows.push(outline_row(
                        id,
                        child,
                        row,
                        selected_child == Some(child),
                        focused,
                        self.expanded.contains(&child),
                        &model.info,
                        model.hint,
                        self.is_deleted(Some(&model.info)),
                        view,
                        &roles,
                        &mono,
                    ));
                }
            }
            Body::Read(model) => {
                let model = model.clone();
                let items = self.read_items.clone();
                let deleted = self.deleted.clone();
                let paging = self.paging;
                let view = view.clone();
                let mono = mono.clone();
                read_list = Some(
                    list(self.read_list.clone(), move |ix, _, _| {
                        let Some(item) = items.get(ix) else {
                            return div().into_any_element();
                        };
                        read_item_row(
                            id,
                            item,
                            &model,
                            selected_child == Some(ix),
                            focused,
                            &deleted,
                            paging,
                            &view,
                            &roles,
                            &mono,
                        )
                    })
                    .size_full()
                    .into_any_element(),
                );
            }
        }
        let is_read = read_list.is_some();
        let container = v_flex()
            .id(SharedString::from(format!("{id}-rows")))
            .key_context(ctx)
            .track_focus(&self.focus)
            .size_full()
            .min_h_0()
            .on_action(cx.listener(Self::on_next))
            .on_action(cx.listener(Self::on_prev))
            .on_action(cx.listener(Self::on_open))
            .on_action(cx.listener(Self::on_agent_next))
            .on_action(cx.listener(Self::on_agent_prev))
            .on_action(cx.listener(Self::on_agent_open))
            .on_action(cx.listener(Self::on_toggle_text));
        let body = match read_list {
            Some(list) => container.child(list),
            None => container
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .children(rows),
        };
        let frame = div().flex_1().min_h_0().relative().child(body);
        if is_read {
            frame
                .child(Scrollbar::vertical(&self.read_list))
                .into_any_element()
        } else {
            frame.vertical_scrollbar(&self.scroll).into_any_element()
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn group_card(
    id: &'static str,
    first_selectable: usize,
    group: &GroupModel,
    deleted: bool,
    view: &Entity<ResultsView>,
    roles: &theme_tokens::Roles,
    mono: &SharedString,
    border: gpui_kit::Hsla,
) -> gpui_kit::AnyElement {
    let full_ref = group.full_ref.clone();
    let copy_view = view.downgrade();
    let open_view = view.downgrade();
    v_flex()
        .id(SharedString::from(format!("{id}-group-{}", group.full_ref)))
        .role(gpui_kit::Role::ListItem)
        .aria_label(group.accessibility_label())
        .px_2()
        .pt_2()
        .pb_1()
        .gap_0p5()
        .border_t_1()
        .border_color(border)
        .bg(roles.sidebar)
        .child(
            h_flex()
                .gap_2()
                .items_baseline()
                .child(
                    div()
                        .text_xs()
                        .text_color(roles.muted_foreground)
                        .child(group.rank.to_string()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .font_medium()
                        .when(deleted, |this| this.line_through().opacity(0.6))
                        .child(group.title.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .font_family(mono.clone())
                        .text_color(roles.accent)
                        .child(group.score_text()),
                ),
        )
        .child(
            h_flex()
                .id(SharedString::from(format!(
                    "{id}-group-meta-{}",
                    group.full_ref
                )))
                .gap_1()
                .items_center()
                .text_xs()
                .text_color(roles.muted_foreground)
                .tooltip({
                    let tip = group.full_ref.clone();
                    move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                    }
                })
                .child(div().flex_1().min_w_0().truncate().child(group.meta()))
                .child(
                    Button::new(SharedString::from(format!(
                        "{id}-copy-ref-{}",
                        group.full_ref
                    )))
                    .icon(IconName::Copy)
                    .ghost()
                    .xsmall()
                    .accessibility_label("Copy ref")
                    .tooltip("Copy ref (full ch_ handle)")
                    .on_click(move |_, window, cx| {
                        let full_ref = full_ref.clone();
                        copy_view
                            .update(cx, |this, cx| this.copy_ref(full_ref, window, cx))
                            .ok();
                    }),
                ),
        )
        .on_click(move |_, window, cx| {
            // A group card opens its first hit.
            open_view
                .update(cx, |this, cx| {
                    if first_selectable < this.selectables.len() {
                        this.click_row(first_selectable, window, cx);
                    }
                })
                .ok();
        })
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn hit_row(
    id: &'static str,
    hit_index: usize,
    selectable: usize,
    hit: &HitModel,
    show_title: bool,
    deep: bool,
    selected: bool,
    focused: bool,
    deleted: bool,
    view: &Entity<ResultsView>,
    roles: &theme_tokens::Roles,
    mono: &SharedString,
) -> gpui_kit::AnyElement {
    let anchors = hit.anchors.join(",");
    // Stable domain key: the message range of its conversation.
    let key = format!("{}-m{}-m{}", hit.conversation_ref, hit.focus.0, hit.focus.1);
    let click_view = view.downgrade();
    let anchor_view = view.downgrade();
    let menu_view = view.clone();
    let background = if selected {
        Some(if focused {
            roles.list_active
        } else {
            roles.list_hover
        })
    } else {
        None
    };
    v_flex()
        .id(SharedString::from(format!("{id}-hit-{key}")))
        .qa_id(format!("{id}:hit:{hit_index}"))
        .role(gpui_kit::Role::ListItem)
        .aria_label(hit.accessibility_label())
        .tooltip({
            let tip = hit.tooltip();
            move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
            }
        })
        .px_2()
        .py_1()
        .gap_0p5()
        .when_some(background, |this, background| this.bg(background))
        .when(deleted, |this| this.opacity(0.5))
        .hover(|this| this.bg(roles.list_hover))
        .when(show_title, |this| {
            this.child(
                div()
                    .text_xs()
                    .font_medium()
                    .truncate()
                    .child(format!("{} · {}", hit.title, hit.project)),
            )
        })
        .child(
            h_flex()
                .gap_2()
                // Labels line up with the first line of a two-line preview.
                .items_start()
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .font_family(mono.clone())
                        .text_color(roles.accent)
                        .child(format!("▸ {}", hit.source)),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .font_family(mono.clone())
                        .child(hit.focus_label()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .font_family(mono.clone())
                        .text_color(roles.muted_foreground)
                        .child(hit.score_text()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(roles.muted_foreground)
                        // Two lines, the second ends with `…` (a long path
                        // has no break and was cut under the copy button).
                        .line_clamp(2)
                        .text_ellipsis()
                        .child(hit.preview.clone()),
                )
                .child(
                    Button::new(SharedString::from(format!("{id}-copy-anchor-{key}")))
                        .icon(IconName::Copy)
                        .ghost()
                        .xsmall()
                        .accessibility_label("Copy anchors")
                        .tooltip(format!("Copy anchors ({anchors})"))
                        .on_click(move |_, window, cx| {
                            let anchors = anchors.clone();
                            anchor_view
                                .update(cx, |this, cx| this.copy_anchor(anchors, window, cx))
                                .ok();
                        }),
                ),
        )
        .on_click(move |_: &ClickEvent, window, cx| {
            click_view
                .update(cx, |this, cx| this.click_row(selectable, window, cx))
                .ok();
        })
        // Findable by its domain id in gpui tests (a real click, D-011).
        .test_support()
        .context_menu(move |menu, window, cx| {
            hit_menu(menu, &menu_view, hit_index, deep, window, cx)
        })
        .into_any_element()
}

/// The element id of search hit `hit` in the results view `id` (tests).
pub fn hit_element_id(id: &str, hit: &HitModel) -> SharedString {
    SharedString::from(format!(
        "{id}-hit-{}-m{}-m{}",
        hit.conversation_ref, hit.focus.0, hit.focus.1
    ))
}

/// The hit context menu (§5.4).
fn hit_menu(
    mut menu: PopupMenu,
    view: &Entity<ResultsView>,
    hit_index: usize,
    deep: bool,
    window: &mut Window,
    _cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    for action in HitAction::ALL {
        // The agent panel's hits are already inside one conversation: its
        // own tabs are the Search within / Outline / Read controls.
        if !deep
            && matches!(
                action,
                HitAction::SearchWithin | HitAction::Outline | HitAction::ReadRange
            )
        {
            continue;
        }
        if matches!(action, HitAction::CopyReadCommand) {
            menu = menu.separator();
        }
        menu = menu.item(
            PopupMenuItem::new(action.label()).on_click(window.listener_for(
                view,
                move |this: &mut ResultsView, _, window, cx| {
                    this.hit_action(hit_index, action, window, cx)
                },
            )),
        );
    }
    menu
}

#[allow(clippy::too_many_arguments)]
fn outline_row(
    id: &'static str,
    child: usize,
    row: &OutlineRow,
    selected: bool,
    focused: bool,
    expanded: bool,
    info: &AgentConversationInfo,
    hint: ContentVisibilityHint,
    deleted: bool,
    view: &Entity<ResultsView>,
    roles: &theme_tokens::Roles,
    mono: &SharedString,
) -> gpui_kit::AnyElement {
    let background = selected.then_some(if focused {
        roles.list_active
    } else {
        roles.list_hover
    });
    let click_view = view.downgrade();
    // Stable domain key: the message (or segment start) ordinal.
    let key = match row {
        OutlineRow::Message { ordinal, .. } => format!("m{ordinal}"),
        OutlineRow::Segment { start, end, .. } => format!("s{start}-{end}"),
    };
    let base = v_flex()
        .id(SharedString::from(format!("{id}-outline-{key}")))
        .qa_id(format!("{id}:row:{child}"))
        .role(gpui_kit::Role::ListItem)
        .aria_label(match row {
            OutlineRow::Message {
                ordinal,
                role,
                chars,
                snippet,
                ..
            } => format!(
                "m{ordinal} \u{b7} {role} \u{b7} {} \u{b7} {snippet}",
                format_chars(*chars)
            ),
            OutlineRow::Segment {
                start,
                end,
                chars,
                first_snippet,
                ..
            } => format!(
                "m{start}..m{end} \u{b7} {} \u{b7} {first_snippet}",
                format_chars(*chars)
            ),
        })
        .px_2()
        .py_0p5()
        .when_some(background, |this, background| this.bg(background))
        .when(deleted, |this| this.opacity(0.5))
        .hover(|this| this.bg(roles.list_hover))
        .on_click(move |_: &ClickEvent, window, cx| {
            click_view
                .update(cx, |this, cx| this.click_row(child, window, cx))
                .ok();
        });
    match row {
        OutlineRow::Message {
            ordinal,
            role,
            chars,
            anchor,
            snippet,
        } => {
            let anchor_view = view.downgrade();
            let anchor_text = anchor.clone();
            base.child(
                h_flex()
                    .gap_2()
                    .items_baseline()
                    .text_xs()
                    .child(
                        div()
                            .flex_none()
                            .font_family(mono.clone())
                            .child(format!("m{ordinal}")),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(roles.accent)
                            .child(role.to_string()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_family(mono.clone())
                            .text_color(roles.muted_foreground)
                            .child(format_chars(*chars)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(roles.muted_foreground)
                            .child(snippet.clone()),
                    )
                    .child(
                        Button::new(SharedString::from(format!("{id}-copy-anchor-{key}")))
                            .icon(IconName::Copy)
                            .ghost()
                            .xsmall()
                            .accessibility_label("Copy anchor")
                            .tooltip(format!("Copy anchor ({anchor_text})"))
                            .on_click(move |_, window, cx| {
                                let anchor = anchor_text.clone();
                                anchor_view
                                    .update(cx, |this, cx| this.copy_anchor(anchor, window, cx))
                                    .ok();
                            }),
                    ),
            )
            .into_any_element()
        }
        OutlineRow::Segment {
            start,
            end,
            chars,
            first_anchor,
            last_anchor,
            first_snippet,
            last_snippet,
        } => {
            let toggle_view = view.downgrade();
            let first = RevealSpec {
                info: info.clone(),
                title: None,
                ordinal: *start,
                hint,
            };
            let last = RevealSpec {
                info: info.clone(),
                title: None,
                ordinal: *end,
                hint,
            };
            let mut block = base.child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .text_xs()
                    .child(
                        Button::new(SharedString::from(format!("{id}-seg-toggle-{key}")))
                            .icon(if expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .ghost()
                            .xsmall()
                            .accessibility_label("Expand segment")
                            .tooltip("Expand segment")
                            .on_click(move |_, _, cx| {
                                toggle_view
                                    .update(cx, |this, cx| this.toggle_segment(child, cx))
                                    .ok();
                            }),
                    )
                    .child(
                        div()
                            .font_family(mono.clone())
                            .child(OutlineModel::segment_label(*start, *end, *chars)),
                    ),
            );
            if expanded {
                for (label, anchor, snippet, spec) in [
                    (format!("m{start}"), first_anchor, first_snippet, first),
                    (format!("m{end}"), last_anchor, last_snippet, last),
                ] {
                    let reveal_view = view.downgrade();
                    block = block.child(
                        h_flex()
                            .id(SharedString::from(format!("{id}-seg-{key}-{label}")))
                            .pl_6()
                            .gap_2()
                            .text_xs()
                            .text_color(roles.muted_foreground)
                            .child(
                                div()
                                    .flex_none()
                                    .font_family(mono.clone())
                                    .child(label.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .font_family(mono.clone())
                                    .child(anchor.clone()),
                            )
                            .child(div().flex_1().min_w_0().truncate().child(snippet.clone()))
                            .on_click(move |_: &ClickEvent, _, cx| {
                                let spec = spec.clone();
                                reveal_view
                                    .update(cx, |_, cx| {
                                        let (target, reveal) = spec.request();
                                        cx.emit(ResultsEvent::Open {
                                            target,
                                            reveal,
                                            focus_viewer: false,
                                            tab: None,
                                        });
                                    })
                                    .ok();
                            }),
                    );
                }
            }
            block.into_any_element()
        }
    }
}

/// One element of the Read list.
#[allow(clippy::too_many_arguments)]
fn read_item_row(
    id: &'static str,
    item: &ReadItem,
    model: &ReadModel,
    selected: bool,
    focused: bool,
    deleted: &[PathBuf],
    paging: Option<(usize, usize)>,
    view: &Entity<ResultsView>,
    roles: &theme_tokens::Roles,
    mono: &SharedString,
) -> gpui_kit::AnyElement {
    match item {
        ReadItem::Header(group_ix) => {
            let group = &model.groups[*group_ix];
            h_flex()
                .id(SharedString::from(format!(
                    "{id}-conv-{}",
                    group.conversation_ref
                )))
                .w_full()
                .px_2()
                .py_1()
                .gap_1()
                .text_xs()
                .font_semibold()
                .bg(roles.sidebar)
                .child(
                    group
                        .title
                        .clone()
                        .unwrap_or_else(|| "Conversation".to_string()),
                )
                .child(
                    div()
                        .font_family(mono.clone())
                        .text_color(roles.muted_foreground)
                        .child(format!("· {}", group.conversation_ref)),
                )
                .into_any_element()
        }
        ReadItem::Card {
            group,
            card,
            selectable,
        } => {
            let card = &model.groups[*group].cards[*card];
            let is_deleted = card.info.as_ref().is_some_and(|info| {
                deleted
                    .iter()
                    .any(|path| crate::globals::same_path(path, &info.path))
            });
            read_card_row(
                id,
                *selectable,
                card,
                selected,
                focused,
                is_deleted,
                view,
                roles,
                mono,
            )
        }
        ReadItem::Continue(command_ix) => {
            let command = model.continue_commands[*command_ix].clone();
            let copy_view = view.downgrade();
            let copy = command.clone();
            h_flex()
                .id(SharedString::from(format!("{id}-continue-{command_ix}")))
                .w_full()
                .px_2()
                .py_1()
                .gap_2()
                .items_center()
                .text_xs()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(mono.clone())
                        .text_color(roles.muted_foreground)
                        .child(format!(
                            "continue read · {}",
                            command.replace("claude-history agent read ", "")
                        )),
                )
                .child(
                    Button::new(SharedString::from(format!("{id}-copy-read-{command_ix}")))
                        .label("Copy read command")
                        .xsmall()
                        .outline()
                        .on_click(move |_, window, cx| {
                            let copy = copy.clone();
                            copy_view
                                .update(cx, |this, cx| this.copy_command(copy, window, cx))
                                .ok();
                        }),
                )
                .into_any_element()
        }
        ReadItem::Paging => paging_row(id, paging, view),
    }
}

/// A match line of a `match=` slice: core numbers them `>N: text`. A
/// markdown quote (`> note`) is not one.
pub fn is_match_line(line: &str) -> bool {
    line.strip_prefix('>')
        .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
}

#[allow(clippy::too_many_arguments)]
fn read_card_row(
    id: &'static str,
    selectable: usize,
    card: &ReadCard,
    selected: bool,
    focused: bool,
    deleted: bool,
    view: &Entity<ResultsView>,
    roles: &theme_tokens::Roles,
    mono: &SharedString,
) -> gpui_kit::AnyElement {
    let background = selected.then_some(if focused {
        roles.list_active
    } else {
        roles.list_hover
    });
    let click_view = view.downgrade();
    let anchor_view = view.downgrade();
    let anchor = card.anchor.clone();
    let lines: Vec<&str> = card.body.lines().collect();
    let match_slice = card
        .slice
        .as_deref()
        .is_some_and(|slice| slice.starts_with("match="));
    let shown = lines.len().min(READ_BODY_LINES);
    let mut body = v_flex()
        .gap_0()
        .text_xs()
        .font_family(mono.clone())
        .text_color(roles.foreground);
    for line in lines.iter().take(shown) {
        let marked = match_slice && is_match_line(line);
        body = body.child(
            div()
                .when(marked, |this| this.bg(roles.match_background))
                .child(if line.is_empty() {
                    " ".to_string()
                } else {
                    (*line).to_string()
                }),
        );
    }
    if lines.len() > shown {
        body = body.child(div().text_color(roles.muted_foreground).child(format!(
            "… {} more lines (see the agent text output)",
            lines.len() - shown
        )));
    }
    v_flex()
        .id(SharedString::from(format!("{id}-card-{}", card.anchor)))
        .qa_id(format!("{id}:row:{selectable}"))
        .role(gpui_kit::Role::ListItem)
        .aria_label(card.header())
        // The list's full width: the selection background and the copy
        // buttons line up whatever the card's content width.
        .w_full()
        .px_2()
        .py_1()
        .gap_0p5()
        .when_some(background, |this, background| this.bg(background))
        .when(card.focused, |this| {
            this.border_l_2().border_color(roles.focus_mark)
        })
        .when(deleted, |this| this.opacity(0.5))
        .hover(|this| this.bg(roles.list_hover))
        .on_click(move |_: &ClickEvent, window, cx| {
            click_view
                .update(cx, |this, cx| this.click_row(selectable, window, cx))
                .ok();
        })
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .text_xs()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(mono.clone())
                        .text_color(roles.muted_foreground)
                        .child(card.header()),
                )
                .child(
                    Button::new(SharedString::from(format!(
                        "{id}-copy-anchor-{}",
                        card.anchor
                    )))
                    .icon(IconName::Copy)
                    .ghost()
                    .xsmall()
                    .accessibility_label("Copy anchor")
                    .tooltip(format!("Copy anchor ({anchor})"))
                    .on_click(move |_, window, cx| {
                        let anchor = anchor.clone();
                        anchor_view
                            .update(cx, |this, cx| this.copy_anchor(anchor, window, cx))
                            .ok();
                    }),
                ),
        )
        .when_some(card.slice.clone(), |this, slice| {
            this.child(
                div()
                    .text_xs()
                    .text_color(roles.muted_foreground)
                    .child(slice),
            )
        })
        .child(body)
        .into_any_element()
}

fn paging_row(
    id: &'static str,
    range: Option<(usize, usize)>,
    view: &Entity<ResultsView>,
) -> gpui_kit::AnyElement {
    let previous = view.downgrade();
    let next = view.downgrade();
    let label = range
        .map(|(start, end)| range_label(start, end))
        .unwrap_or_default();
    h_flex()
        .id(SharedString::from(format!("{id}-paging")))
        .px_2()
        .py_1()
        .gap_2()
        .items_center()
        .child(
            Button::new(SharedString::from(format!("{id}-load-previous")))
                .label("Load previous")
                .xsmall()
                .outline()
                .tooltip("Extend the range by 5 messages before")
                .on_click(move |_, _, cx| {
                    previous
                        .update(cx, |_, cx| {
                            cx.emit(ResultsEvent::LoadMore { previous: true })
                        })
                        .ok();
                }),
        )
        .child(div().text_xs().child(label))
        .child(
            Button::new(SharedString::from(format!("{id}-load-next")))
                .label("Load next")
                .xsmall()
                .outline()
                .tooltip("Extend the range by 5 messages after")
                .on_click(move |_, _, cx| {
                    next.update(cx, |_, cx| {
                        cx.emit(ResultsEvent::LoadMore { previous: false })
                    })
                    .ok();
                }),
        )
        .into_any_element()
}
