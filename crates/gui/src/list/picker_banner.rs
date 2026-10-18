//! The picker-mode banner above the list (PD5): `Picker mode: Ctrl+O or
//! Select prints the session ID and exits` (text per picker action) with a
//! primary **Select** button. WP1.

use claude_history_core::settings::PickerAction;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Sizable as _, h_flex};
use gpui_kit::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, Styled as _,
    TestSupportExt as _, div,
};

use crate::actions::SelectAndExit;
use crate::globals::app_state;
use crate::theme_tokens::roles;

/// The banner text for the picker action (§PD5).
pub fn banner_text(action: PickerAction) -> &'static str {
    match action {
        PickerAction::ShowPath => "Picker mode: Ctrl+O or Select prints the path and exits",
        PickerAction::ShowId => "Picker mode: Ctrl+O or Select prints the session ID and exits",
        PickerAction::Resume { fork: false } => {
            "Picker mode: Ctrl+O or Select resumes the conversation and exits"
        }
        PickerAction::Resume { fork: true } => {
            "Picker mode: Ctrl+O or Select forks the conversation and exits"
        }
    }
}

/// The banner element (picker mode only).
pub fn render(cx: &App) -> Option<AnyElement> {
    let action = app_state(cx).launch.picker?;
    let roles = roles(cx);
    Some(
        h_flex()
            .id("picker-banner")
            .test_support()
            .w_full()
            .flex_none()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .bg(roles.sidebar)
            .border_b_1()
            .border_color(roles.info)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(roles.foreground)
                    .child(banner_text(action)),
            )
            .child(
                Button::new("picker-select")
                    .label("Select")
                    .xsmall()
                    .primary()
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(SelectAndExit), cx);
                    }),
            )
            .into_any_element(),
    )
}
