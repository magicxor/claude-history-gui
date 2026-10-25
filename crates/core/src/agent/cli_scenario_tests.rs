//! Upstream `tests/agent_cli.rs` re-expressed as library tests.
//!
//! Upstream ran the CLI binary with `CLAUDE_CONFIG_DIR` and
//! `PI_CODING_AGENT_SESSION_DIR` pointing at temp dirs (but the real home,
//! so caches and `config.toml` leaked in, until 16cc49c set `HOME` to a temp
//! dir as well). Here every scenario runs
//! [`AgentService::execute`] with a [`HistoryEnv::sandbox`] whose home is a
//! temp dir as well, and with the default configuration (the
//! `exclude_paths` scenarios load the sandbox's `config.toml` like the GUI's
//! `agent` command). The argv of each upstream invocation is kept and
//! translated by a tiny test-only parser, so the scenarios read like the
//! originals. "stdout" is the returned protocol
//! text, "stderr" the protocol error envelope (or the collected notices).
//!
//! The `--render` parts (`direct_render_*`) exercise the terminal display
//! (upstream `display.rs`, not part of core); they are reduced to the
//! active-branch projection the renderer consumes. Rendering assertions
//! belong to `core::view` (Modules stage).

use crate::agent::args::{
    AgentOutlineArgs, AgentReadArgs, AgentRequest, AgentSearchArgs, AgentWithinArgs,
};
use crate::agent::service::AgentService;
use crate::config::ConfigFile;
use crate::env::HistoryEnv;
use crate::error::AppError;
use crate::search::mode::SearchMode;
use std::path::{Path, PathBuf};

struct Output {
    success: bool,
    stdout: String,
    stderr: String,
}

/// Translate an upstream `claude-history agent ...` argv (without the
/// program name) into a request. Supports exactly the flags these scenarios
/// use; anything else panics.
fn parse(args: &[&str]) -> AgentRequest {
    assert_eq!(args[0], "agent");
    let command = args[1];
    let mut positional = Vec::new();
    let mut options = Vec::<(&str, Option<&str>)>::new();
    let mut index = 2;
    while index < args.len() {
        let arg = args[index];
        if let Some(flag) = arg.strip_prefix("--") {
            let takes_value = matches!(
                flag,
                "mode" | "since" | "after" | "before" | "focus" | "budget" | "anchor" | "top"
            );
            if takes_value {
                options.push((flag, Some(args[index + 1])));
                index += 2;
            } else {
                options.push((flag, None));
                index += 1;
            }
        } else {
            positional.push(arg.to_string());
            index += 1;
        }
    }
    match command {
        "search" => {
            let mut search = AgentSearchArgs::new(positional[0].clone());
            for (flag, value) in options {
                match (flag, value) {
                    ("lexical", None) => search.search_mode.lexical = true,
                    ("exact", None) => search.search_mode.exact = true,
                    ("all-kinds", None) => search.all_kinds = true,
                    ("user-only", None) => search.user_only = true,
                    ("local", None) => search.local = true,
                    ("mode", Some(mode)) => {
                        search.search_mode.mode = Some(mode.parse::<SearchMode>().unwrap())
                    }
                    ("since", Some(value)) => search.time.since = Some(value.parse().unwrap()),
                    ("after", Some(value)) => search.time.after = Some(value.parse().unwrap()),
                    ("before", Some(value)) => search.time.before = Some(value.parse().unwrap()),
                    other => panic!("unsupported search option {other:?}"),
                }
            }
            AgentRequest::Search(search)
        }
        "within" => {
            let mut within = AgentWithinArgs::new(positional[0].clone(), positional[1].clone());
            for (flag, value) in options {
                match (flag, value) {
                    ("lexical", None) => within.search_mode.lexical = true,
                    ("exact", None) => within.search_mode.exact = true,
                    other => panic!("unsupported within option {other:?}"),
                }
            }
            AgentRequest::Within(within)
        }
        "read" => {
            let mut read = AgentReadArgs::new(positional);
            for (flag, value) in options {
                match (flag, value) {
                    ("focus", Some(value)) => read.focus = Some(value.to_string()),
                    ("anchor", Some(value)) => read.anchor = Some(value.to_string()),
                    ("budget", Some(value)) => read.output.budget = Some(value.parse().unwrap()),
                    other => panic!("unsupported read option {other:?}"),
                }
            }
            AgentRequest::Read(read)
        }
        "outline" => {
            assert!(
                options.is_empty(),
                "unsupported outline options {options:?}"
            );
            AgentRequest::Outline(AgentOutlineArgs::new(positional[0].clone()))
        }
        other => panic!("unsupported agent command {other}"),
    }
}

fn run_env(env: &HistoryEnv, args: &[&str]) -> Output {
    run_env_with(env, ConfigFile::default(), args)
}

/// [`run_env`] with the configuration the GUI's `agent` command uses: the
/// sandbox's `config.toml` layered with its `gui.toml` (upstream read the
/// shared `config.toml` from its temp home).
fn run_configured(env: &HistoryEnv, args: &[&str]) -> Output {
    let config = crate::config::load_layered_config(env)
        .expect("load the sandbox configuration")
        .effective;
    run_env_with(env, config, args)
}

fn run_env_with(env: &HistoryEnv, config: ConfigFile, args: &[&str]) -> Output {
    let mut service = AgentService::new(env.clone(), config);
    let result = service.execute(parse(args));
    let notices = service
        .take_notices()
        .iter()
        .map(|notice| format!("{notice}\n"))
        .collect::<String>();
    match result {
        Ok(stdout) => Output {
            success: true,
            stdout,
            stderr: notices,
        },
        Err(AppError::AgentProtocol(stderr)) => Output {
            success: false,
            stdout: String::new(),
            stderr,
        },
        Err(error) => Output {
            success: false,
            stdout: String::new(),
            stderr: format!("Error: {error}\n"),
        },
    }
}

/// A sandbox whose home is `home`, with upstream's `CLAUDE_CONFIG_DIR` and
/// `PI_CODING_AGENT_SESSION_DIR` overrides. Its caches and the Codex home it
/// reads stay out of the user's (upstream removes `CODEX_HOME` and
/// `CODEX_SQLITE_HOME` from the binary's environment).
fn sandbox(home: &Path, config: &Path, sessions: &Path) -> HistoryEnv {
    let env = HistoryEnv::sandbox(home)
        .with_claude_config_dir(config)
        .with_pi_session_dir(sessions);
    assert_eq!(env.codex_home(), Some(home.join(".codex")));
    assert_eq!(env.codex_sqlite_home(), None);
    env
}

/// Upstream `run(config, args)`.
fn run(home: &Path, config: &Path, args: &[&str]) -> Output {
    run_env(
        &sandbox(home, config, &config.join("empty-agent-sessions")),
        args,
    )
}

/// Upstream `run_pi(config, sessions, args)`.
fn run_pi(home: &Path, config: &Path, sessions: &Path, args: &[&str]) -> Output {
    run_env(&sandbox(home, config, sessions), args)
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn project(config: &Path) -> PathBuf {
    let project = config.join("projects").join("-tmp-agent-phase3-tests");
    std::fs::create_dir_all(&project).expect("create project");
    project
}

fn write_transcript(path: &Path, needle: &str) {
    write_transcript_at(path, needle, "2026-07-20");
}

/// Backdate a transcript's modification time.
///
/// Conversation timestamps come from the file's mtime (see
/// `history::parser`), not from the records inside it, so time filtering can
/// only be exercised by changing the mtime. `stamp` is `YYYYMMDDhhmm` in
/// local time, like upstream's `touch -t`, which does not exist on Windows
/// (W12); `File::set_modified` works everywhere.
fn set_modified(path: &Path, stamp: &str) {
    use chrono::TimeZone;
    let naive = chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%d%H%M").expect("stamp");
    let local = chrono::Local
        .from_local_datetime(&naive)
        .earliest()
        .expect("local time");
    let file = std::fs::File::options()
        .write(true)
        .open(path)
        .expect("open transcript");
    file.set_modified(std::time::SystemTime::from(local))
        .expect("set mtime");
}

fn write_transcript_at(path: &Path, needle: &str, date: &str) {
    let user = serde_json::json!({
        "type": "user",
        "timestamp": format!("{date}T00:00:00Z"),
        "cwd": "/tmp/agent-phase3-tests",
        "message": {"role": "user", "content": needle}
    });
    let assistant = serde_json::json!({
        "type": "assistant",
        "timestamp": format!("{date}T00:00:01Z"),
        "message": {"role": "assistant", "content": [{"type": "text", "text": "answer"}]}
    });
    std::fs::write(path, format!("{user}\n{assistant}\n")).expect("write transcript");
}

fn first_ref(output: &str) -> String {
    output
        .split_whitespace()
        .find_map(|field| field.strip_prefix("ref="))
        .expect("search ref")
        .trim_end_matches(|character: char| !character.is_ascii_hexdigit())
        .to_string()
}

/// Temp dirs for one scenario: `home` (sandbox home: caches, Pi/OMP
/// defaults), `config` (CLAUDE_CONFIG_DIR) and `sessions`.
fn dirs() -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir) {
    (
        tempfile::tempdir().expect("home"),
        tempfile::tempdir().expect("config"),
        tempfile::tempdir().expect("sessions"),
    )
}

#[test]
fn pi_sessions_support_agent_search_read_and_outline_without_claude_storage() {
    let (home, config, sessions) = dirs();
    std::fs::copy(
        fixture("pi/v3-branched.jsonl"),
        sessions.path().join("pi.jsonl"),
    )
    .expect("copy Pi fixture");

    let search = run_pi(
        home.path(),
        config.path(),
        sessions.path(),
        &["agent", "search", "--lexical", "active root question"],
    );
    assert!(search.success, "{}", search.stderr);
    assert!(
        search
            .stdout
            .contains("uuid=01912345-6789-7abc-8def-0123456789ab")
    );
    assert!(!search.stdout.contains("ABANDONED_BRANCH_SENTINEL"));
    let reference = first_ref(&search.stdout);

    let read = run_pi(
        home.path(),
        config.path(),
        sessions.path(),
        &["agent", "read", &reference],
    );
    assert!(read.success, "{}", read.stderr);
    assert!(read.stdout.contains("active root question"));
    assert!(!read.stdout.contains("compaction summary searchable"));
    assert!(!read.stdout.contains("branch summary searchable"));
    assert!(!read.stdout.contains("ABANDONED_BRANCH_SENTINEL"));

    let outline = run_pi(
        home.path(),
        config.path(),
        sessions.path(),
        &["agent", "outline", &reference],
    );
    assert!(outline.success, "{}", outline.stderr);
    assert!(outline.stdout.contains("active root question"));
}

#[test]
fn codex_threads_support_agent_search_read_and_outline() {
    const FORK: &str = "01a00000-0000-7000-8000-000000000003";
    const EXEC: &str = "01a00000-0000-7000-8000-000000000006";
    let (home, config, sessions) = dirs();
    let codex = crate::history::sources::codex::tests::home_copy(home.path());
    let env = sandbox(home.path(), config.path(), sessions.path()).with_codex_home(&codex);

    let search = run_env(&env, &["agent", "search", "--lexical", "ocelot example"]);
    assert!(search.success, "{}", search.stderr);
    assert!(
        search.stdout.contains(&format!("uuid={FORK}")),
        "{}",
        search.stdout
    );
    let reference = first_ref(&search.stdout);

    let read = run_env(&env, &["agent", "read", &reference]);
    assert!(read.success, "{}", read.stderr);
    assert!(
        read.stdout.contains("Here is an ocelot example."),
        "{}",
        read.stdout
    );
    // The history the fork continues from is the base thread's own.
    assert!(!read.stdout.contains("walrus"), "{}", read.stdout);

    let outline = run_env(&env, &["agent", "outline", &reference]);
    assert!(outline.success, "{}", outline.stderr);
    assert!(outline.stdout.contains("ocelot"));

    // exec runs stay out of agent search like they stay out of the list.
    let hidden = run_env(&env, &["agent", "search", "--lexical", "iguana report"]);
    assert!(hidden.success, "{}", hidden.stderr);
    assert!(!hidden.stdout.contains(EXEC), "{}", hidden.stdout);

    // --all-kinds searches them, and their refs resolve without it.
    let all = run_env(
        &env,
        &[
            "agent",
            "search",
            "--lexical",
            "--all-kinds",
            "iguana report",
        ],
    );
    assert!(all.success, "{}", all.stderr);
    assert!(
        all.stdout.contains(&format!("uuid={EXEC}")),
        "{}",
        all.stdout
    );
    let exec_ref = first_ref(&all.stdout);
    let exec_outline = run_env(&env, &["agent", "outline", &exec_ref]);
    assert!(exec_outline.success, "{}", exec_outline.stderr);
    assert!(
        exec_outline.stdout.contains(&format!("uuid={EXEC}")),
        "{}",
        exec_outline.stdout
    );
}

#[test]
fn omp_sessions_support_agent_search() {
    let (home, config, sessions) = dirs();
    std::fs::copy(fixture("omp/v3.jsonl"), sessions.path().join("omp.jsonl"))
        .expect("copy OMP fixture");

    let search = run_pi(
        home.path(),
        config.path(),
        sessions.path(),
        &["agent", "search", "--lexical", "OMP active question"],
    );
    assert!(search.success, "{}", search.stderr);
    assert!(search.stdout.contains("uuid=omp_session_custom_id"));
    assert!(!search.stdout.contains("OMP_ABANDONED_SENTINEL"));
    let reference = first_ref(&search.stdout);

    let read = run_pi(
        home.path(),
        config.path(),
        sessions.path(),
        &["agent", "read", &reference],
    );
    assert!(read.success, "{}", read.stderr);
    assert!(read.stdout.contains("OMP active answer"));
}

/// Upstream `omp_sessions_support_agent_search_and_direct_render` and
/// `direct_render_supports_pi_active_branch` (render half): the renderer
/// consumes the decoded transcript (`sources::decode_path` /
/// `history::view::entries`), whose events must hold the active branch only. The label/metadata assertions of the rendered text move to the
/// `core::view` tests (Modules stage).
#[test]
fn direct_render_inputs_contain_only_the_active_branch() {
    for (fixture_name, active, abandoned) in [
        (
            "omp/v3.jsonl",
            "OMP active question",
            "OMP_ABANDONED_SENTINEL",
        ),
        (
            "pi/v3-branched.jsonl",
            "active root question",
            "ABANDONED_BRANCH_SENTINEL",
        ),
    ] {
        let transcript =
            crate::history::sources::decode_path(&fixture(fixture_name), None, false).unwrap();
        let text = format!("{:?}", transcript.events);
        assert!(text.contains(active), "{fixture_name}: {text}");
        assert!(!text.contains(abandoned), "{fixture_name}: {text}");
    }
}

#[test]
fn malformed_and_missing_refs_have_structured_stderr_and_nonzero_exit() {
    let (home, config, _) = dirs();
    project(config.path());

    let invalid = run(home.path(), config.path(), &["agent", "read", "not-a-ref"]);
    assert!(!invalid.success);
    assert!(invalid.stdout.is_empty());
    assert!(
        invalid
            .stderr
            .starts_with("protocol agent-error kind=invalid-ref ref=not-a-ref"),
        "{}",
        invalid.stderr
    );

    let missing = run(
        home.path(),
        config.path(),
        &["agent", "read", "ch_12345678"],
    );
    assert!(!missing.success);
    assert!(
        missing
            .stderr
            .starts_with("protocol agent-error kind=not-found ref=ch_12345678"),
        "{}",
        missing.stderr
    );
}

#[test]
fn target_transcript_and_range_failures_have_precise_kinds() {
    let (home, config, _) = dirs();
    let transcript = project(config.path()).join("12345678-1234-4234-9234-123456789abc.jsonl");
    write_transcript(&transcript, "phase three needle");

    let search = run(
        home.path(),
        config.path(),
        &["agent", "search", "--lexical", "phase three needle"],
    );
    assert!(search.success, "{}", search.stderr);
    let reference = first_ref(&search.stdout);

    let range = run(
        home.path(),
        config.path(),
        &["agent", "read", &format!("{reference}:m99")],
    );
    assert!(!range.success);
    assert!(
        range.stderr.starts_with(&format!(
            "protocol agent-error kind=out-of-range ref={reference}"
        )),
        "{}",
        range.stderr
    );

    std::fs::write(&transcript, "{malformed\n").expect("malform transcript");
    let malformed = run(home.path(), config.path(), &["agent", "read", &reference]);
    assert!(!malformed.success);
    assert!(
        malformed.stderr.starts_with(&format!(
            "protocol agent-error kind=malformed-transcript ref={reference}"
        )),
        "{}",
        malformed.stderr
    );
}

#[test]
fn search_reports_partial_warnings_and_preserves_compact_success_output() {
    let (home, config, _) = dirs();
    let project = project(config.path());
    write_transcript(
        &project.join("12345678-1234-4234-9234-123456789abc.jsonl"),
        "warning contract needle",
    );
    std::fs::write(
        project.join("87654321-1234-4234-9234-123456789abc.jsonl"),
        "{malformed\n",
    )
    .expect("write malformed transcript");

    let output = run(
        home.path(),
        config.path(),
        &["agent", "search", "--lexical", "warning contract needle"],
    );
    assert!(output.success, "{}", output.stderr);
    assert!(output.stderr.is_empty());
    assert!(
        output
            .stdout
            .starts_with("protocol agent-search mode=lexical")
    );
    assert!(
        output
            .stdout
            .contains("protocol agent-warning kind=malformed-transcript ref=ch_")
    );
    assert!(output.stdout.contains("read ref=ch_"));
    // The warning's ref addresses the malformed transcript (the same
    // identity discovery gives it on every platform).
    let warning_ref = output
        .stdout
        .lines()
        .find(|line| line.contains("agent-warning kind=malformed-transcript"))
        .and_then(|line| line.split("ref=").nth(1))
        .and_then(|rest| rest.split_whitespace().next())
        .expect("warning ref")
        .to_owned();
    let read = run(home.path(), config.path(), &["agent", "read", &warning_ref]);
    assert!(
        read.stderr.starts_with(&format!(
            "protocol agent-error kind=malformed-transcript ref={warning_ref}"
        )),
        "{}",
        read.stderr
    );
}

/// Upstream 82b355a. Fork: the workspace is the sandbox env's (upstream ran
/// the binary in the repo folder), given in plain form: Claude Code records
/// the working directory without Windows' `\\?\` prefix.
#[test]
fn local_search_includes_sessions_that_worked_in_the_workspace() {
    let (home, config, _) = dirs();
    let group = config.path().join("work").join("group");
    let repo = group.join("repo");
    std::fs::create_dir_all(&repo).expect("create repo");
    // The process sees the resolved directory, as the recorded cwd does.
    let resolved = |dir: &Path| {
        crate::env::normalize_windows_path(&dir.canonicalize().expect("canonical dir"))
    };
    let group = resolved(&group);
    let repo = resolved(&repo);
    let project = config
        .path()
        .join("projects")
        .join(crate::env::workspace_project_dir_name(&group));
    std::fs::create_dir_all(&project).expect("create project");
    let write = |name: &str, cwds: &[&Path]| {
        let lines: Vec<String> = cwds
            .iter()
            .map(|cwd| {
                serde_json::json!({
                    "type": "user",
                    "timestamp": "2026-07-20T00:00:00Z",
                    "cwd": cwd,
                    "message": {"role": "user", "content": "scoped cwd needle"}
                })
                .to_string()
            })
            .collect();
        std::fs::write(project.join(name), lines.join("\n") + "\n").expect("write transcript");
    };
    write(
        "12345678-1234-4234-9234-123456789abc.jsonl",
        &[&group, &repo.join("src")],
    );
    write(
        "87654321-1234-4234-9234-123456789abc.jsonl",
        &[&group, &group.join("repo-other")],
    );

    let env = sandbox(
        home.path(),
        config.path(),
        &config.path().join("empty-agent-sessions"),
    )
    .with_workspace(&repo);
    let search = run_env(
        &env,
        &[
            "agent",
            "search",
            "--local",
            "--lexical",
            "scoped cwd needle",
        ],
    );
    assert!(search.success, "{}", search.stderr);
    assert!(
        search.stdout.contains("uuid=12345678-"),
        "{}",
        search.stdout
    );
    assert!(
        !search.stdout.contains("uuid=87654321-"),
        "{}",
        search.stdout
    );
    let outline = run(
        home.path(),
        config.path(),
        &["agent", "outline", &first_ref(&search.stdout)],
    );
    assert!(outline.success, "{}", outline.stderr);
    assert!(outline.stdout.contains("12345678"));
}

#[test]
fn ref_only_commands_parse_only_the_selected_transcript() {
    let (home, config, _) = dirs();
    let project = project(config.path());
    let selected = project.join("12345678-1234-4234-9234-123456789abc.jsonl");
    write_transcript(&selected, "selected transcript needle");

    let search = run(
        home.path(),
        config.path(),
        &["agent", "search", "--lexical", "selected transcript needle"],
    );
    assert!(search.success);
    let reference = first_ref(&search.stdout);
    std::fs::write(
        project.join("87654321-1234-4234-9234-123456789abc.jsonl"),
        "{malformed\n",
    )
    .expect("write unrelated malformed transcript");

    let outline = run(
        home.path(),
        config.path(),
        &["agent", "outline", &reference],
    );

    assert!(outline.success, "{}", outline.stderr);
    assert!(outline.stderr.is_empty());
    assert!(outline.stdout.contains("m1 role=user"));
    assert!(outline.stdout.contains("m2 role=assistant"));
    assert!(!outline.stdout.contains("malformed-transcript"));
}

#[test]
fn selected_partial_transcript_recovers_records_and_reports_warning() {
    let (home, config, _) = dirs();
    let transcript = project(config.path()).join("12345678-1234-4234-9234-123456789abc.jsonl");
    write_transcript(&transcript, "partial transcript needle");
    let search = run(
        home.path(),
        config.path(),
        &["agent", "search", "--lexical", "partial transcript needle"],
    );
    assert!(search.success);
    let reference = first_ref(&search.stdout);
    let content = std::fs::read_to_string(&transcript).expect("read transcript");
    let (first, second) = content.split_once('\n').expect("two records");
    std::fs::write(&transcript, format!("{first}\n{{malformed\n{second}"))
        .expect("write partial transcript");

    let recovered_search = run(
        home.path(),
        config.path(),
        &["agent", "search", "--lexical", "partial transcript needle"],
    );
    assert!(recovered_search.success);
    assert!(recovered_search.stdout.contains("focus=m1..m1"));
    assert!(
        recovered_search
            .stdout
            .contains("kind=malformed-transcript")
    );

    let within = run(
        home.path(),
        config.path(),
        &[
            "agent",
            "within",
            &reference,
            "partial transcript needle",
            "--lexical",
        ],
    );
    assert!(within.success);
    assert!(within.stdout.contains("focus=m1..m1"));

    let read = run(
        home.path(),
        config.path(),
        &["agent", "read", &format!("{reference}:m1")],
    );
    assert!(read.success);
    assert!(read.stdout.contains("partial transcript needle"));

    let outline = run(
        home.path(),
        config.path(),
        &["agent", "outline", &reference],
    );

    assert!(outline.success);
    assert!(outline.stdout.contains("warnings=1"));
    assert!(outline.stdout.contains("kind=malformed-transcript"));
    assert!(outline.stdout.contains("m1 role=user"));
    assert!(outline.stdout.contains("m2 role=assistant"));

    let bounded = run(
        home.path(),
        config.path(),
        &["agent", "read", &reference, "--budget", "180"],
    );
    assert!(bounded.success);
    assert!(bounded.stdout.chars().count() <= 180);
    assert!(bounded.stdout.contains("warnings=1"));
    assert!(bounded.stdout.contains("continue read"));
    assert_eq!(bounded.stdout.lines().count(), 2);
}

#[test]
fn agent_filesystem_failures_use_io_envelope() {
    let (home, config, _) = dirs();
    std::fs::write(config.path().join("projects"), "not a directory").expect("write projects file");

    let output = run(
        home.path(),
        config.path(),
        &["agent", "search", "--lexical", "needle"],
    );
    assert!(!output.success);
    assert!(
        output.stderr.starts_with("protocol agent-error kind=io"),
        "{}",
        output.stderr
    );
}

#[test]
fn search_time_range_narrows_the_corpus_without_reporting_skips() {
    let (home, config, _) = dirs();
    let project = project(config.path());

    let recent = project.join("11111111-1111-4111-9111-111111111111.jsonl");
    let old = project.join("22222222-2222-4222-9222-222222222222.jsonl");
    write_transcript_at(&recent, "time filter needle", "2026-07-20");
    write_transcript_at(&old, "time filter needle", "2020-01-15");
    set_modified(&recent, "202607200000");
    set_modified(&old, "202001150000");

    let unfiltered = run(
        home.path(),
        config.path(),
        &["agent", "search", "--lexical", "time filter needle"],
    );
    assert!(unfiltered.success, "{}", unfiltered.stderr);
    assert!(unfiltered.stdout.contains("uuid=11111111"));
    assert!(unfiltered.stdout.contains("uuid=22222222"));

    let filtered = run(
        home.path(),
        config.path(),
        &[
            "agent",
            "search",
            "--lexical",
            "time filter needle",
            "--since",
            "2026-01-01",
        ],
    );
    assert!(filtered.success, "{}", filtered.stderr);
    assert!(filtered.stdout.contains("uuid=11111111"));
    assert!(
        !filtered.stdout.contains("uuid=22222222"),
        "out-of-window conversation still returned: {}",
        filtered.stdout
    );

    // Key discovery walks the projects directory independently of the time
    // filter, so an unfiltered key list would report every excluded
    // conversation as a skipped transcript and claim partial coverage.
    assert!(
        !filtered.stdout.contains("kind=skipped"),
        "filtered-out conversations were reported as skipped: {}",
        filtered.stdout
    );

    // The converse: narrowing the key list must not hide diagnostics for files
    // that are inside the window but failed to parse, or a filtered search would
    // claim full coverage it does not have.
    let unparseable = project.join("33333333-3333-4333-9333-333333333333.jsonl");
    std::fs::write(&unparseable, "{malformed\n").expect("write malformed transcript");
    set_modified(&unparseable, "202607200000");

    let with_malformed = run(
        home.path(),
        config.path(),
        &[
            "agent",
            "search",
            "--lexical",
            "time filter needle",
            "--since",
            "2026-01-01",
        ],
    );
    assert!(with_malformed.success);
    assert!(
        with_malformed.stdout.contains("kind=malformed-transcript"),
        "in-window malformed transcript was silently dropped: {}",
        with_malformed.stdout
    );
}

#[test]
fn search_rejects_an_inverted_time_range() {
    let (home, config, _) = dirs();
    let transcript = project(config.path()).join("33333333-3333-4333-9333-333333333333.jsonl");
    write_transcript(&transcript, "inverted range needle");

    let output = run(
        home.path(),
        config.path(),
        &[
            "agent",
            "search",
            "--lexical",
            "inverted range needle",
            "--after",
            "2026-07-20",
            "--before",
            "2026-01-01",
        ],
    );

    assert!(!output.success);
    assert!(
        output
            .stderr
            .starts_with("protocol agent-error kind=out-of-range"),
        "{}",
        output.stderr
    );
}

#[test]
fn direct_uuid_and_pi_filename_inputs_preserve_agent_recipes() {
    let (home, config, sessions) = dirs();
    let uuid = "01912345-6789-7abc-8def-0123456789ab";
    let stem = format!("2026-09-08T20-20-22-361Z_{uuid}");
    std::fs::copy(
        fixture("pi/v3-branched.jsonl"),
        sessions.path().join(format!("{stem}.jsonl")),
    )
    .unwrap();
    std::fs::write(
        config.path().join("settings.json"),
        serde_json::json!({"sessionDir": sessions.path()}).to_string(),
    )
    .unwrap();
    // Upstream: CLAUDE_CONFIG_DIR=config, PI_CODING_AGENT_DIR=config, no
    // session-dir/profile variables, cwd=config (now the workspace).
    let env = HistoryEnv::sandbox(home.path())
        .with_claude_config_dir(config.path())
        .with_pi_coding_agent_dir(config.path())
        .with_workspace(config.path());
    let invoke = |args: &[&str]| {
        let output = run_env(&env, args);
        assert!(output.success, "{}", output.stderr);
        output.stdout
    };
    for identity in [uuid.to_owned(), stem.clone(), format!("{stem}.jsonl")] {
        let outline = invoke(&["agent", "outline", &identity]);
        assert!(outline.contains("active root question"));
        let read = invoke(&[
            "agent",
            "read",
            &format!("{identity}:m1..m2"),
            "--focus",
            &format!("{uuid}:m1"),
        ]);
        assert!(read.contains("active root question"));
        assert!(!read.contains("ABANDONED_BRANCH_SENTINEL"));
        let within = invoke(&[
            "agent",
            "within",
            &identity,
            "active root question",
            "--exact",
        ]);
        let handle = first_ref(&within);
        let opaque = invoke(&["agent", "outline", &handle]);
        assert_eq!(outline, opaque);
        let anchor = within
            .split_whitespace()
            .find_map(|field| field.strip_prefix("anchors="))
            .unwrap()
            .split(',')
            .next()
            .unwrap();
        let anchored = invoke(&["agent", "read", &identity, "--anchor", anchor]);
        assert!(anchored.contains("active root question"));
    }
}

#[test]
fn duplicate_uuid_cli_errors_offer_resolvable_handles() {
    let (home, config, _) = dirs();
    let uuid = "12345678-1234-4234-9234-123456789abc";
    for name in ["-tmp-first", "-tmp-second"] {
        let dir = config.path().join("projects").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        write_transcript(&dir.join(format!("{uuid}.jsonl")), name);
    }
    for args in [
        vec!["agent", "outline", uuid],
        vec!["agent", "read", uuid],
        vec!["agent", "within", uuid, "answer"],
    ] {
        let output = run(home.path(), config.path(), &args);
        assert!(!output.success);
        let error = &output.stderr;
        assert!(error.contains("kind=ambiguous-ref"), "{error}");
        assert!(error.contains("retry%20with"));
        assert!(error.contains("-tmp-first"));
        assert!(error.contains("-tmp-second"));
    }
    let missing = run(
        home.path(),
        config.path(),
        &["agent", "outline", "00000000-0000-0000-0000-000000000000"],
    );
    assert!(!missing.success);
    assert!(missing.stderr.contains("kind=not-found"));
}

#[test]
fn uuid_qualified_focus_matches_handle_focus_across_conversations() {
    let (home, config, _) = dirs();
    let dir = project(config.path());
    let first = "12345678-1234-4234-9234-123456789abc";
    let second = "87654321-1234-4234-9234-123456789abc";
    for uuid in [first, second] {
        write_transcript(
            &dir.join(format!("{uuid}.jsonl")),
            &"bounded evidence ".repeat(100),
        );
    }
    let outline = run(home.path(), config.path(), &["agent", "outline", second]);
    assert!(outline.success);
    let handle = first_ref(&outline.stdout);
    let mut results = Vec::new();
    for focus in [format!("{second}:m1"), format!("{handle}:m1")] {
        let output = run(
            home.path(),
            config.path(),
            &[
                "agent", "read", first, second, "--focus", &focus, "--budget", "1200",
            ],
        );
        assert!(output.success, "{}", output.stderr);
        results.push(output.stdout);
    }
    assert_eq!(results[0], results[1]);
}

#[test]
fn lexical_and_exact_search_omit_semantic_breakdowns() {
    let (home, config, _) = dirs();
    write_transcript(
        &project(config.path()).join("12345678-1234-4234-9234-123456789abc.jsonl"),
        "cache warming",
    );
    for mode in ["lexical", "exact"] {
        let output = run(
            home.path(),
            config.path(),
            &["agent", "search", "--mode", mode, "cache warming"],
        );
        assert!(output.success, "{}", output.stderr);
        assert!(output.stdout.contains("hit project="));
        for atom in [" hybrid=", " semantic=", " lexical="] {
            assert!(!output.stdout.contains(atom), "{}", output.stdout);
        }
    }
}

/// Upstream dcd3978: `agent search --user-only` takes hits from what the
/// person typed, and semantic or hybrid modes search lexically instead.
#[test]
fn user_only_search_finds_the_prompt_and_falls_back_from_semantic() {
    let (home, config, _) = dirs();
    let project = project(config.path());
    let user = serde_json::json!({
        "type": "user",
        "timestamp": "2026-07-20T00:00:00Z",
        "cwd": "/tmp/agent-phase3-tests",
        "message": {"role": "user", "content": "walrusprompt question"}
    });
    let assistant = serde_json::json!({
        "type": "assistant",
        "timestamp": "2026-07-20T00:00:01Z",
        "message": {"role": "assistant", "content": [{"type": "text", "text": "sealreply answer"}]}
    });
    std::fs::write(
        project.join("12345678-1234-4234-9234-123456789abc.jsonl"),
        format!("{user}\n{assistant}\n"),
    )
    .expect("write transcript");

    let found = run(
        home.path(),
        config.path(),
        &["agent", "search", "--user-only", "walrusprompt"],
    );
    assert!(found.success, "{}", found.stderr);
    assert!(found.stdout.contains("hit project="), "{}", found.stdout);
    let missed = run(
        home.path(),
        config.path(),
        &["agent", "search", "--user-only", "sealreply"],
    );
    assert!(missed.success, "{}", missed.stderr);
    assert!(!missed.stdout.contains("hit project="), "{}", missed.stdout);
    let both = run(
        home.path(),
        config.path(),
        &["agent", "search", "sealreply"],
    );
    assert!(both.stdout.contains("hit project="), "{}", both.stdout);

    let semantic = run(
        home.path(),
        config.path(),
        &[
            "agent",
            "search",
            "--user-only",
            "--mode",
            "semantic",
            "walrusprompt",
        ],
    );
    assert!(semantic.success, "{}", semantic.stderr);
    assert!(
        semantic.stdout.contains("hit project="),
        "{}",
        semantic.stdout
    );
    let text = format!("{}{}", semantic.stdout, semantic.stderr);
    assert!(
        text.contains("kind=semantic-unavailable") && text.contains("your%20own%20messages"),
        "{text}"
    );
}

// --- additions: hermeticity of the scenarios themselves ----------------------

#[test]
fn scenario_caches_stay_inside_the_sandbox_home() {
    let (home, config, sessions) = dirs();
    std::fs::copy(
        fixture("pi/v3-branched.jsonl"),
        sessions.path().join("pi.jsonl"),
    )
    .unwrap();
    write_transcript(
        &project(config.path()).join("12345678-1234-4234-9234-123456789abc.jsonl"),
        "sandboxed cache needle",
    );
    let env = sandbox(home.path(), config.path(), sessions.path());
    let output = run_env(
        &env,
        &["agent", "search", "--lexical", "sandboxed cache needle"],
    );
    assert!(output.success, "{}", output.stderr);

    let cache_root = env.cache_root().unwrap();
    assert!(cache_root.starts_with(home.path()));
    // Agent search loads from the store (upstream 16cc49c): its pass
    // published a generation under the sandbox's cache root.
    let store = crate::store::default_root(&env).unwrap();
    assert!(store.starts_with(&cache_root), "{store:?}");
    assert!(
        store.join("CURRENT").is_file(),
        "store written under the sandbox: {store:?}"
    );
}

#[test]
fn invalid_arguments_become_protocol_errors() {
    let (home, config, _) = dirs();
    project(config.path());
    let env = sandbox(home.path(), config.path(), &config.path().join("none"));
    let mut service = AgentService::new(env, ConfigFile::default());
    let error = service
        .execute(AgentRequest::Search(AgentSearchArgs {
            top: Some(0),
            ..AgentSearchArgs::new("needle")
        }))
        .unwrap_err();
    let AppError::AgentProtocol(text) = error else {
        panic!("expected a protocol envelope");
    };
    assert!(text.starts_with("protocol agent-error kind=invalid-ref ref=command"));
    assert!(text.contains("greater%20than%20zero"), "{text}");
}

// --- [tui] exclude_paths (upstream 982aea9) ----------------------------------

/// Upstream `write_tui_config`: the shared `config.toml` of the sandbox
/// home, read by [`run_configured`].
fn write_tui_config(env: &HistoryEnv, body: &str) -> PathBuf {
    let file = env.config_file().expect("sandbox config file");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, body).unwrap();
    file
}

/// Upstream `shared_exclude_paths_filters_search_without_skips_and_preserves_explicit_access`.
/// The `--render` half (the terminal display, not core) is left out like
/// the other `direct_render_*` parts: a file path opens in the GUI's
/// single-file mode, which has no exclusions (`browser::tests`).
#[test]
fn shared_exclude_paths_filters_search_without_skips_and_preserves_explicit_access() {
    let (home, config, _) = dirs();
    let env = sandbox(
        home.path(),
        config.path(),
        &config.path().join("empty-agent-sessions"),
    );
    let project = project(config.path());
    let hidden_id = "11111111-1111-4111-8111-111111111111";
    let visible_id = "22222222-2222-4222-8222-222222222222";
    let missing_id = "33333333-3333-4333-8333-333333333333";
    let hidden = project.join(format!("{hidden_id}.jsonl"));
    let visible = project.join(format!("{visible_id}.jsonl"));
    let missing = project.join(format!("{missing_id}.jsonl"));
    for path in [&hidden, &visible, &missing] {
        write_transcript(path, "path exclusion needle");
    }
    let text = std::fs::read_to_string(&visible).unwrap();
    std::fs::write(
        &visible,
        text.replace("/tmp/agent-phase3-tests", "/work/real-project"),
    )
    .unwrap();
    let text = std::fs::read_to_string(&missing).unwrap();
    let without_cwd = text.replace(r#""cwd":"/tmp/agent-phase3-tests","#, "");
    assert_ne!(without_cwd, text, "the transcript records a cwd");
    std::fs::write(&missing, without_cwd).unwrap();
    let unfiltered = run_configured(&env, &["agent", "within", hidden_id, "needle", "--lexical"]);
    assert!(unfiltered.success, "{}", unfiltered.stderr);
    let reference = first_ref(&unfiltered.stdout);
    let config_file = write_tui_config(
        &env,
        "[tui]\nexclude_paths = [\"/tmp/\", \"/private/tmp/\"]\n",
    );

    // Repeated invocations exercise both a freshly published and an
    // unchanged store generation.
    for mode in ["--lexical", "--exact", "--lexical"] {
        let output = run_configured(&env, &["agent", "search", mode, "path exclusion needle"]);
        assert!(output.success, "{}", output.stderr);
        let text = &output.stdout;
        assert!(!text.contains(hidden_id), "{text}");
        assert!(text.contains(visible_id), "{text}");
        assert!(text.contains(missing_id), "{text}");
        assert!(!text.contains("kind=skipped"), "{text}");
    }
    for args in [
        vec!["agent", "read", reference.as_str()],
        vec!["agent", "outline", hidden_id],
        vec!["agent", "within", hidden_id, "needle", "--lexical"],
    ] {
        let output = run_configured(&env, &args);
        assert!(output.success, "{}", output.stderr);
        assert!(
            output.stdout.contains("path exclusion needle"),
            "{}",
            output.stdout
        );
    }
    assert!(hidden.exists());

    std::fs::write(
        project.join("44444444-4444-4444-8444-444444444444.jsonl"),
        "{malformed\n",
    )
    .unwrap();
    let output = run_configured(&env, &["agent", "search", "--lexical", "needle"]);
    assert!(output.success, "{}", output.stderr);
    assert!(
        output.stdout.contains("kind=malformed-transcript"),
        "{}",
        output.stdout
    );
    std::fs::remove_file(config_file).unwrap();
    let restored = run_configured(&env, &["agent", "search", "--lexical", "needle"]);
    assert!(restored.success, "{}", restored.stderr);
    assert!(restored.stdout.contains(hidden_id), "{}", restored.stdout);
}

/// Upstream `shared_exclude_paths_filters_pi_and_omp_header_cwd`.
#[test]
fn shared_exclude_paths_filters_pi_and_omp_header_cwd() {
    for (fixture_name, uuid, query) in [
        (
            "pi/v3-branched.jsonl",
            "01912345-6789-7abc-8def-0123456789ab",
            "active root question",
        ),
        (
            "omp/v3.jsonl",
            "omp_session_custom_id",
            "OMP active question",
        ),
    ] {
        let (home, config, sessions) = dirs();
        let env = sandbox(home.path(), config.path(), sessions.path());
        std::fs::copy(fixture(fixture_name), sessions.path().join("session.jsonl")).unwrap();
        let config_file = write_tui_config(&env, "[tui]\nexclude_paths = [\"/tmp/\"]\n");
        for _ in 0..2 {
            let output = run_configured(&env, &["agent", "search", "--lexical", query]);
            assert!(output.success, "{fixture_name}: {}", output.stderr);
            let text = &output.stdout;
            assert!(!text.contains(uuid), "{fixture_name}: {text}");
            assert!(!text.contains("kind=skipped"), "{fixture_name}: {text}");
        }
        std::fs::remove_file(&config_file).unwrap();
        let restored = run_configured(&env, &["agent", "search", "--lexical", query]);
        assert!(restored.success, "{fixture_name}: {}", restored.stderr);
        assert!(
            restored.stdout.contains(uuid),
            "{fixture_name}: {}",
            restored.stdout
        );
        let reference = first_ref(&restored.stdout);
        write_tui_config(&env, "[tui]\nexclude_paths = [\"/tmp/\"]\n");
        let direct = run_configured(&env, &["agent", "outline", &reference]);
        assert!(direct.success, "{fixture_name}: {}", direct.stderr);
        assert!(
            direct.stdout.contains(query),
            "{fixture_name}: {}",
            direct.stdout
        );
    }
}

/// Upstream `shared_exclude_paths_filters_codex_threads`.
#[test]
fn shared_exclude_paths_filters_codex_threads() {
    const FORK: &str = "01a00000-0000-7000-8000-000000000003";
    let (home, config, sessions) = dirs();
    let codex = crate::history::sources::codex::tests::home_copy(home.path());
    let env = sandbox(home.path(), config.path(), sessions.path()).with_codex_home(&codex);
    let claude_id = "11111111-1111-4111-8111-111111111111";
    write_transcript(
        &project(config.path()).join(format!("{claude_id}.jsonl")),
        "ocelot example",
    );
    let search = run_configured(&env, &["agent", "search", "--lexical", "ocelot example"]);
    assert!(search.success, "{}", search.stderr);
    assert!(
        search.stdout.contains(&format!("uuid={FORK}")),
        "{}",
        search.stdout
    );
    let reference = first_ref(&search.stdout);

    // The fixture threads all run in /work/demo.
    write_tui_config(&env, "[tui]\nexclude_paths = [\"/work/\"]\n");
    let hidden = run_configured(&env, &["agent", "search", "--lexical", "ocelot example"]);
    assert!(hidden.success, "{}", hidden.stderr);
    let text = &hidden.stdout;
    assert!(!text.contains(FORK), "{text}");
    assert!(text.contains(claude_id), "{text}");
    assert!(!text.contains("kind=skipped"), "{text}");
    let read = run_configured(&env, &["agent", "read", &reference]);
    assert!(read.success, "{}", read.stderr);
    assert!(read.stdout.contains("ocelot example"), "{}", read.stdout);
}
