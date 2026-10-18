use super::*;
use crate::view::document::DocBlock;
use crate::view::{ToolDisplayMode, ViewPrefs};

fn write(dir: &Path, name: &str, lines: &[&str]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    path
}

/// Upstream `interaction_tests.rs` `write_tool_conversation`.
fn write_tool_conversation(dir: &Path) -> PathBuf {
    write(
        dir,
        "tool.jsonl",
        &[
            r#"{"type":"assistant","timestamp":"2024-01-01T00:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"one\ntwo\nthree\nfour\nfive"}}]}}"#,
        ],
    )
}

fn conversation_lines() -> Vec<&'static str> {
    vec![
        r#"{"type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Find the Bug please"}}"#,
        r#"{"type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":"Looking for the bug."},{"type":"tool_use","id":"toolu_1","name":"Grep","input":{"pattern":"bug"}}]}}"#,
        r#"{"type":"user","timestamp":"2024-01-01T00:00:02Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"src/bug.rs: BUG here\nsrc/other.rs\nl3\nl4\nl5 bug"}]}}"#,
        r#"{"type":"pi-metadata","label":"notice","text":"a bug note"}"#,
        r#"{"type":"assistant","timestamp":"2024-01-01T00:00:03Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"the bug is subtle","signature":"s"},{"type":"text","text":"Fixed the bug."}]}}"#,
    ]
}

fn truncated() -> ViewPrefs {
    ViewPrefs {
        tool_display: ToolDisplayMode::Truncated,
        show_thinking: false,
        show_timing: false,
    }
}

fn ledger_text(rendered: &RenderedConversation) -> String {
    rendered
        .lines
        .iter()
        .flat_map(|line| line.spans.iter().map(|(text, _)| text.as_str()))
        .collect()
}

fn first_clickable_id(rendered: &RenderedConversation) -> ToolOutputId {
    rendered
        .lines
        .iter()
        .find(|line| line.clickable)
        .and_then(|line| line.tool_output_id.clone())
        .unwrap()
}

fn doc_text(doc: &ConversationDocument) -> String {
    doc.messages
        .iter()
        .flat_map(|m| m.blocks.iter().map(DocBlock::plain_text))
        .collect::<Vec<_>>()
        .join("\n")
}

// -----------------------------------------------------------------
// ViewerModel (upstream interaction tests on the viewer)
// -----------------------------------------------------------------

#[test]
fn view_click_toggles_clickable_output() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_tool_conversation(dir.path());
    let mut model = ViewerModel::open(&path, truncated()).unwrap();

    let ledger = model.ledger(80);
    assert!(!ledger_text(&ledger).contains("five"));
    let id = first_clickable_id(&ledger);
    // The document offers the same toggle.
    let doc = model.document();
    assert_eq!(doc.messages[0].blocks[0].toggle_id(), Some(&id));

    assert!(model.toggle_expanded(&id));
    assert!(model.is_expanded(&id));
    assert!(ledger_text(&model.ledger(80)).contains("five"));
    assert!(doc_text(&model.document()).contains("five"));

    assert!(!model.toggle_expanded(&id));
    assert!(model.expanded.is_empty());
    assert!(!ledger_text(&model.ledger(80)).contains("five"));
}

/// V-101: `open_cancellable` abandons the parse mid-file once `cancelled`
/// says so, and equals `open` when it never does.
#[test]
fn open_cancellable_stops_midway_and_matches_open_otherwise() {
    use std::cell::Cell;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.jsonl");
    let line = r#"{"type":"user","uuid":"u","timestamp":"2025-01-01T00:00:00Z","message":{"role":"user","content":"hello"}}"#;
    std::fs::write(&path, format!("{line}\n").repeat(1000)).unwrap();

    let polls = Cell::new(0usize);
    let stop_at_third = || {
        polls.set(polls.get() + 1);
        polls.get() >= 3
    };
    let out = ViewerModel::open_cancellable(&path, ViewPrefs::default(), &stop_at_third).unwrap();
    assert!(out.is_none());
    assert_eq!(polls.get(), 3, "polled before the file and every 64 lines");

    let full = ViewerModel::open_cancellable(&path, ViewPrefs::default(), &|| false)
        .unwrap()
        .expect("never cancelled");
    let plain = ViewerModel::open(&path, ViewPrefs::default()).unwrap();
    assert_eq!(full.entries.len(), plain.entries.len());
    assert_eq!(full.entries.len(), 1000);
}

#[test]
fn view_click_uses_cached_entries_after_file_removed() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_tool_conversation(dir.path());
    let mut model = ViewerModel::open(&path, truncated()).unwrap();
    std::fs::remove_file(&path).unwrap();

    let id = first_clickable_id(&model.ledger(80));
    model.toggle_expanded(&id);
    assert!(ledger_text(&model.ledger(80)).contains("five"));
    assert!(doc_text(&model.document()).contains("five"));
    model.toggle_expanded(&id);
    assert!(!ledger_text(&model.ledger(80)).contains("five"));

    // Toggles and copies keep working from the cache.
    assert_eq!(model.cycle_tool_display(), ToolDisplayMode::Full);
    assert!(!model.document().messages.is_empty());
    assert_eq!(
        model.message_text(0).unwrap(),
        "Bash: one\ntwo\nthree\nfour\nfive"
    );
    assert!(model.plain_display().contains("Claude: Bash: one"));
    // Summary mode hides tools from copies.
    model.cycle_tool_display();
    assert_eq!(model.message_text(0).unwrap(), "");
    assert_eq!(model.plain_display(), "");
}

#[test]
fn open_reports_io_errors() {
    let dir = tempfile::tempdir().unwrap();
    let error =
        ViewerModel::open(&dir.path().join("missing.jsonl"), ViewPrefs::default()).unwrap_err();
    assert!(!error.to_string().is_empty());
}

#[test]
fn preference_toggles_cycle_and_survive_expansion() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_tool_conversation(dir.path());
    let mut model = ViewerModel::open(&path, ViewPrefs::default()).unwrap();

    let labels: Vec<&str> = (0..4)
        .map(|_| model.cycle_tool_display().status_label())
        .collect();
    assert_eq!(labels, ["trn", "all", "sum", "trn"]);
    assert!(model.toggle_thinking());
    assert!(!model.toggle_thinking());
    assert!(model.toggle_timing());
    assert!(model.export_options().show_tools);

    let id = first_clickable_id(&model.ledger(80));
    model.toggle_expanded(&id);
    model.toggle_timing();
    model.cycle_tool_display();
    model.cycle_tool_display();
    model.cycle_tool_display();
    // Back in truncated mode, the expansion is still applied.
    assert_eq!(model.prefs.tool_display, ToolDisplayMode::Truncated);
    assert!(ledger_text(&model.ledger(80)).contains("five"));
}

#[test]
fn message_text_session_id_and_plain_display_use_the_cache() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "abc-123.jsonl", &conversation_lines());
    let mut model = ViewerModel::open(&path, ViewPrefs::default()).unwrap();
    assert_eq!(model.session_id(), Some("abc-123"));

    assert_eq!(model.message_text(0).unwrap(), "Find the Bug please");
    assert_eq!(model.message_text(1).unwrap(), "Looking for the bug.");
    assert_eq!(model.message_text(3).unwrap(), "[notice] a bug note");
    assert_eq!(
        model.message_text(99).unwrap_err(),
        crate::export::MESSAGE_NOT_FOUND
    );

    model.cycle_tool_display();
    model.toggle_thinking();
    assert_eq!(
        model.message_text(1).unwrap(),
        "Looking for the bug.\n\nGrep: \"bug\" in ."
    );
    assert_eq!(
        model.message_text(4).unwrap(),
        "the bug is subtle\n\nFixed the bug."
    );
    // Same as re-reading the file.
    for entry_index in 0..5 {
        assert_eq!(
            model.message_text(entry_index),
            crate::export::extract_message_text(&path, entry_index, model.export_options())
        );
    }
    assert_eq!(
        model.plain_display(),
        crate::export::plain_display(&path, model.export_options()).unwrap()
    );
}

// -----------------------------------------------------------------
// Ledger helpers
// -----------------------------------------------------------------

fn ranges(spec: &[(usize, usize, usize)]) -> Vec<MessageRange> {
    spec.iter()
        .map(|&(entry_index, start_line, end_line)| MessageRange {
            entry_index,
            start_line,
            end_line,
        })
        .collect()
}

#[test]
fn ledger_search_matches_lines_case_insensitively_including_labels() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "c.jsonl", &conversation_lines());
    let model = ViewerModel::open(&path, ViewPrefs::default()).unwrap();
    let rendered = model.ledger(80);

    let bug = search_lines(&rendered.lines, "BUG");
    assert!(!bug.is_empty());
    for &line in &bug {
        assert!(line_matches_query(&rendered.lines[line], "bug"));
    }
    // Upstream quirk: the label column is searchable too.
    let claude = search_lines(&rendered.lines, "claude");
    assert!(claude.iter().all(|&line| {
        rendered.lines[line]
            .spans
            .iter()
            .any(|(text, _)| text.trim() == "Claude")
    }));
    assert!(!claude.is_empty());
    assert!(search_lines(&rendered.lines, "").is_empty());
}

#[test]
fn message_lookup_viewport_and_visibility_helpers() {
    let ranges = ranges(&[(0, 0, 3), (2, 4, 6), (5, 7, 12)]);
    assert_eq!(message_idx_at_line(&ranges, 0), Some(0));
    assert_eq!(message_idx_at_line(&ranges, 3), None);
    assert_eq!(message_idx_at_line(&ranges, 5), Some(1));
    assert_eq!(message_idx_at_line(&ranges, 11), Some(2));
    assert_eq!(message_idx_at_line(&ranges, 12), None);

    assert_eq!(first_message_in_viewport(&ranges, 0, 5), Some(0));
    assert_eq!(first_message_in_viewport(&ranges, 3, 1), None);
    assert_eq!(first_message_in_viewport(&ranges, 3, 2), Some(1));
    assert_eq!(first_message_in_viewport(&[], 0, 5), None);

    // Already visible: unchanged; otherwise scroll to its start, clamped.
    assert_eq!(scroll_to_show(&ranges[1], 2, 5, 13), 2);
    assert_eq!(scroll_to_show(&ranges[2], 0, 5, 13), 7);
    assert_eq!(scroll_to_show(&ranges[2], 0, 5, 9), 4);
    assert_eq!(scroll_to_show(&ranges[0], 5, 5, 13), 0);
}

#[test]
fn anchor_prefers_focus_then_first_message_after_scroll() {
    let ranges = ranges(&[(0, 0, 3), (2, 4, 6), (5, 7, 12)]);
    assert_eq!(
        capture_anchor(&ranges, 5, Some(2), true),
        Some(ScrollAnchor {
            entry_index: 5,
            relative_row: 2
        })
    );
    // Navigation inactive: the first message starting at or after the
    // scroll offset, else the last one.
    assert_eq!(
        capture_anchor(&ranges, 1, Some(0), false),
        Some(ScrollAnchor {
            entry_index: 2,
            relative_row: 3
        })
    );
    assert_eq!(
        capture_anchor(&ranges, 9, None, false),
        Some(ScrollAnchor {
            entry_index: 5,
            relative_row: -2
        })
    );
    assert_eq!(capture_anchor(&[], 0, None, false), None);

    assert_eq!(find_message_idx_or_prev(&ranges, 2), Some(1));
    assert_eq!(find_message_idx_or_prev(&ranges, 3), Some(1));
    assert_eq!(find_message_idx_or_prev(&ranges, 99), Some(2));
    assert_eq!(find_message_idx_or_prev(&ranges[1..], 0), Some(0));
    assert_eq!(find_message_idx_or_prev(&[], 0), None);
}

#[test]
fn resolve_scroll_keeps_the_anchor_row_or_aligns_the_fallback() {
    let after = ranges(&[(0, 0, 6), (2, 8, 10), (5, 11, 20)]);
    // Same entry: keep its distance from the top.
    let anchor = ScrollAnchor {
        entry_index: 2,
        relative_row: 3,
    };
    assert_eq!(resolve_scroll(&after, Some(anchor), 0, 5, 21), (Some(1), 5));
    // Partly scrolled above the top (negative row) stays that way.
    let anchor = ScrollAnchor {
        entry_index: 5,
        relative_row: -2,
    };
    assert_eq!(
        resolve_scroll(&after, Some(anchor), 0, 5, 21),
        (Some(2), 13)
    );
    // The anchor message disappeared: the previous message, aligned to the
    // top (a positive row is dropped).
    let anchor = ScrollAnchor {
        entry_index: 3,
        relative_row: 4,
    };
    assert_eq!(resolve_scroll(&after, Some(anchor), 0, 5, 21), (Some(1), 8));
    // Clamped to the last page.
    let anchor = ScrollAnchor {
        entry_index: 5,
        relative_row: 0,
    };
    assert_eq!(
        resolve_scroll(&after, Some(anchor), 0, 15, 21),
        (Some(2), 6)
    );
    // No anchor: old offset clamped, focus on the first message.
    assert_eq!(resolve_scroll(&after, None, 30, 5, 21), (Some(0), 16));
    assert_eq!(resolve_scroll(&[], None, 3, 5, 2), (None, 0));
}

#[test]
fn rerender_after_toggle_keeps_the_focused_message() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "c.jsonl", &conversation_lines());
    let mut model = ViewerModel::open(&path, ViewPrefs::default()).unwrap();
    let before = model.ledger(80);
    let mut nav = MessageNav::new(before.messages.len());
    nav.focus(2);
    let scroll = before.messages[2].start_line;
    let anchor = capture_anchor(&before.messages, scroll, nav.focused(), nav.is_active());

    // Tool details add lines above the focused message.
    model.cycle_tool_display();
    let after = model.ledger(80);
    let (focused, new_scroll) =
        resolve_scroll(&after.messages, anchor, scroll, 1, after.lines.len());
    nav.refocus(focused);
    let focused = nav.focused().unwrap();
    assert_eq!(
        after.messages[focused].entry_index,
        before.messages[2].entry_index
    );
    assert_eq!(new_scroll, after.messages[focused].start_line);
    assert!(new_scroll > scroll);
}

// -----------------------------------------------------------------
// Document anchors
// -----------------------------------------------------------------

#[test]
fn document_anchor_capture_and_resolution() {
    let nav_entries = [0, 2, 5, 9];
    assert_eq!(
        capture_anchor_entry(&nav_entries, Some(2), true, Some(0)),
        Some(5)
    );
    assert_eq!(
        capture_anchor_entry(&nav_entries, Some(2), false, Some(1)),
        Some(2)
    );
    assert_eq!(
        capture_anchor_entry(&nav_entries, None, false, None),
        Some(9)
    );
    assert_eq!(capture_anchor_entry(&[], Some(0), true, Some(0)), None);

    assert_eq!(find_entry_or_prev(&nav_entries, 5), Some(2));
    assert_eq!(find_entry_or_prev(&nav_entries, 6), Some(2));
    assert_eq!(find_entry_or_prev(&[3, 4], 1), Some(0));
    assert_eq!(find_entry_or_prev(&[], 1), None);
}

#[test]
fn document_anchor_survives_tool_mode_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "c.jsonl", &conversation_lines());
    let mut model = ViewerModel::open(&path, ViewPrefs::default()).unwrap();

    let doc = model.document();
    let entries = doc.navigable_entry_indices();
    assert_eq!(entries, [0, 1, 4]);
    let anchor = capture_anchor_entry(&entries, Some(2), true, None).unwrap();
    assert_eq!(anchor, 4);

    model.cycle_tool_display();
    let doc = model.document();
    let entries = doc.navigable_entry_indices();
    // The tool-result user entry is now its own message.
    assert_eq!(entries, [0, 1, 2, 4]);
    let position = find_entry_or_prev(&entries, anchor).unwrap();
    assert_eq!(
        doc.messages[doc.message_at_nav_position(position).unwrap()].entry_index,
        4
    );
}

// -----------------------------------------------------------------
// Message navigation
// -----------------------------------------------------------------

#[test]
fn first_next_press_syncs_to_the_viewport_then_moves() {
    let mut nav = MessageNav::new(4);
    assert_eq!(nav.focused(), Some(0));
    assert!(!nav.is_active());
    assert_eq!(nav.highlighted(), None);

    // The first visible message is 1: the first press focuses 2.
    assert_eq!(nav.focus_next(4, Some(1)), Some(2));
    assert!(nav.is_active());
    assert_eq!(nav.highlighted(), Some(2));
    // Later presses ignore the viewport.
    assert_eq!(nav.focus_next(4, Some(0)), Some(3));
    assert_eq!(nav.focus_next(4, Some(0)), Some(3));

    assert!(nav.deactivate());
    assert!(!nav.deactivate());
    // Without a visible message the previous focus is kept.
    assert_eq!(nav.focus_prev(4, None), Some(2));
    assert_eq!(nav.focus_prev(4, None), Some(1));
    assert_eq!(nav.focus_prev(4, None), Some(0));
    assert_eq!(nav.focus_prev(4, None), Some(0));
}

#[test]
fn navigation_edge_cases() {
    let mut empty = MessageNav::new(0);
    assert_eq!(empty.focused(), None);
    assert_eq!(empty.focus_next(0, Some(0)), None);
    assert!(!empty.is_active());

    let mut nav = MessageNav::default();
    assert_eq!(nav.focus_next(3, None), Some(0));

    let mut nav = MessageNav::new(3);
    assert!(nav.focus(1));
    assert!(!nav.focus(1));
    assert!(nav.focus(2));
    nav.sync_after_scroll(Some(0));
    assert_eq!(nav.focused(), Some(0));
    nav.deactivate();
    nav.sync_after_scroll(Some(2));
    assert_eq!(nav.focused(), Some(0));

    // `y` activates and syncs, then copies the focused message.
    assert_eq!(nav.copy_target(Some(1)), Some(1));
    assert!(nav.is_active());
    assert_eq!(nav.copy_target(Some(2)), Some(1));

    nav.deactivate();
    nav.refocus(Some(2));
    assert_eq!(nav.focused(), Some(2));
    assert!(!nav.is_active());
}

// -----------------------------------------------------------------
// Search state and Esc order
// -----------------------------------------------------------------

fn finder(lines: &'static [&'static str]) -> impl Fn(&str) -> Vec<usize> {
    move |query: &str| {
        let query = query.to_lowercase();
        lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.to_lowercase().contains(&query))
            .map(|(i, _)| i)
            .collect()
    }
}

const LINES: &[&str] = &["Alpha", "beta", "ALPHABET", "gamma"];

#[test]
fn typing_updates_matches_and_jumps_to_the_first() {
    let find = finder(LINES);
    let mut search: ViewSearch<usize> = ViewSearch::new();
    search.start();
    assert_eq!(search.mode(), ViewSearchMode::Typing);
    assert_eq!(search.input_status(), "");

    assert_eq!(search.push_char('a', &find), Some(0));
    assert_eq!(search.count(), 4);
    assert_eq!(search.push_char('l', &find), Some(0));
    assert_eq!(search.matches(), [0, 2]);
    assert_eq!(search.input_status(), " (1/2)");
    assert_eq!(search.push_char('x', &find), None);
    assert_eq!(search.input_status(), " (no matches)");
    assert_eq!(search.backspace(&find), Some(0));
    assert_eq!(search.query(), "al");

    // n/N only work in an active search.
    assert_eq!(search.next_match(), None);
    search.commit();
    assert_eq!(search.mode(), ViewSearchMode::Active);
    assert_eq!(search.match_counter().as_deref(), Some("1/2"));
    assert_eq!(search.next_match(), Some(2));
    assert_eq!(search.next_match(), Some(0));
    assert_eq!(search.prev_match(), Some(2));
    assert_eq!(search.current(), Some(&2));
    assert_eq!(search.match_counter().as_deref(), Some("2/2"));
}

#[test]
fn commit_without_matches_turns_search_off() {
    let find = finder(LINES);
    let mut search: ViewSearch<usize> = ViewSearch::new();
    search.start();
    search.set_query("zzz", &find);
    search.commit();
    assert_eq!(search.mode(), ViewSearchMode::Off);
    assert_eq!(search.match_counter(), None);
}

#[test]
fn query_editing_shortcuts() {
    let find = finder(LINES);
    let mut search: ViewSearch<usize> = ViewSearch::new();
    search.start();
    search.set_query("alpha  beta  ", &find);
    search.delete_word_backwards(&find);
    assert_eq!(search.query(), "alpha  ");
    search.delete_word_backwards(&find);
    assert_eq!(search.query(), "");
    assert!(!search.clear_query(&find));

    // A multi-byte whitespace separator is cut as a whole character.
    search.set_query("alpha\u{3000}beta", &find);
    search.delete_word_backwards(&find);
    assert_eq!(search.query(), "alpha\u{3000}");

    assert_eq!(search.set_query("beta", &find), Some(1));
    assert!(search.clear_query(&find));
    assert_eq!(search.query(), "");
    assert!(search.matches().is_empty());
    assert_eq!(search.mode(), ViewSearchMode::Typing);
}

#[test]
fn refresh_recomputes_only_active_searches() {
    let mut search: ViewSearch<usize> = ViewSearch::new();
    search.start();
    search.set_query("a", |_| vec![1, 2, 3]);
    search.commit();
    search.next_match();
    search.next_match();
    assert_eq!(search.current_index(), 2);
    // Fewer matches after the re-render: the current one is clamped.
    search.refresh(|_| vec![4, 5]);
    assert_eq!(search.current_index(), 1);
    assert_eq!(search.matches(), [4, 5]);
    search.refresh(|_| Vec::new());
    assert_eq!(search.current_index(), 0);

    let mut typing: ViewSearch<usize> = ViewSearch::new();
    typing.start();
    typing.set_query("a", |_| vec![1]);
    typing.refresh(|_| panic!("typing searches are not refreshed"));
    assert_eq!(typing.matches(), [1]);
}

#[test]
fn escape_unwinds_typing_then_navigation_then_search_then_leaves() {
    let find = finder(LINES);
    let mut nav = MessageNav::new(3);
    let mut search: ViewSearch<usize> = ViewSearch::new();

    search.start();
    search.set_query("al", &find);
    nav.focus(1);
    assert_eq!(escape(&mut nav, &mut search), ViewEscape::ClearedSearch);
    assert_eq!(search.mode(), ViewSearchMode::Off);
    assert!(nav.is_active());

    search.start();
    search.set_query("al", &find);
    search.commit();
    assert_eq!(escape(&mut nav, &mut search), ViewEscape::ExitedNav);
    assert_eq!(search.mode(), ViewSearchMode::Active);
    assert_eq!(escape(&mut nav, &mut search), ViewEscape::ClearedSearch);
    assert_eq!(search.query(), "");
    assert_eq!(escape(&mut nav, &mut search), ViewEscape::Leave);
}

// -----------------------------------------------------------------
// Document search
// -----------------------------------------------------------------

#[test]
fn document_search_reports_block_byte_ranges() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "c.jsonl", &conversation_lines());
    let model = ViewerModel::open(&path, truncated()).unwrap();
    let doc = model.document();
    let index = DocSearchIndex::new(&doc);

    let matches = index.find("BUG");
    let located: Vec<(usize, usize, &str)> = matches
        .iter()
        .map(|m| {
            let text = index.text(m.message, m.block, m.item).unwrap();
            (m.message, m.block, &text[m.range.clone()])
        })
        .collect();
    // Case-insensitive, every occurrence, in document order; labels are not
    // searched, and the collapsed result only shows its first 4 lines
    // ("l5 bug" stays hidden).
    assert_eq!(
        located,
        [
            (0, 0, "Bug"),
            (1, 0, "bug"),
            (1, 1, "bug"),
            (2, 0, "bug"),
            (2, 0, "BUG"),
            (3, 0, "bug"),
            (4, 0, "bug"),
        ]
    );
    assert!(index.find("").is_empty());
    assert!(index.find("claude").is_empty());

    // Expanding the result makes its last line searchable.
    let mut expanded = model.clone();
    let id = doc.messages[2].blocks[0].toggle_id().unwrap().clone();
    expanded.toggle_expanded(&id);
    let index = DocSearchIndex::new(&expanded.document());
    assert_eq!(index.find("l5 BUG").len(), 1);
}

#[test]
fn document_search_covers_expanded_summary_items() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "c.jsonl", &conversation_lines()[..3]);
    let mut model = ViewerModel::open(&path, ViewPrefs::default()).unwrap();
    // Summary mode: "Looking for the bug." + sentence, then nothing else.
    let index = DocSearchIndex::new(&model.document());
    assert!(index.find("src/other").is_empty());

    let path = write(
        dir.path(),
        "grouped.jsonl",
        &[
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"Main.rs"}}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"fn main"}]}}"#,
        ],
    );
    model = ViewerModel::open(&path, ViewPrefs::default()).unwrap();
    let id = model.document().messages[0].blocks[0]
        .toggle_id()
        .unwrap()
        .clone();
    model.toggle_expanded(&id);
    let index = DocSearchIndex::new(&model.document());
    let matches = index.find("MAIN");
    assert_eq!(matches.len(), 2);
    assert_eq!((matches[0].block, matches[0].item), (0, Some(0)));
    assert_eq!((matches[1].block, matches[1].item), (0, Some(1)));
    assert_eq!(index.text(0, 0, Some(1)), Some("fn main"));
    assert_eq!(
        &index.text(0, 0, Some(0)).unwrap()[matches[0].range.clone()],
        "Main"
    );
}

#[test]
fn document_search_folds_greek_final_sigma_in_both_directions() {
    // Per-character lowercasing turns 'Σ' into 'σ' but leaves the
    // lowercase word-final 'ς' alone, so "οδος" missed "ΟΔΟΣ" and back.
    let target = SearchTarget::new(0, 0, None, "ΟΔΟΣ οδος".to_string());
    // ς and σ have the same UTF-8 length: no byte map is needed.
    assert!(target.origin.is_none());
    let index = DocSearchIndex {
        targets: vec![target],
    };
    let text = index.text(0, 0, None).unwrap();
    for query in ["οδος", "ΟΔΟΣ", "οδοσ", "Οδος"] {
        let found: Vec<&str> = index
            .find(query)
            .into_iter()
            .map(|m| &text[m.range])
            .collect();
        assert_eq!(found, ["ΟΔΟΣ", "οδος"], "{query}");
    }
}

#[test]
fn document_search_maps_case_folding_back_to_original_bytes() {
    // 'İ' lowercases to two characters and the Kelvin sign to a 1-byte 'k'.
    let target = SearchTarget::new(0, 0, None, "xİy \u{212A}elvin Straße".to_string());
    assert!(target.origin.is_some());
    let index = DocSearchIndex {
        targets: vec![target],
    };
    let text = index.text(0, 0, None).unwrap();
    let found: Vec<&str> = ["i", "kelvin", "STRASSE", "straße", "y"]
        .iter()
        .flat_map(|query| index.find(query))
        .map(|m| &text[m.range])
        .collect();
    // "i" matches the first half of 'İ' (the whole character is reported)
    // and the 'i' of "Kelvin"; "STRASSE" does not fold to "straße".
    assert_eq!(found, ["İ", "i", "\u{212A}elvin", "Straße", "y"]);

    // Plain text (identity folding) needs no byte map.
    let plain = SearchTarget::new(0, 0, None, "Приветствие ABC".to_string());
    assert!(plain.origin.is_none());
    let index = DocSearchIndex {
        targets: vec![plain],
    };
    let text = index.text(0, 0, None).unwrap();
    let m = &index.find("ПРИВЕТ")[0];
    assert_eq!(&text[m.range.clone()], "Привет");
}

// --- K5: agent message ordinals ---

/// A transcript exercising every placement rule: a file-history snapshot,
/// a Warmup exchange, a streamed assistant message (two records with one
/// id), a tool result, an agent_progress record, a `/clear` command, a
/// summary and an assistant message without id.
fn ordinal_lines() -> Vec<&'static str> {
    vec![
        r#"{"type":"file-history-snapshot","messageId":"x","snapshot":{"messageId":"x","trackedFileBackups":{},"timestamp":"2024-01-01T00:00:00Z"},"isSnapshotUpdate":false}"#,
        r#"{"type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Warmup"}}"#,
        r#"{"type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":"Ready."}]}}"#,
        r#"{"type":"user","timestamp":"2024-01-01T00:00:02Z","message":{"role":"user","content":"real question"}}"#,
        r#"{"type":"assistant","timestamp":"2024-01-01T00:00:03Z","message":{"id":"msg_1","role":"assistant","content":[{"type":"text","text":"partial"}]}}"#,
        r#"{"type":"assistant","timestamp":"2024-01-01T00:00:04Z","message":{"id":"msg_1","role":"assistant","content":[{"type":"text","text":"partial answer"},{"type":"tool_use","id":"toolu_1","name":"Grep","input":{"pattern":"x"}}]}}"#,
        r#"{"type":"user","timestamp":"2024-01-01T00:00:05Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"found"}]}}"#,
        r#"{"type":"progress","data":{"type":"agent_progress","agentId":"agent-1","message":{"type":"user","message":{"role":"user","content":[{"type":"text","text":"sub task"}]}}}}"#,
        r#"{"type":"user","timestamp":"2024-01-01T00:00:06Z","message":{"role":"user","content":"<command-name>/clear</command-name>"}}"#,
        r#"{"type":"summary","summary":"A summary"}"#,
        r#"{"type":"assistant","timestamp":"2024-01-01T00:00:07Z","message":{"role":"assistant","content":[{"type":"text","text":"final"}]}}"#,
    ]
}

#[test]
fn agent_ordinals_match_the_agent_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "ordinals.jsonl", &ordinal_lines());
    let viewer = ViewerModel::open(&path, truncated()).unwrap();
    let transcript = crate::agent::transcript::AgentTranscript::load(&path).unwrap();
    assert_eq!(transcript.messages.len(), 5);

    for message in &transcript.messages {
        // Every line is a valid entry, so entry index = JSONL line - 1.
        let entry = message.jsonl_line - 1;
        assert_eq!(
            viewer.agent_ordinal_for_entry(entry),
            Some(message.ordinal),
            "m{}",
            message.ordinal
        );
    }
    // The streamed message: the transcript keeps its last record (line 6),
    // the viewer maps the ordinal to the first one (entry 4); both records
    // carry the ordinal.
    assert_eq!(transcript.messages[1].jsonl_line, 6);
    assert_eq!(viewer.entry_for_agent_ordinal(2), Some(4));
    assert_eq!(viewer.agent_ordinal_for_entry(4), Some(2));
    assert_eq!(viewer.agent_ordinal_for_entry(5), Some(2));
    for (ordinal, entry) in [(1, 3), (3, 6), (4, 7), (5, 10)] {
        assert_eq!(viewer.entry_for_agent_ordinal(ordinal), Some(entry));
    }
    // Snapshot, Warmup exchange, /clear and summary are not messages.
    for entry in [0, 1, 2, 8, 9, 42] {
        assert_eq!(viewer.agent_ordinal_for_entry(entry), None, "entry {entry}");
    }
    assert_eq!(viewer.entry_for_agent_ordinal(0), None);
    assert_eq!(viewer.entry_for_agent_ordinal(6), None);
}

#[test]
fn agent_ordinals_match_the_agent_transcript_for_pi_and_omp_fixtures() {
    for fixture in [
        "tests/fixtures/pi/v1.jsonl",
        "tests/fixtures/pi/v2.jsonl",
        "tests/fixtures/pi/v3-branched.jsonl",
        "tests/fixtures/omp/v3.jsonl",
    ] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(fixture);
        let viewer = ViewerModel::open(&path, truncated()).unwrap();
        let transcript = crate::agent::transcript::AgentTranscript::load(&path).unwrap();
        assert!(!transcript.messages.is_empty(), "{fixture}");
        for message in &transcript.messages {
            // The transcript numbers the lines of the normalized projection.
            let entry = message.jsonl_line - 1;
            assert_eq!(
                viewer.agent_ordinal_for_entry(entry),
                Some(message.ordinal),
                "{fixture} m{}",
                message.ordinal
            );
            assert_eq!(
                viewer.entry_for_agent_ordinal(message.ordinal),
                Some(entry),
                "{fixture} m{}",
                message.ordinal
            );
        }
    }
}
