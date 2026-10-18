//! Syntax highlighting of code segments on the background executor with a
//! token LRU keyed by `(item, segment, ThemeKind)` (DESIGN §9.7). WP2.
//!
//! Highlighting is core's (`core::syntax::highlight_code_with_theme`, the
//! TUI's syntect themes: base16-ocean.dark / InspiredGitHub, V-037), run per
//! batch of visible code segments with the palette of the theme kind the
//! frame was drawn in. The first frame shows the plain code color; the
//! viewer re-renders when the batch is done. Unknown languages yield no
//! tokens. syntect's lazy statics are warmed on a `syntect-warmup` thread
//! when the first viewer opens (30–65 ms first-use hitch, spike-viewer).

use std::ops::Range;
use std::sync::{Arc, Once};

use claude_history_core::syntax::highlight_code_with_theme;
use claude_history_core::theme::{ThemeKind, theme_for};
use gpui_kit::{FontStyle, FontWeight, HighlightStyle, Hsla};

/// Highlight runs of a code segment (sorted, non-overlapping byte ranges).
pub type Tokens = Arc<Vec<(Range<usize>, HighlightStyle)>>;

/// Code longer than this (bytes) is not highlighted (plain code color).
pub const MAX_HIGHLIGHT_BYTES: usize = 64 * 1024;

/// A syntect token color as a theme color. Token colors are data from core's
/// syntect themes, not literals; theme_tokens.rs converts them (X-019).
fn token_color(value: (u8, u8, u8)) -> Hsla {
    crate::theme_tokens::data_color(value)
}

/// The highlight runs of `code` in `lang` for the palette of `kind`; empty
/// for an unknown language or very large code.
pub fn highlight(code: &str, lang: &str, kind: ThemeKind) -> Tokens {
    if code.is_empty() || lang.is_empty() || code.len() > MAX_HIGHLIGHT_BYTES {
        return Arc::new(Vec::new());
    }
    let Some(lines) = highlight_code_with_theme(code, lang, theme_for(kind)) else {
        return Arc::new(Vec::new());
    };
    let mut runs = Vec::new();
    let mut offset = 0usize;
    for line in lines {
        for token in line {
            let len = token.text.len();
            let end = (offset + len).min(code.len());
            // Skip the line break itself and empty tokens.
            let visible_end = if code[offset.min(end)..end].ends_with('\n') {
                end - 1
            } else {
                end
            };
            if visible_end > offset
                && code.is_char_boundary(offset)
                && code.is_char_boundary(visible_end)
            {
                let mut style = HighlightStyle {
                    color: Some(token_color(token.fg)),
                    ..Default::default()
                };
                if token.bold {
                    style.font_weight = Some(FontWeight::BOLD);
                }
                if token.italic {
                    style.font_style = Some(FontStyle::Italic);
                }
                runs.push((offset..visible_end, style));
            }
            offset = end;
        }
    }
    Arc::new(runs)
}

/// Warm syntect's syntax and theme sets once per process, off the UI thread.
pub fn warm_up() {
    static WARM: Once = Once::new();
    WARM.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("syntect-warmup".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let _ = highlight("fn main() {}\n", "rust", ThemeKind::Dark);
                let _ = highlight("fn main() {}\n", "rust", ThemeKind::Light);
            });
    });
}
