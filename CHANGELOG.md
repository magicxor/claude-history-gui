# Changelog

Release notes are taken from the `## v<version>` section of this file by the release workflow.

## Unreleased

- First rows appear sooner with large Pi or OMP histories: Claude, Pi and OMP conversations load at the same time
  and stream into the list in small batches, and the list keeps drawing while they arrive.
- Lower memory use: conversation text is shared instead of copied, and search no longer keeps a second copy of it.
- The list shows the newest conversations right away at startup, taken from a snapshot of the previous complete
  load, and they can be opened, resumed and forked at once; rename and delete become available when loading
  finishes, which swaps in the loaded list without moving the selection. The snapshot is kept per workspace and
  Pi/OMP session folder, so a different one never shows another's conversations at startup.
- Quoted and identifier literals match only at word starts (after a non-alphanumeric character): "Config" no longer
  matches inside AppConfig.
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
