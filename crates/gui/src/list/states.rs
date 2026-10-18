//! The list area's non-row states (DESIGN §12 "List area"): initial loading,
//! load failure, no history, no matches, nothing in scope, semantic failure.
//! Every error state offers a recovery action (DG §9.13). WP1.

use claude_history_core::browser::{ListSearchMode, SemanticProgress};
use claude_history_core::time_filter::TimeFilter;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::empty::{
    Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle,
};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, Window, div,
};

use crate::actions::{OpenSettings, ShowDebugLog};

use super::ListPane;
use super::load::ViewState;

/// A recovery action of an empty state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateAction {
    Retry,
    ShowDebugLog,
    Refresh,
    ClearTimeFilter,
    ClearSearch,
    ShowAll,
    OpenSettings,
    SwitchToLexical,
    RetrySemantic,
}

impl StateAction {
    /// The button label.
    pub fn label(self) -> &'static str {
        match self {
            StateAction::Retry => "Retry",
            StateAction::ShowDebugLog => "Show debug log",
            StateAction::Refresh => "Refresh",
            StateAction::ClearTimeFilter => "Clear time filter",
            StateAction::ClearSearch => "Clear search",
            StateAction::ShowAll => "Show all",
            StateAction::OpenSettings => "Open Settings",
            StateAction::SwitchToLexical => "Switch to lexical",
            StateAction::RetrySemantic => "Retry",
        }
    }
}

/// How an empty state looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateKind {
    Loading,
    Info,
    Error,
}

/// What the list area shows instead of rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmptyState {
    pub kind: StateKind,
    pub title: String,
    pub description: Option<String>,
    pub actions: Vec<StateAction>,
}

impl EmptyState {
    fn new(kind: StateKind, title: impl Into<String>) -> Self {
        Self {
            kind,
            title: title.into(),
            description: None,
            actions: Vec::new(),
        }
    }

    fn description(mut self, text: impl Into<String>) -> Self {
        self.description = Some(text.into());
        self
    }

    fn actions(mut self, actions: &[StateAction]) -> Self {
        self.actions = actions.to_vec();
        self
    }
}

/// `No conversation history found in selected scope` (+ the time range).
pub fn no_history_text(time_filter_active: bool) -> String {
    if time_filter_active {
        "No conversation history found in selected scope within the requested time range"
            .to_string()
    } else {
        "No conversation history found in selected scope".to_string()
    }
}

impl ListPane {
    /// The state replacing the rows, if any.
    pub fn empty_state(&self, cx: &gpui_kit::App) -> Option<EmptyState> {
        let _ = cx;
        let browser = &self.browser;
        match &self.view {
            ViewState::Failed(error) => Some(
                EmptyState::new(StateKind::Error, "Couldn't load history")
                    .description(error.clone())
                    .actions(&[StateAction::Retry, StateAction::ShowDebugLog]),
            ),
            ViewState::Loading => browser
                .filtered()
                .is_empty()
                .then(|| EmptyState::new(StateKind::Loading, "Reading history…")),
            ViewState::Ready => {
                if !browser.filtered().is_empty() {
                    return None;
                }
                if browser.conversations().is_empty() {
                    let mut actions = vec![StateAction::Refresh];
                    if self.time_filter.is_active() {
                        actions.push(StateAction::ClearTimeFilter);
                    }
                    return Some(
                        EmptyState::new(
                            StateKind::Info,
                            no_history_text(self.time_filter.is_active()),
                        )
                        .actions(&actions),
                    );
                }
                if self.mode() == ListSearchMode::Semantic
                    && matches!(browser.semantic_progress(), SemanticProgress::EmptyCorpus)
                {
                    // Shown even while a request is nominally in flight: core
                    // keeps `has_search_work_in_flight()` set for an empty
                    // semantic corpus (see the hand-off CCRs).
                    return Some(
                        EmptyState::new(StateKind::Info, "No text to search semantically")
                            .actions(&[StateAction::SwitchToLexical]),
                    );
                }
                if browser.has_search_work_in_flight() {
                    // The previous (empty) result stays until the worker answers.
                    return None;
                }
                // `semantic_error()` is only set in Semantic mode.
                if let Some(error) = browser.semantic_error() {
                    return Some(
                        EmptyState::new(StateKind::Error, "Semantic search failed")
                            .description(error.to_string())
                            .actions(&[StateAction::SwitchToLexical, StateAction::RetrySemantic]),
                    );
                }
                if !browser.query().trim().is_empty() {
                    return Some(
                        EmptyState::new(StateKind::Info, "No matching conversations")
                            .actions(&[StateAction::ClearSearch]),
                    );
                }
                if !self.exclude_projects.is_empty() {
                    // L-079: `[tui] exclude_projects` keeps hiding rows (the core
                    // keeps excluded rows in `conversations()`), so Show all alone
                    // cannot help: name the cause and offer Settings.
                    let list = self.exclude_projects.join(", ");
                    if self.scope_is_workspace() {
                        return Some(
                            EmptyState::new(StateKind::Info, "No conversations in this scope")
                                .description(format!(
                                    "Switch to All or change the workspace. Excluded projects stay hidden: {list}."
                                ))
                                .actions(&[StateAction::ShowAll, StateAction::OpenSettings]),
                        );
                    }
                    return Some(
                        EmptyState::new(
                            StateKind::Info,
                            "Every conversation is in an excluded project",
                        )
                        .description(format!("Excluded in Settings (exclude_projects): {list}."))
                        .actions(&[StateAction::OpenSettings]),
                    );
                }
                Some(
                    EmptyState::new(StateKind::Info, "No conversations in this scope")
                        .description("Switch to All or change the workspace.")
                        .actions(&[StateAction::ShowAll]),
                )
            }
        }
    }

    /// Run a recovery action of an empty state.
    pub(crate) fn run_state_action(
        &mut self,
        action: StateAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            StateAction::Retry | StateAction::Refresh => self.refresh(cx),
            StateAction::ShowDebugLog => window.dispatch_action(Box::new(ShowDebugLog), cx),
            StateAction::OpenSettings => window.dispatch_action(Box::new(OpenSettings), cx),
            StateAction::ClearTimeFilter => self.set_time_filter(TimeFilter::default(), None, cx),
            StateAction::ClearSearch => self.clear_query(window, cx),
            StateAction::ShowAll => {
                self.set_scope_workspace(false, cx);
                self.clear_query(window, cx);
            }
            StateAction::SwitchToLexical => self.switch_to_lexical(cx),
            StateAction::RetrySemantic => self.retry_semantic(cx),
        }
    }
}

impl EmptyState {
    /// The element; buttons call back into the pane.
    pub(super) fn render(
        self,
        _list: &ListPane,
        _window: &mut Window,
        cx: &mut Context<ListPane>,
    ) -> AnyElement {
        if self.kind == StateKind::Loading {
            let muted = cx.theme().muted_foreground;
            return v_flex()
                .id("list-state-loading")
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .gap_3()
                .child(Spinner::new())
                .child(div().text_sm().text_color(muted).child(self.title))
                .into_any_element();
        }
        let icon = match self.kind {
            StateKind::Error => IconName::CircleAlert,
            _ => IconName::Inbox,
        };
        let mut header = EmptyHeader::new()
            .media(EmptyMedia::new().child(icon))
            .title(EmptyTitle::new().child(SharedString::from(self.title.clone())));
        if let Some(description) = &self.description {
            header = header.description(
                EmptyDescription::new().child(SharedString::from(description.clone())),
            );
        }
        let buttons = self.actions.iter().enumerate().map(|(ix, action)| {
            let action = *action;
            let this = cx.entity().downgrade();
            let button = Button::new(("list-state-action", ix))
                .label(action.label())
                .small();
            let button = if ix == 0 {
                button.primary()
            } else {
                button.outline()
            };
            button.on_click(move |_, window, cx| {
                this.update(cx, |list, cx| list.run_state_action(action, window, cx))
                    .ok();
            })
        });
        let mut empty = Empty::new().header(header);
        if !self.actions.is_empty() {
            empty = empty.content(EmptyContent::new().child(h_flex().gap_2().children(buttons)));
        }
        div()
            .id("list-state")
            .flex_1()
            .min_h_0()
            .w_full()
            .child(empty)
            .into_any_element()
    }
}
