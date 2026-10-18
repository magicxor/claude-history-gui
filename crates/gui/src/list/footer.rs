//! The list footer and the search input's status suffix (DESIGN §5.1, §8.7,
//! §10.4): state first (count, load/refresh indicator, semantic activity),
//! then key hints from the live keymap, priority-trimmed, `F1 help` always
//! kept. WP1.
//!
//! The hint chips are only shown while the list has focus and no activity is
//! displayed; the five action chips are dimmed while the initial load runs
//! (AD6). Refresh is never advertised (L-041).

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
    ClearQuery, Delete, Fork, OpenConversation, Rename, Resume, ShowShortcuts, ToggleScope,
    ToggleSearchMode,
};
use crate::keymap::{CTX_LIST, CTX_WORKSPACE, binding_labels};
use crate::theme_tokens::roles;

use super::ListPane;

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
}

impl FooterChip {
    /// `Ctrl+R resume`
    pub fn text(&self) -> String {
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
    /// `sem preparing embeddings` / `sem embedding 40%  12/30 chunks`
    pub semantic_activity: Option<String>,
    /// The embedding progress in percent, when known.
    pub semantic_percent: Option<f32>,
    /// Hint chips (empty while an activity shows or the list lacks focus).
    pub chips: Vec<FooterChip>,
}

impl FooterModel {
    /// Whether an activity replaces the hints.
    pub fn has_activity(&self) -> bool {
        self.refreshing || self.semantic_activity.is_some()
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
        let percent = match self.browser.semantic_progress() {
            SemanticProgress::Embedding { completed, total } if activity.is_some() => {
                Some(if *total == 0 {
                    100.0
                } else {
                    (*completed).min(*total) as f32 * 100.0 / *total as f32
                })
            }
            _ => None,
        };
        let mut model = FooterModel {
            count: crate::notify::gui_text(&self.browser.search_status_text()),
            refreshing: self.is_refreshing(),
            semantic_activity: activity,
            semantic_percent: percent,
            chips: Vec::new(),
        };
        if !model.has_activity() && self.contains_focus(window, cx) {
            let specs = self.chip_specs(cx);
            let kept = fit_chips(&specs, &model.count, pane_rem);
            model.chips = specs
                .into_iter()
                .zip(kept)
                .filter(|(_, keep)| *keep)
                .map(|(spec, _)| FooterChip {
                    kind: spec.kind,
                    key: spec.key,
                    label: spec.label,
                    dimmed: spec.dimmed,
                    active: match spec.kind {
                        ChipKind::Scope => self.scope_is_workspace(),
                        ChipKind::Mode => self.mode() == ListSearchMode::Semantic,
                        _ => false,
                    },
                })
                .collect();
        }
        model
    }

    fn chip_specs(&self, cx: &App) -> Vec<ChipSpec> {
        let loading = self.is_loading();
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
            loading,
        ));
        specs.extend(spec(
            ChipKind::Resume,
            Box::new(Resume),
            CTX_LIST,
            HINT_STACK,
            "resume",
            loading,
        ));
        specs.extend(spec(
            ChipKind::Fork,
            Box::new(Fork),
            CTX_LIST,
            HINT_STACK,
            "fork",
            loading,
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

/// Which chips fit a pane `pane_rem` wide: the lowest priority goes first,
/// `F1 help` stays.
fn fit_chips(specs: &[ChipSpec], count: &str, pane_rem: f32) -> Vec<bool> {
    let count_rem = count.chars().count() as f32 * CHAR_REM + 1.0;
    let available = pane_rem - FOOTER_PADDING_REM - count_rem;
    let widths: Vec<f32> = specs
        .iter()
        .map(|spec| chip_width_rem(spec.key.chars().count(), spec.label.chars().count()))
        .collect();
    trim_chips(&widths, available)
}

/// The estimated width of a chip (rem).
pub fn chip_width_rem(key_chars: usize, label_chars: usize) -> f32 {
    (key_chars + label_chars) as f32 * CHAR_REM + CHIP_CHROME_REM
}

/// Which of the chips (priority order, the last one is `F1 help`) fit
/// `available_rem`: chips are dropped from the lowest priority upward; the
/// first chip and the last (`help`) always stay when at all possible, so the
/// result keeps at least `help`.
pub fn trim_chips(widths: &[f32], available_rem: f32) -> Vec<bool> {
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
    let mut ix = widths.len().saturating_sub(1);
    while total(&keep) > available_rem && ix > 1 {
        ix -= 1;
        keep[ix] = false;
    }
    keep
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
                    .child(kbd)
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
        .when(model.refreshing, |this| {
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
}
