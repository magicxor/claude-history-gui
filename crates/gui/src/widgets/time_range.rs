//! Time filter dropdown and the "Custom range…" dialog (DESIGN §5.3), shared
//! by the conversation list and Deep search. WP1.
//!
//! Presets map to `TimePoint` values (`1h`, `1d`, `2d`, `1w`, `1mo`, `6mo`,
//! `1y`); the custom dialog validates with `TimePoint::from_str` and
//! `TimeFilter::resolve` and shows `TimeFilterError` inline.

use std::rc::Rc;
use std::str::FromStr as _;

use chrono::{DateTime, Local, Timelike as _};
use claude_history_core::time_filter::{TimeFilter, TimeFilterError, TimePoint};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, StyledExt as _, WindowExt as _,
    h_flex, v_flex,
};
use gpui_kit::{
    Anchor, App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div,
};

/// A chosen time range: the resolved filter plus its chip label
/// (`Any time`, `Since 2d`, `Before 2026-07-20`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TimeRangeChoice {
    pub filter: TimeFilter,
    /// `None` = no filter ("Any time").
    pub label: Option<SharedString>,
}

/// The presets of the dropdown: (menu label, short label, `TimePoint` text).
pub const PRESETS: &[(&str, &str, &str)] = &[
    ("Last hour", "1h", "1h"),
    ("Last day", "1d", "1d"),
    ("Last 2 days", "2d", "2d"),
    ("Last week", "1w", "1w"),
    ("Last month", "1mo", "1mo"),
    ("Last 6 months", "6mo", "6mo"),
    ("Last year", "1y", "1y"),
];

/// The tooltip texts of the two custom-range fields (ADD E4).
pub const SINCE_HELP: &str =
    "Only conversations this recent, as a duration or date (2d, 1d6h, 6mo, 2026-07-20)";
pub const BEFORE_HELP: &str = "Only conversations older than this duration or date";

/// The choice for a preset's `TimePoint` text (`Since 2d`), resolved now.
pub fn preset_choice(text: &str) -> Result<TimeRangeChoice, String> {
    choice_from_texts(text, "")
}

/// Validate the two fields of the custom dialog. Both empty = Any time.
/// Errors are the inline texts: core's `TimeFilterError` display, or
/// `Invalid time range: …` for an inverted range.
pub fn choice_from_texts(since: &str, before: &str) -> Result<TimeRangeChoice, String> {
    let since = since.trim();
    let before = before.trim();
    let parse = |text: &str| -> Result<Option<TimePoint>, String> {
        if text.is_empty() {
            return Ok(None);
        }
        TimePoint::from_str(text)
            .map(Some)
            .map_err(|error| error.to_string())
    };
    let since_point = parse(since)?;
    let before_point = parse(before)?;
    let filter = TimeFilter::resolve(since_point.as_ref(), None, before_point.as_ref()).map_err(
        |error| match error {
            TimeFilterError::InvertedRange { .. } => format!("Invalid time range: {error}"),
            other => other.to_string(),
        },
    )?;
    let mut parts: Vec<String> = Vec::new();
    if !since.is_empty() {
        parts.push(format!("Since {since}"));
    }
    if !before.is_empty() {
        parts.push(format!("Before {before}"));
    }
    Ok(TimeRangeChoice {
        filter,
        label: (!parts.is_empty()).then(|| parts.join(" · ").into()),
    })
}

/// The fields' texts of a chip label (`Since 2d · Before 2026-07-20`).
fn texts_from_label(label: Option<&SharedString>) -> (String, String) {
    let mut since = String::new();
    let mut before = String::new();
    if let Some(label) = label {
        for part in label.split(" · ") {
            if let Some(text) = part.strip_prefix("Since ") {
                since = text.to_string();
            } else if let Some(text) = part.strip_prefix("Before ") {
                before = text.to_string();
            }
        }
    }
    (since, before)
}

/// The chip label of a filter given on the command line. Core keeps no source
/// text of a `TimePoint`, so it is rebuilt from the resolved bounds: an
/// instant with whole seconds is an absolute date the user typed; any other
/// is a span back from the launch, written in the coarsest whole unit
/// (`1d`, `30h`, `90m`). Months (`6mo`) show as days.
pub fn launch_filter_label(filter: &TimeFilter, now: DateTime<Local>) -> Option<SharedString> {
    let mut parts: Vec<String> = Vec::new();
    let absolute = |at: DateTime<Local>| -> String {
        let format = if at.hour() == 0 && at.minute() == 0 && at.second() == 0 {
            "%Y-%m-%d"
        } else if at.second() == 0 {
            "%Y-%m-%d %H:%M"
        } else {
            "%Y-%m-%d %H:%M:%S"
        };
        at.format(format).to_string()
    };
    let describe = |at: DateTime<Local>, upper: bool| -> String {
        if at.nanosecond() == 0 {
            return absolute(at);
        }
        if upper && at.nanosecond() == 999_999_999 {
            // An absolute upper bound extends to the end of the typed precision.
            let next = at + chrono::Duration::nanoseconds(1);
            let step = if next.hour() == 0 && next.minute() == 0 && next.second() == 0 {
                chrono::Duration::days(1)
            } else if next.second() == 0 {
                chrono::Duration::minutes(1)
            } else {
                chrono::Duration::seconds(1)
            };
            return absolute(next - step);
        }
        let minutes = (now - at).num_seconds().max(0);
        let minutes = (minutes + 30) / 60;
        if minutes > 0 && minutes % (24 * 60) == 0 {
            format!("{}d", minutes / (24 * 60))
        } else if minutes > 0 && minutes % 60 == 0 {
            format!("{}h", minutes / 60)
        } else {
            format!("{minutes}m")
        }
    };
    if let Some(after) = filter.after {
        parts.push(format!("Since {}", describe(after, false)));
    }
    if let Some(before) = filter.before {
        parts.push(format!("Before {}", describe(before, true)));
    }
    (!parts.is_empty()).then(|| parts.join(" · ").into())
}

/// A callback receiving the chosen range.
pub type OnRange = Rc<dyn Fn(TimeRangeChoice, &mut Window, &mut App)>;

/// The dropdown button of a time filter: presets, Any time, Custom range….
/// `on_choose` receives the choice (resolved when clicked).
pub fn time_range_button(
    id: impl Into<gpui_kit::ElementId>,
    current: TimeRangeChoice,
    on_choose: impl Fn(TimeRangeChoice, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let on_choose: OnRange = Rc::new(on_choose);
    let label = current.label.clone().unwrap_or_else(|| "Any time".into());
    let current_label = current.label.clone();
    let initial = current.clone();
    Button::new(id)
        .icon(IconName::Calendar)
        .label(label)
        .xsmall()
        .outline()
        .dropdown_caret(true)
        .tooltip("Time filter")
        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _window, _cx| {
            let mut menu = menu;
            let choose = on_choose.clone();
            menu = menu.item(
                PopupMenuItem::new("Any time")
                    .checked(current_label.is_none())
                    .on_click(move |_, window, cx| {
                        choose(TimeRangeChoice::default(), window, cx);
                    }),
            );
            for (menu_label, short, text) in PRESETS {
                let choose = on_choose.clone();
                let checked = current_label
                    .as_ref()
                    .is_some_and(|label| label.as_ref() == format!("Since {text}"));
                menu = menu.item(
                    PopupMenuItem::new(format!("{menu_label}    {short}"))
                        .checked(checked)
                        .on_click(move |_, window, cx| match preset_choice(text) {
                            Ok(choice) => choose(choice, window, cx),
                            Err(error) => crate::notify::error(window, cx, error),
                        }),
                );
            }
            menu = menu.item(PopupMenuItem::separator());
            let choose = on_choose.clone();
            let initial = initial.clone();
            menu.item(
                PopupMenuItem::new("Custom range…").on_click(move |_, window, cx| {
                    let choose = choose.clone();
                    open_custom_range_dialog(
                        initial.clone(),
                        move |choice, window, cx| choose(choice, window, cx),
                        window,
                        cx,
                    );
                }),
            )
        })
}

/// The content of the custom range dialog.
pub struct TimeRangeForm {
    since: Entity<InputState>,
    before: Entity<InputState>,
    error: Option<String>,
    choice: Option<TimeRangeChoice>,
    on_apply: OnRange,
    _subscriptions: Vec<Subscription>,
}

impl TimeRangeForm {
    fn new(
        initial: &TimeRangeChoice,
        on_apply: OnRange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (since_text, before_text) = texts_from_label(initial.label.as_ref());
        let since = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("2d, 1d6h, 6mo, 2026-07-20")
                .default_value(since_text)
        });
        let before = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("2026-07-20")
                .default_value(before_text)
        });
        let mut subscriptions = Vec::new();
        for input in [&since, &before] {
            subscriptions.push(cx.subscribe_in(
                input,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => this.revalidate(cx),
                    InputEvent::PressEnter { .. } => this.apply(window, cx),
                    _ => {}
                },
            ));
        }
        let mut form = Self {
            since,
            before,
            error: None,
            choice: None,
            on_apply,
            _subscriptions: subscriptions,
        };
        form.revalidate(cx);
        form
    }

    fn revalidate(&mut self, cx: &mut Context<Self>) {
        let since = self.since.read(cx).value();
        let before = self.before.read(cx).value();
        match choice_from_texts(&since, &before) {
            Ok(choice) => {
                self.choice = Some(choice);
                self.error = None;
            }
            Err(error) => {
                self.choice = None;
                self.error = Some(error);
            }
        }
        cx.notify();
    }

    fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(choice) = self.choice.clone() else {
            return;
        };
        let on_apply = self.on_apply.clone();
        window.close_dialog(cx);
        on_apply(choice, window, cx);
    }

    /// The inline error, if the fields are invalid.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Whether Apply is enabled.
    pub fn can_apply(&self) -> bool {
        self.choice.is_some()
    }

    /// The input entities (tests).
    pub fn inputs(&self) -> (Entity<InputState>, Entity<InputState>) {
        (self.since.clone(), self.before.clone())
    }
}

impl Render for TimeRangeForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let field = |label: &'static str, input: &Entity<InputState>, help: &'static str| {
            v_flex()
                .gap_1()
                .child(div().text_sm().font_medium().child(label))
                .child(Input::new(input).small().aria_label(label))
                .child(div().text_xs().text_color(muted).child(help))
        };
        v_flex()
            .id("time-range-form")
            .gap_3()
            .child(field("Since", &self.since, SINCE_HELP))
            .child(field("Before", &self.before, BEFORE_HELP))
            .child(
                div()
                    .id("time-range-error")
                    .min_h(gpui_kit::rems(1.0))
                    .text_xs()
                    .text_color(danger)
                    .when_some(self.error.clone(), |this, error| {
                        this.child(format!("⚠ {error}"))
                    }),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("time-range-cancel")
                            .label("Cancel")
                            .outline()
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("time-range-apply")
                            .label("Apply")
                            .primary()
                            .disabled(self.choice.is_none())
                            .on_click(cx.listener(|this, _, window, cx| this.apply(window, cx))),
                    ),
            )
    }
}

use gpui_kit::prelude::FluentBuilder as _;

/// The most recently opened custom range form (tests).
#[cfg(test)]
pub struct LastTimeRangeForm(pub gpui_kit::WeakEntity<TimeRangeForm>);

#[cfg(test)]
impl gpui_kit::Global for LastTimeRangeForm {}

/// Open the custom range dialog; `on_apply` receives the validated choice.
pub fn open_custom_range_dialog(
    initial: TimeRangeChoice,
    on_apply: impl Fn(TimeRangeChoice, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let on_apply: OnRange = Rc::new(on_apply);
    let form = cx.new(|cx| TimeRangeForm::new(&initial, on_apply, window, cx));
    #[cfg(test)]
    cx.set_global(LastTimeRangeForm(form.downgrade()));
    let first_render = std::cell::Cell::new(true);
    let width = window.rem_size() * 34.0;
    window.open_dialog(cx, move |dialog, window, cx| {
        if first_render.replace(false) {
            let since = form.read(cx).since.focus_handle(cx);
            since.focus(window, cx);
        }
        dialog.title("Time filter").width(width).child(form.clone())
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    #[test]
    fn texts_validate_like_the_cli() {
        let any = choice_from_texts("", "  ").unwrap();
        assert_eq!(any.label, None);
        assert!(!any.filter.is_active());
        let since = choice_from_texts("2d", "").unwrap();
        assert_eq!(since.label.as_deref(), Some("Since 2d"));
        assert!(since.filter.after.is_some() && since.filter.before.is_none());
        let both = choice_from_texts("2026-01-01", "2026-02-01").unwrap();
        assert_eq!(
            both.label.as_deref(),
            Some("Since 2026-01-01 · Before 2026-02-01")
        );
        assert_eq!(
            choice_from_texts("2x", "").unwrap_err(),
            "'2x': unknown unit 'x' (s, m (minutes), h, d, w, mo (months), y)"
        );
        let inverted = choice_from_texts("2026-03-01", "2026-02-01").unwrap_err();
        assert!(
            inverted.starts_with("Invalid time range: lower bound "),
            "{inverted}"
        );
        assert!(inverted.contains("is later than upper bound"));
    }

    #[test]
    fn presets_resolve() {
        for (_, short, text) in PRESETS {
            let choice = preset_choice(text).unwrap();
            assert_eq!(
                choice.label.as_deref(),
                Some(format!("Since {short}").as_str())
            );
        }
    }

    #[test]
    fn launch_labels_are_rebuilt_from_the_bounds() {
        let now = Local.with_ymd_and_hms(2026, 10, 15, 12, 0, 0).unwrap();
        let day_ago = now - chrono::Duration::days(1) - chrono::Duration::nanoseconds(123);
        let filter = TimeFilter {
            after: Some(day_ago),
            before: None,
        };
        assert_eq!(
            launch_filter_label(&filter, now).as_deref(),
            Some("Since 1d")
        );
        let hours = TimeFilter {
            after: Some(now - chrono::Duration::hours(30) - chrono::Duration::nanoseconds(5)),
            before: None,
        };
        assert_eq!(
            launch_filter_label(&hours, now).as_deref(),
            Some("Since 30h")
        );
        let absolute = TimeFilter {
            after: None,
            before: Some(
                Local.with_ymd_and_hms(2026, 7, 20, 23, 59, 59).unwrap()
                    + chrono::Duration::nanoseconds(999_999_999),
            ),
        };
        assert_eq!(
            launch_filter_label(&absolute, now).as_deref(),
            Some("Before 2026-07-20")
        );
        assert_eq!(launch_filter_label(&TimeFilter::default(), now), None);
    }

    #[test]
    fn labels_round_trip_into_the_dialog_fields() {
        let label: SharedString = "Since 2d · Before 2026-07-20".into();
        assert_eq!(
            texts_from_label(Some(&label)),
            ("2d".to_string(), "2026-07-20".to_string())
        );
        assert_eq!(texts_from_label(None), (String::new(), String::new()));
    }
}
