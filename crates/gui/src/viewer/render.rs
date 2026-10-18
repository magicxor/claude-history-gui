//! The item renderer (DESIGN §9.4): focus mark, timestamp lane, label lane,
//! separator, content; search highlights overlaid deterministically. WP2.
//!
//! ```text
//! h_flex (one ledger row per item)
//!  ├─ focus mark   2 px accent bar + item tint when its message is focused (PD7 flag)
//!  ├─ timestamp    mono text_xs, secondary — only while timing is on (lane always reserved)
//!  ├─ label        mono text_xs right-aligned, role color — first chunk only
//!  ├─ separator    border_l_1 (dimmed rows: 50 % opacity)
//!  └─ content      one SelText per segment; code boxed; tables as mono rows
//!                  in a horizontally scrollable block; tool chevrons and the
//!                  "(N more lines…)" indicator
//! ```
//!
//! Style runs are built in a fixed order: the segment's flags (or syntect
//! tokens for code), then the search matches on top (`combine_highlights`).
//! Dimmed and thinking segments get no syntax colors (V-016, V-063).

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use claude_history_core::theme::ThemeKind;
use claude_history_core::view::ToolOutputId;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ElementId, FontStyle, FontWeight, HighlightStyle, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Pixels, Point, SharedString,
    StatefulInteractiveElement as _, StrikethroughStyle, Styled as _, TestSupportExt as _,
    UnderlineStyle, WeakEntity, Window, canvas, combine_highlights, div, px, rems,
};

use super::ViewerPane;
use super::items::{ItemKind, LaneRole, ViewerItem};
use super::lru::{HandleCache, TokenCache};
use super::search::MatchesByItem;
use super::segments::{SegKind, Segment, Style, table_cell_ranges};
use super::seltext::{LinkHandler, SelText};
use crate::theme_tokens::Roles;

/// Width of the timestamp lane (rem).
pub const TIMESTAMP_LANE_REM: f32 = 6.0;
/// Width of the label lane (rem): 9 mono cells at `text_xs` plus padding.
pub const LABEL_LANE_REM: f32 = 5.0;
/// Horizontal space of the separator and the content padding (rem).
pub const CONTENT_PADDING_REM: f32 = 1.75;
/// Width of the focus mark (px).
pub const FOCUS_MARK_PX: f32 = 2.0;

/// Everything the list closure needs for one frame (DESIGN §9.6).
pub struct RenderCtx {
    pub items: Arc<Vec<ViewerItem>>,
    pub roles: Roles,
    pub mono: SharedString,
    pub kind: ThemeKind,
    pub show_timing: bool,
    /// The nav position marked as focused (nav active only).
    pub focused_message: Option<usize>,
    pub hovered: Option<ToolOutputId>,
    pub matches: Rc<MatchesByItem>,
    pub handles: Rc<RefCell<HandleCache>>,
    pub tokens: Rc<RefCell<TokenCache>>,
    /// `(item, segment, byte)` whose position the next paint reports.
    pub reveal: Option<(usize, usize, usize)>,
    pub reveal_y: Rc<Cell<Option<Point<Pixels>>>>,
    /// A position probe (tests): `(item, segment, byte)` and its cell.
    pub probe: Option<(usize, usize, usize)>,
    pub probe_point: Rc<Cell<Option<Point<Pixels>>>>,
    /// Items painted in the current frame.
    pub painted: Rc<RefCell<Vec<usize>>>,
    pub weak: WeakEntity<ViewerPane>,
    pub on_link_click: LinkHandler,
    pub on_link_hover: LinkHandler,
    /// Width of one mono cell at `text_xs` (the table grid layout, V-045).
    pub cell_px: Pixels,
    /// Select All is active: every segment paints as selected (V-088).
    pub select_all: bool,
}

/// The highlight style of a set of flags in `roles`.
pub fn style_for(flags: Style, roles: &Roles) -> HighlightStyle {
    let mut style = HighlightStyle::default();
    if flags.contains(Style::BOLD) {
        style.font_weight = Some(FontWeight::BOLD);
    }
    if flags.contains(Style::ITALIC) {
        style.font_style = Some(FontStyle::Italic);
    }
    if flags.contains(Style::HEADING) {
        style.color = Some(roles.heading);
    }
    if flags.contains(Style::QUOTE) {
        style.color = Some(roles.quote);
    }
    if flags.contains(Style::CODE) {
        style.color = Some(roles.code);
    }
    if flags.contains(Style::LINK) {
        style.color = Some(roles.link);
        style.underline = Some(UnderlineStyle {
            thickness: px(1.),
            color: Some(roles.link),
            wavy: false,
        });
    }
    if flags.contains(Style::TOOL) {
        style.color = Some(roles.tool_text);
    }
    if flags.contains(Style::DIFF_ADD) {
        style.color = Some(roles.diff_add);
    }
    if flags.contains(Style::DIFF_REMOVE) {
        style.color = Some(roles.diff_remove);
    }
    if flags.contains(Style::STRIKE) {
        style.strikethrough = Some(StrikethroughStyle {
            thickness: px(1.),
            color: Some(roles.dimmed),
        });
    }
    if flags.contains(Style::THINKING) {
        style.color = Some(roles.thinking);
        style.font_style = Some(FontStyle::Italic);
    }
    if flags.contains(Style::DIMMED) {
        style.color = Some(roles.dimmed);
    }
    style
}

/// Whether a code segment gets syntect tokens: never when dimmed or
/// thinking (those colors override syntax colors, V-016, V-063).
pub fn wants_tokens(segment: &Segment) -> bool {
    segment.kind == SegKind::Code
        && segment.code_lang.is_some()
        && !segment
            .spans
            .iter()
            .any(|(_, flags)| flags.intersects(Style::DIMMED | Style::THINKING))
}

/// The base runs of a segment: its flags, or syntect tokens for code that
/// is neither dimmed nor thinking (empty while tokens are pending).
fn base_runs(
    ctx: &RenderCtx,
    item: &ViewerItem,
    segment_ix: usize,
    segment: &Segment,
) -> Vec<(Range<usize>, HighlightStyle)> {
    if wants_tokens(segment)
        && let Some(lang) = &segment.code_lang
    {
        let key = (item.id.clone(), segment_ix as u16, ctx.kind);
        let mut tokens = ctx.tokens.borrow_mut();
        if let Some(runs) = tokens.lru.get(&key) {
            return (*runs).clone();
        }
        if !tokens.requested.contains(&key) {
            tokens.requested.insert(key.clone());
            tokens
                .wanted
                .push((key, segment.text.clone(), lang.clone()));
        }
        return Vec::new();
    }
    segment
        .spans
        .iter()
        .map(|(range, flags)| (range.clone(), style_for(*flags, &ctx.roles)))
        .collect()
}

/// The styles of search matches (V-075): `(other matches, current match)`.
/// Both set a background and a foreground, so they win over every base
/// color (flags and syntax tokens) where they apply.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatchStyles {
    pub other: HighlightStyle,
    pub current: HighlightStyle,
}

// The legible text color on a match background: the role's own foreground
// unless black or white contrasts clearly better (V-075; the colors live in
// theme_tokens.rs, X-019).
use crate::theme_tokens::legible_on;

impl MatchStyles {
    pub fn new(roles: &Roles) -> Self {
        MatchStyles {
            other: HighlightStyle {
                background_color: Some(roles.match_background),
                color: Some(legible_on(
                    roles.match_background,
                    roles.match_foreground,
                    roles.background,
                )),
                ..Default::default()
            },
            current: HighlightStyle {
                background_color: Some(roles.current_match_background),
                color: Some(legible_on(
                    roles.current_match_background,
                    roles.current_match_foreground,
                    roles.background,
                )),
                ..Default::default()
            },
        }
    }
}

/// Overlay the search matches of segment `segment_ix` on its base runs
/// (§9.4): base runs first, matches on top (`combine_highlights` lets the
/// later list override overlapping fields), the current match with its own
/// style. `matches` are one item's `(segment, range, is_current)` in order.
pub fn overlay_matches(
    base: Vec<(Range<usize>, HighlightStyle)>,
    matches: &[(usize, Range<usize>, bool)],
    segment_ix: usize,
    styles: MatchStyles,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let runs: Vec<(Range<usize>, HighlightStyle)> = matches
        .iter()
        .filter(|(segment, range, _)| *segment == segment_ix && !range.is_empty())
        .map(|(_, range, current)| {
            let style = if *current {
                styles.current
            } else {
                styles.other
            };
            (range.clone(), style)
        })
        .collect();
    if runs.is_empty() {
        base
    } else {
        combine_highlights(base, runs).collect()
    }
}

/// The final runs of a segment (deterministic overlay: base, then matches).
pub fn segment_runs(
    ctx: &RenderCtx,
    item: &ViewerItem,
    item_ix: usize,
    segment_ix: usize,
    segment: &Segment,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let base = base_runs(ctx, item, segment_ix, segment);
    match ctx.matches.get(&item_ix) {
        Some(matches) => overlay_matches(base, matches, segment_ix, MatchStyles::new(&ctx.roles)),
        None => base,
    }
}

fn lane_color(role: LaneRole, roles: &Roles) -> gpui_kit::Hsla {
    match role {
        LaneRole::TextPrimary => roles.text_primary,
        LaneRole::Accent => roles.accent,
        LaneRole::AccentDim => roles.accent_dim,
        LaneRole::TextSecondary => roles.text_secondary,
        LaneRole::ToolText => roles.tool_text,
    }
}

/// The `SelText` of one segment.
fn seg_text(
    ctx: &RenderCtx,
    item: &ViewerItem,
    item_ix: usize,
    segment_ix: usize,
    segment: &Segment,
    cx: &mut App,
) -> SelText {
    seg_part(ctx, item, item_ix, segment_ix, segment, None, cx)
}

/// The runs of `runs` inside `range`, shifted to start at 0.
fn clip_runs(
    runs: Vec<(Range<usize>, HighlightStyle)>,
    range: &Range<usize>,
) -> Vec<(Range<usize>, HighlightStyle)> {
    runs.into_iter()
        .filter_map(|(run, style)| {
            let start = run.start.max(range.start);
            let end = run.end.min(range.end);
            (start < end).then(|| (start - range.start..end - range.start, style))
        })
        .collect()
}

/// The `SelText` of a segment, or of one grid cell of it (`cell`: its index
/// and byte range in the segment text; own selection handle and order).
fn seg_part(
    ctx: &RenderCtx,
    item: &ViewerItem,
    item_ix: usize,
    segment_ix: usize,
    segment: &Segment,
    cell: Option<(usize, Range<usize>)>,
    cx: &mut App,
) -> SelText {
    let part = cell.as_ref().map(|(_, range)| range.clone());
    let shown = match &part {
        Some(range) => &segment.text[range.clone()],
        None => segment.text.as_str(),
    };
    let mut runs = segment_runs(ctx, item, item_ix, segment_ix, segment);
    if let Some(range) = &part {
        runs = clip_runs(runs, range);
    }
    let mono_ranges: Vec<Range<usize>> = if segment.is_mono() {
        Vec::new()
    } else {
        segment
            .spans
            .iter()
            .filter(|(_, flags)| flags.contains(Style::CODE))
            .map(|(range, _)| range.clone())
            .collect()
    };
    let cell_ix = cell.as_ref().map_or(0, |(ix, _)| (*ix).min(255));
    let key = match &cell {
        Some(_) => (1u32 << 24) | ((segment_ix as u32) << 8) | cell_ix as u32,
        None => segment_ix as u32,
    };
    let handle = ctx
        .handles
        .borrow_mut()
        .get_or_create(&item.id, key, item_ix, shown, cx);
    let id = ElementId::NamedInteger(
        "seg".into(),
        item.key_hash
            .wrapping_mul(31)
            .wrapping_add(segment_ix as u64)
            .wrapping_mul(257)
            .wrapping_add(if cell.is_some() {
                cell_ix as u64 + 1
            } else {
                0
            }),
    );
    let mut text = SelText::new(
        id,
        handle,
        SharedString::from(shown.to_string()),
        runs,
        mono_ranges,
        ctx.mono.clone(),
    )
    .order(((item_ix as u64) * 1024 + segment_ix as u64) * 256 + cell_ix as u64)
    .selection_color(ctx.roles.selection)
    .select_all(ctx.select_all);
    if let Some((reveal_item, reveal_segment, byte)) = ctx.reveal
        && reveal_item == item_ix
        && reveal_segment == segment_ix
        && let Some(byte) = in_part(byte, &part, segment.text.len())
    {
        text = text.reveal(byte, ctx.reveal_y.clone());
    }
    if let Some((probe_item, probe_segment, byte)) = ctx.probe
        && probe_item == item_ix
        && probe_segment == segment_ix
        && let Some(byte) = in_part(byte, &part, segment.text.len())
    {
        text = text.reveal(byte, ctx.probe_point.clone());
    }
    if !segment.link_urls.is_empty() {
        let links = segment
            .link_urls
            .iter()
            .map(|(range, url)| (range.clone(), SharedString::from(url.clone())))
            .collect();
        text = text.links(links, ctx.on_link_click.clone(), ctx.on_link_hover.clone());
    }
    text
}

/// `byte` (segment text offset) as an offset inside `part`, `None` when it is
/// elsewhere; without a part, clamped to the text.
fn in_part(byte: usize, part: &Option<Range<usize>>, len: usize) -> Option<usize> {
    match part {
        None => Some(byte.min(len)),
        Some(range) => (range.start..=range.end)
            .contains(&byte)
            .then(|| byte - range.start),
    }
}

/// One segment wrapped in its block element.
fn render_segment(
    ctx: &RenderCtx,
    item: &ViewerItem,
    item_ix: usize,
    segment_ix: usize,
    segment: &Segment,
    cx: &mut App,
) -> AnyElement {
    let roles = &ctx.roles;
    let gap = segment.gap_before && segment_ix > 0;
    let element: AnyElement = match &segment.kind {
        SegKind::Rule => div()
            .h(px(1.))
            .w(rems(20.))
            .my_1()
            .bg(roles.border)
            .into_any_element(),
        SegKind::Heading(level) => {
            let text = seg_text(ctx, item, item_ix, segment_ix, segment, cx);
            div()
                .map(|d| match level {
                    1 => d.text_lg(),
                    2 => d.text_base(),
                    _ => d.text_sm(),
                })
                .child(text)
                .into_any_element()
        }
        SegKind::Item {
            depth,
            bullet,
            ordered,
        } => {
            let text = seg_text(ctx, item, item_ix, segment_ix, segment, cx);
            let bullet_color = if *ordered {
                roles.dimmed
            } else {
                roles.text_primary
            };
            gpui_kit::component::h_flex()
                .items_start()
                .pl(rems(f32::from(*depth) * 1.25))
                .child(
                    div()
                        .flex_none()
                        .min_w(rems(1.25))
                        .text_color(bullet_color)
                        .child(SharedString::from(bullet.clone())),
                )
                .child(div().flex_1().min_w_0().child(text))
                .into_any_element()
        }
        SegKind::Quote => {
            let text = seg_text(ctx, item, item_ix, segment_ix, segment, cx);
            div().text_color(roles.quote).child(text).into_any_element()
        }
        SegKind::Code => {
            let radius = cx.theme().radius;
            let text = seg_text(ctx, item, item_ix, segment_ix, segment, cx);
            let boxed = item.kind != ItemKind::ToolResult || item.first_chunk;
            div()
                .bg(roles.code_background)
                .when(boxed, |d| d.rounded(radius))
                .px_2()
                .py_1()
                .font_family(ctx.mono.clone())
                .text_xs()
                .text_color(roles.code)
                .child(text)
                .into_any_element()
        }
        SegKind::TableRow {
            cells,
            widths,
            grid: true,
            ..
        } => {
            // One fixed-width element per cell: columns stay aligned
            // whatever advance the fallback font gives wide glyphs (V-045).
            let ranges = table_cell_ranges(cells, widths, segment.text.len());
            let mut row = gpui_kit::component::h_flex()
                .font_family(ctx.mono.clone())
                .text_xs()
                .whitespace_nowrap()
                .border_b_1()
                .border_color(roles.border);
            for (ix, range) in ranges.into_iter().enumerate() {
                let text = seg_part(
                    ctx,
                    item,
                    item_ix,
                    segment_ix,
                    segment,
                    Some((ix, range)),
                    cx,
                );
                let columns = (widths[ix] + 2) as f32;
                row = row.child(div().flex_none().w(ctx.cell_px * columns).child(text));
            }
            row.into_any_element()
        }
        SegKind::TableRow { .. } => {
            let text = seg_text(ctx, item, item_ix, segment_ix, segment, cx);
            div()
                .font_family(ctx.mono.clone())
                .text_xs()
                .whitespace_nowrap()
                .border_b_1()
                .border_color(roles.border)
                .child(text)
                .into_any_element()
        }
        SegKind::ToolBody | SegKind::Plain => {
            let text = seg_text(ctx, item, item_ix, segment_ix, segment, cx);
            div()
                .font_family(ctx.mono.clone())
                .text_xs()
                .child(text)
                .into_any_element()
        }
        SegKind::Para => seg_text(ctx, item, item_ix, segment_ix, segment, cx).into_any_element(),
    };
    if gap {
        div().pt(rems(0.6)).child(element).into_any_element()
    } else {
        element
    }
}

/// The content column of an item: its segments (tables grouped in a
/// horizontally scrollable block) and tool controls.
fn render_content(ctx: &RenderCtx, item: &ViewerItem, item_ix: usize, cx: &mut App) -> AnyElement {
    let roles = &ctx.roles;
    let mut column = gpui_kit::component::v_flex().w_full().min_w_0();
    if let Some(group) = item.group_toggle.clone() {
        column = column.child(
            gpui_kit::component::h_flex()
                .gap_1()
                .text_xs()
                .text_color(roles.dimmed)
                .child(chevron(ctx, item_ix, group, true, "group-chevron"))
                .child("tool details"),
        );
    }
    let mut segment_ix = 0;
    let segments = item.segments.clone();
    while segment_ix < segments.len() {
        let segment = &segments[segment_ix];
        if crate::viewer::items::is_header(item, segment_ix) {
            let header = seg_text(ctx, item, item_ix, segment_ix, segment, cx);
            let chevron = item
                .toggle
                .clone()
                .filter(|_| item.kind != ItemKind::ToolSummary)
                .map(|id| chevron(ctx, item_ix, id, item.expanded, "chevron"));
            column = column.child(
                gpui_kit::component::h_flex()
                    .gap_1()
                    .items_start()
                    .font_family(ctx.mono.clone())
                    .text_xs()
                    .children(chevron)
                    .child(div().flex_1().min_w_0().child(header)),
            );
            if item.kind == ItemKind::ToolCall && segments.len() > 1 {
                // The ledger's empty row between a call header and its body.
                column = column.child(div().h(rems(0.5)));
            }
            segment_ix += 1;
            continue;
        }
        if matches!(segment.kind, SegKind::TableRow { .. }) {
            let start = segment_ix;
            let mut rows = gpui_kit::component::v_flex();
            while segment_ix < segments.len()
                && matches!(segments[segment_ix].kind, SegKind::TableRow { .. })
            {
                rows = rows.child(render_segment(
                    ctx,
                    item,
                    item_ix,
                    segment_ix,
                    &segments[segment_ix],
                    cx,
                ));
                segment_ix += 1;
            }
            let gap = segments[start].gap_before && start > 0;
            // A table wider than the content column scrolls horizontally,
            // with the kit's horizontal scrollbar (V-018).
            column = column.child(
                div().w_full().when(gap, |d| d.pt(rems(0.6))).child(
                    div()
                        .w_full()
                        .overflow_x_scrollbar()
                        .id(ElementId::NamedInteger(
                            "table".into(),
                            item.key_hash.wrapping_add(start as u64),
                        ))
                        .child(rows),
                ),
            );
            continue;
        }
        if item.kind == ItemKind::ToolSummary {
            let chevron = item
                .toggle
                .clone()
                .map(|id| chevron(ctx, item_ix, id, false, "summary-chevron"));
            column = column.child(
                gpui_kit::component::h_flex()
                    .gap_1()
                    .items_start()
                    .children(chevron)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(render_segment(ctx, item, item_ix, segment_ix, segment, cx)),
                    ),
            );
            segment_ix += 1;
            continue;
        }
        column = column.child(render_segment(ctx, item, item_ix, segment_ix, segment, cx));
        segment_ix += 1;
    }
    if item.last_chunk
        && let (Some(more), Some(id)) = (item.more, item.toggle.clone())
    {
        let weak = ctx.weak.clone();
        column = column.child(
            div()
                .id(ElementId::NamedInteger("more".into(), item.key_hash))
                .cursor_pointer()
                .font_family(ctx.mono.clone())
                .text_xs()
                .text_color(roles.dimmed)
                .hover(|s| s.underline())
                .child(SharedString::from(more_lines_text(more)))
                .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                .on_click(move |_, window, cx| {
                    weak.update(cx, |this, cx| this.toggle_output(&id, item_ix, window, cx))
                        .ok();
                })
                .test_support(),
        );
    }
    // An expanded result has no header chevron: give it one to collapse
    // again (the original keeps expanded result rows clickable, V-056).
    if item.last_chunk
        && item.expanded
        && item.more.is_none()
        && item.kind == ItemKind::ToolResult
        && let Some(id) = item.toggle.clone()
    {
        column = column.child(
            gpui_kit::component::h_flex()
                .gap_1()
                .font_family(ctx.mono.clone())
                .text_xs()
                .text_color(roles.dimmed)
                .child(chevron(ctx, item_ix, id, true, "collapse-chevron"))
                .child("collapse"),
        );
    }
    column.into_any_element()
}

/// `(N more lines…)` (TUI `(N more lines...)`, in-app copy rules).
pub fn more_lines_text(more: usize) -> String {
    format!("({more} more lines…)")
}

fn chevron(
    ctx: &RenderCtx,
    item_ix: usize,
    id: ToolOutputId,
    expanded: bool,
    name: &'static str,
) -> AnyElement {
    let weak = ctx.weak.clone();
    let key = ctx.items[item_ix].key_hash;
    div()
        .id(ElementId::NamedInteger(name.into(), key))
        .flex_none()
        .w(rems(0.9))
        .cursor_pointer()
        .text_color(ctx.roles.accent_dim)
        .child(if expanded { "▾" } else { "▸" })
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(move |_, window, cx| {
            weak.update(cx, |this, cx| this.toggle_output(&id, item_ix, window, cx))
                .ok();
        })
        .test_support()
        .into_any_element()
}

/// One transcript row (the list closure).
pub fn render_item(
    ctx: &Rc<RenderCtx>,
    item_ix: usize,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Some(item) = ctx.items.get(item_ix) else {
        return div().into_any_element();
    };
    let roles = ctx.roles;
    let focused = ctx.focused_message.is_some() && item.nav_position == ctx.focused_message;
    let hover_id = item.hover_id().cloned();
    let hovered = hover_id
        .as_ref()
        .is_some_and(|id| ctx.hovered.as_ref() == Some(id));
    let lanes = &item.lanes;
    let label_color = if lanes.dimmed {
        roles.dimmed
    } else {
        lane_color(lanes.role, &roles)
    };
    let separator = if lanes.dimmed {
        roles.separator.opacity(0.5)
    } else {
        roles.separator
    };
    let content = render_content(ctx, item, item_ix, cx);
    let painted = ctx.painted.clone();
    let weak = ctx.weak.clone();
    let hover_weak = ctx.weak.clone();
    gpui_kit::component::h_flex()
        .id(ElementId::NamedInteger("viewer-item".into(), item.key_hash))
        .w_full()
        .items_start()
        .relative()
        .when(item.first_of_message, |d| d.pt_3())
        .when(!item.first_of_message && item.first_chunk, |d| d.pt_1())
        .when(focused, |d| d.bg(roles.focus_tint))
        .when(hovered && !focused, |d| d.bg(roles.list_hover))
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            weak.update(cx, |this, cx| this.on_item_mouse_down(item_ix, cx))
                .ok();
        })
        .when_some(hover_id, move |d, id| {
            d.on_hover(move |hovered: &bool, _, cx| {
                let hovered = *hovered;
                let id = id.clone();
                hover_weak
                    .update(cx, |this, cx| this.set_hovered(id, hovered, cx))
                    .ok();
            })
        })
        .child(
            div()
                .flex_none()
                .w(px(FOCUS_MARK_PX))
                .self_stretch()
                .when(focused, |d| d.bg(roles.focus_mark)),
        )
        .when(ctx.show_timing, |d| {
            d.child(
                div()
                    .flex_none()
                    .w(rems(TIMESTAMP_LANE_REM))
                    .pl_1()
                    .text_xs()
                    .font_family(ctx.mono.clone())
                    .text_color(roles.text_secondary)
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .when_some(lanes.timestamp.clone(), |d, stamp| {
                        d.child(SharedString::from(stamp))
                    }),
            )
        })
        .child(
            div()
                .flex_none()
                .w(rems(LABEL_LANE_REM))
                .pr_2()
                .text_xs()
                .font_family(ctx.mono.clone())
                .text_right()
                .whitespace_nowrap()
                .overflow_hidden()
                .text_color(label_color)
                .when(lanes.bold, |d| d.font_weight(FontWeight::BOLD))
                .when_some(lanes.label.clone(), |d, label| {
                    d.child(SharedString::from(label))
                }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .border_l_1()
                .border_color(separator)
                .pl_3()
                .pr_4()
                .text_sm()
                .text_color(roles.foreground)
                .child(content),
        )
        .child(
            canvas(
                |_, _, _| (),
                move |_, _, _, _| painted.borrow_mut().push(item_ix),
            )
            .absolute()
            .size_0(),
        )
        .into_any_element()
}
