//! The workspace picker dropdown (PD3, DESIGN §5.3): Recent, Projects in
//! conversations (cached `project_paths`), Choose folder…, Clear workspace.
//! WP1.
//!
//! The menu builder only reads data cached on `ListPane` (the recents'
//! existence and the corpus's project paths are checked on a background
//! thread after each load): building the menu performs no file-system call.

use std::path::{Path, PathBuf};

use claude_history_core::WorkspaceScope;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::{
    App, Context, Entity, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div,
};

use crate::globals::{app_state, update_app_state};
use crate::model::OpenPathsOptions;
use crate::notify::Toast;
use crate::services::services;

use super::ListPane;

/// Projects listed in the dropdown (PD3).
pub const MAX_PROJECTS: usize = 30;

/// Test counter of `is_dir` calls made by this module (the menu builder must
/// not make any).
#[cfg(test)]
pub static IS_DIR_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// `path.is_dir()`; the only file-system access of the workspace picker.
pub fn path_is_dir(path: &Path) -> bool {
    #[cfg(test)]
    IS_DIR_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    path.is_dir()
}

/// One workspace the dropdown offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerEntry {
    /// The folder's short name.
    pub name: String,
    pub path: PathBuf,
    /// The current workspace (check mark).
    pub current: bool,
}

/// The dropdown's content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerModel {
    pub recent: Vec<PickerEntry>,
    pub projects: Vec<PickerEntry>,
    /// Clear workspace is available.
    pub can_clear: bool,
}

fn entry(path: &Path, current: Option<&Path>) -> PickerEntry {
    PickerEntry {
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
        path: path.to_path_buf(),
        current: current.is_some_and(|current| current == path),
    }
}

impl ListPane {
    /// The dropdown content from cached data only (no file-system calls).
    pub fn workspace_picker_model(&self, cx: &App) -> PickerModel {
        let current = self
            .browser
            .workspace()
            .map(|scope| scope.dir().to_path_buf());
        let current = current.as_deref();
        let recent: Vec<PickerEntry> = app_state(cx)
            .ui
            .recent_workspaces
            .iter()
            .filter(|path| self.recent_existing.contains(path))
            .map(|path| entry(path, current))
            .collect();
        let projects: Vec<PickerEntry> = self
            .project_paths
            .iter()
            .take(MAX_PROJECTS)
            .map(|path| entry(path, current))
            .collect();
        PickerModel {
            recent,
            projects,
            can_clear: current.is_some(),
        }
    }

    /// Switch to the folder `dir`: validate, store in the app state, reload.
    pub(crate) fn apply_workspace_choice(
        &mut self,
        dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !path_is_dir(&dir) {
            self.toast(
                Toast::warning(format!("Workspace not found: {}", dir.display())),
                Vec::new(),
                window,
                cx,
            );
            return;
        }
        match WorkspaceScope::new(&dir) {
            Ok(scope) => {
                let for_state = scope.clone();
                update_app_state(cx, |state, _| state.set_workspace(Some(for_state)));
                self.set_workspace(Some(scope), cx);
            }
            Err(error) => {
                self.toast(
                    Toast::error(format!("Couldn't switch the workspace: {error}")),
                    Vec::new(),
                    window,
                    cx,
                );
            }
        }
    }

    /// Clear the workspace (dropdown item; `ClearWorkspace` does the same from the menus).
    pub(crate) fn clear_workspace(&mut self, cx: &mut Context<Self>) {
        update_app_state(cx, |state, _| state.set_workspace(None));
        self.set_workspace(None, cx);
    }
}

/// File → Choose workspace… / dropdown item: the native directory picker
/// through `PlatformServices` (`Ok(None)` = cancelled).
pub fn choose_folder(window: &mut Window, cx: &mut Context<ListPane>) {
    let this = cx.entity().downgrade();
    services(cx).prompt_open_paths(
        OpenPathsOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: "Choose workspace".into(),
        },
        window,
        cx,
        Box::new(move |result, window, cx| match result {
            Ok(Some(paths)) => {
                if let Some(dir) = paths.into_iter().next() {
                    this.update(cx, |list, cx| list.apply_workspace_choice(dir, window, cx))
                        .ok();
                }
            }
            Ok(None) => {}
            Err(error) => crate::notify::error(
                window,
                cx,
                format!("Couldn't open the folder picker: {error}"),
            ),
        }),
    );
}

/// The dropdown menu of the workspace button.
pub fn build_menu(
    menu: PopupMenu,
    view: &Entity<ListPane>,
    window: &mut Window,
    cx: &mut App,
) -> PopupMenu {
    let model = view.read(cx).workspace_picker_model(cx);
    let muted = cx.theme().muted_foreground;
    let mut menu = menu;
    let section =
        |menu: PopupMenu, title: &'static str, entries: &[PickerEntry], id: &'static str| {
            if entries.is_empty() {
                return menu;
            }
            let mut menu = menu.item(PopupMenuItem::label(title));
            for entry in entries {
                let path = entry.path.clone();
                let name: SharedString = entry.name.clone().into();
                let shown: SharedString = entry.path.display().to_string().into();
                menu = menu.item(
                    PopupMenuItem::element(move |_, _| {
                        h_flex()
                            .gap_2()
                            .child(div().child(name.clone()))
                            .child(div().text_xs().text_color(muted).child(shown.clone()))
                            .into_any_element()
                    })
                    .checked(entry.current)
                    .on_click(window.listener_for(
                        view,
                        move |this: &mut ListPane, _, window, cx| {
                            let _ = id;
                            this.apply_workspace_choice(path.clone(), window, cx);
                        },
                    )),
                );
            }
            menu
        };
    menu = section(menu, "Recent", &model.recent, "recent");
    menu = section(
        menu,
        "Projects in conversations",
        &model.projects,
        "project",
    );
    if !model.recent.is_empty() || !model.projects.is_empty() {
        menu = menu.item(PopupMenuItem::separator());
    }
    menu = menu.item(
        PopupMenuItem::new("Choose folder…").on_click(window.listener_for(
            view,
            |this: &mut ListPane, _, window, cx| {
                this.choose_workspace_folder(window, cx);
            },
        )),
    );
    menu.item(
        PopupMenuItem::new("Clear workspace")
            .disabled(!model.can_clear)
            .on_click(window.listener_for(view, |this: &mut ListPane, _, _, cx| {
                this.clear_workspace(cx);
            })),
    )
}
