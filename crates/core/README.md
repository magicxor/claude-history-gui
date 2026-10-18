# claude-history-core

The UI-agnostic core of **claude-history-gui**: loading, parsing, searching and
rendering-preparation for Claude Code, Pi and OMP conversation history. The GUI
crate (`crates/gui`) depends only on the APIs of this crate; nothing here
depends on a terminal or GUI toolkit.

## Origin and license

This crate is derived from [claude-history](https://github.com/raine/claude-history)
v0.1.76 (commit `8d4cf67`), Copyright (c) 2024 Raine, released under the MIT
license. The upstream license text is in [`LICENSE-MIT`](LICENSE-MIT), and this
crate is distributed under the same MIT terms. The rest of the repository uses
the Unlicense (see the repository root `LICENSE`).

Most modules are upstream files copied verbatim or with small, documented
edits. The terminal UI, the clap command line, the stdout/pager display and
the self-updater are not part of this crate; the upstream TUI's logic is
here as UI-neutral controllers and models (`browser`, `view`), and a GitHub
release check (`update`) replaces the self-updater.

## Module map

| Module | Contents |
|---|---|
| `env` | `HistoryEnv` (every path and environment input: home, `CLAUDE_CONFIG_DIR`, Pi/OMP variables, `PATH`, the workspace that replaces the process cwd, cache/config/log locations) and `WorkspaceScope` |
| `config` | the CLI's `config.toml` (read-only, upstream schema), `gui.toml` overrides plus the `[gui]` table, layered loading, atomic `gui.toml` saving (whole file, or only the changed keys with comments kept: `save_gui_config_changes`), neutral `KeyChord` key bindings |
| `history` | Claude/Pi/OMP discovery and loading (streaming and blocking), JSONL parsing, per-project caches, rename, delete, delete-empty, path encoding |
| `search` | query parsing, lexical/literal search and evidence, `SearchMode` |
| `semantic` | chunking, embedding cache, fastembed embedder, semantic index and ranking |
| `agent` | the agent protocol (`search`, `within`, `read`, `outline`): `agent::args` (request structs with `validate()`), `agent::cli` (upstream's `agent ...` command line), `agent::execute` (an `AgentResponse`: the CLI's exact stdout/stderr/exit code plus the result as data), `agent::execute_text`, `AgentService` (cancellable) |
| `markdown`, `syntax`, `tool_format`, `text_match`, `time_filter`, `claude`, `commands` | backend-neutral markdown layout, syntax highlighting, tool-call formatting, text matching, `--since/--before` grammar, transcript records, command-tag handling |
| `theme` | dark/light palettes; `set_theme`/`current` switch the active palette at runtime |
| `debug`, `debug_log` | leveled debug messages with a pluggable sink, and the append-only debug log |
| `program` | resolution of the `claude`/`pi`/`omp` executables (`PATH`/`PATHEXT`, `.cmd` shims, overrides) |
| `browser` | the conversation list controller (upstream `tui/app`): loaded rows (one shared `Arc` corpus, also used by the search workers), query, workspace scope and exclusions, lexical search worker, semantic mode with lexical fallback (suspendable for exclusive cache work), selection, rename and delete in two halves (blocking disk work in `history`/`maintenance`, then `apply_renamed`/`remove_paths`), the UUID jump; paths and session ids are indexed and the rows in scope cached, so lookups never scan the corpus and an edit costs one O(n) pointer pass on the rows (no per-row work); `HistoryLoader` streams the history into it and prepares the corpus (with its lookup data, scope and row mapping) off the caller's thread, and `Browser::poll` applies worker answers |
| `semantic_worker` | the background semantic search worker (upstream `tui/semantic_worker.rs`) with an injectable embedder factory and a waker |
| `display_format` | list-row formatting: highlight byte ranges, relative timestamps with a recency bucket, row header and preview composition, model names |
| `view` | the conversation viewer: `view::ledger` (upstream's monospace renderer), `view::document` (a layout-free `ConversationDocument` for native rendering), `view::state` (`ViewerModel`, in-view search, message navigation, scroll anchoring, Esc order) and `ViewPrefs` |
| `export` | ledger, plain, markdown and JSONL exports written to a caller-supplied path (no clipboard transport), and the CLI `--plain` text |
| `resume` | resume/fork planning for Claude, Pi and OMP from the conversation and the chosen workspace, cross-project session copy, and `resume::terminal` (opening a terminal window that runs the command, with verified Windows quoting) |
| `maintenance` | delete-empty (dry run and confirmed delete), delete by UUID, show-dir, debug-search and parse-error reports |
| `semantic_ops` | semantic cache generation (progress and cancellation), clearing, search/debug reports and cache status |
| `settings` | effective settings (launch flags over layered config over defaults), the initial `ViewPrefs`, picker output, and the GUI's hand-written command-line parser (every upstream flag and conflict group, the `delete-empty`, `update`, `agent` and `help` subcommands) |
| `update` | the GitHub release check for claude-history-gui (no self-update) |

## Data locations

Defaults are derived from the home directory and are isolated from an
installed `claude-history` CLI:

| What | Default |
|---|---|
| Claude data | `$CLAUDE_CONFIG_DIR` (must be absolute; blank counts as unset) or `~/.claude` |
| caches (history, Pi/OMP, semantic embeddings, fastembed model) | `~/.cache/claude-history-gui` |
| CLI config (read-only, shared settings) | `~/.config/claude-history/config.toml` |
| GUI config (GUI settings and overrides) | `~/.config/claude-history/gui.toml` |
| debug log | `~/.local/state/claude-history-gui/debug.log` |

Cache file formats are byte-compatible with upstream; only the location
differs. Core never writes `config.toml`.

## Tests

```sh
cargo test -p claude-history-core
cargo test -p claude-history-core --features hermetic
```

The `hermetic` feature turns the unit tests' guard rails (below) on in a
normal build of the library, for other crates' tests (the GUI enables it in
its `[dev-dependencies]`): `HistoryEnv::from_process_env` panics, the
default embedders and `FastembedEmbedder::new_in` itself refuse to load the
model, the update check panics and the terminal spawn functions return
`Unsupported`. `tests/hermetic_guards.rs`
checks that build. `examples/terminal_smoke.rs` opens a real terminal and is
only run by CI smoke jobs.

Tests are hermetic:

- Every test that touches the file system builds a `HistoryEnv::sandbox(tempdir)`,
  whose home directory (and therefore Claude data, Pi/OMP sessions, caches,
  config and log) lives in a temporary directory. `HistoryEnv::from_process_env()`
  panics under `cfg(test)`, so a test cannot reach the real home by accident.
- No test changes environment variables or the process working directory.
- Semantic tests use fake embedders (the agent service, the semantic worker
  and `semantic_ops` accept an embedder factory); no test downloads the
  embedding model or uses the network. Under `cfg(test)` the semantic
  worker's default factory refuses to load fastembed and the update check
  never makes a request. The only real-model test is `#[ignore]`d.
- Terminal launching is tested on the commands it would spawn; tests never
  open a terminal or run `claude`, `pi` or `omp`.
- Upstream's `tests/agent_cli.rs` scenarios run against `agent::AgentService`
  in a sandbox (`src/agent/cli_scenario_tests.rs`); mtimes are set with
  `File::set_modified` instead of `touch`.
- Fixtures live in `tests/fixtures` and are checked out without line-ending
  conversion (`.gitattributes`).
