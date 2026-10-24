# Changelog

Release notes are taken from the `## v<version>` section of this file by the release workflow.

## Unreleased

- First rows appear sooner with large Pi or OMP histories: Claude, Pi and OMP conversations load at the same time
  and stream into the list in small batches, and the list keeps drawing while they arrive. Unchanged Pi and OMP
  caches are no longer rewritten on every start, and empty Pi/OMP files are no longer parsed again each time.
- Lower memory use: conversation text is shared instead of copied, and search no longer keeps a second copy of it.
- History caches are smaller: they no longer store a normalized copy of every conversation's text. The caches are
  rewritten once on the first start after the update.
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
