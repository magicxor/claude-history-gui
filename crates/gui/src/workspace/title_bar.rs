//! The title bar content (PD1, DESIGN §5.1): the in-window `AppMenuBar` on
//! Windows/Linux and the window title. WP0 → WP6.
//!
//! `TitleBar::window_options()` makes the title bar app-drawn, so every
//! window root renders [`AppTitleBar`] as its first child. Interactive
//! children stop mouse-down propagation so they do not start a window drag.

use gpui_kit::component::menu::AppMenuBar;
use gpui_kit::component::{ActiveTheme as _, TitleBar, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Entity, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Window, div,
};

use crate::globals::AppState;

#[cfg(test)]
#[path = "title_bar_tests.rs"]
mod title_bar_tests;

/// The app name used in window titles.
pub const APP_TITLE: &str = "Claude History";

/// `Claude History — <workspace short name | All projects>` (+ ` — Select` in picker mode).
pub fn window_title(cx: &App) -> String {
    let Some(state) = cx.try_global::<AppState>() else {
        return APP_TITLE.to_string();
    };
    let scope = state
        .workspace
        .as_ref()
        .and_then(|workspace| {
            workspace
                .dir()
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "All projects".to_string());
    let mut title = format!("{APP_TITLE} — {scope}");
    if state.is_picker() {
        title.push_str(" — Select");
    }
    title
}

/// The title of a single-file window: `<file name> — Claude History`.
pub fn single_file_title(file_name: &str) -> String {
    format!("{file_name} — {APP_TITLE}")
}

/// The app-drawn title bar of a window.
#[derive(IntoElement)]
pub struct AppTitleBar {
    menu_bar: Option<Entity<AppMenuBar>>,
    title: SharedString,
}

impl AppTitleBar {
    pub fn new(menu_bar: Option<Entity<AppMenuBar>>, title: impl Into<SharedString>) -> Self {
        Self {
            menu_bar,
            title: title.into(),
        }
    }
}

impl RenderOnce for AppTitleBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        if self.menu_bar.is_some() {
            crate::menus::sync_availability(window, cx);
        }
        let muted = cx.theme().muted_foreground;
        // The title shrinks (and truncates) before the window controls are
        // pushed out of a narrow window (integrator, Phase A screenshot review:
        // a 640 px single-file window clipped the close button).
        TitleBar::new().child(
            h_flex()
                .size_full()
                .min_w_0()
                .overflow_hidden()
                .gap_2()
                .when_some(self.menu_bar, |this, menu_bar| {
                    this.child(
                        div()
                            .flex_shrink_0()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child(menu_bar),
                    )
                })
                // The kit's bar never shrinks below its content's min-content
                // width, so the title is laid out absolutely: it adds nothing
                // to that width and truncates in the space that is left.
                .child(
                    div().flex_1().min_w_0().h_full().relative().child(
                        h_flex()
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .justify_center()
                            .child(
                                div()
                                    .min_w_0()
                                    .text_sm()
                                    .text_color(muted)
                                    .truncate()
                                    .child(self.title),
                            ),
                    ),
                ),
        )
    }
}
