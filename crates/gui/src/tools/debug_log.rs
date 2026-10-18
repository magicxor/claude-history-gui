//! Tools → Debug log… (DESIGN §5.14; T-015, T-022). WP5.
//!
//! Three tabs: Messages (the `DebugBuffer`, filtered by the level selector,
//! which is also the capture level of the next load, T-022), Parse errors (of
//! the conversation the invoking window's viewer showed when the dialog
//! opened; `Write to debug.log` appends core's block) and debug.log (its last
//! 2,000 lines).

use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;
use std::time::Duration;

use claude_history_core::debug::DebugLevel;
use claude_history_core::history::{Conversation, Source};
use claude_history_core::maintenance::{ParseErrorReport, log_parse_errors, parse_error_report};
use gpui_kit::base::Root;
use gpui_kit::component::button::{Button, ButtonGroup};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, Styled as _, Task, Window, div, rems,
};

use super::common::{
    DialogSpec, close_dialog, current, error_text, forget_dialog, open_dialog, place_here,
    set_current, spawn_thread,
};
use crate::globals::{DebugLine, app_state, debug_buffer, main_panes};
use crate::model::{ConversationTarget, DebugLogTab};
use crate::notify::{self, Toast};
use crate::services::services;
use crate::single_file::SingleFileWindow;
use crate::viewer::ViewerPane;
use crate::widgets::ReadOnlyText;

/// The QA id of the dialog.
pub const DIALOG_ID: &str = "debug-log";
/// Lines of debug.log shown (§5.14).
pub const TAIL_LINES: usize = 2000;
/// The most bytes of debug.log read for the tail.
const TAIL_BYTES: u64 = 1024 * 1024;

/// The viewer of `window`: the main window's, or a single-file window's own.
pub fn window_viewer(window: &Window, cx: &App) -> Option<Entity<ViewerPane>> {
    let panes = main_panes(cx);
    if panes.window == Some(window.window_handle()) {
        return panes.viewer();
    }
    if let Some(root) = window.root::<Root>().flatten()
        && let Ok(single) = root.read(cx).view().clone().downcast::<SingleFileWindow>()
    {
        return Some(single.read(cx).viewer().clone());
    }
    panes.viewer()
}

/// `HH:MM:SS [LEVEL] message`, one line per entry.
pub fn format_messages(lines: &[DebugLine]) -> String {
    lines
        .iter()
        .map(|line| {
            format!(
                "{} [{}] {}",
                line.at.format("%H:%M:%S"),
                line.level.tag(),
                line.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The Parse errors tab text: `File: …`, `Errors: N`, then per error
/// `Line N: <message>` with its context lines.
pub fn parse_errors_text(report: &ParseErrorReport) -> String {
    let mut out = format!(
        "File: {}\nErrors: {}\n\n",
        report.file.display(),
        report.errors.len()
    );
    for error in &report.errors {
        out.push_str(&format!("Line {}: {}\n", error.line_number, error.message));
        for line in &error.context {
            let marker = if line.is_error { ">" } else { " " };
            let text: String = if line.text.chars().count() > 200 {
                line.text.chars().take(200).collect::<String>() + "…"
            } else {
                line.text.clone()
            };
            out.push_str(&format!("{marker} {:>4} | {text}\n", line.line_number));
        }
        out.push('\n');
    }
    out
}

/// The last `max_lines` lines of the file at `path` (reads at most the last
/// MiB). `None` when the file does not exist.
pub fn tail_lines(path: &Path, max_lines: usize) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && !lines.is_empty() {
        // The first line is probably cut in the middle.
        lines.remove(0);
    }
    let from = lines.len().saturating_sub(max_lines);
    Some(lines[from..].join("\n"))
}

/// What the Parse errors tab has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseTab {
    NoConversation,
    Loading,
    /// No errors in the conversation.
    Clean(String),
    Errors(usize),
    Failed(String),
}

/// The dialog content.
pub struct DebugLogDialog {
    focus: FocusHandle,
    tab: DebugLogTab,
    output: ReadOnlyText,
    target: Option<ConversationTarget>,
    conversation: Option<Conversation>,
    parse: ParseTab,
    status: Option<String>,
    shown_pushed: u64,
    closed: bool,
    _parse_task: Option<Task<()>>,
    _file_task: Option<Task<()>>,
    _ticker: Option<Task<()>>,
}

impl Focusable for DebugLogDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// The open dialog (tests and QA).
pub fn current_dialog(cx: &App) -> Option<Entity<DebugLogDialog>> {
    current::<DebugLogDialog>(cx).filter(|dialog| !dialog.read(cx).closed)
}

impl DebugLogDialog {
    fn new(
        tab: DebugLogTab,
        target: Option<ConversationTarget>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            focus: cx.focus_handle(),
            tab,
            output: ReadOnlyText::new(window, cx),
            parse: if target.is_some() {
                ParseTab::Loading
            } else {
                ParseTab::NoConversation
            },
            target,
            conversation: None,
            status: None,
            shown_pushed: 0,
            closed: false,
            _parse_task: None,
            _file_task: None,
            _ticker: None,
        }
    }

    pub fn tab(&self) -> DebugLogTab {
        self.tab
    }

    pub fn parse_tab(&self) -> &ParseTab {
        &self.parse
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub fn captured_target(&self) -> Option<&ConversationTarget> {
        self.target.as_ref()
    }

    /// The text of the visible tab.
    pub fn text(&self, cx: &App) -> String {
        self.output.text(cx).to_string()
    }

    /// The level shown and captured from the next load on.
    pub fn level(&self, cx: &App) -> DebugLevel {
        debug_buffer(cx).min_level()
    }

    pub fn set_level(&mut self, level: DebugLevel, window: &mut Window, cx: &mut Context<Self>) {
        debug_buffer(cx).set_min_level(level);
        self.refresh(window, cx);
    }

    pub fn set_tab(&mut self, tab: DebugLogTab, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = tab;
        self.status = None;
        self.refresh(window, cx);
    }

    /// Rebuild the visible tab's text.
    pub fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.tab {
            DebugLogTab::Messages => {
                let buffer = debug_buffer(cx);
                self.shown_pushed = buffer.pushed();
                let text = format_messages(&buffer.lines_at_or_above(buffer.min_level()));
                self.output.set_text(text, window, cx);
            }
            DebugLogTab::ParseErrors => self.show_parse_errors(window, cx),
            DebugLogTab::File => self.load_file(window, cx),
        }
        cx.notify();
    }

    fn show_parse_errors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else {
            self.output.set_text("No conversation is open.", window, cx);
            return;
        };
        if let Some(conversation) = &self.conversation {
            let text = match parse_error_report(conversation) {
                Some(report) => parse_errors_text(&report),
                None => format!("No parse errors in {}.", target.path.display()),
            };
            self.output.set_text(text, window, cx);
            return;
        }
        match self.parse.clone() {
            ParseTab::Failed(error) => self.output.set_text(error, window, cx),
            ParseTab::Clean(path) => {
                self.output
                    .set_text(format!("No parse errors in {path}."), window, cx)
            }
            _ => {
                self.output
                    .set_text("Reading the conversation…", window, cx);
                self.ensure_parse(window, cx);
            }
        }
    }

    /// Start parsing the captured conversation in the background (once).
    fn ensure_parse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else {
            return;
        };
        if self._parse_task.is_some() {
            // Already reading (or read); `finish_parse` fills the tab.
            return;
        }
        let path = target.path.clone();
        let source = target.source;
        let rx = spawn_thread("debug-log-parse", move || {
            let result = if source == Some(Source::Omp) {
                claude_history_core::history::parser::process_omp_conversation_file(
                    path, None, None,
                )
            } else {
                claude_history_core::history::process_conversation_file(path, None, None)
            };
            result.map_err(|error| error_text(&error))
        });
        self._parse_task = Some(cx.spawn_in(window, async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                this.update_in(cx, |this, window, cx| this.finish_parse(result, window, cx))
                    .ok();
            }
        }));
    }

    fn finish_parse(
        &mut self,
        result: Result<Option<Conversation>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = self
            .target
            .as_ref()
            .map(|target| target.path.display().to_string())
            .unwrap_or_default();
        match result {
            Ok(Some(conversation)) => {
                let errors = conversation.parse_errors.len();
                self.parse = if errors == 0 {
                    ParseTab::Clean(path)
                } else {
                    ParseTab::Errors(errors)
                };
                self.conversation = Some(conversation);
            }
            Ok(None) => self.parse = ParseTab::Clean(path),
            Err(error) => self.parse = ParseTab::Failed(error),
        }
        if self.tab == DebugLogTab::ParseErrors {
            match &self.parse {
                ParseTab::Failed(error) => self.output.set_text(error.clone(), window, cx),
                ParseTab::Clean(path) => {
                    self.output
                        .set_text(format!("No parse errors in {path}."), window, cx)
                }
                _ => self.show_parse_errors(window, cx),
            }
        }
        cx.notify();
    }

    fn load_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = app_state(cx).env.debug_log_file() else {
            self.output
                .set_text("debug.log has no location (no home directory).", window, cx);
            return;
        };
        self.output.set_text("Reading debug.log…", window, cx);
        let rx = spawn_thread("debug-log-tail", move || tail_lines(&path, TAIL_LINES));
        self._file_task = Some(cx.spawn_in(window, async move |this, cx| {
            if let Ok(text) = rx.recv().await {
                this.update_in(cx, |this, window, cx| {
                    if this.tab == DebugLogTab::File {
                        let shown =
                            text.unwrap_or_else(|| "debug.log does not exist yet.".to_string());
                        this.output.set_text(shown, window, cx);
                    }
                })
                .ok();
            }
        }));
    }

    /// Clear (Messages tab): empties the buffer.
    pub fn clear_messages(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        debug_buffer(cx).clear();
        self.refresh(window, cx);
    }

    /// Copy the visible text.
    pub fn copy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        services(cx).clipboard_write(self.output.text(cx).to_string(), cx);
        notify::info(window, cx, "Copied to clipboard");
    }

    /// Write to debug.log (Parse errors tab): core's block, appended.
    pub fn write_parse_errors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(conversation) = self.conversation.clone() else {
            return;
        };
        let env = (*app_state(cx).env).clone();
        let rx = spawn_thread("debug-log-write", move || {
            log_parse_errors(&env, &conversation).map_err(|error| error_text(&error))
        });
        self._file_task = Some(cx.spawn_in(window, async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                this.update_in(cx, |this, window, cx| {
                    match result {
                        Ok(Some(message)) => {
                            this.status = Some(message.clone());
                            Toast::success(message).show(window, cx);
                        }
                        Ok(None) => {
                            this.status = Some("There are no parse errors to log.".to_string())
                        }
                        Err(error) => {
                            this.status = Some(format!("Couldn't write debug.log: {error}"));
                            notify::error(window, cx, format!("Couldn't write debug.log: {error}"));
                        }
                    }
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    /// The Open file button (debug.log tab).
    pub fn open_file(&self, cx: &mut Context<Self>) {
        if let Some(path) = app_state(cx).env.debug_log_file() {
            services(cx).open_with_system(&path, cx);
        }
    }

    /// The Reveal button (debug.log tab).
    pub fn reveal_file(&self, cx: &mut Context<Self>) {
        if let Some(path) = app_state(cx).env.debug_log_file() {
            services(cx).reveal_path(&path, cx);
        }
    }

    /// The 1 s live refresh of the Messages tab: new lines are appended and
    /// the reader's scroll position and selection stay (integrator, Phase C;
    /// a refresh used to jump back to the top).
    pub(crate) fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == DebugLogTab::Messages && debug_buffer(cx).pushed() != self.shown_pushed {
            let buffer = debug_buffer(cx);
            self.shown_pushed = buffer.pushed();
            let text = format_messages(&buffer.lines_at_or_above(buffer.min_level()));
            self.output.update_text(text, window, cx);
            cx.notify();
        }
    }

    /// The text view (tests).
    #[cfg(test)]
    pub(crate) fn output(&self) -> &ReadOnlyText {
        &self.output
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.closed = true;
        close_dialog(DIALOG_ID, window, cx);
    }
}

impl Render for DebugLogDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let level = debug_buffer(cx).min_level();
        let tab_button =
            |tab: DebugLogTab, label: &'static str, id: &'static str, cx: &mut Context<Self>| {
                Button::new(id)
                    .label(label)
                    .small()
                    .selected(self.tab == tab)
                    .on_click(cx.listener(move |this, _, window, cx| this.set_tab(tab, window, cx)))
            };
        let tabs = ButtonGroup::new("debug-log-tabs")
            .outline()
            .compact()
            .child(tab_button(
                DebugLogTab::Messages,
                "Messages",
                "debug-log-tab-messages",
                cx,
            ))
            .child(tab_button(
                DebugLogTab::ParseErrors,
                "Parse errors",
                "debug-log-tab-parse",
                cx,
            ))
            .child(tab_button(
                DebugLogTab::File,
                "debug.log",
                "debug-log-tab-file",
                cx,
            ));
        let mut levels = ButtonGroup::new("debug-log-levels").outline().compact();
        for (label, value) in [
            ("Debug", DebugLevel::Debug),
            ("Info", DebugLevel::Info),
            ("Warn", DebugLevel::Warn),
            ("Error", DebugLevel::Error),
        ] {
            levels = levels.child(
                Button::new(format!("debug-log-level-{label}"))
                    .label(label)
                    .small()
                    .selected(level == value)
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.set_level(value, window, cx)),
                    ),
            );
        }
        let can_write = matches!(self.parse, ParseTab::Errors(_));
        v_flex()
            .id("debug-log-dialog")
            .track_focus(&self.focus)
            .w_full()
            .gap_3()
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(tabs)
                    .child(div().flex_1())
                    .child(div().text_xs().text_color(muted).child("Level"))
                    .child(levels),
            )
            // The text area fills its box (a bare Textarea is a few lines tall).
            .child(
                div()
                    .h(rems(22.0))
                    .child(self.output.element("Debug log").size_full()),
            )
            .when_some(self.status.clone(), |this, status| {
                this.child(
                    div()
                        .id("debug-log-status")
                        .text_xs()
                        .text_color(muted)
                        .child(status),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .when(self.tab == DebugLogTab::Messages, |row| {
                        row.child(
                            Button::new("debug-log-clear")
                                .label("Clear")
                                .small()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.clear_messages(window, cx)
                                })),
                        )
                    })
                    .when(self.tab == DebugLogTab::ParseErrors, |row| {
                        row.child(
                            Button::new("debug-log-write")
                                .label("Write to debug.log")
                                .small()
                                .disabled(!can_write)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.write_parse_errors(window, cx)
                                })),
                        )
                    })
                    .when(self.tab == DebugLogTab::File, |row| {
                        row.child(
                            Button::new("debug-log-open")
                                .label("Open file")
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| this.open_file(cx))),
                        )
                        .child(
                            Button::new("debug-log-reveal")
                                .label("Reveal")
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| this.reveal_file(cx))),
                        )
                    })
                    .child(
                        Button::new("debug-log-copy")
                            .label("Copy")
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| this.copy(window, cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("debug-log-close")
                            .label("Close")
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    ),
            )
    }
}

/// Tools → Debug log… (§5.14) on `tab`; the Parse errors tab shows the
/// conversation the invoking window's viewer holds now.
pub fn open_debug_log(tab: DebugLogTab, window: &mut Window, cx: &mut App) {
    let viewer = window_viewer(window, cx);
    open_debug_log_with(tab, viewer.as_ref(), window, cx);
}

/// [`open_debug_log`] with the invoking window's viewer already in hand (the
/// window root is leased while it runs an action, so it cannot be read).
pub fn open_debug_log_with(
    tab: DebugLogTab,
    viewer: Option<&Entity<ViewerPane>>,
    window: &mut Window,
    cx: &mut App,
) {
    let existing = current_dialog(cx);
    if let Some(existing) = place_here(cx, window, DIALOG_ID, existing, |d: &mut DebugLogDialog| {
        d.closed = true
    }) {
        existing.update(cx, |dialog, cx| dialog.set_tab(tab, window, cx));
        return;
    }
    let target = viewer.and_then(|viewer| viewer.read(cx).target().cloned());
    let dialog = cx.new(|cx| DebugLogDialog::new(tab, target, window, cx));
    set_current(cx, &dialog, window);
    dialog.update(cx, |dialog, cx| {
        dialog.refresh(window, cx);
        if dialog.target.is_some() && dialog.tab != DebugLogTab::ParseErrors {
            // Parse the captured conversation in the background so the badge
            // tab is ready (and Write is enabled) when it is opened.
            dialog.ensure_parse(window, cx);
        }
        dialog._ticker = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this
                    .update_in(cx, |this, window, cx| this.tick(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    });
    let focus = dialog.read(cx).focus.clone();
    let dismissed = dialog.clone();
    open_dialog(
        DialogSpec {
            id: DIALOG_ID,
            title: "Debug log",
            width_rem: 52.0,
            enter_closes: false,
        },
        dialog,
        focus,
        move |cx| {
            dismissed.update(cx, |dialog, _| dialog.closed = true);
            forget_dialog(DIALOG_ID, cx);
        },
        window,
        cx,
    );
}
