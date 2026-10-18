//! Deep search and agent panel tests (P and G, DESIGN §17.8). WP4.
//!
//! * `models` — P: the view models built from real core responses;
//! * `worker_t` — the agent worker (slots, cancellation, the semantic gate);
//! * `search_g` — G: the Deep search tab in the main window;
//! * `panel_g` — G: the agent panel (Outline / Within / Read).

mod models;
mod panel_g;
mod review_g;
mod search_g;
mod worker_t;

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use claude_history_core::agent::AgentResponse;
use gpui_kit::AppContext as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, Context, Entity, TestAppContext, Window};

use crate::deep_search::panel::{AgentPanel, AgentPanelEvent};
use crate::deep_search::results::ResultsView;
use crate::deep_search::{DeepSearchEvent, DeepSearchPane};
use crate::list::ListPaneEvent;
use crate::model::{AgentTab, ConversationTarget};
use crate::test_support::{TestWorld, open_main};
use crate::workspace::Workspace;

/// Real-time deadline of every wait.
pub const WAIT: Duration = Duration::from_secs(30);

/// A main window with the Deep search pane.
pub struct DeepFixture {
    pub handle: AnyWindowHandle,
    pub ws: Entity<Workspace>,
    pub deep: Entity<DeepSearchPane>,
    pub results: Entity<ResultsView>,
}

impl DeepFixture {
    pub fn open(cx: &mut TestAppContext, world: &TestWorld, args: &[&str]) -> Self {
        let (handle, ws) = open_main(cx, world, args);
        let deep = cx.read(|cx| ws.read(cx).deep().clone());
        let results = cx.read(|cx| deep.read(cx).results().clone());
        Self {
            handle,
            ws,
            deep,
            results,
        }
    }

    /// Run `f` on the pane in its window.
    pub fn with<R>(
        &self,
        cx: &mut TestAppContext,
        f: impl FnOnce(&mut DeepSearchPane, &mut Window, &mut Context<DeepSearchPane>) -> R,
    ) -> R {
        let deep = self.deep.clone();
        cx.update_window(self.handle, |_, window, cx| {
            deep.update(cx, |pane, cx| f(pane, window, cx))
        })
        .expect("the window is open")
    }

    /// Start a search without waiting.
    pub fn start(&self, cx: &mut TestAppContext, query: &str) {
        self.with(cx, |pane, window, cx| {
            pane.set_query(query, window, cx);
            pane.run(cx);
        });
    }

    /// Search and wait for the answer.
    pub fn search(&self, cx: &mut TestAppContext, query: &str) {
        self.start(cx, query);
        self.wait_idle(cx);
    }

    /// Wait (real time) until the search is no longer running.
    pub fn wait_idle(&self, cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let deadline = Instant::now() + WAIT;
        loop {
            cx.run_until_parked();
            if !cx.read(|cx| self.deep.read(cx).is_running(cx)) {
                return;
            }
            assert!(Instant::now() < deadline, "the search did not finish");
            std::thread::sleep(Duration::from_millis(3));
        }
    }

    /// Wait (real time) until the main window's conversation list has loaded
    /// (tests that compare against the list's rows).
    pub fn wait_list_loaded(&self, cx: &mut TestAppContext) {
        let list = cx.read(|cx| {
            crate::globals::main_panes(cx)
                .list()
                .expect("the list pane")
        });
        cx.executor().allow_parking();
        let deadline = Instant::now() + WAIT;
        loop {
            cx.run_until_parked();
            let loading = cx.read(|cx| {
                let list = list.read(cx);
                list.is_loading() || list.loader().is_running()
            });
            if !loading {
                return;
            }
            assert!(Instant::now() < deadline, "the list did not load");
            std::thread::sleep(Duration::from_millis(3));
        }
    }

    pub fn probe(&self, cx: &TestAppContext, key: &str) -> String {
        cx.read(|cx| self.deep.read(cx).qa_probe(key, cx))
            .unwrap_or_else(|| panic!("probe {key} is answered"))
    }

    /// The latest response.
    pub fn response(&self, cx: &TestAppContext) -> AgentResponse {
        cx.read(|cx| {
            self.results
                .read(cx)
                .response()
                .cloned()
                .expect("a response arrived")
        })
    }

    /// Record the events of the pane.
    pub fn record(&self, cx: &mut TestAppContext) -> Rc<RefCell<Vec<DeepSearchEvent>>> {
        let events = Rc::new(RefCell::new(Vec::new()));
        let sink = events.clone();
        let deep = self.deep.clone();
        cx.update(|cx| {
            cx.subscribe(&deep, move |_, event: &DeepSearchEvent, _| {
                sink.borrow_mut().push(event.clone());
            })
            .detach();
        });
        events
    }

    /// Render a frame (focus and layout settle).
    pub fn render(&self, cx: &mut TestAppContext) {
        render(cx, self.handle);
    }
}

/// The `summary` line of a transcript: the title the list would show.
fn summary_title(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines().find_map(|line| {
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        (value["type"] == "summary")
            .then(|| value["summary"].as_str().map(str::to_string))
            .flatten()
    })
}

pub fn render(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("the window is open");
    cx.run_until_parked();
}

/// A main window whose viewer shows a conversation, with its agent panel.
pub struct PanelFixture {
    pub handle: AnyWindowHandle,
    pub ws: Entity<Workspace>,
    pub panel: Entity<AgentPanel>,
}

impl PanelFixture {
    /// Open the main window and the conversation at `path` with the panel on
    /// `tab` (an open request of the list, as Enter / the row menu does).
    pub fn open(
        cx: &mut TestAppContext,
        world: &TestWorld,
        args: &[&str],
        path: &Path,
        tab: AgentTab,
    ) -> Self {
        let (handle, ws) = open_main(cx, world, args);
        let fixture = Self {
            handle,
            ws: ws.clone(),
            panel: cx.read(|cx| ws.read(cx).viewer().read(cx).agent_panel().clone()),
        };
        fixture.open_conversation(cx, path, Some(tab));
        fixture
    }

    /// Show `path` in the viewer (optionally with the panel on `tab`).
    pub fn open_conversation(&self, cx: &mut TestAppContext, path: &Path, tab: Option<AgentTab>) {
        let list = cx.read(|cx| self.ws.read(cx).list().clone());
        let mut target = ConversationTarget::from_parsed_file(path, None);
        if let Some(title) = summary_title(path) {
            target.title = title.into();
        }
        list.update(cx, |_, cx| {
            cx.emit(ListPaneEvent::OpenRequested {
                target,
                focus_viewer: tab.is_none(),
                agent_tab: tab,
            })
        });
        cx.run_until_parked();
        self.wait_idle(cx);
    }

    /// Run `f` on the panel in its window.
    pub fn with<R>(
        &self,
        cx: &mut TestAppContext,
        f: impl FnOnce(&mut AgentPanel, &mut Window, &mut Context<AgentPanel>) -> R,
    ) -> R {
        let panel = self.panel.clone();
        cx.update_window(self.handle, |_, window, cx| {
            panel.update(cx, |panel, cx| f(panel, window, cx))
        })
        .expect("the window is open")
    }

    /// Wait until no key lookup and no job of the panel is pending.
    pub fn wait_idle(&self, cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let deadline = Instant::now() + WAIT;
        loop {
            cx.run_until_parked();
            let busy = cx.read(|cx| {
                let panel = self.panel.read(cx);
                matches!(
                    panel.key_state(),
                    crate::deep_search::panel::KeyState::Pending(_)
                ) || [AgentTab::Outline, AgentTab::Within, AgentTab::Read]
                    .into_iter()
                    .any(|tab| panel.results_of(tab).read(cx).is_running())
            });
            if !busy {
                return;
            }
            assert!(Instant::now() < deadline, "the agent panel stayed busy");
            std::thread::sleep(Duration::from_millis(3));
        }
    }

    pub fn probe(&self, cx: &TestAppContext, key: &str) -> String {
        cx.read(|cx| self.panel.read(cx).qa_probe(key, cx))
            .unwrap_or_else(|| panic!("probe {key} is answered"))
    }

    /// The results entity of `tab`.
    pub fn results(&self, cx: &TestAppContext, tab: AgentTab) -> Entity<ResultsView> {
        cx.read(|cx| self.panel.read(cx).results_of(tab).clone())
    }

    /// Record the panel's events.
    pub fn record(&self, cx: &mut TestAppContext) -> Rc<RefCell<Vec<AgentPanelEvent>>> {
        let events = Rc::new(RefCell::new(Vec::new()));
        let sink = events.clone();
        let panel = self.panel.clone();
        cx.update(|cx| {
            cx.subscribe(&panel, move |_, event: &AgentPanelEvent, _| {
                sink.borrow_mut().push(event.clone());
            })
            .detach();
        });
        events
    }

    pub fn render(&self, cx: &mut TestAppContext) {
        render(cx, self.handle);
    }
}
