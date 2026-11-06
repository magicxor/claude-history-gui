# Changelog

Release notes are taken from the `## v<version>` section of this file by the release workflow.

## Unreleased

- The viewer has a new **Calls** tool display between Summary and Truncated (toolbar, View > Tool display, and the
  `t` cycle: Off, Summary, Calls, Truncated, Full). It lists every tool call of a run on its own line, such as
  `Bash: cargo test`, without the results; a click on any of these lines expands the run into its calls and
  results, as a click on a Summary row does. The mode is remembered like the others.
- Summary mode keeps a run of tool calls in one row even when records that show nothing (bookkeeping entries,
  thinking-only messages while thinking is hidden) sit between the calls, instead of one "Ran 1 shell command" row per call.
- Tool calls without a dedicated format (MCP tools and the like) now show as one line of JSON, for example
  `clock__sleep: { "duration_ms": 20000 }`, instead of a bare name with a pretty-printed body. In Summary mode a group
  of exactly one such call shows that line instead of "Called 1 tool".
- Summary rows now name tools without their own wording (`Read 1 file, called mcp__docs__search, clock__sleep ×2`)
  instead of counting them as "Called N tools".
- Semantic search uses the snowflake-arctic-embed-xs model (about 90 MB), which ranks as well or better than the
  previous one at half the indexing cost. After the update the history is re-embedded in the background, ranking
  what is indexed so far meanwhile, and the old model download is removed once the new one loads.
- Semantic indexing is several times faster: the embedding model now runs on half of the processor cores instead of
  one, and embeds passages of similar length together. The embedding threads and the background pass that updates
  the list index run at a lower priority, also on Windows, and the embedding threads sleep while there is nothing to
  embed, so the app and other programs stay responsive meanwhile.
- Semantic search answers right away from the conversations it has already embedded, and embeds new ones in the
  background ("sem indexing 40%  1200/3000 chunks" with a progress bar in the list footer) instead of making every
  search wait until all of them are embedded. When indexing finishes, the results are refreshed, keeping the row you
  selected; a search that nothing embedded so far matches shows the keyword matches until something indexed matches,
  which is after the first slice, about two seconds in.
- Claude Code `/btw` side questions are listed and searched as conversations of their own, tagged `fork`, and the
  question is also searchable in the session it was asked in.
- Claude Code subagent transcripts are indexed with their session and can be read in full. The viewer header of a
  session names its subagents and forks: press `s` (or click the count) to open one; the header of a subagent or
  fork names its session: press Shift+P (or click it) to open that. Esc and `q` go back through the conversations
  opened this way before returning to the list. Subagents stay hidden from the list unless Ctrl+G or `--all-kinds`
  is on. Resuming a subagent or fork resumes its session; deleting one removes only its own files; deleting a
  session keeps its `/btw` forks. The list index is rebuilt once after the update.
- The Workspace scope also lists conversations that worked in the workspace or a folder below it, not only those
  started there: a session started in `~/code` that moved into `~/code/repo` shows up in `~/code/repo`'s scope.
  Pi, OMP and Codex sessions now also count from a parent folder, so a workspace at `~/code` includes the sessions
  of every repo below it. The same rule applies to Deep search's Workspace scope, Debug search and the semantic
  tools. While the scope is on, the search prompt shows the workspace folder (`~/code/repo ❯`) instead of
  `Project`. The list index is rebuilt once after the update.
- Resume, Fork and the Codex delete can start an agent through your own command or wrapper (a mise or nix shim, a
  script that sets environment variables first, a differently named binary): set it in Settings > Resume (one word
  per line) or as `command` under `[resume]` (Claude Code), `[resume.pi]`, `[resume.omp]` or `[resume.codex]`, for
  example `command = ["mise", "exec", "--", "claude"]`. A command in gui.toml wins over the program path on the
  General page, which wins over a command in config.toml. When the program cannot be found, the error names it and
  where it is set, before anything is copied or started.
- The viewer has a new **Off** tool display (toolbar, View > Tool display, and first in the `t` cycle: Off, Summary,
  Truncated, Full) that shows only the conversation text: tool calls, results and summaries are hidden, messages
  that hold only tools disappear, and message navigation skips them. `--no-tools` and `no_tools = true` now start
  the viewer with tools off (they used to start in Summary); Settings > Display > Default tool display offers Off,
  Summary and Full. If you chose Summary in Settings with an earlier version, it was saved as `no_tools = true` and
  now starts in Off; choose Summary again to remove the key (the remembered mode or config.toml then applies).
- On Windows the Workspace scope, Deep search local scope and `exclude_paths` ignore letter case in project paths, so
  `C:\Users\Foo\myproject` finds sessions recorded under `MyProject`.
- Deep search lists conversations in the same order as the list search and no longer drops conversations that match
  only through their title, summary or project, or whose words are spread over several messages; such a
  conversation shows one hit at its first message. Queries with Chinese characters (also kanji) match anywhere in a message,
  and projects in Excluded projects stay out of Deep search.
- Find in conversation (`/`) matches words the way the list search does: `istanbul` finds `İstanbul`, `strasse`
  finds `Straße`, `needle token` finds `needle_token` (highlighted as one match), any word of the query can match,
  quoted text matches exactly, and a word no longer matches inside a longer one (`ired` does not find `fired`).
  Pressing Enter on a search with no matches says so.
- Search ignores how letters are encoded and folds case fully: a decomposed `é` finds a composed one, `strasse`
  finds `straße`, `istanbul` finds `İstanbul` and `λογος` finds `ΛΟΓΟΣ`. Accents still count: `cafe` does not
  find `café`. The conversation index is rebuilt once.
- List highlights and match snippets follow the search rules: `İstanbul`, `Straße` and CJK words inside a longer run
  are highlighted and shown, and `.net` is no longer highlighted inside `asp.net`.
- Search finds a word that starts inside a longer match: `1.1` now matches `v1.1.1`.
- A switch between a letter and a digit starts a word: `1.2.3` finds `v1.2.3`, `264` finds `h264` and `8` finds `utf8`.
  `AppConfig` is still not split (`onfig` does not find it). The conversation index is rebuilt once.
- Searching only your own messages, and list previews, keep text after an unclosed tag or an `[Image...` lookalike:
  `[Image upload fails` and a typed `<system-reminder>` stay as typed text (only `[Image]`, `[Image #N]` and
  `[Image: description]` are placeholders), and previews drop a tag only when its opening and closing tag both
  occur, so `a < b` and `Vec<T>` stay. The conversation index is rebuilt once.
- Search only your own messages: the list's new **You only** toggle (Alt+U, View > Search only your own messages,
  rebindable as `[keys] user_only`) makes lexical search match only what you typed, leaving out assistant replies,
  tool output, injected context, subagent prompts and titles; snippets come from your messages too. The query and
  the selection stay when you switch, and the footer shows `you only` while it is on. Semantic search ignores it.
  The conversation index is rebuilt once to include what you typed.
- New setting `[tui] exclude_paths` (Settings → Search → Excluded working directories): conversations whose recorded
  working directory starts with a listed prefix, such as throwaway runs under `/tmp/` or your Temp folder, are hidden
  from the list, its searches and Deep Search, for Claude, Pi, OMP and Codex alike. Prefixes match literally and
  case-sensitively. Hidden conversations stay on disk and still open by session ID, file path or agent ref.
- Conversations are dated by their last message, not the file's modified time: a session that Claude touched on
  reopening, or whose Codex rollout got a settings record, no longer jumps to the top of the list as just used. This
  also moves the time column, the time filter and the viewer's date; sessions last used long ago can leave a recent
  time range. The index is rebuilt once.
- While the conversation index is being built (the first start, or after an update changed its format), the list
  footer shows its progress ("Indexing conversations… 2,345 / 7,185") and says that search results may be
  incomplete while you type; a message says when it is done. A failed index update is reported ("Indexing
  failed: …", with the debug log one click away) instead of only in the debug log, and when nothing could be
  listed at all the list shows the error with Retry.
- Codex exec runs and subagent threads can be listed on request: Ctrl+G (View > Show automated and subagent
  conversations, rebindable as `[keys] all_kinds`) or `--all-kinds` brings them into the list and its keyword search,
  tagged `auto` or `sub`, keeping the query and the selection. Deep Search's "Include exec and subagent runs" (`agent
  search --all-kinds`) searches them too, and their refs open in the agent panel either way. Semantic search never
  finds them.
- Deep Search has an "Only my messages" option (`agent search --user-only`): hits come only from what you typed, not
  from replies or tool output. Semantic and hybrid modes search by keywords instead and show a warning.
- Conversation refs and message anchors in Deep Search and the agent panel change once: a Codex thread keeps its ref
  across reverts and archiving, and a Pi or OMP ref no longer depends on its working directory still existing. Old
  refs and anchors still resolve; an anchor whose message was edited or dropped is reported as `stale-anchor`.
- Pi and OMP messages in Deep Search and the agent panel are numbered by their line in the session file. Reopening
  a conversation viewed recently no longer reads its file again while it is unchanged, and Codex threads open in the
  viewer and exports with the state database `CODEX_SQLITE_HOME` names. Print with `--debug` reports only the records
  of a Pi or OMP session that could not be read.
- Deep Search and the agent panel find, read and outline Codex threads; Codex exec runs and subagent threads stay out
  of agent search, for every source.
- First rows appear sooner with large Pi or OMP histories: Claude, Pi and OMP conversations load at the same time
  and stream into the list in small batches, and the list keeps drawing while they arrive.
- Lower memory use: conversation text is shared instead of copied, and search no longer keeps a second copy of it.
- The list shows the newest conversations right away at startup, taken from a snapshot of the previous complete
  load, and they can be opened, resumed and forked at once; rename and delete become available when loading
  finishes, which swaps in the loaded list without moving the selection. The snapshot is kept per workspace and
  Pi/OMP session folder, so a different one never shows another's conversations at startup.
- Quoted and identifier literals match only at word starts (after a non-alphanumeric character or at a switch
  between a letter and a digit): "Config" no longer matches inside AppConfig.
- Searching works right after startup: while the history is still loading, a keyword query answers from the search
  index saved with the startup snapshot, showing the first page of results before the rest. The snapshot now also
  stores the conversation text and that index (rebuilt in the background after a load that changed anything; quitting
  does not wait for the rebuild, which then runs again at the next start).
- Updating the startup snapshot after a load is much cheaper: only conversation files that are new or changed since
  the last update are read again and added to it, and nothing is written when no file changed. An update cut short by
  quitting keeps what it saved and continues at the next start.
- The list no longer loads the whole history at startup: it shows and searches the conversations straight from that
  index, which is brought up to date in the background at start, on refresh and after a rename or delete, and newer
  results replace the list without moving the selection or clearing a search. Startup is faster and uses far less
  memory with a large history. Rename and delete become available moments after the first rows appear; a deleted
  conversation disappears at once. Changing the time filter no longer reads the history again. Semantic search reads
  only what it needs from that index the first time it is used ("sem loading conversations") instead of every
  conversation, and keeps answering while it reads a newer one.
- Semantic search and Deep search use less memory and start faster: the embedding cache is now a memory-mapped file
  that is read in place instead of being loaded into memory by each search. The existing cache is converted once
  on the first semantic search after the update. The list, Deep search and another running instance of the
  app no longer overwrite each other's newly computed embeddings when they save the cache.
- Debug search, the semantic search report and debug dump, Generate semantic cache and Deep search read the
  conversations from the history index (after bringing it up to date, waiting while the list is still indexing)
  instead of parsing every session file again. Deep search still warns about malformed transcripts: the index now
  keeps their parse errors, so it is rebuilt once on the first start after the update. When the Pi and OMP session
  folders are the same folder, its OMP conversations are no longer left out of the list.
- The per-project and Pi/OMP conversation caches of earlier versions are no longer used: the first index update of
  this version removes them from `~/.cache/claude-history-gui` (only files the app wrote there, never the
  claude-history CLI's cache), freeing their disk space.
- Parse errors that follow a skipped user message (a Warmup message, the records `/clear` leaves behind) show their
  own line number and surrounding lines in Tools > Debug log > Parse errors and in debug.log; they were reported one
  line early, with the wrong context. Claude conversation files are also recognized and read with less work.
- Codex threads are listed and searchable next to Claude Code, Pi and OMP conversations (marked `CX`), read from
  `~/.codex` or `CODEX_HOME`, including archived and compressed sessions, with the titles Codex keeps in its state
  database (only ever read).
- Automated runs (such as `codex exec`) and subagent threads no longer appear in the list, list search or semantic
  search (Deep search still finds them); Codex subagent threads whose task came from their parent are no longer
  skipped as empty.
- Codex threads open in the viewer, in single-file windows and in every export and copy format; a forked thread
  shows the history it continues from, then a "Fork" marker where its own messages begin. Resume and Fork run
  `codex resume` / `codex fork` with the thread's id in a new terminal, with `CODEX_HOME` set to the thread's home
  and the arguments of the new `[resume.codex] default_args`; Delete runs `codex delete --force` after the usual
  confirmation. Renaming a Codex thread is refused with a message, and a thread whose current history Codex no
  longer has allows no action. Settings has fields for the codex program and the Codex default arguments.

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
