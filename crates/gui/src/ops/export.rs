//! Export to a file and copy to the clipboard (PD8, DESIGN §5.9, §11.11). WP3.
//!
//! * The menu dialog (key context `ExportMenu`): `1`–`4` pick, `j`/`k`/↑/↓
//!   move, Enter picks, Esc cancels, rows are clickable.
//! * Export then asks for a path through `PlatformServices::prompt_save_path`
//!   (default name `conversation-<timestamp>.<ext>`, default folder = the
//!   remembered export folder, else `<home>/Downloads`, else `<home>`), appends
//!   the format's extension to an extension-less answer and asks
//!   `Replace "<name>"?` when that path exists (Enter ignored). A failing save
//!   dialog (Linux without the portal) falls back to the default folder.
//! * Everything is computed from the captured target and the options captured
//!   with it; writing and generating run off the UI thread.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use chrono::Local;
use claude_history_core::export::{
    COPIED_TO_CLIPBOARD, ExportFormat, ExportOptions, default_export_file_name, export_to_path,
    exported_status, generate_export,
};
use gpui_kit::base::actions::{Cancel, Confirm, SelectDown, SelectUp};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    App, AppContext as _, Context, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Window, div,
};

use crate::actions::{ExportPick1, ExportPick2, ExportPick3, ExportPick4};
use crate::globals::{OpenTargets, app_state};
use crate::model::ConversationTarget;
use crate::notify::Toast;
use crate::ops::copy::clear_dialog;
use crate::persist;
use crate::qa_ext::QaExt as _;
use crate::services::services;
use crate::widgets::{ConfirmOptions, ConfirmOutcome, open_destructive_confirm};
use crate::workspace::startup::register_dialog;

/// QA ids of the dialogs.
pub const EXPORT_DIALOG_ID: &str = "export-menu";
pub const COPY_DIALOG_ID: &str = "copy-menu";
pub const REPLACE_DIALOG_ID: &str = "export-replace";

/// Which menu is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuMode {
    /// `Export to file`: the save prompt follows.
    Export,
    /// `Copy to clipboard`.
    Copy,
}

impl MenuMode {
    fn title(self) -> &'static str {
        match self {
            MenuMode::Export => "Export to file",
            MenuMode::Copy => "Copy to clipboard",
        }
    }

    fn dialog_id(self) -> &'static str {
        match self {
            MenuMode::Export => EXPORT_DIALOG_ID,
            MenuMode::Copy => COPY_DIALOG_ID,
        }
    }
}

/// The Export to file menu (formats 1–4), then the save prompt.
pub fn open_export_menu(
    target: ConversationTarget,
    opts: ExportOptions,
    window: &mut Window,
    cx: &mut App,
) {
    open_menu(MenuMode::Export, target, opts, window, cx);
}

/// The Copy to clipboard menu (formats 1–4).
pub fn open_copy_menu(
    target: ConversationTarget,
    opts: ExportOptions,
    window: &mut Window,
    cx: &mut App,
) {
    open_menu(MenuMode::Copy, target, opts, window, cx);
}

fn open_menu(
    mode: MenuMode,
    target: ConversationTarget,
    opts: ExportOptions,
    window: &mut Window,
    cx: &mut App,
) {
    let registration = Rc::new(Cell::new(None::<u64>));
    let menu = cx.new(|cx| ExportMenu {
        target: target.clone(),
        opts,
        mode,
        selected: 0,
        focus: cx.focus_handle(),
        closed: false,
        registration: registration.clone(),
    });
    let id = {
        let menu = menu.downgrade();
        OpenTargets::register(
            cx,
            window.window_handle(),
            target.path.clone(),
            target.title.clone(),
            Box::new(move |window, cx| {
                if let Some(menu) = menu.upgrade() {
                    menu.update(cx, |menu, cx| menu.mark_closed(cx));
                }
                window.close_dialog(cx);
            }),
        )
    };
    registration.set(Some(id));
    register_dialog(cx, mode.dialog_id());
    #[cfg(test)]
    cx.set_global(LastExportMenu(menu.downgrade()));

    let first_render = Rc::new(Cell::new(true));
    let width = window.rem_size() * 24.0;
    window.open_dialog(cx, move |builder, window, cx| {
        if first_render.replace(false) {
            menu.read(cx).focus.clone().focus(window, cx);
        }
        let close = menu.clone();
        let cancel = menu.clone();
        builder
            .title(mode.title())
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

/// The most recently opened export / copy menu (tests).
#[cfg(test)]
pub struct LastExportMenu(pub gpui_kit::WeakEntity<ExportMenu>);

#[cfg(test)]
impl gpui_kit::Global for LastExportMenu {}

/// The menu content (key context `ExportMenu`).
pub struct ExportMenu {
    target: ConversationTarget,
    opts: ExportOptions,
    mode: MenuMode,
    selected: usize,
    focus: FocusHandle,
    closed: bool,
    registration: Rc<Cell<Option<u64>>>,
}

impl Focusable for ExportMenu {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ExportMenu {
    /// The highlighted row (0–3).
    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn mode(&self) -> MenuMode {
        self.mode
    }

    pub fn target(&self) -> &ConversationTarget {
        &self.target
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    fn mark_closed(&mut self, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.closed = true;
        if let Some(id) = self.registration.take() {
            OpenTargets::unregister(cx, id);
        }
        clear_dialog(cx, self.mode.dialog_id());
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = ExportFormat::ALL.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(count) as usize;
        cx.notify();
    }

    /// Pick format `index` (0–3): close the menu, then copy or ask for a path.
    pub fn pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(format) = ExportFormat::from_index(index) else {
            return;
        };
        if self.closed {
            return;
        }
        self.mark_closed(cx);
        window.close_dialog(cx);
        let (target, opts) = (self.target.clone(), self.opts);
        match self.mode {
            MenuMode::Copy => copy_as(target, format, opts, window, cx),
            MenuMode::Export => start_export(target, format, opts, window, cx),
        }
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.mark_closed(cx);
        window.close_dialog(cx);
    }
}

impl Render for ExportMenu {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let selected = self.selected;
        let rows = ExportFormat::ALL
            .into_iter()
            .enumerate()
            .map(|(ix, format)| {
                // A semantic list item: the selection is the item's own
                // persistent state, the hover fill is the list's.
                ListItem::new(("export-format", ix))
                    .selected(ix == selected)
                    .rounded_md()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().w_4().child(if ix == selected { "▶" } else { "" }))
                            .child(div().text_color(muted).child(format!("[{}]", ix + 1)))
                            .child(div().child(format.label())),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| this.pick(ix, window, cx)))
                    .qa_id(format!("export:format:{}", ix + 1))
            });
        v_flex()
            .id("export-menu")
            .key_context("ExportMenu")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &ExportPick1, window, cx| this.pick(0, window, cx)))
            .on_action(cx.listener(|this, _: &ExportPick2, window, cx| this.pick(1, window, cx)))
            .on_action(cx.listener(|this, _: &ExportPick3, window, cx| this.pick(2, window, cx)))
            .on_action(cx.listener(|this, _: &ExportPick4, window, cx| this.pick(3, window, cx)))
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
                Button::new("export-cancel")
                    .ghost()
                    .label("[Esc] Cancel")
                    .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
            )
    }
}

// ----- copy to the clipboard ------------------------------------------------------------

/// Copy the whole conversation as `format` (background `generate_export`).
pub fn copy_as(
    target: ConversationTarget,
    format: ExportFormat,
    opts: ExportOptions,
    window: &mut Window,
    cx: &mut App,
) {
    let path = target.path.clone();
    let task = cx
        .background_executor()
        .spawn(async move { generate_export(&path, format, opts) });
    window
        .spawn(&*cx, async move |cx| {
            let result = task.await;
            cx.update(|window, cx| match result {
                Ok(text) => {
                    services(cx).clipboard_write(text, cx);
                    Toast::success(COPIED_TO_CLIPBOARD)
                        .replacing::<CopyAsToast>("copy-as")
                        .show(window, cx);
                }
                Err(error) => {
                    Toast::error(format!("Failed to read: {error}")).show(window, cx);
                }
            })
            .ok();
        })
        .detach();
}

struct CopyAsToast;

// ----- export to a file ------------------------------------------------------------------

/// The folder the save prompt starts in: the remembered export folder, else
/// `<home>/Downloads`, else `<home>` (C-042).
pub fn default_export_dir(cx: &App) -> PathBuf {
    let state = app_state(cx);
    let home = state.env.home_dir.clone();
    let downloads = home.as_ref().map(|home| home.join("Downloads"));
    persist::export_dir_or_default(
        state.ui.last_export_dir.as_deref(),
        downloads.as_deref(),
        home.as_deref(),
    )
    .or(home)
    .unwrap_or_else(|| PathBuf::from("."))
}

/// `path` with `.<ext>` appended when it has no extension (`x` → `x.md`).
pub fn with_default_extension(path: PathBuf, format: ExportFormat) -> PathBuf {
    if path.extension().is_some() {
        return path;
    }
    let mut text = path.into_os_string();
    text.push(".");
    text.push(format.extension());
    PathBuf::from(text)
}

fn start_export(
    target: ConversationTarget,
    format: ExportFormat,
    opts: ExportOptions,
    window: &mut Window,
    cx: &mut App,
) {
    let dir = default_export_dir(cx);
    let name = default_export_file_name(format, Local::now());
    let done = {
        let dir = dir.clone();
        let name = name.clone();
        move |answer: Result<Option<PathBuf>, String>, window: &mut Window, cx: &mut App| {
            match answer {
                // Cancelled: nothing happens.
                Ok(None) => {}
                Ok(Some(path)) => chosen(target, format, opts, path, window, cx),
                Err(error) => {
                    save_dialog_failed(target, format, opts, error, dir, name, window, cx)
                }
            }
        }
    };
    services(cx).prompt_save_path(&dir, &name, window, cx, Box::new(done));
}

/// The save dialog could not be shown: offer the default folder.
#[allow(clippy::too_many_arguments)]
fn save_dialog_failed(
    target: ConversationTarget,
    format: ExportFormat,
    opts: ExportOptions,
    error: String,
    dir: PathBuf,
    name: String,
    window: &mut Window,
    cx: &mut App,
) {
    Toast::error(format!("Can't open the save dialog: {error}"))
        .action("Export to Downloads", move |window, cx| {
            write_export(target.clone(), format, opts, dir.join(&name), window, cx);
        })
        .show(window, cx);
}

/// The save prompt answered with `path`.
fn chosen(
    target: ConversationTarget,
    format: ExportFormat,
    opts: ExportOptions,
    path: PathBuf,
    window: &mut Window,
    cx: &mut App,
) {
    let appended = path.extension().is_none();
    let dest = with_default_extension(path, format);
    // The native dialog's overwrite prompt covered the typed name only: a
    // name that gained an extension needs its own question.
    if appended && dest.exists() {
        confirm_replace(target, format, opts, dest, window, cx);
    } else {
        write_export(target, format, opts, dest, window, cx);
    }
}

/// The title of the replace prompt (X-003).
pub fn replace_title(file_name: &str) -> String {
    format!("Replace \"{file_name}\"?")
}

fn confirm_replace(
    target: ConversationTarget,
    format: ExportFormat,
    opts: ExportOptions,
    dest: PathBuf,
    window: &mut Window,
    cx: &mut App,
) {
    let file_name = dest
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| dest.display().to_string());
    let folder = dest
        .parent()
        .map(|dir| dir.display().to_string())
        .unwrap_or_default();
    let slot: Rc<std::cell::RefCell<Option<crate::widgets::ConfirmControl>>> =
        Rc::new(std::cell::RefCell::new(None));
    let id = {
        let slot = slot.clone();
        OpenTargets::register(
            cx,
            window.window_handle(),
            target.path.clone(),
            target.title.clone(),
            Box::new(move |_, cx| {
                let control = slot.borrow().clone();
                if let Some(control) = control {
                    control.close(cx);
                }
            }),
        )
    };
    register_dialog(cx, REPLACE_DIALOG_ID);
    let options = ConfirmOptions::destructive(replace_title(&file_name), "Replace")
        .description(format!("A file with this name already exists in {folder}."))
        .on_closed(move |cx| {
            OpenTargets::unregister(cx, id);
            clear_dialog(cx, REPLACE_DIALOG_ID);
        });
    let control = open_destructive_confirm(options, window, cx, move |control, window, cx| {
        // Close first, then write: the write may toast but opens no dialog.
        control.close(cx);
        write_export(target.clone(), format, opts, dest.clone(), window, cx);
        ConfirmOutcome::Close
    });
    *slot.borrow_mut() = Some(control);
}

/// Write the export in the background, then toast `Exported to <path>` with a
/// Reveal action (the folder is remembered) or the `ExportError` text.
pub fn write_export(
    target: ConversationTarget,
    format: ExportFormat,
    opts: ExportOptions,
    dest: PathBuf,
    window: &mut Window,
    cx: &mut App,
) {
    let source = target.path.clone();
    let task = {
        let dest = dest.clone();
        cx.background_executor()
            .spawn(async move { export_to_path(&source, format, opts, &dest) })
    };
    window
        .spawn(&*cx, async move |cx| {
            let result = task.await;
            cx.update(|window, cx| match result {
                Ok(()) => {
                    if let Some(dir) = dest.parent().filter(|dir| !dir.as_os_str().is_empty()) {
                        persist::remember_export_dir(dir, cx);
                    }
                    let revealed: PathBuf = dest.clone();
                    let message: SharedString = exported_status(&dest.display().to_string()).into();
                    Toast::success(message)
                        .action("Reveal", move |_, cx| reveal_file(&revealed, cx))
                        .show(window, cx);
                }
                Err(error) => {
                    Toast::error(error.to_string()).show(window, cx);
                }
            })
            .ok();
        })
        .detach();
}

fn reveal_file(path: &Path, cx: &mut App) {
    services(cx).reveal_path(path, cx);
}
