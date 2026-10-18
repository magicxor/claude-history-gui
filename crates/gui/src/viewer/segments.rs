//! Markdown → `Segment`s (DESIGN §9.3; gpui-free, unit-tested): pulldown-cmark
//! with strikethrough and tables, tabs expanded (K10), style flags not colors.
//!
//! A segment is one block-level piece of rendered text (paragraph, heading,
//! list item, quote, code block, table row, rule, tool body, plain rows). It
//! carries style *flags* per byte range; `render.rs` turns them into colors
//! of the active theme. The rules mirror core's `markdown/layout.rs` and the
//! TUI sink (V-033…V-046, V-099, V-100):
//!
//! * only `ENABLE_STRIKETHROUGH | ENABLE_TABLES` (footnotes, task lists and
//!   autolinks stay literal text);
//! * soft and hard breaks become `\n` inside the segment (no reflow);
//! * headings hide their `#` marker; emphasis → italic, strong → bold,
//!   strikethrough → dimmed; inline code → `code` (mono override); links →
//!   `link` with the URL kept aside (tooltip / "Open link?");
//! * HTML (block or inline) is literal text;
//! * code blocks hide their fences, keep blank lines, expand tabs per line;
//! * tables become one `TableRow` per row (plain cells, shared widths);
//! * a `> ` prefix starts every line of a quote (dimmed);
//! * `gap_before` marks the TUI's blank-line equivalents: before headings,
//!   code blocks, quotes, rules, tables, top-level lists and paragraphs —
//!   never before the first segment, between list items, before the first
//!   paragraph of a list item or inside a quote;
//! * trailing blank lines are trimmed; empty segments are dropped.

use std::ops::{BitOr, BitOrAssign, Range};
use std::sync::Arc;

use claude_history_core::markdown::{TAB_WIDTH, expand_tabs};
use claude_history_core::view::{BodyLineKind, ToolBodyLine};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use unicode_width::{UnicodeWidthChar as _, UnicodeWidthStr as _};

/// Style flags of a byte range (combinable; colors are applied per theme).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Style(u16);

impl Style {
    pub const NONE: Style = Style(0);
    pub const BOLD: Style = Style(1);
    pub const ITALIC: Style = Style(1 << 1);
    /// Inline code (code color + mono font).
    pub const CODE: Style = Style(1 << 2);
    pub const HEADING: Style = Style(1 << 3);
    pub const LINK: Style = Style(1 << 4);
    pub const QUOTE: Style = Style(1 << 5);
    /// Muted text; overrides every other foreground (V-016).
    pub const DIMMED: Style = Style(1 << 6);
    pub const STRIKE: Style = Style(1 << 7);
    pub const DIFF_ADD: Style = Style(1 << 8);
    pub const DIFF_REMOVE: Style = Style(1 << 9);
    /// Thinking text (italic, thinking color; overrides syntax colors).
    pub const THINKING: Style = Style(1 << 10);
    /// Tool text color (tool call headers, summary sentences).
    pub const TOOL: Style = Style(1 << 11);

    pub const fn contains(self, other: Style) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn intersects(self, other: Style) -> bool {
        self.0 & other.0 != 0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn bits(self) -> u16 {
        self.0
    }
}

impl BitOr for Style {
    type Output = Style;
    fn bitor(self, rhs: Style) -> Style {
        Style(self.0 | rhs.0)
    }
}

impl BitOrAssign for Style {
    fn bitor_assign(&mut self, rhs: Style) {
        self.0 |= rhs.0;
    }
}

/// The block kind of a segment.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SegKind {
    Para,
    Heading(u8),
    /// A list item: `bullet` is `- ` or `N. ` (empty for a continuation
    /// paragraph of the same item); `depth` is the nesting level (0 = top).
    Item {
        depth: u8,
        bullet: String,
        ordered: bool,
    },
    Quote,
    Code,
    /// One table row: the segment text is the padded row (`cells` padded to
    /// `widths` display cells, separated by two spaces).
    TableRow {
        cells: Vec<String>,
        widths: Arc<Vec<usize>>,
        last: bool,
        /// Some cell of the table holds a non-ASCII glyph: wide glyphs come
        /// from fallback fonts whose advance is not two mono cells, so the
        /// row is laid out as one fixed-width element per cell instead of
        /// padded text (V-045). Equal for every row of a table.
        grid: bool,
    },
    Rule,
    /// Tool call body lines (mono; diff / muted per line).
    ToolBody,
    /// Plain mono rows (subagent results, truncated result rows, headers).
    Plain,
}

/// One rendered block of text with style flags per byte range.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Segment {
    pub kind: SegKind,
    pub text: String,
    /// Sorted, non-overlapping byte ranges (on char boundaries) with flags.
    pub spans: Vec<(Range<usize>, Style)>,
    /// Normalized code language (`Code` segments).
    pub code_lang: Option<String>,
    /// Link text ranges and their URLs.
    pub link_urls: Vec<(Range<usize>, String)>,
    /// A blank-line equivalent precedes this segment (renderer gap).
    pub gap_before: bool,
}

impl Segment {
    fn new(kind: SegKind) -> Self {
        Segment {
            kind,
            text: String::new(),
            spans: Vec::new(),
            code_lang: None,
            link_urls: Vec::new(),
            gap_before: false,
        }
    }

    /// The flags at byte `index` (`NONE` outside every span).
    pub fn style_at(&self, index: usize) -> Style {
        self.spans
            .iter()
            .find(|(range, _)| range.start <= index && index < range.end)
            .map(|(_, style)| *style)
            .unwrap_or_default()
    }

    /// Rendered lines of the text (at least 1).
    pub fn line_count(&self) -> usize {
        self.text.bytes().filter(|b| *b == b'\n').count() + 1
    }

    /// Rendered in the mono font as a whole (code, tool bodies, rows, tables).
    pub fn is_mono(&self) -> bool {
        matches!(
            self.kind,
            SegKind::Code | SegKind::ToolBody | SegKind::Plain | SegKind::TableRow { .. }
        )
    }

    /// Add `style` to every byte of the text (gaps included): dimmed and
    /// thinking blocks override every span (V-016, V-063).
    pub fn overlay(&mut self, style: Style) {
        let len = self.text.len();
        let mut spans = Vec::with_capacity(self.spans.len() * 2 + 1);
        let mut at = 0;
        for (range, flags) in self.spans.drain(..) {
            if range.start > at {
                spans.push((at..range.start, style));
            }
            spans.push((range.clone(), flags | style));
            at = range.end;
        }
        if at < len {
            spans.push((at..len, style));
        }
        self.spans = spans;
    }

    fn push(&mut self, text: &str, style: Style) {
        if text.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(text);
        let end = self.text.len();
        if style.is_empty() {
            return;
        }
        if let Some(last) = self.spans.last_mut()
            && last.0.end == start
            && last.1 == style
        {
            last.0.end = end;
            return;
        }
        self.spans.push((start..end, style));
    }

    /// Display columns of the current (last) line.
    fn last_line_width(&self) -> usize {
        let line = self.text.rsplit('\n').next().unwrap_or("");
        line.width()
    }

    /// Drop trailing line breaks (and the spans past the end).
    fn trim_end_newlines(&mut self) {
        let trimmed = self.text.trim_end_matches('\n').len();
        self.text.truncate(trimmed);
        self.spans.retain_mut(|(range, _)| {
            range.end = range.end.min(trimmed);
            range.start < range.end
        });
        self.link_urls.retain_mut(|(range, _)| {
            range.end = range.end.min(trimmed);
            range.start < range.end
        });
    }
}

/// Whole-message plain text: the segment texts joined with `\n` (copy/search).
pub fn plain_text(segments: &[Segment]) -> String {
    let mut out = String::new();
    for (ix, segment) in segments.iter().enumerate() {
        if ix > 0 {
            out.push('\n');
        }
        out.push_str(&segment.text);
    }
    out
}

// ---------------------------------------------------------------------------
// Markdown
// ---------------------------------------------------------------------------

struct ItemCtx {
    depth: u8,
    bullet: String,
    ordered: bool,
    /// The item's own text segment was flushed (a nested block followed):
    /// further text is a continuation paragraph.
    continued: bool,
    /// The first paragraph of the item started (a later one is loose).
    had_paragraph: bool,
}

struct TableAcc {
    rows: Vec<Vec<String>>,
    cell: Option<String>,
}

struct Builder {
    segs: Vec<Segment>,
    cur: Option<Segment>,
    /// Inline flag stack (emphasis, strong, links, …).
    flags: Vec<Style>,
    /// Open lists: the next ordinal of ordered lists.
    lists: Vec<Option<u64>>,
    items: Vec<ItemCtx>,
    quote: usize,
    /// `items.len()` when each open quote started (an item opened later is
    /// inside the quote).
    quote_items: Vec<usize>,
    code: Option<(Option<String>, String)>,
    table: Option<TableAcc>,
    links: Vec<(usize, String)>,
    /// A gap was requested for the next segment.
    pending_gap: bool,
}

impl Builder {
    fn new() -> Self {
        Builder {
            segs: Vec::new(),
            cur: None,
            flags: Vec::new(),
            lists: Vec::new(),
            items: Vec::new(),
            quote: 0,
            quote_items: Vec::new(),
            code: None,
            table: None,
            links: Vec::new(),
            pending_gap: false,
        }
    }

    fn inline_flags(&self) -> Style {
        let mut style = self.flags.iter().fold(Style::NONE, |acc, f| acc | *f);
        if self.quote > 0 {
            style |= Style::QUOTE;
        }
        style
    }

    /// Request a blank-line equivalent before the next segment (never at
    /// the start of the document).
    fn request_gap(&mut self) {
        self.flush();
        if !self.segs.is_empty() {
            self.pending_gap = true;
        }
    }

    fn start(&mut self, kind: SegKind) {
        self.flush();
        let mut segment = Segment::new(kind);
        segment.gap_before = std::mem::take(&mut self.pending_gap) && !self.segs.is_empty();
        self.cur = Some(segment);
    }

    fn flush(&mut self) {
        let Some(mut segment) = self.cur.take() else {
            return;
        };
        if let Some(item) = self.items.last_mut()
            && matches!(segment.kind, SegKind::Item { .. })
        {
            item.continued = true;
        }
        segment.trim_end_newlines();
        if segment.text.is_empty() && segment.kind != SegKind::Rule {
            // An empty segment keeps its gap request for the next one.
            if segment.gap_before {
                self.pending_gap = true;
            }
            return;
        }
        self.segs.push(segment);
    }

    /// The segment text goes into (starting one when needed).
    fn ensure_text_segment(&mut self) {
        if self.cur.is_some() {
            return;
        }
        if self.quote > 0 {
            let item_inside = self.items.len() > self.quote_items.last().copied().unwrap_or(0);
            let bullet = match self.items.last() {
                Some(item) if item_inside && !item.continued => item.bullet.clone(),
                Some(item) if item_inside => " ".repeat(item.bullet.width()),
                _ => String::new(),
            };
            self.start(SegKind::Quote);
            let prefix = self.quote_prefix();
            if let Some(segment) = self.cur.as_mut() {
                segment.push(&prefix, Style::DIMMED | Style::QUOTE);
                segment.push(&bullet, Style::DIMMED | Style::QUOTE);
            }
        } else if let Some(item) = self.items.last() {
            let kind = SegKind::Item {
                depth: item.depth,
                bullet: if item.continued {
                    String::new()
                } else {
                    item.bullet.clone()
                },
                ordered: item.ordered,
            };
            self.start(kind);
        } else {
            self.start(SegKind::Para);
        }
    }

    fn quote_prefix(&self) -> String {
        "> ".repeat(self.quote)
    }

    /// Display columns before the text of the current line (bullets, quote
    /// prefixes) for tab expansion.
    fn line_prefix_width(&self) -> usize {
        match self.cur.as_ref().map(|s| &s.kind) {
            Some(SegKind::Item { depth, bullet, .. }) if !bullet.is_empty() => {
                usize::from(*depth) * 2 + bullet.width()
            }
            Some(SegKind::Item { depth, .. }) => usize::from(*depth) * 2,
            _ => 0,
        }
    }

    fn text(&mut self, text: &str) {
        if let Some(table) = self.table.as_mut() {
            if let Some(cell) = table.cell.as_mut() {
                let col = cell.width();
                cell.push_str(&expand_tabs(text, col, TAB_WIDTH).replace('\n', " "));
            }
            return;
        }
        if let Some((_, code)) = self.code.as_mut() {
            code.push_str(text);
            return;
        }
        self.ensure_text_segment();
        let style = self.inline_flags();
        let prefix_width = self.line_prefix_width();
        let Some(segment) = self.cur.as_mut() else {
            return;
        };
        let col = prefix_width + segment.last_line_width();
        let expanded = expand_tabs(text, col, TAB_WIDTH).replace('\n', " ");
        segment.push(&expanded, style);
    }

    fn inline_code(&mut self, code: &str) {
        if let Some(table) = self.table.as_mut() {
            if let Some(cell) = table.cell.as_mut() {
                cell.push_str(&code.replace('\n', " "));
            }
            return;
        }
        self.ensure_text_segment();
        let style = self.inline_flags() | Style::CODE;
        if let Some(segment) = self.cur.as_mut() {
            segment.push(code, style);
        }
    }

    fn line_break(&mut self) {
        if let Some(table) = self.table.as_mut() {
            if let Some(cell) = table.cell.as_mut() {
                cell.push(' ');
            }
            return;
        }
        if self.cur.is_none() {
            return;
        }
        let prefix = (self.quote > 0).then(|| self.quote_prefix());
        if let Some(segment) = self.cur.as_mut() {
            segment.push("\n", Style::NONE);
            if let Some(prefix) = prefix {
                segment.push(&prefix, Style::DIMMED | Style::QUOTE);
            }
        }
    }

    fn start_tag(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                if self.quote > 0 {
                    // A further paragraph of the same quote continues it on
                    // a new prefixed line (no gap inside a quote).
                    if self.cur.as_ref().is_some_and(|s| s.kind == SegKind::Quote) {
                        self.line_break();
                    }
                    return;
                }
                if let Some(item) = self.items.last_mut() {
                    if !item.had_paragraph {
                        // The first paragraph of an item continues its bullet line.
                        item.had_paragraph = true;
                        return;
                    }
                    // A loose list's later paragraph: a gap, same item.
                    item.continued = true;
                    self.request_gap();
                    self.ensure_text_segment();
                    return;
                }
                self.request_gap();
                self.start(SegKind::Para);
            }
            Tag::Heading { level, .. } => {
                self.request_gap();
                self.start(SegKind::Heading(level as u8));
                self.flags.push(Style::HEADING | Style::BOLD);
            }
            Tag::BlockQuote(_) => {
                if self.quote == 0 {
                    self.request_gap();
                } else {
                    self.flush();
                }
                self.quote += 1;
                self.quote_items.push(self.items.len());
            }
            Tag::CodeBlock(kind) => {
                self.request_gap();
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => Some(info.to_string()),
                    CodeBlockKind::Indented => None,
                };
                self.code = Some((lang, String::new()));
            }
            Tag::HtmlBlock => {
                self.request_gap();
            }
            Tag::List(start) => {
                if self.items.is_empty() && self.quote == 0 {
                    self.request_gap();
                } else {
                    self.flush();
                }
                self.lists.push(start);
            }
            Tag::Item => {
                self.flush();
                let depth = self.lists.len().saturating_sub(1).min(u8::MAX as usize) as u8;
                let (bullet, ordered) = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let bullet = format!("{n}. ");
                        *n += 1;
                        (bullet, true)
                    }
                    _ => ("- ".to_string(), false),
                };
                self.items.push(ItemCtx {
                    depth,
                    bullet,
                    ordered,
                    continued: false,
                    had_paragraph: false,
                });
            }
            Tag::Emphasis => self.flags.push(Style::ITALIC),
            Tag::Strong => self.flags.push(Style::BOLD),
            Tag::Strikethrough => self.flags.push(Style::DIMMED | Style::STRIKE),
            Tag::Link { dest_url, .. } => {
                if self.table.is_some() || self.code.is_some() {
                    self.flags.push(Style::LINK);
                    self.links.push((usize::MAX, String::new()));
                    return;
                }
                self.ensure_text_segment();
                let start = self.cur.as_ref().map_or(0, |s| s.text.len());
                self.links.push((start, dest_url.to_string()));
                self.flags.push(Style::LINK);
            }
            Tag::Table(_) => {
                self.request_gap();
                self.table = Some(TableAcc {
                    rows: Vec::new(),
                    cell: None,
                });
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    table.rows.push(Vec::new());
                }
            }
            Tag::TableCell => {
                if let Some(table) = self.table.as_mut() {
                    table.cell = Some(String::new());
                }
            }
            _ => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                if self.quote > 0 || !self.items.is_empty() {
                    // Quotes and items keep collecting until their own end.
                    return;
                }
                self.flush();
            }
            TagEnd::Heading(_) => {
                self.flags.pop();
                self.flush();
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote = self.quote.saturating_sub(1);
                self.quote_items.pop();
            }
            TagEnd::CodeBlock => {
                if let Some((lang, code)) = self.code.take() {
                    let mut segment = code_block(&code, lang.as_deref().unwrap_or(""));
                    segment.gap_before = std::mem::take(&mut self.pending_gap);
                    if lang.is_none() {
                        segment.code_lang = None;
                    }
                    self.flush();
                    self.segs.push(segment);
                }
            }
            TagEnd::HtmlBlock => self.flush(),
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
            }
            TagEnd::Item => {
                self.flush();
                self.items.pop();
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.flags.pop();
            }
            TagEnd::Link => {
                self.flags.pop();
                if let Some((start, url)) = self.links.pop()
                    && start != usize::MAX
                    && let Some(segment) = self.cur.as_mut()
                {
                    let end = segment.text.len();
                    if end > start {
                        segment.link_urls.push((start..end, url));
                    }
                }
            }
            TagEnd::TableCell => {
                if let Some(table) = self.table.as_mut()
                    && let Some(cell) = table.cell.take()
                    && let Some(row) = table.rows.last_mut()
                {
                    row.push(cell.trim().to_string());
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    let gap = std::mem::take(&mut self.pending_gap);
                    let mut rows = table_rows(&table.rows);
                    if let Some(first) = rows.first_mut() {
                        first.gap_before = gap;
                    }
                    self.segs.extend(rows);
                }
            }
            _ => {}
        }
    }

    fn rule(&mut self) {
        self.request_gap();
        self.start(SegKind::Rule);
        self.flush();
    }

    fn finish(mut self) -> Vec<Segment> {
        if let Some((lang, code)) = self.code.take() {
            let mut segment = code_block(&code, lang.as_deref().unwrap_or(""));
            segment.gap_before = std::mem::take(&mut self.pending_gap);
            self.flush();
            self.segs.push(segment);
        }
        self.flush();
        self.segs
    }
}

/// Markdown text → segments (see the module documentation for the rules).
pub fn markdown(text: &str) -> Vec<Segment> {
    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
    let mut builder = Builder::new();
    for event in Parser::new_ext(text, options) {
        match event {
            Event::Start(tag) => builder.start_tag(tag),
            Event::End(tag) => builder.end_tag(tag),
            Event::Text(text) => builder.text(&text),
            Event::Code(code) => builder.inline_code(&code),
            Event::SoftBreak | Event::HardBreak => builder.line_break(),
            Event::Rule => builder.rule(),
            Event::Html(html) | Event::InlineHtml(html) => {
                if builder.code.is_none() && builder.table.is_none() {
                    builder.ensure_text_segment();
                    let style = builder.inline_flags();
                    if let Some(segment) = builder.cur.as_mut() {
                        segment.push(&html, style);
                    }
                } else {
                    builder.text(&html);
                }
            }
            Event::FootnoteReference(label) => builder.text(&format!("[^{label}]")),
            Event::TaskListMarker(done) => builder.text(if done { "[x] " } else { "[ ] " }),
            Event::InlineMath(math) | Event::DisplayMath(math) => builder.text(&math),
        }
    }
    builder.finish()
}

/// The table rows of a parsed table (plain cells, shared widths).
fn table_rows(rows: &[Vec<String>]) -> Vec<Segment> {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    if columns == 0 {
        return Vec::new();
    }
    let mut widths = vec![0usize; columns];
    for row in rows {
        for (ix, cell) in row.iter().enumerate() {
            widths[ix] = widths[ix].max(cell.width());
        }
    }
    let widths = Arc::new(widths);
    let grid = rows.iter().flatten().any(|cell| !cell.is_ascii());
    let count = rows.len();
    rows.iter()
        .enumerate()
        .map(|(row_ix, row)| {
            let mut text = String::new();
            for (ix, width) in widths.iter().enumerate() {
                let cell = row.get(ix).map(String::as_str).unwrap_or("");
                if ix > 0 {
                    text.push_str("  ");
                }
                text.push_str(cell);
                if ix + 1 < columns {
                    text.extend(std::iter::repeat_n(' ', width.saturating_sub(cell.width())));
                }
            }
            let text = text.trim_end().to_string();
            let mut cells: Vec<String> = row.clone();
            cells.resize(columns, String::new());
            Segment {
                kind: SegKind::TableRow {
                    cells,
                    widths: widths.clone(),
                    last: row_ix + 1 == count,
                    grid,
                },
                text,
                spans: Vec::new(),
                code_lang: None,
                link_urls: Vec::new(),
                gap_before: false,
            }
        })
        .collect()
}

/// The byte range of every cell inside the padded row text of a
/// [`SegKind::TableRow`] (cells joined by two spaces after padding, trailing
/// blanks trimmed), for the grid layout (V-045).
pub fn table_cell_ranges(
    cells: &[String],
    widths: &[usize],
    text_len: usize,
) -> Vec<std::ops::Range<usize>> {
    let columns = widths.len();
    let mut start = 0;
    let mut out = Vec::with_capacity(columns);
    for (ix, width) in widths.iter().enumerate() {
        let cell = cells.get(ix).map(String::as_str).unwrap_or("");
        let end = start + cell.len();
        out.push(start.min(text_len)..end.min(text_len));
        start = end + width.saturating_sub(cell.width()) * usize::from(ix + 1 < columns) + 2;
    }
    out
}

/// Expand the tabs of every line from column 0.
fn expand_lines(text: &str) -> String {
    if !text.contains('\t') {
        return text.to_string();
    }
    text.split('\n')
        .map(|line| expand_tabs(line, 0, TAB_WIDTH))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A fenced code block (or a fenced tool result, `lang = "text"`): fences
/// hidden, tabs expanded per line, blank lines kept, the trailing line break
/// dropped. `lang` is normalized by `core::syntax` when highlighting.
pub fn code_block(text: &str, lang: &str) -> Segment {
    let body = text.strip_suffix('\n').unwrap_or(text);
    let body = body.strip_suffix('\r').unwrap_or(body);
    let lang = lang
        .split([',', ' ', '\t'])
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    Segment {
        kind: SegKind::Code,
        text: expand_lines(body),
        spans: Vec::new(),
        code_lang: (!lang.is_empty()).then_some(lang),
        link_urls: Vec::new(),
        gap_before: false,
    }
}

/// At most `cols` display cells of `line`, ending in `…` when clipped.
pub fn clip_to_cols(line: &str, cols: usize) -> String {
    if line.width() <= cols {
        return line.to_string();
    }
    let mut out = String::new();
    let mut width = 0;
    for ch in line.chars() {
        let w = ch.width().unwrap_or(0);
        if width + w + 1 > cols {
            break;
        }
        out.push(ch);
        width += w;
    }
    out.push('…');
    out
}

fn prepare_line(line: &str, clip_cols: Option<usize>) -> String {
    let line = expand_tabs(line, 0, TAB_WIDTH);
    match clip_cols {
        Some(cols) => clip_to_cols(&line, cols.max(2)),
        None => line,
    }
}

/// Tool call body lines: `+ ` lines `diff_add`, `- ` lines `diff_remove`,
/// the rest muted; clipped to `clip_cols` display cells when collapsed.
pub fn tool_body(lines: &[ToolBodyLine], clip_cols: Option<usize>) -> Segment {
    let mut segment = Segment::new(SegKind::ToolBody);
    for (ix, line) in lines.iter().enumerate() {
        if ix > 0 {
            segment.push("\n", Style::NONE);
        }
        let style = match line.kind {
            BodyLineKind::Add => Style::DIFF_ADD,
            BodyLineKind::Remove => Style::DIFF_REMOVE,
            BodyLineKind::Plain => Style::DIMMED,
        };
        let text = prepare_line(&line.text, clip_cols);
        if text.is_empty() {
            continue;
        }
        segment.push(&text, style);
    }
    segment
}

/// Plain mono rows (subagent results, truncated result rows), muted when
/// `dimmed`, each line clipped to `clip_cols` display cells when collapsed.
pub fn plain(text: &str, dimmed: bool, clip_cols: Option<usize>) -> Segment {
    let mut segment = Segment::new(SegKind::Plain);
    let joined = text
        .split('\n')
        .map(|line| prepare_line(line.strip_suffix('\r').unwrap_or(line), clip_cols))
        .collect::<Vec<_>>()
        .join("\n");
    segment.push(&joined, if dimmed { Style::DIMMED } else { Style::NONE });
    segment
}

/// A one-line plain segment with `style` (tool headers, summary sentences).
pub fn styled_line(text: &str, style: Style) -> Segment {
    let mut segment = Segment::new(SegKind::Plain);
    segment.push(&expand_tabs(text, 0, TAB_WIDTH), style);
    segment
}

/// Split `segment` into pieces of at most `max_lines` lines (spans and links
/// rebased); `gap_before` stays on the first piece.
pub fn split_lines(segment: &Segment, max_lines: usize) -> Vec<Segment> {
    let max_lines = max_lines.max(1);
    if segment.line_count() <= max_lines {
        return vec![segment.clone()];
    }
    // Byte offsets where each piece starts.
    let mut starts = vec![0usize];
    let mut lines_in_piece = 0;
    for (ix, byte) in segment.text.bytes().enumerate() {
        if byte == b'\n' {
            lines_in_piece += 1;
            if lines_in_piece == max_lines {
                starts.push(ix + 1);
                lines_in_piece = 0;
            }
        }
    }
    let len = segment.text.len();
    let mut pieces = Vec::with_capacity(starts.len());
    for (piece_ix, start) in starts.iter().enumerate() {
        let end = starts.get(piece_ix + 1).map_or(len, |next| next - 1);
        let rebase = |range: &Range<usize>| -> Option<Range<usize>> {
            let s = range.start.max(*start);
            let e = range.end.min(end);
            (s < e).then(|| s - start..e - start)
        };
        let mut piece = Segment {
            kind: segment.kind.clone(),
            text: segment.text[*start..end].to_string(),
            spans: segment
                .spans
                .iter()
                .filter_map(|(range, style)| rebase(range).map(|r| (r, *style)))
                .collect(),
            code_lang: segment.code_lang.clone(),
            link_urls: segment
                .link_urls
                .iter()
                .filter_map(|(range, url)| rebase(range).map(|r| (r, url.clone())))
                .collect(),
            gap_before: piece_ix == 0 && segment.gap_before,
        };
        if piece_ix > 0
            && let SegKind::Item { bullet, .. } = &mut piece.kind
        {
            bullet.clear();
        }
        pieces.push(piece);
    }
    pieces
}

/// Rendered lines of a list of segments (gaps count one line).
pub fn rendered_lines(segments: &[Segment]) -> usize {
    segments
        .iter()
        .map(|s| s.line_count() + usize::from(s.gap_before))
        .sum()
}

/// Split segments into chunks (DESIGN §9.2): unchanged when the whole has at
/// most `limit` rendered lines; otherwise segments longer than `split_over`
/// lines are split into `target`-line pieces and packed into chunks of at
/// most `target` lines at segment boundaries.
pub fn chunk_segments(
    segments: Vec<Segment>,
    limit: usize,
    split_over: usize,
    target: usize,
) -> Vec<Vec<Segment>> {
    if rendered_lines(&segments) <= limit {
        return vec![segments];
    }
    let mut pieces = Vec::new();
    for segment in segments {
        if segment.line_count() > split_over {
            pieces.extend(split_lines(&segment, target));
        } else {
            pieces.push(segment);
        }
    }
    let mut chunks: Vec<Vec<Segment>> = Vec::new();
    let mut current: Vec<Segment> = Vec::new();
    let mut lines = 0;
    for piece in pieces {
        let piece_lines = piece.line_count() + usize::from(piece.gap_before);
        if !current.is_empty() && lines + piece_lines > target {
            chunks.push(std::mem::take(&mut current));
            lines = 0;
        }
        lines += piece_lines;
        current.push(piece);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

// ---------------------------------------------------------------------------
// Search texts
// ---------------------------------------------------------------------------

/// The lowercased text of a segment for search. `origin` maps each byte of
/// `lower` to the byte where its source character starts; it exists only
/// when lowercasing changed byte lengths (ASCII fast path: `None`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchText {
    pub lower: String,
    pub origin: Option<Vec<u32>>,
}

/// Lowercase one character for search (final sigma folds to `σ`, as core's
/// `DocSearchIndex`).
fn fold_char(ch: char) -> impl Iterator<Item = char> {
    ch.to_lowercase().map(|c| if c == 'ς' { 'σ' } else { c })
}

/// Fold a query the same way as [`search_text`].
pub fn fold_query(query: &str) -> String {
    if query.is_ascii() {
        return query.to_ascii_lowercase();
    }
    query.chars().flat_map(fold_char).collect()
}

/// The search text of `text` (built lazily, on the first search).
pub fn search_text_of(text: &str) -> SearchText {
    if text.is_ascii() {
        return SearchText {
            lower: text.to_ascii_lowercase(),
            origin: None,
        };
    }
    let mut lower = String::with_capacity(text.len());
    let mut identity = true;
    for ch in text.chars() {
        let before = lower.len();
        lower.extend(fold_char(ch));
        identity &= lower.len() - before == ch.len_utf8();
    }
    let origin = (!identity).then(|| {
        let mut origin = Vec::with_capacity(lower.len());
        for (start, ch) in text.char_indices() {
            let added: usize = fold_char(ch).map(char::len_utf8).sum();
            origin.extend(std::iter::repeat_n(start as u32, added));
        }
        origin
    });
    SearchText { lower, origin }
}

/// The search text of a segment.
pub fn search_text(segment: &Segment) -> SearchText {
    search_text_of(&segment.text)
}

impl SearchText {
    /// Every non-overlapping occurrence of the folded `query`, as byte
    /// ranges of the original text (whole characters).
    pub fn find(&self, folded_query: &str, original: &str) -> Vec<Range<usize>> {
        if folded_query.is_empty() {
            return Vec::new();
        }
        self.lower
            .match_indices(folded_query)
            .map(|(start, found)| self.original_range(start..start + found.len(), original))
            .collect()
    }

    /// Number of occurrences of the folded `query`.
    pub fn count(&self, folded_query: &str) -> usize {
        if folded_query.is_empty() {
            return 0;
        }
        self.lower.matches(folded_query).count()
    }

    fn original_range(&self, folded: Range<usize>, original: &str) -> Range<usize> {
        let Some(origin) = &self.origin else {
            return folded;
        };
        let start = origin[folded.start] as usize;
        let last = origin[folded.end - 1] as usize;
        let end = last + original[last..].chars().next().map_or(0, char::len_utf8);
        start..end
    }
}
