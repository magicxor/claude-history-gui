//! The agent panel's Read tab (DESIGN §5.4, §11.14). WP4.
//!
//! One or more refs (whitespace or commas; empty = the viewer's
//! conversation), Anchor, Focus, `Lines S..E` or `Match` + Context, the
//! visibility toggles and the budget. Cards are grouped by conversation;
//! "Load previous / Load next" extend the range of a single-ref read by 5
//! messages.

use claude_history_core::agent::AgentRequest;
use claude_history_core::agent::refs::parse_read_ref;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Styled as _, Window, div,
    rems,
};

use crate::deep_search::form::{Defaults, ReadForm, default_placeholder, help, parse_refs};
use crate::deep_search::outline::FormTab;
use crate::deep_search::panel::{AgentPanel, TabRun};
use crate::deep_search::worker::AgentJobKind;
use crate::globals::app_state;
use crate::model::AgentTab;

/// How far Load previous / Load next extend the range (messages).
pub const PAGE_MESSAGES: usize = 5;

/// The single-ref range of the last read, for paging.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paging {
    /// The ref text before the `:mA..mB` range.
    pub base: String,
    pub start: usize,
    pub end: usize,
}

impl Paging {
    /// The ref text of the range extended by one page.
    pub fn extended(&self, previous: bool) -> Paging {
        let mut next = self.clone();
        if previous {
            next.start = self.start.saturating_sub(PAGE_MESSAGES).max(1);
        } else {
            next.end = self.end + PAGE_MESSAGES;
        }
        next
    }

    pub fn text(&self) -> String {
        format!("{}:m{}..m{}", self.base, self.start, self.end)
    }
}

/// The paging state of a read of `refs`: a single ref with a range.
pub fn paging_of(refs: &[String]) -> Option<Paging> {
    let [only] = refs else { return None };
    let parsed = parse_read_ref(only).ok()?;
    let range = parsed.range?;
    Some(Paging {
        base: only
            .split_once(':')
            .map_or(only.clone(), |(base, _)| base.to_string()),
        start: range.start,
        end: range.end,
    })
}

/// State of the Read tab.
pub struct ReadTab {
    pub form: ReadForm,
    pub refs: Entity<InputState>,
    pub anchor: Entity<InputState>,
    pub focus: Entity<InputState>,
    pub lines: Entity<InputState>,
    pub match_query: Entity<InputState>,
    pub context: Entity<InputState>,
    pub budget: Entity<InputState>,
    pub run: TabRun,
    /// The range of the last single-ref read.
    pub paging: Option<Paging>,
    /// The running read is a Load previous / Load next: only then is an
    /// out-of-range answer clamped to the transcript's end. An explicit
    /// range the user typed keeps its `out-of-range` error (D-014).
    pub loading_more: bool,
}

impl ReadTab {
    pub(super) fn new(window: &mut Window, cx: &mut Context<AgentPanel>) -> Self {
        let defaults = Defaults::resolve(&app_state(cx).config.effective);
        let input = |placeholder: String, window: &mut Window, cx: &mut Context<AgentPanel>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        ReadTab {
            form: ReadForm::default(),
            refs: input(
                "Viewer's conversation, or ch_…:m10..m14 ch_…".into(),
                window,
                cx,
            ),
            anchor: input("ma_…".into(), window, cx),
            focus: input("m12".into(), window, cx),
            lines: input("S..E".into(), window, cx),
            match_query: input("text".into(), window, cx),
            context: input("Default (3)".into(), window, cx),
            budget: input(default_placeholder(defaults.budget), window, cx),
            run: TabRun::new("agent-read", cx),
            paging: None,
            loading_more: false,
        }
    }
}

impl AgentPanel {
    fn sync_read_form(&mut self, cx: &gpui_kit::App) {
        let read = &mut self.read;
        read.form.refs = read.refs.read(cx).value().to_string();
        read.form.anchor = read.anchor.read(cx).value().to_string();
        read.form.focus = read.focus.read(cx).value().to_string();
        read.form.lines = read.lines.read(cx).value().to_string();
        read.form.match_query = read.match_query.read(cx).value().to_string();
        read.form.context = read.context.read(cx).value().to_string();
        read.form.output.budget = read.budget.read(cx).value().to_string();
    }

    /// Read the refs of the Refs field (default: the viewer's conversation).
    pub fn run_read(&mut self, cx: &mut Context<Self>) {
        self.run_read_paged(false, cx);
    }

    fn run_read_paged(&mut self, loading_more: bool, cx: &mut Context<Self>) {
        self.sync_read_form(cx);
        let default_ref = self.viewer_full_ref().map(str::to_string);
        if default_ref.is_none() && parse_refs(&self.read.form.refs).is_empty() {
            self.pending_run = Some(AgentTab::Read);
            return;
        }
        match self.read.form.to_args(default_ref.as_deref()) {
            Ok(args) => {
                self.read.run.error = None;
                self.read.paging = paging_of(&args.refs);
                self.read.loading_more = loading_more;
                self.submit(
                    AgentTab::Read,
                    AgentJobKind::Read,
                    AgentRequest::Read(args),
                    "Reading…",
                    cx,
                );
            }
            Err(error) => {
                self.read.run.error = Some(error);
                cx.notify();
            }
        }
    }

    /// Put `text` in the Refs field (an ambiguous candidate was clicked, or
    /// paging rewrote the range).
    pub fn set_refs(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.read
            .refs
            .update(cx, |input, cx| input.set_value(text, window, cx));
        self.read.form.refs = text.to_string();
        cx.notify();
    }

    /// Load previous / Load next: extend the single-ref range by 5 messages
    /// and read again.
    pub fn load_more(&mut self, previous: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(paging) = self.read.paging.clone() else {
            return;
        };
        let extended = paging.extended(previous);
        self.set_refs(&extended.text(), window, cx);
        self.run_read_paged(true, cx);
    }

    /// An out-of-range answer to Load next: read up to the transcript's end.
    pub(super) fn clamp_paging(
        &mut self,
        detail: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.read.loading_more {
            return;
        }
        let Some(length) = detail
            .rsplit_once("transcript length m")
            .and_then(|(_, rest)| rest.trim().parse::<usize>().ok())
        else {
            return;
        };
        let Some(mut paging) = self.read.paging.clone() else {
            return;
        };
        if length < paging.start {
            return;
        }
        paging.end = length;
        self.set_refs(&paging.text(), window, cx);
        // The retry is a plain read: it is not clamped again.
        self.run_read(cx);
    }

    /// The Read tab's controls.
    pub(super) fn render_read_controls(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let muted = cx.theme().muted_foreground;
        let running = self.read.run.results.read(cx).is_running();
        let form = self.read.form.output.clone();
        let flags = self.render_flags(FormTab::Read, cx);
        let budget =
            self.render_budget(&self.read.budget.clone(), form.no_budget, FormTab::Read, cx);
        let labelled = |label: &'static str,
                        tooltip: &'static str,
                        input: &Entity<InputState>,
                        width: Option<f32>| {
            let element = Input::new(input).small().aria_label(label);
            h_flex()
                .id(label)
                .gap_1()
                .items_center()
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(tooltip).build(window, cx)
                })
                .child(div().text_xs().text_color(muted).child(label))
                .child(match width {
                    Some(width) => div().w(rems(width)).child(element),
                    None => div().flex_1().min_w_0().child(element),
                })
        };
        v_flex()
            .gap_1()
            .child(labelled("Refs", help::REFS, &self.read.refs, None))
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(labelled(
                        "Anchor",
                        help::ANCHOR,
                        &self.read.anchor,
                        Some(9.0),
                    ))
                    .child(labelled("Focus", help::FOCUS, &self.read.focus, Some(5.0))),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(labelled("Lines", help::LINES, &self.read.lines, Some(5.0)))
                    .child(labelled(
                        "Match",
                        help::MATCH,
                        &self.read.match_query,
                        Some(8.0),
                    ))
                    .child(labelled(
                        "Context",
                        help::CONTEXT,
                        &self.read.context,
                        // Wide enough for the `Default (3)` placeholder.
                        Some(6.0),
                    )),
            )
            .child(flags)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(budget)
                    .child(div().flex_1())
                    .child({
                        let view = view.clone();
                        if running {
                            Button::new("read-cancel")
                                .label("Cancel")
                                .small()
                                .outline()
                                .on_click(move |_, _, cx| {
                                    view.update(cx, |this, cx| this.cancel_tab(AgentTab::Read, cx));
                                })
                        } else {
                            Button::new("read-run")
                                .label("Read")
                                .small()
                                .primary()
                                .on_click(move |_, _, cx| {
                                    view.update(cx, |this, cx| this.run_read(cx));
                                })
                        }
                    }),
            )
    }
}

use gpui_kit::InteractiveElement as _;
use gpui_kit::StatefulInteractiveElement as _;
