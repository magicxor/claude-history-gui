//! A conversation's parent, subagents and `/btw` forks in the viewer
//! (upstream 3c2ffeb: the header's family line, `s` lists the subagents and
//! forks, `P` opens the parent, `Esc` walks back). WP2.
//!
//! The picker is an in-app dialog (key context `FamilyMenu`): `j`/`k`/↑/↓
//! move, Enter opens, Esc, `q` or `s` cancels, rows are clickable. Each row shows
//! the kind (`fork` / `sub`), core's `conversation_label`, the message count
//! and `%m-%d %H:%M`, oldest first (upstream's `Children` dialog). Opening a
//! row asks the viewer to open it as a related conversation, which keeps
//! the conversation it came from on the viewer's back stack.

use gpui_kit::base::actions::{Cancel, Confirm, SelectDown, SelectUp};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    App, AppContext as _, Context, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, WeakEntity, Window, div,
};

use crate::keymap::CTX_FAMILY_MENU;
use crate::model::FamilyMember;
use crate::ops::copy::clear_dialog;
use crate::qa_ext::QaExt as _;
use crate::workspace::startup::register_dialog;

use super::ViewerPane;

/// The QA / probe id of the dialog.
pub const DIALOG_ID: &str = "family-menu";
/// The dialog's title.
pub const TITLE: &str = "Subagents and forks";
/// Upstream's status texts (core `browser::{NO_PARENT_STATUS,
/// PARENT_NOT_FOUND_STATUS, NO_CHILDREN_STATUS}`) in the GUI's words
/// (§12: "conversation").
pub const NO_PARENT: &str = "This conversation has no parent";
pub const PARENT_NOT_FOUND: &str = "Parent conversation not found";
pub const NO_CHILDREN: &str = "No subagents or forks in this conversation";

/// `fork` or `sub`, as upstream's picker tags a row.
pub fn kind_tag(member: &FamilyMember) -> &'static str {
    if member.fork { "fork" } else { "sub" }
}

/// A picker row's trailing text: `12 · 10-15 14:50`.
pub fn row_meta(member: &FamilyMember) -> String {
    format!(
        "{} · {}",
        member.message_count,
        member.timestamp.format("%m-%d %H:%M")
    )
}

/// Open the picker over `children` for `viewer`.
pub fn open_family_menu(
    viewer: WeakEntity<ViewerPane>,
    children: Vec<FamilyMember>,
    window: &mut Window,
    cx: &mut App,
) {
    let menu = cx.new(|cx| FamilyMenu {
        viewer,
        children,
        selected: 0,
        focus: cx.focus_handle(),
        closed: false,
    });
    register_dialog(cx, DIALOG_ID);
    #[cfg(any(test, feature = "qa"))]
    cx.set_global(LastFamilyMenu(menu.downgrade()));
    let first_render = std::rc::Rc::new(std::cell::Cell::new(true));
    let width = window.rem_size() * 36.0;
    window.open_dialog(cx, move |builder, window, cx| {
        if first_render.replace(false) {
            menu.read(cx).focus.clone().focus(window, cx);
        }
        let close = menu.clone();
        let cancel = menu.clone();
        builder
            .title(TITLE)
            .w(width)
            .child(menu.clone())
            .footer(div())
            .on_ok(|_, _, _| false)
            .on_cancel(move |_, _, cx| {
                cancel.update(cx, |menu, cx| menu.mark_closed(cx));
                true
            })
            .on_close(move |_, _, cx| close.update(cx, |menu, cx| menu.mark_closed(cx)))
    });
}

/// The most recently opened picker (tests and QA).
#[cfg(any(test, feature = "qa"))]
pub struct LastFamilyMenu(pub WeakEntity<FamilyMenu>);

#[cfg(any(test, feature = "qa"))]
impl gpui_kit::Global for LastFamilyMenu {}

/// The picker content (key context `FamilyMenu`).
pub struct FamilyMenu {
    viewer: WeakEntity<ViewerPane>,
    children: Vec<FamilyMember>,
    selected: usize,
    focus: FocusHandle,
    closed: bool,
}

impl Focusable for FamilyMenu {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl FamilyMenu {
    /// The highlighted row.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The rows, oldest first.
    pub fn children(&self) -> &[FamilyMember] {
        &self.children
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    fn mark_closed(&mut self, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.closed = true;
        clear_dialog(cx, DIALOG_ID);
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let last = self.children.len().saturating_sub(1) as isize;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
        cx.notify();
    }

    /// Open row `index` in the viewer and close the picker.
    pub fn pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        let Some(member) = self.children.get(index).cloned() else {
            return;
        };
        self.mark_closed(cx);
        window.close_dialog(cx);
        self.viewer
            .update(cx, |viewer, cx| {
                viewer.open_related(member.target, window, cx)
            })
            .ok();
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.mark_closed(cx);
        window.close_dialog(cx);
        self.viewer
            .update(cx, |viewer, cx| viewer.focus(window, cx))
            .ok();
    }
}

impl Render for FamilyMenu {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let selected = self.selected;
        let rows = self.children.iter().enumerate().map(|(ix, member)| {
            ListItem::new(("family-member", ix))
                .selected(ix == selected)
                .rounded_md()
                .child(
                    h_flex()
                        .gap_2()
                        .w_full()
                        .min_w_0()
                        .child(div().w_4().child(if ix == selected { "▶" } else { "" }))
                        .child(div().w_8().text_color(muted).child(kind_tag(member)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(SharedString::from(member.label.clone())),
                        )
                        .child(div().text_color(muted).child(row_meta(member))),
                )
                .on_click(cx.listener(move |this, _, window, cx| this.pick(ix, window, cx)))
                .qa_id(format!("family:row:{}", ix + 1))
        });
        v_flex()
            .id("family-menu")
            .key_context(CTX_FAMILY_MENU)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &SelectDown, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &SelectUp, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| {
                let selected = this.selected;
                this.pick(selected, window, cx)
            }))
            .on_action(cx.listener(|this, _: &Cancel, window, cx| this.cancel(window, cx)))
            .gap_0p5()
            .w_full()
            .children(rows)
            .child(
                Button::new("family-cancel")
                    .ghost()
                    .label("[Esc] Cancel")
                    .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
            )
    }
}
