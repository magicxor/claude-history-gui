//! Viewer tests (P and G, DESIGN §17.4). WP2.
//!
//! P: segments (every markdown rule of §9.3 incl. tabs and table rows),
//! items (label table, lanes, ids stable across rebuilds, chunking,
//! visual-row truncation with K9), the header builder (omission rules),
//! search over items incl. hidden text, the splice diff.
//! G: open/install/stale generations, toggles with the anchor kept, width
//! changes, expansion clicks, navigation, search, the Esc order, `q`,
//! SmartCopy and partial copies, the link dialog, single-file windows,
//! deleted-file toggles. Row ids in test names are FEATURES.md ids.

mod pure {
    use std::sync::Arc;

    use claude_history_core::theme::ThemeKind;
    use claude_history_core::view::{BodyLineKind, LabelRole, ToolBodyLine, ToolDisplayMode};

    use crate::test_support::fixtures_viewer::*;
    use crate::viewer::code;
    use crate::viewer::header;
    use crate::viewer::items::{self, ItemId, ItemKey, ItemKind, LaneRole, Splice, TextKind};
    use crate::viewer::nav;
    use crate::viewer::render;
    use crate::viewer::search::SearchIndex;
    use crate::viewer::segments::{self, SegKind, Segment, Style};
    use crate::viewer::status;

    fn kinds(segs: &[Segment]) -> Vec<SegKind> {
        segs.iter().map(|s| s.kind.clone()).collect()
    }

    fn texts(segs: &[Segment]) -> Vec<&str> {
        segs.iter().map(|s| s.text.as_str()).collect()
    }

    /// The flags of the bytes of `needle` in `seg`.
    fn style_of(seg: &Segment, needle: &str) -> Style {
        let at = seg.text.find(needle).expect("needle in text");
        seg.style_at(at)
    }

    // ----- segments (§9.3) -------------------------------------------------------

    #[test]
    fn v033_extensions_beyond_tables_and_strikethrough_stay_literal() {
        let segs = segments::markdown("A note[^1] here.\n\n- [ ] open task");
        assert_eq!(segs[0].text, "A note[^1] here.");
        assert_eq!(segs[1].text, "[ ] open task");
        assert!(matches!(segs[1].kind, SegKind::Item { .. }));
    }

    #[test]
    fn v034_soft_and_hard_breaks_are_line_breaks() {
        let segs = segments::markdown("a\nb");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].kind, SegKind::Para);
        assert_eq!(segs[0].text, "a\nb");
        let segs = segments::markdown("a  \nb");
        assert_eq!(segs[0].text, "a\nb");
    }

    #[test]
    fn v035_block_spacing_follows_the_rule_list() {
        let source =
            "# H\n\npara\n\n- a\n- b\n\n```\ncode\n```\n\n> q\n\n---\n\n|x|y|\n|-|-|\n|1|2|";
        let segs = segments::markdown(source);
        let summary: Vec<(String, bool)> = segs
            .iter()
            .map(|s| {
                let kind = match &s.kind {
                    SegKind::Heading(_) => "heading",
                    SegKind::Para => "para",
                    SegKind::Item { .. } => "item",
                    SegKind::Code => "code",
                    SegKind::Quote => "quote",
                    SegKind::Rule => "rule",
                    SegKind::TableRow { .. } => "row",
                    _ => "other",
                };
                (kind.to_string(), s.gap_before)
            })
            .collect();
        let expected = [
            ("heading", false),
            ("para", true),
            ("item", true),
            ("item", false),
            ("code", true),
            ("quote", true),
            ("rule", true),
            ("row", true),
            ("row", false),
        ];
        assert_eq!(
            summary,
            expected
                .iter()
                .map(|(k, g)| (k.to_string(), *g))
                .collect::<Vec<_>>()
        );
        // No gap before the first paragraph of a list item, none inside a quote.
        let segs = segments::markdown("- item\n\n  second para\n\n> one\n>\n> two");
        assert!(!segs[0].gap_before);
        assert_eq!(segs[2].kind, SegKind::Quote);
        assert_eq!(segs[2].text, "> one\n> two");
    }

    #[test]
    fn v036_headings_hide_the_marker_and_are_heading_bold() {
        let segs = segments::markdown("## Title");
        assert_eq!(segs[0].kind, SegKind::Heading(2));
        assert_eq!(segs[0].text, "Title");
        let style = segs[0].style_at(0);
        assert!(style.contains(Style::HEADING) && style.contains(Style::BOLD));
    }

    #[test]
    fn v037_code_blocks_hide_fences_keep_blank_lines_and_highlight() {
        let segs = segments::markdown("```rust\nfn main() {\n\n    let x = 1;\n}\n```");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].kind, SegKind::Code);
        assert_eq!(segs[0].code_lang.as_deref(), Some("rust"));
        assert_eq!(segs[0].text, "fn main() {\n\n    let x = 1;\n}");
        let tokens = code::highlight(&segs[0].text, "rust", ThemeKind::Dark);
        let colors: std::collections::HashSet<_> = tokens
            .iter()
            .filter_map(|(_, style)| style.color.map(|c| format!("{c:?}")))
            .collect();
        assert!(colors.len() >= 2, "{colors:?}");
        assert!(code::highlight("anything", "no-such-language", ThemeKind::Dark).is_empty());
        // Light and dark palettes differ (theme-keyed cache).
        let light = code::highlight(&segs[0].text, "rust", ThemeKind::Light);
        assert_ne!(*tokens, *light);
    }

    #[test]
    fn v038_lists_use_dash_and_ordered_bullets_with_depth() {
        let segs = segments::markdown("3. a\n4. b");
        match (&segs[0].kind, &segs[1].kind) {
            (
                SegKind::Item {
                    bullet: first,
                    ordered: true,
                    depth: 0,
                },
                SegKind::Item {
                    bullet: second,
                    ordered: true,
                    ..
                },
            ) => {
                assert_eq!(first, "3. ");
                assert_eq!(second, "4. ");
            }
            other => panic!("{other:?}"),
        }
        let segs = segments::markdown("- a\n  - b");
        assert!(
            matches!(&segs[0].kind, SegKind::Item { depth: 0, bullet, ordered: false } if bullet == "- ")
        );
        assert!(matches!(&segs[1].kind, SegKind::Item { depth: 1, .. }));
        assert_eq!(texts(&segs), ["a", "b"]);
    }

    #[test]
    fn v039_emphasis_strong_strikethrough() {
        let segs = segments::markdown("*a* **b** ~~c~~");
        let seg = &segs[0];
        assert!(style_of(seg, "a").contains(Style::ITALIC));
        assert!(style_of(seg, "b").contains(Style::BOLD));
        assert!(style_of(seg, "c").contains(Style::DIMMED));
    }

    #[test]
    fn v040_quotes_have_a_dimmed_prefix_and_quote_style() {
        let segs = segments::markdown("> q");
        assert_eq!(segs[0].kind, SegKind::Quote);
        assert_eq!(segs[0].text, "> q");
        let prefix = segs[0].style_at(0);
        assert!(prefix.contains(Style::DIMMED) && prefix.contains(Style::QUOTE));
        assert!(style_of(&segs[0], "q").contains(Style::QUOTE));
    }

    #[test]
    fn v042_inline_code_gets_code_style() {
        let segs = segments::markdown("use `x` here");
        assert_eq!(segs[0].text, "use x here");
        assert!(style_of(&segs[0], "x").contains(Style::CODE));
    }

    #[test]
    fn v029_v043_html_is_literal_text() {
        assert_eq!(
            segments::markdown("<system-reminder>x</system-reminder>")[0].text,
            "<system-reminder>x</system-reminder>"
        );
        assert_eq!(segments::markdown("a <b>x</b> c")[0].text, "a <b>x</b> c");
        assert_eq!(segments::markdown("<b>x</b>")[0].text, "<b>x</b>");
    }

    #[test]
    fn v041_links_keep_their_url_aside() {
        let segs = segments::markdown("See [the RFC](https://example.com/rfc) now.");
        assert_eq!(segs[0].text, "See the RFC now.");
        assert_eq!(segs[0].link_urls.len(), 1);
        let (range, url) = &segs[0].link_urls[0];
        assert_eq!(&segs[0].text[range.clone()], "the RFC");
        assert_eq!(url, "https://example.com/rfc");
        assert!(style_of(&segs[0], "the RFC").contains(Style::LINK));
        // Bare URLs are plain text (no autolinks).
        assert!(
            segments::markdown("https://example.com")[0]
                .link_urls
                .is_empty()
        );
    }

    #[test]
    fn v044_a_rule_is_one_rule_segment() {
        let segs = segments::markdown("---");
        assert_eq!(kinds(&segs), [SegKind::Rule]);
    }

    #[test]
    fn v045_tables_are_rows_with_shared_widths_and_no_bold() {
        let segs = segments::markdown("| Name | Value |\n|---|---|\n| alpha | 1 |\n| b | 22222 |");
        assert_eq!(segs.len(), 3);
        let mut widths = None;
        for seg in &segs {
            let SegKind::TableRow {
                widths: row_widths,
                cells,
                ..
            } = &seg.kind
            else {
                panic!("{:?}", seg.kind)
            };
            assert_eq!(cells.len(), 2);
            match &widths {
                None => widths = Some(row_widths.clone()),
                Some(w) => assert!(Arc::ptr_eq(w, row_widths)),
            }
            assert!(seg.spans.iter().all(|(_, s)| !s.contains(Style::BOLD)));
        }
        assert_eq!(**widths.as_ref().unwrap(), vec![5, 5]);
        assert_eq!(segs[0].text, "Name   Value");
        assert_eq!(segs[1].text, "alpha  1");
        assert!(matches!(segs[2].kind, SegKind::TableRow { last: true, .. }));
    }

    #[test]
    fn v046_trailing_blank_lines_are_trimmed() {
        let segs = segments::markdown("text\n\n\n");
        assert_eq!(texts(&segs), ["text"]);
        assert_eq!(segments::code_block("a\n\n", "").text, "a\n");
    }

    #[test]
    fn v100_tabs_expand_to_8_column_stops_everywhere() {
        let expanded = "a       b";
        assert_eq!(segments::markdown("a\tb")[0].text, expanded);
        assert_eq!(segments::markdown("```\na\tb\n```")[0].text, expanded);
        assert_eq!(segments::code_block("a\tb", "text").text, expanded);
        assert_eq!(segments::plain("a\tb", false, None).text, expanded);
        let body = segments::tool_body(
            &[ToolBodyLine {
                kind: BodyLineKind::Plain,
                text: "a\tb".into(),
            }],
            None,
        );
        assert_eq!(body.text, expanded);
    }

    #[test]
    fn collapsed_lines_are_clipped_to_the_column_width() {
        assert_eq!(segments::clip_to_cols("abcdefghij", 5), "abcd…");
        assert_eq!(segments::clip_to_cols("abc", 5), "abc");
        let seg = segments::plain("0123456789\nab", true, Some(6));
        assert_eq!(seg.text, "01234…\nab");
        assert!(seg.style_at(0).contains(Style::DIMMED));
    }

    #[test]
    fn split_lines_rebases_spans_and_keeps_the_gap_on_the_first_piece() {
        let mut seg = segments::code_block(&numbered_lines(250), "text");
        seg.gap_before = true;
        seg.overlay(Style::DIMMED);
        let pieces = segments::split_lines(&seg, 100);
        assert_eq!(pieces.len(), 3);
        assert_eq!(pieces[0].line_count(), 100);
        assert_eq!(pieces[2].line_count(), 50);
        assert!(pieces[0].gap_before && !pieces[1].gap_before);
        assert!(pieces[1].text.starts_with("line 101"));
        assert!(
            pieces[1]
                .spans
                .iter()
                .all(|(r, _)| r.end <= pieces[1].text.len())
        );
        assert_eq!(
            pieces
                .iter()
                .map(|p| p.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            seg.text
        );
    }

    // ----- items (§9.2) ----------------------------------------------------------

    fn roles_fixture() -> Vec<String> {
        vec![
            user_at("hello", 0),
            skill_user("skill body"),
            thinking_and_text("pondering", "answer", 1),
            tool_call("t1", "Grep", r#"{"pattern":"x"}"#, 2),
            tool_result("t1", "grep result", 2),
            subagent_text("toolu_parent_abc", "sub text", 3),
            subagent_result("toolu_parent_abc", "s1", "sub result", 3),
        ]
    }

    #[test]
    fn v015_label_table_row_by_row() {
        let items = items_for(
            &roles_fixture(),
            prefs(ToolDisplayMode::Full, true, false),
            80,
        );
        let rows: Vec<(ItemKind, Option<String>, LaneRole, bool, bool)> = items
            .iter()
            .map(|item| {
                (
                    item.kind,
                    item.lanes.label.clone(),
                    item.lanes.role,
                    item.lanes.bold,
                    item.lanes.dimmed,
                )
            })
            .collect();
        let row = |kind: ItemKind, label: &str| {
            rows.iter()
                .find(|r| r.0 == kind && r.1.as_deref() == Some(label))
                .unwrap_or_else(|| panic!("{kind:?} {label}: {rows:?}"))
                .clone()
        };
        assert_eq!(
            row(ItemKind::Markdown, "You"),
            (
                ItemKind::Markdown,
                Some("You".into()),
                LaneRole::TextPrimary,
                true,
                false
            )
        );
        // The skill expansion: `You`, dimmed, not bold.
        assert!(
            rows.iter()
                .any(|r| r.0 == ItemKind::Markdown && r.1.as_deref() == Some("You") && !r.3 && r.4)
        );
        assert_eq!(
            row(ItemKind::Markdown, "Claude"),
            (
                ItemKind::Markdown,
                Some("Claude".into()),
                LaneRole::Accent,
                true,
                false
            )
        );
        assert_eq!(
            row(ItemKind::Thinking, "Thinking"),
            (
                ItemKind::Thinking,
                Some("Thinking".into()),
                LaneRole::AccentDim,
                false,
                false
            )
        );
        assert_eq!(row(ItemKind::ToolCall, "Claude").2, LaneRole::AccentDim);
        assert_eq!(
            row(ItemKind::ToolResult, "↳ Result"),
            (
                ItemKind::ToolResult,
                Some("↳ Result".into()),
                LaneRole::ToolText,
                false,
                false
            )
        );
        let sub = rows
            .iter()
            .find(|r| {
                r.0 == ItemKind::Markdown && r.1.as_deref().is_some_and(|l| l.starts_with('↳'))
            })
            .expect("subagent text");
        assert!(sub.4 && !sub.3, "subagent rows are dimmed: {sub:?}");
        assert_eq!(
            row(ItemKind::SubagentResult, "↳ Tool"),
            (
                ItemKind::SubagentResult,
                Some("↳ Tool".into()),
                LaneRole::AccentDim,
                false,
                true
            )
        );
        assert_eq!(
            LaneRole::from(LabelRole::TextSecondary),
            LaneRole::TextSecondary
        );
    }

    #[test]
    fn v013_v014_label_lane_is_nine_cells_and_fits_names() {
        assert_eq!(items::LABEL_LANE_CELLS, 9);
        let items = items_for(
            &[user("q"), pi_metadata("Branch summary", "summary text")],
            prefs(ToolDisplayMode::Hidden, false, false),
            80,
        );
        assert!(
            items
                .iter()
                .any(|item| item.lanes.label.as_deref() == Some("Branch s…")),
            "{:?}",
            items.iter().map(|i| &i.lanes.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn v022_pi_metadata_renders_dimmed_with_its_label() {
        let items = items_for(
            &[user("q"), pi_metadata("notice", "custom note")],
            prefs(ToolDisplayMode::Hidden, false, false),
            80,
        );
        let meta = items
            .iter()
            .find(|i| i.lanes.label.as_deref() == Some("notice"))
            .expect("metadata item");
        assert!(meta.lanes.dimmed);
        assert_eq!(meta.lanes.role, LaneRole::TextSecondary);
        assert!(
            meta.segments[0]
                .spans
                .iter()
                .all(|(_, s)| s.contains(Style::DIMMED))
        );
        assert_eq!(meta.nav_position, None, "Pi metadata is not navigable");
    }

    #[test]
    fn v016_dimmed_messages_override_every_span_and_get_no_tokens() {
        let items = items_for(
            &[
                user("q"),
                subagent_text(
                    "toolu_parent_abc",
                    "text with `code`\n\n```rust\nfn x() {}\n```",
                    1,
                ),
            ],
            prefs(ToolDisplayMode::Hidden, true, false),
            80,
        );
        let item = &items[1];
        assert!(item.lanes.dimmed);
        for seg in item.segments.iter() {
            let covered: usize = seg.spans.iter().map(|(r, _)| r.len()).sum();
            assert_eq!(covered, seg.text.len(), "every byte styled: {seg:?}");
            assert!(seg.spans.iter().all(|(_, s)| s.contains(Style::DIMMED)));
        }
        let code = item
            .segments
            .iter()
            .find(|s| s.kind == SegKind::Code)
            .expect("code segment");
        assert!(!render::wants_tokens(code));
        assert!(render::wants_tokens(&segments::code_block(
            "fn x() {}",
            "rust"
        )));
    }

    #[test]
    fn v031_timing_lane_stamps_the_first_block_only() {
        let lines = vec![
            user_at("question", 0),
            assistant_blocks(
                r#"[{"type":"text","text":"first"},{"type":"text","text":"second"}]"#,
                1,
            ),
            skill_user("dimmed"),
        ];
        let items = items_for(&lines, prefs(ToolDisplayMode::Hidden, false, true), 80);
        let stamps: Vec<Option<&str>> = items
            .iter()
            .map(|item| item.lanes.timestamp.as_deref())
            .collect();
        assert!(stamps[0].is_some(), "{stamps:?}");
        assert!(stamps[1].is_some(), "first assistant block: {stamps:?}");
        assert!(stamps[2].is_none(), "second text block loses the stamp");
        assert!(stamps[3].is_none(), "dimmed blocks are never stamped");
        // Without timing no stamp at all.
        let items = items_for(&lines, prefs(ToolDisplayMode::Hidden, false, false), 80);
        assert!(items.iter().all(|i| i.lanes.timestamp.is_none()));
    }

    #[test]
    fn v053_mixed_text_and_tools_summary_is_not_clickable() {
        let lines = vec![
            user("q"),
            assistant_blocks(
                r#"[{"type":"text","text":"Let me look."},{"type":"tool_use","id":"a","name":"Read","input":{"file_path":"x.rs"}}]"#,
                1,
            ),
            tool_result("a", "content", 1),
        ];
        let items = items_for(&lines, prefs(ToolDisplayMode::Hidden, false, false), 80);
        let summary = items
            .iter()
            .find(|i| i.kind == ItemKind::ToolSummary)
            .expect("summary item");
        assert_eq!(summary.toggle, None);
        assert!(matches!(
            summary.id.key,
            ItemKey::Text {
                kind: TextKind::Summary,
                ..
            }
        ));
        assert!(items::item_text(summary).contains("Read 1 file"));
    }

    #[test]
    fn v055_edit_call_body_lines_are_diff_colored() {
        let lines = vec![
            user("q"),
            tool_call(
                "e",
                "Edit",
                r#"{"file_path":"x.rs","old_string":"old line","new_string":"new line"}"#,
                1,
            ),
            tool_result("e", "ok", 1),
        ];
        let items = items_for(&lines, prefs(ToolDisplayMode::Full, false, false), 80);
        let call = items
            .iter()
            .find(|i| i.kind == ItemKind::ToolCall)
            .expect("call item");
        assert!(call.has_header);
        let body = &call.segments[1];
        assert_eq!(body.kind, SegKind::ToolBody);
        let flags: Vec<Style> = body.spans.iter().map(|(_, s)| *s).collect();
        assert!(flags.contains(&Style::DIFF_ADD), "{body:?}");
        assert!(flags.contains(&Style::DIFF_REMOVE), "{body:?}");
        assert!(call.segments[0].style_at(0).contains(Style::TOOL));
    }

    #[test]
    fn v056_truncated_call_body_shows_three_lines_plus_indicator() {
        let prompt = numbered_lines(10);
        let lines = vec![
            user("q"),
            tool_call(
                "t",
                "Task",
                &format!(
                    r#"{{"description":"d","prompt":{}}}"#,
                    serde_json::to_string(&prompt).unwrap()
                ),
                1,
            ),
            tool_result("t", "done", 1),
            tool_call(
                "s",
                "Task",
                r#"{"description":"short","prompt":"one\ntwo\nthree"}"#,
                2,
            ),
            tool_result("s", "done", 2),
        ];
        let items = items_for(&lines, prefs(ToolDisplayMode::Truncated, false, false), 80);
        let calls: Vec<_> = items
            .iter()
            .filter(|i| i.kind == ItemKind::ToolCall)
            .collect();
        assert_eq!(calls[0].segments[1].line_count(), 3);
        assert_eq!(calls[0].more, Some(7));
        assert_eq!(render::more_lines_text(7), "(7 more lines…)");
        assert!(calls[0].toggle.is_some());
        assert_eq!(calls[1].more, None, "3 lines: no indicator");
        assert!(calls[1].toggle.is_none());
        // Full mode: nothing to toggle.
        let items = items_for(&lines, prefs(ToolDisplayMode::Full, false, false), 80);
        assert!(items.iter().all(|i| i.toggle.is_none() && i.more.is_none()));
    }

    #[test]
    fn v057_long_single_line_result_is_truncated_by_visual_rows() {
        let big = "x".repeat(50_000);
        let lines = vec![
            user("q"),
            bash_call("b", "cat big", 1),
            tool_result("b", &big, 1),
        ];
        let at = |cols: usize| {
            let items = items_for(
                &lines,
                prefs(ToolDisplayMode::Truncated, false, false),
                cols,
            );
            items
                .into_iter()
                .find(|i| i.kind == ItemKind::ToolResult)
                .expect("result item")
        };
        let wide = at(120);
        let text = items::item_text(&wide);
        assert!(text.lines().count() <= 4, "{} rows", text.lines().count());
        assert!(text.lines().all(|l| l.chars().count() <= 120));
        let narrow = at(60);
        assert!(narrow.more.unwrap() > wide.more.unwrap());
        assert!(wide.cols_dependent);
    }

    #[test]
    fn v063_thinking_is_italic_and_subagent_thinking_has_no_item() {
        let lines = vec![
            user("q"),
            thinking_and_text("deep *thought*", "answer", 1),
            r#"{"type":"assistant","parent_tool_use_id":"p1","message":{"role":"assistant","content":[{"type":"thinking","thinking":"sub thinking","signature":""},{"type":"text","text":"sub answer"}]}}"#
                .to_string(),
        ];
        let items = items_for(&lines, prefs(ToolDisplayMode::Hidden, true, false), 80);
        let thinking: Vec<_> = items
            .iter()
            .filter(|i| i.kind == ItemKind::Thinking)
            .collect();
        assert_eq!(thinking.len(), 1, "subagent thinking is not shown");
        for seg in thinking[0].segments.iter() {
            assert!(
                seg.spans
                    .iter()
                    .all(|(_, s)| s.contains(Style::ITALIC) && s.contains(Style::THINKING))
            );
        }
    }

    #[test]
    fn v064_subagent_result_has_a_result_header_and_clipped_rows() {
        // A block array pretty-prints to several JSON lines (quirk 4 kept).
        let blocks: Vec<String> = (1..=4)
            .map(|i| format!(r#"{{"type":"text","text":"row {i} {}"}}"#, "z".repeat(100)))
            .collect();
        let lines = vec![
            user("q"),
            subagent_text("toolu_parent_abc", "sub", 1),
            format!(
                r#"{{"type":"user","parent_tool_use_id":"toolu_parent_abc","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"r","content":[{}]}}]}}}}"#,
                blocks.join(",")
            ),
        ];
        let items = items_for(&lines, prefs(ToolDisplayMode::Truncated, true, false), 40);
        let result = items
            .iter()
            .find(|i| i.kind == ItemKind::SubagentResult)
            .expect("subagent result");
        assert!(result.has_header);
        assert_eq!(result.segments[0].text, "<Result>");
        let body = &result.segments[1];
        assert!(body.line_count() <= 4);
        assert!(body.text.lines().all(|l| l.chars().count() <= 40));
        assert!(result.more.is_some());
    }

    #[test]
    fn v065_agent_progress_uses_the_short_agent_label_and_no_stamp() {
        let lines = vec![
            user("q"),
            agent_progress("agent-abc1234567", "working on it"),
        ];
        let items = items_for(&lines, prefs(ToolDisplayMode::Hidden, true, true), 80);
        let progress = items
            .iter()
            .find(|i| i.lanes.label.as_deref().is_some_and(|l| l.starts_with('↳')))
            .expect("agent progress item");
        // The 7-character short id fits the 9-cell lane.
        assert_eq!(progress.lanes.label.as_deref(), Some("↳agent-a"));
        assert!(progress.lanes.timestamp.is_none());
    }

    #[test]
    fn v071_item_positions_follow_navigable_messages() {
        let lines = roles_fixture();
        let (_dir, model) = model_for(&lines, prefs(ToolDisplayMode::Full, true, false));
        let doc = model.document();
        let (items, info) = items::build_with_info(&doc, &model, 80, &|| false).unwrap();
        let navigable = doc.navigable_indices();
        for item in &items {
            let expected = navigable.iter().position(|&m| m == item.message_index);
            assert_eq!(item.nav_position, expected);
        }
        assert_eq!(info.nav_count, navigable.len());
        assert_eq!(info.nav_entries, doc.navigable_entry_indices());
        for (position, first) in info.nav_first_item.iter().enumerate() {
            assert_eq!(items[*first].nav_position, Some(position));
            assert!(items[*first].first_of_message);
        }
    }

    #[test]
    fn v099_big_outputs_are_chunked_with_lanes_on_the_first_chunk() {
        let body: String = (1..=5000).map(|n| format!("output line {n}\n")).collect();
        let lines = vec![
            user("q"),
            bash_call("b", "seq 5000", 1),
            tool_result("b", &body, 1),
        ];
        let items = items_for(&lines, prefs(ToolDisplayMode::Full, false, false), 80);
        let chunks: Vec<_> = items
            .iter()
            .filter(|i| i.kind == ItemKind::ToolResult)
            .collect();
        assert_eq!(chunks.len(), 50);
        assert!(chunks[0].lanes.label.is_some());
        assert!(
            chunks[1..]
                .iter()
                .all(|c| c.lanes.label.is_none() && !c.first_chunk)
        );
        assert!(chunks.last().unwrap().last_chunk);
        let ids: std::collections::HashSet<_> = chunks.iter().map(|c| c.id.clone()).collect();
        assert_eq!(ids.len(), 50, "chunk ids are distinct");
        assert!(items::item_text(chunks[29]).contains("output line 2901"));
    }

    #[test]
    fn ids_are_stable_across_rebuilds() {
        let lines = roles_fixture();
        let (_dir, mut model) = model_for(&lines, prefs(ToolDisplayMode::Hidden, false, false));
        let before = items_of(&model, 80);
        model.prefs.show_timing = true;
        let after = items_of(&model, 80);
        let ids =
            |items: &[items::ViewerItem]| items.iter().map(|i| i.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&before), ids(&after));
        assert_ne!(before[0].version, after[0].version, "lanes changed");
        // The markdown items keep their ids when tools appear around them.
        model.prefs.tool_display = ToolDisplayMode::Full;
        let full = items_of(&model, 80);
        for item in before.iter().filter(|i| i.kind == ItemKind::Markdown) {
            assert!(full.iter().any(|f| f.id == item.id), "{:?}", item.id);
        }
    }

    // ----- header (§9.10) ----------------------------------------------------------

    fn header_info() -> crate::model::HeaderInfo {
        use chrono::TimeZone as _;
        crate::model::HeaderInfo {
            title: "Fix login bug".into(),
            project_name: Some("proj".into()),
            custom_title: None,
            model: None,
            message_count: 2,
            duration_minutes: None,
            total_tokens: 0,
            timestamp: chrono::Local
                .with_ymd_and_hms(2026, 10, 12, 14, 3, 0)
                .unwrap(),
            summary: None,
            parse_error_count: 0,
            file_stem: "3f2a".into(),
        }
    }

    #[test]
    fn v004_header_omission_rules_and_token_forms() {
        let mut info = header_info();
        let text = header::layout(Some(&info), "3f2a", 200).text();
        assert_eq!(text, "proj · 2 messages · 2026-10-12 14:03");
        info.message_count = 1;
        assert!(
            header::layout(Some(&info), "x", 200)
                .text()
                .contains("1 message ·")
        );
        info.total_tokens = 926_000;
        info.model = Some("claude-opus-4-5-20251101".into());
        info.duration_minutes = Some(65);
        info.custom_title = Some("My title".into());
        let wide = header::layout(Some(&info), "x", 200).text();
        assert_eq!(
            wide,
            "proj · My title · opus-4.5 · 1 message · 1h 5m · 926k tokens · 2026-10-12 14:03"
        );
        let narrow = header::layout(Some(&info), "x", 60).text();
        assert!(narrow.contains("· 926k ·"), "{narrow}");
        info.summary = Some("A long summary of everything".into());
        let layout = header::layout(Some(&info), "x", 200);
        assert!(layout.summary_inline);
        let layout = header::layout(Some(&info), "x", 80);
        assert!(!layout.summary_inline, "the summary moves to line 2");
        assert!(layout.text().contains("\nA long summary"));
    }

    #[test]
    fn v006_v008_stem_only_header_and_parse_error_badge() {
        assert_eq!(header::layout(None, "3f2a19c0", 100).text(), "3f2a19c0");
        let stem = crate::model::HeaderInfo::stem_only(std::path::Path::new("/x/abc.jsonl"));
        assert_eq!(header::layout(Some(&stem), "abc", 100).text(), "abc");
        assert_eq!(header::parse_error_badge(1), "⚠ 1");
        assert_eq!(header::parse_error_tooltip(1), "1 malformed line");
        assert_eq!(header::parse_error_tooltip(3), "3 malformed lines");
        let mut info = header_info();
        info.parse_error_count = 2;
        assert_eq!(header::layout(Some(&info), "x", 100).parse_errors, 2);
    }

    // ----- search, splice, status ------------------------------------------------

    #[test]
    fn search_counts_body_occurrences_and_hidden_lines() {
        let body = "needle one\n".to_string() + &numbered_lines(20) + "\nneedle two\nneedle three";
        let lines = vec![
            user("Claude, find the needle"),
            assistant("Claude found a NEEDLE and another needle."),
            bash_call("b", "grep needle", 1),
            tool_result("b", &body, 1),
        ];
        let (_dir, model) = model_for(&lines, prefs(ToolDisplayMode::Truncated, false, false));
        let doc = model.document();
        let items = items_of(&model, 80);
        let mut index = SearchIndex::build(&items);
        let matches = index.find(&items, &doc, "Claude");
        // Labels ("Claude") are not searched: only the two body occurrences.
        assert_eq!(matches.len(), 2, "{matches:?}");
        let matches = index.find(&items, &doc, "needle");
        let hidden = matches.iter().filter(|m| m.hidden.is_some()).count();
        // user 1 + assistant 2 + call header 1 + result: 1 visible + 2 hidden.
        assert_eq!(matches.len(), 7, "{matches:?}");
        assert_eq!(hidden, 2);
        assert!(index.find(&items, &doc, "").is_empty());
    }

    #[test]
    fn search_texts_fold_case_and_map_ranges() {
        let text = "Straße ΟΔΟΣ";
        let st = segments::search_text_of(text);
        let ranges = st.find(&segments::fold_query("οδος"), text);
        assert_eq!(ranges.len(), 1);
        assert_eq!(&text[ranges[0].clone()], "ΟΔΟΣ");
        let ascii = segments::search_text_of("Hello World");
        assert!(ascii.origin.is_none());
        assert_eq!(ascii.find("world", "Hello World"), vec![6..11]);
    }

    #[test]
    fn v075_overlay_gives_the_current_match_its_own_style() {
        use gpui_kit::{FontWeight, HighlightStyle, hsla};
        let styles = render::MatchStyles {
            other: HighlightStyle {
                background_color: Some(hsla(0.5, 0.5, 0.5, 0.25)),
                color: Some(hsla(0.0, 0.0, 0.1, 1.0)),
                ..Default::default()
            },
            current: HighlightStyle {
                background_color: Some(hsla(0.15, 0.9, 0.5, 1.0)),
                color: Some(hsla(0.0, 0.0, 0.0, 1.0)),
                ..Default::default()
            },
        };
        // A bold, colored base run over the whole text "abc needle needle".
        let base_style = HighlightStyle {
            color: Some(hsla(0.6, 0.6, 0.6, 1.0)),
            font_weight: Some(FontWeight::BOLD),
            ..Default::default()
        };
        let base = vec![(0..17, base_style)];
        // Matches of segment 0 (the second one current) and one of segment 1.
        let matches = vec![(0, 4..10, false), (0, 11..17, true), (1, 0..3, false)];
        let runs = render::overlay_matches(base.clone(), &matches, 0, styles);
        let style_at = |byte: usize| {
            runs.iter()
                .find(|(range, _)| range.contains(&byte))
                .map(|(_, style)| *style)
                .expect("styled byte")
        };
        // Outside the matches the base run is untouched.
        assert_eq!(style_at(0), base_style);
        // Other matches: match background and foreground over the base.
        let other = style_at(4);
        assert_eq!(other.background_color, styles.other.background_color);
        assert_eq!(other.color, styles.other.color);
        assert_eq!(other.font_weight, Some(FontWeight::BOLD), "base flags stay");
        // The current match: the current-match style.
        let current = style_at(11);
        assert_eq!(current.background_color, styles.current.background_color);
        assert_eq!(current.color, styles.current.color);
        assert_ne!(current.background_color, other.background_color);
        // Another segment's match is not drawn here; no matches = the base.
        assert!(
            runs.iter()
                .filter(|(_, s)| s.background_color.is_some())
                .count()
                == 2
        );
        assert_eq!(render::overlay_matches(base.clone(), &[], 0, styles), base);
        assert_eq!(
            render::overlay_matches(base.clone(), &matches, 2, styles),
            base
        );
    }

    fn id(n: u16) -> (ItemId, u64) {
        (
            ItemId {
                key: ItemKey::Text {
                    entry: n as usize,
                    kind: TextKind::Markdown,
                    ordinal: 0,
                },
                chunk: 0,
            },
            0,
        )
    }

    #[test]
    fn splice_diff_keeps_unchanged_items() {
        let old: Vec<_> = (0..5).map(id).collect();
        assert!(items::splice_diff(&old, &old).is_empty());
        // Insert two after item 1, remove item 3, change item 4.
        let mut new = vec![id(0), id(1), id(10), id(11), id(2)];
        let mut changed = id(4);
        changed.1 = 9;
        new.push(changed);
        let splices = items::splice_diff(&old, &new);
        assert_eq!(
            splices,
            vec![
                Splice {
                    start: 2,
                    removed: 0,
                    inserted: 2
                },
                Splice {
                    start: 5,
                    removed: 2,
                    inserted: 1
                },
            ]
        );
        // Applying the splices to the old length yields the new length.
        let mut len = old.len();
        for s in &splices {
            len = len - s.removed + s.inserted;
        }
        assert_eq!(len, new.len());
        assert_eq!(
            items::splice_diff(&old, &[]),
            vec![Splice {
                start: 0,
                removed: 5,
                inserted: 0
            }]
        );
    }

    #[test]
    fn status_hints_trim_low_priority_first_and_keep_help() {
        let hint = |key: &str, label: &'static str, pinned: bool| status::Hint {
            key: key.into(),
            label,
            pinned,
        };
        let hints = vec![
            hint("/", "search", false),
            hint("e", "export", false),
            hint("y", "yank", false),
            hint("F1", "help", true),
        ];
        let all = status::trim(hints.clone(), 200);
        assert_eq!(all.len(), 4);
        let trimmed = status::trim(hints, 20);
        assert_eq!(
            trimmed.iter().map(status::Hint::text).collect::<Vec<_>>(),
            ["/ search", "F1 help"]
        );
        assert_eq!(
            status::search_strip_with(
                "1/3",
                Some("n".into()),
                Some("N".into()),
                Some("Esc".into())
            ),
            "n next  N prev  1/3  Esc clear"
        );
        let prefs = prefs(ToolDisplayMode::Truncated, true, false);
        assert_eq!(
            status::toggle_texts(&prefs),
            ["tools·trn", "think·on", "info·off"]
        );
    }

    #[test]
    fn nav_helpers_map_items_and_positions() {
        let lines = long_conversation(6);
        let (_dir, model) = model_for(&lines, prefs(ToolDisplayMode::Hidden, false, false));
        let doc = model.document();
        let (items, info) = items::build_with_info(&doc, &model, 80, &|| false).unwrap();
        assert_eq!(nav::indicator(&items, &info, 0), "[1/6]");
        assert_eq!(nav::indicator(&items, &info, items.len() - 1), "[6/6]");
        assert_eq!(nav::first_at_or_below_top(&info, 0, true), Some(0));
        assert_eq!(nav::first_at_or_below_top(&info, 0, false), Some(1));
        assert_eq!(
            nav::position_of_entry_or_next(&info, info.nav_entries[2]),
            Some(2)
        );
        assert_eq!(nav::item_range_of(&items, &info, 0), Some((0, 0)));
    }
}

mod gui {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    use claude_history_core::view::{ToolDisplayMode, ViewSearchMode};
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Entity, Focusable as _, InputEvent as _, Modifiers,
        MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Point, TestAppContext, point,
        px, size,
    };

    use crate::actions::*;
    use crate::globals::last_toast;
    use crate::model::{
        ContentVisibilityHint, ConversationTarget, OpenRequest, RevealKind, RevealRequest,
    };
    use crate::services::ServiceCall;
    use crate::test_support::fixtures_viewer::*;
    use crate::test_support::{TestWorld, kbd, open_single_file, recording};
    use crate::viewer::items::ItemKind;
    use crate::viewer::{PARTIAL_COPY_TOAST, ViewerPane, ViewerPaneEvent, failed_to_open, status};

    fn read<R>(
        cx: &mut TestAppContext,
        viewer: &Entity<ViewerPane>,
        f: impl FnOnce(&ViewerPane) -> R,
    ) -> R {
        cx.read(|cx| f(viewer.read(cx)))
    }

    fn keys(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        viewer: &Entity<ViewerPane>,
        keys: &str,
    ) {
        cx.simulate_keystrokes(handle, keys);
        wait_viewer_ready(cx, handle, viewer);
    }

    fn tool_display(cx: &mut TestAppContext, viewer: &Entity<ViewerPane>) -> ToolDisplayMode {
        read(cx, viewer, |v| v.prefs().unwrap().tool_display)
    }

    fn top_item(cx: &mut TestAppContext, viewer: &Entity<ViewerPane>) -> usize {
        read(cx, viewer, |v| v.list_state().logical_scroll_top().item_ix)
    }

    fn top_offset(cx: &mut TestAppContext, viewer: &Entity<ViewerPane>) -> f32 {
        read(cx, viewer, |v| {
            f32::from(v.list_state().logical_scroll_top().offset_in_item)
        })
    }

    fn events(
        cx: &mut TestAppContext,
        viewer: &Entity<ViewerPane>,
    ) -> Rc<RefCell<Vec<ViewerPaneEvent>>> {
        let log = Rc::new(RefCell::new(Vec::new()));
        let sink = log.clone();
        cx.update(|cx| {
            cx.subscribe(viewer, move |_, event: &ViewerPaneEvent, _| {
                sink.borrow_mut().push(event.clone())
            })
            .detach()
        });
        log
    }

    /// Advance past the width debounce and settle.
    fn settle(cx: &mut TestAppContext, handle: AnyWindowHandle, viewer: &Entity<ViewerPane>) {
        draw(cx, handle);
        cx.executor().advance_clock(Duration::from_millis(250));
        wait_viewer_ready(cx, handle, viewer);
    }

    fn move_mouse(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        position: Point<gpui_kit::Pixels>,
    ) {
        cx.update_window(handle, |_, window, cx| {
            window.dispatch_event(
                MouseMoveEvent {
                    position,
                    pressed_button: None,
                    modifiers: Modifiers::default(),
                }
                .to_platform_input(),
                cx,
            );
            window.render_frame(cx);
        })
        .unwrap();
    }

    fn click_at(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        position: Point<gpui_kit::Pixels>,
    ) {
        move_mouse(cx, handle, position);
        cx.update_window(handle, |_, window, cx| {
            window.dispatch_event(
                MouseDownEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                    first_mouse: false,
                }
                .to_platform_input(),
                cx,
            );
            window.dispatch_event(
                MouseUpEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                }
                .to_platform_input(),
                cx,
            );
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
    }

    /// The window position of byte `byte` of segment `segment` of item `item`.
    fn text_position(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        viewer: &Entity<ViewerPane>,
        item: usize,
        segment: usize,
        byte: usize,
    ) -> Point<gpui_kit::Pixels> {
        viewer.update(cx, |v, cx| v.request_text_probe(item, segment, byte, cx));
        draw(cx, handle);
        read(cx, viewer, |v| v.text_probe()).expect("the probed segment painted")
    }

    fn item_index(
        cx: &mut TestAppContext,
        viewer: &Entity<ViewerPane>,
        pred: impl Fn(&crate::viewer::items::ViewerItem) -> bool,
    ) -> usize {
        read(cx, viewer, |v| {
            v.items().unwrap().iter().position(&pred).expect("item")
        })
    }

    // ----- open / install ------------------------------------------------------

    #[gpui_kit::test]
    fn opens_a_conversation_and_installs_items(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (_h, viewer, path) = open_viewer(cx, &world, &long_conversation(4), &[]);
        read(cx, &viewer, |v| {
            assert_eq!(v.target().unwrap().path, path);
            assert_eq!(v.items().unwrap().len(), 4);
            assert_eq!(v.build_info().unwrap().nav_count, 4);
            assert!(!v.is_loading());
        });
    }

    /// V-101: opening 10 conversations in a row installs only the last.
    #[gpui_kit::test]
    fn v101_rapid_opens_install_only_the_last(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let dir = world.home().join("many");
        let paths: Vec<_> = (0..10)
            .map(|i| write_transcript(&dir, &format!("c{i}.jsonl"), &long_conversation(2 + i)))
            .collect();
        let (handle, ws) = crate::test_support::open_main(cx, &world, &[]);
        let viewer = cx.read(|cx| ws.read(cx).viewer().clone());
        cx.update_window(handle, |_, window, cx| {
            viewer.update(cx, |viewer, cx| {
                for path in &paths {
                    viewer.open(
                        OpenRequest::new(ConversationTarget::from_parsed_file(path, None), false),
                        window,
                        cx,
                    );
                }
            })
        })
        .unwrap();
        wait_viewer_ready(cx, handle, &viewer);
        read(cx, &viewer, |v| {
            assert_eq!(v.target().unwrap().path, paths[9]);
            assert_eq!(v.items().unwrap().len(), 11);
            assert!(v.completed_builds() < 10, "{} builds", v.completed_builds());
        });
    }

    /// §15.8 (working set stable over 20 opens), §9.7: opening another
    /// conversation drops the previous items, document and caches.
    #[gpui_kit::test]
    fn opening_another_conversation_releases_the_previous_one(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, ws, viewer, _) = open_main_viewer(cx, &world, &long_conversation(6), &[]);
        let first = read(cx, &viewer, |v| {
            std::sync::Arc::downgrade(&v.items().unwrap())
        });
        keys(cx, handle, &viewer, "/");
        cx.simulate_input(handle, "message");
        wait_viewer_ready(cx, handle, &viewer);
        assert!(read(cx, &viewer, |v| v.has_search_index()));
        let other = write_transcript(
            &world.home().join("other"),
            "other.jsonl",
            &long_conversation(3),
        );
        open_in_main(cx, handle, &ws, &other, true);
        read(cx, &viewer, |v| {
            assert_eq!(v.items().unwrap().len(), 3);
            assert!(!v.has_search_index(), "search texts dropped on open");
        });
        assert!(
            first.upgrade().is_none(),
            "the previous conversation's items are released"
        );
    }

    /// A-054: a directory as a conversation shows the error view and a toast.
    #[gpui_kit::test]
    fn a054_open_failure_shows_error_view_and_toast(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, ws) = crate::test_support::open_main(cx, &world, &[]);
        let dir = world.home().join("a-directory");
        std::fs::create_dir_all(&dir).unwrap();
        let viewer = open_in_main(cx, handle, &ws, &dir, true);
        let expected = failed_to_open(
            &claude_history_core::view::ViewerModel::open(&dir, Default::default())
                .unwrap_err()
                .to_string(),
        );
        assert_eq!(read(cx, &viewer, |v| v.failure()), Some(expected.clone()));
        assert_eq!(cx.read(last_toast).as_ref(), expected);
        cx.update_window(handle, |_, window, _| {
            assert!(window.try_find("viewer-retry").is_some());
            assert!(window.try_find("viewer-debug-log").is_some());
        })
        .unwrap();
    }

    /// V-002: toggles keep working after the file was deleted.
    #[gpui_kit::test]
    fn v002_toggles_work_after_the_file_is_deleted(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let lines = vec![
            user("q"),
            bash_call("b", "ls", 1),
            tool_result("b", "a\nb", 1),
        ];
        let (handle, viewer, path) = open_viewer(cx, &world, &lines, &[]);
        let before = read(cx, &viewer, |v| v.items().unwrap().len());
        std::fs::remove_file(&path).unwrap();
        keys(cx, handle, &viewer, "t");
        read(cx, &viewer, |v| {
            assert!(v.failure().is_none());
            assert!(
                v.items().unwrap().len() > before,
                "tools shown after the delete"
            );
        });
    }

    /// V-082: no file watching.
    #[gpui_kit::test]
    fn v082_appending_to_the_file_changes_nothing_until_reopened(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, viewer, path) = open_viewer(cx, &world, &long_conversation(2), &[]);
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(&user("appended later"));
        text.push('\n');
        std::fs::write(&path, text).unwrap();
        keys(cx, handle, &viewer, "t");
        keys(cx, handle, &viewer, "t");
        keys(cx, handle, &viewer, "t");
        assert_eq!(read(cx, &viewer, |v| v.items().unwrap().len()), 2);
    }

    /// V-003: header, toolbar, list and status regions.
    #[gpui_kit::test]
    fn v003_viewer_regions_are_rendered(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, _viewer, _) = open_viewer(cx, &world, &long_conversation(2), &[]);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            for id in [
                "viewer-header",
                "viewer-toolbar",
                "viewer-list",
                "viewer-status",
            ] {
                assert!(window.try_find(id).is_some(), "{id}");
            }
            let header = window.find("viewer-header").bounds();
            let list = window.find("viewer-list").bounds();
            let status = window.find("viewer-status").bounds();
            assert!(header.top() < list.top() && list.bottom() <= status.top() + px(1.));
        })
        .unwrap();
    }

    /// V-006: no corpus entry → the header is the file stem.
    #[gpui_kit::test]
    fn v006_set_header_none_shows_the_file_stem(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (_h, _ws, viewer, _) = open_main_viewer(cx, &world, &long_conversation(2), &[]);
        viewer.update(cx, |v, cx| v.set_header(None, cx));
        assert_eq!(
            read(cx, &viewer, |v| v.header_layout().unwrap().text()),
            "main-fixture"
        );
    }

    /// V-008: a malformed line shows `⚠ 1`; clicking it opens the Debug log.
    #[gpui_kit::test]
    fn v008_parse_error_badge(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let paths = world.standard_corpus();
        let broken = paths
            .iter()
            .find(|p| {
                p.to_string_lossy()
                    .contains(&crate::test_support::synth::uuid(91))
            })
            .unwrap()
            .clone();
        let handle = open_single_file(cx, &world, &broken);
        let viewer = single_file_viewer(cx, handle);
        wait_viewer_ready(cx, handle, &viewer);
        let layout = read(cx, &viewer, |v| v.header_layout().unwrap());
        assert_eq!(layout.parse_errors, 1);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("viewer-parse-errors").is_some());
            window.click("viewer-parse-errors", cx);
        })
        .unwrap();
        // Tools → Debug log (WP5): the dialog opens on the Parse errors tab
        // with this window's conversation captured (the single-file window's
        // own viewer, not the main window's).
        let dialog = cx
            .read(crate::tools::debug_log::current_dialog)
            .expect("the Debug log opened");
        assert_eq!(
            cx.read(|cx| dialog.read(cx).tab()),
            crate::model::DebugLogTab::ParseErrors
        );
        assert_eq!(
            cx.read(|cx| dialog.read(cx).captured_target().map(|t| t.path.clone())),
            Some(broken.clone())
        );
        // Integrator, Phase C (acceptance depth): the badge reads `⚠ 1` with
        // the tooltip `1 malformed line` in the main window too, and its click
        // opens the Parse errors tab there, listing the error.
        assert_eq!(
            cx.read(
                crate::tools::common::current_window::<crate::tools::debug_log::DebugLogDialog>
            ),
            Some(handle),
            "opened in the clicking window"
        );
        cx.update_window(handle, |_, window, cx| {
            dialog.update(cx, |dialog, cx| dialog.close(window, cx))
        })
        .unwrap();
        let (main, ws) = crate::test_support::open_main(cx, &world, &[]);
        let viewer = open_in_main(cx, main, &ws, &broken, true);
        // The main window's header comes from the corpus: opened before the
        // initial load ends it shows the stem, and `CorpusChanged` fills it in
        // (a loaded machine reaches the open first).
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while read(cx, &viewer, |v| v.header_layout().unwrap().parse_errors) == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the corpus never filled the header"
            );
            cx.executor().allow_parking();
            std::thread::sleep(Duration::from_millis(10));
            cx.run_until_parked();
        }
        let layout = read(cx, &viewer, |v| v.header_layout().unwrap());
        assert_eq!(layout.parse_errors, 1);
        assert_eq!(
            crate::viewer::header::parse_error_badge(layout.parse_errors),
            "⚠ 1"
        );
        assert_eq!(
            crate::viewer::header::parse_error_tooltip(layout.parse_errors),
            "1 malformed line"
        );
        cx.update_window(main, |_, window, cx| {
            window.render_frame(cx);
            window.click("viewer-parse-errors", cx);
        })
        .unwrap();
        let dialog = cx
            .read(crate::tools::debug_log::current_dialog)
            .expect("the Debug log opened in the main window");
        assert_eq!(
            cx.read(
                crate::tools::common::current_window::<crate::tools::debug_log::DebugLogDialog>
            ),
            Some(main)
        );
        assert_eq!(
            cx.read(|cx| dialog.read(cx).tab()),
            crate::model::DebugLogTab::ParseErrors
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            cx.executor().allow_parking();
            cx.run_until_parked();
            let text = cx.read(|cx| dialog.read(cx).text(cx));
            if text.contains("Errors: 1") {
                assert!(
                    text.contains(&format!("File: {}", broken.display())),
                    "{text}"
                );
                break;
            }
            assert!(std::time::Instant::now() < deadline, "{text}");
            std::thread::sleep(Duration::from_millis(3));
        }
    }

    // ----- status, toggles ---------------------------------------------------------

    /// V-009: the scroll indicator.
    #[gpui_kit::test]
    fn v009_scroll_indicator(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, viewer, _) = open_viewer(cx, &world, &long_conversation(40), &[]);
        assert_eq!(read(cx, &viewer, |v| v.indicator()), "[1/40]");
        keys(cx, handle, &viewer, "shift-g");
        let indicator = read(cx, &viewer, |v| v.indicator());
        let k: usize = indicator[1..indicator.find('/').unwrap()].parse().unwrap();
        assert!(k > 1 && k <= 40, "{indicator}");
        let top = top_item(cx, &viewer);
        let expected = read(cx, &viewer, |v| {
            v.items().unwrap()[top].nav_position.unwrap() + 1
        });
        assert_eq!(k, expected);
        keys(cx, handle, &viewer, "g");
        for _ in 0..45 {
            cx.simulate_keystrokes(handle, "shift-j");
        }
        wait_viewer_ready(cx, handle, &viewer);
        assert_eq!(read(cx, &viewer, |v| v.nav().focused()), Some(39));
    }

    /// V-010: `t`, `T`, `i` and clicks on the status indicators.
    #[gpui_kit::test]
    fn v010_toggle_indicators(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, viewer, _) = open_viewer(cx, &world, &long_conversation(2), &[]);
        let texts = |cx: &mut TestAppContext| {
            read(cx, &viewer, |v| status::toggle_texts(&v.prefs().unwrap()))
        };
        assert_eq!(texts(cx)[0], "tools·sum");
        keys(cx, handle, &viewer, "t");
        assert_eq!(texts(cx)[0], "tools·trn");
        keys(cx, handle, &viewer, "t");
        assert_eq!(texts(cx)[0], "tools·all");
        keys(cx, handle, &viewer, "t");
        assert_eq!(texts(cx)[0], "tools·sum");
        keys(cx, handle, &viewer, "shift-t");
        assert_eq!(texts(cx)[1], "think·on");
        keys(cx, handle, &viewer, "i");
        assert_eq!(texts(cx)[2], "info·on");
        cx.update_window(handle, |_, window, cx| window.click("status-think", cx))
            .unwrap();
        wait_viewer_ready(cx, handle, &viewer);
        assert_eq!(texts(cx)[1], "think·off");
    }

    /// V-011: single-file hints and the active-search strip.
    #[gpui_kit::test]
    fn v011_status_hints(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let lines = vec![
            user("needle one"),
            assistant("needle two"),
            user("needle three"),
        ];
        let (handle, viewer, _) = open_viewer(cx, &world, &lines, &[]);
        let hints = cx
            .update_window(handle, |_, window, cx| {
                viewer.read(cx).status_hint_texts(window)
            })
            .unwrap();
        assert!(hints.iter().any(|h| h == "q close"), "{hints:?}");
        assert!(
            !hints
                .iter()
                .any(|h| h.ends_with("resume") || h.ends_with("fork") || h.ends_with("del"))
        );
        keys(cx, handle, &viewer, "/");
        cx.simulate_input(handle, "needle");
        keys(cx, handle, &viewer, "enter");
        let hints = cx
            .update_window(handle, |_, window, cx| {
                viewer.read(cx).status_hint_texts(window)
            })
            .unwrap();
        assert_eq!(
            hints,
            [format!("n next  N prev  1/3  {} clear", kbd("escape"))]
        );
    }

    /// V-012: the find bar text.
    #[gpui_kit::test]
    fn v012_find_bar_text(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let lines = vec![user("needle one"), assistant("needle two needle three")];
        let (handle, viewer, _) = open_viewer(cx, &world, &lines, &[]);
        keys(cx, handle, &viewer, "/");
        cx.simulate_input(handle, "zzz");
        wait_viewer_ready(cx, handle, &viewer);
        assert_eq!(
            read(cx, &viewer, |v| v.find_bar_text()),
            "/zzz (no matches)"
        );
        cx.simulate_keystrokes(handle, "ctrl-u");
        cx.simulate_input(handle, "needle");
        wait_viewer_ready(cx, handle, &viewer);
        assert_eq!(read(cx, &viewer, |v| v.find_bar_text()), "/needle (1/3)");
    }

    /// V-048: the tool display carries over to the next conversation.
    #[gpui_kit::test]
    fn v048_tool_display_persists_across_conversations(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, ws, viewer, _) = open_main_viewer(cx, &world, &long_conversation(2), &[]);
        keys(cx, handle, &viewer, "t");
        assert_eq!(tool_display(cx, &viewer), ToolDisplayMode::Truncated);
        let other = write_transcript(
            &world.home().join("other"),
            "other.jsonl",
            &long_conversation(3),
        );
        let viewer = open_in_main(cx, handle, &ws, &other, true);
        assert_eq!(tool_display(cx, &viewer), ToolDisplayMode::Truncated);
    }

    /// V-062: `T` shows thinking.
    #[gpui_kit::test]
    fn v062_thinking_toggle_adds_thinking_items(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let lines = vec![user("q"), thinking_and_text("hmm", "answer", 1)];
        let (handle, viewer, _) = open_viewer(cx, &world, &lines, &[]);
        let thinking = |cx: &mut TestAppContext| {
            read(cx, &viewer, |v| {
                v.items()
                    .unwrap()
                    .iter()
                    .filter(|i| i.kind == ItemKind::Thinking)
                    .count()
            })
        };
        assert_eq!(thinking(cx), 0);
        keys(cx, handle, &viewer, "shift-t");
        assert_eq!(thinking(cx), 1);
        assert_eq!(
            read(cx, &viewer, |v| status::toggle_texts(&v.prefs().unwrap())
                [1]
            .clone()),
            "think·on"
        );
    }

    // ----- expansion -----------------------------------------------------------

    fn truncated_fixture() -> Vec<String> {
        let prompt = numbered_lines(10);
        vec![
            user("q"),
            tool_call(
                "t",
                "Task",
                &format!(
                    r#"{{"description":"d","prompt":{}}}"#,
                    serde_json::to_string(&prompt).unwrap()
                ),
                1,
            ),
            tool_result("t", &numbered_lines(9), 1),
            assistant_at("after", 2),
        ]
    }

    /// V-056 / V-059: Truncated mode indicators; a click expands and focuses.
    #[gpui_kit::test]
    fn v056_v059_indicator_click_expands_and_focuses(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, viewer, _) = open_viewer(cx, &world, &truncated_fixture(), &[]);
        keys(cx, handle, &viewer, "t");
        settle(cx, handle, &viewer);
        let call = item_index(cx, &viewer, |i| i.kind == ItemKind::ToolCall);
        let (more, hash) = read(cx, &viewer, |v| {
            let item = &v.items().unwrap()[call];
            (item.more, item.key_hash)
        });
        assert_eq!(more, Some(7));
        cx.update_window(handle, |_, window, cx| {
            window.click(gpui_kit::ElementId::NamedInteger("more".into(), hash), cx)
        })
        .unwrap();
        wait_viewer_ready(cx, handle, &viewer);
        read(cx, &viewer, |v| {
            let item = &v.items().unwrap()[call];
            assert!(item.expanded);
            assert_eq!(item.segments[1].line_count(), 10);
            assert!(v.nav().is_active());
            assert_eq!(v.nav().focused(), item.nav_position);
        });
    }

    /// V-060: the expanded set survives toggles, resets on open.
    #[gpui_kit::test]
    fn v060_expanded_set_survives_toggles_not_reopen(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, ws, viewer, path) = open_main_viewer(cx, &world, &truncated_fixture(), &[]);
        keys(cx, handle, &viewer, "t");
        let call = item_index(cx, &viewer, |i| i.kind == ItemKind::ToolCall);
        let id = read(cx, &viewer, |v| {
            v.items().unwrap()[call].toggle.clone().unwrap()
        });
        cx.update_window(handle, |_, window, cx| {
            viewer.update(cx, |v, cx| v.toggle_output(&id, call, window, cx))
        })
        .unwrap();
        wait_viewer_ready(cx, handle, &viewer);
        keys(cx, handle, &viewer, "shift-t");
        keys(cx, handle, &viewer, "shift-t");
        assert!(read(cx, &viewer, |v| v
            .model()
            .unwrap()
            .expanded
            .contains(&id)));
        let other = write_transcript(
            &world.home().join("other"),
            "other.jsonl",
            &long_conversation(2),
        );
        open_in_main(cx, handle, &ws, &other, true);
        let viewer = open_in_main(cx, handle, &ws, &path, true);
        assert!(read(cx, &viewer, |v| v
            .model()
            .unwrap()
            .expanded
            .is_empty()));
    }

    /// V-052: summary group chevrons.
    #[gpui_kit::test]
    fn v052_summary_group_expands_and_collapses(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let lines = vec![
            user("q"),
            tool_call("g1", "Grep", r#"{"pattern":"one"}"#, 1),
            tool_result("g1", &numbered_lines(8), 1),
            tool_call("r1", "Read", r#"{"file_path":"x.rs"}"#, 1),
            tool_result("r1", "content", 1),
            assistant_at("done", 2),
        ];
        let (handle, viewer, _) = open_viewer(cx, &world, &lines, &[]);
        let count = |cx: &mut TestAppContext| read(cx, &viewer, |v| v.items().unwrap().len());
        let before = count(cx);
        let summary = item_index(cx, &viewer, |i| i.kind == ItemKind::ToolSummary);
        let hash = read(cx, &viewer, |v| v.items().unwrap()[summary].key_hash);
        cx.update_window(handle, |_, window, cx| {
            window.click(
                gpui_kit::ElementId::NamedInteger("summary-chevron".into(), hash),
                cx,
            )
        })
        .unwrap();
        wait_viewer_ready(cx, handle, &viewer);
        let expanded = count(cx);
        assert!(expanded > before, "details added");
        // An inner result toggles on its own.
        let inner = item_index(cx, &viewer, |i| {
            i.kind == ItemKind::ToolResult && i.more.is_some()
        });
        let (inner_id, inner_hash) = read(cx, &viewer, |v| {
            (
                v.items().unwrap()[inner].toggle.clone().unwrap(),
                v.items().unwrap()[inner].key_hash,
            )
        });
        cx.update_window(handle, |_, window, cx| {
            window.click(
                gpui_kit::ElementId::NamedInteger("more".into(), inner_hash),
                cx,
            )
        })
        .unwrap();
        wait_viewer_ready(cx, handle, &viewer);
        assert!(read(cx, &viewer, |v| v
            .model()
            .unwrap()
            .expanded
            .contains(&inner_id)));
        assert_eq!(count(cx), expanded);
        // The group chevron collapses the details again.
        let first_detail = item_index(cx, &viewer, |i| i.group_toggle.is_some());
        let group_hash = read(cx, &viewer, |v| v.items().unwrap()[first_detail].key_hash);
        cx.update_window(handle, |_, window, cx| {
            window.click(
                gpui_kit::ElementId::NamedInteger("group-chevron".into(), group_hash),
                cx,
            )
        })
        .unwrap();
        wait_viewer_ready(cx, handle, &viewer);
        assert_eq!(count(cx), before);
    }

    /// V-095: `x` toggles every output of the focused message.
    #[gpui_kit::test]
    fn v095_x_toggles_all_outputs_of_the_focused_message(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let lines = vec![
            user("q"),
            assistant_blocks(
                &format!(
                    r#"[{{"type":"tool_use","id":"a","name":"Task","input":{{"description":"one","prompt":{p}}}}},{{"type":"tool_use","id":"b","name":"Task","input":{{"description":"two","prompt":{p}}}}}]"#,
                    p = serde_json::to_string(&numbered_lines(8)).unwrap()
                ),
                1,
            ),
        ];
        let (handle, viewer, _) = open_viewer(cx, &world, &lines, &[]);
        keys(cx, handle, &viewer, "t");
        keys(cx, handle, &viewer, "shift-j");
        keys(cx, handle, &viewer, "x");
        let expanded =
            |cx: &mut TestAppContext| read(cx, &viewer, |v| v.model().unwrap().expanded.len());
        assert_eq!(expanded(cx), 2);
        keys(cx, handle, &viewer, "x");
        assert_eq!(expanded(cx), 0);
    }

    /// V-057 / V-080: narrowing the window recomputes truncation and keeps the top.
    #[gpui_kit::test]
    fn v057_v080_width_change_recomputes_truncation(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let mut lines = long_conversation(20);
        lines.push(bash_call("b", "cat big", 30));
        lines.push(tool_result("b", &"x".repeat(50_000), 30));
        let (handle, viewer, _) = open_viewer(cx, &world, &lines, &[]);
        keys(cx, handle, &viewer, "t");
        settle(cx, handle, &viewer);
        let result = item_index(cx, &viewer, |i| i.kind == ItemKind::ToolResult);
        let (wide_more, wide_cols) = read(cx, &viewer, |v| {
            (v.items().unwrap()[result].more.unwrap(), v.cols())
        });
        let rows = read(cx, &viewer, |v| {
            crate::viewer::items::item_text(&v.items().unwrap()[result])
        });
        assert!(rows.lines().count() <= 4 && rows.lines().all(|l| l.chars().count() <= wide_cols));
        keys(
            cx,
            handle,
            &viewer,
            "shift-j shift-j shift-j shift-j shift-j",
        );
        keys(cx, handle, &viewer, "escape");
        let top_id = read(cx, &viewer, |v| {
            v.items().unwrap()[v.list_state().logical_scroll_top().item_ix]
                .id
                .clone()
        });
        cx.simulate_window_resize(handle, size(px(700.), px(800.)));
        settle(cx, handle, &viewer);
        let (narrow_more, narrow_cols) = read(cx, &viewer, |v| {
            (v.items().unwrap()[result].more.unwrap(), v.cols())
        });
        assert!(narrow_cols < wide_cols, "{narrow_cols} < {wide_cols}");
        assert!(narrow_more > wide_more);
        let new_top = read(cx, &viewer, |v| {
            v.items().unwrap()[v.list_state().logical_scroll_top().item_ix]
                .id
                .clone()
        });
        assert_eq!(new_top, top_id);
    }

    /// V-081 / X-015: toggles keep the top message at the same offset.
    #[gpui_kit::test]
    fn v081_x015_toggles_keep_the_anchor(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let mut lines = Vec::new();
        for ix in 0..40 {
            lines.push(user_at(
                &format!("Question {ix}\n\nmore text\n\nand more"),
                ix,
            ));
            lines.push(thinking_and_text(
                "thinking here",
                &format!("Answer {ix}"),
                ix,
            ));
            lines.push(bash_call(&format!("b{ix}"), "ls", ix));
            lines.push(tool_result(&format!("b{ix}"), &numbered_lines(6), ix));
        }
        let (handle, viewer, _) = open_viewer(cx, &world, &lines, &[]);
        // Scroll so that a user message near message 20 is at the top,
        // partly scrolled (a summary group would be replaced by `t`).
        let first = read(cx, &viewer, |v| {
            let items = v.items().unwrap();
            let info = v.build_info().unwrap();
            info.nav_first_item[20..]
                .iter()
                .copied()
                .find(|&ix| items[ix].lanes.label.as_deref() == Some("You"))
                .unwrap()
        });
        viewer.update(cx, |v, _| {
            v.list_state().scroll_to(gpui_kit::ListOffset {
                item_ix: first,
                offset_in_item: px(5.),
            })
        });
        draw(cx, handle);
        let id = read(cx, &viewer, |v| v.items().unwrap()[first].id.clone());
        for key in ["t", "shift-t", "t", "i"] {
            keys(cx, handle, &viewer, key);
            let top = top_item(cx, &viewer);
            assert_eq!(
                read(cx, &viewer, |v| v.items().unwrap()[top].id.clone()),
                id,
                "after {key}"
            );
            assert_eq!(top_offset(cx, &viewer), 5.0, "after {key}");
        }
        // A window resize keeps it too.
        cx.simulate_window_resize(handle, size(px(900.), px(700.)));
        settle(cx, handle, &viewer);
        let top = top_item(cx, &viewer);
        assert_eq!(
            read(cx, &viewer, |v| v.items().unwrap()[top].id.clone()),
            id
        );
        assert_eq!(top_offset(cx, &viewer), 5.0, "after the resize");
    }

    /// A toggle pressed while another conversation is still opening
    /// restarts that open; it never rebuilds the previous conversation under
    /// the new target.
    #[gpui_kit::test]
    fn toggle_while_opening_shows_the_new_conversation(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, _ws, viewer, _) = open_main_viewer(cx, &world, &long_conversation(3), &[]);
        let other = write_transcript(&world.home().join("two"), "b.jsonl", &long_conversation(7));
        cx.update_window(handle, |_, window, cx| {
            viewer.update(cx, |viewer, cx| {
                viewer.open(
                    OpenRequest::new(ConversationTarget::from_parsed_file(&other, None), false),
                    window,
                    cx,
                );
                viewer.handle_forwarded(&ToggleThinking, window, cx);
                viewer.handle_forwarded(&CycleToolDisplay, window, cx);
            })
        })
        .unwrap();
        wait_viewer_ready(cx, handle, &viewer);
        read(cx, &viewer, |v| {
            assert_eq!(v.target().unwrap().path, other);
            assert_eq!(v.model().unwrap().path, other);
            assert_eq!(v.items().unwrap().len(), 7);
            assert_eq!(v.build_info().unwrap().nav_count, 7);
        });
    }

    /// V-056 / V-059: an expanded tool result can be collapsed with its
    /// chevron (the original keeps expanded result rows clickable).
    #[gpui_kit::test]
    fn expanded_tool_result_has_a_collapse_control(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let lines = vec![
            user("q"),
            bash_call("b1", "seq 12", 1),
            tool_result("b1", &numbered_lines(12), 1),
        ];
        let (handle, viewer, _) = open_viewer(cx, &world, &lines, &[]);
        keys(cx, handle, &viewer, "t");
        settle(cx, handle, &viewer);
        let result = item_index(cx, &viewer, |i| i.kind == ItemKind::ToolResult);
        let (more, hash) = read(cx, &viewer, |v| {
            let item = &v.items().unwrap()[result];
            (item.more, item.key_hash)
        });
        assert!(more.is_some(), "collapsed result shows (N more lines…)");
        cx.update_window(handle, |_, window, cx| {
            window.click(gpui_kit::ElementId::NamedInteger("more".into(), hash), cx)
        })
        .unwrap();
        wait_viewer_ready(cx, handle, &viewer);
        let hash = read(cx, &viewer, |v| {
            let item = &v.items().unwrap()[result];
            assert!(item.expanded && item.more.is_none());
            item.key_hash
        });
        cx.update_window(handle, |_, window, cx| {
            window.click(
                gpui_kit::ElementId::NamedInteger("collapse-chevron".into(), hash),
                cx,
            )
        })
        .unwrap();
        wait_viewer_ready(cx, handle, &viewer);
        read(cx, &viewer, |v| {
            let item = &v.items().unwrap()[result];
            assert!(!item.expanded, "the chevron collapsed it again");
            assert!(item.more.is_some());
        });
    }

    /// X-015 with message navigation active: the focused message starts
    /// above the viewport and keeps its row across toggles.
    #[gpui_kit::test]
    fn nav_active_toggles_keep_a_message_above_the_viewport(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let mut lines = Vec::new();
        for ix in 0..40 {
            lines.push(user_at(
                &format!("Question {ix}\n\nmore text\n\nand more"),
                ix,
            ));
            lines.push(thinking_and_text(
                "thinking here",
                &format!("Answer {ix}"),
                ix,
            ));
        }
        let (handle, viewer, _) = open_viewer(cx, &world, &lines, &[]);
        keys(cx, handle, &viewer, "shift-j shift-j shift-j");
        let focused = read(cx, &viewer, |v| v.nav().focused().unwrap());
        let first = read(cx, &viewer, |v| {
            v.build_info().unwrap().nav_first_item[focused]
        });
        viewer.update(cx, |v, _| {
            v.list_state().scroll_to(gpui_kit::ListOffset {
                item_ix: first,
                offset_in_item: px(30.),
            })
        });
        draw(cx, handle);
        let id = read(cx, &viewer, |v| v.items().unwrap()[first].id.clone());
        keys(cx, handle, &viewer, "i");
        let top = top_item(cx, &viewer);
        assert_eq!(
            read(cx, &viewer, |v| v.items().unwrap()[top].id.clone()),
            id
        );
        assert_eq!(top_offset(cx, &viewer), 30.0);
    }

    /// V-041: only a left press and release on the link opens the dialog.
    #[gpui_kit::test]
    fn v041_right_click_on_a_link_does_not_open_the_dialog(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let lines = vec![user("[a link to the docs](https://example.com/docs)")];
        let (handle, viewer, _) = open_viewer(cx, &world, &lines, &[]);
        cx.update(|cx| cx.set_reduce_motion(true));
        let position = text_position(cx, handle, &viewer, 0, 0, 3) + point(px(1.), px(4.));
        move_mouse(cx, handle, position);
        cx.update_window(handle, |_, window, cx| {
            window.dispatch_event(
                MouseDownEvent {
                    button: MouseButton::Right,
                    position,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                    first_mouse: false,
                }
                .to_platform_input(),
                cx,
            );
            window.dispatch_event(
                MouseUpEvent {
                    button: MouseButton::Right,
                    position,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                }
                .to_platform_input(),
                cx,
            );
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
        let dialog = cx
            .update_window(handle, |_, window, cx| window.has_active_dialog(cx))
            .unwrap();
        assert!(!dialog, "a right click is not a link click");
    }

    // ----- navigation ------------------------------------------------------------

    /// V-067 / V-069 / V-070: J/K, the focus mark, Esc leaves navigation.
    #[gpui_kit::test]
    fn v067_v069_v070_message_navigation(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, viewer, _) = open_viewer(cx, &world, &long_conversation(40), &[]);
        keys(cx, handle, &viewer, "shift-j");
        assert_eq!(read(cx, &viewer, |v| v.nav().focused()), Some(1));
        assert_eq!(read(cx, &viewer, |v| v.render_ctx_snapshot().1), Some(1));
        keys(cx, handle, &viewer, "shift-k shift-k shift-k");
        assert_eq!(read(cx, &viewer, |v| v.nav().focused()), Some(0));
        // A focused message below the viewport is scrolled to.
        for _ in 0..30 {
            cx.simulate_keystrokes(handle, "]");
        }
        wait_viewer_ready(cx, handle, &viewer);
        let focused = read(cx, &viewer, |v| v.nav().focused().unwrap());
        let first = read(cx, &viewer, |v| {
            v.build_info().unwrap().nav_first_item[focused]
        });
        let visible = read(cx, &viewer, |v| {
            v.list_state().item_is_above_viewport(first) == Some(false)
                && v.list_state().item_is_below_viewport(first) == Some(false)
        });
        assert!(visible);
        keys(cx, handle, &viewer, "escape");
        assert!(!read(cx, &viewer, |v| v.nav().is_active()));
        cx.update_window(handle, |_, window, cx| {
            assert!(viewer.read(cx).contains_focus(window, cx));
        })
        .unwrap();
    }

    /// V-068: with navigation active a scroll re-syncs the focus.
    #[gpui_kit::test]
    fn v068_scrolling_resyncs_navigation(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, viewer, _) = open_viewer(cx, &world, &long_conversation(40), &[]);
        keys(cx, handle, &viewer, "shift-j");
        keys(cx, handle, &viewer, "pagedown");
        let top = top_item(cx, &viewer);
        let expected = read(cx, &viewer, |v| v.items().unwrap()[top].nav_position);
        assert_eq!(read(cx, &viewer, |v| v.nav().focused()), expected);
        assert!(expected.unwrap() > 1);
    }

    /// V-068 by the mouse wheel: the list calls its scroll handler while its
    /// state is borrowed, so the re-sync must not read the list from there
    /// (`RefCell already mutably borrowed` aborted the app).
    #[gpui_kit::test]
    fn v068_wheel_scrolling_resyncs_navigation(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, viewer, _) = open_viewer(cx, &world, &long_conversation(40), &[]);
        keys(cx, handle, &viewer, "shift-j");
        let at = text_position(cx, handle, &viewer, 0, 0, 0);
        cx.update_window(handle, |_, window, cx| {
            window.dispatch_event(
                gpui_kit::ScrollWheelEvent {
                    position: point(at.x + px(20.), at.y + px(4.)),
                    delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-1500.))),
                    modifiers: Modifiers::default(),
                    touch_phase: gpui_kit::TouchPhase::Moved,
                }
                .to_platform_input(),
                cx,
            );
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
        let top = top_item(cx, &viewer);
        let expected = read(cx, &viewer, |v| v.items().unwrap()[top].nav_position);
        assert_eq!(read(cx, &viewer, |v| v.nav().focused()), expected);
        assert!(expected.unwrap() > 1);
    }

    /// V-079: scroll keys do nothing while typing in the find bar.
    #[gpui_kit::test]
    fn v079_scroll_keys(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, viewer, _) = open_viewer(cx, &world, &long_conversation(40), &[]);
        keys(cx, handle, &viewer, "/");
        for key in ["down", "pagedown", "ctrl-d", "end"] {
            keys(cx, handle, &viewer, key);
            assert_eq!(top_item(cx, &viewer), 0, "{key} while typing");
            assert_eq!(top_offset(cx, &viewer), 0.0, "{key} while typing");
        }
        keys(cx, handle, &viewer, "escape");
        keys(cx, handle, &viewer, "shift-g");
        assert!(top_item(cx, &viewer) > 0);
        keys(cx, handle, &viewer, "g");
        assert_eq!(top_item(cx, &viewer), 0);
        keys(cx, handle, &viewer, "end");
        assert!(top_item(cx, &viewer) > 0);
        keys(cx, handle, &viewer, "home");
        assert_eq!(top_item(cx, &viewer), 0);
        keys(cx, handle, &viewer, "ctrl-d");
        assert!(top_item(cx, &viewer) > 0 || top_offset(cx, &viewer) > 0.0);
        let before = (top_item(cx, &viewer), top_offset(cx, &viewer));
        keys(cx, handle, &viewer, "j");
        assert_ne!((top_item(cx, &viewer), top_offset(cx, &viewer)), before);
    }

    /// V-086: Enter does nothing in the transcript.
    #[gpui_kit::test]
    fn v086_enter_does_nothing(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, viewer, path) = open_viewer(cx, &world, &long_conversation(10), &[]);
        keys(cx, handle, &viewer, "shift-j");
        let before = read(cx, &viewer, |v| {
            (v.nav().focused(), v.view_search().mode(), top_item_of(v))
        });
        keys(cx, handle, &viewer, "enter");
        let after = read(cx, &viewer, |v| {
            (v.nav().focused(), v.view_search().mode(), top_item_of(v))
        });
        assert_eq!(before, after);
        assert_eq!(
            read(cx, &viewer, |v| v.target().unwrap().path.clone()),
            path
        );
    }

    fn top_item_of(v: &ViewerPane) -> usize {
        v.list_state().logical_scroll_top().item_ix
    }

    // ----- search ----------------------------------------------------------------

    /// V-072: typing, Ctrl+U, Enter, Esc.
    #[gpui_kit::test]
    fn v072_search_typing_commit_and_clear(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let lines = vec![user("a needle here"), assistant("another needle")];
        let (handle, viewer, _) = open_viewer(cx, &world, &lines, &[]);
        keys(cx, handle, &viewer, "/");
        assert_eq!(
            read(cx, &viewer, |v| v.view_search().mode()),
            ViewSearchMode::Typing
        );
        cx.simulate_input(handle, "nee");
        keys(cx, handle, &viewer, "ctrl-u");
        assert_eq!(
            read(cx, &viewer, |v| v.view_search().query().to_string()),
            ""
        );
        cx.simulate_input(handle, "needle");
        keys(cx, handle, &viewer, "enter");
        read(cx, &viewer, |v| {
            assert_eq!(v.view_search().mode(), ViewSearchMode::Active);
            assert_eq!(v.view_search().count(), 2);
        });
        keys(cx, handle, &viewer, "escape");
        read(cx, &viewer, |v| {
            // First Esc leaves navigation (the search focused a message),
            // the next clears the committed search.
            assert!(!v.nav().is_active());
        });
        keys(cx, handle, &viewer, "escape");
        read(cx, &viewer, |v| {
            assert_eq!(v.view_search().mode(), ViewSearchMode::Off);
            assert_eq!(v.view_search().query(), "");
        });
    }

    /// V-073: only body text matches; the first match's message is focused.
    #[gpui_kit::test]
    fn v073_search_matches_body_text_only(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let lines = vec![
            user("nothing here"),
            assistant("plain"),
            user("Ask Claude twice: Claude"),
        ];
        let (handle, viewer, _) = open_viewer(cx, &world, &lines, &[]);
        keys(cx, handle, &viewer, "/");
        cx.simulate_input(handle, "Claude");
        wait_viewer_ready(cx, handle, &viewer);
        read(cx, &viewer, |v| {
            assert_eq!(v.view_search().count(), 2, "label lanes are not searched");
            assert_eq!(v.nav().focused(), Some(2));
            assert!(v.nav().is_active());
        });
    }

    /// V-074: n/N wrap; a visible match does not scroll.
    #[gpui_kit::test]
    fn v074_next_prev_wrap(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let lines = vec![user("needle"), assistant("needle"), user("needle")];
        let (handle, viewer, _) = open_viewer(cx, &world, &lines, &[]);
        keys(cx, handle, &viewer, "/");
        cx.simulate_input(handle, "needle");
        keys(cx, handle, &viewer, "enter");
        let current =
            |cx: &mut TestAppContext| read(cx, &viewer, |v| v.view_search().current_index());
        assert_eq!(current(cx), 0);
        keys(cx, handle, &viewer, "n");
        keys(cx, handle, &viewer, "n");
        assert_eq!(current(cx), 2);
        keys(cx, handle, &viewer, "n");
        assert_eq!(current(cx), 0, "wraps to 1/3");
        assert_eq!(top_item(cx, &viewer), 0, "visible matches never scroll");
        keys(cx, handle, &viewer, "shift-n");
        assert_eq!(current(cx), 2, "N from 1/3 goes to 3/3");
        // The desktop alias of `n`: F3, on macOS Cmd+G.
        let next_alias = if cfg!(target_os = "macos") {
            "cmd-g"
        } else {
            "f3"
        };
        keys(cx, handle, &viewer, next_alias);
        assert_eq!(current(cx), 0);
    }

    /// V-076: a rebuild keeps an active search and clamps the current match.
    #[gpui_kit::test]
    fn v076_rebuild_refreshes_and_clamps_matches(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let lines = vec![
            user("needle one"),
            bash_call("b", "echo", 1),
            tool_result("b", "needle in output", 1),
        ];
        let (handle, viewer, _) = open_viewer(cx, &world, &lines, &["--show-tools"]);
        keys(cx, handle, &viewer, "/");
        cx.simulate_input(handle, "needle");
        keys(cx, handle, &viewer, "enter");
        keys(cx, handle, &viewer, "shift-n");
        assert_eq!(
            read(cx, &viewer, |v| (
                v.view_search().current_index(),
                v.view_search().count()
            )),
            (1, 2)
        );
        // Full → Summary: the output disappears.
        keys(cx, handle, &viewer, "t");
        read(cx, &viewer, |v| {
            assert_eq!(v.view_search().mode(), ViewSearchMode::Active);
            assert_eq!(v.view_search().count(), 1);
            assert_eq!(v.view_search().current_index(), 0);
        });
    }

    /// V-078: a match only in hidden lines expands the output and is visible.
    #[gpui_kit::test]
    fn v078_hidden_match_auto_expands(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let mut body = numbered_lines(30);
        body.push_str("\nthe hidden treasure");
        let mut lines = long_conversation(30);
        lines.push(bash_call("b", "cat", 40));
        lines.push(tool_result("b", &body, 40));
        let (handle, viewer, _) = open_viewer(cx, &world, &lines, &[]);
        keys(cx, handle, &viewer, "t");
        keys(cx, handle, &viewer, "/");
        cx.simulate_input(handle, "treasure");
        wait_viewer_ready(cx, handle, &viewer);
        draw(cx, handle);
        wait_viewer_ready(cx, handle, &viewer);
        let result = item_index(cx, &viewer, |i| i.kind == ItemKind::ToolResult);
        read(cx, &viewer, |v| {
            assert!(v.items().unwrap()[result].expanded, "auto-expanded");
            let m = v.view_search().current().unwrap().clone();
            assert!(m.hidden.is_none());
            assert_eq!(m.item_ix, result);
            assert!(v.list_state().item_is_above_viewport(result) == Some(false));
            assert!(v.list_state().item_is_below_viewport(result) == Some(false));
        });
    }

    /// V-099: a match in chunk 30 of a big output is revealed.
    #[gpui_kit::test]
    fn v099_match_in_a_late_chunk_is_revealed(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let body: String = (1..=5000).map(|n| format!("output line {n}\n")).collect();
        let lines = vec![
            user("q"),
            bash_call("b", "seq", 1),
            tool_result("b", &body, 1),
        ];
        let (handle, viewer, _) = open_viewer(cx, &world, &lines, &["--show-tools"]);
        keys(cx, handle, &viewer, "/");
        cx.simulate_input(handle, "output line 2950");
        wait_viewer_ready(cx, handle, &viewer);
        draw(cx, handle);
        wait_viewer_ready(cx, handle, &viewer);
        read(cx, &viewer, |v| {
            let m = v.view_search().current().unwrap().clone();
            let item = &v.items().unwrap()[m.item_ix];
            assert!(item.id.chunk == 29, "chunk {}", item.id.chunk);
            let bounds = v.list_state().bounds_for_item(m.item_ix).expect("rendered");
            let viewport = v.list_state().viewport_bounds();
            assert!(bounds.top() < viewport.bottom() && bounds.bottom() > viewport.top());
        });
    }

    /// V-102: the search index is lazy and dropped on open.
    #[gpui_kit::test]
    fn v102_search_index_is_lazy(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, ws, viewer, _) = open_main_viewer(cx, &world, &long_conversation(4), &[]);
        assert!(!read(cx, &viewer, |v| v.has_search_index()));
        keys(cx, handle, &viewer, "/");
        cx.simulate_input(handle, "message");
        wait_viewer_ready(cx, handle, &viewer);
        assert!(read(cx, &viewer, |v| v.has_search_index()));
        let other = write_transcript(&world.home().join("o"), "o.jsonl", &long_conversation(2));
        let viewer = open_in_main(cx, handle, &ws, &other, false);
        assert!(!read(cx, &viewer, |v| v.has_search_index()));
    }

    // ----- Esc, q -------------------------------------------------------------------

    /// V-077: the Esc order, then leave; a single-file window closes.
    #[gpui_kit::test]
    fn v077_escape_order(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let lines = vec![user("needle"), assistant("needle"), user("x")];
        let (handle, _ws, viewer, _) = open_main_viewer(cx, &world, &lines, &[]);
        let log = events(cx, &viewer);
        keys(cx, handle, &viewer, "/");
        cx.simulate_input(handle, "needle");
        keys(cx, handle, &viewer, "enter");
        keys(cx, handle, &viewer, "/");
        cx.simulate_input(handle, "x");
        wait_viewer_ready(cx, handle, &viewer);
        // 1: the search being typed.
        keys(cx, handle, &viewer, "escape");
        assert_eq!(
            read(cx, &viewer, |v| v.view_search().mode()),
            ViewSearchMode::Off
        );
        assert!(read(cx, &viewer, |v| v.nav().is_active()));
        // 2: navigation.
        keys(cx, handle, &viewer, "escape");
        assert!(!read(cx, &viewer, |v| v.nav().is_active()));
        // Committed search, then 3: cleared.
        keys(cx, handle, &viewer, "/");
        cx.simulate_input(handle, "needle");
        keys(cx, handle, &viewer, "enter");
        keys(cx, handle, &viewer, "escape");
        keys(cx, handle, &viewer, "escape");
        assert_eq!(
            read(cx, &viewer, |v| v.view_search().mode()),
            ViewSearchMode::Off
        );
        assert!(
            log.borrow()
                .iter()
                .all(|e| *e != ViewerPaneEvent::LeaveRequested)
        );
        // 4: leave.
        keys(cx, handle, &viewer, "escape");
        assert!(log.borrow().contains(&ViewerPaneEvent::LeaveRequested));

        let world2 = TestWorld::new();
        let mut cx2 = cx.new_app();
        let (single, _, _) = open_viewer(&mut cx2, &world2, &long_conversation(2), &[]);
        cx2.simulate_keystrokes(single, "escape");
        cx2.run_until_parked();
        assert!(
            cx2.update_window(single, |_, _, _| ()).is_err(),
            "the window closed"
        );
    }

    /// V-084: `q` leaves without unwinding navigation or search.
    #[gpui_kit::test]
    fn v084_q_leaves_without_unwinding(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let lines = vec![user("needle"), assistant("needle")];
        let (handle, _ws, viewer, _) = open_main_viewer(cx, &world, &lines, &[]);
        let log = events(cx, &viewer);
        keys(cx, handle, &viewer, "/");
        cx.simulate_input(handle, "needle");
        keys(cx, handle, &viewer, "enter");
        assert!(read(cx, &viewer, |v| v.nav().is_active()));
        keys(cx, handle, &viewer, "q");
        assert!(log.borrow().contains(&ViewerPaneEvent::LeaveRequested));
        read(cx, &viewer, |v| {
            assert!(v.nav().is_active());
            assert_eq!(v.view_search().mode(), ViewSearchMode::Active);
        });
    }

    // ----- copy, selection, links ----------------------------------------------

    fn drag(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        from: Point<gpui_kit::Pixels>,
        to: Point<gpui_kit::Pixels>,
    ) {
        cx.update_window(handle, |_, window, cx| window.drag(from, to, cx))
            .unwrap();
        cx.run_until_parked();
    }

    /// V-085 / V-088: selection copy and the partial-copy toast.
    #[gpui_kit::test]
    fn v085_v088_smart_copy_of_the_transcript_selection(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, viewer, _) = open_viewer(cx, &world, &long_conversation(60), &[]);
        let start = text_position(cx, handle, &viewer, 0, 0, 0);
        let end = text_position(cx, handle, &viewer, 2, 0, 10);
        drag(
            cx,
            handle,
            start + point(px(1.), px(4.)),
            end + point(px(1.), px(4.)),
        );
        keys(cx, handle, &viewer, "secondary-c");
        let copied = recording(cx).clipboard().unwrap_or_default();
        assert!(copied.contains("Message 1 body."), "{copied:?}");
        assert!(copied.contains("Message 2"), "{copied:?}");
        assert_eq!(
            cx.read(crate::exit::quit_requested),
            None,
            "Ctrl+C never quits"
        );
        // Scroll the selection start out of the painted range.
        viewer.update(cx, |v, _| {
            v.list_state().scroll_to(gpui_kit::ListOffset {
                item_ix: 1,
                offset_in_item: px(0.),
            })
        });
        draw(cx, handle);
        draw(cx, handle);
        recording(cx).set_clipboard(None);
        keys(cx, handle, &viewer, "secondary-c");
        let toast = cx.read(last_toast).to_string();
        assert_eq!(toast, PARTIAL_COPY_TOAST);
        let copied = recording(cx).clipboard().unwrap_or_default();
        assert!(copied.contains("Message 2"), "{copied:?}");
    }

    /// V-088: Ctrl+A selects the whole transcript (also what is not painted)
    /// and Ctrl+C copies all of it; Esc, a click or a drag ends it, and the
    /// Edit menu lists Select All.
    #[gpui_kit::test]
    fn v088_select_all_then_copy_takes_the_whole_transcript(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, viewer, _) = open_viewer(cx, &world, &long_conversation(60), &[]);
        keys(cx, handle, &viewer, "secondary-a");
        assert!(read(cx, &viewer, |v| v.select_all), "Ctrl+A selects all");
        draw(cx, handle);
        keys(cx, handle, &viewer, "secondary-c");
        let copied = recording(cx).clipboard().unwrap_or_default();
        assert!(copied.contains("Message 1 body."), "{copied:?}");
        assert!(
            copied.contains("Message 60 body."),
            "items that are not painted are copied too"
        );
        assert!(copied.contains("Second paragraph of message 30."));
        assert_eq!(copied, read(cx, &viewer, |v| v.all_text()));
        let painted = read(cx, &viewer, |v| v.painted_items().len());
        assert!(painted < 60, "{painted} of 60 painted: a real subset");
        // Esc ends it (and does not leave the viewer).
        keys(cx, handle, &viewer, "escape");
        assert!(!read(cx, &viewer, |v| v.select_all));
        // A drag replaces it.
        keys(cx, handle, &viewer, "secondary-a");
        draw(cx, handle);
        let start = text_position(cx, handle, &viewer, 0, 0, 0);
        let end = text_position(cx, handle, &viewer, 0, 0, 7);
        drag(
            cx,
            handle,
            start + point(px(1.), px(4.)),
            end + point(px(1.), px(4.)),
        );
        draw(cx, handle);
        assert!(!read(cx, &viewer, |v| v.select_all), "a drag ends it");
        recording(cx).set_clipboard(None);
        keys(cx, handle, &viewer, "secondary-c");
        let copied = recording(cx).clipboard().unwrap_or_default();
        assert!(copied.len() < 40, "only the dragged text: {copied:?}");
        // The Edit menu's Select All dispatches the same action.
        keys(cx, handle, &viewer, "escape");
        cx.update_window(handle, |_, window, cx| {
            viewer.update(cx, |v, cx| v.focus(window, cx));
            window.dispatch_action(Box::new(SelectAllTranscript), cx);
        })
        .unwrap();
        wait_viewer_ready(cx, handle, &viewer);
        assert!(read(cx, &viewer, |v| v.select_all), "Edit > Select All");
    }

    /// V-045: a table with CJK and emoji cells keeps its columns aligned: the
    /// cells are laid out on a fixed grid, not by padding with spaces.
    #[test]
    fn v045_wide_glyph_tables_use_the_cell_grid() {
        use crate::viewer::segments::{self, SegKind};
        let segs = segments::markdown(
            "| a | b |
|---|---|
| 日本語 | 東京 |
| ✅ ok | x |",
        );
        let grids: Vec<bool> = segs
            .iter()
            .map(|s| matches!(s.kind, SegKind::TableRow { grid: true, .. }))
            .collect();
        assert_eq!(grids, [true, true, true], "every row of the table");
        let SegKind::TableRow { cells, widths, .. } = &segs[1].kind else {
            panic!()
        };
        let ranges = segments::table_cell_ranges(cells, widths, segs[1].text.len());
        assert_eq!(&segs[1].text[ranges[0].clone()], "日本語");
        assert_eq!(&segs[1].text[ranges[1].clone()], "東京");
        let ascii = segments::markdown(
            "| a | b |
|---|---|
| c | d |",
        );
        assert!(
            ascii
                .iter()
                .all(|s| matches!(s.kind, SegKind::TableRow { grid: false, .. })),
            "ASCII tables keep the padded text"
        );
    }

    /// V-045 (G): in the painted transcript, every column starts at the same
    /// x in every row of a table with CJK and emoji cells.
    #[gpui_kit::test]
    fn v045_wide_glyph_columns_start_at_the_same_x(cx: &mut TestAppContext) {
        use crate::viewer::segments::{self, SegKind};
        let world = TestWorld::new();
        let table = "| name | city | state |
|---|---|---|
| ascii | Tokyo | ok |
| 日本語テキスト | 東京 | ✅ ok |
| 한국어 | 서울 | 🙂 |";
        let (handle, viewer, _) = open_viewer(cx, &world, &[user("q"), assistant(table)], &[]);
        let item = item_index(cx, &viewer, |i| {
            i.segments
                .iter()
                .any(|s| matches!(s.kind, SegKind::TableRow { .. }))
        });
        let rows: Vec<(usize, Vec<usize>)> = read(cx, &viewer, |v| {
            v.items().unwrap()[item]
                .segments
                .iter()
                .enumerate()
                .filter_map(|(ix, s)| match &s.kind {
                    SegKind::TableRow { cells, widths, .. } => Some((
                        ix,
                        segments::table_cell_ranges(cells, widths, s.text.len())
                            .into_iter()
                            .map(|r| r.start)
                            .collect(),
                    )),
                    _ => None,
                })
                .collect()
        });
        assert_eq!(rows.len(), 4);
        let mut starts: Vec<Vec<f32>> = Vec::new();
        for (segment, bytes) in &rows {
            starts.push(
                bytes
                    .iter()
                    .map(|byte| {
                        f32::from(text_position(cx, handle, &viewer, item, *segment, *byte).x)
                    })
                    .collect(),
            );
        }
        for column in 0..3 {
            let first = starts[0][column];
            for (row, row_starts) in starts.iter().enumerate() {
                assert!(
                    (row_starts[column] - first).abs() < 0.5,
                    "column {column} of row {row} starts at {} not {first}: {starts:?}",
                    row_starts[column]
                );
            }
        }
    }

    /// V-041: a click on a link opens "Open link?".
    #[gpui_kit::test]
    fn v041_link_click_opens_the_link_dialog(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let lines = vec![user("[a link to the docs](https://example.com/docs)")];
        let (handle, viewer, _) = open_viewer(cx, &world, &lines, &[]);
        // No dialog animation: button positions are stable between frames.
        cx.update(|cx| cx.set_reduce_motion(true));
        let position = text_position(cx, handle, &viewer, 0, 0, 3);
        click_at(cx, handle, position + point(px(1.), px(4.)));
        let has_dialog = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| window.has_active_dialog(cx))
                .unwrap()
        };
        assert!(has_dialog(cx), "Open link? dialog");
        cx.update_window(handle, |_, window, cx| window.click("link-open", cx))
            .unwrap();
        cx.run_until_parked();
        assert!(
            recording(cx)
                .calls()
                .contains(&ServiceCall::OpenUrl("https://example.com/docs".into()))
        );
        assert!(!has_dialog(cx));
        click_at(cx, handle, position + point(px(1.), px(4.)));
        cx.update_window(handle, |_, window, cx| window.click("link-copy", cx))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            recording(cx).clipboard().as_deref(),
            Some("https://example.com/docs")
        );
    }

    /// V-017: hovering an expandable output marks its id.
    #[gpui_kit::test]
    fn v017_hover_marks_the_tool_output(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, viewer, _) = open_viewer(cx, &world, &truncated_fixture(), &[]);
        keys(cx, handle, &viewer, "t");
        let call = item_index(cx, &viewer, |i| i.kind == ItemKind::ToolCall);
        let id = read(cx, &viewer, |v| v.items().unwrap()[call].toggle.clone());
        let over_call = text_position(cx, handle, &viewer, call, 0, 2);
        move_mouse(cx, handle, over_call + point(px(1.), px(4.)));
        cx.run_until_parked();
        assert_eq!(read(cx, &viewer, |v| v.hovered().cloned()), id);
        let over_text = text_position(cx, handle, &viewer, 0, 0, 0);
        move_mouse(cx, handle, over_text + point(px(1.), px(4.)));
        cx.run_until_parked();
        assert_eq!(read(cx, &viewer, |v| v.hovered().cloned()), None);
    }

    /// V-018: long lines wrap (no clipping).
    #[gpui_kit::test]
    fn v018_long_lines_wrap(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let long = "word ".repeat(60);
        let (handle, viewer, _) = open_viewer(cx, &world, &[user("short"), assistant(&long)], &[]);
        draw(cx, handle);
        let (short, tall) = read(cx, &viewer, |v| {
            let list = v.list_state();
            (
                list.bounds_for_item(0).unwrap().size.height,
                list.bounds_for_item(1).unwrap().size.height,
            )
        });
        assert!(tall > short * 1.6, "{tall:?} vs {short:?}");
    }

    /// V-018: a table wider than the content column scrolls horizontally
    /// (its rows move left under a horizontal wheel; prose does not).
    #[gpui_kit::test]
    fn v018_wide_tables_scroll_horizontally(cx: &mut TestAppContext) {
        use crate::viewer::segments::SegKind;
        let world = TestWorld::new();
        let wide = format!(
            "| key | value |\n|---|---|\n| first | {} |\n| second | end |",
            "x".repeat(600)
        );
        let (handle, viewer, _) = open_viewer(cx, &world, &[user("q"), assistant(&wide)], &[]);
        let item = item_index(cx, &viewer, |i| {
            i.segments
                .iter()
                .any(|s| matches!(s.kind, SegKind::TableRow { .. }))
        });
        let row = read(cx, &viewer, |v| {
            v.items().unwrap()[item]
                .segments
                .iter()
                .rposition(|s| matches!(s.kind, SegKind::TableRow { .. }))
                .unwrap()
        });
        let before = text_position(cx, handle, &viewer, item, row, 0);
        cx.update_window(handle, |_, window, cx| {
            window.dispatch_event(
                gpui_kit::ScrollWheelEvent {
                    position: point(before.x + px(20.), before.y + px(4.)),
                    delta: gpui_kit::ScrollDelta::Pixels(point(px(-300.), px(0.))),
                    modifiers: Modifiers::default(),
                    touch_phase: gpui_kit::TouchPhase::Moved,
                }
                .to_platform_input(),
                cx,
            );
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
        let after = text_position(cx, handle, &viewer, item, row, 0);
        assert!(
            after.x < before.x - px(100.),
            "the table scrolled left: {before:?} → {after:?}"
        );
        assert_eq!(after.y, before.y, "no vertical movement");
    }

    /// V-089: with the export menu open (`e`), a real click on the
    /// `(7 more lines…)` control of the transcript does not expand it; the
    /// same click after Esc does (integrator, Phase C: the real menu and a
    /// pointer click instead of a direct call).
    #[gpui_kit::test]
    fn v089_dialogs_block_transcript_clicks(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, _ws, viewer, _) = open_main_viewer(cx, &world, &truncated_fixture(), &[]);
        cx.update(|cx| cx.set_reduce_motion(true));
        keys(cx, handle, &viewer, "t");
        settle(cx, handle, &viewer);
        let call = item_index(cx, &viewer, |i| i.kind == ItemKind::ToolCall);
        let (more, hash) = read(cx, &viewer, |v| {
            let item = &v.items().unwrap()[call];
            (item.more, item.key_hash)
        });
        assert_eq!(more, Some(7));
        let more_id = gpui_kit::ElementId::NamedInteger("more".into(), hash);
        let expanded =
            |cx: &mut TestAppContext| read(cx, &viewer, |v| v.model().unwrap().expanded.len());

        keys(cx, handle, &viewer, "e");
        let open = cx
            .update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.has_active_dialog(cx)
            })
            .unwrap();
        assert!(open, "the export menu is open");
        assert_eq!(
            cx.read(|cx| crate::workspace::startup::last_dialog(cx).to_string()),
            "export-menu"
        );
        cx.update_window(handle, |_, window, cx| window.click(more_id.clone(), cx))
            .unwrap();
        wait_viewer_ready(cx, handle, &viewer);
        assert_eq!(expanded(cx), 0, "the modal menu took the click");
        // The click landed on the dialog's overlay (which may dismiss it).
        let still_open = cx
            .update_window(handle, |_, window, cx| window.has_active_dialog(cx))
            .unwrap();
        if still_open {
            cx.simulate_keystrokes(handle, "escape");
            cx.run_until_parked();
        }
        draw(cx, handle);
        cx.update_window(handle, |_, window, cx| window.click(more_id.clone(), cx))
            .unwrap();
        wait_viewer_ready(cx, handle, &viewer);
        assert_eq!(expanded(cx), 1, "without the menu the click expands");
    }

    // ----- toolbar, agent panel, empty states --------------------------------------

    /// V-096: each toolbar control dispatches the same action as its key —
    /// the control's click and the key bound to its action in the transcript
    /// add the same entry to the action log and have the same effect
    /// (integrator, Phase C: Resume and Fork included, keys looked up from
    /// the live keymap; the tool display buttons have no key and compare
    /// with their View menu actions).
    #[gpui_kit::test]
    fn v096_toolbar_dispatches_the_key_actions(cx: &mut TestAppContext) {
        use crate::test_support::fixtures_ops::{OpsFixture, claude_session, write_gui_config};
        let world = TestWorld::new();
        write_gui_config(&world, "", "");
        let path = claude_session(&world, "proj", 1, "Toolbar");
        let f = OpsFixture::open(cx, &world, &[]);
        let handle = f.handle();
        // Toasts leave at once (they cover the toolbar's right end).
        cx.update(|cx| cx.set_reduce_motion(true));
        let viewer = f.open_in_viewer(cx, &path);
        let log_len = |cx: &mut TestAppContext| read(cx, &viewer, |v| v.action_log().len());
        let log_since = |cx: &mut TestAppContext, from: usize| {
            read(cx, &viewer, |v| v.action_log()[from..].to_vec())
        };
        let controls: Vec<(&str, Box<dyn gpui_kit::Action>)> = vec![
            ("tools-truncated", Box::new(SetToolsTruncated)),
            ("tools-full", Box::new(SetToolsFull)),
            ("tools-summary", Box::new(SetToolsSummary)),
            ("toggle-thinking", Box::new(ToggleThinking)),
            ("toggle-timing", Box::new(ToggleTiming)),
            ("toolbar-fork", Box::new(Fork)),
            ("toolbar-resume", Box::new(Resume)),
            ("toolbar-outline", Box::new(ToggleOutline)),
        ];
        let mut keyed = Vec::new();
        for (id, action) in controls {
            let name = action.name();
            // The click.
            let from = log_len(cx);
            let terminal_before = f.terminal_commands().len();
            // A launch toast sits over the toolbar's right end: dismiss it and
            // let it leave before the click.
            cx.update_window(handle, |_, window, cx| window.clear_notifications(cx))
                .unwrap();
            cx.executor().advance_clock(Duration::from_secs(2));
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.click(id, cx)
            })
            .unwrap();
            f.settle(cx);
            wait_viewer_ready(cx, handle, &viewer);
            // Outline is logged by the session route and the panel toggle.
            let clicked = log_since(cx, from);
            assert!(
                !clicked.is_empty() && clicked.iter().all(|entry| *entry == name),
                "{id}: {clicked:?}"
            );
            let clicked_launches = f.terminal_commands().len() - terminal_before;

            // The key bound to the action in the transcript (or the menu
            // action when the control has no key).
            cx.update_window(handle, |_, window, cx| {
                viewer.update(cx, |viewer, cx| viewer.focus(window, cx));
                window.render_frame(cx);
            })
            .unwrap();
            let binding = cx
                .update_window(handle, |_, window, cx| {
                    let focus = viewer.read(cx).focus_handle(cx);
                    window
                        .highest_precedence_binding_for_action_in(action.as_ref(), &focus)
                        .map(|binding| {
                            binding
                                .keystrokes()
                                .iter()
                                .map(|k| k.inner().unparse())
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                })
                .unwrap();
            let from = log_len(cx);
            let terminal_before = f.terminal_commands().len();
            match &binding {
                Some(keys) => {
                    keyed.push((id, keys.clone()));
                    cx.simulate_keystrokes(handle, keys);
                }
                None => {
                    let action = action.boxed_clone();
                    cx.update_window(handle, |_, window, cx| window.dispatch_action(action, cx))
                        .unwrap();
                }
            }
            f.settle(cx);
            wait_viewer_ready(cx, handle, &viewer);
            assert_eq!(log_since(cx, from), clicked, "{id} vs {binding:?}");
            assert_eq!(
                f.terminal_commands().len() - terminal_before,
                clicked_launches,
                "{id}: the same launches"
            );
        }
        assert!(
            keyed.len() >= 5,
            "thinking, timing, resume, fork, outline have keys: {keyed:?}"
        );
        let commands = f.terminal_commands();
        assert_eq!(
            commands.len(),
            4,
            "resume and fork, by click and by key: {commands:?}"
        );
    }

    /// A-055: the toolbar's ⋯ dropdown is the real PopupMenu: opened by a
    /// click, an item picked with the keyboard (Down, Enter) reaches the
    /// viewer through `action_context` as the same action the key or the
    /// Session menu dispatches, with the same effect.
    #[gpui_kit::test]
    fn a055_toolbar_more_menu_items_reach_the_viewer(cx: &mut TestAppContext) {
        use crate::test_support::fixtures_ops::{OpsFixture, claude_session, write_gui_config};
        let world = TestWorld::new();
        write_gui_config(&world, "", "");
        let path = claude_session(&world, "proj", 1, "More menu");
        let f = OpsFixture::open(cx, &world, &[]);
        let handle = f.handle();
        cx.update(|cx| cx.set_reduce_motion(true));
        let viewer = f.open_in_viewer(cx, &path);
        let log_len = |cx: &mut TestAppContext| read(cx, &viewer, |v| v.action_log().len());
        let log_since = |cx: &mut TestAppContext, from: usize| {
            read(cx, &viewer, |v| v.action_log()[from..].to_vec())
        };
        let dialog = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| window.has_active_dialog(cx))
                .unwrap()
        };
        let open_menu = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| window.clear_notifications(cx))
                .unwrap();
            cx.executor().advance_clock(Duration::from_secs(2));
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.click("toolbar-more", cx)
            })
            .unwrap();
            f.settle(cx);
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .unwrap();
        };
        // Position in the menu (clickable items, the submenu counted), the
        // action it dispatches, and whether it opens a dialog.
        let items: [(usize, &str, bool); 9] = [
            (1, "history::ExportConversation", true),
            (3, "history::CopyFocusedMessage", false),
            (4, "history::CopyPath", false),
            (5, "history::CopySessionId", false),
            (6, "history::ShowPath", false),
            (7, "history::RevealInFileManager", false),
            (8, "history::Rename", true),
            (9, "history::SearchWithinConversation", false),
            (11, "history::Delete", true),
        ];
        for (downs, name, opens_dialog) in items {
            // The same action dispatched directly (what its key does).
            cx.update_window(handle, |_, window, cx| {
                viewer.update(cx, |viewer, cx| viewer.focus(window, cx))
            })
            .unwrap();
            let action = cx.update(|cx| cx.build_action(name, None).unwrap());
            let from = log_len(cx);
            let writes_before = f.clipboard_writes().len();
            cx.update_window(handle, |_, window, cx| {
                window.dispatch_action(action.boxed_clone(), cx)
            })
            .unwrap();
            f.settle(cx);
            let direct = log_since(cx, from);
            let direct_writes = f.clipboard_writes().len() - writes_before;
            let direct_dialog = dialog(cx);
            if direct_dialog {
                cx.simulate_keystrokes(handle, "escape");
                f.settle(cx);
            }

            // The menu: click ⋯, move down to the item, confirm.
            open_menu(cx);
            for _ in 0..downs {
                cx.simulate_keystrokes(handle, "down");
            }
            let from = log_len(cx);
            let writes_before = f.clipboard_writes().len();
            cx.simulate_keystrokes(handle, "enter");
            f.settle(cx);
            wait_viewer_ready(cx, handle, &viewer);
            let picked = log_since(cx, from);
            assert_eq!(picked, direct, "{name}: menu vs key");
            assert!(
                picked.iter().all(|entry| *entry == name),
                "{name}: {picked:?}"
            );
            assert!(!picked.is_empty(), "{name} reached the viewer");
            assert_eq!(
                f.clipboard_writes().len() - writes_before,
                direct_writes,
                "{name}: the same clipboard effect"
            );
            assert_eq!(dialog(cx), direct_dialog, "{name}");
            assert_eq!(direct_dialog, opens_dialog, "{name} dialog");
            if dialog(cx) {
                cx.simulate_keystrokes(handle, "escape");
                f.settle(cx);
            }
        }
    }

    /// V-097: `o` shows and focuses the agent panel; `o` again hides it.
    #[gpui_kit::test]
    fn v097_outline_panel_toggles_with_o(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, _ws, viewer, _) = open_main_viewer(cx, &world, &long_conversation(3), &[]);
        let panel = read(cx, &viewer, |v| v.agent_panel().clone());
        keys(cx, handle, &viewer, "o");
        cx.update_window(handle, |_, window, cx| {
            assert!(panel.read(cx).is_visible());
            assert!(panel.read(cx).contains_focus(window, cx));
        })
        .unwrap();
        keys(cx, handle, &viewer, "o");
        cx.update_window(handle, |_, window, cx| {
            assert!(!panel.read(cx).is_visible());
            assert!(
                viewer
                    .read(cx)
                    .focus_handle(cx)
                    .contains_focused(window, cx)
            );
        })
        .unwrap();
    }

    /// V-098: each state shows its text — `No conversation selected` (with
    /// its hint), `Loading conversation…` while the open runs, `This
    /// conversation has no visible messages` — and the last one's Show
    /// thinking button toggles thinking (integrator, Phase C: the texts are
    /// the ones `render_transcript` draws, `ViewerPane::transcript_state`).
    #[gpui_kit::test]
    fn v098_empty_states(cx: &mut TestAppContext) {
        use crate::viewer::TranscriptState;
        let world = TestWorld::new();
        let (handle, ws) = crate::test_support::open_main(cx, &world, &[]);
        let viewer = cx.read(|cx| ws.read(cx).viewer().clone());
        draw(cx, handle);
        assert_eq!(
            cx.read(|cx| viewer.read(cx).qa_probe("viewer.status", cx))
                .as_deref(),
            Some("empty")
        );
        let state = read(cx, &viewer, |v| v.transcript_state()).unwrap();
        assert_eq!(state, TranscriptState::NothingSelected);
        assert_eq!(state.title(), "No conversation selected");
        assert_eq!(
            state.description(),
            Some("Select a conversation on the left, or press Enter.")
        );

        let only_thinking = vec![assistant_blocks(
            r#"[{"type":"thinking","thinking":"just thinking","signature":""}]"#,
            1,
        )];
        let path = write_transcript(&world.home().join("t"), "thinking.jsonl", &only_thinking);
        // Loading: right after the open, before the background parse lands.
        let loading = cx
            .update_window(handle, |_, window, cx| {
                viewer.update(cx, |v, cx| {
                    v.open(
                        OpenRequest::new(ConversationTarget::from_parsed_file(&path, None), true),
                        window,
                        cx,
                    );
                    v.transcript_state()
                })
            })
            .unwrap()
            .unwrap();
        assert_eq!(loading, TranscriptState::Loading);
        assert_eq!(loading.title(), "Loading conversation…");
        wait_viewer_ready(cx, handle, &viewer);

        assert_eq!(read(cx, &viewer, |v| v.items().unwrap().len()), 0);
        let hidden = read(cx, &viewer, |v| v.transcript_state()).unwrap();
        assert_eq!(hidden, TranscriptState::NoVisibleMessages);
        assert_eq!(hidden.title(), "This conversation has no visible messages");
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("viewer-show-tools").is_some());
            window.click("viewer-show-thinking", cx)
        })
        .unwrap();
        wait_viewer_ready(cx, handle, &viewer);
        assert!(read(cx, &viewer, |v| v.prefs().unwrap().show_thinking));
        assert_eq!(read(cx, &viewer, |v| v.items().unwrap().len()), 1);
        assert_eq!(
            read(cx, &viewer, |v| v.transcript_state()),
            None,
            "rows now"
        );
    }

    // ----- single-file windows -------------------------------------------------

    /// V-090 / S-033: single-file windows have no session actions or modes.
    #[gpui_kit::test]
    fn v090_s033_single_file_window_restrictions(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, viewer, _) = open_viewer(cx, &world, &long_conversation(3), &[]);
        recording(cx).clear_calls();
        keys(cx, handle, &viewer, "ctrl-x");
        keys(cx, handle, &viewer, "ctrl-r");
        cx.update_window(handle, |_, window, cx| {
            assert!(!window.has_active_dialog(cx));
            for action in [
                Box::new(Resume) as Box<dyn gpui_kit::Action>,
                Box::new(Fork),
                Box::new(Rename),
                Box::new(Delete),
                Box::new(SetModeLexical),
                Box::new(SetModeSemantic),
            ] {
                assert!(
                    !window.is_action_available(action.as_ref(), cx),
                    "{}",
                    action.name()
                );
            }
            assert!(window.try_find("mode-lexical").is_none());
        })
        .unwrap();
        assert!(recording(cx).calls().is_empty());
        cx.simulate_keystrokes(handle, "escape");
        cx.run_until_parked();
        assert!(cx.update_window(handle, |_, _, _| ()).is_err());
    }

    /// V-091: unreadable or empty single files show their reason.
    #[gpui_kit::test]
    fn v091_single_file_errors(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let empty = write_transcript(
            &world.home().join("e"),
            "empty.jsonl",
            &[r#"{"type":"summary","summary":"s","leafUuid":"x"}"#.to_string()],
        );
        let handle = open_single_file(cx, &world, &empty);
        let viewer = single_file_viewer(cx, handle);
        wait_viewer_ready(cx, handle, &viewer);
        assert_eq!(
            read(cx, &viewer, |v| v.file_error().map(str::to_string)),
            Some(format!(
                "{} contains no conversation messages",
                empty.display()
            ))
        );
        let missing = world.home().join("nope.jsonl");
        let handle = open_single_file(cx, &world, &missing);
        let viewer = single_file_viewer(cx, handle);
        wait_viewer_ready(cx, handle, &viewer);
        assert_eq!(
            read(cx, &viewer, |v| v.file_error().map(str::to_string)),
            Some(format!("File not found: {}", missing.display()))
        );
    }

    /// Single-file headers come from the background parse; the source is learned.
    #[gpui_kit::test]
    fn single_file_header_and_source_from_the_probe(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let paths = world.standard_corpus();
        let handle = open_single_file(cx, &world, &paths[0]);
        let viewer = single_file_viewer(cx, handle);
        wait_viewer_ready(cx, handle, &viewer);
        read(cx, &viewer, |v| {
            assert!(
                v.target().unwrap().can_modify(),
                "source known after parsing"
            );
            let header = v.header_layout().unwrap().text();
            assert!(
                header.contains("2 messages") || header.contains("4 messages"),
                "{header}"
            );
        });
    }

    /// §15.3: a single-file window answers the `viewer.*` probes and `focus`
    /// (the QA scenarios `single-file.qa`, `perf-viewer.qa` run there).
    #[gpui_kit::test]
    fn single_file_window_answers_qa_probes(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let (handle, viewer, path) = open_viewer(cx, &world, &long_conversation(3), &[]);
        let probe = |cx: &mut TestAppContext, key: &str| {
            cx.update_window(handle, |root, window, cx| {
                let root = root
                    .downcast::<gpui_kit::base::Root>()
                    .expect("a gpui-kit root");
                let view = root.read(cx).view().clone();
                let single = view
                    .downcast::<crate::single_file::SingleFileWindow>()
                    .expect("a single-file window");
                single.update(cx, |single, cx| single.qa_probe(key, window, cx))
            })
            .unwrap()
        };
        assert_eq!(
            probe(cx, "viewer.path").as_deref(),
            Some(path.display().to_string().as_str())
        );
        assert_eq!(probe(cx, "viewer.items").as_deref(), Some("3"));
        assert_eq!(probe(cx, "viewer.messages").as_deref(), Some("3"));
        assert_eq!(probe(cx, "viewer.tool_display").as_deref(), Some("sum"));
        assert_eq!(probe(cx, "focus").as_deref(), Some("viewer"));
        assert_eq!(probe(cx, "window.kind").as_deref(), Some("single-file"));
        assert_eq!(probe(cx, "list.count"), None);
        keys(cx, handle, &viewer, "t");
        assert_eq!(probe(cx, "viewer.tool_display").as_deref(), Some("trn"));
    }

    // ----- reveal, theme -------------------------------------------------------

    /// §9.9: reveal by agent ordinal with a visibility hint.
    #[gpui_kit::test]
    fn reveal_by_agent_ordinal_applies_the_hint(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let mut lines = long_conversation(30);
        lines.push(thinking_and_text("secret thought", "final", 40));
        let (handle, _ws, viewer, path) = open_main_viewer(cx, &world, &lines, &[]);
        let reveal = RevealRequest {
            path: path.clone(),
            full_ref: None,
            kind: RevealKind::AgentOrdinal {
                ordinal: 31,
                hint: ContentVisibilityHint {
                    thinking: true,
                    ..Default::default()
                },
            },
        };
        cx.update_window(handle, |_, window, cx| {
            viewer.update(cx, |v, cx| assert!(v.reveal(reveal, window, cx)))
        })
        .unwrap();
        wait_viewer_ready(cx, handle, &viewer);
        read(cx, &viewer, |v| {
            assert!(v.prefs().unwrap().show_thinking, "hint applied");
            assert_eq!(v.nav().focused(), Some(30));
        });
        assert!(
            !cx.read(|cx| crate::globals::app_state(cx).view_defaults.show_thinking),
            "not remembered"
        );
        let missing = RevealRequest {
            path,
            full_ref: None,
            kind: RevealKind::AgentOrdinal {
                ordinal: 999,
                hint: ContentVisibilityHint::default(),
            },
        };
        cx.update_window(handle, |_, window, cx| {
            viewer.update(cx, |v, cx| v.reveal(missing, window, cx))
        })
        .unwrap();
        wait_viewer_ready(cx, handle, &viewer);
        assert_eq!(
            cx.read(last_toast).as_ref(),
            "Message m999 is not shown in the viewer"
        );
    }

    /// V-092 (`global_lock`): a runtime theme switch (the app's path,
    /// `theme_tokens::apply_theme`) empties the syntect cache, and the visible
    /// code is highlighted again with the new theme's palette — the cached
    /// tokens equal core's highlighting in that theme (integrator, Phase C).
    #[gpui_kit::test]
    fn v092_theme_switch_rehighlights_code(cx: &mut TestAppContext) {
        use crate::viewer::code;
        use crate::viewer::segments::SegKind;
        use claude_history_core::theme::{ThemeKind, ThemePreference};
        let _lock = crate::test_support::global_lock();
        let world = TestWorld::new();
        let lines = vec![
            user("q"),
            assistant("```rust\nfn main() { let x = 1; }\n```"),
        ];
        let (handle, viewer, _) = open_viewer(cx, &world, &lines, &[]);
        let switch = |cx: &mut TestAppContext, pref: ThemePreference| {
            cx.update_window(handle, |_, window, cx| {
                crate::theme_tokens::apply_theme(pref, window, cx)
            })
            .unwrap();
            cx.run_until_parked();
        };
        let wait_tokens = |cx: &mut TestAppContext| {
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            while read(cx, &viewer, |v| v.token_cache_len()) == 0 {
                cx.executor().allow_parking();
                draw(cx, handle);
                cx.run_until_parked();
                assert!(std::time::Instant::now() < deadline, "not highlighted");
                std::thread::sleep(Duration::from_millis(3));
            }
            draw(cx, handle);
        };
        // The code segment and what core makes of it in each theme.
        let (code_text, lang) = read(cx, &viewer, |v| {
            v.items()
                .unwrap()
                .iter()
                .flat_map(|item| item.segments.iter())
                .find(|segment| matches!(segment.kind, SegKind::Code))
                .map(|segment| (segment.text.clone(), segment.code_lang.clone().unwrap()))
                .expect("a code segment")
        });
        let expected = |kind: ThemeKind| code::highlight(&code_text, &lang, kind);
        assert_ne!(expected(ThemeKind::Light), expected(ThemeKind::Dark));
        let cached = |cx: &mut TestAppContext| read(cx, &viewer, |v| v.cached_tokens());

        switch(cx, ThemePreference::Light);
        draw(cx, handle);
        wait_tokens(cx);
        let light = cached(cx);
        assert!(!light.is_empty());
        for (key, tokens) in &light {
            assert_eq!(key.2, ThemeKind::Light);
            assert_eq!(*tokens, expected(ThemeKind::Light));
        }

        switch(cx, ThemePreference::Dark);
        // The switch emptied the cache: no light token survives it (the
        // dark batch may already have landed).
        assert!(
            cached(cx).iter().all(|(key, _)| key.2 == ThemeKind::Dark),
            "the switch emptied the cache"
        );
        wait_tokens(cx);
        let dark = cached(cx);
        assert!(!dark.is_empty(), "re-highlighted");
        for (key, tokens) in &dark {
            assert_eq!(key.2, ThemeKind::Dark);
            assert_eq!(*tokens, expected(ThemeKind::Dark), "the dark palette");
        }
        assert_eq!(
            read(cx, &viewer, |v| v.drawn_theme()),
            Some(ThemeKind::Dark)
        );
        switch(cx, ThemePreference::Light);
    }

    /// §15.8 structural precondition: the 3k transcript installs without
    /// O(n²) UI work and toggles stay cheap (debug build, test platform).
    #[gpui_kit::test]
    fn large_transcript_installs_and_toggles(cx: &mut TestAppContext) {
        let world = TestWorld::new();
        let path = perf_transcript(&world, 3000, 6, 5000);
        let handle = open_single_file(cx, &world, &path);
        let viewer = single_file_viewer(cx, handle);
        wait_viewer_ready(cx, handle, &viewer);
        let items = read(cx, &viewer, |v| v.items().unwrap().len());
        assert!(items >= 3000, "{items}");
        keys(cx, handle, &viewer, "shift-g");
        keys(cx, handle, &viewer, "t");
        assert_eq!(tool_display(cx, &viewer), ToolDisplayMode::Truncated);
        let probe = |key: &str, cx: &mut TestAppContext| -> f64 {
            cx.read(|cx| viewer.read(cx).qa_probe(key, cx))
                .unwrap()
                .parse()
                .unwrap()
        };
        let install = probe("viewer.install_ms", cx);
        let toggle = probe("viewer.toggle_ms", cx);
        eprintln!("3k install {install:.1} ms (debug), toggle {toggle:.1} ms");
        assert!(toggle < 50.0, "toggle UI time {toggle} ms");
    }
}
