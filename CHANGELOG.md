# Changelog

GitHub generates the release notes from the merged pull requests; a `## v<version>` section of this file, if there
is one, is put above them by the release workflow.

## v0.2.0

Update notes: the history store is rebuilt once on the first start (the list footer shows its progress), and the
semantic search model changed, so the history is re-embedded in the background after the update. The caches of
earlier versions in `~/.cache/claude-history-gui` are removed by the first update (only files the app wrote there).

### New

- Codex threads are listed and searchable next to Claude Code, Pi and OMP conversations (marked `CX`), read from
  `~/.codex` or `CODEX_HOME`, including archived and compressed sessions. They open in the viewer, single-file
  windows, exports and copies (a forked thread shows the history it continues from, then a "Fork" marker). Resume
  and Fork run `codex resume` / `codex fork` in a new terminal, Delete runs `codex delete --force` after the usual
  confirmation; renaming a Codex thread is refused. Deep Search and the agent panel find, read and outline them.
- Claude Code subagent transcripts and `/btw` side questions are indexed. Subagents are opened from their session's
  viewer header (`s`, or click the count) and Shift+P in a subagent or fork opens its session; `/btw` forks are listed and searched as
  conversations of their own, tagged `fork`. Resuming a subagent or fork resumes its session, deleting one removes
  only its own files, and deleting a session keeps its forks.
- Automated Codex runs and subagent threads are hidden by default. Ctrl+G (View > Show automated and subagent
  conversations, `[keys] all_kinds`) or `--all-kinds` shows them in the list, tagged `auto` or `sub`; Deep Search's
  "Include exec and subagent runs" (`agent search --all-kinds`) searches them. Semantic search never finds them.
- **You only** toggle (Alt+U, View > Search only your own messages, `[keys] user_only`): lexical search matches only
  what you typed, and snippets come from your messages. Deep Search has the same as "Only my messages"
  (`agent search --user-only`); semantic and hybrid modes fall back to keywords with a warning.
- Viewer tool display **Calls**, between Summary and Truncated (`t` cycle: Off, Summary, Calls, Truncated, Full):
  one line per tool call, such as `Bash: cargo test`; a click expands just that call and its result.
- Viewer tool display **Off** shows only the conversation text; messages that hold only tools disappear and
  navigation skips them. `--no-tools` and `no_tools = true` start in Off, and Settings > Display > Default tool
  display offers Off, Summary and Full. A Summary chosen in Settings with an earlier version was saved as
  `no_tools = true` and now starts in Off; choose Summary again to fix it.
- Settings > Search > Excluded working directories (`[tui] exclude_paths`): conversations whose working directory
  starts with a listed prefix are hidden from the list, its searches and Deep Search. Prefixes match literally,
  case-sensitively except on Windows. Hidden conversations still open by session ID, file path or agent ref.
- Resume, Fork and the Codex delete can run through your own command or wrapper (a mise or nix shim, a script
  that sets variables first): Settings > Resume or `command` under `[resume]`, `[resume.pi]`, `[resume.omp]` or
  `[resume.codex]`, for example `command = ["mise", "exec", "--", "claude"]`. gui.toml wins over the program path
  on the General page, which wins over config.toml. A missing program is reported, naming where it is set, before
  anything starts.
- The conversation list shows your prompt count (`12 prompts`) rather than counting tool calls and results; the
  viewer header shows both, such as `113 prompts · 948 tool calls`.
- A shared `config.toml` with `[semantic] daemon` or `pause_on_battery` is accepted; the GUI ignores those keys.

### Changed

- Startup is faster and uses much less memory: the list and searches read from the history index, which is brought
  up to date in the background (at start, on refresh, after rename or delete) and replaces the list without
  moving the selection or clearing a search. Rename and delete become available moments after the first rows
  appear. Changing the time filter does not reload the history. Updates read only new or changed files.
- Semantic search uses the snowflake-arctic-embed-xs model (about 90 MB), which ranks as well or better at half the
  indexing cost; the old model download is removed once the new one loads. Indexing is several times faster and
  runs at a lower priority so the app stays responsive.
- Semantic search answers at once from what is embedded and embeds the rest in the background (progress in the list
  footer), refreshing the results when done. Generate and Clear semantic cache pause the background indexing
  first. The embedding cache is a memory-mapped file, converted once on first use.
- Debug search, the semantic report and dump, Generate semantic cache and Deep search read from the history index,
  which keeps parse errors so Deep search still warns about malformed transcripts.
- Conversations are dated by their last message, not the file's modified time, so a session that was merely
  touched no longer jumps to the top; this also affects the time column, time filter and viewer date.
- The Workspace scope (and Deep search's, Debug search and the semantic tools) also lists conversations that
  worked in the workspace or a folder below it, including Pi, OMP and Codex sessions; the search prompt shows the
  workspace folder instead of `Project`. On Windows these scopes and `exclude_paths` ignore letter case.
- Search folds case and Unicode encoding fully (`strasse` finds `straße`, `istanbul` finds `İstanbul`, a decomposed
  `é` finds a composed one; accents still count). A letter-digit switch starts a word (`264` finds `h264`), `1.1`
  finds `v1.1.1`, and quoted or identifier literals match only at word starts, so "Config" misses `AppConfig`.
  Chinese queries match anywhere in a message.
- Find in conversation (`/`) follows the same word rules (`needle token` finds `needle_token`, `ired` no longer
  finds `fired`), any query word can match, and Enter on no matches says so. List highlights and snippets agree
  with the search, so `.net` is no longer highlighted inside `asp.net`.
- Deep search orders conversations like the list search, keeps conversations that match only by title, summary,
  project or words spread over several messages (one hit at the first message), and leaves out Excluded projects.
  It also leaves out automated and subagent runs unless "Include exec and subagent runs" is on.
- Searching only your own messages and list previews keep text after an unclosed tag or `[Image...` lookalike;
  only `[Image]`, `[Image #N]` and `[Image: description]` are placeholders.
- While you search, the viewer opens the selected result at its match (for a semantic result, the passage closest
  to the row's), marks that message and highlights the query; the match found stands out and others get a soft
  wash. Clearing the query or searching with `/` removes the highlights.
- Tool display: results show images and documents as `[image: image/png]` instead of base64, tool errors as
  `Error: ...`, and one-line JSON pretty-printed; subagent results show their text. Exports and copies match.
  Calls without their own format show one line of JSON (`clock__sleep: { "duration_ms": 20000 }`), Pi and OMP
  tools (`bash`, `read`, `edit`, `write`, `grep`, `find`, `codemode`, `ls`, `web_search`, `fetch_content`,
  `process`, `suba`) read like Claude Code ones, and `Skill` calls show as `Skill: /commit`. Summary rows name
  tools without their own wording instead of "Called N tools", and keep one run together across records that show
  nothing. In Calls mode a call with more to show ends with `(N more lines)`; `<persisted-output>` loses its tags.
- Expanding or collapsing a Summary row keeps it in place instead of scrolling its message to the top.
- Conversation refs and message anchors in Deep Search and the agent panel change once (a Codex ref survives
  reverts and archiving; a Pi or OMP ref no longer depends on its working directory). Old ones still resolve; an
  edited or dropped message is reported as `stale-anchor`. Pi and OMP messages are numbered by file line.

### Fixed

- The list footer reports index progress ("Indexing conversations… 2,345 / 7,185"), says results may be incomplete
  meanwhile, and shows failures ("Indexing failed: …") instead of only logging them; if nothing can be listed, the
  list shows the error with Retry.
- Parse errors after a skipped user message (a Warmup message, `/clear` leftovers) show the right line number and
  context in Tools > Debug log > Parse errors.
- When the Pi and OMP session folders are the same folder, its OMP conversations are no longer left out.
- Less CPU spent checking session files, and the embedding cache is no longer overwritten by another running
  instance when it saves. Reopening a recently viewed conversation does not reread an unchanged file, and Codex
  threads honor `CODEX_SQLITE_HOME`.

## v0.1.1

First published release: the v0.1.0 release build failed on a flaky test and was never published, so this
release carries everything listed under v0.1.0, plus:

- The history loader installs the prepared conversation list on the poll after the one that finished reading,
  so a poll that adds rows always leaves the list loading (fixes the flaky `rows_stream_in_while_loading` test).

## v0.1.0

First release of the desktop browser for Claude Code, Pi and OMP conversation history: conversation list with
lexical and semantic search, a viewer with search and message navigation, Deep Search and the agent panel,
resume / fork / rename / delete / export, picker mode, a Tools menu, settings and a command palette. Windows,
macOS (Apple silicon) and Linux builds.
