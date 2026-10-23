# Notes for coding agents

The specification is [`docs/DESIGN.md`](docs/DESIGN.md); the commands CI runs are in the README
([Development](README.md#development)) and `.github/workflows/ci.yml`. Several agents usually work at
the same time, each in its own git worktree, on one machine: keep builds incremental and narrow.

## Building and testing (Windows)

A cold build of the dependency tree (gpui, ONNX Runtime, ~420 crates) takes 7-9 minutes; a rebuild
after an edit takes about 15 s.

* **Fresh worktree:** before the first cargo command, run
  `pwsh -NoProfile -File tools/dev/seed-target.ps1`. It copies the main checkout's compiled
  dependencies into this worktree's `target/`, so only the two workspace crates are compiled.
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
* Do not run `cargo clean`, and do not test with `--release`.
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
  deletes its target volume (`--gc` deletes the volumes of all worktrees that no longer exist).
