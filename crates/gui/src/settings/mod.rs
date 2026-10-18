//! The Settings dialog: gui.toml only (PD13, DESIGN §5.16, §13.2, §11.18).
//! WP0 stub → WP6.
//!
//! * [`open_settings`] opens (or re-focuses) the dialog, optionally on a page.
//! * [`draft::SettingsDraft`] holds the edits; unedited fields follow
//!   `AppState` (a View-menu theme change during the dialog), edited ones
//!   keep the user's value.
//! * Save (disabled while gui.toml cannot be parsed) writes only the changed
//!   keys through core's change-set save (K12, config.toml is never written),
//!   reloads both files into `AppState`, applies the changes live (key
//!   bindings, theme, excluded projects, previews) and toasts
//!   `Settings saved to <path>`; errors stay inline and keep the draft.
//! * Cancel discards the draft and writes nothing.

pub mod draft;
pub mod keys_page;
pub mod pages;
pub mod probe;

#[cfg(test)]
mod tests;

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

use claude_history_core::config::{LayeredConfig, save_gui_config_changes};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, FocusHandle, Focusable, Global,
    IntoElement, ParentElement as _, Render, SharedString, Subscription, WeakEntity, Window, div,
};

use crate::globals::{app_state, main_panes, update_app_state};
use crate::model::{ConfigErrors, KeyAction, OpenPathsOptions, SettingsPage};
use crate::notify::Toast;
use crate::services::services;
use crate::{keymap, theme_tokens, workspace};

use draft::{Field, SettingsDraft, Value};

/// The key context of the dialog content.
pub const SETTINGS_CONTEXT: &str = "SettingsDialog";
/// The dialog width in rem (§5.16).
pub const DIALOG_WIDTH_REM: f32 = 56.0;
/// The content height in rem (the dialog is 40rem with its title).
pub const CONTENT_HEIGHT_REM: f32 = 34.0;

/// Fields edited with a single-line input.
pub const TEXT_FIELDS: [Field; 14] = [
    Field::Terminal,
    Field::ClaudePath,
    Field::PiPath,
    Field::OmpPath,
    Field::DefaultWorkspace,
    Field::Key(KeyAction::Resume),
    Field::Key(KeyAction::Fork),
    Field::Key(KeyAction::Rename),
    Field::Key(KeyAction::Delete),
    Field::Key(KeyAction::Refresh),
    Field::AgentOutputChars,
    Field::AgentTop,
    Field::AgentWithinTop,
    Field::AgentHits,
];

/// Fields edited as one item per line.
pub const LIST_FIELDS: [Field; 3] = [
    Field::ExcludeProjects,
    Field::ResumeArgs,
    Field::AgentExclude,
];

/// The open dialog (tests and QA find it here).
#[derive(Default)]
struct OpenDialog(
    Option<WeakEntity<SettingsDialog>>,
    Option<gpui_kit::AnyWindowHandle>,
);

impl Global for OpenDialog {}

/// The open Settings dialog, if any.
pub fn current_dialog(cx: &App) -> Option<Entity<SettingsDialog>> {
    cx.try_global::<OpenDialog>()
        .and_then(|open| open.0.as_ref())
        .and_then(WeakEntity::upgrade)
        .filter(|dialog| !dialog.read(cx).closed)
}

/// Tools → Settings… (`secondary-,`), optionally on `page`.
pub fn open_settings(page: Option<SettingsPage>, window: &mut Window, cx: &mut App) {
    if let Some(existing) = current_dialog(cx) {
        let other_window = cx
            .try_global::<OpenDialog>()
            .and_then(|open| open.1)
            .filter(|handle| *handle != window.window_handle());
        if let Some(other) = other_window {
            // Settings is open in another window (the shortcut was pressed
            // here): close it there and open it in this window below.
            existing.update(cx, |dialog, _| dialog.closed = true);
            cx.defer(move |cx| {
                other
                    .update(cx, |_, window, cx| window.close_dialog(cx))
                    .ok();
            });
        } else {
            existing.update(cx, |dialog, cx| {
                if let Some(page) = page {
                    dialog.set_page(page, cx);
                }
                cx.notify();
            });
            return;
        }
    }
    let dialog = cx.new(|cx| SettingsDialog::new(page.unwrap_or_default(), window, cx));
    cx.set_global(OpenDialog(
        Some(dialog.downgrade()),
        Some(window.window_handle()),
    ));
    dialog.update(cx, |dialog, cx| dialog.schedule_probe(cx));
    let first_render = Rc::new(Cell::new(true));
    let width = window.rem_size() * DIALOG_WIDTH_REM;
    window.open_dialog(cx, move |builder, window, cx| {
        if first_render.replace(false) {
            dialog.read(cx).focus.clone().focus(window, cx);
        }
        let cancel = dialog.clone();
        let close = dialog.clone();
        builder
            .title("Settings")
            .w(width)
            .child(dialog.clone())
            // The buttons live in the content (their state depends on the draft).
            .footer(div())
            .on_ok(|_, _, _| false)
            .on_cancel(move |_, _, cx| {
                cancel.update(cx, |dialog, _| dialog.closed = true);
                true
            })
            .on_close(move |_, _, cx| close.update(cx, |dialog, _| dialog.closed = true))
    });
}

/// A core error as shown in Settings and toasts: without the `Configuration
/// error: ` prefix core's `Display` adds (`gui.toml has errors; fix or remove
/// it before saving: <e>`, §12).
pub fn error_text(error: &claude_history_core::AppError) -> String {
    let text = error.to_string();
    text.strip_prefix("Configuration error: ")
        .map(str::to_string)
        .unwrap_or(text)
}

/// Re-read config.toml and gui.toml into `AppState` (both independently; a
/// failing file contributes defaults and its error): the effective settings,
/// the view defaults for conversations opened later and the errors Settings
/// shows. Returns the new errors.
pub fn reload_config(cx: &mut App) -> ConfigErrors {
    let env = app_state(cx).env.clone();
    let (config, errors) = workspace::startup::load_config(&env);
    let result = errors.clone();
    update_app_state(cx, move |state, _| {
        let settings = state.launch.args.settings(&config.effective);
        let (view_defaults, list_mode) =
            workspace::startup::resolve_initial_view(&state.launch, &config, &settings, &state.ui);
        state.settings = settings;
        state.view_defaults = view_defaults;
        state.list_mode = list_mode;
        state.config = config;
        state.config_errors = errors;
    });
    result
}

/// The Settings dialog content.
pub struct SettingsDialog {
    pub(crate) focus: FocusHandle,
    pub(crate) scroll: gpui_kit::ScrollHandle,
    pub(crate) page: SettingsPage,
    pub(crate) draft: SettingsDraft,
    /// The saved configuration the source lines describe.
    pub(crate) config: LayeredConfig,
    /// gui.toml's parse error: Save is disabled while set.
    pub(crate) gui_error: Option<String>,
    pub(crate) inputs: BTreeMap<Field, Entity<InputState>>,
    pub(crate) areas: BTreeMap<Field, Entity<TextareaState>>,
    /// Inline validation errors by field (Save is disabled while any exists).
    pub(crate) errors: BTreeMap<Field, String>,
    pub(crate) save_error: Option<SharedString>,
    pub(crate) saving: bool,
    pub(crate) closed: bool,
    /// Background probe results (programs, terminal preview) rendered as is.
    pub(crate) probes: probe::Probes,
    /// Path fields whose typed text has not been checked yet (Save waits).
    pub(crate) pending_paths: BTreeMap<Field, String>,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for SettingsDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SettingsDialog {
    pub fn new(page: SettingsPage, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (config, errors) = {
            let state = app_state(cx);
            (state.config.clone(), state.config_errors.clone())
        };
        let draft = SettingsDraft::new(&config.gui_file);
        let mut inputs = BTreeMap::new();
        let mut areas = BTreeMap::new();
        let mut subscriptions = Vec::new();
        for field in TEXT_FIELDS {
            let placeholder = pages::placeholder(field, &config);
            let text = pages::control_text(field, &draft);
            let input = cx.new(|cx| {
                let mut state = InputState::new(window, cx).placeholder(placeholder);
                state.set_value(text, window, cx);
                state
            });
            subscriptions.push(cx.subscribe_in(
                &input,
                window,
                move |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::Change = event {
                        this.on_text_edited(field, window, cx);
                    }
                },
            ));
            inputs.insert(field, input);
        }
        for field in LIST_FIELDS {
            let text = pages::control_text(field, &draft);
            let area = cx.new(|cx| {
                let mut state = TextareaState::new(window, cx);
                state.set_value(text, window, cx);
                state
            });
            subscriptions.push(cx.subscribe_in(
                &area,
                window,
                move |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::Change = event {
                        this.on_text_edited(field, window, cx);
                    }
                },
            ));
            areas.insert(field, area);
        }
        subscriptions.push(
            cx.observe_global_in::<crate::globals::AppState>(window, |this, window, cx| {
                this.refresh_from_state(window, cx)
            }),
        );
        Self {
            focus: cx.focus_handle(),
            scroll: gpui_kit::ScrollHandle::new(),
            page,
            draft,
            config,
            gui_error: errors.gui,
            inputs,
            areas,
            errors: BTreeMap::new(),
            save_error: None,
            saving: false,
            closed: false,
            probes: probe::Probes::default(),
            pending_paths: BTreeMap::new(),
            _subscriptions: subscriptions,
        }
    }

    /// What a probe needs right now (cheap: copies, no I/O).
    pub(crate) fn probe_input(&self, cx: &App) -> probe::ProbeInput {
        let state = app_state(cx);
        probe::ProbeInput {
            env: state.env.clone(),
            terminal: match self.draft.value(Field::Terminal) {
                Value::Text(text) => Some(text),
                _ => None,
            },
            cwd: state
                .workspace
                .as_ref()
                .map(|workspace| workspace.dir().to_path_buf())
                .or_else(|| state.env.home_dir.clone()),
            programs: probe::PROGRAM_FIELDS
                .iter()
                .map(|(field, name)| (*field, self.draft.value(*field), *name))
                .collect(),
            paths: self
                .pending_paths
                .iter()
                .map(|(field, text)| (*field, text.clone(), *field == Field::DefaultWorkspace))
                .collect(),
        }
    }

    /// Re-probe when `field` feeds a probe (terminal, programs, typed paths).
    fn probe_after_edit(&mut self, field: Field, cx: &mut Context<Self>) {
        if field == Field::Terminal
            || self.pending_paths.contains_key(&field)
            || probe::PROGRAM_FIELDS.iter().any(|(f, _)| *f == field)
        {
            self.schedule_probe(cx);
        }
    }

    /// Start a probe on the background executor; a newer probe supersedes it.
    pub(crate) fn schedule_probe(&mut self, cx: &mut Context<Self>) {
        self.probes.generation += 1;
        let generation = self.probes.generation;
        let input = self.probe_input(cx);
        let task = cx
            .background_executor()
            .spawn(async move { probe::run(input) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| this.apply_probe(generation, result, cx))
                .ok();
        })
        .detach();
    }

    fn apply_probe(&mut self, generation: u64, result: probe::ProbeResult, cx: &mut Context<Self>) {
        if generation != self.probes.generation {
            return;
        }
        self.probes.programs = result.programs;
        self.probes.terminal = result.terminal;
        let mut draft_changed = false;
        for (field, text, outcome) in result.paths {
            // Only the newest typed text of a field counts.
            if self.pending_paths.get(&field) != Some(&text) {
                continue;
            }
            self.pending_paths.remove(&field);
            match outcome {
                Ok(()) => {
                    self.errors.remove(&field);
                    self.set_or_reset(field, &text, |t| Value::Text(t.to_string()));
                    draft_changed = true;
                }
                Err(error) => {
                    self.errors.insert(field, error);
                }
            }
        }
        if draft_changed {
            // The programs' status follows the draft's new path.
            self.schedule_probe(cx);
        }
        cx.notify();
    }

    pub fn page(&self) -> SettingsPage {
        self.page
    }

    pub fn set_page(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        self.page = page;
        self.scroll
            .set_offset(gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.)));
        cx.notify();
    }

    pub fn draft(&self) -> &SettingsDraft {
        &self.draft
    }

    /// The saved configuration the dialog describes.
    pub fn config(&self) -> &LayeredConfig {
        &self.config
    }

    /// gui.toml's parse error (Save is disabled while it is set).
    pub fn gui_error(&self) -> Option<&str> {
        self.gui_error.as_deref()
    }

    /// The inline validation error of a field.
    pub fn field_error(&self, field: Field) -> Option<&str> {
        self.errors.get(&field).map(String::as_str)
    }

    pub fn save_error(&self) -> Option<&str> {
        self.save_error.as_deref()
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    pub fn is_saving(&self) -> bool {
        self.saving
    }

    /// Whether Save can run: gui.toml parses, no field is invalid, no save runs.
    pub fn can_save(&self) -> bool {
        self.gui_error.is_none()
            && self.errors.is_empty()
            && self.pending_paths.is_empty()
            && !self.saving
            && !self.closed
    }

    /// The warnings of the Keys page for the draft.
    pub fn key_warnings(&self) -> Vec<String> {
        keys_page::warnings(&self.draft, &self.config.cli)
    }

    /// Set a field from a control that holds a ready value (segmented
    /// buttons, switches) or from code.
    pub fn set_value(&mut self, field: Field, value: Value, cx: &mut Context<Self>) {
        self.errors.remove(&field);
        self.pending_paths.remove(&field);
        self.draft.set(field, value);
        self.probe_after_edit(field, cx);
        cx.notify();
    }

    /// Type `text` into a text or list field (as the user would): the control
    /// shows it, the draft takes it when it validates, otherwise the field
    /// shows the error and the draft keeps its previous value.
    pub fn set_text(
        &mut self,
        field: Field,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.write_control(field, text, window, cx);
        self.apply_text(field, text);
        self.probe_after_edit(field, cx);
        cx.notify();
    }

    /// Reset: remove the gui.toml key; the field falls back to config.toml,
    /// the remembered value or the default.
    pub fn reset(&mut self, field: Field, window: &mut Window, cx: &mut Context<Self>) {
        self.draft.reset(field);
        self.errors.remove(&field);
        self.pending_paths.remove(&field);
        self.write_control(field, "", window, cx);
        self.probe_after_edit(field, cx);
        cx.notify();
    }

    fn write_control(
        &mut self,
        field: Field,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(input) = self.inputs.get(&field) {
            if input.read(cx).value().as_ref() != text {
                input.update(cx, |state, cx| {
                    state.set_value(text.to_string(), window, cx)
                });
            }
        } else if let Some(area) = self.areas.get(&field)
            && area.read(cx).value().as_ref() != text
        {
            area.update(cx, |state, cx| {
                state.set_value(text.to_string(), window, cx)
            });
        }
    }

    fn control_value(&self, field: Field, cx: &App) -> Option<String> {
        if let Some(input) = self.inputs.get(&field) {
            Some(input.read(cx).value().to_string())
        } else {
            self.areas
                .get(&field)
                .map(|area| area.read(cx).value().to_string())
        }
    }

    fn on_text_edited(&mut self, field: Field, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.control_value(field, cx) {
            self.apply_text(field, &text);
            self.probe_after_edit(field, cx);
        }
        cx.notify();
    }

    /// Validate `text` for `field` and move it into the draft.
    fn apply_text(&mut self, field: Field, text: &str) {
        use draft::{lines_to_list, parse_number_text};
        let result: Result<(), String> = match field {
            Field::Terminal => {
                self.set_or_reset(field, text, |text| Value::Text(text.to_string()));
                Ok(())
            }
            // Existence is checked in the background (`apply_probe`).
            Field::ClaudePath | Field::PiPath | Field::OmpPath | Field::DefaultWorkspace => {
                if text.trim().is_empty() {
                    self.pending_paths.remove(&field);
                    self.set_or_reset(field, text, |t| Value::Text(t.to_string()));
                } else {
                    self.pending_paths.insert(field, text.trim().to_string());
                }
                Ok(())
            }
            Field::Key(action) => {
                keys_page::apply_key_text(&mut self.draft, &self.config.cli, action, text)
            }
            Field::AgentOutputChars
            | Field::AgentTop
            | Field::AgentWithinTop
            | Field::AgentHits => {
                if text.trim().is_empty() {
                    self.draft.reset(field);
                    Ok(())
                } else {
                    parse_number_text(text)
                        .map(|number| self.draft.set(field, Value::Number(number)))
                }
            }
            Field::ExcludeProjects | Field::ResumeArgs | Field::AgentExclude => {
                let list = lines_to_list(text);
                if list.is_empty() {
                    self.draft.reset(field);
                } else {
                    self.draft.set(field, Value::List(list));
                }
                Ok(())
            }
            _ => Ok(()),
        };
        match result {
            Ok(()) => {
                self.errors.remove(&field);
            }
            Err(error) => {
                self.errors.insert(field, error);
            }
        }
    }

    fn set_or_reset(&mut self, field: Field, text: &str, make: impl FnOnce(&str) -> Value) {
        let text = text.trim();
        if text.is_empty() {
            self.draft.reset(field);
        } else {
            self.draft.set(field, make(text));
        }
    }

    /// `AppState` changed: fields the user has not edited follow the new
    /// gui.toml (a View-menu theme change), edited fields keep their value.
    fn refresh_from_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (config, errors) = {
            let state = app_state(cx);
            (state.config.clone(), state.config_errors.clone())
        };
        self.gui_error = errors.gui;
        let changed = self.draft.refresh_from(&config.gui_file);
        self.config = config;
        self.schedule_probe(cx);
        for field in changed {
            let text = pages::control_text(field, &self.draft);
            self.write_control(field, &text, window, cx);
            self.errors.remove(&field);
        }
        cx.notify();
    }

    /// The Retry action of the gui.toml error banner: read both files again.
    pub fn retry_config(&mut self, cx: &mut Context<Self>) {
        reload_config(cx);
    }

    /// Cancel: discard the draft, write nothing.
    pub fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        self.closed = true;
        window.close_dialog(cx);
    }

    /// Save (§11.18 step 3): the changed keys only, through core's change-set
    /// save; a refusal (gui.toml has errors) or write error stays inline.
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_save() {
            return;
        }
        let fields: Vec<Field> = Field::ALL
            .into_iter()
            .filter(|field| self.draft.is_edited(*field))
            .collect();
        if fields.is_empty() {
            self.closed = true;
            window.close_dialog(cx);
            return;
        }
        self.saving = true;
        self.save_error = None;
        cx.notify();
        let env = app_state(cx).env_with_workspace();
        let (base, edited) = (self.draft.base.clone(), self.draft.edited.clone());
        let task = cx.background_executor().spawn(async move {
            save_gui_config_changes(&env, &base, &edited).map_err(|error| error_text(&error))
        });
        let dialog = cx.entity().downgrade();
        let handle = window.window_handle();
        let app: &mut App = cx;
        app.spawn(async move |cx| {
            let result = task.await;
            cx.update(|cx| finish_save(result, fields, dialog, handle, cx));
        })
        .detach();
    }

    /// Browse… for a program (file) or the default workspace (folder).
    pub fn browse(&mut self, field: Field, window: &mut Window, cx: &mut Context<Self>) {
        let folder = field == Field::DefaultWorkspace;
        let options = OpenPathsOptions {
            files: !folder,
            directories: folder,
            multiple: false,
            prompt: if folder {
                "Choose workspace".into()
            } else {
                "Choose program".into()
            },
        };
        let dialog = cx.entity().downgrade();
        let app: &mut App = cx;
        services(app).prompt_open_paths(
            options,
            window,
            app,
            Box::new(move |result, window, cx| {
                let Ok(Some(paths)) = result else {
                    return;
                };
                let Some(path) = paths.into_iter().next() else {
                    return;
                };
                let text = path.display().to_string();
                dialog
                    .update(cx, |dialog, cx| dialog.set_text(field, &text, window, cx))
                    .ok();
            }),
        );
    }
}

/// What a finished save does (on the UI thread, outside the dialog's update).
fn finish_save(
    result: Result<PathBuf, String>,
    fields: Vec<Field>,
    dialog: WeakEntity<SettingsDialog>,
    handle: AnyWindowHandle,
    cx: &mut App,
) {
    let path = match result {
        Ok(path) => path,
        Err(error) => {
            dialog
                .update(cx, |dialog, cx| {
                    dialog.saving = false;
                    dialog.save_error = Some(error.into());
                    cx.notify();
                })
                .ok();
            return;
        }
    };
    reload_config(cx);
    apply_saved(&fields, cx);
    let still_open = dialog
        .upgrade()
        .is_some_and(|dialog| !dialog.read(cx).closed);
    let theme = app_state(cx).config.gui.theme();
    dialog
        .update(cx, |dialog, _| {
            dialog.saving = false;
            dialog.closed = true;
        })
        .ok();
    handle
        .update(cx, |_, window, cx| {
            if fields.contains(&Field::Theme) {
                theme_tokens::set_theme_preference(theme, false, window, cx);
            }
            if still_open {
                window.close_dialog(cx);
            }
            Toast::success(format!("Settings saved to {}", path.display())).show(window, cx);
        })
        .ok();
}

/// The live effects of a saved change (§11.18): key bindings, excluded
/// projects, preview refresh. (Theme: the caller; terminal, programs,
/// `[agent]`, view defaults: read from `AppState` when they are used.)
fn apply_saved(fields: &[Field], cx: &mut App) {
    let settings = app_state(cx).settings.clone();
    if fields.iter().any(|field| matches!(field, Field::Key(_))) {
        install_keys(cx);
    }
    let list = main_panes(cx).list();
    if let Some(list) = list {
        if fields.contains(&Field::ExcludeProjects) {
            let excluded = settings.exclude_projects.clone();
            list.update(cx, |list, cx| list.set_exclude_projects(excluded, cx));
        }
        if fields.contains(&Field::PreviewLast) {
            list.update(cx, |list, cx| list.set_show_last(settings.show_last, cx));
        }
    }
}

/// (Re)install the `[keys]` overrides of the current settings.
fn install_keys(cx: &mut App) {
    let settings = app_state(cx).settings.clone();
    let (keys, _) = keymap::sanitize_keys(&settings.keys);
    if let Err(error) = keymap::install_user_bindings(cx, &keys)
        && let Some(buffer) = cx.try_global::<crate::globals::DebugBufferHandle>()
    {
        buffer.0.push(
            claude_history_core::debug::DebugLevel::Warn,
            format!("[keys] {error}"),
        );
    }
}

/// The live effects of a config retry in the startup error dialog (C-023):
/// the files were re-read into `AppState` ([`reload_config`]); the key
/// bindings, the excluded projects, the preview mode and an explicit
/// gui.toml theme now take effect without a restart. `before` is the
/// effective settings from before the reload.
pub fn apply_reloaded(
    before: &claude_history_core::settings::EffectiveSettings,
    window: &mut Window,
    cx: &mut App,
) {
    let (settings, theme) = {
        let state = app_state(cx);
        let theme = state
            .config_errors
            .gui
            .is_none()
            .then(|| state.config.gui_file.gui_settings().theme)
            .flatten();
        (state.settings.clone(), theme)
    };
    if settings.keys != before.keys {
        install_keys(cx);
    }
    if let Some(list) = main_panes(cx).list() {
        if settings.exclude_projects != before.exclude_projects {
            let excluded = settings.exclude_projects.clone();
            list.update(cx, |list, cx| list.set_exclude_projects(excluded, cx));
        }
        if settings.show_last != before.show_last {
            list.update(cx, |list, cx| list.set_show_last(settings.show_last, cx));
        }
    }
    if let Some(theme) = theme
        && theme != theme_tokens::preference(cx)
    {
        theme_tokens::set_theme_preference(theme, false, window, cx);
    }
}

impl Render for SettingsDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_dialog(window, cx)
    }
}
