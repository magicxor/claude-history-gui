//! The viewer's agent panel: Outline / Within / Read (PD11, DESIGN §5.4,
//! §11.14). WP4.
//!
//! The panel is a **sibling** of the transcript's `Viewer` key context (own
//! context `AgentPanel`), so viewer bindings never see its focus (F21).
//! Every result set stores the target it was computed for; rows reveal in
//! their own conversation ([`AgentPanelEvent::Reveal`]).
//!
//! Frozen contracts:
//! * key contexts: `AgentPanel` on the root (Esc, `o`, `?`),
//!   `AgentPanel > Input` for its inputs (Enter runs) and `AgentRows` on the
//!   focusable row list (`↑`/`↓`/`j`/`k`, Enter reveals) — the panel's
//!   buttons keep Enter/Space;
//! * session actions with the panel focused reach the window root's fallback
//!   (`ActivePane::AgentPanel`), which uses [`AgentPanel::selected_target`]
//!   (the selected row's result-set target), never the viewer's conversation;
//! * `AgentPanelEscape`: the panel clears its input or cancels a running job;
//!   with nothing to clear or cancel it calls `cx.propagate()` and the viewer
//!   focuses the transcript.
//!
//! The viewer's conversation key (`ch_…`, K8) is resolved on the agent
//! worker when the panel is shown (and again when the viewer target changes
//! while it is shown). A file outside the history roots has none: the panel
//! body says so (D-035) and nothing runs.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

use claude_history_core::agent::AgentRequest;
use claude_history_core::agent::args::AgentOutlineArgs;
use claude_history_core::config::ConfigFile;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tab::TabBar;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, CursorStyle, DragMoveEvent, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div,
    px, rems,
};

use crate::actions::{AgentPanelEscape, CancelDeepSearch, RunDeepSearch, ToggleAgentText};
use crate::deep_search::controls::checkbox;
use crate::deep_search::form::{Defaults, Flag, OutputForm, default_placeholder};
use crate::deep_search::outline::{FormTab, OutlineTab};
use crate::deep_search::read::ReadTab;
use crate::deep_search::results::{ResultsEvent, ResultsView, progress_text};
use crate::deep_search::within::WithinTab;
use crate::deep_search::worker::{
    AgentClient, AgentJob, AgentJobKind, AgentReply, AgentWorkerHandle,
};
use crate::globals::same_path;
use crate::globals::{ActivePane, app_state};
use crate::keymap::CTX_AGENT;
use crate::model::{AgentTab, ConversationTarget, PaneFocus, RevealRequest};

/// The panel body of a file outside the history roots (D-035).
pub const OUT_OF_ROOT_TEXT: &str = "Outline, Within and Read work only for conversations inside the Claude, Pi or OMP history folders.";

/// Events of the agent panel (§7.2).
#[derive(Clone, Debug, PartialEq)]
pub enum AgentPanelEvent {
    /// Reveal a message of the conversation the result set was computed for
    /// (may differ from the viewer's).
    Reveal {
        target: ConversationTarget,
        reveal: RevealRequest,
    },
}

/// The conversation a result set belongs to, captured when it ran.
#[derive(Clone, Debug, PartialEq)]
pub struct SetInfo {
    pub target: ConversationTarget,
    pub full_ref: String,
}

/// Where the viewer's conversation key stands (K8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyState {
    /// No viewer conversation, or not resolved yet (the panel is hidden).
    Unknown,
    Pending(PathBuf),
    Known {
        path: PathBuf,
        full_ref: String,
    },
    /// Not inside the history roots.
    Unavailable(PathBuf),
    /// The lookup failed (e.g. an unreadable history folder): never silent
    /// (PD16), the panel shows the error.
    Failed(PathBuf, String),
}

/// One tab's result set and run bookkeeping.
pub struct TabRun {
    pub results: Entity<ResultsView>,
    /// The generation of the latest submitted job (older replies drop).
    pub generation: u64,
    /// The set of the running job.
    pub running: Option<SetInfo>,
    /// The set the rows belong to.
    pub set: Option<SetInfo>,
    /// A form error (validation) shown inline.
    pub error: Option<String>,
    /// The conversation of the selected row.
    pub selected: Option<ConversationTarget>,
}

impl TabRun {
    pub(super) fn new(id: &'static str, cx: &mut Context<AgentPanel>) -> Self {
        TabRun {
            results: cx.new(|cx| ResultsView::new(id, false, cx)),
            generation: 0,
            running: None,
            set: None,
            error: None,
            selected: None,
        }
    }
}

/// The agent panel.
pub struct AgentPanel {
    pub(super) focus: FocusHandle,
    pub(super) visible: bool,
    pub(super) tab: AgentTab,
    pub(super) viewer_target: Option<ConversationTarget>,
    pub(super) key: KeyState,
    /// A run requested before the key was known.
    pub(super) pending_run: Option<AgentTab>,
    key_generation: u64,
    pub(super) client: AgentClient,
    pub(super) outline: OutlineTab,
    pub(super) within: WithinTab,
    pub(super) read: ReadTab,
    deleted: Vec<PathBuf>,
    /// The `[agent]` defaults the placeholders show.
    applied: Defaults,
    _reply_task: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<AgentPanelEvent> for AgentPanel {}

impl Focusable for AgentPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl AgentPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle().tab_stop(true);
        let client = AgentWorkerHandle::global(cx).client();
        let replies = client.replies();
        let reply_task = cx.spawn_in(window, async move |this, cx| {
            while let Ok(reply) = replies.recv().await {
                if this
                    .update_in(cx, |this, window, cx| this.on_reply(reply, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let outline = OutlineTab::new(window, cx);
        let within = WithinTab::new(window, cx);
        let read = ReadTab::new(window, cx);
        let mut subscriptions = vec![
            cx.on_focus_in(&focus, window, |_, window, cx| {
                ActivePane::set(cx, window.window_handle(), PaneFocus::AgentPanel);
            }),
            // Settings changes (live apply, K12) change the `Default (N)`
            // placeholders.
            cx.observe_global_in::<crate::globals::AppState>(window, |this, window, cx| {
                this.refresh_defaults(window, cx);
            }),
        ];
        for results in [
            outline.run.results.clone(),
            within.run.results.clone(),
            read.run.results.clone(),
        ] {
            subscriptions.push(cx.subscribe_in(
                &results,
                window,
                |this, results, event: &ResultsEvent, window, cx| {
                    this.on_results_event(results, event, window, cx)
                },
            ));
        }
        Self {
            focus,
            visible: false,
            tab: AgentTab::Outline,
            viewer_target: None,
            key: KeyState::Unknown,
            pending_run: None,
            key_generation: 0,
            client,
            outline,
            within,
            read,
            deleted: Vec::new(),
            applied: Defaults::resolve(&app_state(cx).config.effective),
            _reply_task: reply_task,
            _subscriptions: subscriptions,
        }
    }

    /// `[agent]` changed: the budget and Top placeholders follow.
    fn refresh_defaults(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let defaults = Defaults::resolve(&app_state(cx).config.effective);
        if defaults == self.applied {
            return;
        }
        for (input, value) in [
            (self.outline.budget.clone(), defaults.budget),
            (self.within.budget.clone(), defaults.budget),
            (self.within.top.clone(), defaults.within_top),
            (self.read.budget.clone(), defaults.budget),
        ] {
            let placeholder = default_placeholder(value);
            input.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx)
            });
        }
        self.applied = defaults;
        cx.notify();
    }

    // ----- viewer target and visibility ----------------------------------------------

    /// The viewer opened another conversation (re-runs a visible Outline;
    /// Within/Read sets become stale, §11.14).
    pub fn set_viewer_target(
        &mut self,
        target: Option<ConversationTarget>,
        cx: &mut Context<Self>,
    ) {
        let changed = match (&self.viewer_target, &target) {
            (Some(old), Some(new)) => !same_path(&old.path, &new.path),
            (None, None) => false,
            _ => true,
        };
        // The viewer's parsed title replaces the file name in the result
        // sets of the same conversation.
        if let Some(new) = &target {
            for run in [
                &mut self.outline.run,
                &mut self.within.run,
                &mut self.read.run,
            ] {
                for set in [&mut run.set, &mut run.running].into_iter().flatten() {
                    if same_path(&set.target.path, &new.path) {
                        set.target = new.clone();
                    }
                }
            }
        }
        self.viewer_target = target;
        if changed {
            self.key = KeyState::Unknown;
            self.pending_run = None;
            self.key_generation = self.key_generation.wrapping_add(1);
            self.client.cancel(AgentJobKind::KeyForPath(PathBuf::new()));
            if self.visible {
                self.start_key_lookup(cx);
            }
        }
        cx.notify();
    }

    /// Show the panel on `tab` and focus it.
    pub fn show_tab(&mut self, tab: AgentTab, window: &mut Window, cx: &mut Context<Self>) {
        let was_visible = self.visible;
        self.tab = tab;
        self.visible = true;
        if !was_visible || matches!(self.key, KeyState::Unknown) {
            self.start_key_lookup(cx);
        }
        self.maybe_run_outline(cx);
        self.focus_tab(window, cx);
        cx.notify();
    }

    /// Focus the tab's primary element: the query, the Refs field, or the
    /// outline's row list (`j`/`k`/Enter work as soon as the panel opens).
    fn focus_tab(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shows_message() {
            // No tab body (rows, inputs) is rendered: keep the focus on the
            // panel so `o` and Esc still work (integrator, Phase B).
            self.focus.focus(window, cx);
            return;
        }
        match self.tab {
            AgentTab::Within => self.within.query.focus_handle(cx).focus(window, cx),
            AgentTab::Read => self.read.refs.focus_handle(cx).focus(window, cx),
            AgentTab::Outline => {
                let rows = self.outline.run.results.clone();
                rows.update(cx, |rows, cx| rows.focus_rows(window, cx));
            }
        }
    }

    /// The element [`Self::focus_tab`] focuses for the current tab.
    fn tab_focus_handle(&self, cx: &App) -> FocusHandle {
        match self.tab {
            AgentTab::Within => self.within.query.focus_handle(cx),
            AgentTab::Read => self.read.refs.focus_handle(cx),
            AgentTab::Outline => self.outline.run.results.read(cx).focus_handle(cx),
        }
    }

    /// The panel shows a message instead of the tab body: a file outside the
    /// history roots (D-035), a failed key lookup, or no conversation.
    fn shows_message(&self) -> bool {
        matches!(self.key, KeyState::Unavailable(_) | KeyState::Failed(..))
            || self.viewer_target.is_none()
    }

    /// Down in an input of the panel moves into the tab's rows.
    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key != "down" || event.keystroke.modifiers.modified() {
            return;
        }
        let results = self.current_run().results.clone();
        let rows_focused = results
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx);
        if rows_focused || results.read(cx).selectable_count() == 0 {
            return;
        }
        results.update(cx, |rows, cx| {
            if rows.selected().is_none() {
                rows.select(0, cx);
            }
            rows.focus_rows(window, cx);
        });
        cx.stop_propagation();
    }

    /// Select `tab` (a click on the tab bar).
    pub fn select_tab(&mut self, tab: AgentTab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.maybe_run_outline(cx);
        cx.notify();
    }

    /// Result sets for deleted conversations are marked (clicks disabled).
    pub fn mark_deleted(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        self.deleted.extend(paths.iter().cloned());
        for results in self.all_results() {
            results.update(cx, |results, cx| results.mark_deleted(paths, cx));
        }
        cx.notify();
    }

    fn all_results(&self) -> [Entity<ResultsView>; 3] {
        [
            self.outline.run.results.clone(),
            self.within.run.results.clone(),
            self.read.run.results.clone(),
        ]
    }

    /// The conversation of the selected row's result set (session actions
    /// from the panel use it, §7.8); without a row, the viewer's.
    pub fn selected_target(&self) -> Option<ConversationTarget> {
        let selected = self.current_run().selected.clone();
        // The outline follows the viewer: a row of the previous
        // conversation's outline no longer counts.
        let selected = selected.filter(|target| {
            self.tab != AgentTab::Outline
                || self
                    .viewer_target
                    .as_ref()
                    .is_none_or(|viewer| same_path(&viewer.path, &target.path))
        });
        // The row's own target knows only what the agent protocol does (file
        // stem as title, no project or custom title): the result set's
        // target, captured from the viewer, is richer for its conversation.
        let set_target = self.current_run().set.as_ref().map(|set| &set.target);
        let selected = selected.map(|row| match set_target {
            Some(target) if same_path(&target.path, &row.path) => target.clone(),
            _ => row,
        });
        selected.or_else(|| self.viewer_target.clone())
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn tab(&self) -> AgentTab {
        self.tab
    }

    pub fn hide(&mut self, cx: &mut Context<Self>) {
        self.visible = false;
        cx.notify();
    }

    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
    }

    /// The viewer's full `ch_` ref, once resolved.
    pub fn viewer_full_ref(&self) -> Option<&str> {
        match &self.key {
            KeyState::Known { full_ref, .. } => Some(full_ref),
            _ => None,
        }
    }

    pub fn key_state(&self) -> &KeyState {
        &self.key
    }

    fn current_run(&self) -> &TabRun {
        match self.tab {
            AgentTab::Outline => &self.outline.run,
            AgentTab::Within => &self.within.run,
            AgentTab::Read => &self.read.run,
        }
    }

    fn run_of(&mut self, tab: AgentTab) -> &mut TabRun {
        match tab {
            AgentTab::Outline => &mut self.outline.run,
            AgentTab::Within => &mut self.within.run,
            AgentTab::Read => &mut self.read.run,
        }
    }

    /// The result set of `tab`.
    pub fn set_of(&self, tab: AgentTab) -> Option<&SetInfo> {
        match tab {
            AgentTab::Outline => self.outline.run.set.as_ref(),
            AgentTab::Within => self.within.run.set.as_ref(),
            AgentTab::Read => self.read.run.set.as_ref(),
        }
    }

    /// The results entity of `tab`.
    pub fn results_of(&self, tab: AgentTab) -> &Entity<ResultsView> {
        match tab {
            AgentTab::Outline => &self.outline.run.results,
            AgentTab::Within => &self.within.run.results,
            AgentTab::Read => &self.read.run.results,
        }
    }

    /// Whether `set` belongs to another conversation than the viewer's.
    pub fn is_stale(&self, set: &SetInfo) -> bool {
        self.viewer_target
            .as_ref()
            .is_none_or(|viewer| !same_path(&viewer.path, &set.target.path))
    }

    // ----- key lookup ---------------------------------------------------------------------

    fn start_key_lookup(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.viewer_target.clone() else {
            self.key = KeyState::Unknown;
            return;
        };
        if matches!(&self.key, KeyState::Pending(path) | KeyState::Known { path, .. } | KeyState::Unavailable(path) | KeyState::Failed(path, _) if same_path(path, &target.path))
        {
            return;
        }
        self.key = KeyState::Pending(target.path.clone());
        let state = app_state(cx);
        let generation = self.client.submit(AgentJob {
            kind: AgentJobKind::KeyForPath(target.path.clone()),
            request: AgentRequest::Outline(AgentOutlineArgs::default()),
            env: state.env_with_workspace(),
            config: state.config.effective.clone(),
        });
        self.key_generation = generation;
    }

    // ----- running ------------------------------------------------------------------------

    /// Submit a job of `tab`; the set is captured now.
    pub(super) fn submit(
        &mut self,
        tab: AgentTab,
        kind: AgentJobKind,
        request: AgentRequest,
        label: &'static str,
        cx: &mut Context<Self>,
    ) {
        let (Some(target), Some(full_ref)) = (
            self.viewer_target.clone(),
            self.viewer_full_ref().map(str::to_string),
        ) else {
            return;
        };
        let state = app_state(cx);
        let job = AgentJob {
            kind,
            request,
            env: state.env_with_workspace(),
            config: state.config.effective.clone(),
        };
        let generation = self.client.submit(job);
        let run = self.run_of(tab);
        run.generation = generation;
        run.running = Some(SetInfo { target, full_ref });
        run.results
            .update(cx, |results, cx| results.set_running(label, cx));
        cx.notify();
    }

    /// Run the current tab (Enter in an input).
    pub fn run_current(&mut self, cx: &mut Context<Self>) {
        match self.tab {
            AgentTab::Outline => self.run_outline(cx),
            AgentTab::Within => self.run_within(cx),
            AgentTab::Read => self.run_read(cx),
        }
    }

    /// Cancel the running job of `tab`.
    pub fn cancel_tab(&mut self, tab: AgentTab, cx: &mut Context<Self>) {
        let kind = match tab {
            AgentTab::Outline => AgentJobKind::Outline,
            AgentTab::Within => AgentJobKind::Within,
            AgentTab::Read => AgentJobKind::Read,
        };
        if !self.results_of(tab).read(cx).is_running() {
            return;
        }
        self.client.cancel(kind);
        let run = self.run_of(tab);
        run.generation = run.generation.wrapping_add(u64::MAX / 2);
        run.running = None;
        run.results
            .update(cx, |results, cx| results.set_cancelled(cx));
        cx.notify();
    }

    /// The stale banner's button: run the tab for the viewer's conversation.
    pub fn run_for_viewer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.tab {
            AgentTab::Outline => self.run_outline(cx),
            AgentTab::Within => self.run_within(cx),
            AgentTab::Read => {
                // The default refs are the viewer's conversation.
                self.set_refs("", window, cx);
                self.run_read(cx);
            }
        }
    }

    fn on_reply(&mut self, reply: AgentReply, window: &mut Window, cx: &mut Context<Self>) {
        match reply {
            AgentReply::Key {
                generation,
                path,
                full_ref,
                error,
            } => {
                if generation != self.key_generation
                    || !matches!(&self.key, KeyState::Pending(pending) if same_path(pending, &path))
                {
                    return;
                }
                self.key = match (full_ref, error) {
                    (Some(full_ref), _) => KeyState::Known { path, full_ref },
                    (None, Some(error)) => KeyState::Failed(path, error),
                    (None, None) => KeyState::Unavailable(path),
                };
                // The tab body that had (or was just given) the focus is
                // replaced by a message: the panel keeps the focus, else it
                // would be lost. `is_focused` also covers a reply that
                // arrives before the panel's first frame was drawn.
                if self.shows_message()
                    && (self.focus.contains_focused(window, cx)
                        || self.tab_focus_handle(cx).is_focused(window))
                {
                    self.focus.focus(window, cx);
                }
                self.after_key(cx);
                cx.notify();
            }
            AgentReply::Progress {
                kind,
                generation,
                notice,
            } => {
                let Some(tab) = tab_of(&kind) else { return };
                if self.run_of(tab).generation != generation {
                    return;
                }
                if let Some(text) = progress_text(&notice) {
                    self.run_of(tab)
                        .results
                        .update(cx, |results, cx| results.set_progress(text, cx));
                }
            }
            AgentReply::Done {
                kind,
                generation,
                response,
            } => {
                let Some(tab) = tab_of(&kind) else { return };
                if self.run_of(tab).generation != generation {
                    return;
                }
                if tab == AgentTab::Read {
                    self.retarget_read_set(&response);
                }
                let titles = self.titles(cx);
                let paging_detail = (tab == AgentTab::Read)
                    .then_some(response.error.as_ref())
                    .flatten()
                    .filter(|error| {
                        error.kind
                            == claude_history_core::agent::diagnostic::AgentErrorKind::OutOfRange
                    })
                    .map(|error| error.detail.clone());
                let run = self.run_of(tab);
                if let Some(set) = run.running.take() {
                    run.set = Some(set);
                }
                run.results.update(cx, |results, cx| {
                    results.set_response(response, &titles, window, cx)
                });
                if tab == AgentTab::Read {
                    let paging = self
                        .read
                        .paging
                        .as_ref()
                        .map(|paging| (paging.start, paging.end));
                    self.read
                        .run
                        .results
                        .update(cx, |results, cx| results.set_paging(paging, cx));
                    if let Some(detail) = paging_detail
                        && self.read.paging.is_some()
                    {
                        self.clamp_paging(&detail, window, cx);
                    }
                }
                cx.notify();
            }
        }
    }

    /// D-034: a Read result set belongs to the conversation it read, not to
    /// the viewer's: Refs may name others. The viewer's own conversation
    /// keeps the viewer's target (title, project, custom title); with
    /// several conversations the viewer's wins if it is among them, else the
    /// first read.
    fn retarget_read_set(&mut self, response: &claude_history_core::agent::AgentResponse) {
        let Some(claude_history_core::agent::AgentResult::Read(read)) = &response.result else {
            return;
        };
        let Some(running) = self.read.run.running.as_mut() else {
            return;
        };
        let viewer = running.target.path.clone();
        let chosen = read
            .conversations
            .iter()
            .find(|info| same_path(&info.path, &viewer))
            .or_else(|| read.conversations.first());
        if let Some(info) = chosen
            && !same_path(&info.path, &viewer)
        {
            running.target = ConversationTarget::from_agent_info(info, None);
            running.full_ref = info.full_ref.clone();
        }
    }

    /// The viewer's title by full ref (headers of read cards).
    fn titles(&self, _: &App) -> BTreeMap<String, String> {
        let mut titles = BTreeMap::new();
        if let (Some(target), Some(full_ref)) = (&self.viewer_target, self.viewer_full_ref()) {
            titles.insert(full_ref.to_string(), target.title.to_string());
        }
        for set in [&self.read.run.running, &self.read.run.set]
            .into_iter()
            .flatten()
        {
            titles.insert(set.full_ref.clone(), set.target.title.to_string());
        }
        titles
    }

    /// The key is known: run what was asked for, or the automatic outline.
    fn after_key(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.key, KeyState::Known { .. }) {
            self.pending_run = None;
            return;
        }
        match self.pending_run.take() {
            Some(AgentTab::Outline) => self.run_outline(cx),
            Some(AgentTab::Within) => self.run_within(cx),
            Some(AgentTab::Read) => self.run_read(cx),
            None => self.maybe_run_outline(cx),
        }
    }

    fn on_results_event(
        &mut self,
        results: &Entity<ResultsView>,
        event: &ResultsEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ResultsEvent::Selection(target) => {
                for tab in [AgentTab::Outline, AgentTab::Within, AgentTab::Read] {
                    if self.results_of(tab) == results {
                        self.run_of(tab).selected = target.clone();
                    }
                }
            }
            ResultsEvent::Open { target, reveal, .. } => cx.emit(AgentPanelEvent::Reveal {
                target: target.clone(),
                reveal: reveal.clone(),
            }),
            ResultsEvent::FillRefs(handle) => {
                self.tab = AgentTab::Read;
                self.set_refs(handle, window, cx);
            }
            ResultsEvent::LoadMore { previous } => self.load_more(*previous, window, cx),
        }
    }

    // ----- shared controls -----------------------------------------------------------------

    fn output_form_mut(&mut self, tab: FormTab) -> Option<&mut OutputForm> {
        match tab {
            FormTab::Outline => Some(&mut self.outline.form),
            FormTab::Read => Some(&mut self.read.form.output),
            FormTab::Within => None,
        }
    }

    /// A visibility toggle changed.
    pub fn set_flag(&mut self, tab: FormTab, flag: Flag, value: bool, cx: &mut Context<Self>) {
        if let Some(form) = self.output_form_mut(tab) {
            form.set(flag, value);
        }
        cx.notify();
    }

    /// No budget changed.
    pub fn set_no_budget(&mut self, tab: FormTab, value: bool, cx: &mut Context<Self>) {
        match tab {
            FormTab::Within => self.within.form.no_budget = value,
            other => {
                if let Some(form) = self.output_form_mut(other) {
                    form.no_budget = value;
                }
            }
        }
        cx.notify();
    }

    /// How a visibility toggle shows: `(checked, disabled, tooltip)`. A flag
    /// `[agent]` forces on renders on and disabled (D-036).
    pub fn flag_state(&self, tab: FormTab, flag: Flag, cx: &App) -> (bool, bool, String) {
        let config: ConfigFile = app_state(cx).config.effective.clone();
        let form = match tab {
            FormTab::Outline => Some(&self.outline.form),
            FormTab::Read => Some(&self.read.form.output),
            FormTab::Within => None,
        };
        let forced = flag.forced_by_config(&config);
        (
            forced || form.is_some_and(|form| form.get(flag)),
            forced,
            if forced {
                flag.forced_tooltip()
            } else {
                flag.help().to_string()
            },
        )
    }

    /// The visibility toggles; `[agent]` flags render on and disabled.
    pub(super) fn render_flags(
        &mut self,
        tab: FormTab,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let view = cx.entity();
        let mut row = h_flex().gap_2().flex_wrap();
        for flag in Flag::ALL {
            let (checked, forced, tooltip) = self.flag_state(tab, flag, cx);
            let view = view.clone();
            row = row.child(checkbox(
                SharedString::from(format!("agent-flag-{tab:?}-{}", flag.config_key())),
                flag.label(),
                checked,
                forced,
                Some(tooltip.into()),
                Rc::new(move |checked, _, cx| {
                    view.update(cx, |this, cx| this.set_flag(tab, flag, checked, cx));
                }),
            ));
        }
        row
    }

    /// `Budget [6000] ☐ No budget`.
    pub(super) fn render_budget(
        &mut self,
        input: &Entity<gpui_kit::component::input::InputState>,
        no_budget: bool,
        tab: FormTab,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let view = cx.entity();
        let muted = cx.theme().muted_foreground;
        h_flex()
            .id(SharedString::from(format!("agent-budget-{tab:?}")))
            .gap_2()
            .items_center()
            .tooltip(|window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(super::form::help::BUDGET)
                    .build(window, cx)
            })
            .child(div().text_xs().text_color(muted).child("Budget"))
            .child(
                div().w(rems(8.0)).child(
                    gpui_kit::component::input::Input::new(input)
                        .small()
                        .aria_label("Budget"),
                ),
            )
            .child(checkbox(
                SharedString::from(format!("agent-no-budget-{tab:?}")),
                "No budget",
                no_budget,
                false,
                Some(super::form::help::NO_BUDGET.into()),
                Rc::new(move |checked, _, cx| {
                    view.update(cx, |this, cx| this.set_no_budget(tab, checked, cx));
                }),
            ))
    }

    // ----- actions --------------------------------------------------------------------------

    fn on_run(&mut self, _: &RunDeepSearch, _: &mut Window, cx: &mut Context<Self>) {
        self.run_current(cx);
    }

    fn on_cancel(&mut self, _: &CancelDeepSearch, _: &mut Window, cx: &mut Context<Self>) {
        let tab = self.tab;
        self.cancel_tab(tab, cx);
    }

    fn on_toggle_text(&mut self, _: &ToggleAgentText, window: &mut Window, cx: &mut Context<Self>) {
        let results = self.results_of(self.tab).clone();
        results.update(cx, |results, cx| results.toggle_text(window, cx));
    }

    fn on_escape(&mut self, _: &AgentPanelEscape, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.tab;
        if self.results_of(tab).read(cx).is_running() {
            self.cancel_tab(tab, cx);
            return;
        }
        let input = match tab {
            AgentTab::Within => Some(self.within.query.clone()),
            AgentTab::Read => Some(self.read.refs.clone()),
            AgentTab::Outline => None,
        };
        if let Some(input) = input
            && !input.read(cx).value().is_empty()
        {
            input.update(cx, |input, cx| input.set_value("", window, cx));
            return;
        }
        // Nothing to clear or cancel: the viewer focuses the transcript.
        cx.propagate();
    }

    // ----- probes ---------------------------------------------------------------------------

    /// QA probes (§15.3, keys `agent.*`; the main window reaches them as
    /// `deep.agent.*`, see `DeepSearchPane::qa_probe`).
    #[cfg(any(test, feature = "qa"))]
    pub fn qa_probe(&self, key: &str, cx: &App) -> Option<String> {
        let run = self.current_run();
        let results = run.results.read(cx);
        Some(match key {
            "agent.tab" => format!("{:?}", self.tab).to_lowercase(),
            "agent.visible" => self.visible.to_string(),
            "agent.target" => self
                .viewer_target
                .as_ref()
                .map(|target| target.title.to_string())
                .unwrap_or_default(),
            "agent.ref" => self.viewer_full_ref().unwrap_or_default().to_string(),
            "agent.key" => match self.key {
                KeyState::Unknown => "unknown",
                KeyState::Pending(_) => "pending",
                KeyState::Known { .. } => "known",
                KeyState::Unavailable(_) => "unavailable",
                KeyState::Failed(..) => "failed",
            }
            .to_string(),
            "agent.set" => run
                .set
                .as_ref()
                .map(|set| set.target.title.to_string())
                .unwrap_or_default(),
            "agent.stale" => run
                .set
                .as_ref()
                .is_some_and(|set| self.is_stale(set))
                .to_string(),
            "agent.within_top_placeholder" => default_placeholder(self.applied.within_top),
            "agent.rows" | "agent.hits" => results.selectable_count().to_string(),
            "agent.status" => results.status_text(),
            "agent.error" => results
                .error()
                .map(|error| error.text.clone())
                .or_else(|| run.error.clone())
                .unwrap_or_default(),
            "agent.text" => results.text_view().text().to_string(),
            "agent.text_shown" => results.text_view().is_shown().to_string(),
            "agent.selected" => results
                .selected()
                .map(|ix| ix.to_string())
                .unwrap_or_else(|| "none".to_string()),
            "agent.summary" => match results.body() {
                crate::deep_search::results::Body::Outline(model) => model.summary(),
                crate::deep_search::results::Body::Search(model) => model.stats_line(),
                _ => String::new(),
            },
            _ => return None,
        })
    }
}

/// The panel's width while it is open (rem): the remembered width within
/// 18-28rem. The main window lays the left pane out around it (PD1, X-040).
pub fn panel_width_rem(cx: &App) -> f32 {
    app_state(cx).ui.split.agent_panel_width_rem.clamp(
        crate::persist::AGENT_WIDTH_MIN_REM,
        crate::persist::AGENT_WIDTH_MAX_REM,
    )
}

/// Drag payload of the panel's resize handle (C-038, DESIGN section 2: 18-28rem).
#[derive(Clone, Copy, Debug)]
struct PanelDrag;

impl Render for PanelDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

fn tab_of(kind: &AgentJobKind) -> Option<AgentTab> {
    match kind {
        AgentJobKind::Outline => Some(AgentTab::Outline),
        AgentJobKind::Within => Some(AgentTab::Within),
        AgentJobKind::Read => Some(AgentTab::Read),
        AgentJobKind::Search | AgentJobKind::KeyForPath(_) => None,
    }
}

impl Render for AgentPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let border = theme.border;
        let danger_color = theme.danger;
        let accent = theme.accent;
        let selected = match self.tab {
            AgentTab::Outline => 0,
            AgentTab::Within => 1,
            AgentTab::Read => 2,
        };
        let width = panel_width_rem(cx);
        let unavailable = matches!(self.key, KeyState::Unavailable(_));

        let tabs = TabBar::new("agent-tabs")
            .underline()
            .selected_index(selected)
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                let tab = match ix {
                    0 => AgentTab::Outline,
                    1 => AgentTab::Within,
                    _ => AgentTab::Read,
                };
                this.select_tab(tab, cx);
            }))
            .child("Outline")
            .child("Within")
            .child("Read")
            .suffix(
                Button::new("agent-close")
                    .icon(IconName::Close)
                    .ghost()
                    .xsmall()
                    .accessibility_label("Close the agent panel")
                    .tooltip("Close")
                    .on_click({
                        let view = view.clone();
                        move |_, _, cx| {
                            view.update(cx, |this, cx| this.hide(cx));
                        }
                    }),
            );

        let body = if let KeyState::Failed(_, error) = &self.key {
            div()
                .id("agent-key-failed")
                .p_3()
                .text_sm()
                .text_color(danger_color)
                .child(format!(
                    "Could not look up this conversation's ref: {error}"
                ))
                .into_any_element()
        } else if unavailable {
            div()
                .id("agent-out-of-root")
                .p_3()
                .text_sm()
                .text_color(muted)
                .child(OUT_OF_ROOT_TEXT)
                .into_any_element()
        } else if self.viewer_target.is_none() {
            div()
                .id("agent-no-conversation")
                .p_3()
                .text_sm()
                .text_color(muted)
                .child("Open a conversation to outline, search within or read it.")
                .into_any_element()
        } else {
            self.render_body(cx).into_any_element()
        };

        v_flex()
            .id("agent-panel")
            .key_context(CTX_AGENT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_run))
            .on_action(cx.listener(Self::on_cancel))
            .on_action(cx.listener(Self::on_toggle_text))
            .on_action(cx.listener(Self::on_escape))
            .on_key_down(cx.listener(Self::on_key_down))
            .relative()
            .w(rems(width))
            .flex_none()
            .h_full()
            .min_h_0()
            .border_l_1()
            .border_color(border)
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<PanelDrag>, window, cx| {
                    this.on_resize_drag(event, window, cx)
                }),
            )
            .child(
                div()
                    .id("agent-panel-divider")
                    .absolute()
                    .top_0()
                    .left(px(-3.0))
                    .h_full()
                    .w(px(6.0))
                    .cursor(CursorStyle::ResizeLeftRight)
                    .hover(move |style| style.bg(accent.opacity(0.4)))
                    .on_drag(PanelDrag, |drag, _, _, cx| cx.new(|_| *drag)),
            )
            .child(tabs)
            .child(body)
    }
}

impl AgentPanel {
    /// The resize handle was dragged: the panel is docked at the window's
    /// right edge, so its width is the distance from the pointer to it.
    fn on_resize_drag(
        &mut self,
        event: &DragMoveEvent<PanelDrag>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rem = f32::from(window.rem_size());
        if rem <= 0.0 {
            return;
        }
        let width =
            (f32::from(window.viewport_size().width) - f32::from(event.event.position.x)) / rem;
        self.set_width_rem(width, cx);
    }

    /// Set the panel width (rem), clamped to 18-28rem and remembered (C-038).
    pub fn set_width_rem(&mut self, rem: f32, cx: &mut Context<Self>) {
        let before = panel_width_rem(cx);
        crate::persist::remember_agent_panel(self.visible, self.tab, Some(rem), cx);
        if (panel_width_rem(cx) - before).abs() > f32::EPSILON {
            cx.notify();
        }
    }

    /// Restore the remembered visibility and tab (startup of the main
    /// window): no focus change and no run; the key lookup follows the
    /// viewer's conversation.
    pub fn restore(&mut self, state: crate::persist::AgentPanelState, cx: &mut Context<Self>) {
        self.visible = state.visible;
        self.tab = state.tab;
        cx.notify();
    }

    fn render_body(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let (muted, border, foreground, danger) = {
            let theme = cx.theme();
            (
                theme.muted_foreground,
                theme.border,
                theme.foreground,
                theme.danger,
            )
        };
        let view = cx.entity();
        let tab = self.tab;

        // "For: <title> (ch_… ⧉)": the conversation of the shown result set.
        let (for_title, for_ref) = match self.set_of(tab) {
            Some(set) => (set.target.title.to_string(), Some(set.full_ref.clone())),
            None => (
                self.viewer_target
                    .as_ref()
                    .map(|target| target.title.to_string())
                    .unwrap_or_default(),
                self.viewer_full_ref().map(str::to_string),
            ),
        };
        let for_line = h_flex()
            .id("agent-for")
            .px_2()
            .py_1()
            .gap_1()
            .items_center()
            .text_xs()
            .text_color(muted)
            .child(div().child("For:"))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_medium()
                    .text_color(foreground)
                    .child(for_title),
            )
            .when_some(for_ref, |this, full_ref| {
                let short: String = full_ref.chars().take(15).collect();
                let copy = full_ref.clone();
                let results = self.results_of(tab).clone();
                this.child(div().flex_none().child(format!("({short}")))
                    .child(
                        Button::new("agent-copy-ref")
                            .icon(IconName::Copy)
                            .ghost()
                            .xsmall()
                            .accessibility_label("Copy ref")
                            .tooltip(format!("Copy ref ({full_ref})"))
                            .on_click(move |_, window, cx| {
                                let copy = copy.clone();
                                results
                                    .update(cx, |results, cx| results.copy_ref(copy, window, cx));
                            }),
                    )
                    .child(div().flex_none().child(")"))
            });

        let controls = match tab {
            AgentTab::Outline => self.render_outline_controls(cx).into_any_element(),
            AgentTab::Within => self.render_within_controls(cx).into_any_element(),
            AgentTab::Read => self.render_read_controls(cx).into_any_element(),
        };
        let run = self.current_run();
        let form_error = run.error.clone();
        let stale = run
            .set
            .as_ref()
            .filter(|_| tab != AgentTab::Outline)
            .filter(|set| self.is_stale(set))
            .map(|set| set.target.title.to_string());
        let results = run.results.clone();

        v_flex()
            .size_full()
            .min_h_0()
            .child(for_line)
            .child(
                v_flex()
                    .px_2()
                    .pb_1()
                    .gap_1()
                    .border_b_1()
                    .border_color(border)
                    .child(controls)
                    .when_some(form_error, |this, error| {
                        this.child(
                            div()
                                .id("agent-form-error")
                                .text_xs()
                                .text_color(danger)
                                .child(format!("⚠ {error}")),
                        )
                    }),
            )
            .when_some(stale, |this, title| {
                let view = view.clone();
                this.child(
                    h_flex()
                        .id("agent-stale")
                        .px_2()
                        .py_1()
                        .gap_2()
                        .items_center()
                        .border_b_1()
                        .border_color(border)
                        .text_xs()
                        .text_color(muted)
                        .child(Icon::new(IconName::Info).xsmall())
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(format!("Results are for {title}.")),
                        )
                        .child(
                            Button::new("agent-run-for-viewer")
                                .label("Run for this conversation")
                                .xsmall()
                                .outline()
                                .on_click(move |_, window, cx| {
                                    view.update(cx, |this, cx| this.run_for_viewer(window, cx));
                                }),
                        ),
                )
            })
            .child(div().flex_1().min_h_0().child(results))
    }
}
