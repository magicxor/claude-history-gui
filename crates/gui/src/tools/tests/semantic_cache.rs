//! G tests of Generate / Clear semantic cache (T-008, T-009, T-010, T-024).

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use claude_history_core::semantic::types::DEFAULT_EMBEDDING_BATCH_SIZE;
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext};

use crate::deep_search::worker::AgentWorkerHandle;
use crate::globals::{SemanticGate, SemanticGateState, last_toast};
use crate::model::SemanticOp;
use crate::test_support::fixtures_list::{
    FakeEmbedderControl, Gate, ListFixture, install_fake_embedder,
};
use crate::test_support::fixtures_tools::{settle, wait_for};
use crate::test_support::{TestWorld, synth};
use crate::tools::common::Scope;
use crate::tools::semantic_cache::{
    GenerateDialog, GenerateState, current_generate, open_clear_semantic_cache,
    open_generate_semantic_cache,
};

/// Conversations whose passages (two each) take three embedder calls: two
/// full batches of `DEFAULT_EMBEDDING_BATCH_SIZE` and half a batch.
const THREE_BATCHES: usize = DEFAULT_EMBEDDING_BATCH_SIZE + DEFAULT_EMBEDDING_BATCH_SIZE / 4;

/// Opens the gates when the test ends (a worker blocked on a gate would
/// otherwise outlive it).
struct OpenOnDrop(Vec<Arc<Gate>>);

impl Drop for OpenOnDrop {
    fn drop(&mut self) {
        for gate in &self.0 {
            gate.open();
        }
    }
}

fn setup(
    cx: &mut TestAppContext,
    args: &[&str],
    sessions: usize,
) -> (
    TestWorld,
    Arc<FakeEmbedderControl>,
    ListFixture,
    AnyWindowHandle,
) {
    let world = TestWorld::new();
    synth::corpus(&world, sessions);
    ListFixture::init_app(cx, &world, args);
    let control = install_fake_embedder(cx);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let handle = f.handle;
    (world, control, f, handle)
}

/// Put the list in semantic mode (its prewarm uses the first `embed_passages`
/// call), then close the batch gate: a Generate run now blocks inside its
/// first batch until the returned gate opens. The list stays in semantic mode.
fn prewarmed_batch_gate(
    cx: &mut TestAppContext,
    control: &Arc<FakeEmbedderControl>,
    f: &ListFixture,
) -> Arc<Gate> {
    f.press(cx, "ctrl-t");
    f.wait_idle(cx);
    let gate = Gate::closed();
    *control.batch_gate.lock().unwrap() = Some(gate.clone());
    gate
}

fn open_generate(cx: &mut TestAppContext, handle: AnyWindowHandle) -> Entity<GenerateDialog> {
    cx.update_window(handle, |_, window, cx| {
        open_generate_semantic_cache(false, window, cx)
    })
    .unwrap();
    settle(cx, handle);
    cx.read(current_generate)
        .expect("the Generate dialog is open")
}

fn state(cx: &TestAppContext, dialog: &Entity<GenerateDialog>) -> GenerateState {
    cx.read(|cx| dialog.read(cx).state().clone())
}

/// T-008: the dialog opens idle (never auto-started); a run shows progress
/// and ends with core's report text.
#[gpui_kit::test]
fn generate_runs_and_reports(cx: &mut TestAppContext) {
    let (_world, control, _f, handle) = setup(cx, &[], 6);
    let dialog = open_generate(cx, handle);
    assert_eq!(state(cx, &dialog), GenerateState::Idle);
    assert_eq!(control.loads.load(Ordering::SeqCst), 0, "not auto-started");
    dialog.update(cx, |d, cx| d.start(cx));
    wait_for(cx, "the run to end", |cx| {
        matches!(
            dialog.read(cx).state(),
            GenerateState::Done(_) | GenerateState::Failed(_)
        )
    });
    let GenerateState::Done(text) = state(cx, &dialog) else {
        panic!("generate failed: {:?}", state(cx, &dialog));
    };
    assert!(text.contains("Semantic cache: "), "{text}");
    let progress = cx
        .read(|cx| dialog.read(cx).progress_text())
        .expect("progress");
    assert!(progress.starts_with("Semantic cache: "), "{progress}");
    assert!(cx.read(SemanticGate::is_open), "the gate is left");
}

/// Parse `Semantic cache: embedded x/y changed chunk(s)` into `(x, y)`.
fn embedded_counts(text: &str) -> Option<(usize, usize)> {
    let rest = text.strip_prefix("Semantic cache: embedded ")?;
    let (done, total) = rest.strip_suffix(" changed chunk(s)")?.split_once('/')?;
    Some((done.parse().ok()?, total.parse().ok()?))
}

/// T-008: progress reaches the dialog while the run is going: the
/// `embedded x/y changed chunk(s)` line drives the bar's percent, and the last
/// event is `y/y`.
#[gpui_kit::test]
fn progress_events_reach_the_dialog_while_running(cx: &mut TestAppContext) {
    // Three batches: the first batch is embedded, the second waits at the
    // gate.
    let (_world, control, _f, handle) = setup(cx, &[], THREE_BATCHES);
    let gate = Gate::closed();
    *control.batch_gate.lock().unwrap() = Some(gate.clone());
    let _open = OpenOnDrop(vec![gate.clone()]);
    let dialog = open_generate(cx, handle);
    dialog.update(cx, |d, cx| d.start(cx));
    wait_for(cx, "the first batch", |cx| {
        dialog
            .read(cx)
            .progress_text()
            .is_some_and(|text| embedded_counts(&text).is_some())
    });
    assert_eq!(state(cx, &dialog), GenerateState::Running);
    let (text, percent) = cx.read(|cx| {
        let d = dialog.read(cx);
        (d.progress_text().unwrap(), d.percent())
    });
    let (done, total) = embedded_counts(&text).unwrap();
    assert!(done > 0 && done < total, "{text}");
    let expected = done as f32 * 100.0 / total as f32;
    assert!((percent - expected).abs() < 0.01, "{percent} vs {expected}");
    gate.open();
    wait_for(cx, "the run to end", |cx| {
        matches!(dialog.read(cx).state(), GenerateState::Done(_))
    });
    let last = cx.read(|cx| dialog.read(cx).progress_text()).unwrap();
    let (done, total) = embedded_counts(&last).expect("the last event is an Embedded one");
    assert_eq!(done, total, "{last}");
}

/// T-008: choosing Generate in a second window while a run is going does not
/// cancel it; the running dialog stays where it is and the user is told.
#[gpui_kit::test]
fn a_second_window_does_not_cancel_a_running_generate(cx: &mut TestAppContext) {
    let (world, control, f, handle) = setup(cx, &[], 40);
    let gate = Gate::closed();
    *control.batch_gate.lock().unwrap() = Some(gate.clone());
    let _open = OpenOnDrop(vec![gate.clone()]);
    let dialog = open_generate(cx, handle);
    dialog.update(cx, |d, cx| d.start(cx));
    wait_for(cx, "the first batch", |cx| {
        dialog
            .read(cx)
            .progress_text()
            .is_some_and(|text| embedded_counts(&text).is_some())
    });
    let path = f.paths(cx).remove(0);
    let other = crate::test_support::open_single_file(cx, &world, &path);
    cx.update_window(other, |_, window, cx| {
        open_generate_semantic_cache(false, window, cx)
    })
    .unwrap();
    settle(cx, other);
    assert_eq!(state(cx, &dialog), GenerateState::Running, "not cancelled");
    assert_eq!(
        cx.read(crate::tools::common::current_window::<GenerateDialog>),
        Some(handle),
        "the running dialog stays in its window"
    );
    assert_eq!(
        cx.read(|cx| current_generate(cx).map(|d| d.entity_id())),
        Some(dialog.entity_id())
    );
    assert_eq!(
        cx.read(last_toast).as_ref(),
        crate::tools::semantic_cache::RUNNING_ELSEWHERE
    );
    assert!(!cx.read(SemanticGate::is_open), "still holding the gate");
    gate.open();
    wait_for(cx, "the run to end", |cx| {
        matches!(dialog.read(cx).state(), GenerateState::Done(_))
    });
}

/// T-008: Cancel stops the run (`Semantic search cancelled`).
#[gpui_kit::test]
fn cancel_stops_a_run(cx: &mut TestAppContext) {
    // Three batches: the second batch waits at the gate, the cancel is seen
    // before the third.
    let (_world, control, _f, handle) = setup(cx, &[], THREE_BATCHES);
    let init = Gate::closed();
    *control.batch_gate.lock().unwrap() = Some(init.clone());
    let _open = OpenOnDrop(vec![init.clone()]);
    let dialog = open_generate(cx, handle);
    dialog.update(cx, |d, cx| d.start(cx));
    assert_eq!(state(cx, &dialog), GenerateState::Running);
    cx.update_window(handle, |_, window, cx| {
        dialog.update(cx, |d, cx| d.cancel_or_close(window, cx))
    })
    .unwrap();
    init.open();
    wait_for(cx, "the cancelled run", |cx| {
        matches!(
            dialog.read(cx).state(),
            GenerateState::Failed(_) | GenerateState::Done(_)
        )
    });
    assert_eq!(
        state(cx, &dialog),
        GenerateState::Failed("Semantic search cancelled".to_string())
    );
    assert!(cx.read(SemanticGate::is_open));
}

/// T-024: the gate pauses the list worker and agent semantic jobs while
/// Generate runs, refuses a second operation, and releases afterwards.
#[gpui_kit::test]
fn generate_holds_the_semantic_gate(cx: &mut TestAppContext) {
    let (_world, control, f, handle) = setup(cx, &[], 6);
    let init = prewarmed_batch_gate(cx, &control, &f);
    let _open = OpenOnDrop(vec![init.clone()]);
    let dialog = open_generate(cx, handle);
    dialog.update(cx, |d, cx| d.start(cx));
    cx.run_until_parked();
    assert_eq!(
        cx.read(SemanticGate::current),
        SemanticGateState::Exclusive(SemanticOp::Generate)
    );
    assert!(cx.update(|cx| AgentWorkerHandle::global(cx).is_semantic_paused()));
    let status = f.read(cx, |list, _| list.search_status().text);
    assert!(status.contains("sem paused"), "{status}");
    assert!(
        cx.update(|cx| SemanticGate::enter_exclusive(SemanticOp::Clear, cx))
            .is_none()
    );
    assert!(
        cx.update(|cx| SemanticGate::enter_exclusive(SemanticOp::Generate, cx))
            .is_none()
    );
    // A second Start is a no-op while running.
    dialog.update(cx, |d, cx| d.start(cx));
    assert_eq!(state(cx, &dialog), GenerateState::Running);
    init.open();
    wait_for(cx, "the run to end", |cx| {
        matches!(
            dialog.read(cx).state(),
            GenerateState::Done(_) | GenerateState::Failed(_)
        )
    });
    cx.run_until_parked();
    assert!(cx.read(SemanticGate::is_open));
    assert!(!cx.update(|cx| AgentWorkerHandle::global(cx).is_semantic_paused()));
    f.wait_idle(cx);
    let status = f.read(cx, |list, _| list.search_status().text);
    assert!(!status.contains("sem paused"), "{status}");
}

/// Put the list in semantic mode with the batch gate closed: its background
/// indexing embeds its first slice (one batch) and then blocks inside its
/// second embedder call until the returned gate opens.
fn list_worker_in_a_batch(
    cx: &mut TestAppContext,
    control: &Arc<FakeEmbedderControl>,
    f: &ListFixture,
) -> Arc<Gate> {
    let gate = Gate::closed();
    *control.batch_gate.lock().unwrap() = Some(gate.clone());
    f.press(cx, "ctrl-t");
    wait_for(cx, "the list's indexing to block in a batch", |_| {
        control.passage_calls.load(Ordering::SeqCst) >= 2
    });
    gate
}

/// T-024, K6: taking the gate stops the list's semantic worker without
/// waiting for it on the UI thread, even while the worker is inside an
/// embedder call; the entry is ready once that call returned and the worker
/// ended, without embedding anything more.
#[gpui_kit::test]
fn the_gate_does_not_wait_for_a_busy_list_worker(cx: &mut TestAppContext) {
    let (_world, control, f, _handle) = setup(cx, &[], THREE_BATCHES);
    let batch = list_worker_in_a_batch(cx, &control, &f);
    let _open = OpenOnDrop(vec![batch.clone()]);
    let entry = cx
        .update(|cx| SemanticGate::enter_exclusive(SemanticOp::Generate, cx))
        .expect("the gate was open");
    // Back on the UI thread while the worker is still in its call.
    assert!(!entry.is_ready(), "the worker is still embedding");
    let status = f.read(cx, |list, _| list.search_status().text);
    assert!(status.contains("sem paused"), "{status}");
    batch.open();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !entry.is_ready() {
        assert!(Instant::now() < deadline, "the list's worker did not end");
        std::thread::sleep(Duration::from_millis(2));
    }
    entry.wait();
    assert_eq!(
        control.passage_calls.load(Ordering::SeqCst),
        2,
        "the worker stopped after its running call"
    );
    cx.update(SemanticGate::leave);
    f.wait_idle(cx);
}

/// T-010, K6: Clear starts only once the list's semantic worker has ended
/// (until then it may use the model and write the cache). Confirmed while the
/// worker is inside an embedder call, the UI goes on and nothing is deleted
/// until the call returns; then the folder is emptied and stays empty.
#[gpui_kit::test]
fn clear_waits_for_the_list_worker_off_the_ui_thread(cx: &mut TestAppContext) {
    let (world, control, f, handle) = setup(cx, &[], THREE_BATCHES);
    let batch = list_worker_in_a_batch(cx, &control, &f);
    let _open = OpenOnDrop(vec![batch.clone()]);
    // Back to lexical: the worker goes on with its call, and leaving the gate
    // starts no new one.
    f.press(cx, "ctrl-t");
    cx.run_until_parked();
    let shared = cx.update(crate::embedder::shared);
    let dir = world.env.semantic_cache_dir().expect("semantic dir");
    let model = dir.join("fastembed").join("model.bin");
    std::fs::create_dir_all(model.parent().unwrap()).unwrap();
    std::fs::write(&model, "model").unwrap();

    cx.update_window(handle, |_, window, cx| {
        open_clear_semantic_cache(window, cx)
    })
    .unwrap();
    settle(cx, handle);
    cx.simulate_keystrokes(handle, "y");
    cx.run_until_parked();
    assert_eq!(
        cx.read(SemanticGate::current),
        SemanticGateState::Exclusive(SemanticOp::Clear)
    );
    assert!(model.exists(), "Clear waits for the worker's call");
    assert!(shared.is_loaded(), "the worker still holds the model");

    batch.open();
    wait_for(cx, "Clear to finish", |cx| {
        last_toast(cx).as_ref() == "Semantic cache cleared."
    });
    cx.run_until_parked();
    assert!(!model.exists());
    assert!(
        !dir.join("embeddings-v2.bin").exists(),
        "the worker wrote nothing after the clear"
    );
    assert!(!shared.is_loaded(), "the model was dropped");
    assert_eq!(control.passage_calls.load(Ordering::SeqCst), 2);
    assert!(cx.read(SemanticGate::is_open));
}

/// T-009: with `--since 1d` Generate still covers the whole corpus (also the
/// conversations the launch filter excludes) and never shows the refusal text.
#[gpui_kit::test]
fn generate_ignores_the_launch_time_filter(cx: &mut TestAppContext) {
    let (world, _control, _f, handle) = setup(cx, &["--since", "1d"], 6);
    assert!(cx.read(|cx| crate::globals::app_state(cx).launch.time_filter.is_active()));
    // Conversations are dated by their files: make three of the six a week old.
    let mut files: Vec<_> = std::fs::read_dir(world.projects_root())
        .unwrap()
        .flatten()
        .flat_map(|dir| std::fs::read_dir(dir.path()).unwrap().flatten())
        .map(|file| file.path())
        .collect();
    files.sort();
    assert_eq!(files.len(), 6);
    for file in &files[..3] {
        crate::test_support::fixtures_tools::age_file(file, 7);
    }
    // Guard: the launch filter really excludes those three.
    let filtered = cx.read(|cx| {
        let state = crate::globals::app_state(cx);
        claude_history_core::maintenance::load_sorted_conversations(
            &state.env,
            state.settings.show_last,
            None,
            &state.launch.time_filter,
        )
        .expect("filtered load")
        .len()
    });
    assert_eq!(filtered, 3, "the filter keeps only the three fresh files");
    let dialog = open_generate(cx, handle);
    dialog.update(cx, |d, cx| d.start(cx));
    wait_for(cx, "the run to end", |cx| {
        matches!(
            dialog.read(cx).state(),
            GenerateState::Done(_) | GenerateState::Failed(_)
        )
    });
    let GenerateState::Done(text) = state(cx, &dialog) else {
        panic!("generate failed: {:?}", state(cx, &dialog));
    };
    assert!(
        !text.contains("cannot be combined with a time filter"),
        "{text}"
    );
    assert!(
        text.contains("from 6 recent conversation(s)"),
        "all six, not the filtered three: {text}"
    );
}

#[gpui_kit::test]
fn generate_defaults_to_all_workspaces_without_local(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    synth::corpus(&world, 4);
    let ws = synth::cwd_base().join("project-00");
    std::fs::create_dir_all(&ws).ok();
    ListFixture::init_app(cx, &world, &[]);
    let _control = install_fake_embedder(cx);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    let dialog = open_generate(cx, f.handle);
    // Without a workspace the Workspace scope cannot be chosen by the button,
    // but `local` + workspace preselects it (checked in the startup tests).
    assert_eq!(cx.read(|cx| dialog.read(cx).scope()), Scope::All);
}

/// T-010: Clear drops the shared model, empties the semantic folder but keeps
/// it and `embeddings.lock`, and a second Clear says it is already empty.
#[gpui_kit::test]
fn clear_drops_the_model_and_keeps_the_lock(cx: &mut TestAppContext) {
    let (world, _control, f, handle) = setup(cx, &[], 6);
    // A semantic list search loads the (fake) model into the shared embedder.
    f.press(cx, "ctrl-t");
    f.type_text(cx, "synthetic");
    f.wait_idle(cx);
    let shared = cx.update(crate::embedder::shared);
    assert!(
        shared.is_loaded(),
        "the list's semantic search loaded the model"
    );
    // Back to lexical: resuming the gate must not reload the model.
    f.press(cx, "ctrl-t");
    f.wait_idle(cx);
    let dir = world.env.semantic_cache_dir().expect("semantic dir");
    std::fs::create_dir_all(dir.join("fastembed")).unwrap();
    std::fs::write(dir.join("fastembed").join("model.bin"), "model").unwrap();
    std::fs::write(dir.join("embeddings-v2.bin"), "cache").unwrap();
    std::fs::write(dir.join("embeddings.lock"), "").unwrap();

    cx.update_window(handle, |_, window, cx| {
        open_clear_semantic_cache(window, cx)
    })
    .unwrap();
    settle(cx, handle);
    cx.simulate_keystrokes(handle, "y");
    wait_for(cx, "Clear to finish", |cx| {
        last_toast(cx).as_ref() == "Semantic cache cleared."
    });
    cx.run_until_parked();
    assert!(!shared.is_loaded(), "the model was dropped");
    assert!(dir.is_dir(), "the semantic folder stays");
    assert!(
        dir.join("embeddings.lock").exists(),
        "embeddings.lock stays"
    );
    assert!(!dir.join("embeddings-v2.bin").exists());
    assert!(!dir.join("fastembed").exists());
    assert!(cx.read(SemanticGate::is_open));

    cx.update_window(handle, |_, window, cx| {
        open_clear_semantic_cache(window, cx)
    })
    .unwrap();
    settle(cx, handle);
    cx.simulate_keystrokes(handle, "y");
    wait_for(cx, "the second Clear", |cx| {
        last_toast(cx).as_ref() == "Semantic cache is already empty."
    });
}

/// Clear is refused (inline error, nothing deleted) while the gate is held.
#[gpui_kit::test]
fn clear_is_refused_while_the_gate_is_held(cx: &mut TestAppContext) {
    let (world, _control, _f, handle) = setup(cx, &[], 6);
    let dir = world.env.semantic_cache_dir().expect("semantic dir");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("embeddings-v2.bin"), "cache").unwrap();
    assert!(
        cx.update(|cx| SemanticGate::enter_exclusive(SemanticOp::Generate, cx))
            .is_some()
    );
    cx.update_window(handle, |_, window, cx| {
        open_clear_semantic_cache(window, cx)
    })
    .unwrap();
    settle(cx, handle);
    cx.simulate_keystrokes(handle, "y");
    cx.run_until_parked();
    assert!(dir.join("embeddings-v2.bin").exists());
    cx.update(SemanticGate::leave);
}
