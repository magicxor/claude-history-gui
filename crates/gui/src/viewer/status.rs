//! The viewer status bar (DESIGN §9.10): `[i/N]`, toggle indicators, hints
//! from the live keymap (§8.7). WP2.
//!
//! Left: the scroll indicator `[i/N]` (mono) and the toggle indicators
//! `tools·sum|trn|all  think·on|off  info·on|off` (key letter accent, rest
//! muted, each clickable). Right: key hints for the focused viewer only, in
//! priority order — `/ search`, `e export`, `y yank`, Resume `resume`, Fork
//! `fork`, Delete `del`, `q back` (single-file: `q close`, no Resume, Fork or
//! Delete; V-011 quirk 9 fixed), `F1 help` — trimmed from the lowest
//! priority when they do not fit; `F1 help` always stays. With an active
//! search that has matches the hints are `n next  N prev  <i>/<n>  Esc
//! clear` (TUI text).

use claude_history_core::view::{ToolDisplayMode, ViewPrefs};
use gpui_kit::{Action, AsKeystroke as _, KeyContext, Keystroke, Window};

use crate::actions::{
    Delete, ExportConversation, Fork, NextMatch, PrevMatch, Resume, ShowShortcuts, StartSearch,
    ViewerBack, ViewerEscape, YankOrCopyMessage,
};
use crate::keymap::{CTX_VIEWER, CTX_WORKSPACE};
use crate::model::ViewerMode;

/// The toggle indicators (`tools·sum`, `think·off`, `info·off`).
pub fn toggle_texts(prefs: &ViewPrefs) -> [String; 3] {
    [
        format!("tools·{}", prefs.tool_display.status_label()),
        format!("think·{}", if prefs.show_thinking { "on" } else { "off" }),
        format!("info·{}", if prefs.show_timing { "on" } else { "off" }),
    ]
}

/// One hint chip: key text and label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hint {
    pub key: String,
    pub label: &'static str,
    /// Never trimmed (`F1 help`).
    pub pinned: bool,
}

impl Hint {
    pub fn text(&self) -> String {
        format!("{} {}", self.key, self.label)
    }
}

/// A key label: plain single characters as typed (`e`, `/`, `q`), the rest
/// in gpui-kit's platform format (`Ctrl+R`, `F1`).
pub fn key_label(keystroke: &Keystroke) -> String {
    let m = &keystroke.modifiers;
    let plain = !m.control && !m.alt && !m.shift && !m.platform && !m.function;
    if plain && keystroke.key.chars().count() == 1 {
        return keystroke.key.clone();
    }
    // Shift + a letter reads as the capital (`N`), like the original's hints.
    let only_shift = m.shift && !m.control && !m.alt && !m.platform && !m.function;
    if only_shift
        && keystroke.key.chars().count() == 1
        && keystroke.key.chars().all(char::is_alphabetic)
    {
        return keystroke.key.to_uppercase();
    }
    gpui_kit::component::kbd::Kbd::format(keystroke)
}

/// The live key text of `action` in `context` (`None` when unbound).
pub fn binding_text(action: &dyn Action, context: &str, window: &Window) -> Option<String> {
    let context = KeyContext::parse(context).ok()?;
    let binding = window.highest_precedence_binding_for_action_in_context(action, context)?;
    let keystroke = binding.keystrokes().first()?;
    Some(key_label(keystroke.as_keystroke()))
}

/// The primary (first-registered) key text of `action` in `context`: the
/// default `n` rather than its `F3` alias (`None` when unbound).
pub fn primary_binding_text(action: &dyn Action, context: &str, window: &Window) -> Option<String> {
    let context = KeyContext::parse(context).ok()?;
    let bindings = window.bindings_for_action_in_context(action, context);
    let keystroke = bindings.first()?.keystrokes().first()?.clone();
    Some(key_label(keystroke.as_keystroke()))
}

/// The hint specs in priority order for `mode`.
fn specs(mode: ViewerMode) -> Vec<(Box<dyn Action>, &'static str, &'static str)> {
    let mut specs: Vec<(Box<dyn Action>, &'static str, &'static str)> = vec![
        (Box::new(StartSearch), CTX_VIEWER, "search"),
        (Box::new(ExportConversation), CTX_VIEWER, "export"),
        (Box::new(YankOrCopyMessage), CTX_VIEWER, "yank"),
    ];
    if mode == ViewerMode::Main {
        specs.push((Box::new(Resume), CTX_VIEWER, "resume"));
        specs.push((Box::new(Fork), CTX_VIEWER, "fork"));
        specs.push((Box::new(Delete), CTX_VIEWER, "del"));
    }
    specs.push((
        Box::new(ViewerBack),
        CTX_VIEWER,
        if mode == ViewerMode::Main {
            "back"
        } else {
            "close"
        },
    ));
    specs
}

/// The hints for the focused viewer (before trimming).
pub fn hints(mode: ViewerMode, window: &Window) -> Vec<Hint> {
    let mut out: Vec<Hint> = specs(mode)
        .into_iter()
        .filter_map(|(action, context, label)| {
            binding_text(action.as_ref(), context, window).map(|key| Hint {
                key,
                label,
                pinned: false,
            })
        })
        .collect();
    let help_context = match mode {
        ViewerMode::Main => CTX_WORKSPACE,
        ViewerMode::SingleFile => crate::keymap::CTX_SINGLE_FILE,
    };
    if let Some(key) = binding_text(&ShowShortcuts, help_context, window) {
        out.push(Hint {
            key,
            label: "help",
            pinned: true,
        });
    }
    out
}

/// The search-active strip (`n next  N prev  <i>/<n>  Esc clear`) from the
/// live bindings (V-011); an unbound action's hint is left out.
pub fn search_strip(counter: &str, window: &Window) -> String {
    search_strip_with(
        counter,
        primary_binding_text(&NextMatch, CTX_VIEWER, window),
        primary_binding_text(&PrevMatch, CTX_VIEWER, window),
        primary_binding_text(&ViewerEscape, CTX_VIEWER, window),
    )
}

/// The strip for the given key texts.
pub fn search_strip_with(
    counter: &str,
    next: Option<String>,
    prev: Option<String>,
    clear: Option<String>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(key) = next {
        parts.push(format!("{key} next"));
    }
    if let Some(key) = prev {
        parts.push(format!("{key} prev"));
    }
    parts.push(counter.to_string());
    if let Some(key) = clear {
        parts.push(format!("{key} clear"));
    }
    parts.join("  ")
}

/// Keep the hints that fit in `available` cells, dropping the lowest
/// priority first; pinned hints always stay.
pub fn trim(hints: Vec<Hint>, available: usize) -> Vec<Hint> {
    let width = |hints: &[Hint]| -> usize {
        hints
            .iter()
            .map(|hint| hint.text().chars().count() + 2)
            .sum()
    };
    let mut hints = hints;
    while width(&hints) > available {
        let Some(position) = hints.iter().rposition(|hint| !hint.pinned) else {
            break;
        };
        hints.remove(position);
    }
    hints
}

/// The status label of a tool display mode (for tests and probes).
pub fn tools_label(mode: ToolDisplayMode) -> &'static str {
    mode.status_label()
}
