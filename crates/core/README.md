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
| `env` | `HistoryEnv` (every path and environment input: home, `CLAUDE_CONFIG_DIR`, Pi/OMP variables, `CODEX_HOME` and `CODEX_SQLITE_HOME` (`codex_home`, `codex_sqlite_home`), `PATH`, the workspace that replaces the process cwd, cache/config/log locations, the installed CLI's cache root that core never deletes from) and `WorkspaceScope` |
| `config` | the CLI's `config.toml` (read-only, upstream schema, `[resume.codex] default_args` included: `ResumeConfig::default_args(source)`), `gui.toml` overrides plus the `[gui]` table (`codex_path` among the program paths), layered loading, atomic `gui.toml` saving (whole file, or only the changed keys with comments kept: `save_gui_config_changes`), neutral `KeyChord` key bindings |
| `history` | Claude/Pi/OMP discovery and loading (the in-memory list streams Claude projects one by one, concurrent to every other instance parsed through the registry, Codex included since upstream 9597669; every file is parsed, with no cache of its own since upstream 75df2d4), JSONL parsing, rename, delete, delete-empty, path encoding; `model`, the normalized transcript model (source, conversation key, revision, events) that adapters decode into; `adapter` and `sources`: one adapter per configured root (Claude projects dir, Pi and OMP session roots, the Codex home) that lists its conversations with one stat each (`Located`, `Stamp`; an adapter may carry its own resolution in `Located::native`, and maps an artifact to its native id with `Adapter::native_id_of`), says whether that listing was complete (`Completeness`) and decodes a session file through `Adapter::decode_into`, event by event (`decode` collects them into a `Transcript`) (Claude records converted into events by `sources::claude::Records`; Pi and OMP records decoded straight into events whose origin is the record's line in the session file, `pi::PiSession`, `pi::read_header` for a header-only read, upstream 9597669; a loose file through `sources::decode_path`, its source recognized from its first records), gathered in `sources::Registry::from_env(&HistoryEnv)` (fork: from the env, not the process environment); `sources::codex` reads a Codex home (upstream 7f5c443): rollouts under `sessions/YYYY/MM/DD` and `archived_sessions` (`.jsonl` and zstd `.jsonl.zst`) grouped by thread id (the native id), each thread's head rollout chosen with Codex's state database (`state_<N>.sqlite`, opened read-only through rusqlite; busy or unreadable: a degraded pass that keeps the stored rows) or inferred from the rollouts, its `history_base` chain followed into inherited and own segments, legacy and paginated rollouts decoded into events and `sources::codex::view` decodes a rollout outside any configured home as its thread (upstream eea6d37; `sources::decode_path_into` goes through it); Codex threads resume and fork through the Codex CLI with `CODEX_HOME` and are deleted with `codex delete --force` (`delete_spec`, `thread_rollouts`), never renamed; each adapter also declares the actions its tool supports (`actions::Capabilities`; `actions::capabilities_of(source, provenance)` allows nothing for an orphaned conversation and `actions::refusal` words why, upstream eea6d37) and resumes (with `resume`'s planners), deletes and renames its conversations, and `Registry::for_path` gives the adapter acting on a conversation, the instance of its source whose root holds the file (`for_source` without one): resume planning, `rename_session`, `maintenance::plan_session_delete` and `Browser::delete_conversation` go through it (upstream 9799550, 9597669); `view::entries` is the single display projection of a transcript (messages with the source's speaker, `SourceKind::assistant_name`; subagent reports; notes; hidden records; a `Fork` note where a forked thread's own records begin), which the viewer, exports, copy-message and `--plain`/`--render` render (upstream 9597669; fork: `entries_with_events` names each entry's event); `index` folds a `Transcript` into the `Conversation` the list and search store (previews, full text, semantic turns and their message ranges, route text, own text (`messages::authored_text` of top-level user messages, upstream ae7ae39), token totals, duration), source-agnostic; reconcile and `process_conversation_file` (for callers with a loose path) feed the decoded events into `index::Indexer` (`push` per event, `finish` with the transcript metadata) instead of buffering a `Transcript` (upstream 350314d; fork: the list loaders stream through `sources::decode_path_into` as well); `service::HistoryService` opens a conversation by `ConversationKey` (`open`: `Adapter::resolve` then decode, reusing an `Arc<Transcript>` while the stamp is known and unchanged, the 8 opened last kept, upstream 9597669) or by path (`open_path`: through `Registry::key_of`, else a loose session), and the agent commands load transcripts through it (upstream 59b9565); fork: built per environment (`from_env`, no process-wide `shared()`; `Default` has no instances), and `open_cancellable`/`open_path_cancellable` give a decode up once a flag is set (`Adapter::decode_cancellable`, checked at each read of a Claude, Pi or OMP session file; `adapter::is_cancelled`) |
| `store` | the on-disk list snapshot and its search index (upstream `store`): an immutable, memory-mapped generation under `<cache base dir>/v3/roots-<hash>` (`HistoryEnv::cache_base_dir`) (a manifest of newest-first card rows, a project table, file fingerprints, each card's index location, the index segment table, the ignored files and a string heap, validated on open; `text` stores each conversation's text as one checksummed zstd frame in a `text-NNNN.zst` segment, referenced from its card, and its semantic data (`semantic::SemanticDoc`: turns, message ranges, route text, agent text, parse errors; one bincode frame per card) the same way in `sem-NNNN.zst`, and what the person typed (`Conversation::own_text`) in `own-NNNN.zst`, located by an `OwnRow[]` manifest section (upstream ae7ae39, b9497ce), manifest version 8; and the suffix index segments `base.*` and `delta-NNNN.*`, each with an `own-` twin over the own text (`lexindex::Field::{Body, Own}`, same doc ids), queried as one by `segments::IndexSet`), all written into `gNNN.tmp` and published atomically under a `LOCK` (`PublishLock::try_acquire` takes it without waiting, `PublishLock::wait` waits until it is free) and collected one generation behind `CURRENT`; `reconcile` brings it up to date with the session files the adapters of `history::sources::Registry::from_env(&HistoryEnv)` discover (one stat per file, only new or changed files parsed the way the loader parses them, files without a conversation remembered as ignored, unlistable sources keep their cards; nothing written when nothing changed): `generation::publish_update` publishes the changes as a delta generation (new text, own-text, semantic and index segment, every field, unchanged segments hard-linked), and `compact` merges index segments of every field (`lexindex::merge`, postings only) and rewrites mostly dead text, own-text and semantic segments past `MergePolicy` thresholds; after a pass that published, `legacy` removes the per-project and Pi/OMP caches older versions kept in the store's cache dir (only files with their names and tags, then the dirs this empties; `Outcome::removed`; fork: never in the installed CLI's cache dir, `Registry::cli_cache`); fork: `reconcile_cancellable` (with `publish_update_cancellable`, `compact_cancellable`, `lexindex::build_cancellable`/`merge_cancellable`) gives a pass up soon after a flag is set (the GUI reconciles in-process and quits without waiting), leaving no `gNNN.tmp`; `progress` shares a running pass's progress through `PROGRESS` in the store root (passes that parse at least 50 files; `failed <message>` after a failed pass; upstream 04c714b); `Snapshot` reads the newest cards in a scope for the browser's first rows, the scope of every card (fork: under a time filter set at runtime, `with_time_filter`), a card by path, key hash or session id (`find`, `contains_key`, `find_session`) and whether the cards mix sources; `SearchIndex` searches a generation (cards, index segments and text frames), or with `user_only` only the own text (the `own-` segments, without title, summary or project; upstream ae7ae39); `load_conversations` gives every conversation of the env's store after a reconcile pass of its own (waiting for, then repeating, a pass another thread or process runs), as cards only (`Contents::Cards`) or with text and semantic data (`Contents::Full`), parsing the session files instead when the store cannot be used (the one-shot tools' `maintenance::load_sorted_conversations` and agent search; fork: `load_conversations_cancellable`, used by agent search, gives the pass, the wait for `LOCK` (`PublishLock::wait_cancellable`) and the reads up once a flag is set); `SemanticCards` reads every card with its semantic data for semantic search (in parallel, reusing the data of cards unchanged since the cards read before; fork: `load_cancellable`); `lexindex` is the word-start suffix index for list search (per segment: `.fst` fst partitions, `.post` postings, `.short` 1-2 byte prefix columns, memory-mapped and validated on open; per-document capped term frequencies for a query word or adjacent pair, remapped to card rows); `search` ranks conversations over that index with the same results, scores and order as `search::lexical` (per-word tf from postings, title/summary/project from cards, freshness from timestamps, adjacency from pair terms; conversation text is read only for literals, words over 24 bytes, self-overlapping words and CJK; `Ranking::top(n)` evaluates lazily by upper bound, `complete()` the rest), checked against `debug_search` by oracle tests; the `#[ignore]` `real_corpus_oracle` reads a QA sandbox named by `CHG_SEARCH_CORPUS_HOME` (needs the `.qa-sandbox` marker) |
| `search` | query parsing, lexical/literal search and evidence (`LexicalEvidence` carries ±1024-char windows of the text around the hidden matches, so it can come from a store text frame), `SearchMode` |
| `semantic` | chunking, embedding cache (`matrix`: the memory-mapped `embeddings-v2.bin`, merged on every save; an `embeddings-v1.bin` is converted once), fastembed embedder, semantic index and ranking |
| `agent` | the agent protocol (`search`, `within`, `read`, `outline`): `agent::args` (request structs with `validate()`), `agent::cli` (upstream's `agent ...` command line), `agent::execute` (an `AgentResponse`: the CLI's exact stdout/stderr/exit code plus the result as data), `agent::execute_text`, `AgentService` (cancellable) |
| `markdown`, `syntax`, `tool_format`, `text_match`, `time_filter`, `claude`, `commands` | backend-neutral markdown layout, syntax highlighting, tool-call formatting, text matching, `--since/--before` grammar, transcript records, command-tag handling |
| `theme` | dark/light palettes; `set_theme`/`current` switch the active palette at runtime |
| `bench` | startup timing marks (`start(sink)`, `mark`, `mark_peak_rss`, `mark_peak_rss_as`, `enabled`); no-ops until a caller starts them with an explicit sink, core reads no environment |
| `debug`, `debug_log` | leveled debug messages with a pluggable sink, and the append-only debug log |
| `program` | resolution of the `claude`/`pi`/`omp` executables (`PATH`/`PATHEXT`, `.cmd` shims, overrides) |
| `browser` | the conversation list controller (upstream `tui/app`): rows (one shared `Arc` corpus, also used by the search workers), query, workspace scope and exclusions, lexical search worker, semantic mode with lexical fallback (suspendable for exclusive cache work), selection, rename and delete in two halves (blocking disk work in `history`/`maintenance`, then `apply_renamed_session`/`remove_paths`; a renamed or UUID-loaded session is normalized in the background as a `PreparedSession`), the UUID jump; paths and session ids are indexed and the rows in scope cached, so lookups never scan the corpus and an edit costs one O(n) pointer pass on the rows (no per-row work); `Browser::poll` applies worker answers; `Readiness` (`list`, `lexical`, `reconcile`, `semantic`) says which parts are usable. Generation mode (`cards`, upstream 358becb; `HistoryLoader` with `LoaderOptions::snapshot`): the rows are the cards of the store's generation (no text): the newest are listed for the first frame (`show_snapshot`), then every card with the generation's index, opened on a `generation-open` thread (`install_generation`; the lexical worker answers the top page first); reconcile passes on a `store-reconcile` thread (at start, on refresh, after a rename or delete; a store locked by another instance is retried every second; a `store-watch` thread wakes the poll for each newer generation and for new progress in `PROGRESS`, which the browser shows as indexing, `indexing_status_text`, and whose end or failure leaves an `IndexNotice`) publish generations that are swapped in keeping the query, scope and selection; deletes and renames are overlays until a generation holds them; semantic search ranks the cards with their semantic data (`store::SemanticCards`), read on a `semantic-corpus` thread and again after a swap while the older cards serve. Without the store `HistoryLoader` streams the history into it and prepares the corpus (with its lookup data, scope and row mapping) off the caller's thread |
| `semantic_worker` | the background semantic search worker (upstream `tui/semantic_worker.rs`) with an injectable embedder factory and a waker |
| `display_format` | list-row formatting: highlight byte ranges, relative timestamps with a recency bucket, row header and preview composition, model names |
| `view` | the conversation viewer over a decoded transcript's `history::view::entries` (upstream 9597669): `view::ledger` (upstream's monospace renderer; `open_conversation(history, path)`), `view::document` (a layout-free `ConversationDocument` for native rendering), `view::state` (`ViewerModel` holding the `Arc<Transcript>`, in-view search, message navigation, scroll anchoring, Esc order) and `ViewPrefs` |
| `export` | ledger, plain, markdown and JSONL exports of a decoded transcript written to a caller-supplied path (no clipboard transport), and the CLI `--plain` text |
| `resume` | resume/fork planning for Claude, Pi, OMP and Codex (`codex resume` or `codex fork <thread id>` with `CODEX_HOME` in the plan's `env`, upstream eea6d37) from the conversation and the chosen workspace (dispatched through the adapter of the conversation's source after `actions::capabilities_of` allowed it; upstream 9799550), cross-project session copy, and `resume::terminal` (opening a terminal window that runs the command with its environment variables, with verified Windows quoting; `run_to_completion` runs a CLI without a terminal, refused in tests and hermetic builds) |
| `maintenance` | delete-empty (dry run and confirmed delete), delete by UUID, the two-step delete of a listed conversation (a Codex thread through `codex delete --force`, run by the caller: `execute_session_delete_with`), show-dir, debug-search and parse-error reports |
| `semantic_ops` | semantic cache generation (progress and cancellation), clearing, search/debug reports and cache status |
| `settings` | effective settings (launch flags over layered config over defaults), the initial `ViewPrefs`, picker output, and the GUI's hand-written command-line parser (every upstream flag and conflict group, the `delete-empty`, `update`, `agent` and `help` subcommands) |
| `update` | the GitHub release check for claude-history-gui (no self-update) |

## Data locations

Defaults are derived from the home directory and are isolated from an
installed `claude-history` CLI:

| What | Default |
|---|---|
| Claude data | `$CLAUDE_CONFIG_DIR` (must be absolute; blank counts as unset) or `~/.claude` |
| Codex home (read-only) | `$CODEX_HOME` or `~/.codex`; its state database in the home's `config.toml` `sqlite_home`, else `$CODEX_SQLITE_HOME`, else the home (relative values resolve like the `PI_*` paths) |
| caches (list snapshot `v3/roots-<hash>/`, semantic embeddings, fastembed model) | `~/.cache/claude-history-gui` |
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

The ignored `store::search::tests::real_corpus_oracle` compares index search with the in-memory search on a
QA sandbox's history: set `CHG_SEARCH_CORPUS_HOME` to a sandbox made with `tools/qa/new-sandbox.ps1` (the
`.qa-sandbox` marker is required) and run `cargo test --release -p claude-history-core --lib
store::search::tests::real_corpus_oracle -- --ignored --nocapture`. The ignored `real_corpus_query_latency` in the same
module times index queries (first page, complete ordering, text reads) against the in-memory search on the same
sandbox; run it like the oracle with `-- --ignored --nocapture --test-threads=1` on a quiet machine.

Tests are hermetic:

- Every test that touches the file system builds a `HistoryEnv::sandbox(tempdir)`,
  whose home directory (and therefore Claude data, Pi/OMP sessions, the Codex
  home, caches, config and log) lives in a temporary directory. Codex tests
  read the fixture home `tests/fixtures/codex` and build its state database
  in a temporary directory. `HistoryEnv::from_process_env()`
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
