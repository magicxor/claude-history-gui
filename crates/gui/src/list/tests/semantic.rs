//! G tests of the semantic flows with a fake embedder (L-020, L-033, L-039,
//! L-070, L-071, S-023, S-025…S-029, S-036, X-016). The fake never loads a
//! model; gates make the slow phases observable.

use std::sync::Arc;

use claude_history_core::browser::ListSearchMode;
use std::sync::atomic::Ordering;

use gpui_kit::AppContext as _;
use gpui_kit::TestAppContext;
use gpui_kit::component::WindowExt as _;

use crate::list::footer::FooterModel;
use crate::list::semantic_details::{LastDetails, detail_lines};
use crate::list::states::StateAction;
use crate::notify::ToastKind;
use crate::test_support::fixtures_list::{
    FakeEmbedderControl, Gate, ListFixture, SessionSpec, install_failing_embedder,
    install_fake_embedder,
};
use crate::test_support::{TestWorld, synth};

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

fn footer(f: &ListFixture, cx: &mut TestAppContext) -> FooterModel {
    cx.update_window(f.handle, |_, window, cx| {
        f.list.read(cx).footer_model(window, cx)
    })
    .unwrap()
}

fn semantic_world(sessions: usize) -> TestWorld {
    let world = TestWorld::new();
    synth::corpus(&world, sessions);
    world
}

fn to_semantic(f: &ListFixture, cx: &mut TestAppContext) {
    f.press(cx, "ctrl-t");
}

/// S-029: switching to Semantic with an empty query embeds the passages
/// before any query is typed (prewarm).
#[gpui_kit::test]
fn semantic_mode_prewarms_on_toggle(cx: &mut TestAppContext) {
    let world = semantic_world(12);
    ListFixture::init_app(cx, &world, &[]);
    let control = install_fake_embedder(cx);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    assert_eq!(control.passages.load(Ordering::SeqCst), 0);
    to_semantic(&f, cx);
    f.wait_until(cx, "the passages to be embedded", |_, _| {
        control.passages.load(Ordering::SeqCst) > 0
    });
}

/// L-039, S-023: the footer shows `sem preparing embeddings` while the model
/// loads, no hints meanwhile, and nothing once it is ready.
#[gpui_kit::test]
fn the_footer_shows_the_model_preparation(cx: &mut TestAppContext) {
    let world = semantic_world(12);
    ListFixture::init_app(cx, &world, &[]);
    let control = install_fake_embedder(cx);
    let gate = Gate::closed();
    *control.init_gate.lock().unwrap() = Some(gate.clone());
    let _open = OpenOnDrop(vec![gate.clone()]);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    to_semantic(&f, cx);
    f.wait_until(cx, "the preparing activity", |list, _| {
        list.browser().semantic_activity_status_text().as_deref()
            == Some("sem preparing embeddings")
    });
    let model = footer(&f, cx);
    assert_eq!(
        model.semantic_activity.as_deref(),
        Some("sem preparing embeddings")
    );
    assert!(model.chips.is_empty(), "the activity replaces the hints");
    gate.open();
    f.wait_until(cx, "the activity to end", |list, _| {
        list.browser().semantic_activity_status_text().is_none()
            && !list.browser().has_search_work_in_flight()
    });
    assert_eq!(
        footer(&f, cx).semantic_activity,
        None,
        "S-023: none after Complete"
    );
}

/// L-039, S-026, S-036, X-016, S-025, L-020: embedding progress shows as a
/// percentage; typing keeps the rows and the progress; rows have no scores
/// while pending and scores after completion; Lexical rows never have scores.
#[gpui_kit::test]
fn embedding_progress_and_scores(cx: &mut TestAppContext) {
    let world = semantic_world(60);
    ListFixture::init_app(cx, &world, &[]);
    let control = install_fake_embedder(cx);
    let gate = Gate::closed();
    *control.batch_gate.lock().unwrap() = Some(gate.clone());
    let _open = OpenOnDrop(vec![gate.clone()]);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    to_semantic(&f, cx);
    f.wait_until(cx, "embedding progress", |list, _| {
        list.browser()
            .semantic_activity_status_text()
            .is_some_and(|text| text.starts_with("sem embedding "))
    });
    let model = footer(&f, cx);
    let text = model.semantic_activity.clone().unwrap();
    // `sem embedding <p>%  <d>/<t> chunks`
    let (percent, rest) = text
        .strip_prefix("sem embedding ")
        .and_then(|rest| rest.split_once("%  "))
        .unwrap_or_else(|| panic!("format of {text:?}"));
    assert!(percent.parse::<u32>().is_ok(), "{text}");
    let (done, total) = rest
        .strip_suffix(" chunks")
        .unwrap()
        .split_once('/')
        .unwrap();
    assert!(done.parse::<usize>().unwrap() <= total.parse::<usize>().unwrap());
    assert!(model.chips.is_empty());

    // Typing during the embedding keeps the rows and the progress.
    f.type_text(cx, "synthetic");
    // X-016: the lexical result of the typed query arrives while the passages
    // are still being embedded (a result of that query, not the empty browse).
    f.wait_until(cx, "the lexical fallback", |list, cx| {
        list.query(cx).as_ref() == "synthetic"
            && !list.browser().filtered().is_empty()
            && list.browser().list_search_mode() == ListSearchMode::Semantic
            && list.browser().search_status_text().contains("/")
    });
    assert!(f.read(cx, |list, _| list.browser().filtered().len()) > 0);
    let kept = footer(&f, cx);
    assert!(
        kept.semantic_activity
            .as_deref()
            .is_some_and(|text| text.starts_with("sem embedding ")),
        "{kept:?}"
    );
    // S-025: pending rows have no scores.
    let pending = f.read(cx, |list, _| list.row_models(0..3, 480.0));
    assert!(pending.iter().all(|row| row.score.is_none()), "{pending:?}");

    gate.open();
    f.wait_until(cx, "the semantic result", |list, _| {
        list.browser().semantic_activity_status_text().is_none()
            && !list.browser().has_search_work_in_flight()
            && list
                .browser()
                .semantic_result_metadata_for_selection()
                .is_some()
    });
    let done = f.read(cx, |list, _| list.row_models(0..3, 480.0));
    for row in &done {
        let score = row.score.as_deref().expect("a score in Semantic mode");
        assert!(
            score.len() == 4 && score.as_bytes()[1] == b'.',
            "two decimals: {score}"
        );
        assert!(row.meta_text().contains(score));
    }
    // Lexical mode: no scores.
    to_semantic(&f, cx);
    f.wait_idle(cx);
    let lexical = f.read(cx, |list, _| list.row_models(0..3, 480.0));
    assert!(lexical.iter().all(|row| row.score.is_none()));
}

/// L-033, S-027: a failing model shows `sem failed` with the error in a
/// tooltip, one error toast with recovery actions and an empty-state with
/// Switch to lexical / Retry; Switch to lexical restores the rows.
#[gpui_kit::test]
fn a_failing_embedder_is_reported_with_recovery(cx: &mut TestAppContext) {
    let world = semantic_world(10);
    ListFixture::init_app(cx, &world, &[]);
    install_failing_embedder(cx, "boom");
    let f = ListFixture::open_loaded(cx, &world, &[]);
    to_semantic(&f, cx);
    f.type_text(cx, "synthetic");
    f.wait_until(cx, "the failure", |list, _| {
        list.browser().semantic_error().is_some()
    });
    f.wait_idle(cx);
    let status = f.read(cx, |list, _| list.search_status());
    assert!(status.text.contains("sem failed"), "{status:?}");
    assert!(
        status
            .tooltip
            .as_deref()
            .is_some_and(|t| t.contains("boom")),
        "{status:?}"
    );
    let toasts: Vec<_> = f.read(cx, |list, _| {
        list.toasts()
            .iter()
            .filter(|toast| toast.kind == ToastKind::Error)
            .cloned()
            .collect()
    });
    assert_eq!(toasts.len(), 1, "one error toast: {toasts:?}");
    assert!(toasts[0].text.contains("boom"));
    assert_eq!(
        toasts[0].buttons,
        vec!["Switch to lexical".to_string(), "Retry".to_string()]
    );
    // S-027: the list is empty with the recovery state.
    assert!(f.paths(cx).is_empty());
    let state = f.read(cx, |list, cx| list.empty_state(cx)).unwrap();
    assert_eq!(state.title, "Semantic search failed");
    assert!(
        state
            .description
            .as_deref()
            .is_some_and(|d| d.contains("boom"))
    );
    assert_eq!(
        state.actions,
        vec![StateAction::SwitchToLexical, StateAction::RetrySemantic]
    );
    f.with_window(cx, |list, window, cx| {
        list.run_state_action(StateAction::SwitchToLexical, window, cx)
    });
    f.wait_idle(cx);
    assert_eq!(f.paths(cx).len(), 10, "the lexical rows are back");
}

/// L-070, L-071: Ctrl+S shows the exact semantic lines of a snapshot; a new
/// result set does not change them; any key closes; Lexical mode has no dialog.
#[gpui_kit::test]
fn ctrl_s_shows_a_snapshot_of_the_semantic_result(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    SessionSpec::new("proj", 1)
        .summary("Beta release")
        .messages(&[
            ("user", "beta release plan"),
            ("assistant", "beta checklist"),
        ])
        .write(&world);
    SessionSpec::new("proj", 2)
        .summary("Alpha")
        .messages(&[("user", "alpha rollout"), ("assistant", "alpha notes")])
        .start(chrono::Utc::now() - chrono::Duration::hours(3))
        .write(&world);
    ListFixture::init_app(cx, &world, &[]);
    let _control = install_fake_embedder(cx);
    let f = ListFixture::open_loaded(cx, &world, &[]);

    // L-071: Lexical mode, no metadata: no dialog.
    f.press(cx, "ctrl-s");
    assert!(
        !cx.update_window(f.handle, |_, window, cx| window.has_active_dialog(cx))
            .unwrap()
    );

    to_semantic(&f, cx);
    f.type_text(cx, "beta");
    f.wait_until(cx, "a semantic result", |list, _| {
        !list.browser().has_search_work_in_flight()
            && list
                .browser()
                .semantic_result_metadata_for_selection()
                .is_some()
    });
    let expected = f.read(cx, |list, _| {
        detail_lines(
            list.browser()
                .semantic_result_metadata_for_selection()
                .unwrap(),
        )
    });
    f.press(cx, "ctrl-s");
    assert!(
        cx.update_window(f.handle, |_, window, cx| window.has_active_dialog(cx))
            .unwrap()
    );
    let details = cx
        .read(|cx| cx.global::<LastDetails>().0.upgrade())
        .expect("the dialog");
    let lines = cx.read(|cx| details.read(cx).lines());
    assert_eq!(lines, expected);
    assert_eq!(lines.len(), 6);
    for (line, prefix) in lines.iter().zip([
        "hybrid ",
        "rationale ",
        "chunk ",
        "terms ",
        "preview ",
        "Esc close",
    ]) {
        assert!(line.starts_with(prefix), "{line:?} starts with {prefix:?}");
    }
    assert_eq!(lines[3], "terms beta", "{lines:?}");

    // A result set arriving while the dialog is open does not change it.
    f.with_window(cx, |list, window, cx| {
        list.apply_query("alpha".to_string(), window, cx)
    });
    f.wait_idle(cx);
    assert_eq!(cx.read(|cx| details.read(cx).lines()), expected);

    // Any key closes it.
    f.press(cx, "q");
    assert!(
        !cx.update_window(f.handle, |_, window, cx| window.has_active_dialog(cx))
            .unwrap()
    );
}

/// S-028: a corpus without text shows `sem no text` and the matching state.
#[gpui_kit::test]
fn a_corpus_without_text_has_no_semantic_text(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    SessionSpec::new("proj", 1)
        .summary("Only a summary")
        .messages(&[
            (
                "user",
                "<command-name>/model</command-name>\n<command-message>model</command-message>",
            ),
            (
                "assistant",
                "<local-command-stdout>Set model</local-command-stdout>",
            ),
        ])
        .write(&world);
    ListFixture::init_app(cx, &world, &[]);
    let _control: Arc<FakeEmbedderControl> = install_fake_embedder(cx);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    to_semantic(&f, cx);
    f.type_text(cx, "anything");
    f.wait_until(cx, "the no-text answer", |list, _| {
        matches!(
            list.browser().semantic_progress(),
            claude_history_core::browser::SemanticProgress::EmptyCorpus
        )
    });
    let status = f.read(cx, |list, _| list.search_status());
    assert!(status.text.contains("sem no text"), "{status:?}");
    let state = f.read(cx, |list, cx| list.empty_state(cx)).unwrap();
    assert_eq!(state.title, "No text to search semantically");
    assert_eq!(state.actions, vec![StateAction::SwitchToLexical]);
}

/// The semantic gate (§7.6): suspended → `sem paused`; resumed → normal again.
#[gpui_kit::test]
fn a_suspended_semantic_worker_says_paused(cx: &mut TestAppContext) {
    let world = semantic_world(8);
    ListFixture::init_app(cx, &world, &[]);
    let _control = install_fake_embedder(cx);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    to_semantic(&f, cx);
    f.wait_idle(cx);
    f.with_window(cx, |list, _, cx| list.suspend_semantic(cx));
    let status = f.read(cx, |list, _| list.search_status().text);
    assert!(status.contains("sem paused"), "{status}");
    f.with_window(cx, |list, _, cx| list.resume_semantic(cx));
    f.wait_idle(cx);
    let status = f.read(cx, |list, _| list.search_status().text);
    assert!(!status.contains("sem paused"), "{status}");
}

/// S-029: starting in Semantic mode embeds the passages after the corpus has
/// loaded, without a toggle.
#[gpui_kit::test]
fn starting_in_semantic_mode_prewarms_after_loaded(cx: &mut TestAppContext) {
    let world = semantic_world(12);
    ListFixture::init_app(cx, &world, &["--semantic"]);
    let control = install_fake_embedder(cx);
    let f = ListFixture::open(cx, &world, &["--semantic"]);
    f.wait_loaded(cx);
    assert_eq!(f.read(cx, |list, _| list.mode()), ListSearchMode::Semantic);
    f.wait_until(cx, "the passages to be embedded", |_, _| {
        control.passages.load(Ordering::SeqCst) > 0
    });
}
