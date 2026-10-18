//! Session-operation fixtures (DESIGN §6, §15.2): two-copy UUID,
//! cross-project fork, Pi/OMP resume, fake programs. Owned by WP3.
//!
//! Builders use `super::TestWorld` and `super::synth`; GUI-only fixture
//! files go into `crates/gui/tests/fixtures/ops/`.
//!
//! * [`write_gui_config`]: `gui.toml` with `[gui] claude_path|pi_path|omp_path`
//!   pointing at fake programs (so a resume resolves without `PATH`), plus
//!   extra tables;
//! * session builders for the resume decisions: [`claude_session`] (its
//!   project directory exists: resume runs in place), [`claude_session_gone`]
//!   (the project directory is missing), [`two_copies`] (one UUID in two
//!   projects, with session folders), [`pi_session`], [`omp_session`];
//! * [`OpsFixture`]: the main window over a world with the list loaded and
//!   helpers to select a row by path, open a row in the viewer with its real
//!   target, and read the recorded calls.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext};

use crate::list::ListPane;
use crate::list::ListPaneEvent;
use crate::model::ConversationTarget;
use crate::services::{RecordingServices, ServiceCall};
use crate::viewer::ViewerPane;
use crate::workspace::Workspace;

use super::fixtures_list::{ListFixture, SessionSpec};
use super::fixtures_viewer::wait_viewer_ready;
use super::{TestWorld, recording, synth};

/// A fake program the resume can resolve: `<home>/bin/fake-<name>.cmd`
/// (Windows) or an executable `fake-<name>` script (Unix). Never run: tests
/// only record the terminal commands.
pub fn fake_program(world: &TestWorld, name: &str) -> PathBuf {
    let dir = world.home().join("bin");
    std::fs::create_dir_all(&dir).expect("create the bin dir");
    let path = if cfg!(windows) {
        dir.join(format!("fake-{name}.cmd"))
    } else {
        dir.join(format!("fake-{name}"))
    };
    let body = if cfg!(windows) {
        "@echo off\r\n"
    } else {
        "#!/bin/sh\nexit 0\n"
    };
    std::fs::write(&path, body).expect("write the fake program");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("make the fake program executable");
    }
    path
}

/// Quote `path` as a TOML literal string.
pub fn toml_path(path: &Path) -> String {
    format!("'{}'", path.display())
}

/// Write `gui.toml` with `[gui] claude_path|pi_path|omp_path` set to fake
/// programs and `extra` appended (other tables, or more `[gui]` keys must go
/// into `gui_extra`, which is placed inside the `[gui]` table).
pub fn write_gui_config(world: &TestWorld, gui_extra: &str, extra: &str) {
    let claude = fake_program(world, "claude");
    let pi = fake_program(world, "pi");
    let omp = fake_program(world, "omp");
    // Linux detects a terminal emulator on PATH and the test world's PATH has
    // none: name one so a resume reaches the recorded terminal call. macOS
    // always detects two (Terminal.app, then osascript): name the first, so a
    // launch is one call there too, as on Windows. Nothing is spawned.
    let terminal = if gui_extra.contains("terminal") {
        ""
    } else if cfg!(target_os = "linux") {
        "terminal = \"xterm\"\n"
    } else if cfg!(target_os = "macos") {
        "terminal = \"terminal\"\n"
    } else {
        ""
    };
    let text = format!(
        "[gui]\nclaude_path = {}\npi_path = {}\nomp_path = {}\n{terminal}{gui_extra}\n{extra}\n",
        toml_path(&claude),
        toml_path(&pi),
        toml_path(&omp),
    );
    world.write_config(None, Some(&text));
}

/// Create (if needed) and return the project folder `<home>/work/<name>`.
pub fn project_dir(world: &TestWorld, name: &str) -> PathBuf {
    let dir = world.home().join("work").join(name);
    std::fs::create_dir_all(&dir).expect("create the project folder");
    dir
}

/// A Claude session whose working directory is the existing folder `cwd`,
/// stored in that folder's projects directory. `number` makes the UUID.
pub fn claude_session_in(world: &TestWorld, cwd: &Path, number: u32, summary: &str) -> PathBuf {
    SessionSpec::new("ops", number)
        .cwd(cwd.to_path_buf())
        .summary(summary)
        .write(world)
}

/// A Claude session in the existing project `<home>/work/<project>`.
pub fn claude_session(world: &TestWorld, project: &str, number: u32, summary: &str) -> PathBuf {
    let cwd = project_dir(world, project);
    claude_session_in(world, &cwd, number, summary)
}

/// A Claude session with a Bash tool call and its result between a question
/// and an answer (export options, message copy and the summary group).
pub fn tool_session(world: &TestWorld, project: &str, number: u32) -> PathBuf {
    use super::fixtures_viewer::{assistant_at, bash_call, tool_result, user_at};
    let cwd = project_dir(world, project);
    let uuid = synth::uuid(number);
    let lines = [
        user_at("Run the tests", 0),
        bash_call("toolu_1", "cargo test", 1),
        tool_result("toolu_1", "all green", 2),
        assistant_at("All tests pass", 3),
    ];
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    world.add_claude_session(&synth::encoded_project_dir(&cwd), &uuid, &refs)
}

/// A Claude session whose recorded working directory no longer exists.
pub fn claude_session_gone(world: &TestWorld, number: u32, summary: &str) -> PathBuf {
    let cwd = world
        .home()
        .join("work")
        .join("gone")
        .join(format!("p{number}"));
    SessionSpec::new("gone", number)
        .cwd(cwd)
        .summary(summary)
        .write(world)
}

/// The two copies of one session UUID (a cross-project fork), each with a
/// session folder.
pub struct TwoCopies {
    pub uuid: String,
    pub first: PathBuf,
    pub second: PathBuf,
    pub first_dir: PathBuf,
    pub second_dir: PathBuf,
}

/// One UUID in the projects `a` and `b` (summaries `Copy in a` / `Copy in b`),
/// each with `<uuid>/tool-results/result.txt`.
pub fn two_copies(world: &TestWorld, number: u32) -> TwoCopies {
    let cwd_a = project_dir(world, "copy-a");
    let cwd_b = project_dir(world, "copy-b");
    let uuid = synth::uuid(number);
    let mut paths = Vec::new();
    for (cwd, summary) in [(&cwd_a, "Copy in a"), (&cwd_b, "Copy in b")] {
        let path = claude_session_in(world, cwd, number, summary);
        let dir = path.parent().expect("a project dir").join(&uuid);
        std::fs::create_dir_all(dir.join("tool-results")).expect("create the session folder");
        std::fs::write(dir.join("tool-results").join("result.txt"), "output\n")
            .expect("write a tool result");
        paths.push((path, dir));
    }
    let (second, second_dir) = paths.pop().expect("two copies");
    let (first, first_dir) = paths.pop().expect("two copies");
    TwoCopies {
        uuid,
        first,
        second,
        first_dir,
        second_dir,
    }
}

fn json_inner(text: &str) -> String {
    let quoted = serde_json::to_string(text).expect("strings serialize");
    quoted[1..quoted.len() - 1].to_string()
}

/// A Pi session in `.pi/agent/sessions/--ops--/` recording `cwd`.
pub fn pi_session(world: &TestWorld, cwd: &Path) -> PathBuf {
    let text =
        synth::PI_SESSION.replace("/tmp/pi-project", &json_inner(&cwd.display().to_string()));
    world.add_fixture(
        ".pi/agent/sessions/--ops--/2024-03-02T00-00-00-000Z_01912345-6789-7abc-8def-0123456789ab.jsonl",
        text.as_bytes(),
    )
}

/// An OMP session (title `OMP fixture title`) recording `cwd`.
pub fn omp_session(world: &TestWorld, cwd: &Path) -> PathBuf {
    let text =
        synth::OMP_SESSION.replace("/tmp/omp-project", &json_inner(&cwd.display().to_string()));
    world.add_fixture(
        ".omp/agent/sessions/--ops--/2026-01-02T03-04-05-000Z_omp_session_custom_id.jsonl",
        text.as_bytes(),
    )
}

/// A file that is not a conversation (single-file windows get an unknown source).
pub fn unparsable_file(world: &TestWorld) -> PathBuf {
    world.add_fixture("unparsable/not-a-conversation.jsonl", b"this is not json\n")
}

/// The main window over a world, loaded, with helpers for the ops tests.
pub struct OpsFixture {
    pub list: ListFixture,
    pub recording: Arc<RecordingServices>,
}

impl OpsFixture {
    /// Open the main window on `world` with `args` and wait for the load.
    pub fn open(cx: &mut TestAppContext, world: &TestWorld, args: &[&str]) -> Self {
        let list = ListFixture::open_loaded(cx, world, args);
        let recording = recording(cx);
        Self { list, recording }
    }

    pub fn handle(&self) -> AnyWindowHandle {
        self.list.handle
    }

    pub fn ws(&self) -> &Entity<Workspace> {
        &self.list.ws
    }

    pub fn list_pane(&self) -> &Entity<ListPane> {
        &self.list.list
    }

    /// The captured target of the listed conversation at `path`.
    pub fn target(&self, cx: &TestAppContext, path: &Path) -> ConversationTarget {
        self.list
            .read(cx, |list, _| list.conversation(path))
            .map(|conversation| ConversationTarget::from_conversation(&conversation))
            .unwrap_or_else(|| panic!("{} is not listed", path.display()))
    }

    /// Select the row of `path` in the list.
    pub fn select(&self, cx: &mut TestAppContext, path: &Path) {
        let selected = self
            .list
            .list
            .update(cx, |list, cx| list.select_path(path, cx));
        assert!(selected, "{} is not listed", path.display());
        cx.run_until_parked();
        self.focus_list(cx);
    }

    /// Focus the list's search input (where `ctrl-r` & co. are bound).
    pub fn focus_list(&self, cx: &mut TestAppContext) {
        cx.update_window(self.handle(), |_, window, cx| {
            self.list
                .list
                .update(cx, |list, cx| list.focus_search(window, cx))
        })
        .expect("the window is open");
        cx.run_until_parked();
    }

    /// Open `path` in the main window's viewer with its real target (source
    /// from the list) and focus the viewer.
    pub fn open_in_viewer(&self, cx: &mut TestAppContext, path: &Path) -> Entity<ViewerPane> {
        let target = self.target(cx, path);
        self.list.list.update(cx, |_, cx| {
            cx.emit(ListPaneEvent::OpenRequested {
                target,
                focus_viewer: true,
                agent_tab: None,
            })
        });
        let viewer = cx.read(|cx| self.list.ws.read(cx).viewer().clone());
        wait_viewer_ready(cx, self.handle(), &viewer);
        viewer
    }

    /// Press keys in the window.
    pub fn press(&self, cx: &mut TestAppContext, keys: &str) {
        cx.simulate_keystrokes(self.handle(), keys);
        cx.run_until_parked();
    }

    /// Let queued background work and deferred callbacks finish.
    pub fn settle(&self, cx: &mut TestAppContext) {
        for _ in 0..20 {
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        cx.run_until_parked();
    }

    /// Every recorded terminal command's display string; for a launch through
    /// a script (macOS Terminal.app: `open -a Terminal <fresh path>`) the
    /// script, which holds the command line.
    pub fn terminal_commands(&self) -> Vec<String> {
        self.recording
            .terminal_commands()
            .iter()
            .map(|command| match &command.script {
                Some(script) => script.contents.clone(),
                None => command.display(),
            })
            .collect()
    }

    /// The clipboard writes recorded so far.
    pub fn clipboard_writes(&self) -> Vec<String> {
        self.recording
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                ServiceCall::ClipboardWrite(text) => Some(text),
                _ => None,
            })
            .collect()
    }
}
