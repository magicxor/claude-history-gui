//! Help → About Claude History GUI (DESIGN §5.18; T-021). WP5.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, Styled as _, Window, div,
};

use super::common::{
    DialogSpec, close_dialog, current, forget_dialog, open_dialog, place_here, set_current,
};
use crate::globals::app_state;
use crate::persist::state_file;
use crate::services::services;

/// The QA id of the dialog.
pub const DIALOG_ID: &str = "about";
/// The one-line description.
pub const TAGLINE: &str = "A desktop browser for Claude Code, Pi and OMP history.";
/// The license line.
pub const LICENSE_LINE: &str =
    "License: Unlicense. Core derived from claude-history (MIT, © 2024 Raine).";
/// Where the project lives.
pub const PROJECT_URL: &str = "https://github.com/magicxor/claude-history-gui";

/// The GUI's version.
pub fn gui_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The core crate's version (its `Cargo.toml`, read at compile time; the core
/// exposes no constant).
pub fn core_version() -> &'static str {
    const MANIFEST: &str = include_str!("../../../core/Cargo.toml");
    MANIFEST
        .lines()
        .map(str::trim)
        .find_map(|line| {
            line.strip_prefix("version = \"")
                .and_then(|rest| rest.strip_suffix('"'))
        })
        .unwrap_or("unknown")
}

/// `Claude History GUI 0.1.0 (core 0.1.76)`.
pub fn version_line() -> String {
    format!(
        "Claude History GUI {} (core {})",
        gui_version(),
        core_version()
    )
}

/// The lines of the dialog (version, text, data paths), for tests and render.
pub fn about_lines(cx: &App) -> Vec<String> {
    let state = app_state(cx);
    let env = &state.env;
    let show = |path: Option<std::path::PathBuf>| {
        path.map(|path| path.display().to_string())
            .unwrap_or_else(|| "(unknown)".to_string())
    };
    let data = env
        .claude_dir()
        .map(|dir| dir.display().to_string())
        .unwrap_or_else(|error| error.to_string());
    let mut config = show(env.gui_config_file());
    if env.config_file().is_some() {
        config.push_str(" (+ config.toml)");
    }
    vec![
        version_line(),
        TAGLINE.to_string(),
        LICENSE_LINE.to_string(),
        format!("Data: {data}"),
        format!("Cache: {}", show(env.cache_root())),
        format!("Config: {config}"),
        format!("State: {}", show(state_file(env))),
    ]
}

/// The dialog content.
pub struct AboutDialog {
    focus: FocusHandle,
    lines: Vec<String>,
    closed: bool,
}

impl Focusable for AboutDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// The open dialog (tests and QA).
pub fn current_dialog(cx: &App) -> Option<Entity<AboutDialog>> {
    current::<AboutDialog>(cx).filter(|dialog| !dialog.read(cx).closed)
}

impl AboutDialog {
    pub fn lines(&self) -> &[String] {
        &self.lines
    }
}

impl Render for AboutDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mono = cx.theme().mono_font_family.clone();
        v_flex()
            .id("about-dialog")
            .track_focus(&self.focus)
            .w_full()
            .gap_2()
            .children(self.lines.iter().enumerate().map(|(ix, line)| {
                let row = div().id(("about-line", ix)).child(line.clone());
                match ix {
                    0 => row.text_base().font_weight(gpui_kit::FontWeight::SEMIBOLD),
                    1 | 2 => row.text_sm(),
                    _ => row.text_xs().text_color(muted).font_family(mono.clone()),
                }
            }))
            .child(
                h_flex()
                    .gap_2()
                    .pt_2()
                    .child(
                        Button::new("about-updates")
                            .label("Check for updates…")
                            .small()
                            .on_click(|_, window, cx| {
                                close_dialog(DIALOG_ID, window, cx);
                                super::check_for_updates(window, cx);
                            }),
                    )
                    .child(
                        Button::new("about-project")
                            .label("Open project page")
                            .small()
                            .on_click(|_, _, cx| services(cx).open_url(PROJECT_URL, cx)),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("about-close")
                            .label("Close")
                            .small()
                            .primary()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.closed = true;
                                close_dialog(DIALOG_ID, window, cx);
                            })),
                    ),
            )
    }
}

/// Help → About Claude History GUI (§5.18).
pub fn open_about(window: &mut Window, cx: &mut App) {
    let existing = current_dialog(cx);
    if place_here(cx, window, DIALOG_ID, existing, |d: &mut AboutDialog| {
        d.closed = true
    })
    .is_some()
    {
        return;
    }
    let lines = about_lines(cx);
    let dialog = cx.new(|cx| AboutDialog {
        focus: cx.focus_handle(),
        lines,
        closed: false,
    });
    set_current(cx, &dialog, window);
    let focus = dialog.read(cx).focus.clone();
    let dismissed = dialog.clone();
    open_dialog(
        DialogSpec {
            id: DIALOG_ID,
            title: "About Claude History GUI",
            width_rem: 38.0,
            enter_closes: true,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_read_from_the_manifests() {
        assert_eq!(gui_version(), "0.1.1");
        assert_eq!(core_version(), "0.1.76");
        assert_eq!(version_line(), "Claude History GUI 0.1.1 (core 0.1.76)");
    }
}
