//! Action routing shared by the window roots (DESIGN §8.1, §7.8). Frozen:
//! written by WP0, afterwards changed only by the integrator.
//!
//! * Panes handle the actions of their region themselves (they are on the
//!   focused dispatch path). The window roots (`Workspace`,
//!   `SingleFileWindow`) register the **fallback** for actions dispatched with
//!   no pane on the path ([`forward`]): list-view actions → `ListPane`;
//!   viewer actions → `ViewerPane` (toast `No conversation open` when it is
//!   empty; `StartSearch` focuses the viewer first); session actions → the
//!   `ActivePane` of that window, which resolves its own target.
//! * [`run_session_action`] maps a session action plus a captured target to
//!   the `ops` entry points (every pane uses it) and tells the pane what is
//!   left to do ([`SessionRoute`]): open the target in the viewer (with an
//!   agent-panel tab) or hand the action to the viewer.
//! * [`run_app_action`] handles the window-level commands (tools, settings,
//!   help, palette, theme, copy) for both window roots.
//! * [`on_actions!`] / [`capture_actions!`] / [`on_session_actions!`] /
//!   [`capture_session_actions!`] register one handler method for a list of
//!   action types.
//!
//! Menu enabled state (§8.9, §8.10) is "the action has a listener on the
//! dispatch path" (`Window::is_action_available`), so roots and panes
//! register a listener only where the action can run: `Quit` on both roots
//! (plus a global listener for focus-less dispatch), `CloseWindow` only on
//! single-file roots, `OpenJsonlFile` only outside picker mode,
//! `ShowConversations` only on single-file roots, and Resume/Fork/Rename/
//! Delete/SelectAndExit never in single-file windows.

use claude_history_core::export::{ExportFormat, ExportOptions};
use claude_history_core::theme::ThemePreference;
use gpui_kit::component::WindowExt as _;
use gpui_kit::{Action, App, Entity, Window};

use crate::actions::*;
use crate::deep_search::DeepSearchPane;
use crate::deep_search::panel::AgentPanel;
use crate::globals::app_state;
use crate::list::ListPane;
use crate::model::{
    AgentTab, ConversationTarget, DebugLogTab, HelpSection, OpenRequest, PaneFocus, RowMenuItem,
    SemanticReportMode,
};
use crate::viewer::ViewerPane;
use crate::{help, notify, ops, palette, settings, theme_tokens, tools};

/// Register `$handler` (a method `fn(&mut Self, &dyn Action, &mut Window,
/// &mut Context<Self>)`) for every listed action type on `$element`.
#[macro_export]
macro_rules! on_actions {
    ($element:expr, $cx:expr, $handler:path, [$($action:ty),* $(,)?]) => {
        $element
            $(.on_action($cx.listener(|this, action: &$action, window, cx| {
                $handler(this, action as &dyn ::gpui_kit::Action, window, cx)
            })))*
    };
}

/// [`on_actions!`] in the capture phase: `$handler` runs on the way down,
/// before any pane handles the action, and the action continues unless the
/// handler calls `cx.stop_propagation()`.
#[macro_export]
macro_rules! capture_actions {
    ($element:expr, $cx:expr, $handler:path, [$($action:ty),* $(,)?]) => {
        $element
            $(.capture_action($cx.listener(|this, action: &$action, window, cx| {
                $handler(this, action as &dyn ::gpui_kit::Action, window, cx)
            })))*
    };
}

/// [`on_actions!`] for every session action (target-resolving, §8.2).
#[macro_export]
macro_rules! on_session_actions {
    ($element:expr, $cx:expr, $handler:path) => {
        $crate::on_actions!(
            $element,
            $cx,
            $handler,
            [
                $crate::actions::OpenConversation,
                $crate::actions::Resume,
                $crate::actions::Fork,
                $crate::actions::Rename,
                $crate::actions::Delete,
                $crate::actions::ExportConversation,
                $crate::actions::YankOrCopyMessage,
                $crate::actions::CopyFocusedMessage,
                $crate::actions::CopyPath,
                $crate::actions::CopySessionId,
                $crate::actions::CopyHeaderSessionId,
                $crate::actions::ShowPath,
                $crate::actions::RevealInFileManager,
                $crate::actions::SelectAndExit,
                $crate::actions::SearchWithinConversation,
                $crate::actions::ToggleOutline,
                $crate::actions::ReadRange,
                $crate::actions::CopyAsLedger,
                $crate::actions::CopyAsPlain,
                $crate::actions::CopyAsMarkdown,
                $crate::actions::CopyAsJsonl,
            ]
        )
    };
}

/// [`capture_actions!`] for every action that acts on the list's selection
/// (the session actions, `ShowSemanticDetails` and `RowMenu`): the main
/// window flushes a pending debounced preview before the list handles them
/// (PD2, §7.8, L-085).
#[macro_export]
macro_rules! capture_session_actions {
    ($element:expr, $cx:expr, $handler:path) => {
        $crate::capture_actions!(
            $element,
            $cx,
            $handler,
            [
                $crate::actions::OpenConversation,
                $crate::actions::Resume,
                $crate::actions::Fork,
                $crate::actions::Rename,
                $crate::actions::Delete,
                $crate::actions::ExportConversation,
                $crate::actions::YankOrCopyMessage,
                $crate::actions::CopyFocusedMessage,
                $crate::actions::CopyPath,
                $crate::actions::CopySessionId,
                $crate::actions::CopyHeaderSessionId,
                $crate::actions::ShowPath,
                $crate::actions::RevealInFileManager,
                $crate::actions::SelectAndExit,
                $crate::actions::SearchWithinConversation,
                $crate::actions::ToggleOutline,
                $crate::actions::ReadRange,
                $crate::actions::CopyAsLedger,
                $crate::actions::CopyAsPlain,
                $crate::actions::CopyAsMarkdown,
                $crate::actions::CopyAsJsonl,
                $crate::actions::ShowSemanticDetails,
                $crate::actions::RowMenu,
            ]
        )
    };
}

/// [`on_actions!`] for every window-level command ([`run_app_action`]).
/// `OpenJsonlFile` (not in picker mode), `ShowConversations` (single-file
/// windows only) and `ShowProjectsDirectory` (only with a workspace, T-016)
/// are registered separately by the roots: an item is enabled exactly when a
/// listener exists (§8.10).
#[macro_export]
macro_rules! on_app_actions {
    ($element:expr, $cx:expr, $handler:path) => {
        $crate::on_actions!(
            $element,
            $cx,
            $handler,
            [
                $crate::actions::OpenSettings,
                $crate::actions::ShowShortcuts,
                $crate::actions::CommandPalette,
                $crate::actions::ShowDebugLog,
                $crate::actions::DeleteEmptyTranscripts,
                $crate::actions::GenerateSemanticCache,
                $crate::actions::ClearSemanticCache,
                $crate::actions::DebugSearch,
                $crate::actions::SemanticDebugSearch,
                $crate::actions::DeleteSessionById,
                $crate::actions::CheckForUpdates,
                $crate::actions::ShowAbout,
                $crate::actions::SetThemeSystem,
                $crate::actions::SetThemeLight,
                $crate::actions::SetThemeDark,
                $crate::actions::SmartCopy,
                $crate::actions::EditCopy,
                $crate::actions::SelectAllTranscript,
            ]
        )
    };
}

/// [`on_actions!`] for every action the window roots forward ([`forward`]).
#[macro_export]
macro_rules! on_fallback_actions {
    ($element:expr, $cx:expr, $handler:path) => {
        $crate::on_actions!(
            $element,
            $cx,
            $handler,
            [
                // list view
                $crate::actions::SetModeLexical,
                $crate::actions::SetModeSemantic,
                $crate::actions::SetScopeAll,
                $crate::actions::SetScopeWorkspace,
                $crate::actions::ToggleScope,
                $crate::actions::ToggleSearchMode,
                $crate::actions::ShowSemanticDetails,
                $crate::actions::Refresh,
                // viewer
                $crate::actions::CycleToolDisplay,
                $crate::actions::SetToolsSummary,
                $crate::actions::SetToolsTruncated,
                $crate::actions::SetToolsFull,
                $crate::actions::ToggleThinking,
                $crate::actions::ToggleTiming,
                $crate::actions::StartSearch,
                $crate::actions::NextMatch,
                $crate::actions::PrevMatch,
                // session (incl. the viewer's ToggleOutline / Yank / CopyFocusedMessage)
                $crate::actions::OpenConversation,
                $crate::actions::Resume,
                $crate::actions::Fork,
                $crate::actions::Rename,
                $crate::actions::Delete,
                $crate::actions::ExportConversation,
                $crate::actions::YankOrCopyMessage,
                $crate::actions::CopyFocusedMessage,
                $crate::actions::CopyPath,
                $crate::actions::CopySessionId,
                $crate::actions::CopyHeaderSessionId,
                $crate::actions::ShowPath,
                $crate::actions::RevealInFileManager,
                $crate::actions::SelectAndExit,
                $crate::actions::SearchWithinConversation,
                $crate::actions::ToggleOutline,
                $crate::actions::ReadRange,
                $crate::actions::CopyAsLedger,
                $crate::actions::CopyAsPlain,
                $crate::actions::CopyAsMarkdown,
                $crate::actions::CopyAsJsonl,
            ]
        )
    };
}

/// List-view actions forwarded to `ListPane`.
pub fn is_list_action(action: &dyn Action) -> bool {
    let a = action.as_any();
    a.is::<SetModeLexical>()
        || a.is::<SetModeSemantic>()
        || a.is::<SetScopeAll>()
        || a.is::<SetScopeWorkspace>()
        || a.is::<ToggleScope>()
        || a.is::<ToggleSearchMode>()
        || a.is::<ShowSemanticDetails>()
        || a.is::<Refresh>()
}

/// Viewer actions forwarded to `ViewerPane`.
pub fn is_viewer_action(action: &dyn Action) -> bool {
    let a = action.as_any();
    a.is::<CycleToolDisplay>()
        || a.is::<SetToolsSummary>()
        || a.is::<SetToolsTruncated>()
        || a.is::<SetToolsFull>()
        || a.is::<ToggleThinking>()
        || a.is::<ToggleTiming>()
        || a.is::<StartSearch>()
        || a.is::<NextMatch>()
        || a.is::<PrevMatch>()
        || a.is::<ToggleOutline>()
        || a.is::<YankOrCopyMessage>()
        || a.is::<CopyFocusedMessage>()
}

/// Session (target-resolving) actions.
pub fn is_session_action(action: &dyn Action) -> bool {
    let a = action.as_any();
    a.is::<OpenConversation>()
        || a.is::<Resume>()
        || a.is::<Fork>()
        || a.is::<Rename>()
        || a.is::<Delete>()
        || a.is::<ExportConversation>()
        || a.is::<YankOrCopyMessage>()
        || a.is::<CopyFocusedMessage>()
        || a.is::<CopyPath>()
        || a.is::<CopySessionId>()
        || a.is::<CopyHeaderSessionId>()
        || a.is::<ShowPath>()
        || a.is::<RevealInFileManager>()
        || a.is::<SelectAndExit>()
        || a.is::<SearchWithinConversation>()
        || a.is::<ToggleOutline>()
        || a.is::<ReadRange>()
        || a.is::<CopyAsLedger>()
        || a.is::<CopyAsPlain>()
        || a.is::<CopyAsMarkdown>()
        || a.is::<CopyAsJsonl>()
}

/// Session actions that need a known source (disabled for unknown targets).
pub fn needs_known_source(action: &dyn Action) -> bool {
    let a = action.as_any();
    a.is::<Resume>() || a.is::<Fork>() || a.is::<Rename>() || a.is::<Delete>()
}

/// Session actions a single-file window has no handler for (PD14): their
/// menu items are disabled there.
pub fn is_main_window_only(action: &dyn Action) -> bool {
    needs_known_source(action) || action.as_any().is::<SelectAndExit>()
}

/// The agent-panel tab a session action opens (`SearchWithinConversation` →
/// Within, `ToggleOutline` → Outline, `ReadRange` → Read).
pub fn agent_tab_for(action: &dyn Action) -> Option<AgentTab> {
    let a = action.as_any();
    if a.is::<SearchWithinConversation>() {
        Some(AgentTab::Within)
    } else if a.is::<ToggleOutline>() {
        Some(AgentTab::Outline)
    } else if a.is::<ReadRange>() {
        Some(AgentTab::Read)
    } else {
        None
    }
}

/// The session action a row-menu item stands for (§5.2). The list runs it on
/// the target captured under the item's token.
pub fn row_menu_action(item: RowMenuItem) -> Box<dyn Action> {
    match item {
        RowMenuItem::Open => Box::new(OpenConversation),
        RowMenuItem::Resume => Box::new(Resume),
        RowMenuItem::Fork => Box::new(Fork),
        RowMenuItem::Rename => Box::new(Rename),
        RowMenuItem::Export => Box::new(ExportConversation),
        RowMenuItem::CopyAs(ExportFormat::Ledger) => Box::new(CopyAsLedger),
        RowMenuItem::CopyAs(ExportFormat::Plain) => Box::new(CopyAsPlain),
        RowMenuItem::CopyAs(ExportFormat::Markdown) => Box::new(CopyAsMarkdown),
        RowMenuItem::CopyAs(ExportFormat::Jsonl) => Box::new(CopyAsJsonl),
        RowMenuItem::CopyPath => Box::new(CopyPath),
        RowMenuItem::CopySessionId => Box::new(CopySessionId),
        RowMenuItem::CopyHeaderSessionId => Box::new(CopyHeaderSessionId),
        RowMenuItem::Reveal => Box::new(RevealInFileManager),
        RowMenuItem::SearchWithin => Box::new(SearchWithinConversation),
        RowMenuItem::Outline => Box::new(ToggleOutline),
        RowMenuItem::Delete => Box::new(Delete),
    }
}

/// A short user-facing name of an action (for placeholders and toasts).
pub fn action_label(action: &dyn Action) -> &'static str {
    let a = action.as_any();
    if a.is::<Resume>() {
        "Resume"
    } else if a.is::<Fork>() {
        "Fork"
    } else if a.is::<Rename>() {
        "Rename"
    } else if a.is::<Delete>() {
        "Delete"
    } else if a.is::<ExportConversation>() {
        "Export"
    } else if a.is::<YankOrCopyMessage>() || a.is::<CopyFocusedMessage>() {
        "Copy message"
    } else if a.is::<SearchWithinConversation>() {
        "Search within conversation"
    } else if a.is::<ToggleOutline>() {
        "Outline"
    } else if a.is::<ReadRange>() {
        "Read range"
    } else if a.is::<SelectAndExit>() {
        "Print and exit"
    } else {
        "This command"
    }
}

/// Export options from the startup settings (list and fallback routes; the
/// viewer passes its own `ViewerModel::export_options`).
pub fn default_export_options(cx: &App) -> ExportOptions {
    let settings = &app_state(cx).settings;
    ExportOptions {
        show_tools: settings.show_tools,
        show_thinking: settings.show_thinking,
    }
}

/// What a pane still has to do after [`run_session_action`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionRoute {
    /// Done through the `ops` entry points (or ignored: Resume/Fork/Rename/
    /// Delete on a target with an unknown source).
    Done,
    /// Show the target in the main window's viewer: `OpenConversation`
    /// (`agent_tab: None`, keyboard focus into the viewer) or
    /// Search within / Outline / Read range (the agent panel on that tab,
    /// focused; D-029). The list emits `ListPaneEvent::OpenRequested`, Deep
    /// search `DeepSearchEvent::OpenHit`, the window root's fallback returns
    /// [`Forwarded::Open`]; the viewer itself opens its panel.
    Open { agent_tab: Option<AgentTab> },
    /// `CopyFocusedMessage`: only the viewer showing the target can do it.
    /// A pane other than the viewer propagates the action (`cx.propagate()`),
    /// so the window root forwards it to its viewer.
    Viewer,
    /// Not a session action.
    NotSession,
}

/// Run a session action on a captured `target` through the `ops` entry
/// points, or say what the pane has to do instead ([`SessionRoute`]).
/// Resume/Fork/Rename/Delete on a target with an unknown source do nothing.
pub fn run_session_action(
    action: &dyn Action,
    target: ConversationTarget,
    options: ExportOptions,
    window: &mut Window,
    cx: &mut App,
) -> SessionRoute {
    let a = action.as_any();
    if !is_session_action(action) {
        return SessionRoute::NotSession;
    }
    if needs_known_source(action) && !target.can_modify() {
        return SessionRoute::Done;
    }
    if a.is::<OpenConversation>() {
        return SessionRoute::Open { agent_tab: None };
    }
    if let Some(tab) = agent_tab_for(action) {
        return SessionRoute::Open {
            agent_tab: Some(tab),
        };
    }
    if a.is::<CopyFocusedMessage>() {
        return SessionRoute::Viewer;
    }
    if a.is::<Resume>() {
        ops::resume::resume(target, false, window, cx);
    } else if a.is::<Fork>() {
        ops::resume::resume(target, true, window, cx);
    } else if a.is::<Rename>() {
        ops::rename::open_rename_dialog(target, window, cx);
    } else if a.is::<Delete>() {
        ops::delete::open_delete_dialog(target, window, cx);
    } else if a.is::<ExportConversation>() {
        ops::export::open_export_menu(target, options, window, cx);
    } else if a.is::<YankOrCopyMessage>() {
        ops::export::open_copy_menu(target, options, window, cx);
    } else if a.is::<CopyAsLedger>() {
        ops::export::copy_as(target, ExportFormat::Ledger, options, window, cx);
    } else if a.is::<CopyAsPlain>() {
        ops::export::copy_as(target, ExportFormat::Plain, options, window, cx);
    } else if a.is::<CopyAsMarkdown>() {
        ops::export::copy_as(target, ExportFormat::Markdown, options, window, cx);
    } else if a.is::<CopyAsJsonl>() {
        ops::export::copy_as(target, ExportFormat::Jsonl, options, window, cx);
    } else if a.is::<CopyPath>() {
        ops::copy::copy_path(&target, window, cx);
    } else if a.is::<CopySessionId>() {
        ops::copy::copy_session_id(&target, window, cx);
    } else if a.is::<CopyHeaderSessionId>() {
        ops::copy::copy_header_session_id(&target, window, cx);
    } else if a.is::<ShowPath>() {
        ops::copy::show_path(&target, window, cx);
    } else if a.is::<RevealInFileManager>() {
        ops::copy::reveal(&target, window, cx);
    } else if a.is::<SelectAndExit>() {
        ops::select::select_and_exit(Some(target), window, cx);
    }
    SessionRoute::Done
}

/// The panes a window root forwards to.
#[derive(Clone, Default)]
pub struct RouteTargets {
    pub list: Option<Entity<ListPane>>,
    pub viewer: Option<Entity<ViewerPane>>,
    pub deep: Option<Entity<DeepSearchPane>>,
    /// The region that last had focus in this window (`ActivePane`).
    pub active: PaneFocus,
}

impl RouteTargets {
    fn agent_panel(&self, cx: &App) -> Option<Entity<AgentPanel>> {
        self.viewer
            .as_ref()
            .map(|viewer| viewer.read(cx).agent_panel().clone())
    }
}

/// What [`forward`] did.
#[derive(Clone, Debug, PartialEq)]
pub enum Forwarded {
    /// Nothing handled it: the root propagates.
    Unhandled,
    /// Handled (or answered with a toast).
    Handled,
    /// The window root opens this request in its viewer (a session action of
    /// Deep search or the agent panel that needs the viewer, §7.8).
    Open(Box<OpenRequest>),
}

impl Forwarded {
    fn from_bool(handled: bool) -> Self {
        if handled {
            Forwarded::Handled
        } else {
            Forwarded::Unhandled
        }
    }
}

/// The fallback for an action that reached the window root (no pane on the
/// dispatch path handled it). A session action goes to the `ActivePane`
/// first when that is the list or Deep search (their target, even for
/// Outline / Copy message, which otherwise act on the viewer's conversation).
pub fn forward(
    action: &dyn Action,
    targets: &RouteTargets,
    window: &mut Window,
    cx: &mut App,
) -> Forwarded {
    if is_session_action(action)
        && matches!(
            targets.active,
            PaneFocus::List | PaneFocus::ListToolbar | PaneFocus::DeepSearch
        )
    {
        return forward_session_action(action, targets, window, cx);
    }
    if is_viewer_action(action) {
        if let Some(viewer) = &targets.viewer {
            let empty = viewer.read(cx).target().is_none();
            if empty {
                notify::no_conversation_open(window, cx);
                return Forwarded::Handled;
            }
            if action.as_any().is::<StartSearch>() {
                viewer.update(cx, |viewer, cx| viewer.focus(window, cx));
            }
            return Forwarded::from_bool(
                viewer.update(cx, |viewer, cx| viewer.handle_forwarded(action, window, cx)),
            );
        }
        if !is_session_action(action) {
            return Forwarded::Unhandled;
        }
    }
    if is_list_action(action) {
        return match &targets.list {
            Some(list) => Forwarded::from_bool(
                list.update(cx, |list, cx| list.handle_forwarded(action, window, cx)),
            ),
            None => Forwarded::Unhandled,
        };
    }
    if is_session_action(action) {
        return forward_session_action(action, targets, window, cx);
    }
    Forwarded::Unhandled
}

/// Session actions with no pane on the path: the `ActivePane` of the window
/// resolves its own target; no target → `No conversation selected`.
fn forward_session_action(
    action: &dyn Action,
    targets: &RouteTargets,
    window: &mut Window,
    cx: &mut App,
) -> Forwarded {
    match targets.active {
        PaneFocus::List | PaneFocus::ListToolbar => {
            // `false` = the action needs the viewer (Copy message): below.
            if let Some(list) = &targets.list
                && list.update(cx, |list, cx| list.handle_forwarded(action, window, cx))
            {
                return Forwarded::Handled;
            }
        }
        PaneFocus::Viewer => {
            if let Some(viewer) = &targets.viewer {
                return Forwarded::from_bool(
                    viewer.update(cx, |viewer, cx| viewer.handle_forwarded(action, window, cx)),
                );
            }
        }
        PaneFocus::DeepSearch => {
            if let Some(deep) = &targets.deep {
                let target = deep.read(cx).selected_hit_target(cx);
                return run_with_target(action, target, targets, window, cx);
            }
        }
        PaneFocus::AgentPanel => {
            if let Some(panel) = targets.agent_panel(cx) {
                let target = panel.read(cx).selected_target();
                return run_with_target(action, target, targets, window, cx);
            }
        }
        PaneFocus::Other => {}
    }
    // Last resort: the viewer's conversation, else nothing selected.
    if let Some(viewer) = &targets.viewer
        && viewer.read(cx).target().is_some()
    {
        return Forwarded::from_bool(
            viewer.update(cx, |viewer, cx| viewer.handle_forwarded(action, window, cx)),
        );
    }
    notify::no_conversation_selected(window, cx);
    Forwarded::Handled
}

fn run_with_target(
    action: &dyn Action,
    target: Option<ConversationTarget>,
    targets: &RouteTargets,
    window: &mut Window,
    cx: &mut App,
) -> Forwarded {
    let Some(target) = target else {
        notify::no_conversation_selected(window, cx);
        return Forwarded::Handled;
    };
    let options = default_export_options(cx);
    match run_session_action(action, target.clone(), options, window, cx) {
        SessionRoute::Open { agent_tab } => Forwarded::Open(Box::new(
            OpenRequest::new(target, true).with_agent_tab(agent_tab),
        )),
        SessionRoute::Viewer => match &targets.viewer {
            Some(viewer) => Forwarded::from_bool(
                viewer.update(cx, |viewer, cx| viewer.handle_forwarded(action, window, cx)),
            ),
            None => Forwarded::Unhandled,
        },
        SessionRoute::Done => Forwarded::Handled,
        SessionRoute::NotSession => Forwarded::Unhandled,
    }
}

/// The help section of the region that had focus (F1 opens it there).
pub fn help_section_for(pane: PaneFocus) -> Option<HelpSection> {
    match pane {
        PaneFocus::List | PaneFocus::ListToolbar => Some(HelpSection::List),
        PaneFocus::Viewer => Some(HelpSection::Viewer),
        PaneFocus::AgentPanel => Some(HelpSection::AgentPanel),
        PaneFocus::DeepSearch => Some(HelpSection::DeepSearch),
        PaneFocus::Other => None,
    }
}

/// Window-level commands, the same for every window root. Returns `false`
/// for other actions.
pub fn run_app_action(
    action: &dyn Action,
    viewer: Option<&Entity<ViewerPane>>,
    active: PaneFocus,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let a = action.as_any();
    let local = {
        let state = app_state(cx);
        state.launch.args.local && state.workspace.is_some()
    };
    if a.is::<OpenSettings>() {
        settings::open_settings(None, window, cx);
    } else if a.is::<ShowShortcuts>() {
        help::open_shortcuts(help_section_for(active), window, cx);
    } else if a.is::<CommandPalette>() {
        palette::open(window, cx);
    } else if a.is::<OpenJsonlFile>() {
        if !app_state(cx).is_picker() {
            tools::open_jsonl_file_dialog(window, cx);
        }
    } else if a.is::<ShowConversations>() {
        crate::workspace::show_main_window(cx);
    } else if a.is::<ShowDebugLog>() {
        tools::open_debug_log_with(DebugLogTab::Messages, viewer, window, cx);
    } else if a.is::<DeleteEmptyTranscripts>() {
        tools::open_delete_empty(local, window, cx);
    } else if a.is::<GenerateSemanticCache>() {
        tools::open_generate_semantic_cache(local, window, cx);
    } else if a.is::<ClearSemanticCache>() {
        tools::open_clear_semantic_cache(window, cx);
    } else if a.is::<DebugSearch>() {
        tools::open_debug_search(None, local, window, cx);
    } else if a.is::<SemanticDebugSearch>() {
        tools::open_semantic_debug(
            None,
            SemanticReportMode::Search { top: 20 },
            local,
            window,
            cx,
        );
    } else if a.is::<ShowProjectsDirectory>() {
        tools::show_projects_directory(window, cx);
    } else if a.is::<DeleteSessionById>() {
        tools::open_delete_by_id(None, window, cx);
    } else if a.is::<CheckForUpdates>() {
        tools::check_for_updates(window, cx);
    } else if a.is::<ShowAbout>() {
        tools::open_about(window, cx);
    } else if a.is::<SetThemeSystem>() {
        theme_tokens::set_theme_preference(ThemePreference::System, true, window, cx);
    } else if a.is::<SetThemeLight>() {
        theme_tokens::set_theme_preference(ThemePreference::Light, true, window, cx);
    } else if a.is::<SetThemeDark>() {
        theme_tokens::set_theme_preference(ThemePreference::Dark, true, window, cx);
    } else if a.is::<SmartCopy>() || a.is::<EditCopy>() {
        smart_copy(viewer, window, cx);
    } else if a.is::<SelectAllTranscript>() {
        select_all(viewer, window, cx);
    } else {
        return false;
    }
    true
}

/// Edit → Select All (V-088): the focused input selects its text (gpui-kit's
/// own action), anywhere else the viewer selects the whole transcript.
pub fn select_all(viewer: Option<&Entity<ViewerPane>>, window: &mut Window, cx: &mut App) {
    if window.focused_input(cx).is_some() {
        window.dispatch_action(Box::new(gpui_kit::base::input::SelectAll), cx);
        return;
    }
    if let Some(viewer) = viewer {
        viewer.update(cx, |viewer, cx| {
            viewer.handle_forwarded(&SelectAllTranscript, window, cx);
        });
    }
}

/// Ctrl+C / Edit → Copy (§9.5): the focused input's selection if non-empty,
/// else the viewer's text selection; both through `services.clipboard_write`.
pub fn smart_copy(viewer: Option<&Entity<ViewerPane>>, window: &mut Window, cx: &mut App) {
    if let Some(input) = window.focused_input(cx) {
        let selected = if let Some(state) = input.as_input() {
            // `is_copyable`: a non-empty selection of an input that is not masked.
            let state = state.read(cx);
            if state.is_copyable() {
                state.selected_text().to_string()
            } else {
                String::new()
            }
        } else if let Some(state) = input.as_textarea() {
            state.read(cx).selected_text().to_string()
        } else {
            String::new()
        };
        if !selected.is_empty() {
            crate::services::services(cx).clipboard_write(selected, cx);
            return;
        }
    }
    if let Some(viewer) = viewer {
        viewer.update(cx, |viewer, cx| {
            viewer.handle_forwarded(&SmartCopy, window, cx);
        });
    }
}
