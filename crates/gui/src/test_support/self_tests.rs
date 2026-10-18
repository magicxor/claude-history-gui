//! Self-tests of the harness (frozen with it): the standard corpus is found
//! by core's loaders with the right sources and paths, the synthetic
//! transcript parses, and the failure fixtures fail and recover.

use claude_history_core::history::{Source, load_all_conversations, process_conversation_file};

use super::{TestWorld, fault, synth};

#[test]
fn standard_corpus_is_loaded_by_core_with_every_source() {
    let world = TestWorld::new();
    let written = world.standard_corpus();
    assert_eq!(written.len(), 10, "6 + empty + malformed Claude, Pi, OMP");
    for path in &written {
        assert!(path.is_file(), "{}", path.display());
    }
    let conversations = load_all_conversations(&world.env, true, None).expect("the corpus loads");
    let by_source = |source: Source| {
        conversations
            .iter()
            .filter(|conversation| conversation.source == source)
            .collect::<Vec<_>>()
    };
    let claude = by_source(Source::Claude);
    let pi = by_source(Source::Pi);
    let omp = by_source(Source::Omp);
    // The 6 titled sessions and the one with a malformed line; the empty
    // transcript has no user message and is not listed.
    assert_eq!(claude.len(), 7, "{claude:#?}");
    for title in [
        "Fix login bug",
        "Deploy pipeline",
        "Token refresh retry",
        "Add API pagination",
        "Rate limit errors",
        "Worktree cleanup",
        "Malformed line",
    ] {
        assert!(
            claude
                .iter()
                .any(|conversation| conversation.summary.as_deref() == Some(title)),
            "{title}"
        );
    }
    let malformed = claude
        .iter()
        .find(|conversation| conversation.summary.as_deref() == Some("Malformed line"))
        .unwrap();
    assert_eq!(malformed.parse_errors.len(), 1);
    assert_eq!(pi.len(), 1, "the Pi session is under the sandbox Pi root");
    assert_eq!(pi[0].path, written[8]);
    assert_eq!(
        omp.len(),
        1,
        "the OMP session is under the sandbox OMP root"
    );
    assert_eq!(omp[0].path, written[9]);
    assert_eq!(omp[0].session_id, "omp_session_custom_id");
    // Three Claude projects, one of them a worktree of another.
    let mut projects: Vec<_> = claude
        .iter()
        .filter_map(|conversation| conversation.project_path.clone())
        .collect();
    projects.sort();
    projects.dedup();
    assert_eq!(projects.len(), 3, "{projects:?}");
}

#[test]
fn synthetic_transcript_parses() {
    let world = TestWorld::new();
    let path = world
        .projects_root()
        .join("C--synthetic-big")
        .join("0badc0de-1c2d-4e5f-8a9b-0c1d2e3f4a5b.jsonl");
    synth::transcript(&path, 40, 3, 50);
    let text = std::fs::read_to_string(&path).unwrap();
    // Every message is one line; each big output adds the tool-result line.
    assert_eq!(text.lines().count(), 40 + 3);
    assert_eq!(text.matches("output line 50\\n").count(), 3);
    let conversation = process_conversation_file(path.clone(), None, None)
        .expect("the transcript parses")
        .expect("the transcript has messages");
    assert!(conversation.parse_errors.is_empty());
    assert_eq!(conversation.source, Source::Claude);
    assert!(
        conversation.message_count >= 40,
        "{}",
        conversation.message_count
    );
}

#[test]
fn synthetic_corpus_writes_one_session_per_conversation() {
    let world = TestWorld::new();
    synth::corpus(&world, 12);
    let conversations = load_all_conversations(&world.env, true, None).unwrap();
    assert_eq!(conversations.len(), 12);
}

#[test]
fn undeletable_blocks_deletion_until_dropped() {
    if fault::running_as_root() {
        eprintln!("skipped: permissions do not apply to root");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("victim.jsonl");
    std::fs::write(&path, "{}\n").unwrap();
    {
        let _guard = fault::undeletable(&path);
        assert!(std::fs::remove_file(&path).is_err(), "deletion fails");
        assert!(path.exists());
    }
    std::fs::remove_file(&path).expect("deletion works after the guard");
}

#[test]
fn unwritable_blocks_writes_until_dropped() {
    if fault::running_as_root() {
        eprintln!("skipped: permissions do not apply to root");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "a = 1\n").unwrap();
    {
        let _guard = fault::unwritable(&path);
        assert!(std::fs::write(&path, "a = 2\n").is_err(), "writing fails");
    }
    std::fs::write(&path, "a = 3\n").expect("writing works after the guard");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a = 3\n");
}

#[test]
fn stale_redirect_roots_are_swept() {
    let parent = tempfile::tempdir().unwrap();
    // A pid far above any real one and the current process.
    let stale = parent.path().join("pid-4294967290");
    let own = parent.path().join(format!("pid-{}", std::process::id()));
    let other = parent.path().join("not-a-root");
    for dir in [&stale, &own, &other] {
        std::fs::create_dir_all(dir.join("home")).unwrap();
    }
    super::sweep_stale_roots(parent.path());
    assert!(
        !stale.exists(),
        "a root of a process that is gone is removed"
    );
    assert!(own.exists(), "the running process's root stays");
    assert!(other.exists(), "other directories are never touched");
}
