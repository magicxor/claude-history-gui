//! The viewer toolbar (DESIGN §9.10): Summary|Truncated|Full, Thinking,
//! Timing, find bar, Outline, Resume, Fork, ⋯ menu. WP2.
//!
//! Every control dispatches the same action as its key (V-096) from the
//! transcript's focus, so the viewer's own handlers (and the menus' enabled
//! state) apply. Single-file windows have no Resume / Fork / Rename /
//! Delete; a target with an unknown source shows them disabled with the
//! tooltip `Unknown conversation type`.

use claude_history_core::view::{ToolDisplayMode, ViewPrefs};
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu};
use gpui_kit::component::{Disableable as _, IconName, Selectable as _, Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Action, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, TestSupportExt as _, Window, div, rems,
};

use super::ViewerPane;
use crate::actions::*;
use crate::keymap::CTX_VIEWER;
use crate::model::ViewerMode;
use crate::qa_ext::QaExt as _;

/// The disabled tooltip of actions that need a known source.
pub const UNKNOWN_SOURCE_TOOLTIP: &str = "Unknown conversation type";

/// What the toolbar shows.
pub struct ToolbarModel {
    pub prefs: ViewPrefs,
    pub mode: ViewerMode,
    /// A conversation is open.
    pub has_target: bool,
    /// Its source is known (Resume/Fork/Rename/Delete allowed).
    pub can_modify: bool,
    /// The header session ID differs from the file stem.
    pub header_id_differs: bool,
    /// The find input while a search is typed.
    pub find: Option<(Entity<InputState>, SharedString)>,
    pub outline_open: bool,
}

/// A toolbar button that focuses the transcript and dispatches `action`.
fn action_button(
    id: &'static str,
    label: &'static str,
    action: Box<dyn Action>,
    focus: &FocusHandle,
) -> Button {
    let focus = focus.clone();
    Button::new(id)
        .label(label)
        .xsmall()
        .ghost()
        .tooltip_with_action(label, action.as_ref(), Some(CTX_VIEWER))
        .on_click(move |_, window, cx| {
            focus.focus(window, cx);
            window.dispatch_action(action.boxed_clone(), cx);
        })
}

pub fn render_toolbar(
    model: ToolbarModel,
    focus: &FocusHandle,
    _window: &mut Window,
    _cx: &mut Context<ViewerPane>,
) -> impl IntoElement + use<> {
    let tool = model.prefs.tool_display;
    let enabled = model.has_target;
    let mode_button =
        |id: &'static str, label: &'static str, value: ToolDisplayMode, action: Box<dyn Action>| {
            let focus = focus.clone();
            Button::new(id)
                .label(label)
                .xsmall()
                .selected(tool == value)
                .disabled(!enabled)
                .on_click(move |_, window, cx| {
                    focus.focus(window, cx);
                    window.dispatch_action(action.boxed_clone(), cx);
                })
        };
    let toggle_button =
        |id: &'static str, label: &'static str, on: bool, action: Box<dyn Action>| {
            let focus = focus.clone();
            Button::new(id)
                .label(label)
                .xsmall()
                .outline()
                .selected(on)
                .disabled(!enabled)
                .tooltip_with_action(label, action.as_ref(), Some(CTX_VIEWER))
                .on_click(move |_, window, cx| {
                    focus.focus(window, cx);
                    window.dispatch_action(action.boxed_clone(), cx);
                })
        };
    let main = model.mode == ViewerMode::Main;
    let can_modify = model.can_modify;
    let header_id_differs = model.header_id_differs;
    let menu_focus = focus.clone();
    h_flex()
        .id("viewer-toolbar")
        .qa_id("viewer.toolbar")
        .test_support()
        .flex_wrap()
        .gap_2()
        .px_3()
        .py_1()
        .child(
            h_flex()
                .gap_1()
                .child(div().text_xs().child("Tools"))
                .child(
                    ButtonGroup::new("viewer-tools")
                        .outline()
                        .compact()
                        .child(mode_button(
                            "tools-summary",
                            "Summary",
                            ToolDisplayMode::Hidden,
                            Box::new(SetToolsSummary),
                        ))
                        .child(mode_button(
                            "tools-truncated",
                            "Truncated",
                            ToolDisplayMode::Truncated,
                            Box::new(SetToolsTruncated),
                        ))
                        .child(mode_button(
                            "tools-full",
                            "Full",
                            ToolDisplayMode::Full,
                            Box::new(SetToolsFull),
                        )),
                ),
        )
        .child(toggle_button(
            "toggle-thinking",
            "Thinking",
            model.prefs.show_thinking,
            Box::new(ToggleThinking),
        ))
        .child(toggle_button(
            "toggle-timing",
            "Timing",
            model.prefs.show_timing,
            Box::new(ToggleTiming),
        ))
        .when_some(model.find, |this, (input, status)| {
            this.child(
                div().w(rems(16.)).child(
                    Input::new(&input)
                        .small()
                        .cleanable(false)
                        .prefix(div().text_xs().child("/"))
                        .suffix(div().text_xs().child(status)),
                ),
            )
        })
        .child(div().flex_1())
        .child(
            action_button("toolbar-outline", "Outline", Box::new(ToggleOutline), focus)
                .selected(model.outline_open)
                .disabled(!enabled),
        )
        .when(main, |this| {
            this.child(
                action_button("toolbar-resume", "Resume", Box::new(Resume), focus)
                    .disabled(!enabled || !can_modify)
                    .when(enabled && !can_modify, |b| {
                        b.tooltip(UNKNOWN_SOURCE_TOOLTIP)
                    }),
            )
            .child(
                action_button("toolbar-fork", "Fork", Box::new(Fork), focus)
                    .disabled(!enabled || !can_modify)
                    .when(enabled && !can_modify, |b| {
                        b.tooltip(UNKNOWN_SOURCE_TOOLTIP)
                    }),
            )
        })
        .child(
            Button::new("toolbar-more")
                .icon(IconName::Ellipsis)
                .xsmall()
                .ghost()
                .disabled(!enabled)
                .tooltip("More actions")
                .accessibility_label("More actions")
                .dropdown_menu(move |menu: PopupMenu, window, cx| {
                    more_menu(
                        menu,
                        main,
                        can_modify,
                        header_id_differs,
                        &menu_focus,
                        window,
                        cx,
                    )
                }),
        )
}

/// The `⋯` menu (§9.10).
fn more_menu(
    menu: PopupMenu,
    main: bool,
    can_modify: bool,
    header_id_differs: bool,
    focus: &FocusHandle,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let menu = menu
        .action_context(focus.clone())
        .menu("Export…", Box::new(ExportConversation))
        .submenu("Copy to clipboard", window, cx, |menu, _, _| {
            menu.menu("Ledger (formatted)", Box::new(CopyAsLedger))
                .menu("Plain text", Box::new(CopyAsPlain))
                .menu("Markdown", Box::new(CopyAsMarkdown))
                .menu("JSONL (raw)", Box::new(CopyAsJsonl))
        })
        .menu("Copy message", Box::new(CopyFocusedMessage))
        .menu("Copy path", Box::new(CopyPath))
        .menu("Copy session ID", Box::new(CopySessionId))
        .when(header_id_differs, |menu| {
            menu.menu("Copy header session ID", Box::new(CopyHeaderSessionId))
        })
        .menu("Show file path", Box::new(ShowPath))
        .menu("Reveal in file manager", Box::new(RevealInFileManager))
        .separator()
        .when(main, |menu| {
            menu.menu_with_disabled("Rename…", Box::new(Rename), !can_modify)
        })
        .menu(
            "Search within conversation…",
            Box::new(SearchWithinConversation),
        )
        .menu("Read range…", Box::new(ReadRange));
    if main {
        menu.separator()
            .menu_with_disabled("Delete…", Box::new(Delete), !can_modify)
    } else {
        menu
    }
}
