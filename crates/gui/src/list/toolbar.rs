//! The list toolbar (DESIGN §5.1): mode Lexical/Semantic, the `You only`
//! toggle (upstream ae7ae39), scope All/Workspace, workspace picker, time
//! filter, sort order (fork), Refresh. WP1.

use claude_history_core::WorkspaceScope;
use claude_history_core::browser::{ListSearchMode, ListSortOrder};
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{
    Disableable as _, IconName, Selectable as _, Sizable as _, h_flex, v_flex,
};
use gpui_kit::{
    Action, Anchor, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, div,
};

use crate::actions::{
    Refresh, SetModeLexical, SetModeSemantic, SetScopeAll, SetScopeWorkspace, SetSortNewest,
    SetSortOldest, SetSortRelevance, ToggleUserOnly,
};
use crate::keymap::{CTX_LIST, CTX_LIST_TOOLBAR, CTX_WORKSPACE};
use crate::widgets::time_range::{TimeRangeChoice, time_range_button};

use super::ListPane;

/// The tooltip of the disabled Workspace segment (L-084).
pub const NO_WORKSPACE_TOOLTIP: &str = "Choose a workspace to filter by project";
/// The tooltip of the enabled Workspace segment.
pub const WORKSPACE_TOOLTIP: &str = "Only conversations of the workspace";
/// The label of the user-only toggle (upstream ae7ae39's `you only`).
pub const USER_ONLY_BUTTON: &str = "You only";
/// The user-only toggle's tooltip in lexical mode.
pub const USER_ONLY_TOOLTIP: &str = crate::menus::USER_ONLY_LABEL;
/// The user-only toggle's tooltip in semantic mode, which ignores it.
pub const USER_ONLY_SEMANTIC_TOOLTIP: &str =
    "Search only your own messages (lexical search; semantic search ignores it)";
/// The sort button's tooltip in lexical mode.
pub const SORT_TOOLTIP: &str = "Sort the results";
/// The sort button's tooltip in semantic mode, where it is disabled.
pub const SORT_SEMANTIC_TOOLTIP: &str =
    "Semantic results are ranked by similarity (sorting applies to lexical search)";

/// The sort button's label: the order in a word.
pub fn sort_button_label(order: ListSortOrder) -> &'static str {
    match order {
        ListSortOrder::Relevance => "Relevance",
        ListSortOrder::Newest => "Newest",
        ListSortOrder::Oldest => "Oldest",
    }
}

/// The action choosing `order` (toolbar menu, View → Sort).
pub fn sort_action(order: ListSortOrder) -> Box<dyn Action> {
    match order {
        ListSortOrder::Relevance => Box::new(SetSortRelevance),
        ListSortOrder::Newest => Box::new(SetSortNewest),
        ListSortOrder::Oldest => Box::new(SetSortOldest),
    }
}

/// The workspace button label: the folder's short name, else "No workspace".
pub fn workspace_label(workspace: Option<&WorkspaceScope>) -> String {
    match workspace {
        Some(scope) => scope
            .dir()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| scope.dir().display().to_string()),
        None => "No workspace".to_string(),
    }
}

/// What the toolbar shows (tests read it; the element is built from it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolbarModel {
    pub mode: ListSearchMode,
    /// The `You only` toggle is on (search matches only the user's own
    /// messages in lexical mode).
    pub user_only: bool,
    pub user_only_tooltip: &'static str,
    pub scope_workspace: bool,
    /// The Workspace segment is enabled (a workspace exists).
    pub workspace_enabled: bool,
    pub workspace_tooltip: &'static str,
    pub workspace_label: String,
    /// `Any time`, `Since 2d`, `Before 2026-07-20`
    pub time_label: SharedString,
    /// The order the rows are in (Relevance in semantic mode).
    pub sort_order: ListSortOrder,
    /// The sort button is enabled (lexical mode).
    pub sort_enabled: bool,
    pub sort_tooltip: &'static str,
    pub refresh_loading: bool,
}

impl ListPane {
    /// The toolbar's state.
    pub fn toolbar_model(&self) -> ToolbarModel {
        let enabled = self.has_workspace();
        ToolbarModel {
            mode: self.mode(),
            user_only: self.user_only(),
            user_only_tooltip: if self.mode() == ListSearchMode::Semantic {
                USER_ONLY_SEMANTIC_TOOLTIP
            } else {
                USER_ONLY_TOOLTIP
            },
            scope_workspace: self.scope_is_workspace(),
            workspace_enabled: enabled,
            workspace_tooltip: if enabled {
                WORKSPACE_TOOLTIP
            } else {
                NO_WORKSPACE_TOOLTIP
            },
            workspace_label: workspace_label(self.browser.workspace()),
            time_label: self.time_label.clone().unwrap_or_else(|| "Any time".into()),
            sort_order: self.effective_sort_order(),
            sort_enabled: self.sort_order_applies(),
            sort_tooltip: if self.sort_order_applies() {
                SORT_TOOLTIP
            } else {
                SORT_SEMANTIC_TOOLTIP
            },
            refresh_loading: self.is_refreshing() || self.is_loading(),
        }
    }
}

/// The two toolbar rows (key context `ListToolbar`, tracked by the pane's
/// toolbar focus handle: the F6 region between the search input and the
/// viewer).
pub fn render_toolbar(list: &ListPane, cx: &mut Context<ListPane>) -> impl IntoElement + use<> {
    let model = list.toolbar_model();
    let this = cx.entity();
    let this_for_time = this.downgrade();
    let current_range = TimeRangeChoice {
        filter: *list.time_filter().0,
        label: list.time_filter().1.cloned(),
    };
    v_flex()
        .id("list-toolbar")
        .key_context(CTX_LIST_TOOLBAR)
        .track_focus(&list.toolbar_focus)
        .px_2()
        .pb_2()
        .gap_1()
        .child(
            h_flex()
                .gap_2()
                .child(
                    ButtonGroup::new("list-mode")
                        .outline()
                        .compact()
                        .child(
                            Button::new("mode-lexical")
                                .label("Lexical")
                                .xsmall()
                                .selected(model.mode == ListSearchMode::Lexical)
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(Box::new(SetModeLexical), cx)
                                }),
                        )
                        .child(
                            Button::new("mode-semantic")
                                .label("Semantic")
                                .xsmall()
                                .selected(model.mode == ListSearchMode::Semantic)
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(Box::new(SetModeSemantic), cx)
                                }),
                        ),
                )
                .child(
                    Button::new("user-only")
                        .label(USER_ONLY_BUTTON)
                        .xsmall()
                        .outline()
                        .compact()
                        .selected(model.user_only)
                        .tooltip_with_action(
                            model.user_only_tooltip,
                            &ToggleUserOnly,
                            Some(CTX_LIST),
                        )
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(ToggleUserOnly), cx)
                        }),
                )
                .child(
                    ButtonGroup::new("list-scope")
                        .outline()
                        .compact()
                        .child(
                            Button::new("scope-all")
                                .label("All")
                                .xsmall()
                                .selected(!model.scope_workspace)
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(Box::new(SetScopeAll), cx)
                                }),
                        )
                        .child(
                            Button::new("scope-workspace")
                                .label("Workspace")
                                .xsmall()
                                .selected(model.scope_workspace)
                                .disabled(!model.workspace_enabled)
                                .tooltip(model.workspace_tooltip)
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(Box::new(SetScopeWorkspace), cx)
                                }),
                        ),
                ),
        )
        .child(
            h_flex()
                // A narrow pane wraps the dropdowns instead of clipping them
                // (and pushing Refresh out of the pane).
                .flex_wrap()
                .gap_2()
                .gap_y_1()
                .child(
                    Button::new("workspace-picker")
                        .icon(IconName::FolderOpen)
                        .label(model.workspace_label.clone())
                        .xsmall()
                        .outline()
                        .dropdown_caret(true)
                        .tooltip("Choose the workspace")
                        .dropdown_menu(move |menu, window, cx| {
                            super::workspace_picker::build_menu(menu, &this, window, cx)
                        }),
                )
                .child(time_range_button(
                    "time-filter",
                    current_range,
                    move |choice, _, cx| {
                        let label = choice.label.map(|label| label.to_string());
                        this_for_time
                            .update(cx, |list, cx| {
                                list.set_time_filter(choice.filter, label, cx)
                            })
                            .ok();
                    },
                ))
                .child(sort_button(&model))
                .child(div().flex_1())
                .child(
                    Button::new("refresh")
                        .icon(IconName::RefreshCw)
                        .xsmall()
                        .ghost()
                        .loading(model.refresh_loading)
                        .accessibility_label("Refresh")
                        .tooltip_with_action("Refresh", &Refresh, Some(CTX_WORKSPACE))
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(Refresh), cx)),
                ),
        )
}

/// The sort order dropdown: Relevance, Newest first, Oldest first (each a
/// `SetSort*` action, so the View menu and the palette offer the same).
fn sort_button(model: &ToolbarModel) -> impl IntoElement + use<> {
    let current = model.sort_order;
    Button::new("sort-order")
        .icon(match current {
            ListSortOrder::Oldest => IconName::SortAscending,
            ListSortOrder::Relevance | ListSortOrder::Newest => IconName::SortDescending,
        })
        .label(sort_button_label(current))
        .xsmall()
        .outline()
        .dropdown_caret(true)
        .disabled(!model.sort_enabled)
        .tooltip(model.sort_tooltip)
        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _window, _cx| {
            ListSortOrder::ALL.into_iter().fold(menu, |menu, order| {
                menu.item(
                    PopupMenuItem::new(order.label())
                        .checked(order == current)
                        .on_click(move |_, window, cx| {
                            window.dispatch_action(sort_action(order), cx)
                        }),
                )
            })
        })
}
