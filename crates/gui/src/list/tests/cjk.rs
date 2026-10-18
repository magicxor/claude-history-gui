//! X-013: CJK and emoji fixture: the row model keeps whole characters,
//! highlights land on character boundaries and fields are cut with `…`
//! only between characters. (The glyph rendering itself is an S item: the
//! CJK screenshot.)

use gpui_kit::TestAppContext;

use crate::list::rows::fit_text;
use crate::test_support::TestWorld;
use crate::test_support::fixtures_list::ListFixture;
use claude_history_core::display_format::HighlightedText;

const CJK_SESSION: &[u8] = include_bytes!("../../../tests/fixtures/list/cjk-emoji.jsonl");

#[gpui_kit::test]
fn cjk_and_emoji_rows_keep_whole_characters(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    world.add_fixture(
        ".claude/projects/C--synthetic-cjk/c0ffee00-1c2d-4e5f-8a9b-0c1d2e3f4a5b.jsonl",
        CJK_SESSION,
    );
    let f = ListFixture::open_loaded(cx, &world, &[]);
    assert_eq!(
        f.titles(cx),
        vec!["日本語の会話 🎉 認証トークンの更新".to_string()]
    );
    f.type_text(cx, "トークン");
    f.wait_idle(cx);
    assert_eq!(f.paths(cx).len(), 1, "the CJK query finds the row");
    // Highlights follow core's word-start matching: `認証` starts a word.
    f.press(cx, "escape");
    f.type_text(cx, "認証");
    f.wait_idle(cx);
    let rows = f.read(cx, |list, _| list.row_models(0..1, 480.0));
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    // Every highlight range is a valid slice of its text (character boundaries).
    for field in [&row.title, &row.preview] {
        for &(start, end) in &field.highlights {
            assert!(field.text.is_char_boundary(start) && field.text.is_char_boundary(end));
            let _ = &field.text[start..end];
        }
    }
    assert!(
        row.title
            .highlights
            .iter()
            .any(|&(start, end)| &row.title.text[start..end] == "認証"),
        "{:?}",
        row.title
    );
    assert!(
        row.preview.text.chars().any(|c| c as u32 > 0x2e80),
        "{}",
        row.preview.text
    );

    // Cutting never splits a character or an emoji sequence's scalar.
    let cut = fit_text(HighlightedText::plain("日本語の会話🎉認証".to_string()), 5);
    assert_eq!(cut.text, "日本語の…");
    assert!(cut.text.is_char_boundary(cut.text.len()));
}
