//! One embedding model per process, shared by the list's semantic worker,
//! the agent worker and the Tools (DESIGN §7.6). WP0 → WP7.
//!
//! [`SharedEmbedder`] holds the model behind a mutex and loads it lazily on
//! the first thread that needs it, through its [`ModelFactory`]. It hands out
//! adapters for the three core factory shapes:
//! [`browser_factory`] (`browser::SemanticEmbedderFactory`),
//! [`agent_factory`] (`agent::service::EmbedderFactory`) and
//! [`ops_factory`] (an owned `FnMut` for `semantic_ops::EmbedderFactory<'_>`).
//! Each adapter locks the shared model per call, so one model is in memory
//! and Clear semantic cache can drop it ([`drop_model`]).
//!
//! [`install`] sets up fastembed
//! (`FastembedEmbedder::new_in(env.semantic_model_dir(), false)`, or
//! `--qa-model-dir`); the model is loaded lazily, off the UI thread, by the
//! first consumer. Tests get a refusing factory from `init_for_test` and opt
//! in to a fake with [`install_fake`].

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use claude_history_core::AppError;
use claude_history_core::agent::service::EmbedderFactory as AgentEmbedderFactory;
use claude_history_core::browser::SemanticEmbedderFactory;
use claude_history_core::semantic::embed::SemanticEmbedder;
use claude_history_core::semantic::fastembed::FastembedEmbedder;
use gpui_kit::{App, Global};

use crate::globals::AppState;

/// Creates the (one) model; called lazily on a worker thread.
pub type ModelFactory =
    Arc<dyn Fn() -> claude_history_core::Result<Box<dyn SemanticEmbedder + Send>> + Send + Sync>;

/// An owned factory for `semantic_ops::EmbedderFactory<'_>` (pass `&mut *factory`).
pub type OpsEmbedderFactory =
    Box<dyn FnMut() -> claude_history_core::Result<Box<dyn SemanticEmbedder>> + Send>;

/// The message of the refusing factory used by tests.
pub const NOT_AVAILABLE_IN_TESTS: &str = "semantic model is not available in tests";

/// The process's embedding model, loaded on first use.
pub struct SharedEmbedder {
    factory: Mutex<ModelFactory>,
    model: Mutex<Option<Box<dyn SemanticEmbedder + Send>>>,
    loads: AtomicUsize,
    /// Whether `model` holds a model (readable without its lock, which a
    /// loading or embedding worker holds for a long time).
    loaded: AtomicBool,
    /// A drop was requested while a worker held the model: it drops the
    /// model itself as soon as it is done (the UI thread never waits).
    drop_requested: AtomicBool,
}

impl SharedEmbedder {
    pub fn new(factory: ModelFactory) -> Self {
        Self {
            factory: Mutex::new(factory),
            model: Mutex::new(None),
            loads: AtomicUsize::new(0),
            loaded: AtomicBool::new(false),
            drop_requested: AtomicBool::new(false),
        }
    }

    /// Replace the factory (and drop a loaded model).
    pub fn set_factory(&self, factory: ModelFactory) {
        *self.factory.lock().unwrap_or_else(|e| e.into_inner()) = factory;
        self.drop_model();
    }

    /// Drop the loaded model (files closed); the next use loads it again.
    /// Never blocks: when a worker is loading or embedding, the worker drops
    /// the model right after its current call.
    pub fn drop_model(&self) {
        self.drop_requested.store(true, Ordering::SeqCst);
        if let Ok(mut model) = self.model.try_lock() {
            self.apply_drop(&mut model);
        }
    }

    fn apply_drop(&self, model: &mut Option<Box<dyn SemanticEmbedder + Send>>) {
        if self.drop_requested.swap(false, Ordering::SeqCst) {
            *model = None;
            self.loaded.store(false, Ordering::SeqCst);
        }
    }

    /// Whether a model is loaded right now.
    pub fn is_loaded(&self) -> bool {
        self.loaded.load(Ordering::SeqCst) && !self.drop_requested.load(Ordering::SeqCst)
    }

    /// How many times a model was created (tests: one per process).
    pub fn load_count(&self) -> usize {
        self.loads.load(Ordering::SeqCst)
    }

    /// Run `f` with the model, loading it first when needed.
    fn with_model<R>(
        &self,
        f: impl FnOnce(&mut dyn SemanticEmbedder) -> claude_history_core::Result<R>,
    ) -> claude_history_core::Result<R> {
        let mut model = self.model.lock().unwrap_or_else(|e| e.into_inner());
        self.apply_drop(&mut model);
        if model.is_none() {
            let factory = self
                .factory
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            *model = Some(factory()?);
            self.loads.fetch_add(1, Ordering::SeqCst);
            self.loaded.store(true, Ordering::SeqCst);
        }
        let embedder = model.as_mut().expect("model was just loaded");
        let result = f(embedder.as_mut());
        // A drop requested meanwhile (Clear semantic cache) happens now.
        self.apply_drop(&mut model);
        result
    }
}

/// A thin embedder that locks the shared model per call.
struct SharedModelEmbedder {
    shared: Arc<SharedEmbedder>,
}

impl SemanticEmbedder for SharedModelEmbedder {
    fn embed_passages(
        &mut self,
        passages: &[String],
    ) -> claude_history_core::Result<Vec<Vec<f32>>> {
        self.shared
            .with_model(|model| model.embed_passages(passages))
    }

    fn embed_query(&mut self, query: &str) -> claude_history_core::Result<Option<Vec<f32>>> {
        self.shared.with_model(|model| model.embed_query(query))
    }
}

/// The shared embedder of this process.
#[derive(Clone)]
pub struct SharedEmbedderHandle(pub Arc<SharedEmbedder>);

impl Global for SharedEmbedderHandle {}

/// A factory that always fails with `message`.
pub fn refusing_factory(message: &'static str) -> ModelFactory {
    Arc::new(move || Err(AppError::AgentProtocol(message.to_string())))
}

/// The production factory: the fastembed BGE-small model, downloaded on first
/// use into `model_dir` (`HistoryEnv::semantic_model_dir`, or the QA run's
/// pre-downloaded `--qa-model-dir`). Without a model directory (no home) it
/// fails with a readable message.
pub fn fastembed_factory(model_dir: Option<PathBuf>) -> ModelFactory {
    Arc::new(move || {
        let dir = model_dir.clone().ok_or_else(|| {
            AppError::ConfigError("No home directory: cannot place the semantic model".to_string())
        })?;
        let model = FastembedEmbedder::new_in(dir, false)?;
        Ok(Box::new(model) as Box<dyn SemanticEmbedder + Send>)
    })
}

/// Where the production model lives for this launch: `--qa-model-dir` when
/// given, else the environment's model directory.
pub fn model_dir_for(cx: &App) -> Option<PathBuf> {
    let state = cx.try_global::<AppState>()?;
    state
        .launch
        .qa
        .as_ref()
        .and_then(|qa| qa.model_dir.clone())
        .or_else(|| state.env.semantic_model_dir())
}

/// Install the production embedder (§7.6): one lazily loaded fastembed model
/// shared by every consumer. Idempotent; tests replace it with
/// [`install_fake`].
pub fn install(cx: &mut App) {
    if cx.try_global::<SharedEmbedderHandle>().is_none() {
        let factory = fastembed_factory(model_dir_for(cx));
        cx.set_global(SharedEmbedderHandle(Arc::new(SharedEmbedder::new(factory))));
    }
}

/// Tests/QA: use `factory` for the shared model from now on.
pub fn install_fake(cx: &mut App, factory: ModelFactory) {
    match cx.try_global::<SharedEmbedderHandle>() {
        Some(handle) => handle.0.set_factory(factory),
        None => cx.set_global(SharedEmbedderHandle(Arc::new(SharedEmbedder::new(factory)))),
    }
}

/// The shared embedder (installs the placeholder when missing).
pub fn shared(cx: &mut App) -> Arc<SharedEmbedder> {
    install(cx);
    cx.global::<SharedEmbedderHandle>().0.clone()
}

fn shared_ref(cx: &App) -> Arc<SharedEmbedder> {
    cx.global::<SharedEmbedderHandle>().0.clone()
}

/// `BrowserOptions::with_embedder_factory` adapter.
pub fn browser_factory(cx: &App) -> SemanticEmbedderFactory {
    let shared = shared_ref(cx);
    Arc::new(move || {
        Ok(Box::new(SharedModelEmbedder {
            shared: shared.clone(),
        }) as Box<dyn SemanticEmbedder>)
    })
}

/// `AgentService::with_embedder_factory` adapter.
pub fn agent_factory(cx: &App) -> AgentEmbedderFactory {
    let shared = shared_ref(cx);
    Box::new(move || {
        Ok(Box::new(SharedModelEmbedder {
            shared: shared.clone(),
        }) as Box<dyn SemanticEmbedder>)
    })
}

/// `semantic_ops` adapter (Tools: generate cache, reports).
pub fn ops_factory(cx: &App) -> OpsEmbedderFactory {
    let shared = shared_ref(cx);
    Box::new(move || {
        Ok(Box::new(SharedModelEmbedder {
            shared: shared.clone(),
        }) as Box<dyn SemanticEmbedder>)
    })
}

/// Drop the shared model (Clear semantic cache, under the semantic gate).
pub fn drop_model(cx: &mut App) {
    if let Some(handle) = cx.try_global::<SharedEmbedderHandle>() {
        handle.0.drop_model();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake model (instances are counted by the factory).
    struct Fake;

    impl SemanticEmbedder for Fake {
        fn embed_passages(
            &mut self,
            passages: &[String],
        ) -> claude_history_core::Result<Vec<Vec<f32>>> {
            Ok(passages.iter().map(|_| vec![1.0, 0.0]).collect())
        }

        fn embed_query(&mut self, _query: &str) -> claude_history_core::Result<Option<Vec<f32>>> {
            Ok(Some(vec![1.0, 0.0]))
        }
    }

    fn counting_factory(created: Arc<AtomicUsize>) -> ModelFactory {
        Arc::new(move || {
            created.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Fake) as Box<dyn SemanticEmbedder + Send>)
        })
    }

    fn thin(shared: &Arc<SharedEmbedder>) -> Box<dyn SemanticEmbedder> {
        Box::new(SharedModelEmbedder {
            shared: shared.clone(),
        })
    }

    /// §7.6 / B11: the browser, agent and Tools adapters share one model; it
    /// is created lazily, once, and again only after `drop_model`.
    #[test]
    fn one_model_instance_serves_all_three_adapters() {
        let created = Arc::new(AtomicUsize::new(0));
        let shared = Arc::new(SharedEmbedder::new(counting_factory(created.clone())));
        // The three adapter shapes, built the way `browser_factory`,
        // `agent_factory` and `ops_factory` build them.
        let browser: SemanticEmbedderFactory = {
            let shared = shared.clone();
            Arc::new(move || Ok(thin(&shared)))
        };
        let agent: AgentEmbedderFactory = {
            let shared = shared.clone();
            Box::new(move || Ok(thin(&shared)))
        };
        let mut ops: OpsEmbedderFactory = {
            let shared = shared.clone();
            Box::new(move || Ok(thin(&shared)))
        };
        assert_eq!(created.load(Ordering::SeqCst), 0, "lazy: nothing is loaded");
        assert!(!shared.is_loaded());

        let mut b = browser().unwrap();
        let mut a = agent().unwrap();
        let mut o = ops().unwrap();
        assert_eq!(created.load(Ordering::SeqCst), 0, "adapters load nothing");
        assert_eq!(b.embed_query("q").unwrap(), Some(vec![1.0, 0.0]));
        assert_eq!(a.embed_passages(&["p".to_string()]).unwrap().len(), 1);
        assert!(o.embed_query("q").unwrap().is_some());
        assert_eq!(
            created.load(Ordering::SeqCst),
            1,
            "one model, three consumers"
        );
        assert_eq!(shared.load_count(), 1);
        assert!(shared.is_loaded());

        // Clear semantic cache drops the model; the next use loads it again.
        shared.drop_model();
        assert!(!shared.is_loaded());
        b.embed_query("q").unwrap();
        assert_eq!(created.load(Ordering::SeqCst), 2);
    }

    /// §17.6 through the production adapters: `install_fake`, then
    /// `browser_factory`/`agent_factory`/`ops_factory` built from an App
    /// share one model, and a later `install_fake` reaches them.
    #[gpui_kit::test]
    fn the_production_adapters_share_one_model(cx: &mut gpui_kit::TestAppContext) {
        let _lock = crate::test_support::global_lock();
        let world = crate::test_support::TestWorld::new();
        let (_handle, _ws) = crate::test_support::open_main(cx, &world, &[]);
        let created = Arc::new(AtomicUsize::new(0));
        let (browser, agent, mut ops, handle) = cx.update(|cx| {
            install_fake(cx, counting_factory(created.clone()));
            (
                browser_factory(cx),
                agent_factory(cx),
                ops_factory(cx),
                shared(cx),
            )
        });
        browser().unwrap().embed_query("q").unwrap();
        agent().unwrap().embed_query("q").unwrap();
        ops().unwrap().embed_query("q").unwrap();
        assert_eq!(
            created.load(Ordering::SeqCst),
            1,
            "one model, three adapters"
        );
        // An adapter built earlier sees the replacement factory.
        cx.update(|cx| install_fake(cx, counting_factory(created.clone())));
        assert!(!handle.is_loaded());
        browser().unwrap().embed_query("q").unwrap();
        assert_eq!(created.load(Ordering::SeqCst), 2);
        // Clear semantic cache through the App entry point.
        cx.update(drop_model);
        assert!(!handle.is_loaded());
        ops().unwrap().embed_query("q").unwrap();
        assert_eq!(created.load(Ordering::SeqCst), 3);
    }

    /// The UI thread never waits for a worker that holds the model: the drop
    /// is deferred to the worker.
    #[test]
    fn drop_model_does_not_wait_for_a_busy_worker() {
        let created = Arc::new(AtomicUsize::new(0));
        let shared = Arc::new(SharedEmbedder::new(counting_factory(created.clone())));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let worker = {
            let shared = shared.clone();
            std::thread::spawn(move || {
                shared
                    .with_model(|_| {
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok(())
                    })
                    .unwrap();
            })
        };
        entered_rx.recv().unwrap();
        shared.drop_model(); // would deadlock if it waited for the lock
        shared.set_factory(counting_factory(created.clone()));
        release_tx.send(()).unwrap();
        worker.join().unwrap();
        assert!(!shared.is_loaded(), "the worker dropped the model");
    }

    #[test]
    fn a_failing_factory_reports_its_error_to_every_consumer() {
        let shared = Arc::new(SharedEmbedder::new(refusing_factory(
            NOT_AVAILABLE_IN_TESTS,
        )));
        let error = thin(&shared).embed_query("q").unwrap_err().to_string();
        assert!(error.contains(NOT_AVAILABLE_IN_TESTS), "{error}");
        let error = thin(&shared).embed_passages(&[]).unwrap_err().to_string();
        assert!(error.contains(NOT_AVAILABLE_IN_TESTS), "{error}");
        assert_eq!(shared.load_count(), 0);
    }

    #[test]
    fn set_factory_replaces_the_model() {
        let created = Arc::new(AtomicUsize::new(0));
        let shared = Arc::new(SharedEmbedder::new(counting_factory(created.clone())));
        thin(&shared).embed_query("q").unwrap();
        shared.set_factory(counting_factory(created.clone()));
        assert!(!shared.is_loaded(), "a new factory drops the loaded model");
        thin(&shared).embed_query("q").unwrap();
        assert_eq!(created.load(Ordering::SeqCst), 2);
    }

    /// The production factory never loads or downloads a model in test
    /// builds: core's hermetic guard refuses (K13); without a model
    /// directory it explains itself.
    #[test]
    fn the_production_factory_is_refused_in_test_builds() {
        let dir = tempfile::tempdir().unwrap();
        let model_dir = dir.path().join("fastembed");
        let factory = fastembed_factory(Some(model_dir.clone()));
        let error = factory().err().expect("hermetic builds refuse").to_string();
        assert!(!error.is_empty());
        assert!(!model_dir.exists(), "nothing is created");
        let error = fastembed_factory(None)().err().unwrap().to_string();
        assert!(error.contains("No home directory"), "{error}");
    }
}
