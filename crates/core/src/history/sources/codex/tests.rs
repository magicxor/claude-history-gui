use super::*;
use crate::history::actions::ConfiguredAt;
use crate::history::index;
use crate::history::model::{
    EventKind, ExecutionMode, HeadStatus, Message, Part, Relationship, Role, Visibility,
};
use crate::history::sources::Registry;

const LEGACY: &str = "0198f000-0000-7000-8000-000000000001";
const PAGED: &str = "01a00000-0000-7000-8000-000000000002";
const FORK: &str = "01a00000-0000-7000-8000-000000000003";
const REVERT: &str = "01a00000-0000-7000-8000-000000000004";
const REVERT2: &str = "01a00000-0000-7000-8000-000000000044";
const TORN: &str = "01a00000-0000-7000-8000-000000000005";
const EXEC: &str = "01a00000-0000-7000-8000-000000000006";
const SUB: &str = "01a00000-0000-7000-8000-000000000007";
const ARCH: &str = "01a00000-0000-7000-8000-000000000008";
const ORPHAN: &str = "01a00000-0000-7000-8000-000000000009";

fn fixture_home() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex")
}

/// The fixture home with a state database in `db_dir` that names the
/// threads' rollouts the way Codex does.
fn adapter(db_dir: &Path) -> CodexAdapter {
    create_state_db(&fixture_home(), db_dir);
    CodexAdapter::with_sqlite_home(fixture_home(), db_dir.to_path_buf())
}

/// A copy of the fixture home in `dir`, with its state database, for
/// tests that change it.
pub(crate) fn home_copy(dir: &Path) -> PathBuf {
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let path = entry.unwrap().path();
            let target = to.join(path.file_name().unwrap());
            if path.is_dir() {
                copy(&path, &target);
            } else {
                std::fs::copy(&path, &target).unwrap();
            }
        }
    }
    let home = dir.join("codex");
    copy(&fixture_home(), &home);
    create_state_db(&home, &home);
    home
}

/// Writes `state_5.sqlite` in `db_dir` with a row of every paginated
/// thread of `home`; the orphaned one names a rollout that is gone.
fn create_state_db(home: &Path, db_dir: &Path) {
    let rollouts = discover::list(home).unwrap();
    let path_of = |thread: &str| {
        rollouts
            .iter()
            .filter(|rollout| rollout.thread_id == thread)
            .map(|rollout| rollout.path.clone())
            .max()
            .unwrap()
    };
    let paths: Vec<(&str, PathBuf)> = [PAGED, FORK, REVERT, TORN, EXEC, SUB, ARCH]
        .into_iter()
        .map(|thread| (thread, path_of(thread)))
        .chain([(
            ORPHAN,
            home.join(format!(
                "sessions/2026/09/02/rollout-2026-09-02T14-30-00-{ORPHAN}_{ORPHAN}.jsonl"
            )),
        )])
        .collect();
    let rows: Vec<_> = paths
        .iter()
        .map(|(thread, path)| {
            let name = (*thread == PAGED).then_some("Named in Codex");
            (
                *thread,
                path.as_path(),
                "paginated",
                name,
                1_788_000_000_000,
            )
        })
        .collect();
    state_db::tests::create(db_dir, 5, &rows);
}

fn decoded(adapter: &CodexAdapter, thread: &str) -> Transcript {
    let located = adapter.resolve(thread, far_future()).unwrap();
    adapter.decode(&located).unwrap().unwrap()
}

fn far_future() -> SystemTime {
    SystemTime::now() + std::time::Duration::from_secs(3600)
}

fn texts(transcript: &Transcript, visibility: Visibility) -> Vec<String> {
    transcript
        .events
        .iter()
        .filter(|event| event.visibility == visibility)
        .filter_map(|event| match &event.kind {
            EventKind::Message(Message { parts, .. }) => Some(parts),
            _ => None,
        })
        .flatten()
        .filter_map(|part| match part {
            Part::Text(text) | Part::Thinking(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn mentions(transcript: &Transcript, visibility: Visibility, word: &str) -> bool {
    texts(transcript, visibility)
        .iter()
        .any(|text| text.contains(word))
}

#[test]
fn discovery_resolves_every_thread_of_the_home() {
    let db = tempfile::tempdir().unwrap();
    let adapter = adapter(db.path());
    let discovery = adapter.discover(far_future());
    assert_eq!(discovery.completeness, Completeness::Complete);
    let mut found: Vec<(String, HeadStatus, usize)> = discovery
        .found
        .iter()
        .map(|located| {
            let thread = adapter.thread_of(located).unwrap();
            assert_eq!(located.path, thread.head_link().path);
            assert!(located.stamp.is_known());
            (
                located.key.native_id.clone(),
                thread.head,
                thread.chain.len(),
            )
        })
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0));
    let expected = [
        (LEGACY, HeadStatus::Inferred, 1),
        (PAGED, HeadStatus::Current, 1),
        (FORK, HeadStatus::Current, 2),
        (REVERT, HeadStatus::Current, 2),
        (TORN, HeadStatus::Current, 1),
        (EXEC, HeadStatus::Current, 1),
        (SUB, HeadStatus::Current, 1),
        (ARCH, HeadStatus::Current, 1),
        (ORPHAN, HeadStatus::Orphaned, 1),
    ];
    let mut expected: Vec<_> = expected
        .into_iter()
        .map(|(id, head, links)| (id.to_owned(), head, links))
        .collect();
    expected.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(found, expected);

    let resolved = adapter.resolve(REVERT, far_future()).unwrap();
    let listed = discovery
        .found
        .iter()
        .find(|located| located.key.native_id == REVERT)
        .unwrap();
    assert_eq!(resolved.stamp, listed.stamp);
    assert!(resolved.path.to_string_lossy().contains(REVERT2));
}

#[test]
fn legacy_rollouts_decode_event_messages_and_function_calls() {
    let db = tempfile::tempdir().unwrap();
    let transcript = decoded(&adapter(db.path()), LEGACY);
    assert_eq!(transcript.source, SourceKind::Codex);
    assert_eq!(transcript.mode, ExecutionMode::Interactive);
    assert_eq!(transcript.relationship, Relationship::Root);
    assert_eq!(transcript.meta.session_id, LEGACY);
    assert_eq!(
        transcript.meta.custom_title.as_deref(),
        Some("Zebra listing")
    );
    assert_eq!(transcript.meta.model.as_deref(), Some("gpt-5-codex"));
    assert_eq!(
        transcript.meta.cwd.as_deref(),
        Some(Path::new("/work/demo"))
    );
    assert_eq!(
        transcript.meta.updated_at.unwrap().to_rfc3339(),
        "2026-09-01T06:00:20+00:00"
    );
    assert!(transcript.diagnostics.is_empty());
    assert_eq!(
        texts(&transcript, Visibility::Dialogue),
        [
            "How do I list the zebra files?",
            "Thinking about **listing** files",
            "Run `ls zebra` to see zebra-stripes.txt.",
        ]
    );
    assert!(mentions(&transcript, Visibility::Injected, "AGENTS.md"));
    let parts: Vec<&Part> = transcript
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::Message(message) => Some(&message.parts),
            _ => None,
        })
        .flatten()
        .collect();
    assert!(parts.contains(&&Part::ToolCall {
        id: "call_legacy_1".to_owned(),
        name: "shell".to_owned(),
        input: serde_json::json!({"command": ["ls", "zebra"], "workdir": "/work/demo"}),
    }));
    assert!(parts.contains(&&Part::ToolResult {
        call_id: "call_legacy_1".to_owned(),
        output: Some(serde_json::json!("zebra-stripes.txt\n")),
    }));

    let conversation = index::index(&transcript).unwrap();
    // Cumulative token counts, with a repeated one counted once.
    assert_eq!(conversation.total_tokens, 2500 + 90);
    assert!(conversation.full_text.contains("zebra-stripes.txt"));
    assert!(!conversation.full_text.contains("legacy injected context"));
    assert_eq!(
        conversation.own_text.as_ref(),
        "How do I list the zebra files?"
    );
    assert_eq!(
        conversation.preview_first.as_ref(),
        "How do I list the zebra files? ... Run `ls zebra` to see zebra-stripes.txt."
    );
}

#[test]
fn paginated_rollouts_decode_turn_items_and_custom_tools() {
    let db = tempfile::tempdir().unwrap();
    let transcript = decoded(&adapter(db.path()), PAGED);
    assert_eq!(transcript.head, HeadStatus::Current);
    assert_eq!(
        transcript.meta.custom_title.as_deref(),
        Some("Named in Codex")
    );
    assert_eq!(
        texts(&transcript, Visibility::Dialogue),
        [
            "Explain the walrus operator",
            "Recalling **PEP 572**",
            "The walrus operator assigns inside expressions.",
            "Now show a narwhal example",
            "Here is a narwhal example.",
        ]
    );
    let result = transcript
        .events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::Message(message) if message.role == Role::User => {
                message.parts.iter().find_map(|part| match part {
                    Part::ToolResult { output, .. } => output.clone(),
                    _ => None,
                })
            }
            _ => None,
        });
    assert_eq!(
        result,
        Some(serde_json::json!([
            {"type": "text", "text": "Script completed\n"},
            {"type": "text", "text": "walrus ok"},
        ]))
    );
    let conversation = index::index(&transcript).unwrap();
    assert_eq!(conversation.total_tokens, 7000 + 160);
    assert!(conversation.full_text.starts_with("Named in Codex "));
    assert!(conversation.full_text.contains("walrus ok"));
    assert_eq!(
        conversation.own_text.as_ref(),
        "Explain the walrus operator Now show a narwhal example"
    );
}

#[test]
fn forks_inherit_the_base_prefix_and_reverts_keep_their_own() {
    let db = tempfile::tempdir().unwrap();
    let adapter = adapter(db.path());

    let fork = decoded(&adapter, FORK);
    assert_eq!(fork.segments.len(), 2);
    assert!(fork.segments[0].inherited);
    assert_eq!(fork.segments[0].end, 3787);
    assert!(mentions(&fork, Visibility::Inherited, "walrus"));
    assert!(!mentions(&fork, Visibility::Inherited, "narwhal"));
    assert!(mentions(&fork, Visibility::Dialogue, "ocelot"));
    let conversation = index::index(&fork).unwrap();
    assert!(!conversation.full_text.contains("walrus"));
    assert!(!conversation.own_text.contains("walrus"));
    assert!(conversation.own_text.contains("ocelot"));
    // Only the fork's own usage, beyond the base's cumulative count.
    assert_eq!(conversation.total_tokens, (4000 + 140) - (3000 + 100));

    let revert = decoded(&adapter, REVERT);
    assert_eq!(revert.segments.len(), 2);
    assert!(!revert.segments[0].inherited);
    let dialogue = texts(&revert, Visibility::Dialogue).join(" ");
    assert!(dialogue.contains("heron"));
    assert!(dialogue.contains("flamingo"));
    assert!(!dialogue.contains("pelican"));
}

#[test]
fn a_torn_last_line_is_a_soft_diagnostic() {
    let db = tempfile::tempdir().unwrap();
    let transcript = decoded(&adapter(db.path()), TORN);
    assert_eq!(transcript.diagnostics.len(), 1);
    assert!(
        transcript.diagnostics[0]
            .error_message
            .contains("still being written")
    );
    assert!(
        index::index(&transcript)
            .unwrap()
            .full_text
            .contains("capybaras")
    );
}

#[test]
fn exec_runs_are_automated_and_subagents_children() {
    let db = tempfile::tempdir().unwrap();
    let adapter = adapter(db.path());

    let exec = decoded(&adapter, EXEC);
    assert_eq!(exec.mode, ExecutionMode::Automated);
    assert_eq!(exec.relationship, Relationship::Root);
    let conversation = index::index(&exec).unwrap();
    assert!(conversation.provenance.automated);
    // A subagent's report reaches its parent as a subagent message.
    assert!(
        conversation
            .agent_search_text
            .contains("iguana check passed")
    );
    // Lexically searchable, but not embedded.
    assert!(conversation.full_text.contains("iguana"));
    assert!(conversation.semantic_turns.is_empty());

    let sub = decoded(&adapter, SUB);
    assert_eq!(
        sub.relationship,
        Relationship::Child {
            parent: PAGED.to_owned()
        }
    );
    assert_eq!(sub.mode, ExecutionMode::Unknown);
    assert!(mentions(&sub, Visibility::Inherited, "walrus"));
    assert!(!mentions(&sub, Visibility::Dialogue, "walrus"));
    assert!(mentions(&sub, Visibility::Dialogue, "armadillo"));
    // The task from the parent is the subagent's prompt.
    assert!(mentions(
        &sub,
        Visibility::Dialogue,
        "Task name: /root/armadillo_search"
    ));
    let conversation = index::index(&sub).unwrap();
    assert!(conversation.provenance.child && conversation.provenance.hidden_by_default());
    assert!(conversation.semantic_turns.is_empty());
    // The parent wrote the subagent's prompt.
    assert_eq!(conversation.own_text.as_ref(), "");

    let archived = adapter.resolve(ARCH, far_future()).unwrap();
    assert!(adapter.thread_of(&archived).unwrap().archived);
    assert!(mentions(
        &decoded(&adapter, ARCH),
        Visibility::Dialogue,
        "tapir"
    ));
    assert_eq!(decoded(&adapter, ORPHAN).head, HeadStatus::Orphaned);
}

#[test]
fn a_busy_state_database_degrades_discovery_without_losing_anything() {
    let db = tempfile::tempdir().unwrap();
    let adapter = adapter(db.path());
    let writer = rusqlite::Connection::open(db.path().join("state_5.sqlite")).unwrap();
    writer
        .execute_batch("PRAGMA locking_mode = EXCLUSIVE; BEGIN EXCLUSIVE;")
        .unwrap();
    let discovery = adapter.discover(far_future());
    assert!(discovery.found.is_empty());
    assert_eq!(discovery.completeness, Completeness::Degraded);
    assert!(adapter.resolve(PAGED, far_future()).is_err());
    writer.execute_batch("COMMIT;").unwrap();
}

#[test]
fn without_a_state_database_heads_come_from_the_rollouts() {
    let db = tempfile::tempdir().unwrap();
    let adapter = CodexAdapter::with_sqlite_home(fixture_home(), db.path().to_path_buf());
    let discovery = adapter.discover(far_future());
    assert_eq!(discovery.found.len(), 9);
    let revert = adapter.resolve(REVERT, far_future()).unwrap();
    assert!(revert.path.to_string_lossy().contains(REVERT2));
    let paged = decoded(&adapter, PAGED);
    assert_eq!(paged.head, HeadStatus::Inferred);
    assert_eq!(paged.meta.custom_title.as_deref(), Some("Index name"));
    assert_eq!(decoded(&adapter, ORPHAN).head, HeadStatus::Inferred);
}

#[test]
fn registries_key_rollouts_by_thread_id() {
    let home = fixture_home();
    let registry = Registry::with_roots(None, None, None).with_codex(Some(home.clone()));
    let path = discover::list(&home)
        .unwrap()
        .into_iter()
        .find(|rollout| rollout.rollout_id == REVERT2)
        .unwrap()
        .path;
    let key = registry.key_of(&path).unwrap();
    assert_eq!(key.instance.kind, SourceKind::Codex);
    assert_eq!(key.native_id, REVERT);
    assert_eq!(registry.key_of(&home.join("session_index.jsonl")), None);
}

/// Fork: the commands are the fork's plans (`ResumePlan`, `delete_spec`),
/// and the deletion runs only outside tests.
#[test]
fn actions_run_the_codex_cli_on_the_rollouts_home() {
    let home = fixture_home();
    let adapter = CodexAdapter::with_sqlite_home(PathBuf::new(), PathBuf::new());
    let path = home.join(format!(
        "sessions/2026/09/01/rollout-2026-09-01T10-00-00-{PAGED}.jsonl"
    ));
    let project = tempfile::tempdir().unwrap();
    let target = Target {
        path: &path,
        project: Some(project.path()),
    };
    let sandbox = tempfile::tempdir().unwrap();
    let env = HistoryEnv::sandbox(sandbox.path());
    let args = |plan: &ResumePlan| -> Vec<String> {
        plan.args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    };
    let launch = Launch {
        launcher: Launcher::default_for(SourceKind::Codex),
        default_args: vec!["--yolo".to_owned()],
    };
    let resume = adapter.resume(&env, target, None, false, &launch).unwrap();
    assert!(resume.copy.is_none());
    assert_eq!(resume.launcher.program, Path::new("codex"));
    assert_eq!(args(&resume), ["--yolo", "resume", PAGED]);
    assert_eq!(resume.cwd.as_deref(), Some(project.path()));
    assert_eq!(
        resume.env,
        [(OsString::from("CODEX_HOME"), home.clone().into_os_string())]
    );
    // Upstream 8a99ccc: a configured command and its arguments start Codex.
    let wrapped = Launch {
        launcher: Launcher::configured(
            &env,
            &[
                "codex-wrapper".to_owned(),
                "--profile".to_owned(),
                "work".to_owned(),
            ],
            ConfiguredAt::Config("[resume.codex]"),
        ),
        default_args: Vec::new(),
    };
    let fork = adapter.resume(&env, target, None, true, &wrapped).unwrap();
    assert_eq!(fork.launcher.program, Path::new("codex-wrapper"));
    assert_eq!(args(&fork), ["fork", PAGED]);
    assert_eq!(
        fork.display_command(),
        format!("codex-wrapper --profile work fork {PAGED}")
    );
    let missing = Launcher::configured(
        &env,
        &["/nonexistent/claude-history-test/codex".to_owned()],
        ConfiguredAt::Config("[resume.codex]"),
    );
    let error = adapter
        .delete(&env, &path, &missing)
        .unwrap_err()
        .to_string();
    assert!(error.contains("[resume.codex]"), "{error}");
    assert!(!adapter.capabilities().rename);
    assert!(adapter.rename(&path, "x").is_err());

    // `codex delete --force <id>` on the rollout's home, never run by a test,
    // through the configured program and its leading arguments.
    let codex = sandbox.path().join("codex.exe");
    std::fs::write(&codex, "").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let configured = Launcher {
        args: vec!["--profile".to_owned(), "work".to_owned()],
        ..Launcher::program_path(&env, &codex, ConfiguredAt::GuiPath("codex_path"))
    };
    let spec = delete_spec(&env, &path, Path::new(""), &configured).unwrap();
    assert_eq!(spec.program.path, codex);
    assert_eq!(
        spec.args,
        ["--profile", "work", "delete", "--force", PAGED]
            .map(OsString::from)
            .to_vec()
    );
    assert_eq!(spec.env, resume.env);
    assert_eq!(thread_rollouts(&path).unwrap(), std::slice::from_ref(&path));
    let refused = adapter
        .delete(&env, &path, &Launcher::default_for(SourceKind::Codex))
        .unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("`codex` was not found on PATH"),
        "{refused}"
    );
    assert!(path.is_file(), "a test never deletes a rollout");
    assert!(
        delete_outcome(&RunOutput {
            success: true,
            status: "exit status: 0".to_owned(),
            stderr: String::new(),
        })
        .is_ok()
    );
    let failed = delete_outcome(&RunOutput {
        success: false,
        status: "exit status: 1".to_owned(),
        stderr: "no such thread\n".to_owned(),
    })
    .unwrap_err();
    assert_eq!(
        failed.to_string(),
        "I/O error: codex delete failed (exit status: 1): no such thread"
    );
}

/// Fork: the home and `CODEX_SQLITE_HOME` come from the [`HistoryEnv`]; the
/// home's `config.toml` `sqlite_home` wins over `CODEX_SQLITE_HOME`, as in
/// Codex. A sandbox env reads `<sandbox>/.codex`, never the real one.
#[test]
fn the_env_selects_the_home_and_the_state_database() {
    let sandbox = tempfile::tempdir().unwrap();
    let env = HistoryEnv::sandbox(sandbox.path());
    let adapter = CodexAdapter::from_env(&env).unwrap();
    assert_eq!(adapter.instance().root, sandbox.path().join(".codex"));
    assert_eq!(adapter.sqlite_home(), sandbox.path().join(".codex"));
    let registry = Registry::from_env(&env);
    assert_eq!(
        registry.for_source(SourceKind::Codex).instance().root,
        sandbox.path().join(".codex")
    );

    let home = sandbox.path().join("codex-home");
    let state = sandbox.path().join("codex-state");
    let mut env = env;
    env.codex_home = Some(home.clone());
    env.codex_sqlite_home = Some(state.clone());
    let adapter = CodexAdapter::from_env(&env).unwrap();
    assert_eq!(adapter.instance().root, home);
    assert_eq!(adapter.sqlite_home(), state);

    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("config.toml"), "sqlite_home = \"db\"\n").unwrap();
    assert_eq!(adapter.sqlite_home(), home.join("db"));

    assert!(CodexAdapter::from_env(&HistoryEnv::default()).is_none());
}

/// Fork: [`state_db_dir`] is the adapter's own resolution, public for the
/// GUI's sandbox gate: `config.toml`'s `sqlite_home` (absolute, or relative
/// to the home) wins over `CODEX_SQLITE_HOME`, which wins over the home; a
/// missing, unparseable or non-string setting names nothing.
#[test]
fn the_state_database_directory_resolves_like_the_adapter() {
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path().join("codex-home");
    let state = sandbox.path().join("codex-state");
    assert_eq!(configured_sqlite_home(&home), None);
    assert_eq!(state_db_dir(&home, None), home);
    assert_eq!(state_db_dir(&home, Some(&state)), state);

    std::fs::create_dir_all(&home).unwrap();
    let config = home.join("config.toml");
    std::fs::write(&config, "sqlite_home = \"db\"\n").unwrap();
    assert_eq!(configured_sqlite_home(&home), Some(home.join("db")));
    assert_eq!(state_db_dir(&home, Some(&state)), home.join("db"));

    let elsewhere = sandbox.path().join("elsewhere");
    std::fs::write(
        &config,
        format!("sqlite_home = '{}'\n", elsewhere.display()),
    )
    .unwrap();
    assert_eq!(state_db_dir(&home, Some(&state)), elsewhere);
    let mut env = HistoryEnv::sandbox(sandbox.path());
    env.codex_home = Some(home.clone());
    env.codex_sqlite_home = Some(state.clone());
    assert_eq!(
        CodexAdapter::from_env(&env).unwrap().sqlite_home(),
        elsewhere
    );

    for text in ["sqlite_home = 3\n", "sqlite_home = [\n", "model = \"x\"\n"] {
        std::fs::write(&config, text).unwrap();
        assert_eq!(configured_sqlite_home(&home), None, "{text}");
        assert_eq!(state_db_dir(&home, Some(&state)), state, "{text}");
    }
}

/// Fork: [`view::state_db_dir_of`] names the state database directory that
/// decoding a rollout opened by path opens, for the GUI's sandbox gate: in
/// the env of its own home with the env's `CODEX_SQLITE_HOME`, else that
/// of the rollout's home (its `config.toml`'s `sqlite_home`, else the
/// home); a file that does not decode as a rollout names none.
#[test]
fn opened_rollouts_name_the_state_database_directory_they_open() {
    let sandbox = tempfile::tempdir().unwrap();
    let home = home_copy(sandbox.path());
    let state = sandbox.path().join("codex-state");
    let rollout = discover::list(&home).unwrap()[0].path.clone();
    let mut env = HistoryEnv::sandbox(sandbox.path());
    env.codex_home = Some(home.clone());
    env.codex_sqlite_home = Some(state.clone());
    assert_eq!(view::state_db_dir_of(Some(&env), &rollout), Some(state));
    assert_eq!(view::state_db_dir_of(None, &rollout), Some(home.clone()));
    // An env whose Codex home is another one (`<sandbox>/.codex`).
    let other = HistoryEnv {
        codex_home: None,
        ..env.clone()
    };
    assert_eq!(
        view::state_db_dir_of(Some(&other), &rollout),
        Some(home.clone())
    );

    // The rollout's home names a directory: every decode opens it.
    let elsewhere = sandbox.path().join("elsewhere");
    std::fs::write(
        home.join("config.toml"),
        format!("sqlite_home = '{}'\n", elsewhere.display()),
    )
    .unwrap();
    for env in [Some(&env), Some(&other), None] {
        assert_eq!(
            view::state_db_dir_of(env, &rollout),
            Some(elsewhere.clone())
        );
    }

    // Files that do not decode as rollouts.
    let loose = sandbox.path().join("session.jsonl");
    let stray = home.join("sessions").join("notes.jsonl");
    for path in [loose, stray] {
        assert_eq!(view::state_db_dir_of(Some(&env), &path), None, "{path:?}");
        assert_eq!(view::state_db_dir_of(None, &path), None, "{path:?}");
    }
}

#[test]
fn viewed_threads_show_the_dialogue_and_where_a_fork_begins() {
    use crate::history::view::{self, Entry, EntryKind};
    fn text_of(entry: &Entry<'_>) -> String {
        let message = match entry.kind {
            EntryKind::Message { message, .. } | EntryKind::Subagent { message, .. } => message,
            EntryKind::Note { label, .. } => return format!("[{label}]"),
            EntryKind::Hidden => return String::new(),
        };
        message
            .parts
            .iter()
            .filter_map(|part| match part {
                Part::Text(text) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }
    let dir = tempfile::tempdir().unwrap();
    let home = home_copy(dir.path());
    let path_of = |thread: &str| {
        discover::list(&home)
            .unwrap()
            .into_iter()
            .find(|rollout| rollout.thread_id == thread)
            .unwrap()
            .path
    };

    let fork = super::view::decode_path(&path_of(FORK)).unwrap().unwrap();
    let fork = view::entries(&fork);
    let texts: Vec<String> = fork.iter().map(text_of).collect();
    let walrus = texts.iter().filter(|text| text.contains("walrus")).count();
    assert_eq!(
        walrus, 2,
        "the base's question and answer, once each: {texts:?}"
    );
    let boundary = texts.iter().position(|text| text == "[Fork]").unwrap();
    let ocelot = texts
        .iter()
        .position(|text| text.contains("ocelot"))
        .unwrap();
    assert!(boundary < ocelot);
    assert!(
        texts[..boundary]
            .iter()
            .all(|text| !text.contains("ocelot"))
    );
    assert!(fork.iter().any(|entry| matches!(
        entry.kind,
        EntryKind::Message { message, speaker: "Codex" } if message.role == Role::Assistant
    )));

    let sub = super::view::decode_path(&path_of(SUB)).unwrap().unwrap();
    let sub = view::entries(&sub);
    let texts: Vec<String> = sub.iter().map(text_of).collect();
    assert!(texts.iter().any(|text| text.contains("armadillo")));
    assert!(!texts.iter().any(|text| text == "[Fork]"));
    assert!(!texts.iter().any(|text| text.contains("walrus")));

    let claude = dir.path().join("session.jsonl");
    std::fs::write(&claude, "{}\n").unwrap();
    assert!(super::view::decode_path(&claude).is_none());
}

/// Fork: a rollout opened by path (the viewer, exports, a single-file
/// window) decodes as its thread, also through the streaming path the list
/// loaders use, and is listed with its source and provenance.
#[test]
fn rollouts_opened_by_path_decode_as_their_thread() {
    let dir = tempfile::tempdir().unwrap();
    let home = home_copy(dir.path());
    let path = discover::list(&home)
        .unwrap()
        .into_iter()
        .find(|rollout| rollout.thread_id == SUB)
        .unwrap()
        .path;
    let transcript = crate::history::sources::decode_path(&path, None, false).unwrap();
    assert_eq!(transcript.key.instance.kind, SourceKind::Codex);
    assert_eq!(transcript.key.native_id, SUB);
    assert!(!transcript.events.is_empty());
    let conversation = crate::history::process_conversation_file(path.clone(), None, None)
        .unwrap()
        .unwrap();
    assert_eq!(conversation.source, SourceKind::Codex);
    assert!(conversation.provenance.child);
    assert!(!crate::history::view::entries(&transcript).is_empty());
    let cancelled =
        crate::history::sources::decode_path_cancellable(&path, None, false, &|| true).unwrap_err();
    assert!(crate::history::adapter::is_cancelled(&cancelled));
}

/// Fork: a rollout of the env's own Codex home opened as a single file is
/// decoded through the env's adapter, so the single-file row finds the state
/// database in `CODEX_SQLITE_HOME` when the home's `config.toml` names none,
/// as the viewer does: the database's title and heads (an orphaned thread
/// stays orphaned). Decoded without the env, or as a rollout of another
/// home, the state database is not found there.
#[test]
fn single_files_of_the_envs_home_find_its_state_database() {
    use crate::browser::{Browser, BrowserOptions};
    use crate::history::service::HistoryService;
    use crate::history::sources::{decode_path, decode_path_in};

    let sandbox = tempfile::tempdir().unwrap();
    let home = home_copy(sandbox.path());
    let state = sandbox.path().join("codex-state");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::rename(home.join("state_5.sqlite"), state.join("state_5.sqlite")).unwrap();
    assert_eq!(configured_sqlite_home(&home), None);
    let mut env = HistoryEnv::sandbox(sandbox.path());
    env.codex_home = Some(home.clone());
    env.codex_sqlite_home = Some(state.clone());
    let env = Arc::new(env);
    let path_of = |thread: &str| {
        discover::list(&home)
            .unwrap()
            .into_iter()
            .find(|rollout| rollout.thread_id == thread)
            .unwrap()
            .path
    };
    let (paged, orphan) = (path_of(PAGED), path_of(ORPHAN));
    let history = HistoryService::from_env(&env);

    let row = |path: &Path| {
        let browser =
            Browser::new_single_file(path.to_path_buf(), BrowserOptions::new(env.clone()));
        assert_eq!(browser.single_file_error(), None);
        (**browser.conversations().first().unwrap()).clone()
    };
    let named = row(&paged);
    assert_eq!(named.source, SourceKind::Codex);
    assert_eq!(named.custom_title.as_deref(), Some("Named in Codex"));
    assert!(!named.provenance.orphaned);
    assert!(row(&orphan).provenance.orphaned);
    for (path, head) in [
        (&paged, HeadStatus::Current),
        (&orphan, HeadStatus::Orphaned),
    ] {
        let (_, viewed) = history.open_path(path).unwrap();
        assert_eq!(viewed.head, head);
        assert_eq!(decode_path_in(&env, path, None, false).unwrap().head, head);
    }
    assert_eq!(
        history.open_path(&paged).unwrap().1.meta.custom_title,
        named.custom_title
    );

    // Without the env, the home's own database (there is none) decides.
    let loose = decode_path(&paged, None, false).unwrap();
    assert_eq!(loose.head, HeadStatus::Inferred);
    assert_eq!(loose.meta.custom_title.as_deref(), Some("Index name"));
    // `CODEX_SQLITE_HOME` belongs to the env's own home (`<sandbox>/.codex`
    // here), not to a rollout of another home.
    let mut other = HistoryEnv::sandbox(sandbox.path());
    other.codex_sqlite_home = Some(state);
    let other = BrowserOptions::new(Arc::new(other));
    let browser = Browser::new_single_file(paged.clone(), other);
    let conversation = browser.conversations().first().unwrap();
    assert_eq!(conversation.custom_title.as_deref(), Some("Index name"));
}

/// The thread of a rollout holding `records` after its `session_meta`, in
/// a Codex home under `dir`.
fn rollout_thread(dir: &Path, meta: &str, records: &[&str]) -> Transcript {
    let id = "01a00000-0000-7000-8000-0000000000aa";
    let day = dir.join("sessions/2026/09/03");
    std::fs::create_dir_all(&day).unwrap();
    let path = day.join(format!("rollout-2026-09-03T10-00-00-{id}.jsonl"));
    let meta = format!(
        r#"{{"timestamp":"2026-09-03T10:00:00.000Z","type":"session_meta","payload":{{"id":"{id}","timestamp":"2026-09-03T10:00:00.000Z","cwd":"/work/demo","source":"cli"{meta}}}}}"#
    );
    let mut contents = std::iter::once(meta.as_str())
        .chain(records.iter().copied())
        .collect::<Vec<_>>()
        .join("\n");
    contents.push('\n');
    std::fs::write(&path, contents).unwrap();
    view::decode_path(&path).unwrap().unwrap()
}

fn record(time: &str, payload: &str) -> String {
    format!(r#"{{"timestamp":"2026-09-03T{time}.000Z","type":"event_msg","payload":{payload}}}"#)
}

#[test]
fn threads_were_last_active_at_their_own_newest_message() {
    let dir = tempfile::tempdir().unwrap();
    let records = [
        record("10:30:00", r#"{"type":"user_message","message":"copied"}"#),
        record("10:30:00", r#"{"type":"agent_message","message":"copied"}"#),
        record("10:00:05", r#"{"type":"user_message","message":"own"}"#),
        record("10:00:10", r#"{"type":"agent_message","message":"own"}"#),
        record("10:05:00", r#"{"type":"token_count","info":null}"#),
    ];
    let records: Vec<&str> = records.iter().map(String::as_str).collect();
    let transcript = rollout_thread(
        dir.path(),
        r#","subagent_history_start_ordinal":3"#,
        &records,
    );
    assert!(mentions(&transcript, Visibility::Inherited, "copied"));
    assert_eq!(
        transcript.meta.updated_at.unwrap().to_rfc3339(),
        "2026-09-03T10:00:10+00:00"
    );

    let inherited_only = rollout_thread(
        dir.path(),
        r#","subagent_history_start_ordinal":3"#,
        &records[..2],
    );
    assert_eq!(
        inherited_only.meta.updated_at.unwrap().to_rfc3339(),
        "2026-09-03T10:30:00+00:00"
    );
}

#[test]
fn threads_without_messages_were_last_active_at_their_last_record() {
    let dir = tempfile::tempdir().unwrap();
    let context = r#"{"timestamp":"2026-09-03T10:01:00.000Z","type":"turn_context","payload":{"cwd":"/work/demo","model":"gpt-5-codex"}}"#;
    let transcript = rollout_thread(dir.path(), "", &[context]);
    assert_eq!(
        transcript.meta.updated_at.unwrap().to_rfc3339(),
        "2026-09-03T10:01:00+00:00"
    );
}

#[test]
fn threads_record_the_cwds_of_their_turns() {
    let dir = tempfile::tempdir().unwrap();
    let context = |cwd: &str| {
        format!(
            r#"{{"timestamp":"2026-09-03T10:01:00.000Z","type":"turn_context","payload":{{"cwd":"{cwd}","model":"gpt-5-codex"}}}}"#
        )
    };
    let records = [
        context("/work/demo"),
        context("/work/demo/sub"),
        context("/work/demo"),
    ];
    let records: Vec<&str> = records.iter().map(String::as_str).collect();
    let transcript = rollout_thread(dir.path(), "", &records);
    assert_eq!(transcript.meta.cwd, Some(PathBuf::from("/work/demo")));
    assert_eq!(
        transcript.meta.cwds,
        vec![PathBuf::from("/work/demo"), PathBuf::from("/work/demo/sub")]
    );
    assert_eq!(transcript.meta.model.as_deref(), Some("gpt-5-codex"));
}
