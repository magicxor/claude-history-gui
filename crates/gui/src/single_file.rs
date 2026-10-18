//! Single-file viewer windows (PD14, DESIGN §5.5, §11.12). WP0 stub → WP2.
//!
//! `claude-history-gui FILE` (or File → Open JSONL file…) opens a window with
//! only a viewer (`ViewerMode::SingleFile`). The menu tree is the shared one:
//! Resume, Fork, Rename, Delete and Select and exit have no handler here (not
//! on this root, not in the single-file viewer) and are disabled.
//! `secondary-w` / Esc / `q` close the window; the app quits when no window
//! remains. A deletion broadcast (`CorpusBus`) of the shown file clears the
//! viewer and marks the agent panel's result sets.
//!
//! The header and the conversation's source come from a background parse
//! with core's single-file browser (`Browser::new_single_file`, §11.12): a
//! missing path shows `File not found: <p>`, a directory `Not a file: <p>`,
//! a file the viewer would render empty `<file> contains no conversation
//! messages` (D1, V-091); other errors show their reason.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use claude_history_core::HistoryEnv;
use claude_history_core::browser::{Browser, BrowserOptions};
use claude_history_core::history::Conversation;

use gpui_kit::component::menu::AppMenuBar;
use gpui_kit::component::{ActiveTheme as _, TitleBar, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Action, AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _, Subscription,
    Task, Window, WindowBounds, WindowOptions, div, px, size,
};

use crate::actions::*;
use crate::globals::{ActivePane, CorpusBus, CorpusNotice, app_state, same_path};
use crate::keymap::CTX_SINGLE_FILE;
use crate::model::{ConversationTarget, HeaderInfo, OpenRequest, PaneFocus, ViewerMode};
use crate::viewer::{ViewerPane, ViewerPaneEvent};
use crate::workspace::routing::{self, Forwarded, RouteTargets};
use crate::workspace::title_bar::{AppTitleBar, single_file_title};
use crate::{exit, menus, theme_tokens};

/// Default single-file window size (logical px).
pub const SINGLE_FILE_WINDOW_SIZE: (f32, f32) = (1000.0, 800.0);

/// The root of a single-file window.
pub struct SingleFileWindow {
    window: AnyWindowHandle,
    path: PathBuf,
    menu_bar: Option<Entity<AppMenuBar>>,
    viewer: Entity<ViewerPane>,
    title: String,
    /// The file was deleted (a later probe result must not resurrect it).
    deleted: bool,
    _probe_task: Task<()>,
    _subscriptions: Vec<Subscription>,
}

/// What the background probe of a single file found.
#[derive(Clone, Debug)]
pub enum FileProbe {
    /// The file parsed into a conversation (header, source).
    Conversation(Box<Conversation>),
    /// Readable, not listable, but the viewer shows something.
    Unlisted,
    /// Shown instead of the transcript (`File not found: …`, D1 …).
    Error(String),
}

/// Probe `path` like the TUI's single-file mode (blocking; background).
pub fn probe_file(env: Arc<HistoryEnv>, path: &Path) -> FileProbe {
    match std::fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return FileProbe::Error(format!("File not found: {}", path.display()));
        }
        Err(error) => return FileProbe::Error(error.to_string()),
        Ok(metadata) if !metadata.is_file() => {
            return FileProbe::Error(format!("Not a file: {}", path.display()));
        }
        Ok(_) => {}
    }
    let browser = Browser::new_single_file(path.to_path_buf(), BrowserOptions::new(env));
    if let Some(error) = browser.single_file_error() {
        return FileProbe::Error(error.to_string());
    }
    match browser.conversations().first() {
        Some(conversation) => FileProbe::Conversation(Box::new((**conversation).clone())),
        None => FileProbe::Unlisted,
    }
}

impl SingleFileWindow {
    pub fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let menu_bar = (!cfg!(target_os = "macos")).then(|| {
            let bar = AppMenuBar::new(cx);
            menus::register_menu_bar(&bar, cx);
            bar
        });
        let viewer = cx.new(|cx| ViewerPane::new(ViewerMode::SingleFile, window, cx));
        let target = ConversationTarget::from_parsed_file(&path, None);
        viewer.update(cx, |viewer, cx| {
            viewer.open(OpenRequest::new(target, true), window, cx);
            viewer.set_header(None, cx);
        });
        let env = app_state(cx).env.clone();
        let probe_path = path.clone();
        let probe_task = cx.spawn_in(window, async move |this, cx| {
            let probe = cx
                .background_spawn(async move { probe_file(env, &probe_path) })
                .await;
            this.update_in(cx, |this, _, cx| this.apply_probe(probe, cx))
                .ok();
        });
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let title = single_file_title(&file_name);
        window.set_window_title(&title);
        let bus = CorpusBus::entity(cx);
        let subscriptions = vec![
            cx.subscribe_in(&bus, window, Self::on_corpus_notice),
            cx.subscribe_in(
                &viewer,
                window,
                |_, _, event: &ViewerPaneEvent, window, _| {
                    if let ViewerPaneEvent::LeaveRequested = event {
                        window.remove_window();
                    }
                },
            ),
            cx.observe_window_appearance(window, |_, window, cx| {
                theme_tokens::follow_system(window, cx);
            }),
        ];
        Self {
            window: window.window_handle(),
            path,
            menu_bar,
            viewer,
            title,
            deleted: false,
            _probe_task: probe_task,
            _subscriptions: subscriptions,
        }
    }

    /// The background probe finished: header and source, or the error state.
    fn apply_probe(&mut self, probe: FileProbe, cx: &mut Context<Self>) {
        if self.deleted {
            return;
        }
        let path = self.path.clone();
        self.viewer.update(cx, |viewer, cx| match probe {
            FileProbe::Conversation(conversation) => {
                viewer.set_target(
                    ConversationTarget::from_parsed_file(&path, Some(conversation.as_ref())),
                    cx,
                );
                viewer.set_header(Some(HeaderInfo::from_conversation(&conversation)), cx);
            }
            FileProbe::Unlisted => viewer.set_header(None, cx),
            FileProbe::Error(error) => {
                viewer.set_header(None, cx);
                viewer.set_file_error(Some(error), cx);
            }
        });
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn viewer(&self) -> &Entity<ViewerPane> {
        &self.viewer
    }

    /// QA probes of a single-file window (§15.3): the `viewer.*` keys of its
    /// viewer and `focus` (`viewer` | `agent` | `dialog` | `none`), like the
    /// main window's `Workspace::qa_probe`.
    #[cfg(any(test, feature = "qa"))]
    pub fn qa_probe(&self, key: &str, window: &mut Window, cx: &mut App) -> Option<String> {
        use gpui_kit::component::WindowExt as _;
        match key {
            "focus" => {
                let dialog = window.has_active_dialog(cx);
                let viewer = self.viewer.read(cx);
                Some(
                    if dialog {
                        "dialog"
                    } else if viewer.agent_panel().read(cx).contains_focus(window, cx) {
                        "agent"
                    } else if viewer.contains_focus(window, cx) {
                        "viewer"
                    } else {
                        "none"
                    }
                    .to_string(),
                )
            }
            "window.kind" => Some("single-file".to_string()),
            key if key.starts_with("viewer.") => self.viewer.read(cx).qa_probe(key, cx),
            _ => None,
        }
    }

    fn on_close_window(&mut self, _: &CloseWindow, window: &mut Window, _: &mut Context<Self>) {
        window.remove_window();
    }

    /// A deleted file: the viewer shows nothing; agent result sets for it
    /// are marked (dialogs are closed by the app's `CorpusBus` handler).
    fn on_corpus_notice(
        &mut self,
        _: &Entity<CorpusBus>,
        notice: &CorpusNotice,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let CorpusNotice::Deleted(paths) = notice;
        let shows_deleted = self
            .viewer
            .read(cx)
            .target()
            .is_some_and(|target| paths.iter().any(|path| same_path(path, &target.path)));
        if shows_deleted {
            self.deleted = true;
            self.viewer.update(cx, |viewer, cx| viewer.clear(cx));
        }
        let panel = self.viewer.read(cx).agent_panel().clone();
        panel.update(cx, |panel, cx| panel.mark_deleted(paths, cx));
    }

    fn app_action(&mut self, action: &dyn Action, window: &mut Window, cx: &mut Context<Self>) {
        let active = ActivePane::of_window(cx, self.window);
        if !routing::run_app_action(action, Some(&self.viewer), active, window, cx) {
            cx.propagate();
        }
    }

    fn route_fallback(&mut self, action: &dyn Action, window: &mut Window, cx: &mut Context<Self>) {
        // The transcript, or the agent panel when it had focus last (its
        // result set's target, §7.8).
        let active = match ActivePane::of_window(cx, self.window) {
            PaneFocus::AgentPanel => PaneFocus::AgentPanel,
            _ => PaneFocus::Viewer,
        };
        let targets = RouteTargets {
            list: None,
            viewer: Some(self.viewer.clone()),
            deep: None,
            active,
        };
        // Single-file windows have no Resume/Fork/Rename/Delete/Select (PD14).
        if routing::is_main_window_only(action) || routing::is_list_action(action) {
            cx.propagate();
            return;
        }
        match routing::forward(action, &targets, window, cx) {
            Forwarded::Unhandled => cx.propagate(),
            Forwarded::Handled => {}
            Forwarded::Open(req) => {
                // Only this window's own file can be shown here.
                let own = self
                    .viewer
                    .read(cx)
                    .target()
                    .is_some_and(|target| same_path(&target.path, &req.target.path));
                if own {
                    self.viewer
                        .update(cx, |viewer, cx| viewer.open(*req, window, cx));
                }
            }
        }
    }
}

impl Render for SingleFileWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let picker = crate::globals::app_state(cx).is_picker();
        let has_workspace = crate::globals::app_state(cx).workspace.is_some();
        let root = v_flex()
            .id("single-file-window")
            .key_context(CTX_SINGLE_FILE)
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .on_action(cx.listener(Self::on_close_window))
            // Element-level Quit (menu enabled state); the global handler
            // covers dispatch with nothing focused.
            .on_action(cx.listener(|_, _: &Quit, _, cx| exit::quit(cx)))
            .on_action(cx.listener(|this, action: &ShowConversations, window, cx| {
                this.app_action(action, window, cx)
            }))
            .when(!picker, |root| {
                root.on_action(cx.listener(|this, action: &OpenJsonlFile, window, cx| {
                    this.app_action(action, window, cx)
                }))
            })
            // Only with a workspace (T-016; integrator, Phase C).
            .when(has_workspace, |root| {
                root.on_action(
                    cx.listener(|this, action: &ShowProjectsDirectory, window, cx| {
                        this.app_action(action, window, cx)
                    }),
                )
            });
        let root = crate::on_app_actions!(root, cx, Self::app_action);
        let root = crate::on_actions!(
            root,
            cx,
            Self::route_fallback,
            [
                CycleToolDisplay,
                SetToolsSummary,
                SetToolsTruncated,
                SetToolsFull,
                ToggleThinking,
                ToggleTiming,
                StartSearch,
                NextMatch,
                PrevMatch,
                ExportConversation,
                YankOrCopyMessage,
                CopyFocusedMessage,
                CopyPath,
                CopySessionId,
                CopyHeaderSessionId,
                ShowPath,
                RevealInFileManager,
                SearchWithinConversation,
                ToggleOutline,
                ReadRange,
                CopyAsLedger,
                CopyAsPlain,
                CopyAsMarkdown,
                CopyAsJsonl,
            ]
        );
        root.child(AppTitleBar::new(self.menu_bar.clone(), self.title.clone()))
            .child(div().flex_1().min_h_0().child(self.viewer.clone()))
    }
}

/// Open a single-file viewer window for `path` (§11.12).
pub fn open_window(path: PathBuf, cx: &mut App) -> AnyWindowHandle {
    let (width, height) = SINGLE_FILE_WINDOW_SIZE;
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(width), px(height)),
            cx,
        ))),
        window_min_size: Some(size(px(640.), px(480.))),
        app_id: Some("claude-history-gui".into()),
        #[cfg(target_os = "linux")]
        window_decorations: Some(gpui_kit::WindowDecorations::Client),
        ..TitleBar::window_options()
    };
    // C-037: the remembered single-file placement replaces the default size
    // (integrator, Phase A, WP6 request).
    let options =
        crate::persist::with_restored_bounds(options, crate::persist::WindowKind::SingleFile, cx);
    let (handle, _root) = gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| SingleFileWindow::new(path, window, cx))
    })
    .expect("failed to open a single-file window");
    handle
}
