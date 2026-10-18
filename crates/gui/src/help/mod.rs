//! Help: the Keyboard shortcuts dialog (AD11, DESIGN §5.17, §8.7). WP0 stub → WP6.

pub mod shortcuts;

use gpui_kit::component::WindowExt as _;
use gpui_kit::{App, Window};

use crate::model::HelpSection;

/// F1 / `?` / Help → Keyboard shortcuts, scrolled to `section`.
pub fn open_shortcuts(section: Option<HelpSection>, window: &mut Window, cx: &mut App) {
    shortcuts::open(section, window, cx);
}

/// Which dialog `window` shows, by the id QA scenarios compare
/// (`expect dialog == settings|shortcuts|palette|none`; any other dialog is
/// `other`). WP7's `dialog` probe reads this.
pub fn dialog_id(window: &mut Window, cx: &mut App) -> &'static str {
    if !window.has_active_dialog(cx) {
        "none"
    } else if crate::settings::current_dialog(cx).is_some() {
        "settings"
    } else if shortcuts::current(cx).is_some() {
        "shortcuts"
    } else if crate::palette::is_open(cx) {
        "palette"
    } else {
        "other"
    }
}
