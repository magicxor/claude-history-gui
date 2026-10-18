//! UI-thread timings of the browser on a 20k-row corpus (DESIGN §15.8,
//! CCR-1, CCR-2, X-038): the install of a finished load or refresh, a rename
//! apply, a delete removal and a UUID paste. Ignored by default (timings
//! need the release profile); run with
//!
//! ```text
//! cargo test --release -p claude-history-core --lib browser::perf_tests -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Every operation runs several times and the median is asserted against the
//! §15.8 target (a busy machine adds single slow samples; the minimum and the
//! maximum are printed too).

use super::test_helpers::*;
use super::*;
use crate::history::{LoaderMessage, Source};
use chrono::{Duration as ChronoDuration, Local, TimeZone};
use std::time::{Duration, Instant};

const ROWS: usize = 20_000;
const PROJECTS: usize = 10;
/// Repetitions of each measured operation.
const RUNS: usize = 7;

/// §15.8 list targets (UI-thread time, milliseconds).
const INSTALL_TARGET: f64 = 16.0;
const RENAME_TARGET: f64 = 5.0;
const DELETE_TARGET: f64 = 5.0;
const PASTE_TARGET: f64 = 2.0;

fn uuid(index: usize) -> String {
    format!("{index:08x}-1c2d-4e5f-8a9b-0c1d2e3f4a5b")
}

/// The (never created) working directory of synthetic project `n`.
fn project_dir(n: usize) -> PathBuf {
    let work = if cfg!(windows) {
        "C:\\Users\\someone\\work"
    } else {
        "/home/someone/work"
    };
    Path::new(work).join(format!("project-{n:02}"))
}

/// A synthetic session shaped like `synth::corpus` (GUI test support): ten
/// projects, one exchange each, realistic path lengths.
fn synthetic(root: &Path, index: usize) -> Conversation {
    let project = crate::env::workspace_project_dir_name(&project_dir(index % PROJECTS));
    let path = root.join(&project).join(format!("{}.jsonl", uuid(index)));
    let text = format!(
        "synthetic conversation {index} about refactoring the parser and the loader; \
         the assistant explains the change, lists the files and runs the tests \
         (item {index}, project {})",
        index % PROJECTS
    );
    Conversation {
        source: Source::Claude,
        session_id: uuid(index),
        path,
        index: 0,
        timestamp: Local.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()
            + ChronoDuration::seconds(((index * 7919) % ROWS) as i64 * 60),
        preview: text.clone(),
        preview_first: text.clone(),
        preview_last: text.clone(),
        search_text_lower: search::normalize_for_search(&text),
        semantic_turns: vec![text.clone()],
        semantic_turn_ranges: vec![crate::agent::refs::MessageRange::single(1)],
        full_text: text,
        agent_search_text: String::new(),
        semantic_route_text: String::new(),
        project_name: Some(format!("project-{:02}", index % PROJECTS)),
        project_path: Some(project_dir(index % PROJECTS)),
        cwd: None,
        message_count: 2,
        parse_errors: Vec::new(),
        summary: Some(format!("Summary {index}")),
        custom_title: None,
        model: Some("claude-opus-4-5-20251101".to_string()),
        total_tokens: 1234,
        duration_minutes: Some(3),
    }
}

fn corpus(root: &Path) -> Vec<Conversation> {
    (0..ROWS).map(|index| synthetic(root, index)).collect()
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn timed<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let started = Instant::now();
    let value = f();
    (value, started.elapsed())
}

/// Samples of one measured operation.
struct Samples {
    what: &'static str,
    target: Option<f64>,
    millis: Vec<f64>,
}

impl Samples {
    fn new(what: &'static str, target: Option<f64>) -> Self {
        Self {
            what,
            target,
            millis: Vec::new(),
        }
    }

    fn push(&mut self, took: Duration) {
        self.millis.push(ms(took));
    }

    fn sorted(&self) -> Vec<f64> {
        let mut sorted = self.millis.clone();
        sorted.sort_by(f64::total_cmp);
        sorted
    }

    fn median(&self) -> f64 {
        let sorted = self.sorted();
        sorted[sorted.len() / 2]
    }

    fn line(&self) -> String {
        let sorted = self.sorted();
        let target = self
            .target
            .map_or_else(String::new, |target| format!("  (target {target} ms)"));
        format!(
            "  {:<46} median {:>7.3}  min {:>7.3}  max {:>7.3} ms{target}",
            self.what,
            self.median(),
            sorted[0],
            sorted[sorted.len() - 1],
        )
    }
}

/// Poll until the load ends; returns the event and the install poll.
fn finish(loader: &mut HistoryLoader, browser: &mut Browser) -> (LoadEvent, Duration) {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let (poll, took) = timed(|| loader.poll(browser));
        if let Some(event) = poll.event {
            return (event, took);
        }
        assert!(Instant::now() < deadline, "the load did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// An initial load of the 20k corpus in batches of 500 (a project
/// directory each); `appends` gets every batch poll. Returns the browser
/// and the install poll.
fn initial_load(
    sandbox: &Sandbox,
    options: BrowserOptions,
    appends: &mut Samples,
    while_loading: impl FnOnce(&mut Browser),
) -> (Browser, Duration) {
    let root = sandbox.projects_root();
    let mut browser = Browser::new_loading(options);
    let mut loader = HistoryLoader::new(sandbox.env.clone(), LoaderOptions::default());
    let (tx, rx) = mpsc::channel();
    loader.start_with_receiver_for_test(rx, false);
    let mut conversations = corpus(&root).into_iter().peekable();
    while conversations.peek().is_some() {
        let batch = conversations.by_ref().take(500).collect::<Vec<_>>();
        tx.send(LoaderMessage::Batch(batch)).unwrap();
        appends.push(timed(|| loader.poll(&mut browser)).1);
    }
    while_loading(&mut browser);
    tx.send(LoaderMessage::Done).unwrap();
    let (event, install) = finish(&mut loader, &mut browser);
    assert!(
        matches!(
            event,
            LoadEvent::Loaded {
                conversations: ROWS,
                ..
            }
        ),
        "{event:?}"
    );
    (browser, install)
}

/// A refresh that reads the same 20k sessions again; returns the install.
fn refresh(sandbox: &Sandbox, browser: &mut Browser) -> Duration {
    let mut loader = HistoryLoader::new(sandbox.env.clone(), LoaderOptions::default());
    let (tx, rx) = mpsc::channel();
    loader.start_with_receiver_for_test(rx, true);
    tx.send(LoaderMessage::Batch(corpus(&sandbox.projects_root())))
        .unwrap();
    tx.send(LoaderMessage::Done).unwrap();
    let (event, install) = finish(&mut loader, browser);
    assert!(matches!(event, LoadEvent::Refreshed { .. }), "{event:?}");
    install
}

fn rename(browser: &mut Browser, row: usize, title: String) -> Duration {
    let target = browser.row(row).unwrap().clone();
    let mut renamed = target.clone();
    renamed.custom_title = Some(title);
    timed(|| browser.apply_renamed(&target.path, renamed).unwrap()).1
}

fn delete(browser: &mut Browser, row: usize) -> Duration {
    let victim = browser.row(row).unwrap().path.clone();
    timed(|| assert_eq!(browser.remove_paths(&[victim]), 1)).1
}

#[test]
#[ignore = "20k rows; run in release with --ignored"]
fn browser_timings_on_20k_rows() {
    let sandbox = Sandbox::new();
    let workspace = Some(crate::env::WorkspaceScope::new(project_dir(3)).unwrap());
    let scoped = || {
        sandbox
            .options()
            .with_exclude_projects(vec!["project-07".to_string()])
            .with_workspace(workspace.clone())
            .with_workspace_filter(true)
    };

    let mut appends = Samples::new("load: append of a 500-row batch", None);
    let mut load = Samples::new("load end: install (lexical)", Some(INSTALL_TARGET));
    let mut load_query = Samples::new("load end: install (query typed)", Some(INSTALL_TARGET));
    let mut load_semantic = Samples::new("load end: install (semantic)", Some(INSTALL_TARGET));
    let mut load_scope = Samples::new("load end: install (exclusions+scope)", Some(INSTALL_TARGET));
    let mut refresh_plain = Samples::new("refresh end: install (no query)", Some(INSTALL_TARGET));
    let mut refresh_query =
        Samples::new("refresh end: install (lexical query)", Some(INSTALL_TARGET));
    let mut refresh_semantic =
        Samples::new("refresh end: install (semantic)", Some(INSTALL_TARGET));
    let mut refresh_scope = Samples::new(
        "refresh end: install (exclusions+scope)",
        Some(INSTALL_TARGET),
    );
    let mut rename_plain = Samples::new("rename apply (no query)", Some(RENAME_TARGET));
    let mut rename_query = Samples::new("rename apply (lexical query)", Some(RENAME_TARGET));
    let mut delete_plain = Samples::new("delete removal (no query)", Some(DELETE_TARGET));
    let mut delete_query = Samples::new("delete removal (lexical query)", Some(DELETE_TARGET));
    let mut rename_semantic = Samples::new("rename apply (semantic)", Some(RENAME_TARGET));
    let mut delete_semantic = Samples::new("delete removal (semantic)", Some(DELETE_TARGET));
    let mut paste = Samples::new("UUID paste (loaded session)", Some(PASTE_TARGET));

    for run in 0..RUNS {
        // Lexical: load, edits without a query, paste, refresh, edits with a
        // query, refresh with a query.
        let (mut browser, install) =
            initial_load(&sandbox, sandbox.options(), &mut appends, |_| {});
        load.push(install);
        assert_eq!(browser.filtered().len(), ROWS);
        rename_plain.push(rename(&mut browser, 100 + run, format!("Renamed {run}")));
        delete_plain.push(delete(&mut browser, 10 + run));
        browser.set_query("");
        let id = uuid(15_000 + run);
        paste.push(timed(|| browser.set_query(id)).1);
        assert!(matches!(
            browser.uuid_lookup(),
            Some(UuidLookup::Found { .. })
        ));
        browser.set_query("");
        browser.select(500);
        refresh_plain.push(refresh(&sandbox, &mut browser));
        browser.set_query("synthetic conversation");
        wait_for_search(&mut browser);
        assert!(browser.filtered().len() > ROWS / 2);
        rename_query.push(rename(
            &mut browser,
            200 + run,
            format!("Query rename {run}"),
        ));
        wait_for_search(&mut browser);
        delete_query.push(delete(&mut browser, 20 + run));
        refresh_query.push(refresh(&sandbox, &mut browser));
        wait_for_search(&mut browser);
        drop(browser);

        let (browser, install) = initial_load(&sandbox, sandbox.options(), &mut appends, |b| {
            b.set_query("conversation 19");
        });
        load_query.push(install);
        drop(browser);

        let semantic = sandbox
            .options()
            .with_default_mode(ListSearchMode::Semantic);
        let (mut browser, install) = initial_load(&sandbox, semantic, &mut appends, |_| {});
        load_semantic.push(install);
        // The semantic worker holds the corpus (it is embedding it).
        rename_semantic.push(rename(
            &mut browser,
            300 + run,
            format!("Semantic rename {run}"),
        ));
        delete_semantic.push(delete(&mut browser, 30 + run));
        refresh_semantic.push(refresh(&sandbox, &mut browser));
        // Stop (and join) the worker that is still embedding 20k
        // conversations, so it does not compete with the next measurements.
        browser.suspend_semantic();
        drop(browser);

        let (mut browser, install) = initial_load(&sandbox, scoped(), &mut appends, |_| {});
        assert_eq!(browser.filtered().len(), ROWS / PROJECTS);
        load_scope.push(install);
        refresh_scope.push(refresh(&sandbox, &mut browser));
        drop(browser);
    }

    let all = [
        &appends,
        &load,
        &load_query,
        &load_semantic,
        &load_scope,
        &refresh_plain,
        &refresh_query,
        &refresh_semantic,
        &refresh_scope,
        &rename_plain,
        &rename_query,
        &delete_plain,
        &delete_query,
        &rename_semantic,
        &delete_semantic,
        &paste,
    ];
    eprintln!("browser UI-thread timings on {ROWS} rows (release, {RUNS} runs):");
    for samples in all {
        eprintln!("{}", samples.line());
    }
    let missed = all
        .iter()
        .filter(|samples| {
            samples
                .target
                .is_some_and(|target| samples.median() > target)
        })
        .map(|samples| samples.line())
        .collect::<Vec<_>>();
    assert!(missed.is_empty(), "targets missed:\n{}", missed.join("\n"));
}

/// The parts of the lookup data on 20k rows (diagnostics, no target).
#[test]
#[ignore = "20k rows; run in release with --ignored"]
fn index_costs_on_20k_rows() {
    let sandbox = Sandbox::new();
    let corpus = corpus(&sandbox.projects_root())
        .into_iter()
        .map(Arc::new)
        .collect::<Vec<_>>();
    let (_, keys) = timed(|| {
        corpus.iter().fold(0u64, |acc, conversation| {
            acc ^ super::index::path_key(&conversation.path)
                ^ super::index::session_key(&conversation.session_id)
        })
    });
    let (_, build) = timed(|| super::index::CorpusIndex::build(&corpus, None));
    let (_, scope) = timed(|| {
        super::index::ScopeIndex::compute(&corpus, super::index::ScopeSettings::default())
    });
    let (_, clone) = timed(|| corpus.clone());
    eprintln!("lookup data on {ROWS} rows (release):");
    eprintln!("  path + session keys        {:>8.3} ms", ms(keys));
    eprintln!("  index build                {:>8.3} ms", ms(build));
    eprintln!("  scope                      {:>8.3} ms", ms(scope));
    eprintln!("  copy of the pointer vector {:>8.3} ms", ms(clone));
}
