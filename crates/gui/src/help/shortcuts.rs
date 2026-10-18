//! The Keyboard shortcuts dialog (DESIGN §5.17, §8.7): `keymap::help_rows`
//! grouped by section, listed when `keymap::help_row_visible`, labelled with
//! `HelpRow::label_for` and the live keys of `keymap::help_keys_for` (main or
//! single-file window). WP6.
//!
//! The keys come from the live keymap, so a rebinding (Settings → Keys) shows
//! up the next time the dialog opens; rows that depend on a binding (Ctrl+T,
//! Ctrl+S) disappear while it is unbound (C-029). The dialog opens scrolled to
//! the section of the region that had focus; it scrolls natively (clamped:
//! the last row stays visible, L-069 quirk fix).

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, Global, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, WeakEntity, Window, div,
    rems,
};

use gpui_kit::{KeyBinding, point, px};

use crate::globals::main_panes;
use crate::keymap;
use crate::model::HelpSection;

/// The dialog width in rem.
pub const DIALOG_WIDTH_REM: f32 = 40.0;
/// The scrolling list's height in rem.
pub const LIST_HEIGHT_REM: f32 = 30.0;
/// The key context of the dialog content.
pub const SHORTCUTS_CONTEXT: &str = "ShortcutsDialog";

gpui_kit::actions!(
    shortcuts_dialog,
    [
        ScrollLineDown,
        ScrollLineUp,
        ScrollPageDown,
        ScrollPageUp,
        ScrollHalfPageDown,
        ScrollHalfPageUp,
        ScrollTop,
        CloseDialog,
    ]
);

/// The dialog's keys (the TUI help overlay's, L-069): `j`/`k`/arrows scroll,
/// PgDn/PgUp and `d`/`u` page, `g`/Home jump to the top, `?`, `q` and Esc close.
pub fn bindings() -> Vec<KeyBinding> {
    let c = Some(SHORTCUTS_CONTEXT);
    vec![
        KeyBinding::new("down", ScrollLineDown, c),
        KeyBinding::new("j", ScrollLineDown, c),
        KeyBinding::new("up", ScrollLineUp, c),
        KeyBinding::new("k", ScrollLineUp, c),
        KeyBinding::new("pagedown", ScrollPageDown, c),
        KeyBinding::new("pageup", ScrollPageUp, c),
        KeyBinding::new("d", ScrollHalfPageDown, c),
        KeyBinding::new("u", ScrollHalfPageUp, c),
        KeyBinding::new("home", ScrollTop, c),
        KeyBinding::new("g", ScrollTop, c),
        KeyBinding::new("?", CloseDialog, c),
        KeyBinding::new("q", CloseDialog, c),
        KeyBinding::new("escape", CloseDialog, c),
    ]
}

/// One row of the dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutRow {
    pub section: HelpSection,
    /// The live keys joined with ` / ` (`F2`, `j / ↓`).
    pub keys: SharedString,
    pub label: SharedString,
}

/// Whether `window` is a single-file window (the app has no main window or
/// the main window is another one).
pub fn is_single_file_window(window: &Window, cx: &App) -> bool {
    main_panes(cx).window != Some(window.window_handle())
}

/// Every visible row for a main or single-file window, in section order.
pub fn shortcut_rows(single_file: bool, cx: &App) -> Vec<ShortcutRow> {
    keymap::help_rows()
        .iter()
        .filter(|row| keymap::help_row_visible(row, single_file, cx))
        .map(|row| ShortcutRow {
            section: row.section,
            keys: keymap::help_keys_for(row, single_file, cx),
            label: row.label_for(single_file).into(),
        })
        .collect()
}

/// The sections that have at least one visible row, in order.
pub fn visible_sections(rows: &[ShortcutRow]) -> Vec<HelpSection> {
    HelpSection::ALL
        .into_iter()
        .filter(|section| rows.iter().any(|row| row.section == *section))
        .collect()
}

/// The dialog content.
pub struct ShortcutsDialog {
    focus: FocusHandle,
    scroll: ScrollHandle,
    rows: Vec<ShortcutRow>,
    single_file: bool,
}

impl Focusable for ShortcutsDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ShortcutsDialog {
    pub fn new(single_file: bool, section: Option<HelpSection>, cx: &mut Context<Self>) -> Self {
        let rows = shortcut_rows(single_file, cx);
        let scroll = ScrollHandle::new();
        if let Some(section) = section
            && let Some(ix) = visible_sections(&rows)
                .iter()
                .position(|candidate| *candidate == section)
        {
            scroll.scroll_to_top_of_item(ix);
        }
        Self {
            focus: cx.focus_handle(),
            scroll,
            rows,
            single_file,
        }
    }

    /// Scroll the list by `delta` pixels (positive = down), clamped so the
    /// first and last rows stay reachable.
    pub fn scroll_by(&self, delta: f32, cx: &mut Context<Self>) {
        let max = f32::from(self.scroll.max_offset().y);
        let current = -f32::from(self.scroll.offset().y);
        let target = (current + delta).clamp(0.0, max.max(0.0));
        self.scroll
            .set_offset(point(self.scroll.offset().x, px(-target)));
        cx.notify();
    }

    fn page(&self) -> f32 {
        let height = f32::from(self.scroll.bounds().size.height);
        if height > 0.0 { height } else { 400.0 }
    }

    /// The rows the dialog lists.
    pub fn rows(&self) -> &[ShortcutRow] {
        &self.rows
    }

    pub fn is_single_file(&self) -> bool {
        self.single_file
    }

    /// The list's scroll offset and its largest offset (both as positive
    /// distances in pixels); the offset never exceeds the largest one, so the
    /// last row stays visible (L-069).
    pub fn scroll_extent(&self) -> (f32, f32) {
        (
            -f32::from(self.scroll.offset().y),
            f32::from(self.scroll.max_offset().y),
        )
    }
}

/// The open dialog (tests and QA find it here).
#[derive(Default)]
struct OpenDialog(Option<WeakEntity<ShortcutsDialog>>);

impl Global for OpenDialog {}

/// The most recently opened Shortcuts dialog, while it exists.
pub fn current(cx: &App) -> Option<Entity<ShortcutsDialog>> {
    cx.try_global::<OpenDialog>()
        .and_then(|open| open.0.as_ref())
        .and_then(WeakEntity::upgrade)
}

impl Render for ShortcutsDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, border) = (theme.muted_foreground, theme.border);
        let mono = theme.mono_font_family.clone();
        let sections = visible_sections(&self.rows);
        let rows = &self.rows;
        v_flex()
            .id("shortcuts-dialog")
            .key_context(SHORTCUTS_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &ScrollLineDown, _, cx| this.scroll_by(40.0, cx)))
            .on_action(cx.listener(|this, _: &ScrollLineUp, _, cx| this.scroll_by(-40.0, cx)))
            .on_action(
                cx.listener(|this, _: &ScrollPageDown, _, cx| this.scroll_by(this.page(), cx)),
            )
            .on_action(
                cx.listener(|this, _: &ScrollPageUp, _, cx| this.scroll_by(-this.page(), cx)),
            )
            .on_action(cx.listener(|this, _: &ScrollHalfPageDown, _, cx| {
                this.scroll_by(this.page() / 2.0, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollHalfPageUp, _, cx| {
                this.scroll_by(-this.page() / 2.0, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollTop, _, cx| this.scroll_by(-1.0e9, cx)))
            .on_action(|_: &CloseDialog, window, cx| window.close_dialog(cx))
            .w_full()
            .gap_3()
            .child(
                v_flex()
                    .id("shortcuts-list")
                    .test_support()
                    .h(rems(LIST_HEIGHT_REM))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .vertical_scrollbar(&self.scroll)
                    .gap_3()
                    .pr_2()
                    .children(sections.into_iter().map(|section| {
                        v_flex()
                            .id(SharedString::from(format!(
                                "shortcuts-section-{}",
                                section.title()
                            )))
                            .gap_1()
                            .child(
                                div()
                                    .pb_1()
                                    .border_b_1()
                                    .border_color(border)
                                    .text_sm()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .child(section.title()),
                            )
                            .children(rows.iter().filter(|row| row.section == section).map(|row| {
                                h_flex()
                                    .gap_3()
                                    .items_start()
                                    .text_sm()
                                    .child(
                                        div()
                                            .w(rems(11.))
                                            .flex_none()
                                            .font_family(mono.clone())
                                            .child(row.keys.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_color(muted)
                                            .child(row.label.clone()),
                                    )
                            }))
                    })),
            )
            .child(
                h_flex().justify_end().child(
                    Button::new("shortcuts-close")
                        .label("Close")
                        .small()
                        .primary()
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                ),
            )
    }
}

/// Open the dialog in `window`.
pub fn open(
    section: Option<HelpSection>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<ShortcutsDialog> {
    let single_file = is_single_file_window(window, cx);
    let dialog = cx.new(|cx| ShortcutsDialog::new(single_file, section, cx));
    cx.set_global(OpenDialog(Some(dialog.downgrade())));
    let first_render = std::rc::Rc::new(std::cell::Cell::new(true));
    let width = window.rem_size() * DIALOG_WIDTH_REM;
    let content = dialog.clone();
    window.open_dialog(cx, move |builder, window, cx| {
        if first_render.replace(false) {
            content.read(cx).focus.clone().focus(window, cx);
        }
        builder
            .title("Keyboard shortcuts")
            .w(width)
            .child(content.clone())
            .footer(div())
    });
    dialog
}
