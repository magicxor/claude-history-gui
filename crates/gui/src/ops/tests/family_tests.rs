//! G tests of a Claude session's subagents and `/btw` forks (upstream
//! 3c2ffeb; V-106, V-107, A-070): the viewer header names them, `s` picks
//! one, `P` opens the parent, Esc and `q` walk back through the
//! conversations opened before the viewer is left; a fork cannot be forked
//! or renamed, resuming it resumes its session (refused once that is gone),
//! deleting it removes only its files, and deleting the session keeps it.
//! Nothing ever runs: the terminal launches are recorded.

use std::path::{Path, PathBuf};

use chrono::{Duration, Utc};
use claude_history_core::history::model::SourceKind;
use claude_history_core::maintenance::plan_session_delete;
use gpui_kit::{AppContext as _, Entity, TestAppContext};

use super::{has_dialog, toast};
use crate::actions::Fork;
use crate::list::context_menu;
use crate::ops::delete;
use crate::test_support::TestWorld;
use crate::test_support::fixtures_list::set_mtime;
use crate::test_support::fixtures_ops::{
    OpsFixture, claude_session, project_dir, write_gui_config,
};
use crate::test_support::fixtures_viewer::wait_viewer_ready;
use crate::viewer::ViewerPane;
use crate::viewer::family::{self, LastFamilyMenu};

/// A sidecar `agent-<agent>.jsonl` of `session` (working directory `cwd`)
/// with `meta` beside it, last active `minutes_ago`.
fn sidecar(
    session: &Path,
    cwd: &Path,
    agent: &str,
    meta: &str,
    text: &str,
    minutes_ago: i64,
) -> PathBuf {
    let uuid = session.file_stem().unwrap().to_string_lossy().into_owned();
    let dir = session.parent().unwrap().join(uuid).join("subagents");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("agent-{agent}.jsonl"));
    let lines = [
        serde_json::json!({
            "type": "user",
            "cwd": cwd.display().to_string(),
            "timestamp": "2026-01-01T00:00:00Z",
            "message": {"role": "user", "content": text},
        }),
        serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-01-01T00:01:00Z",
            "message": {
                "role": "assistant",
                "model": "claude-opus-4-5",
                "content": [{"type": "text", "text": "done"}],
            },
        }),
    ];
    let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
    std::fs::write(&path, text).unwrap();
    std::fs::write(path.with_extension("meta.json"), meta).unwrap();
    set_mtime(&path, Utc::now() - Duration::minutes(minutes_ago));
    path
}

struct Family {
    session: PathBuf,
    subagent: PathBuf,
    fork: PathBuf,
}

/// A session with a subagent (older) and a `/btw` fork (newer).
fn family(world: &TestWorld) -> Family {
    let session = claude_session(world, "family", 1, "Walrus feeding plan");
    let cwd = project_dir(world, "family");
    let subagent = sidecar(
        &session,
        &cwd,
        "a1",
        r#"{"agentType":"Explore","description":"Find the walrus","toolUseId":"toolu_1"}"#,
        "explore the enclosure",
        30,
    );
    let fork = sidecar(
        &session,
        &cwd,
        "afork-2",
        r#"{"agentType":"fork","isFork":true,"description":"Ask the narwhal"}"#,
        "is the narwhal awake?",
        20,
    );
    Family {
        session,
        subagent,
        fork,
    }
}

fn shown(cx: &TestAppContext, viewer: &Entity<ViewerPane>) -> Option<PathBuf> {
    cx.read(|cx| viewer.read(cx).target().map(|target| target.path.clone()))
}

fn probe(cx: &TestAppContext, viewer: &Entity<ViewerPane>, key: &str) -> String {
    cx.read(|cx| viewer.read(cx).qa_probe(key, cx).unwrap_or_default())
}

/// Press `keys` in the viewer and wait for what it opens.
fn press(f: &OpsFixture, cx: &mut TestAppContext, viewer: &Entity<ViewerPane>, keys: &str) {
    f.press(cx, keys);
    wait_viewer_ready(cx, f.handle(), viewer);
    f.settle(cx);
}

/// V-106, V-107: the family line, the picker, the parent and the walk back.
#[gpui_kit::test]
fn the_viewer_opens_subagents_and_forks_and_returns_to_their_session(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let family = family(&world);
    let f = OpsFixture::open(cx, &world, &[]);
    // The subagent is hidden from the list; the fork is listed, tagged.
    let listed = f.list.paths(cx);
    assert!(listed.contains(&family.fork), "{listed:?}");
    assert!(!listed.contains(&family.subagent), "{listed:?}");
    let fork_target = f.target(cx, &family.fork);
    assert!(fork_target.provenance.branch);

    let viewer = f.open_in_viewer(cx, &family.session);
    assert_eq!(probe(cx, &viewer, "viewer.family"), "1 subagent, 1 fork");

    // `s` lists the subagent first (oldest), then the fork.
    press(&f, cx, &viewer, "s");
    assert!(has_dialog(cx, f.handle()));
    let menu = cx
        .read(|cx| cx.global::<LastFamilyMenu>().0.upgrade())
        .expect("the picker is open");
    let rows: Vec<(PathBuf, &str, String)> = cx.read(|cx| {
        menu.read(cx)
            .children()
            .iter()
            .map(|child| {
                (
                    child.target.path.clone(),
                    family::kind_tag(child),
                    child.label.clone(),
                )
            })
            .collect()
    });
    assert_eq!(
        rows,
        vec![
            (
                family.subagent.clone(),
                "sub",
                "Explore: Find the walrus".to_string()
            ),
            (family.fork.clone(), "fork", "Ask the narwhal".to_string()),
        ]
    );
    press(&f, cx, &viewer, "j");
    press(&f, cx, &viewer, "enter");
    assert!(!has_dialog(cx, f.handle()));
    assert_eq!(shown(cx, &viewer), Some(family.fork.clone()));
    assert_eq!(
        probe(cx, &viewer, "viewer.family"),
        "↳ fork of Walrus feeding plan"
    );
    assert_eq!(probe(cx, &viewer, "viewer.back"), "1");
    // The list keeps its selection.
    assert_ne!(f.list.selected_path(cx), Some(family.fork.clone()));

    // Actions apply to the conversation in the viewer: a fork is not forked.
    cx.update_window(f.handle(), |_, window, cx| {
        window.dispatch_action(Box::new(Fork), cx)
    })
    .unwrap();
    f.settle(cx);
    assert_eq!(toast(cx), "Forking CC subagents and forks is not supported");

    // `P` opens the parent; Esc walks back, then leaves.
    press(&f, cx, &viewer, "shift-p");
    assert_eq!(shown(cx, &viewer), Some(family.session.clone()));
    assert_eq!(probe(cx, &viewer, "viewer.back"), "2");
    press(&f, cx, &viewer, "escape");
    assert_eq!(shown(cx, &viewer), Some(family.fork.clone()));
    press(&f, cx, &viewer, "q");
    assert_eq!(shown(cx, &viewer), Some(family.session.clone()));
    assert_eq!(probe(cx, &viewer, "viewer.back"), "0");
    press(&f, cx, &viewer, "escape");
    let left = cx.update_window(f.handle(), |_, window, cx| {
        f.list_pane().read(cx).contains_focus(window, cx)
    });
    assert!(
        left.unwrap(),
        "Esc with nothing to return to leaves the viewer"
    );

    // A conversation opened from the list forgets the walk.
    let viewer = f.open_in_viewer(cx, &family.fork);
    assert_eq!(probe(cx, &viewer, "viewer.back"), "0");

    // Also when the list opens the conversation the viewer already shows.
    let viewer = f.open_in_viewer(cx, &family.session);
    press(&f, cx, &viewer, "s");
    press(&f, cx, &viewer, "j");
    press(&f, cx, &viewer, "enter");
    assert_eq!(shown(cx, &viewer), Some(family.fork.clone()));
    assert_eq!(probe(cx, &viewer, "viewer.back"), "1");
    let viewer = f.open_in_viewer(cx, &family.fork);
    assert_eq!(shown(cx, &viewer), Some(family.fork.clone()));
    assert_eq!(probe(cx, &viewer, "viewer.back"), "0");
}

/// V-106: a conversation without relatives says so.
#[gpui_kit::test]
fn a_session_without_relatives_says_so(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let lonely = claude_session(&world, "lonely", 2, "Lonely session");
    let f = OpsFixture::open(cx, &world, &[]);
    let viewer = f.open_in_viewer(cx, &lonely);
    assert_eq!(probe(cx, &viewer, "viewer.family"), "");
    press(&f, cx, &viewer, "s");
    assert!(!has_dialog(cx, f.handle()));
    assert_eq!(toast(cx), family::NO_CHILDREN);
    press(&f, cx, &viewer, "shift-p");
    assert_eq!(toast(cx), family::NO_PARENT);
    assert_eq!(shown(cx, &viewer), Some(lonely));
}

/// A-070: a fork resumes its session and is never forked or renamed; once
/// the session is gone, resuming it is refused. Deleting it removes its
/// transcript and metadata only; deleting the session keeps it.
#[gpui_kit::test]
fn sidecars_resume_their_session_and_delete_alone(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    write_gui_config(&world, "", "");
    let family = family(&world);
    let f = OpsFixture::open(cx, &world, &[]);
    let target = f.target(cx, &family.fork);

    let menu = cx.read(|cx| context_menu::build(1, &target, false, true, cx));
    assert!(menu.is_disabled("Fork") && menu.is_disabled("Rename…"));
    assert!(!menu.is_disabled("Resume") && !menu.is_disabled("Delete…"));

    // Resume runs `claude --resume <session id>`.
    f.recording.clear_calls();
    f.select(cx, &family.fork);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    let commands = f.terminal_commands();
    let uuid = family.session.file_stem().unwrap().to_string_lossy();
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert!(
        commands[0].contains(&format!("--resume {uuid}")),
        "{}",
        commands[0]
    );

    // The plans: the fork alone with its metadata; the session keeps it.
    let env = cx.read(|cx| crate::globals::app_state(cx).env_with_workspace());
    let plan = plan_session_delete(&env, &family.fork, SourceKind::Claude).unwrap();
    let items: Vec<PathBuf> = delete::plan_items(&plan)
        .into_iter()
        .map(|item| item.path)
        .collect();
    assert_eq!(
        items,
        vec![family.fork.clone(), family.fork.with_extension("meta.json")]
    );
    let plan = plan_session_delete(&env, &family.session, SourceKind::Claude).unwrap();
    assert_eq!(
        delete::consequence_text(&plan),
        "Its /btw fork stays, a conversation of its own. This can't be undone."
    );

    // Without its session the fork has nothing to resume.
    std::fs::remove_file(&family.session).unwrap();
    f.recording.clear_calls();
    f.select(cx, &family.fork);
    f.press(cx, "ctrl-r");
    f.settle(cx);
    assert!(f.terminal_commands().is_empty());
    assert!(toast(cx).contains("no longer exists"), "{}", toast(cx));
}
