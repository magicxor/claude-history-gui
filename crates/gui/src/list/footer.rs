//! The list footer and the search input's status suffix (DESIGN §5.1, §8.7,
//! §10.4): state first (count, load/refresh indicator, semantic activity),
//! then key hints from the live keymap, priority-trimmed, `F1 help` always
//! kept. WP1.
//!
//! The store's indexing progress (upstream 04c714b) is an activity too:
//! `Indexing conversations… 2,345 / 7,185` with a progress bar, then
//! `Finishing index… 7,185 conversations` with a spinner, in place of the
//! refresh indicator and after the semantic activity (upstream showed the
//! semantic activity instead), with `  ·  results may be incomplete` while a
//! query is typed.
//!
//! The semantic worker's background indexing (upstream c6d0421) is the
//! semantic activity `sem indexing 40%  12/30 chunks` with a progress bar,
//! while no search has an activity of its own.
//!
//! The hint chips are only shown while the list has focus and no activity is
//! displayed. While the initial load runs (AD6) the rename and delete chips
//! are dimmed, and open, resume and fork until the rows are listed newest
//! first (snapshot cards, upstream 3708bb6). Refresh is never advertised
//! (L-041). While automated and subagent conversations are listed, an
//! active `all kinds` chip says so (upstream cb990e6's `^G·all kinds`), and
//! while search is limited to the user's own messages an active `you only`
//! chip (upstream ae7ae39's `^V·you only`; `you only: not in semantic` in
//! semantic mode, which ignores it).

use claude_history_core::browser::{ListSearchMode, SemanticProgress};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::status_bar::StatusBar;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Action, App, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};

use crate::actions::{
    ClearQuery, Delete, Fork, OpenConversation, Rename, Resume, ShowShortcuts, ToggleAllKinds,
    ToggleScope, ToggleSearchMode, ToggleUserOnly,
};
use crate::keymap::{CTX_LIST, CTX_WORKSPACE, binding_labels};
use crate::theme_tokens::roles;

use super::ListPane;

/// The `you only` chip's label while search is limited to the user's own
/// messages (upstream ae7ae39's status-bar marker).
pub const USER_ONLY_CHIP: &str = "you only";
/// The chip's label in semantic mode, which ignores the toggle.
pub const USER_ONLY_SEMANTIC_CHIP: &str = "you only: not in semantic";

/// The context stack the list's hints are looked up in (outermost first).
const HINT_STACK: &str = "Workspace ConversationList";
/// Estimated width of one character of the footer text (rem).
const CHAR_REM: f32 = 0.35;
/// Estimated chrome of one chip: kbd padding and gaps (rem).
const CHIP_CHROME_REM: f32 = 1.2;
/// Gap between chips (rem).
const CHIP_GAP_REM: f32 = 0.75;
/// Horizontal padding of the footer (rem).
const FOOTER_PADDING_REM: f32 = 1.5;

/// A key hint chip (`<Kbd> label`) from the live keymap; nothing when unbound
/// (frozen WP0 API, also used by the viewer's status bar).
pub fn hint(
    action: &dyn Action,
    context: &str,
    label: &'static str,
    window: &Window,
    cx: &App,
) -> Option<impl IntoElement + use<>> {
    let kbd = Kbd::binding_for_action(action, Some(context), window)?;
    let muted = cx.theme().muted_foreground;
    Some(
        h_flex()
            .gap_1()
            .child(kbd)
            .child(div().text_color(muted).child(label)),
    )
}

/// The search input's right-aligned status (§10.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchStatus {
    /// `Loading… 3` / `1/8` / `0/0` / `sem 1/8` / `sem failed` / …
    pub text: String,
    /// The full semantic error behind `sem failed`.
    pub tooltip: Option<String>,
    /// A search is running on a worker (spinner).
    pub spinner: bool,
}

impl SearchStatus {
    pub fn element(&self, cx: &App) -> impl IntoElement + use<> {
        let roles = roles(cx);
        let failed = self.tooltip.is_some();
        let text = self.text.clone();
        let tooltip = self.tooltip.clone();
        h_flex()
            .gap_1()
            .items_center()
            .when(self.spinner, |this| {
                this.child(Spinner::new().xsmall().color(roles.muted_foreground))
            })
            .child(
                div()
                    .id("search-status")
                    .text_xs()
                    .whitespace_nowrap()
                    .text_color(if failed {
                        roles.danger
                    } else {
                        roles.muted_foreground
                    })
                    .child(text)
                    .when_some(tooltip, |this, tooltip| {
                        this.tooltip(move |window, cx| {
                            Tooltip::new(tooltip.clone()).build(window, cx)
                        })
                    }),
            )
    }
}

/// Which hint a chip stands for (priority order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChipKind {
    Cancel,
    Open,
    Resume,
    Fork,
    Rename,
    Delete,
    Scope,
    /// Shown only while automated and subagent conversations are listed.
    AllKinds,
    /// Shown only while search is limited to the user's own messages.
    UserOnly,
    Mode,
    Help,
}

/// A key hint chip of the footer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FooterChip {
    pub kind: ChipKind,
    /// `Ctrl+R`
    pub key: SharedString,
    /// `resume`
    pub label: SharedString,
    pub dimmed: bool,
    /// The chip shows the non-default value (`Prj`, `sem`): accent and bold (§8.7).
    pub active: bool,
    /// The key part is left out (a narrow footer keeping the pinned chips).
    pub keyless: bool,
}

impl FooterChip {
    /// `Ctrl+R resume`
    pub fn text(&self) -> String {
        if self.keyless {
            return self.label.to_string();
        }
        format!("{} {}", self.key, self.label)
    }
}

/// What the footer shows: state first, then hints.
#[derive(Clone, Debug, PartialEq)]
pub struct FooterModel {
    /// `12/340`, `sem 12/340`, `Loading… 57`
    pub count: String,
    /// `Refreshing conversations…`
    pub refreshing: bool,
    /// `sem preparing embeddings` / `sem embedding 40%  12/30 chunks` /
    /// `sem indexing 40%  12/30 chunks`
    pub semantic_activity: Option<String>,
    /// The embedding or indexing progress in percent, when known.
    pub semantic_percent: Option<f32>,
    /// `Indexing conversations… 2,345 / 7,185` / `Finishing index… 7,185
    /// conversations` while a reconcile pass is shown as indexing
    /// (`Browser::indexing_status_text`).
    pub indexing: Option<String>,
    /// The indexing progress in percent, while files are parsed.
    pub indexing_percent: Option<f32>,
    /// Hint chips (empty while an activity shows or the list lacks focus).
    pub chips: Vec<FooterChip>,
}

impl FooterModel {
    /// Whether an activity replaces the hints.
    pub fn has_activity(&self) -> bool {
        self.refreshing || self.semantic_activity.is_some() || self.indexing.is_some()
    }
}

/// A chip before trimming: the action it hints and its texts.
struct ChipSpec {
    kind: ChipKind,
    action: Box<dyn Action>,
    context: &'static str,
    key: SharedString,
    label: SharedString,
    dimmed: bool,
}

impl ListPane {
    /// The status of the search input (§10.4): the browser's status text with
    /// the `…` substitution, the semantic error as tooltip, the spinner while
    /// a worker runs.
    pub fn search_status(&self) -> SearchStatus {
        SearchStatus {
            text: crate::notify::gui_text(&self.browser.search_status_text()),
            tooltip: self.browser.semantic_error().map(str::to_string),
            // An empty semantic corpus never answers (core keeps the request
            // in flight): no endless spinner for it.
            spinner: self.browser.has_search_work_in_flight()
                && !matches!(
                    self.browser.semantic_progress(),
                    SemanticProgress::EmptyCorpus
                ),
        }
    }

    /// The footer's content for the current state.
    pub fn footer_model(&self, window: &Window, cx: &App) -> FooterModel {
        let rem_px = window.rem_size().as_f32().max(1.0);
        let pane_rem = self
            .metrics
            .get()
            .pane_width
            .map(|width| width.as_f32() / rem_px)
            .unwrap_or(super::DEFAULT_PANE_REM);
        self.footer_model_for_width(pane_rem, window, cx)
    }

    /// [`ListPane::footer_model`] for a pane `pane_rem` wide (the hint chips
    /// are trimmed to it).
    pub fn footer_model_for_width(&self, pane_rem: f32, window: &Window, cx: &App) -> FooterModel {
        let activity = self.browser.semantic_activity_status_text();
        // The counts the activity text shows (`sem embedding`, `sem indexing`).
        let percent = self
            .browser
            .semantic_activity_counts()
            .map(|(completed, total)| {
                if total == 0 {
                    100.0
                } else {
                    completed.min(total) as f32 * 100.0 / total as f32
                }
            });
        let indexing = self
            .browser
            .indexing_status_text()
            .map(|text| crate::notify::gui_text(&text));
        let indexing_percent = self
            .browser
            .index_progress()
            .filter(|progress| indexing.is_some() && progress.done < progress.total)
            .map(|progress| progress.done as f32 * 100.0 / progress.total as f32);
        let mut model = FooterModel {
            count: crate::notify::gui_text(&self.browser.search_status_text()),
            refreshing: self.is_refreshing(),
            semantic_activity: activity,
            semantic_percent: percent,
            indexing,
            indexing_percent,
            chips: Vec::new(),
        };
        if !model.has_activity() && self.contains_focus(window, cx) {
            let specs = self.chip_specs(cx);
            let fit = fit_chips(&specs, &model.count, pane_rem);
            model.chips = specs
                .into_iter()
                .zip(fit.keep)
                .filter(|(_, keep)| *keep)
                .map(|(spec, _)| FooterChip {
                    kind: spec.kind,
                    key: spec.key,
                    keyless: fit.keyless && pinned_kind(spec.kind),
                    // The ignored toggle shows dimmed instead of the long label.
                    dimmed: spec.dimmed || (fit.short_semantic && spec.kind == ChipKind::UserOnly),
                    label: if fit.short_semantic && spec.kind == ChipKind::UserOnly {
                        USER_ONLY_CHIP.into()
                    } else {
                        spec.label
                    },
                    active: match spec.kind {
                        ChipKind::Scope => self.scope_is_workspace(),
                        ChipKind::AllKinds | ChipKind::UserOnly => true,
                        ChipKind::Mode => self.mode() == ListSearchMode::Semantic,
                        _ => false,
                    },
                })
                .collect();
        }
        model
    }

    fn chip_specs(&self, cx: &App) -> Vec<ChipSpec> {
        // Rows can be opened before loading finishes, edited only after.
        let loading = self.is_loading();
        let not_ready = !self.list_ready();
        let picker = crate::exit::is_picker(cx);
        let mut specs = Vec::new();
        let spec = |kind: ChipKind,
                    action: Box<dyn Action>,
                    context: &'static str,
                    stack: &str,
                    label: &str,
                    dimmed: bool|
         -> Option<ChipSpec> {
            let key = binding_labels(action.as_ref(), stack, cx)
                .into_iter()
                .next()?;
            Some(ChipSpec {
                kind,
                action,
                context,
                key,
                label: label.to_string().into(),
                dimmed,
            })
        };
        if picker {
            specs.push(ChipSpec {
                kind: ChipKind::Cancel,
                action: Box::new(ClearQuery),
                context: CTX_LIST,
                key: "Esc".into(),
                label: "cancel".into(),
                dimmed: false,
            });
        }
        specs.extend(spec(
            ChipKind::Open,
            Box::new(OpenConversation),
            CTX_LIST,
            HINT_STACK,
            "open",
            not_ready,
        ));
        specs.extend(spec(
            ChipKind::Resume,
            Box::new(Resume),
            CTX_LIST,
            HINT_STACK,
            "resume",
            not_ready,
        ));
        specs.extend(spec(
            ChipKind::Fork,
            Box::new(Fork),
            CTX_LIST,
            HINT_STACK,
            "fork",
            not_ready,
        ));
        specs.extend(spec(
            ChipKind::Rename,
            Box::new(Rename),
            CTX_LIST,
            HINT_STACK,
            "rename",
            loading,
        ));
        specs.extend(spec(
            ChipKind::Delete,
            Box::new(Delete),
            CTX_LIST,
            HINT_STACK,
            "delete",
            loading,
        ));
        if self.has_workspace() {
            let label = if self.scope_is_workspace() {
                "Prj"
            } else {
                "All"
            };
            specs.extend(spec(
                ChipKind::Scope,
                Box::new(ToggleScope),
                CTX_LIST,
                HINT_STACK,
                label,
                false,
            ));
        }
        if self.all_kinds() {
            specs.extend(spec(
                ChipKind::AllKinds,
                Box::new(ToggleAllKinds),
                CTX_LIST,
                HINT_STACK,
                "all kinds",
                false,
            ));
        }
        if self.user_only() {
            // Semantic search ignores the toggle; the chip says so.
            let label = if self.searches_user_only() {
                USER_ONLY_CHIP
            } else {
                USER_ONLY_SEMANTIC_CHIP
            };
            specs.extend(spec(
                ChipKind::UserOnly,
                Box::new(ToggleUserOnly),
                CTX_LIST,
                HINT_STACK,
                label,
                false,
            ));
        }
        let mode = match self.mode() {
            ListSearchMode::Lexical => "lex",
            ListSearchMode::Semantic => "sem",
        };
        specs.extend(spec(
            ChipKind::Mode,
            Box::new(ToggleSearchMode),
            CTX_LIST,
            HINT_STACK,
            mode,
            false,
        ));
        specs.extend(spec(
            ChipKind::Help,
            Box::new(ShowShortcuts),
            CTX_WORKSPACE,
            "Workspace",
            "help",
            false,
        ));
        specs
    }
}

/// The chips that stay whatever the width: `Ctrl+G all kinds` and
/// `Alt+U you only` are the footer's only signs of their toggles' state.
fn pinned_kind(kind: ChipKind) -> bool {
    matches!(kind, ChipKind::AllKinds | ChipKind::UserOnly)
}

/// Which chips a footer shows and how the pinned ones are drawn.
struct Fit {
    keep: Vec<bool>,
    /// The pinned chips drop their key part.
    keyless: bool,
    /// The semantic `you only: not in semantic` chip shrinks to a dimmed
    /// `you only`.
    short_semantic: bool,
}

/// Which chips fit a pane `pane_rem` wide: the lowest priority goes first,
/// `F1 help` stays. While a pinned chip is shown, `Enter open` may go too,
/// and when the pinned chips and help still do not fit, the pinned chips
/// lose their key and the long semantic label.
fn fit_chips(specs: &[ChipSpec], count: &str, pane_rem: f32) -> Fit {
    let count_rem = count.chars().count() as f32 * CHAR_REM + 1.0;
    let available = pane_rem - FOOTER_PADDING_REM - count_rem;
    let any_pinned = specs.iter().any(|spec| pinned_kind(spec.kind));
    let build = |keyless: bool, short_semantic: bool| {
        let width_of = |spec: &ChipSpec| {
            let key = if keyless && pinned_kind(spec.kind) {
                0
            } else {
                spec.key.chars().count()
            };
            let label = if short_semantic && spec.kind == ChipKind::UserOnly {
                USER_ONLY_CHIP.chars().count()
            } else {
                spec.label.chars().count()
            };
            chip_width_rem(key, label)
        };
        let pinned_rem: f32 = specs
            .iter()
            .filter(|spec| pinned_kind(spec.kind))
            .map(|spec| width_of(spec) + CHIP_GAP_REM)
            .sum();
        let widths: Vec<f32> = specs
            .iter()
            .filter(|spec| !pinned_kind(spec.kind))
            .map(width_of)
            .collect();
        let (kept, fits) = trim_chips_pinned(&widths, available - pinned_rem, !any_pinned);
        let mut kept = kept.into_iter();
        let keep: Vec<bool> = specs
            .iter()
            .map(|spec| pinned_kind(spec.kind) || kept.next().unwrap_or(true))
            .collect();
        (
            Fit {
                keep,
                keyless,
                short_semantic,
            },
            fits,
        )
    };
    for keyless in [false, true] {
        let (fit, fits) = build(keyless, false);
        if fits || !any_pinned {
            return fit;
        }
    }
    build(true, true).0
}

/// The estimated width of a chip (rem).
pub fn chip_width_rem(key_chars: usize, label_chars: usize) -> f32 {
    // Without a key there is no kbd box: only the label's own gap remains.
    let chrome = if key_chars == 0 {
        CHIP_CHROME_REM / 3.0
    } else {
        CHIP_CHROME_REM
    };
    (key_chars + label_chars) as f32 * CHAR_REM + chrome
}

/// Which of the chips (priority order, the last one is `F1 help`) fit
/// `available_rem`: chips are dropped from the lowest priority upward; the
/// first chip and the last (`help`) always stay when at all possible, so the
/// result keeps at least `help`.
pub fn trim_chips(widths: &[f32], available_rem: f32) -> Vec<bool> {
    trim_chips_pinned(widths, available_rem, true).0
}

/// [`trim_chips`] with the first chip droppable (`keep_first` false), and
/// whether what is kept fits `available_rem`.
fn trim_chips_pinned(widths: &[f32], available_rem: f32, keep_first: bool) -> (Vec<bool>, bool) {
    let mut keep = vec![true; widths.len()];
    let total = |keep: &[bool]| -> f32 {
        let chosen: Vec<f32> = widths
            .iter()
            .zip(keep)
            .filter(|(_, keep)| **keep)
            .map(|(width, _)| *width)
            .collect();
        chosen.iter().sum::<f32>() + chosen.len().saturating_sub(1) as f32 * CHIP_GAP_REM
    };
    // Drop from the second-to-last backwards (the last chip is help).
    let floor = if keep_first { 1 } else { 0 };
    let mut ix = widths.len().saturating_sub(1);
    while total(&keep) > available_rem && ix > floor {
        ix -= 1;
        keep[ix] = false;
    }
    let fits = total(&keep) <= available_rem;
    (keep, fits)
}

/// The footer element.
pub fn render_footer(list: &ListPane, window: &Window, cx: &App) -> impl IntoElement + use<> {
    let model = list.footer_model(window, cx);
    let roles = roles(cx);
    let muted = roles.muted_foreground;
    let accent = roles.accent;
    let specs = if model.chips.is_empty() {
        Vec::new()
    } else {
        list.chip_specs(cx)
    };
    let chips: Vec<_> = model
        .chips
        .iter()
        .filter_map(|chip| {
            let spec = specs.iter().find(|spec| spec.kind == chip.kind)?;
            let kbd = Kbd::binding_for_action(spec.action.as_ref(), Some(spec.context), window)?;
            Some(
                h_flex()
                    .gap_1()
                    .when(chip.dimmed, |this| this.opacity(0.5))
                    .when(!chip.keyless, |this| this.child(kbd))
                    .child(if chip.active {
                        div()
                            .text_color(accent)
                            .font_weight(FontWeight::BOLD)
                            .child(chip.label.clone())
                    } else {
                        div().text_color(muted).child(chip.label.clone())
                    }),
            )
        })
        .collect();
    let activity = h_flex()
        .gap_2()
        .items_center()
        // The indexing progress says more than the refresh indicator.
        .when(model.refreshing && model.indexing.is_none(), |this| {
            this.child(
                h_flex()
                    .id("footer-refreshing")
                    .gap_1()
                    .items_center()
                    .child(Spinner::new().xsmall().color(muted))
                    .child(div().text_color(muted).child(crate::notify::gui_text(
                        claude_history_core::browser::REFRESHING_STATUS,
                    ))),
            )
        })
        .when_some(model.semantic_activity.clone(), |this, text| {
            this.child(
                h_flex()
                    .id("footer-semantic-activity")
                    .gap_1()
                    .items_center()
                    .when_some(model.semantic_percent, |this, percent| {
                        this.child(
                            div()
                                .w_12()
                                .child(Progress::new("semantic-progress").value(percent)),
                        )
                    })
                    .when(model.semantic_percent.is_none(), |this| {
                        this.child(Spinner::new().xsmall().color(muted))
                    })
                    .child(div().text_color(muted).child(text)),
            )
        })
        .when_some(model.indexing.clone(), |this, text| {
            this.child(
                h_flex()
                    .id("footer-indexing")
                    .gap_1()
                    .items_center()
                    .when_some(model.indexing_percent, |this, percent| {
                        this.child(
                            div()
                                .w_12()
                                .child(Progress::new("indexing-progress").value(percent)),
                        )
                    })
                    .when(model.indexing_percent.is_none(), |this| {
                        this.child(Spinner::new().xsmall().color(muted))
                    })
                    .child(div().text_color(muted).child(text)),
            )
        });
    StatusBar::new()
        .text_xs()
        .left(
            h_flex()
                .gap_2()
                .items_center()
                .child(div().text_color(muted).child(model.count.clone()))
                .child(activity),
        )
        .when(!chips.is_empty(), |bar| {
            bar.right(h_flex().gap_3().children(chips))
        })
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn trimming_drops_the_lowest_priority_first_and_keeps_help() {
        let widths = [3.0, 3.0, 3.0, 3.0, 3.0, 3.0, 3.0, 3.0];
        assert_eq!(trim_chips(&widths, 100.0), vec![true; 8]);
        let tight = trim_chips(&widths, 8.0);
        assert!(tight[0] && tight[7]);
        assert!(!tight[6] && !tight[5]);
        assert_eq!(tight.iter().filter(|keep| **keep).count(), 2);
        // Nothing fits: help is still kept.
        assert_eq!(trim_chips(&widths, 0.5).last(), Some(&true));
    }

    #[test]
    fn dropping_the_first_chip_keeps_help_and_reports_the_fit() {
        let widths = [3.0, 3.0, 3.0, 3.0];
        let (keep, fits) = trim_chips_pinned(&widths, 4.0, false);
        assert_eq!(keep, vec![false, false, false, true]);
        assert!(fits);
        let (keep, _) = trim_chips_pinned(&widths, 4.0, true);
        assert!(keep[0] && keep[3]);
    }
}
