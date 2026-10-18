//! The Settings pages and fields (DESIGN §13.2, §5.16). WP6.
//!
//! General, Display, Search, Keys, Resume, Agent: one row per field with its
//! control, a source line (`From gui.toml`, `From config.toml: <v>`,
//! `Remembered from last session: <v>`, `Default: <v>`), when a change takes
//! effect, an inline error and Reset (remove the gui.toml key).

use std::ffi::OsString;
use std::path::PathBuf;

use claude_history_core::config::{AgentScopeConfig, LayeredConfig};
use claude_history_core::program::{ProgramKind, ResolvedProgram};
use claude_history_core::resume::terminal::{
    BuildOptions, LaunchSpec, Platform, TerminalLauncher, build_terminal_command, detect_launcher,
};
use claude_history_core::search::mode::SearchMode;
use claude_history_core::theme::ThemePreference;
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::input::{Input, Textarea};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, h_flex,
    scroll::ScrollableElement as _, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, rems,
};

use super::draft::{self, Field, SettingsDraft, Value};
use super::{CONTENT_HEIGHT_REM, SETTINGS_CONTEXT, SettingsDialog};
use crate::globals::app_state;
use crate::keymap;
use crate::model::{KeyAction, SettingsPage};
use crate::qa_ext::QaExt as _;
use crate::services::services;

/// The width of the label column (rem).
const LABEL_REM: f32 = 12.0;

/// The placeholder of a text field: the value in effect without gui.toml.
pub fn placeholder(field: Field, config: &LayeredConfig) -> SharedString {
    match field {
        Field::Terminal => "auto".into(),
        Field::ClaudePath | Field::PiPath | Field::OmpPath => "resolved from PATH".into(),
        Field::DefaultWorkspace => "none".into(),
        Field::Key(action) => {
            let effective = draft::effective_keys(&config.cli, &Default::default());
            keymap::chord_of(&effective, action).config_string().into()
        }
        Field::AgentOutputChars | Field::AgentTop | Field::AgentWithinTop | Field::AgentHits => {
            let cli = draft::cli_value(field, &config.cli);
            let shown = if cli.is_unset() {
                draft::default_value(field)
            } else {
                cli
            };
            draft::describe(field, &shown).into()
        }
        _ => SharedString::default(),
    }
}

/// The text a text or list control starts with: the draft's own value.
pub fn control_text(field: Field, draft: &SettingsDraft) -> String {
    match (field, draft.value(field)) {
        (Field::Key(action), _) => super::keys_page::initial_text(draft, action),
        (_, Value::Text(text)) => text,
        (_, Value::Number(number)) => number.to_string(),
        (_, Value::List(list)) => draft::list_to_lines(&list),
        _ => String::new(),
    }
}

fn element_id(prefix: &str, field: Field) -> SharedString {
    format!("{prefix}-{}", field.key_path()).into()
}

impl SettingsDialog {
    /// The whole dialog content: pages nav, the current page, the gui.toml
    /// error banner, the files and the Save/Cancel row.
    pub(super) fn render_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let page = match self.page {
            SettingsPage::General => self.general_page(window, cx),
            SettingsPage::Display => self.display_page(cx),
            SettingsPage::Search => self.search_page(cx),
            SettingsPage::Keys => self.keys_page(cx),
            SettingsPage::Resume => self.resume_page(cx),
            SettingsPage::Agent => self.agent_page(cx),
        };
        let nav =
            v_flex()
                .w(rems(8.))
                .flex_none()
                .gap_1()
                .children(SettingsPage::ALL.into_iter().map(|target| {
                    div()
                        .w_full()
                        .child(
                            Button::new(SharedString::from(format!(
                                "settings-page-{}",
                                target.title()
                            )))
                            .label(target.title())
                            .ghost()
                            .small()
                            .w_full()
                            .selected(self.page == target)
                            .on_click(cx.listener(move |this, _, _, cx| this.set_page(target, cx))),
                        )
                        .qa_id(format!("settings:page:{}", target.title()))
                }));
        v_flex()
            .id("settings-dialog")
            .key_context(SETTINGS_CONTEXT)
            .track_focus(&self.focus)
            .w_full()
            .h(rems(CONTENT_HEIGHT_REM))
            .gap_2()
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_4()
                    .items_start()
                    .child(nav)
                    .child(
                        div()
                            .id("settings-page")
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .vertical_scrollbar(&self.scroll)
                            .pr_3()
                            .child(page),
                    ),
            )
            .when_some(self.gui_error.clone(), |this, error| {
                this.child(self.gui_error_banner(error, cx))
            })
            .child(self.files_section(cx))
            .child(self.footer(cx))
            .into_any_element()
    }

    fn gui_error_banner(&self, error: String, cx: &mut Context<Self>) -> AnyElement {
        let warning = crate::theme_tokens::roles(cx).warning;
        let path = app_state(cx).env.gui_config_file();
        let open_path = path.clone();
        let reveal_path = path;
        h_flex()
            .id("settings-gui-error")
            .gap_2()
            .items_center()
            .text_xs()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(warning)
                    .child(format!("⚠ gui.toml has errors: {error}")),
            )
            .child(
                Button::new("settings-retry")
                    .label("Retry")
                    .xsmall()
                    .outline()
                    .on_click(cx.listener(|this, _, _, cx| this.retry_config(cx))),
            )
            .child(
                Button::new("settings-open-gui")
                    .label("Open gui.toml")
                    .xsmall()
                    .outline()
                    .disabled(open_path.is_none())
                    .on_click(move |_, _, cx| {
                        if let Some(path) = &open_path {
                            services(cx).open_with_system(path, cx);
                        }
                    }),
            )
            .child(
                Button::new("settings-reveal-gui")
                    .label("Reveal")
                    .xsmall()
                    .outline()
                    .disabled(reveal_path.is_none())
                    .on_click(move |_, _, cx| {
                        if let Some(path) = &reveal_path {
                            services(cx).reveal_path(path, cx);
                        }
                    }),
            )
            .into_any_element()
    }

    fn files_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let env = &app_state(cx).env;
        let line = |label: &str, path: Option<PathBuf>, note: &str| {
            div().w_full().truncate().child(format!(
                "{label} ({note}): {}",
                path.map_or_else(|| "unavailable".to_string(), |p| p.display().to_string())
            ))
        };
        v_flex()
            .id("settings-files")
            .text_xs()
            .text_color(muted)
            .child(line("gui.toml", env.gui_config_file(), "written"))
            .child(line("config.toml", env.config_file(), "read-only"))
            .into_any_element()
    }

    fn footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let danger = crate::theme_tokens::roles(cx).danger;
        let can_save = self.can_save();
        h_flex()
            .id("settings-footer")
            .gap_2()
            .justify_end()
            .items_center()
            .when_some(self.save_error.clone(), |this, error| {
                this.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(danger)
                        .child(error),
                )
            })
            .child(
                div()
                    .child(
                        Button::new("settings-cancel")
                            .label("Cancel")
                            .disabled(self.saving)
                            .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
                    )
                    .qa_id("settings:cancel"),
            )
            .child(
                div()
                    .child(
                        Button::new("settings-save")
                            .label("Save")
                            .primary()
                            .loading(self.saving)
                            .disabled(!can_save)
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    )
                    .qa_id("settings:save"),
            )
            .into_any_element()
    }

    // -----------------------------------------------------------------
    // Rows and controls
    // -----------------------------------------------------------------

    /// The effective value a control shows: the draft's own, else the saved
    /// configuration's (config.toml, remembered, default).
    pub fn shown(&self, field: Field, cx: &App) -> Value {
        let source = draft::source_of(field, &self.config, &app_state(cx).ui);
        draft::shown_value(field, &self.draft.edited, &source)
    }

    /// The source line of a field: `From gui.toml`, `From config.toml: <v>`,
    /// `Remembered from last session: <v>` or `Default: <v>`.
    pub fn source_line(&self, field: Field, cx: &App) -> String {
        draft::source_of(field, &self.config, &app_state(cx).ui).text
    }

    /// The full note under a field's control: the source line and when a
    /// change takes effect.
    pub fn note_line(&self, field: Field, cx: &App) -> String {
        format!("{} · {}", self.source_line(field, cx), field.applies())
    }

    /// The read-only lines about deprecated config.toml keys (C-024), e.g.
    /// `global = true (deprecated, ignored)`.
    pub fn deprecated_lines(&self) -> Vec<String> {
        draft::deprecated_notes(&self.config.cli)
            .into_iter()
            .map(|note| format!("{} = {} ({})", note.key, note.value, note.note))
            .collect()
    }

    /// A field row: label, control, Reset, the source line and any error.
    fn row(
        &self,
        field: Field,
        control: AnyElement,
        extra: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let danger = crate::theme_tokens::roles(cx).danger;
        let theme = cx.theme();
        let (muted, border) = (theme.muted_foreground, theme.border);
        let note_line = self.note_line(field, cx);
        let can_reset = !self.draft.value(field).is_unset() || self.errors.contains_key(&field);
        let indent = rems(LABEL_REM + 0.75);
        v_flex()
            .id(element_id("settings-row", field))
            .gap_1()
            .py_2()
            .border_b_1()
            .border_color(border)
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        div()
                            .w(rems(LABEL_REM))
                            .flex_none()
                            .text_sm()
                            .child(field.label()),
                    )
                    .child(div().flex_1().min_w_0().child(control))
                    .child(
                        Button::new(element_id("settings-reset", field))
                            .label("Reset")
                            .xsmall()
                            .ghost()
                            .disabled(!can_reset)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.reset(field, window, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .pl(indent)
                    .text_xs()
                    .text_color(muted)
                    .child(note_line),
            )
            .when_some(self.errors.get(&field).cloned(), |this, error| {
                this.child(div().pl(indent).text_xs().text_color(danger).child(error))
            })
            .when_some(extra, |this, extra| {
                this.child(
                    div()
                        .pl(indent)
                        .min_w_0()
                        .text_xs()
                        .text_color(muted)
                        .child(extra),
                )
            })
            .into_any_element()
    }

    fn segmented(
        &self,
        field: Field,
        options: Vec<(&'static str, Value)>,
        current: &Value,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut group = ButtonGroup::new(element_id("settings-seg", field))
            .outline()
            .compact();
        for (label, value) in options {
            let selected = &value == current;
            group = group.child(
                Button::new(SharedString::from(format!(
                    "settings-{}-{label}",
                    field.key_path()
                )))
                .label(label)
                .small()
                .selected(selected)
                .on_click(
                    cx.listener(move |this, _, _, cx| this.set_value(field, value.clone(), cx)),
                ),
            );
        }
        group.into_any_element()
    }

    fn switch(&self, field: Field, cx: &mut Context<Self>) -> AnyElement {
        let on = matches!(self.shown(field, cx), Value::Bool(true));
        Switch::new(element_id("settings-switch", field))
            .checked(on)
            .on_change(cx.listener(move |this, value: &bool, _, cx| {
                this.set_value(field, Value::Bool(*value), cx)
            }))
            .into_any_element()
    }

    fn text_input(&self, field: Field) -> AnyElement {
        match self.inputs.get(&field) {
            Some(input) => div()
                .w_full()
                .child(Input::new(input).small())
                .qa_id(format!("settings:input:{}", field.qa_name()))
                .into_any_element(),
            None => div().into_any_element(),
        }
    }

    fn list_input(&self, field: Field, rows: f32) -> AnyElement {
        match self.areas.get(&field) {
            Some(area) => Textarea::new(area).h(rems(rows)).into_any_element(),
            None => div().into_any_element(),
        }
    }

    fn input_with_browse(&self, field: Field, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .gap_2()
            .child(div().flex_1().min_w_0().child(self.text_input(field)))
            .child(
                Button::new(element_id("settings-browse", field))
                    .label("Browse…")
                    .small()
                    .outline()
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.browse(field, window, cx)),
                    ),
            )
            .into_any_element()
    }

    fn note(&self, text: impl Into<SharedString>, cx: &App) -> AnyElement {
        div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(text.into())
            .into_any_element()
    }

    /// A one-line note that is cut with an ellipsis instead of wrapping
    /// (long commands and paths).
    fn note_one_line(&self, text: impl Into<SharedString>, cx: &App) -> AnyElement {
        div()
            .w_full()
            .truncate()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(text.into())
            .into_any_element()
    }

    // -----------------------------------------------------------------
    // Pages
    // -----------------------------------------------------------------

    fn general_page(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme_value = self.shown(Field::Theme, cx);
        let theme = self.segmented(
            Field::Theme,
            vec![
                ("System", Value::Theme(ThemePreference::System)),
                ("Light", Value::Theme(ThemePreference::Light)),
                ("Dark", Value::Theme(ThemePreference::Dark)),
            ],
            &theme_value,
            cx,
        );
        let presets: Vec<&'static str> = match Platform::current() {
            Platform::Windows => vec!["auto", "wt", "cmd"],
            Platform::MacOs => vec!["auto", "terminal", "osascript"],
            Platform::Linux => vec![
                "auto",
                "gnome-terminal",
                "konsole",
                "kitty",
                "alacritty",
                "wezterm",
                "xterm",
            ],
        };
        let preset_buttons = h_flex()
            .gap_1()
            .flex_wrap()
            .children(presets.into_iter().map(|preset| {
                Button::new(SharedString::from(format!("settings-terminal-{preset}")))
                    .label(preset)
                    .xsmall()
                    .outline()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let text = if preset == "auto" { "" } else { preset };
                        this.set_text(Field::Terminal, text, window, cx)
                    }))
            }));
        let terminal_control = v_flex()
            .gap_1()
            .child(self.text_input(Field::Terminal))
            .child(preset_buttons)
            .into_any_element();
        let preview = self.note_one_line(self.probes.terminal.clone(), cx);
        let mut rows = vec![
            self.row(Field::Theme, theme, None, cx),
            self.row(Field::Terminal, terminal_control, Some(preview), cx),
        ];
        for (field, _name) in [
            (Field::ClaudePath, "claude"),
            (Field::PiPath, "pi"),
            (Field::OmpPath, "omp"),
        ] {
            let status = self.note_one_line(self.program_status_text(field), cx);
            let control = self.input_with_browse(field, cx);
            rows.push(self.row(field, control, Some(status), cx));
        }
        let workspace = self.input_with_browse(Field::DefaultWorkspace, cx);
        rows.push(self.row(Field::DefaultWorkspace, workspace, None, cx));
        let restore = self.switch(Field::RestoreSession, cx);
        rows.push(self.row(Field::RestoreSession, restore, None, cx));
        let notes = self.deprecated_section(cx);
        v_flex()
            .id("settings-general")
            .children(rows)
            .child(notes)
            .into_any_element()
    }

    fn display_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let last = self.shown(Field::PreviewLast, cx);
        let preview = self.segmented(
            Field::PreviewLast,
            vec![("Last", Value::Bool(true)), ("First", Value::Bool(false))],
            &last,
            cx,
        );
        let tools = self.shown(Field::ToolDisplay, cx);
        let tool_display = self.segmented(
            Field::ToolDisplay,
            vec![("Summary", Value::Bool(true)), ("Full", Value::Bool(false))],
            &tools,
            cx,
        );
        let thinking = self.switch(Field::ShowThinking, cx);
        let plain = self.switch(Field::Plain, cx);
        let rows = vec![
            self.row(Field::PreviewLast, preview, None, cx),
            self.row(Field::ToolDisplay, tool_display, None, cx),
            self.row(Field::ShowThinking, thinking, None, cx),
            self.row(Field::Plain, plain, None, cx),
        ];
        v_flex()
            .id("settings-display")
            .children(rows)
            .child(div().py_2().child(self.note(
                "Pager: not used by the GUI (config.toml [display] pager is ignored).",
                cx,
            )))
            .child(self.deprecated_section(cx))
            .into_any_element()
    }

    fn search_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mode = self.shown(Field::SearchMode, cx);
        let modes = self.segmented(
            Field::SearchMode,
            SearchMode::ALL
                .into_iter()
                .map(|mode| (mode_label(mode), Value::Mode(mode)))
                .collect(),
            &mode,
            cx,
        );
        let exclude = self.list_input(Field::ExcludeProjects, 6.0);
        let rows = vec![
            self.row(
                Field::SearchMode,
                modes,
                Some(self.note(
                    "The list maps exact and hybrid to Lexical; Deep search uses all four.",
                    cx,
                )),
                cx,
            ),
            self.row(
                Field::ExcludeProjects,
                exclude,
                Some(self.note(
                    "One project name per line; `repo` also hides `repo/<worktree>`.",
                    cx,
                )),
                cx,
            ),
        ];
        v_flex()
            .id("settings-search")
            .children(rows)
            .child(self.deprecated_section(cx))
            .into_any_element()
    }

    fn keys_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let warning = crate::theme_tokens::roles(cx).warning;
        let rows: Vec<AnyElement> = KeyAction::ALL
            .into_iter()
            .map(|action| {
                let field = Field::Key(action);
                let control = self.text_input(field);
                self.row(field, control, None, cx)
            })
            .collect();
        let warnings = self.key_warnings();
        v_flex()
            .id("settings-keys")
            .children(rows)
            .child(self.note(
                "Syntax: ctrl+r, alt+r, f2 or a single key. Changes apply to the list and the viewer as soon as you save.",
                cx,
            ))
            .children(warnings.into_iter().map(|text| {
                div()
                    .text_xs()
                    .text_color(warning)
                    .child(format!("⚠ {text}"))
            }))
            .into_any_element()
    }

    fn resume_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let args = self.list_input(Field::ResumeArgs, 6.0);
        let row = self.row(
            Field::ResumeArgs,
            args,
            Some(self.note(
                "One argument per line; used when resuming Claude conversations.",
                cx,
            )),
            cx,
        );
        v_flex().id("settings-resume").child(row).into_any_element()
    }

    fn agent_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let scope = self.shown(Field::AgentScope, cx);
        let scope_control = self.segmented(
            Field::AgentScope,
            vec![
                ("Global", Value::Scope(AgentScopeConfig::Global)),
                ("Local", Value::Scope(AgentScopeConfig::Local)),
            ],
            &scope,
            cx,
        );
        let mode = self.shown(Field::AgentMode, cx);
        let mode_control = self.segmented(
            Field::AgentMode,
            SearchMode::ALL
                .into_iter()
                .map(|mode| (mode_label(mode), Value::Mode(mode)))
                .collect(),
            &mode,
            cx,
        );
        let mut rows = vec![
            self.row(Field::AgentScope, scope_control, None, cx),
            self.row(Field::AgentMode, mode_control, None, cx),
        ];
        for field in [
            Field::AgentOutputChars,
            Field::AgentTop,
            Field::AgentWithinTop,
            Field::AgentHits,
        ] {
            let control = self.text_input(field);
            rows.push(self.row(field, control, None, cx));
        }
        let exclude = self.list_input(Field::AgentExclude, 4.0);
        rows.push(self.row(
            Field::AgentExclude,
            exclude,
            Some(self.note(
                "One encoded folder name per line, e.g. -Users-me-code-repo.",
                cx,
            )),
            cx,
        ));
        for field in [
            Field::AgentTools,
            Field::AgentToolResults,
            Field::AgentThinking,
            Field::AgentSubagents,
        ] {
            let control = self.switch(field, cx);
            rows.push(self.row(field, control, None, cx));
        }
        v_flex()
            .id("settings-agent")
            .children(rows)
            .into_any_element()
    }

    /// The deprecated config.toml keys, read-only (C-024).
    fn deprecated_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let notes = self.deprecated_lines();
        if notes.is_empty() {
            return div().into_any_element();
        }
        let muted = cx.theme().muted_foreground;
        v_flex()
            .id("settings-deprecated")
            .py_2()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("config.toml (read-only):"),
            )
            .children(
                notes
                    .into_iter()
                    .map(|line| div().text_xs().text_color(muted).child(line)),
            )
            .into_any_element()
    }

    /// `resolved: <path>` / `not found on PATH` for a program field, as last
    /// probed in the background (no file-system access while rendering).
    pub(crate) fn program_status_text(&self, field: Field) -> String {
        self.probes
            .programs
            .get(&field)
            .cloned()
            .unwrap_or_else(|| "checking…".to_string())
    }

    /// [`program_status_for`] for the draft, computed now (tests).
    #[cfg(test)]
    pub(crate) fn program_status(&self, field: Field, name: &str, cx: &App) -> String {
        program_status_for(&app_state(cx).env, self.draft.value(field), name)
    }

    /// [`terminal_preview_for`] for the draft, computed now (tests).
    #[cfg(test)]
    pub(crate) fn terminal_preview(&self, cx: &App) -> String {
        terminal_preview_for(&self.probe_input(cx))
    }
}

/// `resolved: <path>` / `not found on PATH` for a program field whose own
/// path setting is `own` (searches PATH: never call it while rendering).
pub(crate) fn program_status_for(
    env: &claude_history_core::HistoryEnv,
    own: Value,
    name: &str,
) -> String {
    let own = match own {
        Value::Text(text) => Some(PathBuf::from(text)),
        _ => None,
    };
    match env.resolve_program(name, own.as_deref().filter(|path| path.is_file())) {
        Ok(program) => format!("resolved: {}", program.path.display()),
        Err(_) => "not found on PATH".to_string(),
    }
}

/// The command a sample resume would run in the configured terminal
/// (searches PATH for a launcher: never call it while rendering).
pub(crate) fn terminal_preview_for(input: &super::probe::ProbeInput) -> String {
    let platform = Platform::current();
    let launcher = input
        .terminal
        .as_deref()
        .and_then(TerminalLauncher::from_setting)
        .or_else(|| detect_launcher(&input.env, platform, None));
    let Some(launcher) = launcher else {
        return "Preview: no terminal emulator found".to_string();
    };
    let spec = LaunchSpec {
        program: ResolvedProgram {
            path: PathBuf::from("claude"),
            kind: ProgramKind::Executable,
        },
        args: vec![
            OsString::from("--resume"),
            OsString::from("00000000-0000-4000-8000-000000000000"),
        ],
        cwd: input.cwd.clone(),
    };
    let options = BuildOptions {
        platform,
        keep_open: true,
        script_path: Some(PathBuf::from("resume.command")),
    };
    match build_terminal_command(&launcher, &spec, &options) {
        Ok(command) => format!("Preview: {}", command.display()),
        Err(error) => format!("Preview: {error}"),
    }
}

fn mode_label(mode: SearchMode) -> &'static str {
    match mode {
        SearchMode::Lexical => "Lexical",
        SearchMode::Semantic => "Semantic",
        SearchMode::Exact => "Exact",
        SearchMode::Hybrid => "Hybrid",
    }
}
