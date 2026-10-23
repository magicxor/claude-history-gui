# Notes for coding agents

The specification is [`docs/DESIGN.md`](docs/DESIGN.md); the commands CI runs are in the README
([Development](README.md#development)) and `.github/workflows/ci.yml`. Several agents usually work at
the same time, each in its own git worktree, on one machine: keep builds incremental and narrow, and
leave nothing behind.

## Safety

* Never inject OS input: no SendKeys, SendInput, synthetic mouse or keyboard, no UI automation of the
  desktop. Keystrokes land in whatever window has the focus, possibly a terminal running an agent.
  Drive the app through the gpui test platform or the in-process QA runner; screenshots are
  capture-only (`tools/qa`).
* Do not change system settings or the registry (not even temporarily, e.g. to test theme following),
  do not create links or junctions outside your scratchpad, and do not stop processes you did not
  start.
* Anything that may be committed (fixtures, screenshots, docs, test strings, commit messages) holds
  synthetic data only: no real conversations, project folder names or the local user name. Run the
  app only against a QA sandbox (README: [The sandbox gate](README.md#the-sandbox-gate)).

## Temporary files and Docker objects

* Temporary files go only into your own scratchpad directory (the one your instructions name; without
  one, create `%TEMP%\claude-history-gui-<task>` and use it as such): never into the repo, the home
  folder, a drive root or another agent's scratchpad. This holds for subagents too: each writes only
  into its own scratchpad.
* Subagents return their full report in the final message (structured output), not only as a file:
  writing files may be blocked, and scratchpads are deleted afterwards.
* As soon as subagents have finished and their scratchpads are no longer needed, the orchestrating
  agent deletes those scratchpad directories.
* Give every Docker object you create (container, image, volume, network) a specific name that cannot
  be mistaken for the user's: `claude-history-gui-tmp-<task>-<what>`, never a generic one such as
  `test`, `app`, `dev` or `build`.
* At the end of the work, once the task is completely done, remove every temporary container, image
  and volume you created that is certainly no longer needed. Remove only your own: never run
  `docker system prune`, `docker image prune -a` or `docker volume prune`, which also delete the
  user's. The image and volumes of `tools/dev/linux-test.ps1` (`claude-history-gui-linux-dev`,
  `claude-history-gui-target-*`, `-cargo-*`, `-ort-cache`) are not temporary; see
  [Linux](#linux-docker) for when they go.

## Worktrees and orchestration

* Check the base of every worktree before working in it: it must contain the current `main`
  (`git merge-base --is-ancestor main HEAD`); fast-forward first otherwise. A worktree was once
  created from the initial commit.
* Choose models up front: Opus for research, design, audits, orchestration, integration and genuinely
  hard or performance-critical code; Sonnet for implementing a well-specified package and for deep
  criteria-based analysis of large code; Haiku for mechanical edits and first-pass reconnaissance.
* Running subagents cannot be messaged, and stopping a workflow loses their work. If course
  corrections may be needed, give the subagent a mailbox file to check at its milestones. Restart
  from a clean state rather than asking an agent to continue from whatever is in its working tree.
* Orchestration scripts never drop findings silently (a severity filter once lost real review
  findings): log what is filtered out.

## Building and testing (Windows)

A cold build of the dependency tree (gpui, ONNX Runtime, ~420 crates) takes 7-9 minutes; a rebuild
after an edit takes about 15 s.

* **Fresh worktree:** before the first cargo command, run
  `pwsh -NoProfile -File tools/dev/seed-target.ps1`. It copies the main checkout's compiled
  dependencies into this worktree's `target/`, so only the two workspace crates are compiled. Never
  point several worktrees at one `CARGO_TARGET_DIR`: cargo's build lock serializes them (and
  `cargo test` holds it while the tests run).
* **While editing,** use `cargo check` or `cargo clippy` (for example
  `cargo clippy -p claude-history-gui --all-targets`): no code generation, no linking. Their output
  is not reused by `cargo test`, so run the tests once the change is done, not after every edit.
* **Narrow the build:**
  * a change only in `crates/core`: `cargo test -p claude-history-core` (does not build gpui);
  * GUI unit tests: `cargo test -p claude-history-gui --lib [name filter]` (a filter shortens the
    run, not the build).
* **Feature sets:** each of `--features qa`, `--features hermetic` (core) and
  `--features release-dynamic-ort` builds the workspace crates again. Use them only when the change
  touches that code (the QA runner, core's hermetic guards, the bundled ONNX Runtime loader); CI runs
  the whole matrix.
* Do not run `cargo clean`, and do not build with `--release` (perf QA scenarios are the only
  exception, see below).
* **gpui unit tests do not catch everything:** test-platform windows behave differently (the app once
  crashed on every launch with all tests green). After changes to startup, windows, menus, actions or
  focus, also run a QA scenario against the real app (README:
  [QA scenarios](README.md#qa-scenarios)) with a debug qa build:
  `cargo build -p claude-history-gui --features qa`.
* **Timings are noisy:** parallel agents keep the machine busy and timings vary up to 4x. Never loosen
  a timing threshold or a perf gate to get green: rerun the test alone, judge by medians, and compare
  UI-thread CPU time with wall time to tell load from real work. Perf QA scenarios (`perf-*.qa`) are
  measured in release builds and in a quiet window: ask the user for one.
* Before finishing: `cargo fmt --all` and
  `cargo clippy --workspace --all-targets --locked -- -D warnings` (CI denies warnings).
* Dev and test builds carry line tables only (panics and backtraces still show file:line) and no
  debug info for dependencies; the release profile is unchanged.

## Linux (Docker)

CI tests every change on Ubuntu, so a local Linux build is needed only when the change touches
platform-specific code: `cfg(unix)` / `cfg(windows)` / `cfg(target_os = ...)` blocks (core: `env`,
`program`, `settings`, `history/path`, `resume/terminal`, `semantic/cache`; gui: `ops/console`,
`services`, `single_file`, `globals`, `workspace`, `test_support`, `qa/gate`), `libc` calls, file
permissions, or the dependencies (`Cargo.toml`, `Cargo.lock`).

* Build on Linux only through `tools/dev/linux-test.ps1`. It keeps the target directory, the cargo
  registry and the ONNX Runtime download in named Docker volumes and leaves no containers behind.
  Never `docker run` a Rust image with the build inside the container: every such target directory
  stays in the Docker disk (it once grew to 600 GB).
* **At the start of the session,** before the first Linux build:
  `pwsh -NoProfile -File tools/dev/linux-test.ps1 --update` (pulls the newest `rust:1-trixie` and
  rebuilds the dev image).
* Any cargo command, for example
  `pwsh -NoProfile -File tools/dev/linux-test.ps1 test -p claude-history-core --locked` or
  `pwsh -NoProfile -File tools/dev/linux-test.ps1 clippy --workspace --all-targets --locked -- -D warnings`.
  A worktree's first Linux build starts from a copy of the main checkout's compiled dependencies.
* **When the worktree is done:** `pwsh -NoProfile -File tools/dev/linux-test.ps1 --drop-volume`
  deletes its target volume (`--gc` deletes the volumes of all worktrees that no longer exist). The
  image, the main checkout's volume and the shared caches stay.

## gpui-kit pitfalls (verified on 0.7.3, Windows)

* gpui-kit is pinned (`=0.7.3`) and the GUI relies on its internals: check APIs against the sources
  in the cargo registry, not a gpui-kit git checkout (it drifts ahead of the release).
* Side effects (clipboard, dialogs, spawning processes, URLs, revealing paths) go through
  `PlatformServices` (`services.rs`): `reveal_path`, `open_url` and similar panic on the gpui test
  platform, and tests use `RecordingServices`.
* `cx.set_menus` renders nothing on Windows: the menu is `AppMenuBar` inside the `TitleBar`
  (`set_menus` stays for macOS). `window.prompt` is light-themed in dark mode: in-app dialogs only.
* `is_action_available` before a window's first draw can panic (no dispatch tree yet): menu
  availability is synced after the first frame.
* A focused `Input` consumes `ctrl-x` (even with no selection), `home`/`end` and editing chords:
  shortcuts that must work while typing are bound again with a `"<Context> > Input"` predicate
  (`keymap.rs`); single-letter shortcuts are scoped to non-input contexts. Parse user shortcuts with
  `KeyBinding::load` (`KeyBinding::new` panics on bad input).
* A root `track_focus` steals the focus from an `Input` on any click; `TextView` takes the focus on
  mouse-down and loses it when its row scrolls out of a virtualized list: refocus deliberately.
* Lists: `uniform_list` for uniform rows, `gpui_kit::list` with `ListState` for variable heights.
  Never `measure_all()` (about 6 s to first paint and ~1 GB at 3k messages); cap entity-backed
  per-row state with an LRU; keep the scroll anchor by diffing item ids into `ListState::splice`.
* `combine_highlights` picks the winner of two colors in hash order: where two layers set the same
  field, split the runs by hand (`list::rows::overlay`).
* The focused input's caret blink repaints about twice a second: idle-CPU and idle-frame checks
  account for it.
* Never enable `+crt-static` (CRT mismatch with ONNX Runtime); DirectML stays delay-loaded
  (`crates/gui/build.rs`).
