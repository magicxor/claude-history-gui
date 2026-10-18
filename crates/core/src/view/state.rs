//! Viewer state: the open conversation ([`ViewerModel`]) and the UI-agnostic
//! rules of upstream `tui/app/view_state.rs` and the viewer key handling of
//! `tui/app/input_controller.rs`:
//!
//! - in-view search ([`ViewSearch`]): typing, commit, next/previous with
//!   wrap-around, counters; over ledger lines ([`search_lines`]) or over the
//!   document's blocks ([`DocSearchIndex`]);
//! - message navigation ([`MessageNav`]): `J`/`K` focus with the first-press
//!   behavior, click and search focus, re-sync after scrolling;
//! - re-render anchoring: keep the focused (or first visible) message in
//!   place when tools, thinking, timing or an expansion change
//!   ([`capture_anchor`] / [`resolve_scroll`] for the ledger,
//!   [`capture_anchor_entry`] / [`find_entry_or_prev`] for the document);
//! - the Esc order ([`escape`]).
//!
//! Scrolling, hit-testing and drawing stay with the GUI; functions that
//! upstream computed from the viewport take the viewport facts as arguments.

use std::collections::BTreeSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::ViewPrefs;
use super::document::{ConversationDocument, DocBlockKind, build_document};
use super::ledger::{
    MessageRange, RenderableEntry, RenderedConversation, RenderedLine, ToolDisplayMode,
    ToolOutputId, parse_conversation_file, parse_conversation_file_cancellable,
    render_parsed_conversation,
};
use crate::export::{ExportOptions, MESSAGE_NOT_FOUND};

// ---------------------------------------------------------------------
// ViewerModel
// ---------------------------------------------------------------------

/// An open conversation: its parsed entries (cached when opened, so the view
/// keeps rendering and toggling after the file is deleted or changed), the
/// display preferences and the expanded tool outputs.
#[derive(Clone, Debug)]
pub struct ViewerModel {
    pub path: PathBuf,
    pub entries: Arc<Vec<RenderableEntry>>,
    pub prefs: ViewPrefs,
    pub expanded: BTreeSet<ToolOutputId>,
}

impl ViewerModel {
    /// Parse `path` (upstream `enter_view_mode`). Expanded outputs start
    /// empty. On failure upstream showed `"Failed to open: {e}"`.
    pub fn open(path: &Path, prefs: ViewPrefs) -> std::io::Result<Self> {
        let entries = parse_conversation_file(path)?;
        Ok(Self::from_entries(
            path.to_path_buf(),
            Arc::new(entries),
            prefs,
        ))
    }

    /// [`Self::open`] that abandons the parse (`Ok(None)`) once `cancelled`
    /// returns true (polled every 64 lines).
    pub fn open_cancellable(
        path: &Path,
        prefs: ViewPrefs,
        cancelled: &dyn Fn() -> bool,
    ) -> std::io::Result<Option<Self>> {
        Ok(parse_conversation_file_cancellable(path, cancelled)?
            .map(|entries| Self::from_entries(path.to_path_buf(), Arc::new(entries), prefs)))
    }

    /// A model over already parsed entries.
    pub fn from_entries(
        path: PathBuf,
        entries: Arc<Vec<RenderableEntry>>,
        prefs: ViewPrefs,
    ) -> Self {
        Self {
            path,
            entries,
            prefs,
            expanded: BTreeSet::new(),
        }
    }

    /// `t`: summary → truncated → full → summary. Returns the new mode
    /// (its status label is [`ToolDisplayMode::status_label`]).
    pub fn cycle_tool_display(&mut self) -> ToolDisplayMode {
        self.prefs.tool_display = self.prefs.tool_display.next();
        self.prefs.tool_display
    }

    /// `T`: thinking blocks, subagent and agent-progress entries. Returns
    /// the new state.
    pub fn toggle_thinking(&mut self) -> bool {
        self.prefs.show_thinking = !self.prefs.show_thinking;
        self.prefs.show_thinking
    }

    /// `i`: the timestamp column. Returns the new state.
    pub fn toggle_timing(&mut self) -> bool {
        self.prefs.show_timing = !self.prefs.show_timing;
        self.prefs.show_timing
    }

    /// Expand or collapse a tool output (a click on an expandable block or
    /// a clickable ledger line). Returns whether it is now expanded. The
    /// set survives preference toggles and resizes because ids are stable.
    pub fn toggle_expanded(&mut self, id: &ToolOutputId) -> bool {
        if self.expanded.remove(id) {
            false
        } else {
            self.expanded.insert(id.clone());
            true
        }
    }

    pub fn is_expanded(&self, id: &ToolOutputId) -> bool {
        self.expanded.contains(id)
    }

    /// The layout-free document for the current preferences.
    pub fn document(&self) -> ConversationDocument {
        build_document(&self.entries, &self.prefs, &self.expanded)
    }

    /// The ledger rendering at `content_width_cols` columns (see
    /// [`super::content_width_for_frame`]).
    pub fn ledger(&self, content_width_cols: usize) -> RenderedConversation {
        render_parsed_conversation(
            &self.entries,
            &self
                .prefs
                .render_options(content_width_cols, &self.expanded),
        )
    }

    /// Export / copy options matching the view.
    pub fn export_options(&self) -> ExportOptions {
        self.prefs.export_options()
    }

    /// What upstream's `I` copied: the file stem (the session UUID for
    /// Claude; for Pi/OMP the file name, not the header id).
    pub fn session_id(&self) -> Option<&str> {
        self.path.file_stem().and_then(|stem| stem.to_str())
    }

    /// The clipboard text of the message at `entry_index` (upstream `y` in
    /// navigation mode), from the cached entries: the raw markdown source,
    /// tools and thinking per [`Self::export_options`].
    ///
    /// Upstream re-read the file ([`crate::export::extract_message_text`],
    /// still available); reading the cache copies what is displayed, also
    /// after the file changed or was deleted. An empty string means
    /// [`crate::export::NO_TEXT_CONTENT`] (always the case for a summary
    /// group, whose tools are hidden in summary mode).
    pub fn message_text(&self, entry_index: usize) -> Result<String, String> {
        self.entries
            .binary_search_by_key(&entry_index, |entry| entry.entry_index)
            .map(|position| {
                crate::export::message_text(self.entries[position].entry(), self.export_options())
            })
            .map_err(|_| MESSAGE_NOT_FOUND.to_string())
    }

    /// The CLI `--plain` text of the cached entries
    /// ([`crate::export::plain_display`]), with tools and thinking per the
    /// view.
    pub fn plain_display(&self) -> String {
        crate::export::plain_display_entries(
            self.entries.iter().map(RenderableEntry::entry),
            self.export_options(),
        )
    }

    /// The entry ([`RenderableEntry::entry_index`]) of agent message `mN`
    /// (`ordinal` is the 1-based `N` that `agent read` / `outline` / search
    /// hits use), or `None` when the transcript has no such message.
    ///
    /// Ordinals are placed exactly as [`crate::agent::transcript`] places
    /// them (shared rules of `history::messages::MessageOrdinals`), over the
    /// cached entries. A streamed assistant message whose later records
    /// replace it maps to the first entry with that ordinal.
    pub fn entry_for_agent_ordinal(&self, ordinal: usize) -> Option<usize> {
        self.agent_ordinals()
            .into_iter()
            .find(|(_, placed)| *placed == ordinal)
            .map(|(entry_index, _)| entry_index)
    }

    /// The agent message ordinal (`mN`) of the entry `entry_index`, or
    /// `None` for an entry that is not an agent message (metadata, a
    /// Warmup exchange, `/clear`, an entry without content). Every record
    /// of a replaced streaming message has its ordinal.
    pub fn agent_ordinal_for_entry(&self, entry_index: usize) -> Option<usize> {
        self.agent_ordinals()
            .into_iter()
            .find(|(placed_entry, _)| *placed_entry == entry_index)
            .map(|(_, ordinal)| ordinal)
    }

    /// `(entry_index, ordinal)` of every entry placed as an agent message,
    /// in entry order.
    fn agent_ordinals(&self) -> Vec<(usize, usize)> {
        use crate::claude::{LogEntry, parse_agent_progress};
        use crate::history::{MessageOrdinals, Placement};

        let mut ordinals = MessageOrdinals::new();
        let mut placed = Vec::new();
        for parsed in self.entries.iter() {
            let placement = match parsed.entry() {
                LogEntry::Progress { data, .. } => match parse_agent_progress(data) {
                    Some(progress) => ordinals.place_subagent(&progress),
                    None => Placement::Control,
                },
                entry => ordinals.place(entry),
            };
            match placement {
                Placement::Message(ordinal) | Placement::Replaces(ordinal) => {
                    placed.push((parsed.entry_index, ordinal));
                }
                Placement::Control => {}
            }
        }
        placed
    }
}

// ---------------------------------------------------------------------
// Ledger (line-based) helpers, verbatim from upstream view_state.rs
// ---------------------------------------------------------------------

/// Whether a rendered ledger line contains `query_lower` (already
/// lowercased): case-insensitive substring match over the concatenated span
/// text, including the name column, separator and timestamp (upstream
/// quirk).
pub fn line_matches_query(line: &RenderedLine, query_lower: &str) -> bool {
    let full_text: String = line.spans.iter().map(|(text, _)| text.as_str()).collect();
    full_text.to_lowercase().contains(query_lower)
}

/// Indices of the ledger lines matching `query` (upstream
/// `update_search_results`); empty for an empty query. Counts are lines, not
/// occurrences.
pub fn search_lines(lines: &[RenderedLine], query: &str) -> Vec<usize> {
    let query_lower = query.to_lowercase();
    if query_lower.is_empty() {
        return Vec::new();
    }
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line_matches_query(line, &query_lower))
        .map(|(i, _)| i)
        .collect()
}

/// The message whose range contains `line_idx` (binary search).
pub fn message_idx_at_line(ranges: &[MessageRange], line_idx: usize) -> Option<usize> {
    let idx = ranges.partition_point(|m| m.end_line <= line_idx);
    ranges
        .get(idx)
        .is_some_and(|m| line_idx >= m.start_line && line_idx < m.end_line)
        .then_some(idx)
}

/// The first message overlapping the viewport `[scroll_offset,
/// scroll_offset + viewport_height)` (upstream `sync_focus_to_scroll`); the
/// `first_visible` argument of [`MessageNav`].
pub fn first_message_in_viewport(
    ranges: &[MessageRange],
    scroll_offset: usize,
    viewport_height: usize,
) -> Option<usize> {
    let viewport_end = scroll_offset + viewport_height;
    ranges
        .iter()
        .position(|m| m.end_line > scroll_offset && m.start_line < viewport_end)
}

/// The scroll offset that shows the start of `range` (upstream
/// `ensure_message_visible`): unchanged when it is already in the viewport.
pub fn scroll_to_show(
    range: &MessageRange,
    scroll_offset: usize,
    viewport_height: usize,
    total_lines: usize,
) -> usize {
    let max_scroll = total_lines.saturating_sub(viewport_height);
    if range.start_line < scroll_offset || range.start_line >= scroll_offset + viewport_height {
        range.start_line.min(max_scroll)
    } else {
        scroll_offset
    }
}

/// A message and its distance from the top of the viewport, captured before
/// a re-render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrollAnchor {
    pub entry_index: usize,
    pub relative_row: isize,
}

/// Capture the re-render anchor: the focused message when navigation is
/// active, else the first message starting at or after the scroll offset
/// (or the last message).
pub fn capture_anchor(
    ranges: &[MessageRange],
    scroll_offset: usize,
    focused: Option<usize>,
    nav_active: bool,
) -> Option<ScrollAnchor> {
    if ranges.is_empty() {
        return None;
    }

    let msg = if nav_active {
        focused.and_then(|i| ranges.get(i))
    } else {
        None
    }
    .unwrap_or_else(|| {
        let i = ranges.partition_point(|m| m.start_line < scroll_offset);
        ranges.get(i).unwrap_or_else(|| ranges.last().unwrap())
    });

    Some(ScrollAnchor {
        entry_index: msg.entry_index,
        relative_row: msg.start_line as isize - scroll_offset as isize,
    })
}

/// The message with `entry_index`, else the closest previous one, else the
/// first.
pub fn find_message_idx_or_prev(ranges: &[MessageRange], entry_index: usize) -> Option<usize> {
    if ranges.is_empty() {
        return None;
    }
    match ranges.binary_search_by_key(&entry_index, |m| m.entry_index) {
        Ok(idx) => Some(idx),
        Err(0) => Some(0),
        Err(idx) => Some(idx - 1),
    }
}

/// Resolve an anchor after a re-render (upstream `re_render_view`): the
/// message to focus (set even when navigation is inactive) and the new scroll
/// offset. The same message keeps its distance from the top; a fallback
/// message starts at the top unless the anchor was scrolled partly out of
/// view. Without an anchor the old offset is clamped.
pub fn resolve_scroll(
    ranges: &[MessageRange],
    anchor: Option<ScrollAnchor>,
    old_scroll: usize,
    viewport_height: usize,
    total_lines: usize,
) -> (Option<usize>, usize) {
    let max_scroll = total_lines.saturating_sub(viewport_height);

    let resolved_idx = anchor
        .and_then(|a| find_message_idx_or_prev(ranges, a.entry_index))
        .or_else(|| (!ranges.is_empty()).then_some(0));

    let scroll = match (anchor, resolved_idx) {
        (Some(a), Some(idx)) => {
            let new_msg = &ranges[idx];
            let rel = if new_msg.entry_index == a.entry_index {
                a.relative_row
            } else {
                a.relative_row.min(0)
            };
            let raw = new_msg.start_line as isize - rel;
            raw.clamp(0, max_scroll as isize) as usize
        }
        _ => old_scroll.min(max_scroll),
    };
    (resolved_idx, scroll)
}

// ---------------------------------------------------------------------
// Document-model anchors
// ---------------------------------------------------------------------

/// The document analog of [`capture_anchor`]: the entry index to keep in
/// place across a rebuild. `nav_entries` are the entry indices of the
/// navigable messages ([`ConversationDocument::navigable_entry_indices`]);
/// `first_at_or_below_top` is the navigation position of the first
/// navigable message whose top is at or below the top of the viewport
/// (`None` means none, and the last message is used). The GUI keeps its own
/// pixel offset of that message.
pub fn capture_anchor_entry(
    nav_entries: &[usize],
    focused: Option<usize>,
    nav_active: bool,
    first_at_or_below_top: Option<usize>,
) -> Option<usize> {
    if nav_entries.is_empty() {
        return None;
    }
    let focused = if nav_active {
        focused.and_then(|i| nav_entries.get(i))
    } else {
        None
    };
    Some(*focused.unwrap_or_else(|| {
        first_at_or_below_top
            .and_then(|i| nav_entries.get(i))
            .unwrap_or_else(|| nav_entries.last().unwrap())
    }))
}

/// The document analog of [`find_message_idx_or_prev`]: the navigation
/// position of `entry_index`, else of the closest previous entry, else 0.
/// The result is exact when `nav_entries[position] == entry_index`; a
/// fallback message should be aligned to the top of the viewport (unless the
/// anchor was partly scrolled out), as [`resolve_scroll`] does.
pub fn find_entry_or_prev(nav_entries: &[usize], entry_index: usize) -> Option<usize> {
    if nav_entries.is_empty() {
        return None;
    }
    match nav_entries.binary_search(&entry_index) {
        Ok(idx) => Some(idx),
        Err(0) => Some(0),
        Err(idx) => Some(idx - 1),
    }
}

// ---------------------------------------------------------------------
// Message navigation
// ---------------------------------------------------------------------

/// Message navigation mode (`J`/`K`, `]`/`[`, click, search jumps, `y`).
///
/// Positions index the navigable messages: the ledger's `messages`, or
/// [`ConversationDocument::navigable_indices`]. Arguments named
/// `first_visible` are the position of the first message overlapping the
/// viewport (for the ledger: [`first_message_in_viewport`]); `None` when no
/// message is visible.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MessageNav {
    active: bool,
    focused: Option<usize>,
}

impl MessageNav {
    /// The state right after opening: the first message focused, navigation
    /// mode off (no gutter).
    pub fn new(message_count: usize) -> Self {
        Self {
            active: false,
            focused: (message_count > 0).then_some(0),
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn focused(&self) -> Option<usize> {
        self.focused
    }

    /// The message to mark with the focus gutter (`▌`): only while
    /// navigation is active.
    pub fn highlighted(&self) -> Option<usize> {
        self.active.then_some(self.focused).flatten()
    }

    fn activate(&mut self, first_visible: Option<usize>) {
        if !self.active {
            self.active = true;
            if let Some(idx) = first_visible {
                self.focused = Some(idx);
            }
        }
    }

    /// `J` / `]`. The first press activates navigation and syncs the focus to
    /// the first visible message before moving, so it focuses the message
    /// after it (upstream behavior). Clamped at the end. Returns the focused
    /// position; the GUI scrolls it into view (for the ledger:
    /// [`scroll_to_show`]).
    pub fn focus_next(
        &mut self,
        message_count: usize,
        first_visible: Option<usize>,
    ) -> Option<usize> {
        if message_count == 0 {
            return None;
        }
        self.activate(first_visible);
        let next = match self.focused {
            Some(i) if i + 1 < message_count => i + 1,
            Some(i) => i,
            None => 0,
        };
        self.focused = Some(next);
        self.focused
    }

    /// `K` / `[`: like [`Self::focus_next`], backwards, clamped at 0.
    pub fn focus_prev(
        &mut self,
        message_count: usize,
        first_visible: Option<usize>,
    ) -> Option<usize> {
        if message_count == 0 {
            return None;
        }
        self.activate(first_visible);
        let prev = match self.focused {
            Some(i) if i > 0 => i - 1,
            Some(i) => i,
            None => 0,
        };
        self.focused = Some(prev);
        self.focused
    }

    /// Focus a message and activate navigation (a click inside a message,
    /// or a search jump). Returns whether anything changed.
    pub fn focus(&mut self, position: usize) -> bool {
        if self.active && self.focused == Some(position) {
            return false;
        }
        self.active = true;
        self.focused = Some(position);
        true
    }

    /// After any scroll while navigation is active: focus the first visible
    /// message.
    pub fn sync_after_scroll(&mut self, first_visible: Option<usize>) {
        if self.active
            && let Some(idx) = first_visible
        {
            self.focused = Some(idx);
        }
    }

    /// `y`: activate navigation if needed (syncing to the viewport) and
    /// return the message to copy.
    pub fn copy_target(&mut self, first_visible: Option<usize>) -> Option<usize> {
        self.activate(first_visible);
        self.focused
    }

    /// Esc, first step: leave navigation mode. Returns whether it was
    /// active.
    pub fn deactivate(&mut self) -> bool {
        std::mem::replace(&mut self.active, false)
    }

    /// After a re-render, the resolved anchor becomes the focus, whether or
    /// not navigation is active.
    pub fn refocus(&mut self, position: Option<usize>) {
        self.focused = position;
    }
}

// ---------------------------------------------------------------------
// In-view search
// ---------------------------------------------------------------------

/// Search input state (upstream `ViewSearchMode`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewSearchMode {
    /// No search.
    #[default]
    Off,
    /// The query is being typed; matches update live.
    Typing,
    /// Committed with matches; `n`/`N` move between them.
    Active,
}

/// In-view search state, generic over the match type: ledger line indices
/// (with [`search_lines`]) or document [`SearchMatch`]es (with
/// [`DocSearchIndex::find`]). Methods that change the query take the
/// matcher as a closure and return the match to jump to: the GUI scrolls it
/// into view and focuses its message ([`MessageNav::focus`]).
#[derive(Clone, Debug)]
pub struct ViewSearch<M = SearchMatch> {
    mode: ViewSearchMode,
    query: String,
    matches: Vec<M>,
    current: usize,
}

impl<M> Default for ViewSearch<M> {
    fn default() -> Self {
        Self {
            mode: ViewSearchMode::Off,
            query: String::new(),
            matches: Vec::new(),
            current: 0,
        }
    }
}

impl<M: Clone> ViewSearch<M> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mode(&self) -> ViewSearchMode {
        self.mode
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn matches(&self) -> &[M] {
        &self.matches
    }

    /// Number of matches.
    pub fn count(&self) -> usize {
        self.matches.len()
    }

    /// Index of the current match (meaningful when there are matches).
    pub fn current_index(&self) -> usize {
        self.current
    }

    pub fn current(&self) -> Option<&M> {
        self.matches.get(self.current)
    }

    /// `/`: start typing a new query (query and matches cleared).
    pub fn start(&mut self) {
        self.mode = ViewSearchMode::Typing;
        self.query.clear();
        self.matches.clear();
        self.current = 0;
    }

    /// Esc or Ctrl+C while typing, Esc while active: search off, query and
    /// matches cleared. Scroll and focus stay where the search moved them.
    pub fn clear(&mut self) {
        self.mode = ViewSearchMode::Off;
        self.query.clear();
        self.matches.clear();
    }

    /// Recompute the matches for the current query. On a match the current
    /// match resets to the first one, which is returned.
    fn update(&mut self, find: impl FnOnce(&str) -> Vec<M>) -> Option<M> {
        if self.query.is_empty() {
            self.matches.clear();
            return None;
        }
        self.matches = find(&self.query);
        if self.matches.is_empty() {
            return None;
        }
        self.current = 0;
        Some(self.matches[0].clone())
    }

    /// Replace the whole query (an edit in a GUI text input).
    pub fn set_query(&mut self, query: &str, find: impl FnOnce(&str) -> Vec<M>) -> Option<M> {
        self.query.clear();
        self.query.push_str(query);
        self.update(find)
    }

    /// A typed character.
    pub fn push_char(&mut self, c: char, find: impl FnOnce(&str) -> Vec<M>) -> Option<M> {
        self.query.push(c);
        self.update(find)
    }

    /// Backspace.
    pub fn backspace(&mut self, find: impl FnOnce(&str) -> Vec<M>) -> Option<M> {
        self.query.pop();
        self.update(find)
    }

    /// Ctrl+U: clear the query. Returns whether it was non-empty.
    pub fn clear_query(&mut self, find: impl FnOnce(&str) -> Vec<M>) -> bool {
        if self.query.is_empty() {
            return false;
        }
        self.query.clear();
        self.update(find);
        true
    }

    /// Ctrl+W: drop trailing whitespace, then everything after the last
    /// whitespace.
    pub fn delete_word_backwards(&mut self, find: impl FnOnce(&str) -> Vec<M>) -> Option<M> {
        let trimmed = self.query.trim_end();
        // Upstream truncated at `index + 1`, which splits a multi-byte
        // whitespace character (panic); cut after the whole character.
        match trimmed
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
        {
            Some((index, c)) => self.query.truncate(index + c.len_utf8()),
            None => self.query.clear(),
        }
        self.update(find)
    }

    /// Enter: keep the search active only when there are matches.
    pub fn commit(&mut self) {
        self.mode = if self.matches.is_empty() {
            ViewSearchMode::Off
        } else {
            ViewSearchMode::Active
        };
    }

    /// `n` (active search only): the next match, wrapping around. The GUI
    /// scrolls only when the match is outside the viewport.
    pub fn next_match(&mut self) -> Option<M> {
        if self.mode != ViewSearchMode::Active || self.matches.is_empty() {
            return None;
        }
        self.current = (self.current + 1) % self.matches.len();
        Some(self.matches[self.current].clone())
    }

    /// `N` (active search only): the previous match, wrapping around.
    pub fn prev_match(&mut self) -> Option<M> {
        if self.mode != ViewSearchMode::Active || self.matches.is_empty() {
            return None;
        }
        self.current = if self.current == 0 {
            self.matches.len() - 1
        } else {
            self.current - 1
        };
        Some(self.matches[self.current].clone())
    }

    /// After a re-render: an active search recomputes its matches and clamps
    /// the current match (no jump); other modes are left alone.
    pub fn refresh(&mut self, find: impl FnOnce(&str) -> Vec<M>) {
        if self.mode != ViewSearchMode::Active || self.query.is_empty() {
            return;
        }
        self.matches = find(&self.query);
        if self.matches.is_empty() {
            self.current = 0;
        } else {
            self.current = self.current.min(self.matches.len() - 1);
        }
    }

    /// The suffix of the search input line: `""` for an empty query,
    /// `" (no matches)"`, or `" ({current}/{count})"`.
    pub fn input_status(&self) -> String {
        if self.matches.is_empty() {
            if self.query.is_empty() {
                String::new()
            } else {
                " (no matches)".to_string()
            }
        } else {
            format!(" ({}/{})", self.current + 1, self.matches.len())
        }
    }

    /// The status bar counter `"{current}/{count}"` while a committed search
    /// has matches.
    pub fn match_counter(&self) -> Option<String> {
        (self.mode == ViewSearchMode::Active && !self.matches.is_empty())
            .then(|| format!("{}/{}", self.current + 1, self.matches.len()))
    }
}

/// What [`escape`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewEscape {
    /// The search was cleared (while typing, or a committed search).
    ClearedSearch,
    /// Navigation mode was turned off.
    ExitedNav,
    /// Nothing left to unwind: leave the viewer (back to the list, or quit
    /// in single-file mode).
    Leave,
}

/// Esc in the viewer: clear a search being typed; else leave navigation
/// mode; else clear a committed search; else leave the viewer.
pub fn escape<M: Clone>(nav: &mut MessageNav, search: &mut ViewSearch<M>) -> ViewEscape {
    if search.mode() == ViewSearchMode::Typing {
        search.clear();
        return ViewEscape::ClearedSearch;
    }
    if nav.deactivate() {
        return ViewEscape::ExitedNav;
    }
    if search.mode() == ViewSearchMode::Active {
        search.clear();
        return ViewEscape::ClearedSearch;
    }
    ViewEscape::Leave
}

/// One occurrence of the query in the document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    /// Index into [`ConversationDocument::messages`].
    pub message: usize,
    /// Index into the message's blocks.
    pub block: usize,
    /// Index into the items of an expanded summary group
    /// ([`DocBlockKind::ToolSummaryDetails`]); `None` otherwise.
    pub item: Option<usize>,
    /// Byte range in the searched text ([`DocSearchIndex::text`], the
    /// block's or item's [`super::DocBlock::plain_text`]).
    pub range: Range<usize>,
}

#[derive(Clone, Debug)]
struct SearchTarget {
    message: usize,
    block: usize,
    item: Option<usize>,
    text: String,
    folded: String,
    /// For each byte of `folded`, the start of the original character; only
    /// when lowercasing changed byte lengths (identity otherwise).
    origin: Option<Vec<usize>>,
}

/// The searchable text of a document: every block's (or summary item's)
/// plain text, lowercased once. Build it when the document changes and run
/// [`DocSearchIndex::find`] on each keystroke.
///
/// Matching is a case-insensitive substring search (Greek final sigma
/// matches `σ`/`Σ`) over the text the block shows ([`super::DocBlock::plain_text`]): collapsed tool output, hidden
/// thinking and other content that is not displayed is not searchable, as
/// in the ledger. Unlike the ledger search, labels and timestamps are not
/// part of the text, and every occurrence is a match (the ledger counts
/// matching lines).
#[derive(Clone, Debug, Default)]
pub struct DocSearchIndex {
    targets: Vec<SearchTarget>,
}

impl DocSearchIndex {
    pub fn new(doc: &ConversationDocument) -> Self {
        let mut targets = Vec::new();
        for (message_idx, message) in doc.messages.iter().enumerate() {
            for (block_idx, block) in message.blocks.iter().enumerate() {
                match &block.kind {
                    DocBlockKind::ToolSummaryDetails { items, .. } => {
                        for (item_idx, item) in items.iter().enumerate() {
                            targets.push(SearchTarget::new(
                                message_idx,
                                block_idx,
                                Some(item_idx),
                                item.plain_text(),
                            ));
                        }
                    }
                    _ => targets.push(SearchTarget::new(
                        message_idx,
                        block_idx,
                        None,
                        block.plain_text(),
                    )),
                }
            }
        }
        Self { targets }
    }

    /// The searched text of a block (or summary item); match ranges index
    /// into it.
    pub fn text(&self, message: usize, block: usize, item: Option<usize>) -> Option<&str> {
        self.targets
            .iter()
            .find(|t| t.message == message && t.block == block && t.item == item)
            .map(|t| t.text.as_str())
    }

    /// All non-overlapping occurrences of `query`, case-insensitively, in
    /// document order. Empty for an empty query.
    pub fn find(&self, query: &str) -> Vec<SearchMatch> {
        let query: String = query.chars().flat_map(fold_char).collect();
        if query.is_empty() {
            return Vec::new();
        }
        let mut matches = Vec::new();
        for target in &self.targets {
            for (start, found) in target.folded.match_indices(&query) {
                matches.push(SearchMatch {
                    message: target.message,
                    block: target.block,
                    item: target.item,
                    range: target.original_range(start..start + found.len()),
                });
            }
        }
        matches
    }
}

/// Lowercase one character for search. Final sigma folds to `σ` as well, so
/// that "ΟΔΟΣ", "Οδος" and "οδος" match each other in either direction:
/// per-character lowercasing never yields `ς`, while lowercase text spells
/// the word-final sigma `ς` (the ledger's `str::to_lowercase` applies the
/// final-sigma rule instead, which misses some of these pairs). `ς` and `σ`
/// are both two bytes, so byte offsets are unchanged.
fn fold_char(ch: char) -> impl Iterator<Item = char> {
    ch.to_lowercase().map(|c| if c == 'ς' { 'σ' } else { c })
}

impl SearchTarget {
    fn new(message: usize, block: usize, item: Option<usize>, text: String) -> Self {
        let mut folded = String::with_capacity(text.len());
        let mut identity = true;
        for ch in text.chars() {
            let before = folded.len();
            folded.extend(fold_char(ch));
            identity &= folded.len() - before == ch.len_utf8();
        }
        let origin = (!identity).then(|| {
            let mut origin = Vec::with_capacity(folded.len());
            for (start, ch) in text.char_indices() {
                let added: usize = fold_char(ch).map(char::len_utf8).sum();
                origin.extend(std::iter::repeat_n(start, added));
            }
            origin
        });
        Self {
            message,
            block,
            item,
            text,
            folded,
            origin,
        }
    }

    /// Map a non-empty byte range of `folded` to whole characters of `text`.
    fn original_range(&self, folded: Range<usize>) -> Range<usize> {
        let Some(origin) = &self.origin else {
            return folded;
        };
        let start = origin[folded.start];
        let last = origin[folded.end - 1];
        let end = last + self.text[last..].chars().next().map_or(0, char::len_utf8);
        start..end
    }
}

#[cfg(test)]
mod tests;
