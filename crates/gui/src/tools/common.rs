//! Plumbing shared by the Tools dialogs (DESIGN §5.10–§5.18, §7.5). WP5.
//!
//! * [`open_dialog`] opens a dialog whose content is an entity, focuses it on
//!   the first render, registers its QA id (`dialog` probe) and keeps Enter
//!   from closing it when the dialog uses Enter itself.
//! * [`Current`] remembers the most recently opened dialog of each kind
//!   (tests and QA find it there).
//! * [`spawn_thread`] runs blocking work on a named 8 MiB std thread and hands
//!   the result to the UI through a channel (§7.5 "Tools").
//! * [`Scope`] is the All | Workspace control of the report dialogs.

use std::cell::Cell;
use std::rc::Rc;

use claude_history_core::debug::DebugLevel;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonGroup};
use gpui_kit::component::{Disableable as _, Selectable as _, Sizable as _};
use gpui_kit::{
    AnyWindowHandle, App, Entity, FocusHandle, Global, IntoElement, ParentElement as _, Render,
    SharedString, WeakEntity, Window, div,
};

use crate::globals::{DebugBufferHandle, app_state};
use crate::workspace::startup::{LastDialog, last_dialog, register_dialog};

/// Stack size of every Tools worker thread (§7.5).
pub const WORKER_STACK: usize = 8 * 1024 * 1024;

/// The most recently opened dialog of kind `T`, while it exists.
pub struct Current<T: 'static>(Option<(WeakEntity<T>, AnyWindowHandle)>);

impl<T: 'static> Global for Current<T> {}

/// Remember `entity` as the current dialog of its kind.
pub fn set_current<T: 'static>(cx: &mut App, entity: &Entity<T>, window: &Window) {
    cx.set_global(Current(Some((entity.downgrade(), window.window_handle()))));
}

/// Decide what an `open_*` entry point does with the dialog that already
/// exists (`existing`): one in the invoking window is reused (returned); one
/// in another window is closed there (`mark_closed` flags it) so the dialog
/// opens in the invoking window instead (Tools dialogs are per window).
pub fn place_here<T: 'static>(
    cx: &mut App,
    window: &Window,
    id: &str,
    existing: Option<Entity<T>>,
    mark_closed: impl FnOnce(&mut T),
) -> Option<Entity<T>> {
    let existing = existing?;
    let handle = cx
        .try_global::<Current<T>>()
        .and_then(|current| current.0.as_ref())
        .map(|(_, handle)| *handle)?;
    if handle == window.window_handle() {
        return Some(existing);
    }
    existing.update(cx, |dialog, _| mark_closed(dialog));
    let _ = handle.update(cx, |_, other, cx| other.close_dialog(cx));
    forget_dialog(id, cx);
    None
}

/// The current dialog of kind `T`, if it still exists.
pub fn current<T: 'static>(cx: &App) -> Option<Entity<T>> {
    cx.try_global::<Current<T>>()
        .and_then(|current| current.0.as_ref())
        .and_then(|(entity, _)| entity.upgrade())
}

/// The window the current dialog of kind T lives in (tests).
pub fn current_window<T: 'static>(cx: &App) -> Option<AnyWindowHandle> {
    cx.try_global::<Current<T>>()
        .and_then(|current| current.0.as_ref())
        .map(|(_, handle)| *handle)
}

/// What a Tools dialog needs from [`open_dialog`].
#[derive(Clone, Copy)]
pub struct DialogSpec {
    /// The id the QA `dialog` probe reports.
    pub id: &'static str,
    pub title: &'static str,
    pub width_rem: f32,
    /// `false`: the dialog uses Enter itself (inputs, buttons) and Enter must
    /// not close it.
    pub enter_closes: bool,
}

/// Open `content` as a dialog of `window`. `on_dismiss` runs when Esc closes
/// it; [`close_dialog`] is the programmatic route.
pub fn open_dialog<T: Render + 'static>(
    spec: DialogSpec,
    content: Entity<T>,
    focus: FocusHandle,
    on_dismiss: impl Fn(&mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    register_dialog(cx, spec.id);
    let first_render = Rc::new(Cell::new(true));
    let on_dismiss = Rc::new(on_dismiss);
    let width = window.rem_size() * spec.width_rem;
    window.open_dialog(cx, move |builder, window, cx| {
        if first_render.replace(false) {
            focus.clone().focus(window, cx);
        }
        let dismiss = on_dismiss.clone();
        let dismiss_ok = on_dismiss.clone();
        let builder = builder
            .title(spec.title)
            .w(width)
            .child(content.clone())
            // The buttons live in the content (their state depends on it).
            .footer(div())
            .on_cancel(move |_, _, cx| {
                forget_dialog(spec.id, cx);
                dismiss(cx);
                true
            });
        if spec.enter_closes {
            // Enter closes: forget the QA id and flag the dialog closed too.
            builder.on_ok(move |_, _, cx| {
                forget_dialog(spec.id, cx);
                dismiss_ok(cx);
                true
            })
        } else {
            builder.on_ok(|_, _, _| false)
        }
    });
}

/// Close the top-most dialog of `window` and clear its QA id.
pub fn close_dialog(id: &str, window: &mut Window, cx: &mut App) {
    forget_dialog(id, cx);
    window.close_dialog(cx);
}

/// Clear the QA id of a dialog that is gone.
pub fn forget_dialog(id: &str, cx: &mut App) {
    if last_dialog(cx).as_ref() == id {
        cx.set_global(LastDialog::default());
    }
}

/// Run `work` on a named worker thread; its result arrives on the returned
/// channel (closed without a value when the thread panicked).
pub fn spawn_thread<T: Send + 'static>(
    name: &str,
    work: impl FnOnce() -> T + Send + 'static,
) -> async_channel::Receiver<T> {
    let (tx, rx) = async_channel::bounded(1);
    let spawned = std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(WORKER_STACK)
        .spawn(move || {
            let _ = tx.send_blocking(work());
        });
    if spawned.is_err() {
        // No thread: the receiver reports a closed channel.
        let (_, rx) = async_channel::bounded(1);
        return rx;
    }
    rx
}

/// Like [`spawn_thread`], for work that reports progress: `work` gets the
/// sender of an unbounded channel and returns when it is done (dropping the
/// sender closes the channel).
pub fn spawn_progress_thread<T: Send + 'static>(
    name: &str,
    work: impl FnOnce(async_channel::Sender<T>) + Send + 'static,
) -> async_channel::Receiver<T> {
    let (tx, rx) = async_channel::unbounded();
    let spawned = std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(WORKER_STACK)
        .spawn(move || work(tx));
    if spawned.is_err() {
        let (_, rx) = async_channel::unbounded();
        return rx;
    }
    rx
}

/// The scope control of the report dialogs (§11.24).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    All,
    Workspace,
}

impl Scope {
    /// The preselected scope: Workspace when the tool was started with
    /// `local` and a workspace exists.
    pub fn initial(local: bool, cx: &App) -> Scope {
        if local && app_state(cx).workspace.is_some() {
            Scope::Workspace
        } else {
            Scope::All
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Scope::All => "All workspaces",
            Scope::Workspace => "Current workspace",
        }
    }
}

/// Whether a workspace is chosen (the Workspace scope is available).
pub fn workspace_available(cx: &App) -> bool {
    app_state(cx).workspace.is_some()
}

/// A two-button segmented control for [`Scope`].
pub fn scope_control(
    id: &'static str,
    scope: Scope,
    workspace_available: bool,
    disabled: bool,
    on_select: impl Fn(Scope, &mut Window, &mut App) + Clone + 'static,
) -> impl IntoElement {
    let button = |which: Scope, enabled: bool| {
        let on_select = on_select.clone();
        Button::new(SharedString::from(format!("{id}-{which:?}")))
            .label(which.label())
            .small()
            .selected(scope == which)
            .disabled(disabled || !enabled)
            .on_click(move |_, window, cx| on_select(which, window, cx))
    };
    ButtonGroup::new(id)
        .outline()
        .compact()
        .child(button(Scope::All, true))
        .child(button(Scope::Workspace, workspace_available))
}

/// The debug level Debug log lines are shown from (the dialog's capture
/// level, default Info).
pub fn log_level(cx: &App) -> DebugLevel {
    cx.try_global::<DebugBufferHandle>()
        .map(|handle| handle.0.min_level())
        .unwrap_or(DebugLevel::Info)
}

/// The corpus of the one-shot report dialogs (§11.24): a fresh load, never
/// the list's own corpus.
pub type Corpus = std::sync::Arc<Vec<claude_history_core::history::Conversation>>;

/// Load the report corpus (blocking; worker thread): every conversation
/// inside `filter`, newest first.
pub fn load_corpus(
    env: &claude_history_core::HistoryEnv,
    show_last: bool,
    debug: Option<DebugLevel>,
    filter: &claude_history_core::time_filter::TimeFilter,
) -> Result<Corpus, String> {
    claude_history_core::maintenance::load_sorted_conversations(env, show_last, debug, filter)
        .map(std::sync::Arc::new)
        .map_err(|error| error_text(&error))
}

/// `Corpus: fresh load · since 2d (launch filter) · 812 conversations`.
pub fn corpus_line(count: usize, cx: &App) -> String {
    let filter = app_state(cx).launch.time_filter;
    let label = crate::widgets::time_range::launch_filter_label(&filter, chrono::Local::now());
    let mut line = "Corpus: fresh load".to_string();
    if let Some(label) = label {
        let mut text = label.to_string();
        if let Some(first) = text.get(..1).map(str::to_lowercase) {
            text.replace_range(..1, &first);
        }
        line.push_str(&format!(" · {text} (launch filter)"));
    }
    line.push_str(&format!(" · {count} conversations"));
    line
}

/// The text of a core error in-app (never `Configuration error: ` noise).
pub fn error_text(error: &claude_history_core::AppError) -> String {
    crate::settings::error_text(error)
}

/// What an open delete confirmation shows: its title and the paths it lists
/// (tests and QA read it from the owning dialog; it is `None` once the
/// confirmation is gone).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShownConfirm {
    pub title: String,
    pub paths: Vec<String>,
}

impl ShownConfirm {
    /// Summarise the title and path items handed to the confirmation.
    pub fn new(title: &str, items: &[crate::widgets::PathItem]) -> Self {
        Self {
            title: title.to_string(),
            paths: items.iter().map(|item| item.display_path()).collect(),
        }
    }
}
