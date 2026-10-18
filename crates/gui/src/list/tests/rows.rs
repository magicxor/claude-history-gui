//! P tests of the row model and layout (L-011…L-014, L-016…L-022, L-025,
//! L-027, L-028) — no window; core's `Browser` over a sandbox corpus.

use std::sync::Arc;

use chrono::{Duration, Local, TimeZone as _, Utc};
use claude_history_core::browser::{Browser, BrowserOptions};
use claude_history_core::display_format::{HighlightQuery, HighlightedText, Recency};
use claude_history_core::history::load_all_conversations;

use crate::list::rows::{
    self, RowContext, RowModel, TitleKind, build_row_model, fit_text, format_row_time,
    literal_mode, preview_max_chars, row_height_rem,
};
use crate::test_support::TestWorld;
use crate::test_support::fixtures_list::SessionSpec;

fn browser_of(world: &TestWorld) -> Browser {
    let conversations = load_all_conversations(&world.env, true, None).expect("the corpus loads");
    Browser::new(
        conversations,
        BrowserOptions::new(Arc::new(world.env.clone())),
    )
}

/// The row of session `number` (written by `SessionSpec::new(_, number)`).
fn model_of(browser: &Browser, number: u32, query: &str, width_px: f32) -> RowModel {
    let stem = crate::test_support::synth::uuid(number);
    (0..browser.filtered().len())
        .map(|pos| model_at(browser, pos, query, width_px))
        .find(|row| row.stem == stem)
        .unwrap_or_else(|| panic!("session {number} is listed"))
}

fn model_at(browser: &Browser, pos: usize, query: &str, width_px: f32) -> RowModel {
    let highlight = HighlightQuery::parse(query);
    let ctx = RowContext {
        query: &highlight,
        now: Local::now(),
        literal: literal_mode(query),
        multiple_sources: browser.has_multiple_sources(),
        pane_width_px: width_px,
        rem_px: 16.0,
    };
    build_row_model(browser, pos, &ctx).expect("the row exists")
}

/// L-012: quoted literals and `_` identifiers switch to the four-line rows.
#[test]
fn literal_mode_covers_quotes_and_identifiers() {
    assert!(literal_mode("\"x\""));
    assert!(literal_mode("a_b"));
    assert!(!literal_mode("a b"));
    assert!(!literal_mode(""));
    assert_eq!(row_height_rem(false), 4.25);
    assert_eq!(row_height_rem(true), 5.5);
}

/// L-025: the preview budget is the line width over the average character
/// width of `text_xs`, minus 2.
#[test]
fn preview_budget_follows_the_line_width() {
    let line = rows::row_line_width_px(30.0 * 16.0, 16.0, false);
    let avg = rows::PREVIEW_FONT_REM * 16.0 * rows::AVG_CHAR_WIDTH_EM;
    let expected = (line / avg).floor() as usize - 2;
    assert_eq!(preview_max_chars(line, 16.0), expected);
    // A wider pane gets a larger budget, capped at 400.
    assert!(preview_max_chars(line * 2.0, 16.0) > preview_max_chars(line, 16.0));
    assert_eq!(preview_max_chars(100_000.0, 16.0), 400);
    // The lane takes room from the line.
    assert!(
        rows::row_line_width_px(480.0, 16.0, true) < rows::row_line_width_px(480.0, 16.0, false)
    );
}

/// L-022: timestamps of an earlier year carry the year (AD10).
#[test]
fn old_timestamps_show_the_year() {
    let now = Local.with_ymd_and_hms(2026, 10, 15, 12, 0, 0).unwrap();
    let last_year = Local.with_ymd_and_hms(2025, 11, 8, 14, 3, 0).unwrap();
    assert_eq!(format_row_time(last_year, now).0, "Nov 08 2025, 14:03");
    let this_year = Local.with_ymd_and_hms(2026, 3, 8, 14, 3, 0).unwrap();
    assert_eq!(format_row_time(this_year, now).0, "Mar 08, 14:03");
    let (text, recency) = format_row_time(now - Duration::seconds(5), now);
    assert_eq!((text.as_str(), recency), ("just now", Recency::Now));
    // The future (clock skew) stays absolute without a year.
    let future = Local.with_ymd_and_hms(2027, 1, 2, 3, 4, 0).unwrap();
    assert_eq!(format_row_time(future, now).0, "Jan 02, 03:04");
}

/// L-016, L-017: custom title → line 1 and the summary closes line 2; a
/// summary alone is the title; neither → the project.
#[test]
fn line_one_follows_custom_title_summary_project() {
    let world = TestWorld::new();
    SessionSpec::new("proj", 1)
        .summary("Session summary S")
        .custom_title("Custom title T")
        .write(&world);
    SessionSpec::new("proj", 2)
        .summary("Only a summary")
        .start(Utc::now() - Duration::hours(2))
        .write(&world);
    SessionSpec::new("proj", 3)
        .start(Utc::now() - Duration::hours(4))
        .write(&world);
    let browser = browser_of(&world);
    let rows: Vec<RowModel> = (1..=3)
        .map(|number| model_of(&browser, number, "", 480.0))
        .collect();

    assert_eq!(rows[0].title_kind, TitleKind::Custom);
    assert_eq!(rows[0].title_text(), "Custom title T");
    assert!(
        rows[0].meta_text().ends_with("Session summary S"),
        "{}",
        rows[0].meta_text()
    );
    assert!(rows[0].meta_text().starts_with("proj"));

    assert_eq!(rows[1].title_kind, TitleKind::Summary);
    assert_eq!(rows[1].title_text(), "Only a summary");
    assert!(rows[1].project.is_some());
    assert!(rows[1].summary_tail.is_none());

    assert_eq!(rows[2].title_kind, TitleKind::Project);
    assert_eq!(rows[2].title_text(), "proj");
    assert!(
        rows[2].project.is_none(),
        "the project is the title: not repeated on line 2"
    );
}

/// L-019, L-021: `1 msg` / `2 msgs`, durations `1h 5m` / `5m` / none.
#[test]
fn meta_has_counts_and_durations() {
    let world = TestWorld::new();
    SessionSpec::new("proj", 1)
        .messages(&[("user", "q")])
        .start(Utc::now() - Duration::hours(1))
        .write(&world);
    SessionSpec::new("proj", 2)
        .messages(&[("user", "q"), ("assistant", "a")])
        .spacing_minutes(65)
        .start(Utc::now() - Duration::hours(3))
        .write(&world);
    SessionSpec::new("proj", 3)
        .messages(&[("user", "q"), ("assistant", "a")])
        .spacing_minutes(5)
        .start(Utc::now() - Duration::hours(5))
        .write(&world);
    let browser = browser_of(&world);
    let one = model_of(&browser, 1, "", 480.0);
    assert_eq!(one.message_count, "1 msg");
    assert_eq!(one.duration, None);
    assert!(one.meta_text().contains("1 msg") && !one.meta_text().contains("1 msgs"));
    let long = model_of(&browser, 2, "", 480.0);
    assert_eq!(long.message_count, "2 msgs");
    assert_eq!(long.duration.as_deref(), Some("1h 5m"));
    assert!(long.meta_text().contains("2 msgs · 1h 5m"));
    let short = model_of(&browser, 3, "", 480.0);
    assert_eq!(short.duration.as_deref(), Some("5m"));
}

/// L-018: long fields are cut with `…` (a safety net for the layout's own
/// ellipsis) and the score needs a row of at least 22rem.
#[test]
fn narrow_rows_cut_the_title_and_hide_the_score() {
    let long = "A very long conversation title that keeps going and going well past any pane width we could ever show";
    let field = fit_text(HighlightedText::plain(long.to_string()), 30);
    assert!(field.text.ends_with('…'));
    assert!(field.text.chars().count() <= 30);
    assert_eq!(
        fit_text(HighlightedText::plain("short".into()), 30).text,
        "short"
    );

    let world = TestWorld::new();
    SessionSpec::new("proj", 1)
        .summary(&long.repeat(3))
        .write(&world);
    let browser = browser_of(&world);
    let narrow = model_at(&browser, 0, "", 18.0 * 16.0);
    assert!(!narrow.title_text().is_empty());
    assert!(
        narrow.title_text().ends_with('…'),
        "{}",
        narrow.title_text()
    );
    assert_eq!(narrow.score, None);
    let query = HighlightQuery::default();
    let ctx = |width| RowContext {
        query: &query,
        now: Local::now(),
        literal: false,
        multiple_sources: false,
        pane_width_px: width,
        rem_px: 16.0,
    };
    assert!(!ctx(18.0 * 16.0).show_score());
    assert!(ctx(30.0 * 16.0).show_score());
}

/// L-014: the lane exists only when the corpus has more than one source.
#[test]
fn source_lane_only_for_mixed_corpora() {
    let world = TestWorld::new();
    SessionSpec::new("proj", 1).write(&world);
    let single = browser_of(&world);
    assert_eq!(model_at(&single, 0, "", 480.0).source_label, None);

    let mixed_world = TestWorld::new();
    crate::test_support::fixtures_list::listed_corpus(&mixed_world);
    let mixed = browser_of(&mixed_world);
    let labels: Vec<_> = (0..mixed.filtered().len())
        .map(|pos| model_at(&mixed, pos, "", 480.0).source_label)
        .collect();
    assert!(labels.contains(&Some("CC")), "{labels:?}");
    assert!(labels.contains(&Some("Pi")), "{labels:?}");
    assert!(labels.contains(&Some("OMP")), "{labels:?}");
}

/// L-024, L-027, L-028: a query matching only text outside the preview shows
/// the context snippet; a literal gets the context line with a highlight;
/// `run with loader` highlights `run_with_loader`.
#[test]
fn previews_show_context_and_highlights() {
    let world = TestWorld::new();
    let filler = "word ".repeat(200);
    let hidden = format!("{filler} the needle sits here {filler}");
    SessionSpec::new("proj", 1)
        .summary("Needle hunt")
        .messages(&[
            ("user", "short question"),
            ("assistant", "short answer"),
            ("user", hidden.as_str()),
            ("assistant", "ok"),
            ("user", "final question"),
            ("assistant", "final answer"),
            ("user", "really final"),
        ])
        .write(&world);
    SessionSpec::new("proj", 2)
        .summary("Loader")
        .messages(&[
            ("user", "how do we run_with_loader here"),
            ("assistant", "like this"),
        ])
        .start(Utc::now() - Duration::hours(3))
        .write(&world);
    let browser = browser_of(&world);

    // Literal: line 4 holds the snippet with the highlight range.
    let literal = model_of(&browser, 1, "\"needle\"", 480.0);
    assert!(literal.literal);
    let context = literal.context.as_ref().expect("a context line");
    assert!(context.text.contains("needle"), "{}", context.text);
    assert!(!context.highlights.is_empty());
    let (start, end) = context.highlights[0];
    assert_eq!(&context.text[start..end].to_lowercase(), "needle");

    // Without literals no context line is built...
    let plain = model_of(&browser, 1, "needle", 480.0);
    assert!(plain.context.is_none());

    // ...but once the search ran, line 3 shows the snippet around the match
    // outside the default preview (L-024).
    let mut searched = browser_of(&world);
    searched.set_query("needle");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while searched.has_search_work_in_flight() {
        searched.poll();
        assert!(std::time::Instant::now() < deadline, "the search finishes");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let hit = model_of(&searched, 1, "needle", 480.0);
    assert_eq!(
        hit.preview_source,
        claude_history_core::display_format::PreviewSource::LexicalContext
    );
    assert!(hit.preview.text.contains("needle"), "{}", hit.preview.text);

    // Query terms span `_`: `run with loader` covers `run_with_loader`.
    let loader_row = (0..browser.filtered().len())
        .map(|pos| model_at(&browser, pos, "run with loader", 480.0))
        .find(|row| row.title_text() == "Loader")
        .expect("the loader row");
    let covered: Vec<&str> = loader_row
        .preview
        .highlights
        .iter()
        .map(|&(start, end)| &loader_row.preview.text[start..end])
        .collect();
    assert!(
        covered
            .iter()
            .any(|piece| piece.contains("run_with_loader")),
        "{covered:?} in {:?}",
        loader_row.preview.text
    );
}

/// L-091: accessibility label and tooltip.
#[test]
fn rows_have_accessibility_label_and_tooltip() {
    let world = TestWorld::new();
    let path = SessionSpec::new("proj", 1).summary("Title").write(&world);
    let browser = browser_of(&world);
    let row = model_at(&browser, 0, "", 480.0);
    assert!(
        row.accessibility_label.starts_with("Title · proj · "),
        "{}",
        row.accessibility_label
    );
    assert!(row.tooltip.contains(&path.display().to_string()));
    assert!(row.tooltip.starts_with("proj · Title"));
    assert_eq!(row.id.as_ref(), path.display().to_string());
}
