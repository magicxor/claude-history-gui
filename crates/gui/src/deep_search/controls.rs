//! Small form controls shared by the Deep search form and the agent panel
//! tabs: segmented choices, labelled inputs and checkboxes. WP4.
//!
//! Every callback takes `(&mut Window, &mut App)`; the owners build them from
//! their entity (`view.update(cx, |this, cx| this.method(.., cx))`), so a
//! control never holds a `Context`.

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonGroup};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, h_flex, v_flex,
};
use gpui_kit::{
    App, Entity, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div,
};

/// A callback with the picked index.
pub type OnPick = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// A callback with the new checked state.
pub type OnChange = Rc<dyn Fn(bool, &mut Window, &mut App)>;

/// One option of a [`segmented`] control.
pub struct Segment {
    pub label: SharedString,
    pub tooltip: Option<SharedString>,
    pub disabled: bool,
}

impl Segment {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            tooltip: None,
            disabled: false,
        }
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

/// A row of mutually exclusive buttons; `on_pick` gets the index.
pub fn segmented(
    id: &'static str,
    segments: Vec<Segment>,
    current: usize,
    on_pick: OnPick,
) -> impl IntoElement {
    let mut group = ButtonGroup::new(id).outline().compact();
    for (ix, segment) in segments.into_iter().enumerate() {
        let on_pick = on_pick.clone();
        let mut button = Button::new(SharedString::from(format!("{id}-{ix}")))
            .label(segment.label)
            .xsmall()
            .selected(ix == current)
            .disabled(segment.disabled)
            .on_click(move |_, window, cx| on_pick(ix, window, cx));
        if let Some(tooltip) = segment.tooltip {
            button = button.tooltip(tooltip);
        }
        group = group.child(button);
    }
    group
}

/// A checkbox with a label and an optional tooltip.
pub fn checkbox(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    checked: bool,
    disabled: bool,
    tooltip: Option<SharedString>,
    on_change: OnChange,
) -> impl IntoElement {
    let label: SharedString = label.into();
    let mut checkbox = Checkbox::new(id.into())
        .label(label)
        .checked(checked)
        .disabled(disabled)
        .small()
        .on_click(move |checked: &bool, window, cx| on_change(*checked, window, cx));
    if let Some(tooltip) = tooltip {
        checkbox = checkbox.tooltip(tooltip);
    }
    checkbox
}

/// `Label` above a small input.
pub fn field(
    label: &'static str,
    input: &Entity<InputState>,
    cx: &App,
) -> impl IntoElement + use<> {
    let muted = cx.theme().muted_foreground;
    v_flex()
        .gap_0p5()
        .min_w_0()
        .child(div().text_xs().text_color(muted).child(label))
        .child(Input::new(input).small().aria_label(label))
}

/// `Label` and a small input on one line.
pub fn inline_field(
    label: &'static str,
    input: &Entity<InputState>,
    width_rem: f32,
    cx: &App,
) -> impl IntoElement + use<> {
    let muted = cx.theme().muted_foreground;
    h_flex()
        .gap_1()
        .items_center()
        .child(div().text_xs().text_color(muted).child(label))
        .child(
            div()
                .w(gpui_kit::rems(width_rem))
                .child(Input::new(input).small().aria_label(label)),
        )
}
