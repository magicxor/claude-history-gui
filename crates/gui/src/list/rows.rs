//! List rows (DESIGN §10.1–§10.3): the pure `RowModel` built from core's
//! `display_format::{build_row_header, build_row_preview}` for the visible
//! rows only, the layout rules (literal mode, preview width, year in old
//! timestamps) and the `ListItem` element. WP1.
//!
//! A row has three text lines (four in literal mode, which reserves the
//! context line on every row so the list stays uniform) and a separator:
//!
//! 1. `[lane] TITLE ······ time` — the title is the custom title, else the
//!    summary, else the project (AD20); the time is mono and carries the
//!    recency color;
//! 2. `project · N msgs · duration · score · summary-tail`;
//! 3. the preview with the query highlighted;
//! 4. (literal mode) the literal context snippet.
//!
//! The model and its helpers are gpui-free and unit-tested; only
//! [`render_row`] touches gpui.

use std::ops::Range;
use std::path::PathBuf;

use chrono::{DateTime, Datelike as _, Local};
use claude_history_core::browser::{Browser, query_has_literals};
use claude_history_core::display_format::{
    HighlightQuery, HighlightedText, PreviewSource, Recency, build_row_header, build_row_preview,
    format_timestamp,
};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::{
    AnyElement, ElementId, FontWeight, HighlightStyle, IntoElement, MouseButton,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, StyledText,
    WeakEntity, div, rems,
};

use crate::list::ListPane;
use crate::qa_ext::QaExt as _;
use crate::theme_tokens::Roles;

/// Row height in normal mode (rem): 1.25 + 1.125 + 1.125 text lines, two
/// 0.125 gaps and 0.5 of vertical padding.
pub const ROW_HEIGHT_REM: f32 = 4.25;
/// Row height in literal mode (rem): one more text line and gap.
pub const ROW_HEIGHT_LITERAL_REM: f32 = 5.5;
/// The semantic score is hidden on rows narrower than this (rem).
pub const SCORE_MIN_ROW_WIDTH_REM: f32 = 22.0;
/// Horizontal padding of a row on each side (rem, `px_3`).
pub const ROW_PADDING_X_REM: f32 = 0.75;
/// Width of the source lane (rem) and the gap that follows it.
pub const LANE_WIDTH_REM: f32 = 2.0;
pub const LANE_GAP_REM: f32 = 0.5;
/// Width of the scrollbar gutter (rem).
pub const SCROLLBAR_REM: f32 = 0.75;
/// Average advance of the row fonts as a fraction of their size.
pub const AVG_CHAR_WIDTH_EM: f32 = 0.55;
/// Preview / meta font size (rem, `text_xs`).
pub const PREVIEW_FONT_REM: f32 = 0.75;
/// Title font size (rem, `text_sm`).
pub const TITLE_FONT_REM: f32 = 0.875;
/// Width reserved for the time column of line 1 (rem, mono `text_xs`).
pub const TIME_COLUMN_REM: f32 = 8.0;
/// Fields are cut at this multiple of the estimated budget; the exact `…` is
/// drawn by the layout (a safety net against shaping very long summaries).
pub const FIT_SLACK: f32 = 1.3;
/// Upper bound of the preview budget (characters).
pub const PREVIEW_MAX_CHARS_CAP: usize = 400;
/// Lower bound of the preview budget (characters).
pub const PREVIEW_MIN_CHARS: usize = 8;

/// The row height for the current mode (rem).
pub fn row_height_rem(literal: bool) -> f32 {
    if literal {
        ROW_HEIGHT_LITERAL_REM
    } else {
        ROW_HEIGHT_REM
    }
}

/// Whether rows use the four-line layout: the query has quoted literals or
/// unquoted `_` identifiers (core treats both as literal filters; fixes
/// upstream quirk 10).
pub fn literal_mode(query: &str) -> bool {
    literal_mode_of(&HighlightQuery::parse(query), query)
}

/// [`literal_mode`] for a query that is already parsed.
pub fn literal_mode_of(parsed: &HighlightQuery, query: &str) -> bool {
    query_has_literals(query) || !parsed.literal_context_specs().is_empty()
}

/// The preview budget in characters for a line `line_width_px` wide:
/// `floor(width / average character width of text_xs) - 2`, within
/// [`PREVIEW_MIN_CHARS`]..=[`PREVIEW_MAX_CHARS_CAP`] (DESIGN 10.2).
pub fn preview_max_chars(line_width_px: f32, rem_px: f32) -> usize {
    let avg = (PREVIEW_FONT_REM * rem_px * AVG_CHAR_WIDTH_EM).max(1.0);
    let chars = (line_width_px / avg).floor() as i64 - 2;
    (chars.max(0) as usize).clamp(PREVIEW_MIN_CHARS, PREVIEW_MAX_CHARS_CAP)
}

/// The estimated budget (characters) of the title on line 1: the line minus
/// the time column.
pub fn title_max_chars(line_width_px: f32, rem_px: f32) -> usize {
    let avg = (TITLE_FONT_REM * rem_px * AVG_CHAR_WIDTH_EM).max(1.0);
    let room = (line_width_px - TIME_COLUMN_REM * rem_px).max(0.0);
    ((room / avg).floor() as usize).max(PREVIEW_MIN_CHARS)
}

/// The width of a row's text lines in px for a pane `pane_width_px` wide.
pub fn row_line_width_px(pane_width_px: f32, rem_px: f32, has_lane: bool) -> f32 {
    let mut width = pane_width_px - (2.0 * ROW_PADDING_X_REM + SCROLLBAR_REM) * rem_px;
    if has_lane {
        width -= (LANE_WIDTH_REM + LANE_GAP_REM) * rem_px;
    }
    width.max(0.0)
}

/// `text` cut to `max_chars` characters with a trailing `…` (unchanged when
/// it fits); highlight ranges beyond the cut are dropped, a range crossing it
/// is clipped.
pub fn fit_text(field: HighlightedText, max_chars: usize) -> HighlightedText {
    if field.text.chars().count() <= max_chars {
        return field;
    }
    let keep = max_chars.saturating_sub(1);
    let cut = field
        .text
        .char_indices()
        .nth(keep)
        .map(|(ix, _)| ix)
        .unwrap_or(field.text.len());
    let mut text = field.text[..cut].trim_end().to_string();
    let kept_bytes = text.len();
    text.push('\u{2026}');
    let highlights = field
        .highlights
        .into_iter()
        .filter(|&(start, _)| start < kept_bytes)
        .map(|(start, end)| (start, end.min(kept_bytes)))
        .collect();
    HighlightedText { text, highlights }
}

/// A relative timestamp for a row: core's text, with the year added for
/// timestamps of an earlier year (`Nov 08 2025, 14:03`, AD10).
pub fn format_row_time(timestamp: DateTime<Local>, now: DateTime<Local>) -> (String, Recency) {
    let (text, recency) = format_timestamp(timestamp, now);
    if recency == Recency::Old && timestamp <= now && timestamp.year() != now.year() {
        return (timestamp.format("%b %d %Y, %H:%M").to_string(), recency);
    }
    (text, recency)
}

/// Which field line 1 shows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TitleKind {
    Custom,
    Summary,
    Project,
}

/// Everything a visible row needs, built once per frame.
#[derive(Clone, Debug)]
pub struct RowModel {
    /// Position in the filtered list.
    pub index: usize,
    pub path: PathBuf,
    /// The path string: the domain id of the row.
    pub id: SharedString,
    pub stem: String,
    pub selected: bool,
    /// `CC` / `Pi` / `OMP`, only when the corpus has more than one source.
    pub source_label: Option<&'static str>,
    pub title: HighlightedText,
    pub title_kind: TitleKind,
    /// Line 2: the project when it is not the title.
    pub project: Option<HighlightedText>,
    pub message_count: String,
    pub duration: Option<String>,
    /// Hybrid score (Semantic mode with metadata, rows wide enough).
    pub score: Option<String>,
    /// Line 2 tail: the summary when line 1 shows the custom title.
    pub summary_tail: Option<HighlightedText>,
    pub timestamp: String,
    pub recency: Recency,
    pub preview: HighlightedText,
    pub preview_source: PreviewSource,
    /// Line 4 (literal mode).
    pub context: Option<HighlightedText>,
    pub literal: bool,
    pub tooltip: String,
    pub accessibility_label: String,
}

impl RowModel {
    /// The text of line 2: `project - 42 msgs - 1h 5m - 0.87 - summary`.
    pub fn meta_text(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if let Some(project) = &self.project {
            parts.push(&project.text);
        }
        parts.push(&self.message_count);
        if let Some(duration) = &self.duration {
            parts.push(duration);
        }
        if let Some(score) = &self.score {
            parts.push(score);
        }
        if let Some(summary) = &self.summary_tail {
            parts.push(&summary.text);
        }
        parts.join(" \u{b7} ")
    }

    /// The text of line 1 (the title).
    pub fn title_text(&self) -> &str {
        &self.title.text
    }
}

/// What the model builder needs besides the browser (computed once per
/// frame, never per row).
#[derive(Clone, Copy, Debug)]
pub struct RowContext<'a> {
    pub query: &'a HighlightQuery,
    pub now: DateTime<Local>,
    pub literal: bool,
    /// The corpus mixes sources (rows get the `CC`/`Pi`/`OMP` lane).
    pub multiple_sources: bool,
    /// The pane's width in px and the rem size.
    pub pane_width_px: f32,
    pub rem_px: f32,
}

impl RowContext<'_> {
    /// The width of the text lines (px).
    pub fn line_width_px(&self) -> f32 {
        row_line_width_px(self.pane_width_px, self.rem_px, self.multiple_sources)
    }

    /// The semantic score fits (rows at least [`SCORE_MIN_ROW_WIDTH_REM`] wide).
    pub fn show_score(&self) -> bool {
        self.pane_width_px >= SCORE_MIN_ROW_WIDTH_REM * self.rem_px
    }

    /// The preview budget (characters).
    pub fn preview_chars(&self) -> usize {
        preview_max_chars(self.line_width_px(), self.rem_px)
    }
}

/// The model of the row at `pos` of the filtered list; `None` past the end.
pub fn build_row_model(browser: &Browser, pos: usize, ctx: &RowContext<'_>) -> Option<RowModel> {
    let conversation = browser.row(pos)?;
    // The header is built for an unbounded width: every field is present and
    // the layout truncates with an ellipsis (AD20). The source label is its
    // own lane.
    let mut header_input = browser.row_header_input(pos, ctx.query, usize::MAX / 4, ctx.now)?;
    header_input.show_source_label = false;
    let header = build_row_header(&header_input);
    let preview_input = browser.row_preview_input(pos, ctx.query, ctx.preview_chars())?;
    let preview = build_row_preview(&preview_input);

    let stem = conversation
        .path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (timestamp, recency) = format_row_time(conversation.timestamp, ctx.now);
    let project_text = header.project.clone();
    let (title, title_kind, project, summary_tail) = match (header.custom_title, header.summary) {
        (Some(custom), summary) => (custom, TitleKind::Custom, non_empty(project_text), summary),
        (None, Some(summary)) => (summary, TitleKind::Summary, non_empty(project_text), None),
        (None, None) => match non_empty(project_text) {
            Some(project) => (project, TitleKind::Project, None, None),
            None => (
                HighlightedText::plain(stem.clone()),
                TitleKind::Project,
                None,
                None,
            ),
        },
    };
    let project_label = conversation
        .project_name
        .clone()
        .unwrap_or_else(|| stem.clone());
    let mut tooltip = format!("{project_label} \u{b7} {}", title.text);
    if let Some(summary) = &summary_tail {
        tooltip.push_str(" \u{b7} ");
        tooltip.push_str(&summary.text);
    }
    tooltip.push('\n');
    tooltip.push_str(&conversation.path.display().to_string());
    let accessibility_label = format!("{} \u{b7} {project_label} \u{b7} {timestamp}", title.text);
    let source_label = ctx
        .multiple_sources
        .then(|| conversation.source.list_label().trim());

    // Fields are cut at a generous estimate of their width; the layout draws
    // the exact ellipsis.
    let line = ctx.line_width_px();
    let title_budget = (title_max_chars(line, ctx.rem_px) as f32 * FIT_SLACK) as usize;
    let meta_budget = (preview_max_chars(line, ctx.rem_px) as f32 * FIT_SLACK) as usize;
    let title = fit_text(title, title_budget);
    let project = project.map(|field| fit_text(field, meta_budget));
    let summary_tail = summary_tail.map(|field| fit_text(field, meta_budget));

    Some(RowModel {
        index: pos,
        path: conversation.path.clone(),
        id: conversation.path.display().to_string().into(),
        stem,
        selected: browser.selected() == Some(pos),
        source_label,
        title,
        title_kind,
        project,
        message_count: header.message_count,
        duration: header.duration,
        score: header.semantic_score.filter(|_| ctx.show_score()),
        summary_tail,
        timestamp,
        recency,
        preview: preview.preview,
        preview_source: preview.source,
        context: ctx.literal.then_some(preview.context).flatten(),
        literal: ctx.literal,
        tooltip,
        accessibility_label,
    })
}

fn non_empty(text: HighlightedText) -> Option<HighlightedText> {
    (!text.text.is_empty()).then_some(text)
}

// ---------------------------------------------------------------------------
// Text runs
// ---------------------------------------------------------------------------

fn color_style(color: gpui_kit::Hsla) -> HighlightStyle {
    HighlightStyle {
        color: Some(color),
        ..Default::default()
    }
}

/// Split non-overlapping base runs by the (sorted, merged) match ranges and
/// refine the matched parts with `highlight`. Done by hand: gpui's
/// `combine_highlights` picks the winner of two colors in hash order.
pub fn overlay(
    base: Vec<(Range<usize>, HighlightStyle)>,
    matches: &[(usize, usize)],
    highlight: HighlightStyle,
) -> Vec<(Range<usize>, HighlightStyle)> {
    if matches.is_empty() {
        return base;
    }
    let mut out = Vec::with_capacity(base.len() + matches.len() * 2);
    for (range, style) in base {
        let mut pos = range.start;
        for &(match_start, match_end) in matches {
            if match_end <= pos || match_start >= range.end {
                continue;
            }
            let start = match_start.max(pos);
            let end = match_end.min(range.end);
            if start > pos {
                out.push((pos..start, style));
            }
            out.push((start..end, style.highlight(highlight)));
            pos = end;
        }
        if pos < range.end {
            out.push((pos..range.end, style));
        }
    }
    out
}

/// A text built from styled pieces, matches overlaid with `highlight`.
struct Runs {
    text: String,
    runs: Vec<(Range<usize>, HighlightStyle)>,
    matches: Vec<(usize, usize)>,
}

impl Runs {
    fn new() -> Self {
        Self {
            text: String::new(),
            runs: Vec::new(),
            matches: Vec::new(),
        }
    }

    fn push(&mut self, piece: &str, style: HighlightStyle) {
        let start = self.text.len();
        self.text.push_str(piece);
        self.runs.push((start..self.text.len(), style));
    }

    /// Push `field` (its own highlight ranges, shifted to this text).
    fn push_field(&mut self, field: &HighlightedText, style: HighlightStyle) {
        let start = self.text.len();
        self.push(&field.text, style);
        self.matches.extend(
            field
                .highlights
                .iter()
                .map(|&(from, to)| (start + from, start + to)),
        );
    }

    fn into_styled(self, highlight: HighlightStyle) -> StyledText {
        let mut matches = self.matches;
        matches.sort_unstable();
        let runs = overlay(self.runs, &matches, highlight);
        StyledText::new(self.text).with_highlights(runs)
    }
}

// ---------------------------------------------------------------------------
// The element
// ---------------------------------------------------------------------------

/// What the element builder needs besides the model.
pub struct RowStyle {
    pub roles: Roles,
    pub owner: WeakEntity<ListPane>,
    pub mono_font: SharedString,
}

/// The `ListItem` of one row.
pub fn render_row(model: RowModel, style: &RowStyle) -> AnyElement {
    let roles = &style.roles;
    let matched = |color: gpui_kit::Hsla| HighlightStyle {
        color: Some(color),
        font_weight: Some(FontWeight::BOLD),
        background_color: Some(roles.match_background),
        ..Default::default()
    };

    // Line 1: title and time.
    let mut title = Runs::new();
    let (title_color, title_match) = match model.title_kind {
        TitleKind::Custom => (roles.custom_title, roles.custom_title_highlight),
        TitleKind::Summary => (roles.summary, roles.summary_highlight),
        TitleKind::Project => (roles.text_primary, roles.accent),
    };
    title.push_field(
        &model.title,
        HighlightStyle {
            color: Some(title_color),
            font_weight: model.selected.then_some(FontWeight::SEMIBOLD),
            ..Default::default()
        },
    );

    // Line 2: project · N msgs · duration · score · summary.
    let separator = color_style(roles.dot_separator);
    let mut meta = Runs::new();
    if let Some(project) = &model.project {
        meta.push_field(project, color_style(roles.text_secondary));
        meta.push(" · ", separator);
    }
    meta.push(&model.message_count, color_style(roles.msg_count));
    if let Some(duration) = &model.duration {
        meta.push(" · ", separator);
        meta.push(duration, color_style(roles.duration));
    }
    if let Some(score) = &model.score {
        meta.push(" · ", separator);
        meta.push(
            score,
            HighlightStyle {
                color: Some(roles.accent),
                font_weight: Some(FontWeight::SEMIBOLD),
                ..Default::default()
            },
        );
    }
    if let Some(summary) = &model.summary_tail {
        meta.push(" · ", separator);
        meta.push_field(summary, color_style(roles.summary));
    }

    // Line 3: preview.
    let mut preview = Runs::new();
    preview.push_field(&model.preview, color_style(roles.preview));

    let owner_click = style.owner.clone();
    let owner_menu = style.owner.clone();
    let index = model.index;
    let path = model.path.clone();
    let path_menu = model.path.clone();
    let tooltip = model.tooltip.clone();
    let row_height = rems(row_height_rem(model.literal));
    let literal = model.literal;
    let context = model.context.clone();
    let source_label = model.source_label;
    let source_color = roles.source_label;
    let mono = style.mono_font.clone();
    let time_color = roles.recency(model.recency);
    let timestamp = model.timestamp.clone();
    let context_style = color_style(roles.context_base);
    let preview_match = matched(roles.accent);
    let title_highlight = matched(title_match);
    let context_match = matched(roles.context_highlight);
    let border = roles.separator;
    let stem = model.stem.clone();

    ListItem::new(ElementId::Name(format!("row:{}", model.id).into()))
        .selected(model.selected)
        .accessibility_label(model.accessibility_label.clone())
        .h(row_height)
        .px_3()
        .py_1()
        .w_full()
        .border_b_1()
        .border_color(border)
        // Rows never take focus: the search input keeps it (PD2, spike-list §4).
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_mouse_down(MouseButton::Right, move |event, window, cx| {
            window.prevent_default();
            let position = event.position;
            let path = path_menu.clone();
            owner_menu
                .update(cx, |list, cx| {
                    list.on_row_right_click(index, &path, position, window, cx)
                })
                .ok();
        })
        .on_click(move |event, window, cx| {
            let double = event.click_count() >= 2;
            let path = path.clone();
            owner_click
                .update(cx, |list, cx| {
                    list.on_row_click(index, &path, double, window, cx)
                })
                .ok();
        })
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .items_start()
                .gap_2()
                .when_some(source_label, |this, label| {
                    this.child(
                        div()
                            .w(rems(LANE_WIDTH_REM))
                            .flex_none()
                            .text_xs()
                            .line_height(rems(1.25))
                            .font_family(mono.clone())
                            .text_color(source_color)
                            .child(label),
                    )
                })
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .gap_3()
                                .text_sm()
                                .line_height(rems(1.25))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .child(title.into_styled(title_highlight)),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .text_xs()
                                        .whitespace_nowrap()
                                        .font_family(mono)
                                        .text_color(time_color)
                                        .child(timestamp),
                                ),
                        )
                        .child(
                            div()
                                .w_full()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .line_height(rems(1.125))
                                .child(meta.into_styled(preview_match)),
                        )
                        .child(
                            div()
                                .w_full()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .line_height(rems(1.125))
                                .child(preview.into_styled(preview_match)),
                        )
                        .when(literal, |this| {
                            let mut runs = Runs::new();
                            if let Some(context) = &context {
                                runs.push_field(context, context_style);
                            }
                            this.child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .line_height(rems(1.125))
                                    .child(runs.into_styled(context_match)),
                            )
                        }),
                ),
        )
        .qa_id(format!("row:{stem}"))
        .into_any_element()
}

use gpui_kit::prelude::FluentBuilder as _;
