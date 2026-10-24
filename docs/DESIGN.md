# claude-history-gui — GUI design (DESIGN.md)

Status: **revision 2** (review findings resolved, see §20), **updated after Phase 0, Phase A and Phase B**: WP-C, WP0,
WP1, WP2, WP6, WP7, WP-C2, WP3, WP4 and WP5 are merged into master; the frozen contracts in this document describe
the merged code, and §17.11 lists every place where the code deviates from the revision-2 text (read it before
starting a Phase-B/C package).
Design for implementer agents. Companion: `FEATURES.md` (the parity checklist; every row has a stable ID such as
`L-012`, `V-045`, `A-027`, exactly one owning work package and a concrete acceptance check).

Inputs this design is built on (all in `scratchpad/docs/`): `inventory-list-mode.md` (LIST),
`inventory-viewer.md` (VIEW), `inventory-cli-config-actions.md` (CLI), `inventory-agent-protocol.md` (AGENT),
`inventory-addendum.md` (ADD, incl. SAFETY A1/A2), `core-extraction-plan.md` (CORE), `gpui-kit-app-infra.md`
(GK-INFRA), `gpui-kit-components.md` (GK-COMP), `gpui-kit-design-rules.md` (DG), and the spike reports
`spike-viewer.md`, `spike-list.md`, `spike-platform.md` (spike code in `scratchpad/spikes/{viewer,list,platform}/src`).
Upstream source = `C:/git/opensource/claude-history` v0.1.76 (read-only). gpui-kit source of truth = the published
`gpui-kit 0.7.3` crates in the cargo registry (GK-INFRA §0). **Core baseline = `crates/core` at commit `3ba9f90`**
of `C:/git/opensource/claude-history-gui` (the core Modules stage is merged; §4 lists the real signatures), **plus
WP-C (K1–K18, merge commit `78ec36c`)**; §4.3 records where WP-C's code differs from the K-item text.

Contents: §0 Conventions · §1 Goals and non-goals · §2 Product decisions (PD1–PD20, AD1–AD20) · §3 Architecture ·
§4 Core contract (landed APIs, gaps K1–K18, change requests) · §5 Windows, screens, dialogs · §6 File structure and
owners · §7 Entities, events, globals, services, background work, focus, target capture · §8 Actions and keymap ·
§9 Viewer rendering pipeline · §10 List rendering · §11 Data flows · §12 Copy rules and string table · §13 CLI,
configuration, persisted state · §14 Platform specifics · §15 Testing and QA · §16 Build, CI, release · §17 Work
breakdown · §18 Integration order · §19 Risks · §20 Review resolutions (appendix).

---

## 0. Conventions

* **must / should / may** as in DG.
* Keystrokes use GPUI syntax (`ctrl-r`, `shift-j`, `secondary-q`). `secondary-` = Cmd on macOS, Ctrl on
  Windows/Linux. Configurable bindings keep the claude-history config syntax (`ctrl+r`) and are converted (§8.6).
* "Target" = a `ConversationTarget` value captured when an action/dialog/viewer/result set starts (§7.8, PD15).
  Code must never re-read the live list selection to find the object an action acts on.
* Paths: `<home>` = `HistoryEnv::home_dir` (`%USERPROFILE%` on Windows). `crates/gui/src/...` paths are relative
  to the repo `C:/git/opensource/claude-history-gui`.
* **Copy rules** (§12): text that leaves the app (stdout, stderr, clipboard exports, agent text output, "Copy as
  text" reports, `debug.log`) is upstream-verbatim. In-app text (menus, titles, buttons, labels, toasts, empty
  states) follows the GUI copy rules: English, sentence case, `…` (U+2026, never `...`), "conversation" for the
  object (Rename conversation…, Delete conversation…), "session ID" for the identifier. Every in-app string that
  derives from a TUI status text is listed in §12 with its TUI origin.
* Owners: `WP0`, `WP-C`, `WP1` … `WP7`, `WP-I` are the work packages of §17. A file has exactly one owner at any
  time; ownership transfers are listed in §6 and §18.
* "Core" names below are `claude_history_core::…` items; `core::x` is shorthand.

---

## 1. Goals and non-goals

### 1.1 Goals
1. Every user-visible feature, behavior, string, key binding, config option and CLI flag of claude-history v0.1.76
   (TUI list, viewer, CLI one-shots, subcommands, agent protocol) is available in the GUI, as listed in
   `FEATURES.md`; deviations are tagged `[dev]` there with a reason.
2. A desktop-native, keyboard-first master–detail app on gpui-kit 0.7.3: list on the left, viewer on the right, in
   one main window, plus single-file viewer windows. Every command is reachable from the keyboard, including
   menu-only commands (command palette, §8.9).
3. All business logic lives in `claude-history-core` (`crates/core`); `crates/gui` depends only on core's public API
   and gpui-kit (plus the small crates of §16.1). The GUI never re-implements logic core already ships (§4.1).
4. Cross-platform: Windows (primary dev machine), macOS (arm64), Linux (x86_64).
5. Safety: actions act on the conversation the user saw when starting them (ADD A1/A2); destructive and data-writing
   operations are confirmed, name their target, and never confirm on a stray Enter; errors are always shown; tests,
   QA and agent dev runs never touch the user's real data (§15, hermetic builds).
6. Performance (ADD §F, measurable targets in §15.8): near-zero idle CPU; the UI thread never runs O(corpus) or
   blocking I/O work (load end, refresh, rename, delete, UUID paste, open, toggles); typing ≤ 2 ms UI time per key
   on 20k rows; transcript paging p95 ≤ 20 ms on a 10k-message transcript with multi-thousand-line tool outputs.

### 1.2 Non-goals (v1)
* No self-updater (binary replacement). "Check for updates…" only reports and opens the release page (T-020).
* No pager, ANSI colors, OSC 52 or `CLAUDE_HISTORY_CLIPBOARD` (PD10, C-047…C-049).
* No file watching of transcripts (parity, V-082). Refresh is explicit (Ctrl+L / F5 / button / palette).
* No installation of the companion Claude Code skill (never writes `~/.claude/skills`). The skill calls
  `claude-history agent …`; the GUI binary offers the same subcommand as `claude-history-gui agent …` (C-061), but
  the binary name differs, so the skill keeps using the upstream CLI unless the user aliases it (documented).
* No writing of the CLI's `config.toml`, ever (C-033, X-005).
* No sharing of caches with the installed CLI: GUI caches live in `~/.cache/claude-history-gui` (core `env.rs`).
* No OS toast notifications (in-app notifications only; OS toasts need an AUMID registry write).
* No Intel macOS release artifact in v1 (§16.3; ort-sys ships no static ORT for `x86_64-apple-darwin`).

---

## 2. Product decisions (PD1–PD20), refined

Each PD below is the binding behavior. Deviations from the orchestrator's text are marked **[FLAG]** with the
reason; implementers follow the refined text.

### PD1 Layout
* One main window. `TitleBar` (gpui-kit) is the first child of the root; on Windows/Linux it hosts the in-window
  `AppMenuBar` with menus **File, Edit, View, Session, Tools, Help**; on macOS the same menu tree is installed with
  `cx.set_menus` (native menu bar, plus a leading app menu "Claude History" with About, Check for updates…,
  Settings…, Quit) and `AppMenuBar` is not rendered. **One menu tree serves every window** (§8.10): items whose
  action has no handler on the active window's dispatch path are disabled (single-file windows have no session
  handlers, so Resume/Fork/Rename/Delete grey out).
* Because `AppMenuBar` 0.7.3 opens only by mouse (no Alt/F10 activation), every menu command is also in the
  **command palette** (`secondary-shift-p`, aliases `f10`) built from the same menu tree (§8.9) **[FLAG: added]**.
* Body = `h_resizable("main-split")` with minimum widths that always fit the minimum window (880×560 px ≈ 55rem):
  * **Left pane** (default 30rem, range 18–40rem, `bg(sidebar)`): a `TabBar` with two tabs **Conversations** and
    **Deep search** (PD11). The Deep search tab uses its own persisted width (default 34rem, range 24–48rem); the
    pane switches width when the tab changes. Conversations tab = search input, toolbar (mode Lexical/Semantic,
    scope All/Workspace + workspace picker, time filter, refresh), the virtualized conversation list, the list
    footer (count, semantic activity or key hints).
  * **Right pane**: the viewer (minimum 32rem) = header, toolbar, transcript, status bar; plus an optional *agent
    panel* (Outline / Within / Read, PD11) to its right (`h_resizable("viewer-split")`, default 22rem, range
    18–28rem, hidden by default). The agent panel is a **sibling** of the transcript's `Viewer` key context, never a
    descendant (§7.7).
  * Opening the agent panel when `window width − left − panel < 32rem` collapses the left pane (focus-viewer mode,
    marked "auto") and restores it when the panel closes. Restored widths are clamped to the current window on
    startup and on every window resize (left first, then panel).
* **Focus viewer mode** (`ToggleListPane`, `secondary-shift-b`, View → "Focus viewer"): hides the left pane; the
  viewer gets the full width (TUI full-screen feel). Esc in the viewer (after unwinding, PD2) re-shows the list and
  focuses it. Persisted (PD17).
* Keyboard-first: every TUI key works (adapted per §8), plus mouse, plus F6 region cycling and the palette.

### PD2 Click model and focus
* Single click on a list row: selects it (keyboard focus stays in the list search input) and shows it in the viewer
  (preview). Double-click or Enter: selects + shows + moves keyboard focus into the viewer.
* Keyboard selection changes (↑/↓/PgUp/…) also preview the selected row in the viewer, debounced 150 ms, **only
  while focus is inside the Conversations pane**. Automatic selection changes caused by new result sets (selection
  reset to row 0, L-080) preview too while the list has focus. When focus is in the viewer, the agent panel or deep
  search, list selection changes never replace the viewer content.
* Any list-target action (Resume, Fork, Rename, Delete, Export, Copy…, Ctrl+O) first **flushes** a pending debounced
  preview, so the viewer always shows the conversation the action targets (§7.8).
* Esc in the viewer follows core `view::escape` (upstream order): (1) search being typed → clear it; (2) message
  navigation active → deactivate; (3) committed search → clear it; (4) otherwise leave: move focus to the list
  (re-showing it if focus-viewer mode hid it). In a single-file window step (4) closes the window. `q` skips
  (1)–(3) (TUI parity V-084).

### PD3 Workspace (replaces the TUI's process cwd)
Resolution at startup, first match wins:
1. `--workspace DIR` / `-w DIR` (must be an existing directory; otherwise toast `Workspace not found: <DIR>` and
   continue with the next rule);
2. `--local`/`-L` given → the launch cwd (`std::env::current_dir()` read once in `main`), whatever it is (TUI
   parity: `-L` always meant "this directory");
3. the launch cwd if it is *meaningful*: it is not a filesystem root, not the home directory, not the directory of
   the executable (or of the `.app` bundle), and (a) maps to an existing Claude projects dir
   (`env.claude_projects_dir_for(cwd).is_dir()`), or (b) has a `.git` entry (dir or file), or (c) is the recorded
   cwd of at least one Pi/OMP session (checked after the first load batch; if it matches and no workspace was
   chosen yet, the workspace is set then and a refresh is not needed because Pi/OMP roots do not depend on it);
4. `[gui] default_workspace` from gui.toml if it exists **[FLAG: added; an explicit config value beats remembered
   state]**;
5. the last used workspace from `state.json` if `[gui] restore_session` (default true) and it exists;
6. none.

The workspace sets `HistoryEnv::workspace_dir` (core uses it for the Workspace scope filter, Pi's
`<workspace>/.pi/settings.json`, delete-empty Local scope, show-dir, resume/fork launch dir, agent Local scope).
**Relative paths inside `PI_CODING_AGENT_DIR`, `PI_CODING_AGENT_SESSION_DIR`, `PI_CONFIG_DIR`, `XDG_DATA_HOME`
resolve against the launch cwd** (`HistoryEnv::env_path_base`, core gap K16), never against a workspace chosen
later, so a variable set by the launching shell keeps its meaning. Changing the workspace at runtime triggers a
background reload (§11.20) and re-filters.

Workspace picker (toolbar dropdown + File → "Choose workspace…" + palette): sections *Recent* (up to 10 from
state.json, most recent first, missing dirs hidden), *Projects in loaded conversations* (distinct existing
`project_path` values of the corpus, newest first, max 30 — computed in the background after each load/refresh and
cached on ListPane, never inside the menu builder), then "Choose folder…" (native directory picker through
`PlatformServices`), "Clear workspace". The scope toggle (All/Workspace) is disabled with tooltip
`Choose a workspace to filter by project` when there is no workspace.

### PD4 Resume / Fork (core owns planning and terminal command construction)
* Launch `claude` / `pi` / `omp` in a **new terminal window**; the GUI stays open. A toast names the conversation
  and the command: `Opened "Fix login bug" in Windows Terminal` + muted second line `claude --resume 3f2a…`.
  Errors are toasts with recovery actions (§12).
* **Planning** = `core::resume::plan_resume(env, conv, workspace, fork, default_args, &ProgramOverrides)` (landed).
  It decides the strategy (InProject / InWorkspace / CopyToWorkspace / Native), the cwd, the optional `CopyPlan`
  with `existing_targets`, and warnings. Without a workspace a plan that needs one fails with core's
  `WORKSPACE_REQUIRED_RESUME` / `WORKSPACE_REQUIRED_FORK` text; the toast offers "Choose workspace…".
* **Terminal command** = core `resume::terminal` (landed, Windows quoting verified):
  `ResumePlan::launch_spec(env)` → `terminal::prepare_terminal_command(env, &spec, &LaunchOptions)`. Launchers:
  Windows `wt.exe` (with `-w new`, gap K17) → console `cmd.exe /C start`; macOS Terminal.app `.command` script or
  `osascript`; Linux `$TERMINAL`, then the detection list (x-terminal-emulator, gnome-terminal, ptyxis, kgx,
  konsole, xfce4-terminal, tilix, alacritty, kitty, wezterm, xterm; K17 extends core's list); or a user template
  (`{cwd}`, `{args}`, `{cmd}` placeholders, core syntax). **[FLAG: the orchestrator's raw `CreateProcessW`
  fallback is dropped; core's strategy never uses `std::process::Command` + `CREATE_NEW_CONSOLE` (the broken case
  the platform spike found) — `wt.exe` is a launcher and `cmd /C start` creates the console; both were verified.]**
* **Spawning** happens only in `PlatformServices::spawn_terminal` (§7.4), which calls core
  `terminal::spawn_terminal_checked(&cmd, 3 s)` (K17): a spawn error tries the next launcher candidate
  (`terminal::launcher_candidates`, K17); an early non-zero exit (e.g. `open -a iTerm` without iTerm) is an error
  toast; the child is always reaped. The GUI never calls `core::resume::launch` (it copies without asking) — CI
  safety lint (§16.2).
* Terminal configurable in gui.toml `[gui] terminal` with **core's syntax** (`auto`, `wt`, `cmd`/`console`,
  `terminal`, `osascript`, a Linux emulator name, or a template) (§13.2). Program resolution through
  `HistoryEnv::resolve_program` (PATH/PATHEXT, `.cmd` shims, `[gui] claude_path|pi_path|omp_path` overrides).
* A CopyToWorkspace plan writes into `~/.claude/projects` and **must** be confirmed in the "Copy conversation to
  workspace" dialog (§5.8) that lists every file/dir to copy (source → destination) and every `existing_target`
  that will be overwritten. The copy runs only through `ResumePlan::execute_copy()` after that confirmation (core
  skips same-file copies).

### PD5 Picker mode
* Picker mode = launched with `--show-path`, `--show-id` or `--resume [--fork-session]` (core
  `GuiLaunchArgs::picker()`). A banner above the list shows `Picker mode: Ctrl+O or Select prints the session ID
  and exits` (text varies: "path", "session ID", "resumes the conversation") with a primary **Select** button.
* Ctrl+O / Select on the selected row (target captured at that moment, pending preview flushed):
  * `--show-path` / `--show-id` → `core::settings::picker_output(action, conv)` + `\n` to stdout, exit 0;
    non-UTF-8 stem → stderr `Error: Failed to run Claude CLI: Conversation filename is not valid Unicode`, exit 1;
  * `--resume [--fork-session]` → PD4 plan + confirmations. On macOS/Linux when stdin and stdout are TTYs the GUI
    closes its windows and `exec`s the resolved command in the launching terminal (exact TUI parity, through
    `PlatformServices::exec_in_current_terminal`); otherwise, and always on Windows, it opens a new terminal window.
    Then exit 0 **[FLAG: Windows GUI-subsystem processes cannot take over the parent console]**.
* Esc on an empty query exits 0 with no output; **closing the main window (or Quit) exits 0 with no output**, even
  if other windows are open. File → "Open JSONL file…" is disabled in picker mode (no extra windows).
* Without picker flags, Ctrl+O = "Print and exit" (TUI `Action::Select`), adapted to the desktop **[FLAG: refined;
  Ctrl+O is the desktop "Open" key]**:
  * stdout **redirected** (pipe/file) → print immediately and exit 0 (scripting parity);
  * stdout is a **terminal** → AlertDialog `Print "<title>" to the terminal and quit?` [Cancel] [Print and quit]
    (Enter = Print and quit; not destructive), then print and exit 0;
  * **no stdout** (launched from a desktop shortcut/Finder; Windows `AttachConsole` fails) → info toast
    `Nothing to print to: start the app from a terminal, or use Export…` with an "Export…" action.
  * The printed text is the ledger export (`core::export` Ledger, no ANSI) or, with `--plain` /
    `[display] plain = true`, the CLI plain format (`core::export::plain_display`). Options =
    `core::settings::EffectiveSettings` resolved **at startup** (`show_tools` from `--show-tools`/`--no-tools`/
    `[display] no_tools`, `show_thinking` from the flags/config), never the live viewer toggles (A-060).
  * With `--debug LEVEL`, in upstream order (`src/main.rs:519-551`, `src/display.rs:504-541`), all stderr lines
    through `core::debug::{info,warn,error}(Some(level), …)` so the level filter applies: (1)
    `core::maintenance::log_parse_errors(env, conv)` → `[INFO] Logged N parse error(s) to <GUI debug.log path>`
    (`[dev]`: the real GUI path instead of upstream's literal `~/.local/state/claude-history/debug.log`) or
    `[WARN] Failed to write parse errors to log: <e>`; (2) `core::export::display_parse_errors(path)` (K11) → per
    malformed line `[ERROR] Failed to parse line N: <e>` and a `=== Display Parse Error: … ===` block appended via
    `core::debug_log::log_display_error`; (3) print stdout (A-048, A-059).
* Windows release builds use the windows subsystem; stdout handling in §14.3.

### PD6 Quit, close and Esc
* Outside picker mode Esc never quits: it clears the query, closes dialogs/menus, or unwinds the viewer (PD2).
* **Quit** = `secondary-q` (Ctrl+Q / Cmd+Q, bound with no context and handled by a global `cx.on_action`, so it works
  while a dialog or nothing has focus), File → Quit, macOS app menu Quit, **closing the main window** (its
  should-close handler calls `cx.quit()`; single-file windows close with it). Picker mode: Quit = exit 0, no output.
* **Close window** = `secondary-w` bound **only in `SingleFileWindow`** (and `alt-f4` on Windows/Linux with no
  context → closes the active window; for the main window that is Quit). Ctrl+W in any text input is "delete word"
  on Windows/Linux (§8.4); it never closes a window.
* If the app was started with `FILE` (no main window), File → "Show conversations" opens the main window; closing
  the last single-file window then quits (quit mode `LastWindowClosed`).
* Ctrl+C is Copy everywhere (Input copy, viewer text selection). Never quit.

### PD7 Viewer rendering
See §9 (B-first hybrid from the viewer spike), refined: large blocks are split into ≈100-line chunk items; truncation is counted in ledger rows at the estimated column width (core K9); tabs are
expanded (K10); the focused message is marked by a background tint plus a thin 2 px accent bar **[FLAG: DG §9.5
forbids a leading-edge bar as the only selection marker; the tint is the marker, the thin bar keeps PD7's gutter]**.

### PD8 Export and copy
* Export (`e`, Session → "Export…"): the TUI menu (Ledger (formatted), Plain text, Markdown, JSONL (raw); keys 1–4,
  j/k/↑/↓, Enter, Esc) then the native save dialog through `PlatformServices::prompt_save_path(dir, name)`, default
  name `core::export::default_export_file_name(format, now)`, default dir = state.json last export dir, else
  `<home>/Downloads` if it exists, else `<home>`.
  * If the returned path has no extension, the format's extension is appended; if `<path>.<ext>` then exists, an
    AlertDialog `Replace "<name>"?` [Cancel] [Replace] (danger; Enter does not confirm) asks first (the native
    overwrite prompt only covered the typed name).
  * If the dialog cannot be shown (Linux without the xdg FileChooser portal → `Err`), error toast
    `Can't open the save dialog: <e>` with action **Export to Downloads** (writes the default name into the default
    dir, then the normal success toast).
  * Writing runs off the UI thread (`core::export::export_to_path`). Toast `Exported to <full path>` with a
    **Reveal** action (autohide on). Last export dir is persisted.
* Copy to clipboard (`y` when message navigation is inactive; Session → "Copy to clipboard ▸"): same four formats →
  `Copied to clipboard`.
* Copy actions: focused message (`y` with nav active; `core::view::ViewerModel::message_text`), path (`Y`),
  session ID (`I`, file stem — TUI parity, `ViewerModel::session_id`), and "Copy header session ID" (shown when the
  Pi/OMP header id differs from the file stem). Show path (`p`) → toast with the full path and Copy/Reveal buttons.

### PD9 Theme
* Default follows the OS appearance live (`Theme::sync_system_appearance` + `observe_window_appearance`, idempotent).
  Override System/Light/Dark in View → Theme, the palette, and Settings. The preference is stored in **gui.toml
  `[gui] theme`** (core `GuiSettings.theme`), written through the comment-preserving change-set save (K12)
  **[FLAG: PD17 listed theme in state.json; two stores would conflict]**. While gui.toml cannot be parsed, theme
  changes apply to the session only with a warning toast (§11.19).
* On every change: `Theme::change`/`sync_system_appearance` (gpui-kit chrome), then `core::theme::set_theme(kind)`,
  then viewers invalidate theme-keyed caches and re-render; menus/palette rebuilt (check marks).

### PD10 Clipboard
gpui clipboard only, always through `PlatformServices::clipboard_write/read` (so QA never touches the user's real
clipboard). `CLAUDE_HISTORY_CLIPBOARD`, `SSH_*`, OSC 52, `wl-copy/xclip/xsel` are not used; if
`CLAUDE_HISTORY_CLIPBOARD` is set, one Info line `CLAUDE_HISTORY_CLIPBOARD is ignored by the GUI` goes to the debug
sink. The TUI strings "Sent to terminal clipboard" never appear.

### PD11 Deep Search (agent protocol)
Left-pane tab **Deep search** (wider pane, PD1) with every `agent search` option (common options visible, the rest
behind an "Options" disclosure), results with evidence as compact cards, open-at-hit, Cancel; plus the viewer's
agent panel for `within`, `outline`, `read`. Every result set is tied to the conversation(s) it was computed for
(§11.14). Every result view has a "Show agent text output" toggle showing the exact CLI text and a Copy button.
Large text views use a read-only `Textarea`. Details §5.4, §11.14, D-001…D-036.

### PD12 Tools menu
Delete empty transcripts…, Generate semantic cache…, Clear semantic cache…, Debug search…, Semantic debug search…,
Debug log…, Show projects directory, Delete conversation by session ID…, Open JSONL file… (also under File), Check
for updates… (also under Help), Settings…. Keyboard shortcuts (F1, and `?` when not typing) is under Help. All are
in the command palette. Details §5.10–§5.18, §5.20, T-001…T-024.

### PD13 Settings
The Settings dialog edits **only** gui.toml: Save writes just the keys the user changed, through core's change-set
save (K12) which re-reads the file, preserves comments/formatting and keeps keys changed elsewhere (e.g. the View
menu theme) in the meantime. Each field shows the effective value and where it comes from (`gui.toml`,
`config.toml`, `remembered`, or `default`) and how it applies (§13.2), with "Reset" (remove the gui.toml key). Pages:
General, Display, Search, Keys, Resume, Agent. Key bindings are validated (core parser) and re-installed live. While
gui.toml cannot be parsed, Save is disabled with the parse error shown and buttons "Open gui.toml" / "Reveal".

### PD14 Single-file mode
`claude-history-gui FILE` (or `--render FILE` with no stdout redirection, C-011) opens only a viewer window;
File → "Open JSONL file…" opens an additional viewer window (not in picker mode). Resume/Fork/Delete/Rename are
unavailable there (TUI parity; their hints are hidden — fix of V quirk 9). Esc/`q` close the window (the app quits
when it was the last window and no main window exists). Semantic search is off. Header from
`core::history::process_conversation_file`. Unreadable/empty files show an error state with the reason
(`core::browser::Browser::single_file_error` semantics, ADD D1). The agent panel works only for files inside the
history roots (D-035).

### PD15 Safety
Every dialog, menu, palette command, agent result set and viewer action captures its `ConversationTarget` (path,
`Option<Source>`, session id, file stem, title, project) when it opens; confirmations name the target in the title;
delete derives the source from the captured target and runs core's exact plan (K4); a target with unknown source
disables destructive and resume actions. Deleting a conversation by any route (session action, delete-by-ID,
delete-empty) closes every dialog and result set holding that path and clears the viewer (§11.9).

### PD16 Status messages
TUI 3-second status messages become gpui-kit notifications (`Notification::info|success|warning|error`). In-app
wording follows the copy rules (§0, §12) **[FLAG: refined from "same texts": `…`, "conversation"; the meaning of
every TUI status is kept and its origin listed in §12]**. Errors are never silent: delete failures, rename failures,
semantic errors (full text), conversation not found, terminal launch failures, file write failures, config parse
errors. Feedback is not duplicated: semantic activity and refresh progress live in the list footer only (no start
toast); a successful rename shows no toast because the row and header update in place. Repeated status toasts use
`Notification::id1::<T>(key)` so a new one replaces the previous one; toasts with an action set `autohide(true)`
explicitly.

### PD17 Persisted UI state
`<home>/.local/state/claude-history-gui/state.json` (separate from gui.toml; never the CLI's files): window bounds per
window kind, left-pane widths (conversations / deep search), agent-panel width/visibility/tab, focus-viewer mode,
active left tab, workspace + recents, last search mode, last tool display, show thinking, show timing, last export
dir. **Precedence** for the view defaults and the list mode: CLI flag > config key (gui.toml > config.toml) when set >
remembered value from state.json (only when `[gui] restore_session`) > built-in default **[FLAG: refined; a
remembered value must never mask a config key]**. The time filter is **not** persisted (every launch starts
unfiltered unless `--since/--after/--before` is given — upstream behavior). Theme is not here (PD9). Writes are
debounced (500 ms) and flushed on quit; a corrupt file is renamed to `state.json.bad` and defaults are used (warning
in the debug sink).

### PD18 English UI; quirk decisions
English UI. TUI quirks marked `[fix]` in the inventories are fixed; viewer quirks (VIEW §26) are kept for parity
unless listed as fixed in FEATURES (decision column). Ranking, filters, texts that leave the app, and semantics are
preserved.

### PD19 QA without OS input injection
See §15. Tests and QA runs are hermetic: core's guards are compiled in for every GUI test build (`hermetic` feature, K13), all side effects go through `PlatformServices`, QA and debug-build runs
refuse a non-sandbox home.

### PD20 CI / release
See §16. Windows x86_64 (static ORT), macOS arm64 and Linux x86_64 (bundled dynamic ORT as upstream does), frozen
artifact names, toolchain pinned.

### 2.1 Additional decisions this design makes (not in the PD list)
| # | Decision | Reason |
|---|---|---|
| AD1 | Search input keeps keyboard focus in the Conversations pane; list rows never take focus (spike-list). | TUI feel: typing always edits the query; arrows move selection. |
| AD2 | `Ctrl+A` in text inputs = Select all on Windows/Linux (desktop), not "cursor home" (TUI). macOS keeps gpui-kit's emacs bindings (Ctrl+A/E = line start/end; Cmd+A = select all). Other emacs keys (Ctrl+E/B/F/K/U/W, Alt+B/F) keep TUI meaning in the list search. | ADD C7; desktop norm. |
| AD3 | `Home`/`End` in the list = first/last row (TUI), via `ConversationList > Input` duplicates. Caret home/end: `ctrl-home`/`ctrl-end`. | TUI parity. |
| AD4 | PageUp/PageDown move by the number of visible rows; Ctrl+D by half of that. | LIST quirk 4 [fix]. |
| AD5 | Mouse wheel scrolls the list viewport (does not move the selection). | Desktop convention; virtual list (LIST quirk 5 [fix]). |
| AD6 | Viewing (click/Enter → viewer) is allowed while loading; Resume/Fork/Rename/Delete/Ctrl+O stay disabled while loading (hints dimmed). | Capture-at-open makes viewing safe; destructive parity kept. |
| AD7 | In-viewer search matches over the rendered segment text (no ledger chrome) and counts occurrences, `i/n` = occurrence index; state machine = core `view::ViewSearch<SegMatch>`. | PD7; fixes VIEW quirk 12. |
| AD8 | Rename (F2) and Refresh also work from the viewer (additive). | Menus work everywhere; no conflict. |
| AD9 | Generate semantic cache always uses the whole corpus (fresh load, no time filter), so core's `GENERATE_WITH_TIME_FILTER_ERROR` never triggers. | The GUI loads independently of the list filter. |
| AD10 | Timestamps older than the current calendar year render as `%b %d %Y, %H:%M` (TUI: `%b %d, %H:%M`). | spike-list follow-up; additive clarity. |
| AD11 | The TUI help overlay becomes the Shortcuts dialog generated from the live keymap, keeping the TUI row labels. | PD12/§8.7. |
| AD12 | Tool-output expand/collapse: click the "(N more lines…)" row or the header chevron; keyboard `x` toggles all expandable outputs of the focused message. | Body clicks conflict with text selection; keyboard reachability. |
| AD13 | Links in transcripts: tooltip shows the URL; click opens a confirm dialog "Open link?" (Open / Copy link / Cancel). | ADD D5; transcripts are untrusted. |
| AD14 | One-shot CLI flags split by nature: **print** commands (`--show-dir`, `--render FILE` when stdout is redirected, `agent …`) run headless and print exactly like upstream; **interactive/destructive** ones (`--delete`, `--debug-search`, hidden semantic flags, `delete-empty`, `update`) open the matching Tools dialog prefilled after start; nothing destructive runs without confirmation (`--yes` is ignored). | Keeps scripting uses (`cd "$(…--show-dir)"`, `--render f \| less`, the agent protocol) and makes the rest "a GUI". |
| AD15 | Command palette (`secondary-shift-p`, `f10`) lists every menu command with live shortcuts and enabled state. | AppMenuBar has no keyboard activation. |
| AD16 | F6 / Shift+F6 cycle the regions: list search → list toolbar → viewer → agent panel (if open) → deep search (if active) → back. | DG §5.2 (Tab exits a region). |
| AD17 | Frequent menu-only commands get default shortcuts: Choose workspace… `secondary-shift-w`, Reveal in file manager `secondary-shift-r`, Search within conversation… `secondary-shift-i`, Read range… `secondary-shift-m`, Settings `secondary-,`, Debug log `secondary-shift-l`. | Keyboard-first (DG rule 12). |
| AD18 | Destructive confirmations ignore Enter; `y`/`Y` confirms and `n`/`N`/Esc cancels (TUI parity), clicking the danger button confirms. | Upstream accepted only y/Y; a stray Enter must not delete. |
| AD19 | Rename dialog keys: Ctrl+U clears the whole field, Ctrl+E end, Ctrl+Home/Ctrl+End start/end (TUI); Ctrl+A select all on Windows/Linux (AD2). | TUI parity within desktop norms. |
| AD20 | List row = three text lines (title/time, meta, preview) plus the literal-context line in literal mode (§10.1). | The TUI single-line header is unreadable at pane widths (spike-list). |

---

## 3. Architecture overview

```
claude-history-gui/ (Cargo workspace, resolver 3, edition 2024)
  crates/core   package claude-history-core  lib claude_history_core   (UI-agnostic, no gpui; features: release-dynamic-ort, hermetic)
  crates/gui    package claude-history-gui   lib claude_history_gui (src/lib.rs, all modules)
                                             bin claude-history-gui (src/main.rs, thin: args → lib::run)
  .cargo/config.toml   ([env] RUST_MIN_STACK = "8388608"; /STACK:8000000 comes from both build.rs files)
  rust-toolchain.toml  (channel = "1.99.0", components rustfmt, clippy)
  .github/workflows/{ci.yml, qa.yml, release.yml}
  packaging/{macos,linux,windows}/...
  tools/{qa,ci}/*
  tools/dev/*          (agent worktrees: seed-target.ps1, linux-test.ps1 for Linux builds in Docker; AGENTS.md)
```

The GUI crate is a **library plus a thin binary** **[FLAG: refined from bin-only]**: public items of the library are
not dead code while parallel work packages leave consumers unmerged, `cargo clippy -D warnings` stays meaningful,
and `crates/gui/tests/` can hold integration fixtures.

Dependency rule: `gui → core`, never the reverse. The GUI calls `std::env::current_dir()` once in `main` (launch
cwd), `HistoryEnv::from_process_env()` once in `main`, and reads no other process state outside `main.rs`,
`services.rs` and `qa/gate.rs` (enforced by the CI safety lint, §16.2).

Runtime structure:

```
main.rs ── core::settings::parse_launch_args (+K14 additions) ──► LaunchCommand
   ├─ Help / Version / SubcommandHelp(text) / Agent(AgentCli::Help(text)) / usage error
   │        ──► console::write (AttachConsole on Windows) ──► exit 0 / 2
   ├─ Agent(AgentCli::Request(request)) (K15) ──► core::agent::execute(env, config, request) ──► stdout/stderr/exit code (headless)
   ├─ DeleteEmpty { local, .. } / Update ──► Run(GuiLaunchArgs { local, .. default }) with StartupTool::DeleteEmpty { local } / CheckForUpdates
   ├─ --show-dir, --render FILE with stdout available (K14) ──► headless print ──► exit 0 / 1
   └─ Run(args) ──► claude_history_gui::run(launch, env)   // env.env_path_base = launch cwd (K16)
          services = Services::for_launch(&launch)        // recording for a `qa` build run with --qa-scenario
          gpui_kit::application().with_assets(AllAssets).with_quit_mode(LastWindowClosed).run(|cx| {
              gpui_kit::init(cx); app::init(cx, …)   // globals, services, keymap, menus, palette, theme, debug sink
              if launch has FILE → single_file::open_window(path, cx) else workspace::open_main_window(cx)
          })

Main window:  Root(gpui-kit) ─► Workspace (Entity, key_context "Workspace")
                 ├─ TitleBar + AppMenuBar (Win/Linux)
                 ├─ h_resizable "main-split"
                 │    ├─ left: TabBar + (ListPane "ConversationList" | DeepSearchPane "DeepSearch")
                 │    └─ right: ViewerPane ── h_resizable "viewer-split"
                 │                  ├─ transcript column (key_context "Viewer")
                 │                  └─ AgentPanel (key_context "AgentPanel")   ← sibling, not inside "Viewer"
                 └─ overlays via WindowExt (dialogs, sheets, notifications, command palette)

Single-file window: Root ─► SingleFileWindow (Entity, key_context "SingleFileWindow") ─► ViewerPane(mode=SingleFile)
```

Background work (all threads named, 8 MiB stack via `std::thread::Builder::stack_size`):
* core-owned: `history-loader` + `history-loader-forwarder` (streaming load), `corpus-prepare` (K1: sort, dedupe,
  search index off the UI thread), `corpus-release` (drops a replaced corpus off the UI thread), `search-worker`
  (lexical), `semantic-search-worker` (spawned by `core::browser::Browser`);
* GUI-owned: `viewer-worker` (one per ViewerPane: single-flight parse + document + items + segments, §7.5),
  `agent-worker` (Deep Search + agent panel, single-flight per request kind with cancellation), short-lived tool
  threads (`semantic-cache-gen`, `update-check`, `delete-empty-scan`, `uuid-load`, `rename-write`, `delete-exec`,
  `export-write`), `syntect-warmup`, `login-shell-path` (macOS/Linux).

UI-thread work is limited to state mutation of bounded size, layout and paint. Anything that can block > ~5 ms on
I/O or > ~10 ms on CPU, or is O(corpus), runs off the UI thread (§7.5, measured by §15.8).

---

## 4. Core contract

### 4.1 Core APIs the GUI uses (landed at `3ba9f90` + WP-C `78ec36c`; the GUI must not re-implement any of them)

Every package's "Consumes" list in §17 names items from this table. Signatures are abbreviated; the source is the
truth.

| Module | Items the GUI uses |
|---|---|
| `env` | `HistoryEnv { home_dir, claude_config_dir, pi_coding_agent_dir, pi_coding_agent_session_dir, pi_config_dir, omp_profile, pi_profile, xdg_data_home, path_var, pathext, workspace_dir, cache_root, config_file, gui_config_file, debug_log_file }` (+ `env_path_base`, K16: `with_env_path_base`, `launch_base_dir()`, `env_value_base_dir()`; `main` sets it to the launch cwd), `from_process_env()` (main only), `sandbox(root)`, `with_workspace`, `claude_dir()`, `claude_projects_root()`, `claude_projects_dir_for(dir)`, `cache_root()`, `semantic_cache_dir()`, `embedding_cache_file()`, `semantic_model_dir()`, `config_file()`, `gui_config_file()`, `debug_log_file()`, `workspace()`, `resolve_program(name, override)`; `WorkspaceScope { dir, project_dir_name }`, `WorkspaceScope::new`, `canonical_dir()`; `STATE_DIR_NAME`, `CACHE_DIR_NAME` |
| `config` | `ConfigFile`, `GuiConfigFile`, `GuiSettings { theme, terminal, claude_path, pi_path, omp_path, default_workspace, restore_session }`, `LayeredConfig { cli, gui_file, effective, gui }`, `load_config_from(path)`, `load_gui_config_from(path)` (loaded **independently**, §11.1), `ConfigFile::overlaid_with(&GuiConfigFile)`, `save_gui_config_changes` (K12), `KeyChord::{short_label, help_label, gpui_keystroke, config_string}`, `parse_key_binding`, `KeyBindings { resume, fork, rename, delete, refresh }` + `from_config`, `conflicts_with_ctrl_t`, `AgentScopeConfig`, `AgentConfig` |
| `settings` | `parse_launch_args(args) -> Result<LaunchCommand, LaunchArgsError>` (`LaunchCommand::{Run(GuiLaunchArgs), Help, Version, DeleteEmpty { local, all, yes }, Update, Agent(AgentCli), SubcommandHelp(String)}`; K14/K15, `SubcommandHelp` added by WP-C: `update --help`, `delete-empty -h`, `help <subcommand>` → print the text, exit 0), `GuiLaunchArgs` (+`display_flags`, `search_mode_override`, `picker`, `time_filter`, `settings`, `validate`), `help_text(program)`, `effective_settings(config, flags, mode_override) -> EffectiveSettings { show_last, show_tools, tool_display, show_thinking, search_mode, list_search_mode, exclude_projects, keys, semantic_toggle_enabled, resume_default_args }`, `EffectiveSettings::initial_view_prefs()`, `initial_tool_display`, `PickerAction::{ShowPath, ShowId, Resume{fork}}`, `picker_output(action, conv)`, `session_id_from_path(path)` |
| `browser` | `Browser` (`new_loading`, `append_conversations`, `set_query`, `escape -> EscapeOutcome`, `toggle_workspace_filter`, `set_workspace_filter`, `set_workspace`, `set_excluded_projects`, `toggle_list_search_mode`, `set_list_search_mode`, `poll`, `has_search_work_in_flight`, `filtered`, `row`, `selected`, `select*`, `select_half_page_down(viewport_rows)`, `selected_path`, `selected_conversation`, `has_multiple_sources`, `conversation_by_path`, `count_text`, `search_status_text`, `semantic_status_text`, `semantic_activity_status_text`, `semantic_error`, `semantic_progress`, `semantic_result_metadata[_for_selection]`, `lexical_evidence`, `row_preview_input`, `row_header_input`, `uuid_lookup`, `rename_prefill`, `workspace`, `has_project_context`, `list_search_mode`, `is_loading`, `loading_state`), `BrowserOptions` (+ builders, `embedder_factory`, `waker`, `semantic_available`), `HistoryLoader` (`new`, `start_initial_load`, `start_refresh`, `set_time_filter`, `is_running`, `is_refreshing`, `poll(&mut Browser) -> LoaderPoll`), `LoaderOptions`, `LoadEvent::{Loaded, NoHistory, LoadFailed, Refreshed, RefreshIncomplete, RefreshFailed, RefreshInterrupted}` (+`status_message`), `ListSearchMode`, `SemanticProgress`, `SemanticResultMetadata`, `UuidLookup`, `RenameError`, `DeleteError`, `Waker`, `SemanticEmbedderFactory`, `query_has_literals`; K1–K4, K6, K18 additions |
| `display_format` | `build_row_header(&RowHeaderInput) -> RowHeader { project, custom_title, summary, message_count, semantic_score, duration, timestamp, recency, .. }`, `build_row_preview(&RowPreviewInput) -> RowPreview { preview, source, context }`, `HighlightQuery` (`parse`, `needs_literal_context`, `match_ranges`), `HighlightedText`, `Recency`, `format_model_name`, `format_tokens`, `format_tokens_long`, `format_msg_count_long`, `format_duration_minutes`, `format_header_timestamp`, `semantic_row_metadata`, `semantic_rationale_label`, `sanitize_preview` |
| `view` | `ViewerModel { path, entries, prefs, expanded }` (`open`, `from_entries`, `cycle_tool_display`, `toggle_thinking`, `toggle_timing`, `toggle_expanded`, `document`, `export_options`, `session_id`, `message_text`, `plain_display`), `ViewPrefs`, `ToolDisplayMode` (+`next`, `status_label`), `ConversationDocument` (`navigable_indices`, `navigable_entry_indices`, `nav_position`, `message_at_nav_position`), `DocMessage`, `DocMessageKind`, `DocBlock` (`toggle_id`, `tool_output_id`, `plain_text`), `DocBlockKind`, `DocTiming`, `Fold`, `DocToolCall`, `DocToolResult`, `DocSubagentToolResult`, `ToolBodyLine`, `BodyLineKind`, `LabelRole` (+`color`), `ToolOutputId`, `fitted_name`, `NAME_WIDTH`, `TRUNCATED_BODY_LINES`, `TRUNCATED_RESULT_LINES`, `MessageNav`, `ViewSearch<M>`, `ViewSearchMode`, `escape -> ViewEscape`, `capture_anchor_entry`, `find_entry_or_prev`; K5, K9 additions |
| `export` | `ExportFormat::{ALL, from_index, index, label, extension}`, `ExportOptions`, `generate_export`, `export_to_path -> Result<(), ExportError>`, `default_export_file_name`, `plain_display(path, options)`, `exported_status`, constants `COPIED_TO_CLIPBOARD`, `MESSAGE_COPIED_TO_CLIPBOARD`, `NO_TEXT_CONTENT`, `MESSAGE_NOT_FOUND`, `PATH_COPIED_TO_CLIPBOARD`, `SESSION_ID_COPIED_TO_CLIPBOARD`; K11 |
| `resume` | `plan_resume(env, conv, workspace, fork, default_args, &ProgramOverrides) -> Result<ResumePlan>`, `plan_resume_target(…, &ResumeTarget, …)`, `ResumePlan { source, kind, strategy, session_path, program, program_override, args, cwd, copy, warnings }` (+`launch_spec(env)`, `execute_copy()`, `display_command()`), `CopyPlan { files, dirs, existing_targets, target_dir, … }`, `CopyItem`, `CopyReport`, `ResumeStrategy`, `ProgramOverrides::from_gui_settings`, `WORKSPACE_REQUIRED_RESUME`, `WORKSPACE_REQUIRED_FORK`; `terminal::{TerminalLauncher::from_setting, detect_launcher, prepare_terminal_command, build_terminal_command, prepare_launcher_command, LaunchOptions, LaunchSpec, TerminalCommand (+display), Platform}`; K17. **Forbidden in the GUI**: `resume::launch` (copies without confirmation) and direct `terminal::spawn_terminal` outside `services.rs` |
| `maintenance` | `find_empty_candidates(env, &DeleteEmptyScope) -> Vec<EmptyCandidate>` (+`modified_within`, `cli_lines`), `delete_empty_candidates(&[EmptyCandidate]) -> DeleteEmptyOutcome { deleted, skipped, failure }`, `delete_empty_summary_line`, `format_delete_empty_report`, `find_session_copies(env, uuid) -> Vec<SessionCopyFiles>`, `delete_session_by_uuid(env, uuid) -> DeleteByUuidReport`, `delete_by_uuid_message`, `show_dir(env, workspace)`, `WORKSPACE_REQUIRED_SHOW_DIR`, `load_sorted_conversations(env, show_last, debug, &time_filter)`, `debug_search_report(convs, query, now, workspace) -> DebugSearchReport` (+`to_text`), `DEBUG_SEARCH_TOP`, `parse_error_report(conv)`, `log_parse_errors(env, conv)`; K4 (`plan_session_delete`, `execute_session_delete`, `DeletePlan { source, path, session_id, files, dirs }`, `DeleteReport`, `SessionDeleteFailure { error, partial }`) |
| `semantic_ops` | `generate_semantic_cache(env, convs, workspace, &time_filter, embedder, progress, &cancel) -> GenerateReport`, `GenerateProgress`, `GenerateOutcome`, `clear_semantic_cache(env) -> ClearReport` (K6 as built: empties `<cache root>/semantic` but keeps the directory and its `embeddings.lock`), `semantic_search_report(env, query, convs, top, workspace, embedder)`, `semantic_debug_report(env, query, convs, workspace, embedder)`, `semantic_status(env, convs, workspace)`, `EmbedderFactory<'a>`, `fastembed_factory(env)`, `no_conversations_message` |
| `agent` | `AgentRequest::{Search, Within, Read, Outline}`, `args::{AgentSearchArgs, AgentWithinArgs, AgentReadArgs, AgentOutlineArgs, AgentOutputFlags, AgentSearchModeArgs, TimeRangeArgs, DEFAULT_READ_CONTEXT}` (+`validate`), `AgentService::{new, with_embedder_factory, with_notice_sink, with_cancellation, respond}`, `agent::execute(env, config, request) -> AgentResponse` (headless `agent …`), `cli::{AgentCli::{Request, Help}, parse_agent_args}`, `AgentResponse { stdout, stderr, exit_code, result, warnings, error, notices }` (+`text`, `error_text`, `is_success`), `AgentResult::{Search, Within, Read, Outline}`, `AgentSearchResult { output, conversations }` (+`hits`, `conversation(ref)`), `AgentReadResult`, `AgentOutlineResult`, `AgentConversationInfo { reference, full_ref, uuid, project_id, source, session_id, session_filename, path }`, `AgentNotice::{SemanticProgress, UncachedPassages}`, `agent::service::{EmbedderFactory, discover_agent_keys}`, `refs::{AgentConversationKey::from_conversation, conversation_ref, full_ref, full_ref_for_candidate}`, `diagnostic::{AgentError, AgentWarning}`, `protocol::{ReadMessage, OutlineItem, ProtocolOptions, MessageLineRange}`; K5, K7, K8, K15 |
| `update` | `check_latest_release(current) -> UpdateStatus` (services only), `update_status_from_response`, `UpdateStatus::{UpToDate, Available, Newer, NoRelease}`, `ReleaseInfo { tag, version, name, url, notes, … }` (+`asset_for(os, arch)`), `RELEASES_PAGE_URL`, `REPOSITORY` |
| `history` | `Conversation`, `Source` (+`label`, `list_label`), `detect_source(path)`, `process_conversation_file(path, mtime, debug)`, `DeleteEmptyScope::{All, Local(WorkspaceScope)}`, `pi_loader::session_root`, `omp_loader::session_root`; K2 |
| `theme`, `syntax`, `debug`, `debug_log`, `markdown`, `time_filter`, `error` | `theme::{set_theme, kind, current, theme_for, ThemeKind, ThemePreference (+resolve), Theme, Rgb}`; `syntax::highlight_code(code, lang)`; `debug::{set_sink, reset_sink, DebugLevel (+tag), log, info, warn, error, should_log}`; `debug_log::{log_debug, log_display_error}`; `markdown::expand_tabs` (K10); `time_filter::{TimePoint, TimeFilter (+resolve, matches, is_active), TimeFilterError}`; `AppError`, `Result` |

Duplicates the GUI must **not** write (each was planned in revision 1): its own loader bridge and refresh buffer
(use `HistoryLoader`), viewer navigation/search state machines (use `MessageNav`, `ViewSearch<M>`, `escape`,
`capture_anchor_entry`, `find_entry_or_prev`), message copy text (use `ViewerModel::message_text`), picker output
(use `settings::picker_output`), view-default precedence (use `settings::effective_settings` +
`EffectiveSettings::initial_view_prefs`, with the state.json layer of PD17 on top), delete-empty scan/delete/report
(use `maintenance::*`), show-dir (use `maintenance::show_dir`), parse-error report/log (use
`maintenance::{parse_error_report, log_parse_errors}`), count/status strings (use `Browser::{count_text,
search_status_text, semantic_activity_status_text}`), row header/preview composition (use
`display_format::{build_row_header, build_row_preview}`), resume planning and terminal command construction (use
`core::resume`). Model mirror types of revision 1 (`ToolDisplayChoice`, `ExportChoice`, `ExportOpts`, `PickerMode`,
`TerminalLaunch`) are removed; the GUI uses `ToolDisplayMode`, `ExportFormat`, `ExportOptions`, `PickerAction`,
`TerminalCommand`/`LaunchSpec`.

### 4.2 Revision-1 gap list → status at `3ba9f90`

| Old | Status | Where now |
|---|---|---|
| G1 `set_workspace` | landed | `Browser::set_workspace` |
| G2 exclusions live apply | landed (renamed) | `Browser::set_excluded_projects` |
| G3 rename by path | landed; made async by K3 | `Browser::rename_conversation`, `RenameError` |
| G4 delete plan/execute | partly (`find_session_copies`, `delete_conversation`); completed by K4 | `maintenance` |
| G5 agent ordinal ↔ entry | open | K5 |
| G6 structured agent results | superseded | `agent::execute` / `AgentService::respond` → `AgentResponse` |
| G7 semantic error text | landed | `Browser::semantic_error` |
| G8 reset semantic worker | open (private) | K6 |
| G9 quirk 14/16 | landed | `Browser` (module docs) |
| G10 UUID preview mode | landed | `Browser` |
| G11 update check | landed (different shape) | `update::{check_latest_release, UpdateStatus, ReleaseInfo}` |
| G12 launch-arg parser | landed (subset); extended additively by K14/K15 | `settings::parse_launch_args` |
| G13 `--plain` text | landed | `export::plain_display`, `ViewerModel::plain_display` |
| G14 lookups | landed | `Browser::conversation_by_path` → `(usize, &Conversation)` |
| G15 delete-empty mtime | landed | `EmptyCandidate::{modified, modified_within}` |
| G16 embedder factory | landed | `BrowserOptions::embedder_factory` |

### 4.3 Core gaps K1–K18 (owned by WP-C; additive except where marked; each with core tests)

Signatures are binding for the GUI. WP-C may choose internals. Unless marked **breaking**, existing public items keep
their signatures and behavior.

| ID | Module | API | Behavior / tests |
|---|---|---|---|
| K1 | `browser` | `pub type CorpusSnapshot = Arc<Vec<Arc<Conversation>>>`; **breaking**: `Browser::conversations(&self) -> &[Arc<Conversation>]`; `Browser::corpus_snapshot(&self) -> CorpusSnapshot`; `pub struct PreparedCorpus` (Send, opaque); `Browser::prepare_corpus(conversations: Vec<Conversation>, dedupe: bool) -> PreparedCorpus` (associated fn: sort newest first, dedupe by canonical path when `dedupe`, reindex, build the search data); `Browser::install_prepared(&mut self, prepared: PreparedCorpus)` (finishes a load when loading, else replaces like `replace_conversations` incl. quirk-16 row keeping); `HistoryLoader::poll` never runs O(corpus) work on the caller's thread: at the end of a stream it moves the data to a `corpus-prepare` thread and a later `poll` installs it and returns the `LoadEvent` (`LoaderPoll::preparing: bool`); the waker fires when preparation ends | Storage: every conversation exists once (`Arc<Conversation>`), shared by the browser, the lexical worker and the semantic worker; search data likewise `Arc` per conversation; rename/remove/append update one element (O(n) pointer copies at most). Tests: `Arc::ptr_eq` between the browser's and the workers' corpus; existing finish/replace/refresh tests pass through the async path; a test that `poll` returns within 5 ms while preparation of a 20k synthetic corpus is running |
| K2 | `browser`, `history` | `UuidLookup::Pending { uuid: String }` (new variant); `Browser::set_query` never touches the disk; `history::load_session_by_uuid(env: &HistoryEnv, uuid: &str, show_last: bool) -> Result<Option<Conversation>>` (blocking: find `<uuid>.jsonl`, parse, project name, preview per `show_last`); `Browser::resolve_uuid_lookup(&mut self, uuid: &str, loaded: Option<Conversation>)` (applies `Found{loaded_from_disk:true}` / `NotFound` only if the query is still that UUID; appends incrementally) | ADD C1 semantics kept (bypasses filters, appended at the end). Tests: Pending then resolved; stale UUID ignored; not found keeps rows |
| K3 | `browser`, `history` | `history::rename_session(env: &HistoryEnv, path: &Path, source: Source, title: &str, show_last: bool) -> Result<Conversation, RenameError>` (blocking: write per source, re-parse, preview per `show_last`; Pi ensures a trailing newline); `Browser::apply_renamed(&mut self, path: &Path, renamed: Conversation) -> Result<(), RenameError>` (keeps index/project_name/project_path, updates one element and its search data, re-selects by path) | `rename_conversation` stays (sync convenience). Tests: equivalence with `rename_conversation`; `NoTarget` when the path left the corpus meanwhile |
| K4 | `maintenance`, `browser` | `pub struct DeletePlan { pub source: Source, pub path: PathBuf, pub session_id: String, pub files: Vec<PathBuf>, pub dirs: Vec<PathBuf> }` (`path` = the listed conversation, added by WP-C); `pub struct DeleteReport { pub removed_files: Vec<PathBuf>, pub removed_dirs: Vec<PathBuf>, pub missing: Vec<PathBuf> }`; `maintenance::plan_session_delete(env: &HistoryEnv, path: &Path, source: Source) -> Result<DeletePlan>` (Claude: every copy via `find_session_copies` + session dirs; Pi: the validated file; OMP: the validated file + artifact dir); **breaking**: `maintenance::execute_session_delete(env: &HistoryEnv, plan: &DeletePlan) -> Result<DeleteReport, Box<SessionDeleteFailure>>` (was `Result<DeleteReport>`; `SessionDeleteFailure { error, partial }` converts `Into<AppError>`; WP-C3) (removes only plan items; on failure writes upstream's debug-log line `Failed to delete session <path>: <e>`); `Browser::remove_paths(&mut self, paths: &[PathBuf]) -> usize` (removes rows, keeps selection position, clears semantic metadata/evidence like `remove_selected_from_list`) | A1/A2. Tests per source; missing files reported not failed; invalid uuid → `SessionNotFound` |
| K5 | `view` | `impl ViewerModel { pub fn entry_for_agent_ordinal(&self, ordinal: usize) -> Option<usize>; pub fn agent_ordinal_for_entry(&self, entry_index: usize) -> Option<usize> }` | Same placement as `history::messages::MessageOrdinals` over `self.entries`; replaced streaming duplicates map to the first entry with that ordinal. Tests against `agent::transcript` ordinals on fixtures |
| K6 | `browser`, `semantic` | `Browser::suspend_semantic(&mut self)` (cancel, close the channel, **join** the worker thread, drop its embedder; while suspended semantic dispatch reports `SemanticProgress::Suspended` and `semantic_status_text()` = `"sem paused"`); `Browser::resume_semantic(&mut self)` (next dispatch respawns the worker, reloading the cache file; re-dispatches the current semantic query); cache writes take an exclusive `std::fs::File::lock` on `<semantic dir>/embeddings.lock` (wait ≤ 2 s, else skip the write with a Warn debug line); `semantic_ops::{generate_semantic_cache, clear_semantic_cache}` hold the same lock while writing/deleting | Single writer per process and across processes. Tests: suspend joins (thread gone), resume reloads a cache written meanwhile, lock contention skips without corrupting |
| K7 | `agent` | `AgentService::with_cancellation(self, token: SemanticCancellationToken) -> Self`; checked between conversations, retrieval batches and embedding batches; cancelled → `AgentResponse` with `exit_code 1`, `error: None`, `stderr` = `Error: Semantic search cancelled\n` | Tests: a cancelled long fake-embedder search ends early |
| K8 | `agent` | `agent::refs::conversation_key_for_path(env: &HistoryEnv, config: &ConfigFile, path: &Path) -> Result<Option<AgentConversationKey>>` — the discovered key whose canonical path equals `path`'s, else `None` | Out-of-root files (exports, attachments) and same-named copies never resolve to another file. Tests: in-root file → key; copy outside → None; same name in another root → None |
| K9 | `view` | `DocToolResult::ledger_visible_rows(&self, content_width: usize) -> Vec<String>` (the rows the ledger shows at that width in the current fold: hard-wrapped like the ledger, at most `limit` rows when collapsed) | Visual-row truncation for soft-wrapping GUIs (a 50 KB single-line result shows ≤ 4 rows). Tests at widths 40/120 |
| K10 | `markdown` | `pub const TAB_WIDTH: usize = 8; pub fn expand_tabs(input: &str, start_col: usize, tab_width: usize) -> String` (the existing private helper, display-width aware) | Tests (moved) |
| K11 | `export` | `pub struct DisplayParseError { pub line_number: usize, pub error: String, pub line: String }`; `export::display_parse_errors(source: &Path) -> std::io::Result<Vec<DisplayParseError>>` (upstream `process_raw_log_entries` semantics: raw JSONL lines that do not parse as `LogEntry`, also for Pi/OMP files) | Tests on a malformed fixture |
| K12 | `config` | `config::save_gui_config_changes(env: &HistoryEnv, base: &GuiConfigFile, edited: &GuiConfigFile) -> Result<PathBuf>`: re-reads gui.toml (missing → empty document); if it does not parse as `GuiConfigFile` → `ConfigError("gui.toml has errors; fix or remove it before saving: <e>")`; for each leaf key whose value differs between `base` and `edited` sets or removes that key in a `toml_edit::DocumentMut` (comments, ordering and untouched keys preserved, also keys changed by other writers since `base`); validates the result by parsing; atomic write with the existing `config.toml` refusal checks | `toml_edit` is already in the dependency tree through gpui-kit. Tests: comment preserved; concurrent theme change kept; unparseable file refused and unchanged (byte compare) |
| K13 | crate features | `[features] hermetic = []`: every `#[cfg(test)]` guard becomes `#[cfg(any(test, feature = "hermetic"))]`: `HistoryEnv::from_process_env` panics; the semantic worker's default factory and `semantic_ops::fastembed_factory` refuse; `update::check_latest_release` panics; `resume::terminal::spawn_terminal`/`spawn_terminal_checked` return `Err(Unsupported)` | Enabled only from `crates/gui` `[dev-dependencies]` (resolver 3 unifies it into test builds only). Test: a `hermetic` feature build of core passes the same guard tests |
| K14 | `settings` | Additive `GuiLaunchArgs` fields `plain`, `render: Option<PathBuf>`, `show_dir`, `delete: Option<String>`, `debug_search: Option<String>`, `semantic_search: Option<String>`, `semantic_top: Option<usize>`, `debug_semantic_search: Option<String>`, `generate_semantic_cache`, `clear_semantic_cache`, `global`, `pager`, `no_pager`, `no_color`; additive `LaunchCommand::{DeleteEmpty { local: bool, all: bool, yes: bool }, Update, SubcommandHelp(String)}` (`help` is a subcommand: `help` alone = `Help`, `help update\|delete-empty\|help` and `<subcommand> --help` = `SubcommandHelp(text)`, `help agent [cmd]` = `Agent(AgentCli::Help)`); upstream conflict groups (`one_shot_action`, `selection_conflicting_action`, `--delete` × `--global`, `--semantic-top` requires `--semantic-search`), validator messages `value must not be empty`, `value must be greater than zero`, `invalid positive integer: <v>`; `help_text` lists them (hidden flags stay hidden) | The inverted time range stays a runtime error of `time_filter()` (GUI main prints `Error: Invalid time range: …`, exit 1). Table test of every flag and conflict pair (C-016) |
| K15 | `agent` | `agent::cli::parse_agent_args<I, T>(args: I) -> Result<AgentCli, LaunchArgsError>`; `pub enum AgentCli { Request(AgentRequest), Help(String) }`; `LaunchCommand::Agent(AgentCli)` (additive) | Upstream clap surface of `agent search\|within\|read\|outline` (flags, aliases, value names, help texts of ADD E4, error texts, exit code 2). Tests ported from upstream `cli.rs` agent tests |
| K16 | `env`, `history` | `HistoryEnv.env_path_base: Option<PathBuf>` (the launch cwd; `from_process_env` leaves it `None`, `main` sets it); Pi/OMP loaders resolve relative `PI_*`/`XDG_DATA_HOME` values against `env_path_base`, falling back to `workspace_dir`; `<workspace>/.pi/settings.json` and its relative `sessionDir` stay workspace-relative | Tests: relative env value with base A and workspace B resolves under A |
| K17 | `resume::terminal` | Linux list extended (`ptyxis`, `kgx`, `tilix`; `gnome-terminal` keeps `--`); `wt.exe` gets `-w new`; `pub fn launcher_candidates(env: &HistoryEnv, options: &LaunchOptions) -> Vec<TerminalLauncher>` (the configured launcher, else every detected launcher in order; Windows: Windows Terminal then console); `pub fn prepare_launcher_command(env, &TerminalLauncher, &LaunchSpec, &LaunchOptions) -> Result<TerminalCommand>` (WP-C3: defaults the cwd to the home directory and writes a fresh macOS launch script path per call); `pub enum SpawnOutcome { Running, Exited(i32) }`; `pub fn spawn_terminal_checked(command: &TerminalCommand, wait: Duration) -> std::io::Result<SpawnOutcome>` (spawns, waits up to `wait` for an early exit, reaps the child on a thread); `examples/terminal_smoke.rs` (spawns a terminal running `touch <marker>`; used by CI smoke jobs only) | Tests on the built commands for every launcher (pure); no spawning in tests |
| K18 | `browser` | `pub enum RefreshPolicy { KeepOnProjectError, ApplyPartial }`; `HistoryLoader::start_refresh_with(&mut self, policy: RefreshPolicy) -> bool`; `LoadEvent::Refreshed { old, new, project_errors: usize }` (additive field; `status_message` unchanged) | Time-filter and workspace reloads apply partial results with a warning instead of keeping rows loaded under the old filter (§11.20–11.21). Tests: ProjectError + ApplyPartial → Refreshed with `project_errors = 1` |

**As built (WP-C, merged in `78ec36c`).** Where the merged core differs from or refines the rows above (binding for
the GUI; the source is the truth):
* **K1**: every rename/remove the browser applies is stamped from a process-wide counter; `start_initial_load`,
  `start_refresh_with` take a mark before spawning the reader, and the install replays every edit made since that mark
  (removed paths dropped; renamed paths keep the browser's current version with the load's project name/path; the last
  edit of a path wins). While no load is tracked the browser keeps only the newest 256 edits; tracking stops when a
  refresh ends without installing. The GUI still polls on the waker as specified; nothing else changes for it.
* **K2**: when a corpus is replaced (refresh/reload) and the current UUID result is `Found` for a session the new
  corpus lacks, core re-appends that session (with its search data; it stays the only selected row and keeps
  `loaded_from_disk`) instead of re-reading the file; `NotFound` stays `NotFound`, `Pending` stays `Pending` until
  the GUI's in-flight lookup resolves it. A deleted session is not carried. If the file changed between the jump and
  the refresh, the row shows the jump-time version. No GUI change is needed (§11.5).
* **K4**: `DeletePlan` has the extra field `path` (the listed conversation; additive for readers, breaking for struct
  literals — GUI code gets plans only from `plan_session_delete`). A failed delete logs `Failed to delete session
  <listed path>: <e>` (matches `DeleteError::Failed`). `execute_session_delete` re-validates the plan and refuses
  (`SessionNotFound`, nothing removed) anything `plan_session_delete` could not have produced: Claude files must be
  `<projects root>/<plain project>/<id>.jsonl` and dirs `<id>` beside a planned file; Pi/OMP exactly one file equal to
  `plan.path`, still a Pi/OMP session when it exists; Pi no dirs, OMP at most `[session_artifact_dir(file)]`.
* **K6**: `<cache root>/semantic/embeddings.lock` stays inside the semantic directory, so Clear **empties** the
  directory and keeps the directory and the lock file (removing them would break the lock's exclusivity); it never
  follows symlinks/junctions (a linked semantic dir: only the link is removed; links inside are unlinked, targets
  kept). `ClearReport.removed`/`message` keep upstream's semantics: `Semantic cache cleared.` when something was
  removed, `Semantic cache is already empty.` otherwise (a second Clear).
* **K12**: both sides of the diff treat a missing `[gui]` table as `GuiSettings::default()`, so a theme-only save
  writes only `theme`, and concurrent changes of other keys are kept.
* **K13**: `FastembedEmbedder::new_in` itself refuses under `any(test, feature = "hermetic")` with
  `semantic::fastembed::HERMETIC_REFUSAL`, before creating anything (covers the GUI's shared embedder, §7.6).
* **K14**: `help` is a subcommand and `LaunchCommand::SubcommandHelp(String)` is additive (see the K14 row);
  `LaunchCommand::Help` keeps meaning the top-level help. Unknown subcommands → `unrecognized subcommand '<x>'`;
  `--local help` is a conflict; `-- help` is still a FILE. `help_text` lists `help`.
* **K16**: a relative `XDG_DATA_HOME` resolves against `env_value_base_dir()` (launch dir, else workspace), like the
  `PI_*` values, and is ignored only when neither exists.

WP-C acceptance: `cargo test -p claude-history-core` and `cargo test -p claude-history-core --features hermetic`
green on Windows; clippy clean; each K item has the tests listed; existing behavior unchanged except where marked;
cache formats unchanged. WP-C also verifies (and writes when missing) the core tests named by every FEATURES row
owned by WP-C (§17.2).

### 4.4 Core change requests after WP-C

A GUI package that finds a missing or wrong core API writes a **CCR** (core change request) in its hand-off note:
the API, the reason, a test. The integrator batches CCRs into a short `WP-C2` task (one agent, `crates/core` only,
additive, same acceptance as WP-C) and merges it before the requesting package's next iteration. GUI packages never
edit `crates/core` and never add GUI-side copies of core logic as a workaround; until the CCR lands they keep the
dependent row open in the hand-off note.

CCRs after Phase A (C-033 optional `[gui] theme`/`restore_session`, CCR-1 off-thread Browser install, CCR-2 indexed
`apply_renamed`/`remove_paths`, CCR-3 `has_search_work_in_flight` on an empty semantic corpus) landed in Phase B
(WP-C2). Open CCRs after Phase B (details in §17.11): none; the three CCRs (`resume::terminal` per-launcher preparation
helper, `execute_session_delete` partial report, full refs in `AmbiguousRef` candidates) were done in WP-C3.

---

## 5. Windows, screens and dialogs

Sizes use rem helpers (DG §6); `px` only for window bounds/min size. Chrome colors come from `cx.theme()`; content
colors (ledger labels, list fields, diff, code) come from `theme_tokens::roles()`, which takes a gpui-kit token
wherever one exists (selection, list_active, warning/danger, accent, muted, border, foreground) and core's terminal
palette (`core::theme::current()`) only for roles gpui-kit lacks (label roles, diff add/remove, code fallback,
recency colors). Every foreground/background pair a role produces passes a ≥ 4.5:1 contrast test in both themes
(§15, X-039). No literals at call sites.

### 5.1 Main window

Window options: `WindowOptions { window_bounds: restored-or-centered(1280×820), window_min_size: 880×560, app_id:
Some("claude-history-gui"), ..TitleBar::window_options() }`, Linux `window_decorations: Some(WindowDecorations::Client)`.
Title (`set_window_title`): `Claude History — <workspace short name | All projects>`; picker mode appends ` — Select`.
Should-close handler: `cx.quit()` (PD6).

```
┌──────────────────────────────────────────────────────────────────────────────────────────────────┐
│ File Edit View Session Tools Help          Claude History — proj                     ─  ☐  ✕    │ TitleBar
├────────────────────────────────────┬─────────────────────────────────────────────────────────────┤
│ [Conversations] [Deep search]      │ proj · Fix login bug · opus-4.5 · 42 messages · 1h 5m · ⚠ 2 │ viewer header (TUI
│ ┌────────────────────────────────┐ │ 926k tokens · 2026-10-12 14:03                              │  order, wraps; summary
│ │❯ token refresh        12/340 ✕│ │ Session token refresh flow and retry on 401                 │  same line if it fits)
│ └────────────────────────────────┘ │─────────────────────────────────────────────────────────────│
│ (Lexical|Semantic) (All|Workspace) │ Tools [Summary|Truncated|Full] [Thinking] [Timing]           │ viewer toolbar
│ [▣ proj ▾] [Any time ▾]        [⟳] │                       [Outline] [Resume] [Fork] [⋯]          │
├────────────────────────────────────┤─────────────────────────────────────────────────────────────│
│ CC Fix login bug          3 min ago│ Sep 04 13:59      You │ How do I refresh the token when…    │ transcript
│    proj · 42 msgs · 1h 5m · 0.87   │                Claude │ You can wrap the call in a retry …  │ (gpui list,
│    last answer … first question …  │                Claude │ Read 2 files, ran 1 shell command ▸ │  one item per
├────────────────────────────────────┤              ↳ Result │ ┌ code ─────────────────────────┐   │  ledger block
│ Pi Deploy pipeline        yesterday│                       │ │ fn refresh() { … }            │   │  or ≈100-line
│    api · 7 msgs                    │                       │ └───────────────────────────────┘   │  chunk)
│    preview …                       │                       │ (4 more lines…)                     │
├────────────────────────────────────┤             Thinking  │ Considering the retry budget …      │
│ 12/340 · ⟳ sem embedding 40% 12/30 │─────────────────────────────────────────────────────────────│
│                                    │ [12/42] tools·sum think·off info·off │ / search  e export  F1 │ viewer status bar
└────────────────────────────────────┴─────────────────────────────────────────────────────────────┘
```

* Left pane: `v_flex().size_full().min_h_0()`, `bg(sidebar)`. The TabBar is `underline()` small. Switching tabs
  focuses the new tab's primary element (search input / deep-search query) and switches the pane width (PD1).
* Search input: `Input::new(&search).small().cleanable(true).prefix(prompt).suffix(status)` where prompt = `❯`
  (accent role) or `Project ❯` ("Project" muted) when scope = Workspace; status =
  `Browser::search_status_text()` (L-032; `…` substitution per §12) plus a `Spinner::new().xsmall()` while
  `has_search_work_in_flight()`.
* Toolbar row 1: `ButtonGroup` (exclusive) **Lexical | Semantic**; `ButtonGroup` **All | Workspace** (Workspace
  disabled without a workspace). Row 2: workspace dropdown button (icon `FolderOpen`, label = workspace short name
  or "No workspace"), time filter dropdown (`dropdown_menu_with_anchor(Anchor::TopRight)`; label "Any time" or the
  active filter, e.g. "Since 2d" / "Before 2026-07-20"), spacer, Refresh ghost icon button (`RefreshCw`,
  `tooltip_with_action("Refresh", &Refresh, Some("Workspace"))`, `.loading(true)` while a load runs).
* Picker banner (picker mode only) between toolbar and list: info-colored strip with the PD5 text and a primary
  `Select` button.
* List: `uniform_list` of `ListItem`s (row anatomy §10.1), `Scrollbar::vertical`.
* List footer (`StatusBar`, `text_xs`) shows **state first** (DG §9.12): left = count (`12/340`, `sem 12/340`,
  `Loading… 57`) and, when present, the load/refresh indicator (`Spinner` + `Refreshing conversations…`) or the
  semantic activity (`Spinner` + `sem preparing embeddings`, or `Progress` + `sem embedding 40%  12/30 chunks`);
  right = key hints **only while the list has focus and no activity is shown**, in priority order (open, resume,
  fork, rename, delete, scope, mode) with lower-priority chips dropped until the row fits; `F1 help` is always kept
  (§8.7).
* Viewer: §9; agent panel §5.4.

Empty/error placeholders inside the list area and the viewer use `Empty` with a recovery action (§12).

### 5.2 Conversation row context menu (right click; `shift-f10` / Menu key on the selected row)

Implemented with `gpui_kit::component::native_menu::NativeMenu` (native on Windows/macOS, gpui-kit fallback on
Linux), shown at the clicked point or at the selected row's bounds (from the row's recorded bounds, §10.5). Both
entry points use the same builder. When the menu is built, ListPane stores the row's `ConversationTarget` under a
fresh token; every item dispatches `RowMenu { item, token }` (data action), so the target never comes from the live
selection (A1). Right-click also selects and previews the row (the list keeps focus).
```
┌──────────────────────────────┐
│ Open                   Enter │
│ Resume               Ctrl+R  │
│ Fork                 Ctrl+F  │
│ Rename…                  F2  │
│ Export…                      │
│ Copy to clipboard          ▸ │ Ledger (formatted) / Plain text / Markdown / JSONL (raw)
│ Copy path                    │
│ Copy session ID              │
│ Copy header session ID       │ (only when it differs from the file stem)
│ Reveal in file manager       │
│ ──────────────────────────── │
│ Search within conversation…  │
│ Outline                      │
│ ──────────────────────────── │
│ Delete…              Ctrl+X  │
└──────────────────────────────┘
```
Shortcut labels come from the live keymap. Items that need a known source (Resume, Fork, Rename, Delete) are
disabled for targets with an unknown source; while loading they are disabled (AD6).

### 5.3 Workspace picker and time filter dropdowns
```
Workspace ▾                                   Time ▾
┌───────────────────────────────────────┐     ┌──────────────────────┐
│ Recent                                 │     │ ✓ Any time           │
│ ✓ proj        C:\git\proj              │     │   Last hour     1h   │
│   api         C:\git\api               │     │   Last day      1d   │
│ Projects in conversations              │     │   Last 2 days   2d   │
│   claude-history/claude-search  …      │     │   Last week     1w   │
│ ───────────────────────────────────── │     │   Last month    1mo  │
│ Choose folder…        Ctrl+Shift+W     │     │   Last 6 months 6mo  │
│ Clear workspace                        │     │   Last year     1y   │
└───────────────────────────────────────┘     │ ──────────────────── │
                                              │ Custom range…        │
                                              └──────────────────────┘
```
The "Projects in conversations" entries come from ListPane's cached `project_paths` (computed off-thread after each
load, PD3). "Custom range…" dialog (shared widget `widgets/time_range.rs`, also used by Deep search):
```
┌ Time filter ──────────────────────────────────────────────┐
│ Since   [ 2d                                  ]           │
│         Only conversations this recent, as a duration or  │
│         date (2d, 1d6h, 6mo, 2026-07-20)                  │
│ Before  [                                     ]           │
│         Only conversations older than this duration or date│
│ ⚠ '2x': unknown unit 'x' (…)      ← inline TimeFilterError │
│                                   [Cancel]  [Apply]       │
└───────────────────────────────────────────────────────────┘
```
Inverted range → inline `Invalid time range: lower bound … is later than upper bound …`.

### 5.4 Deep Search tab (left pane, 34rem default) and agent panel (viewer)
```
┌─ [Conversations] [Deep search] ─────────────────────┐
│ ┌──────────────────────────────────────────────────┐ │
│ │🔍 "token refresh" retry               [Search]   │ │  Enter runs; [Cancel] while running
│ └──────────────────────────────────────────────────┘ │
│ Mode (Default|Lexical|Semantic|Exact|Hybrid)  Options ▸│  Default = [agent].mode → [search].mode → lexical
│ ┌ Options ───────────────────────────────────────────┐│  (disclosure, collapsed by default, state persisted)
│ │ Scope (Default·All|All|Workspace)  Time [Any ▾]    ││  Default = [agent].scope ("Default (Workspace)")
│ │ Top [10]   Layout (Grouped|Flat)                   ││  "message hits" label when Flat
│ │ Hits per conversation [2]  ☐ Show duplicate hits   ││
│ │ Budget [6000] chars  ☐ No budget                   ││
│ └────────────────────────────────────────────────────┘│
├──────────────────────────────────────────────────────┤
│ ⓘ Exact match forced (quoted-only query)             │  hints / banners
│ ⚠ 3 warnings: malformed-transcript 2, io 1        ▸  │
│ ⓘ 120 passages are not cached. Generate the semantic │
│   cache to include them.   [Generate semantic cache…]│
├──────────────────────────────────────────────────────┤
│ 1 Fix login bug                         0.016393      │  compact group card; full ref in tooltip
│   proj · 3f2a19c0 · sem 0.812 lex 0.100 · 2/5 hits ⧉ │  ⧉ = copy ref (full ch_ ref)
│   ▸ lexical  m12  …token refresh on 401…             │  hit rows (click/Enter = open at m12)
│   ▸ tool     m14  …grep "refresh_token" …            │
│ 2 Deploy pipeline …                                  │
├──────────────────────────────────────────────────────┤
│ 10 groups · 18 hits · shortlisted 50 · loaded 10     │
│ [Show agent text output] [Copy]                      │
└──────────────────────────────────────────────────────┘
```
* With "Show agent text output" on, the results area shows `AgentResponse::text()` (and the notices of `stderr`)
  in a read-only `Textarea` (rope-based, virtualized, selectable, searchable) — never a single `StyledText`.
* Hit row context menu: Open at message, Search within conversation…, Outline, Read range…, Copy read command,
  Copy as agent records, Copy ref, Copy anchors. "Copy as agent records" copies the lines of the agent text that
  belong to the hit; "Copy read command" is D-012; "Copy ref" copies the full `ch_` ref (D-030).
* Results stay when switching tabs (DeepSearchPane keeps its state); a running search shows `Searching…` with the
  progress line and a **Cancel** button (D-033).

Viewer agent panel (right of the transcript, `o` / toolbar "Outline", own key context `AgentPanel`):
```
┌ Outline | Within | Read ────────────────── ✕ ┐
│ For: Fix login bug (ch_9c1e42a7b3d1 ⧉)        │ ← the conversation this result set belongs to
│ [✓tools] [✓tool results] [thinking] [subagents]│ visibility (AgentOutputFlags); config-true flags
│ Budget [6000] ☐ No budget                     │   shown on + disabled ("Enabled by [agent] tools")
│ 42 visible messages                           │
│ m1  user      312  ma_1f…  How do I …         │ ← ≤20: one row per message
│ m2  assistant 1.2k ma_9b…  You can wrap…      │   >20: "seg m1..m10 · 8.1k chars" groups (expandable)
│ …                                             │
│ [Show agent text output] [Copy]               │
└───────────────────────────────────────────────┘
Stale banner (Within/Read when the viewer moved on): "Results are for Fix login bug. [Run for this conversation]"
Within tab: query + Mode + Top(20) + Budget/No budget + [Search]/[Cancel]; target = the viewer's conversation at the
  moment of the run (no conversation field); target line "conversation project=pr_… uuid=… ref=ch_…"; hits as in Deep
  search.
Read tab: Refs [ch_…:m10..m14  ch_…] (one or more, separated by spaces or commas; default = the viewer's ref)
  Anchor [ma_…] Focus [m12] (Lines S..E | Match [text] Context [3]) Budget/No budget, visibility toggles, [Read];
  cards "m12 · assistant · line 88 · ma_…" + body (numbered lines for Lines; `>` markers and `…` gaps for Match),
  grouped under a "Fix login bug · ch_…" header per conversation; "Load previous / Load next" extend the range by 5
  messages (single-ref reads only;
  multi-ref reads show the `continue read` line with "Copy read command").
Out-of-root file (single-file window on an export or attachment): panel body "Outline, Within and Read work only for
  conversations inside the Claude, Pi or OMP history folders." (D-035).
```

### 5.5 Single-file viewer window
```
┌──────────────────────────────────────────────────────────────────────────┐
│ File Edit View Help     3f2a19c0-….jsonl — Claude History         ─ ☐ ✕ │
├──────────────────────────────────────────────────────────────────────────┤
│ proj · Fix login bug · 42 messages · 2026-10-12 14:03                    │
│ Tools [Summary|Truncated|Full] [Thinking] [Timing]   [Outline] [⋯]       │
├──────────────────────────────────────────────────────────────────────────┤
│ (transcript as in the main window)                                       │
├──────────────────────────────────────────────────────────────────────────┤
│ [12/42] tools·sum think·off info·off │ / search  e export  q close  F1   │
└──────────────────────────────────────────────────────────────────────────┘
```
The menu tree is the shared one (§8.10): Session items without a handler here (Resume, Fork, Rename, Delete,
Select and exit) are disabled; File → "Show conversations" opens or focuses the main window.

### 5.6 Rename dialog (`open_dialog`, width 32rem; target captured)
```
┌ Rename conversation ─────────────────────────────────────┐
│ proj · Fix login bug · 3f2a19c0-1c2d-…                   │  muted target line
│ ┌──────────────────────────────────────────────────────┐ │
│ │ Fix login bug▏                                        │ │  prefilled custom title, caret at end
│ └──────────────────────────────────────────────────────┘ │
│ Enter save · Esc cancel · Ctrl+U clear                   │
│ ⚠ Failed to rename: <err>                                │  inline error, dialog stays open
│                                      [Cancel]  [Save]    │  Save = primary (Enter saves; not destructive)
└──────────────────────────────────────────────────────────┘
```
Empty title is allowed (clears the title). While the write runs (K3, background) the buttons show a spinner.
Success → close; the row and header update in place (no toast, PD16).

### 5.7 Delete confirmation (`open_alert_dialog`, key context `Confirm`)
```
┌ Delete "Fix login bug"? ─────────────────────────────────────┐
│ ⚠ Claude Code conversation · proj · 3f2a19c0-1c2d-4e5f-…     │
│ These will be removed from disk:                              │
│   C:\Users\me\.claude\projects\C--git-proj\3f2a….jsonl        │
│   C:\Users\me\.claude\projects\C--git-proj\3f2a…\  (folder)   │
│   C:\Users\me\.claude\projects\C--git-other\3f2a….jsonl       │
│ 2 copies across projects. This can't be undone.               │
│ Y delete · N or Esc cancel                                    │
│                                    [Cancel]  [Delete]         │  Delete = danger
└──────────────────────────────────────────────────────────────┘
```
AlertDialog (not overlay-closable), `.description(..)` + children for the path list (scrollable when long),
`.ok_text("Delete").ok_variant(Danger)`. A focused `div().key_context("Confirm")` inside the alert binds `y`,
`shift-y` → confirm, `n`, `shift-n` → cancel, and **`enter` → no-op** (AD18). Pi: one file; OMP: the file +
`<stem>/` artifacts folder. While the delete runs the buttons are disabled. The same pattern (title naming the
target, danger verb, Enter ignored, y/n keys) is used by every destructive confirmation: §5.8 with overwrites,
§5.10, §5.11 Clear, §5.15, the export "Replace" prompt.

### 5.8 Copy conversation to workspace (cross-project fork / resume copy)
```
┌ Copy "Fix login bug" to C:\git\newproj? ─────────────────────────────┐
│ Claude Code only finds sessions in the project folder of the folder it │
│ runs in, so the conversation is copied:                                │
│   from C:\Users\me\.claude\projects\C--git-proj\3f2a….jsonl            │
│     to C:\Users\me\.claude\projects\C--git-newproj\3f2a….jsonl         │
│   from …\C--git-proj\3f2a…\   (tool results, subagents)                │
│     to …\C--git-newproj\3f2a…\                                         │
│ ⚠ Already exists and will be overwritten:                              │
│   …\C--git-newproj\3f2a….jsonl                                         │
│ Then runs: claude --resume 3f2a19c0-… --model opus   in C:\git\newproj │
│                                           [Cancel]  [Copy and fork]    │
└──────────────────────────────────────────────────────────────────────┘
```
Content from `CopyPlan { files, dirs, existing_targets }` and `ResumePlan::display_command()`. Button text
`Copy and fork` / `Copy and resume`. Without overwrites: primary button, Enter confirms. With overwrites: danger
button, Enter ignored, `y`/`n` keys (§5.7 pattern).

### 5.9 Export / Copy menu dialog (key context `ExportMenu`)
```
┌ Export to file ──────────────┐      ┌ Copy to clipboard ───────────┐
│ ▶ [1] Ledger (formatted)     │      │ ▶ [1] Ledger (formatted)     │
│   [2] Plain text             │      │   [2] Plain text             │
│   [3] Markdown               │      │   [3] Markdown               │
│   [4] JSONL (raw)            │      │   [4] JSONL (raw)            │
│   [Esc] Cancel               │      │   [Esc] Cancel               │
└──────────────────────────────┘      └──────────────────────────────┘
```
Labels from `ExportFormat::label()`. Rows are clickable; 1–4 pick immediately; j/k/↑/↓ move; Enter picks; Esc
cancels. Export then opens the save dialog (PD8).

### 5.10 Delete empty transcripts (Tools)
```
┌ Delete empty transcripts ───────────────────────────────────────────────────────────┐
│ Scope (All workspaces | Current workspace)   ☐ Include files modified in the last 10 min │
│ Found 3 empty transcript(s).                                                          │
│ ┌──────────────┬──────────┬───────┬───────┬──────────────────────────┬─────────────┐ │
│ │ Session      │ Project  │ Users │ Lines │ Preview                  │ Modified    │ │
│ │ 3f2a19c0-…   │ proj     │ 1     │ 4     │ /status                  │ 2 days ago  │ │
│ │ 8a95d451-…   │ personal │ 0     │ 1     │ (no user messages)       │ 5 days ago  │ │
│ │ 11112222-… ⏱ │ api      │ 1     │ 2     │ hello                    │ just now    │ │ ⏱ = recent, excluded
│ └──────────────┴──────────┴───────┴───────┴──────────────────────────┴─────────────┘ │
│ Path of the selected row: C:\Users\me\.claude\projects\C--git-proj\3f2a….jsonl         │
│ Claude Code transcripts only; Pi and OMP conversations are never touched.              │
│                                              [Close]  [Rescan]  [Delete 2…]           │ danger
└──────────────────────────────────────────────────────────────────────────────────────┘
```
Rows from `maintenance::find_empty_candidates` (preview via `truncate_delete_empty_preview`, recency via
`EmptyCandidate::modified_within(now, 10 min)`). "Delete N…" opens AlertDialog `Delete N empty transcripts?`
listing the files (§5.7 pattern) → `delete_empty_candidates(&confirmed)` → result line
`delete_empty_summary_line(true, n)` plus skipped/failed lines. "Copy as text" copies
`format_delete_empty_report` (CLI stdout parity). Scope "Current workspace" is disabled without a workspace.

### 5.11 Generate semantic cache / Clear semantic cache (Tools)
```
┌ Generate semantic cache ───────────────────────────────────────────┐
│ Embeds every conversation so semantic search and Deep search are    │
│ fast. The model (~128 MB) is downloaded on first use.               │
│ Scope (All workspaces | Current workspace)                          │
│ ▓▓▓▓▓▓▓▓░░░░░░░░  Semantic cache: embedded 1200/3000 changed chunk(s)│
│ Semantic search is paused while the cache is generated.             │
│ Cache: C:\Users\me\.cache\claude-history-gui\semantic               │
│                                         [Cancel]  [Generate]         │
└────────────────────────────────────────────────────────────────────┘
┌ Clear semantic cache? ─────────────────────────────────────────────┐
│ Deletes C:\Users\me\.cache\claude-history-gui\semantic, including   │
│ the downloaded embedding model. This can't be undone.               │
│ Y clear · N or Esc cancel                                           │
│                                           [Cancel]  [Clear]         │ danger
└────────────────────────────────────────────────────────────────────┘
```
Results: `GenerateReport::text` lines (`Semantic cache: cached …`), `ClearReport::message` (`Semantic cache cleared.`
/ `Semantic cache is already empty.`), errors with Retry. Both run under the semantic gate (§7.6). Clear **empties**
the directory (K6 as built, §4.3): the directory itself and its `embeddings.lock` stay, links are never followed; a
second Clear reports `Semantic cache is already empty.` The dialog text may keep "Deletes <dir>, including the
downloaded embedding model" (it deletes everything the user cares about) or say "Deletes everything in <dir>, …".

### 5.12 Debug search (Tools)
```
┌ Debug search ──────────────────────────────────────────────────────────────────┐
│ Query [ refresh token                    ]  Scope (All | Workspace)  [Run]      │
│ Corpus: fresh load · since 2d (launch filter) · 812 conversations              │
│ intent: "refresh token"     literals: none                                      │
│ ┌────┬───────┬───────────┬──────────┬──────────────┬──────┐                     │
│ │ #  │ score │ freshness │ project  │ session      │ age  │   (top 30)          │
│ │ 01 │ 12.41 │ 1.83      │ proj     │ 3f2a19c0-…   │ 3h   │ ▸ field breakdown   │
│ └────┴───────┴───────────┴──────────┴──────────────┴──────┘                     │
│    title: tf=2 adj=1 (w=5)   "refresh" tf=2 ln=1.10                              │
│ [Copy as text]                                              [Close]             │
└────────────────────────────────────────────────────────────────────────────────┘
```
Corpus = `maintenance::load_sorted_conversations(env, show_last, debug, &launch_time_filter)` in the background (a
fresh load with the launch time filter, CLI parity; never the list corpus, which may hold UUID-jump rows), cached
for the dialog's lifetime ("Reload" re-runs). Scope Workspace = `debug_search_report(…, Some(workspace))`.
"Copy as text" = `DebugSearchReport::to_text()`.

### 5.13 Semantic debug search (Tools)
```
┌ Semantic debug search ─────────────────────────────────────────────┐
│ Query [                ]  (Search report | Debug dump)  Top [20]    │
│ Scope (All | Workspace)                                  [Run]      │
│ ┌────────────────────────────────────────────────────────────────┐ │
│ │ Semantic search: searching 812 cached chunk(s) from 120 recent │ │ read-only Textarea
│ │ conversation(s) with fastembed BGESmallENV15                   │ │
│ │ #01 0.812 …                                                    │ │
│ └────────────────────────────────────────────────────────────────┘ │
│ [Copy]                                                   [Close]   │
└────────────────────────────────────────────────────────────────────┘
```
Same corpus rule as §5.12. Runs under the semantic gate (shared embedder).

### 5.14 Debug log (Tools, `secondary-shift-l`)
```
┌ Debug log ────────────────────────────────────────────────────────────────┐
│ [Messages] [Parse errors] [debug.log]       Level (Debug|Info|Warn|Error)  │
│ 14:03:12 [INFO] Loading global history from 41 projects                    │ read-only Textarea
│ 14:03:12 [DEBUG] Cache: 120 hits, 3 misses                                 │
│ 14:03:13 [WARN] Failed to load Pi/OMP history: …                           │
│ [Clear] [Copy]                                                   [Close]   │
└────────────────────────────────────────────────────────────────────────────┘
Parse errors tab: the viewer's conversation (captured at open): `maintenance::parse_error_report(conv)`
rendered as "File: …  Errors: N", per error "Line N: <msg>" with context lines; [Write to debug.log]
(`maintenance::log_parse_errors`). debug.log tab: last 2,000 lines of the GUI debug.log; [Open file] [Reveal].
```

### 5.15 Delete conversation by session ID (Tools)
```
┌ Delete conversation by session ID ────────────────────────────────┐
│ Session ID [ 3f2a19c0-1c2d-4e5f-8a9b-0c1d2e3f4a5b ]   [Find]        │
│ Claude Code conversations only. Found 2 copies:                     │
│   …\C--git-proj\3f2a….jsonl                                         │
│   …\C--git-proj\3f2a…\  (folder)                                    │
│   …\C--git-other\3f2a….jsonl                                        │
│ Not removed: file-history, todos, session-env and other global data.│
│                                         [Close]  [Delete…]          │ danger, enabled after Find
└────────────────────────────────────────────────────────────────────┘
```
Find = `maintenance::find_session_copies` (errors: `Session not found: <id>`). Delete… = AlertDialog
`Delete session 3f2a19c0-…?` (§5.7 pattern) → `maintenance::plan_session_delete` + `execute_session_delete` (K4) →
`delete_by_uuid_message(id, n)` as the result line; deleted paths are broadcast (§11.15).

### 5.16 Settings (Tools → Settings…, `secondary-,`; dialog 56rem × 40rem)
```
┌ Settings ──────────────────────────────────────────────────────────────────┐
│ General │ Theme            (System | Light | Dark)            from gui.toml  │
│ Display │ Terminal         [ auto                     ▾ ]     default        │
│ Search  │ claude program   [                     ] [Browse…] resolved: C:\Users\me\.local\bin\claude.exe │
│ Keys    │ pi program       [                     ] [Browse…] not found on PATH │
│ Resume  │ omp program      [                     ] [Browse…]                   │
│ Agent   │ Default workspace[                     ] [Browse…]                   │
│         │ ☑ Restore last session (window, workspace, view options)            │
│         │ Display page: Default tool display (Summary|Full)                   │
│         │      from config.toml: Full · applies at startup, before the       │
│         │      remembered value                                     [Reset]   │
│         │ Keys page:  Resume [ctrl+r ] ⓘ from config.toml   [Reset]           │
│         │             ⚠ "r": a plain character can't be typed in the search field │
│         │ Files: C:\Users\me\.config\claude-history\gui.toml (written)        │
│         │        C:\Users\me\.config\claude-history\config.toml (read-only)   │
│         │ ⚠ gui.toml has errors: <parse error> [Open gui.toml] [Reveal]       │ (only then; Save disabled)
│         │                                       [Cancel]  [Save]             │
└─────────┴────────────────────────────────────────────────────────────────────┘
```
Pages and fields in §13.2.

### 5.17 Keyboard shortcuts (F1; `?` when not typing)
```
┌ Keyboard shortcuts ──────────────────────────────────────┐
│ Conversation list                                         │
│ ↑ / ↓          │ Move selection                           │
│ Ctrl+P / N     │ Move selection                           │
│ …                                                         │
│ Viewer · Agent panel · Deep search · Global · Dialogs     │
│                                               [Close]     │
└──────────────────────────────────────────────────────────┘
```
Rows and labels: §8.7. Opens scrolled to the section of the region that had focus.

### 5.18 About / Check for updates (Help)
```
┌ About Claude History GUI ──────────────────────────────┐
│ Claude History GUI 0.1.0 (core 0.1.76)                  │
│ A desktop browser for Claude Code, Pi and OMP history.  │
│ License: Unlicense. Core derived from claude-history    │
│ (MIT, © 2024 Raine).                                    │
│ Data: C:\Users\me\.claude                               │
│ Cache: C:\Users\me\.cache\claude-history-gui            │
│ Config: …\.config\claude-history\gui.toml (+config.toml)│
│ State: …\.local\state\claude-history-gui\state.json     │
│ [Check for updates…] [Open project page]      [Close]   │
└────────────────────────────────────────────────────────┘
┌ Check for updates ─────────────────────────────────────┐
│ ⟳ Checking for updates…                                │
│ → Available: "Claude History GUI 0.2.0 is available (you have 0.1.0)." + notes + [Open release page] │
│ → UpToDate: "Claude History GUI is up to date (0.1.0)."│
│ → Newer: "This build (0.2.0-dev) is newer than the latest release (0.1.0)."│
│ → NoRelease: "No release has been published yet."      │
│ → no asset for this platform: "… but there is no build for this platform." │
│ → error: "Update error: <e>"                  [Retry]  │
└────────────────────────────────────────────────────────┘
```

### 5.19 Semantic result details (Ctrl+S; snapshot taken at open)
```
┌ Semantic result ──────────────────────────────────────────┐
│ hybrid 1.23  semantic 0.90  lexical 0.10                   │
│ rationale lex boost  quality strong                        │
│ chunk 3f2a19c0-1c2d-4e5f-8a9b-0c1d2e3f4a5b #4              │
│ terms refresh, token            (or "(none)")              │
│ preview …sanitized evidence, wraps…                        │
│ Esc close                                                  │
└────────────────────────────────────────────────────────────┘
```
Any key closes (TUI parity); clicking outside closes. Lines are exactly the TUI lines (L-070).

### 5.20 Other small dialogs and toasts
* **Open link?** `Open link?` / URL in monospace / [Cancel] [Copy link] [Open] (AD13).
* **Configuration error** at startup (ADD E2), one per failing file (config.toml and gui.toml are loaded
  independently): title `Couldn't read <file name>`; body = core error text (`Configuration error: Failed to
  parse config file at '<path>': <toml error>`); buttons [Quit] [Continue with defaults] [Retry] [Open file].
  Continuing keeps the other file's settings and shows a persistent warning notification `<file name> has errors;
  using defaults for its settings` until fixed (Retry from Settings re-reads). While **gui.toml** is unparseable every
  gui.toml write is blocked (K12 refuses; Settings Save disabled; theme changes are session-only).
* **Show projects directory**: with a workspace → toast `Projects directory: <path>` with two buttons Copy and Reveal
  (custom `.content(...)`; Reveal disabled + ` (does not exist yet)` when missing; autohide on). Without a workspace
  the menu/palette item is disabled with tooltip `Choose a workspace first`; `--show-dir` without a workspace →
  stderr `Error: Choose a workspace folder to show its Claude projects directory`, exit 1 (or, with no stdout, a
  toast with "Choose workspace…").
* **Choose folder** uses `PlatformServices::prompt_open_paths { files: false, directories: true, multiple: false,
  prompt: "Choose workspace" }`. **Open JSONL file** uses `{ files: true, directories: false, multiple: true, prompt:
  "Open" }` (one window per file; disabled in picker mode).
* **Replace file?** (export, PD8) and **Print and quit?** (Ctrl+O with a terminal, PD5).

### 5.21 Command palette (`secondary-shift-p`, `f10`)
```
┌ ⌕ Type a command…                                        ┐
│ Session                                                    │
│   Resume                                         Ctrl+R    │
│   Fork                                           Ctrl+F    │
│ View                                                       │
│ ✓ Show thinking                                  Shift+T   │
│ Tools                                                      │
│   Delete empty transcripts…                                │
│   Generate semantic cache…                                 │
│ …                                                          │
└────────────────────────────────────────────────────────────┘
```
`gpui_kit::component::command::{Command, CommandState, CommandItem, CommandGroup}` in a dialog; groups = top-level
menus; items = every menu item with its action (`CommandItem::action`), live shortcut label, check state, keywords
(menu path), and `disabled` when `window.is_action_available` is false for the target window. Confirming dispatches
the action to the element that had focus before the palette opened (stored when it opens), so pane handlers resolve
their own targets (A1).

---

## 6. File structure and owners (one owner per file at any time)

Every file below is first created by WP0 (complete for "frozen" files, a compiling stub with the final public
signatures otherwise) and afterwards owned by the WP shown after the arrow. "Frozen" = written completely by WP0 and
afterwards changed only by the integrator (§18) on a recorded request. A WP that needs a change to a file it does
not own writes it as a "requested change" in its hand-off note.

```
Cargo.toml (root), Cargo.lock, .cargo/config.toml, rust-toolchain.toml   WP0 → integrator
crates/core/**                                core workflow → WP-C → WP-C2 (CCR batches, §4.4)
crates/gui/
  Cargo.toml                      WP0 → integrator   (complete dependency list in WP0, §16.1)
  build.rs                        WP0 → WP7          (WP0: DirectML delay-load; WP7: icon resource; later: /STACK, §14.4)
  assets/                         WP7                icons (app.ico, app.icns, app.png 512), windows/app.rc
  src/
    main.rs                       WP0 → WP7   windows_subsystem attr; parse args; headless print paths; calls lib::run
    lib.rs                        WP0 frozen  `pub mod` declarations; `pub fn run(launch: LaunchContext, env: HistoryEnv)`
    app.rs                        WP0 frozen  init(cx, …): calls owner functions only, in order (§7.3)
    actions.rs                    WP0 frozen  every action (§8.2)
    model.rs                      WP0 frozen  every shared value type (§7.9)
    globals.rs                    WP0 frozen  AppState, DebugBuffer, MainPanes, ActivePane, OpenTargets, SemanticGate, LastToast (§7.3)
    notify.rs                     WP0 frozen  toast helpers (kinds, replace keys, actions with autohide, two-button content)
    exit.rs                       WP0 frozen  quit_without_output(cx), quit_with_output(text, cx), exit codes
    services.rs                   WP0 → WP7   PlatformServices trait (signature frozen) + RecordingServices (WP0) + RealServices (WP7)
    embedder.rs                   WP0 → WP7   SharedEmbedder (one model per process) + adapters to the three core factory types (§7.6)
    debug_sink.rs                 WP0 → WP7   core debug sink → DebugBuffer + stderr level filter
    qa_ext.rs                     WP0 → WP7   QaExt::qa_id (no-op without feature "qa")
    keymap.rs                     WP0 → WP6   static table, input-editing table, user bindings, help rows, lookups (§8)
    menus.rs                      WP0 → WP6   the single menu tree, install/rebuild (§8.10)
    palette.rs                    WP0 → WP6   command palette from the menu tree (§8.9)
    theme_tokens.rs               WP0 → WP6   app color roles, theme switching, startup theme
    persist.rs                    WP0 → WP6   UiState (state.json) load/save/debounce
    widgets/
      mod.rs                      WP0 frozen
      confirm.rs                  WP0 frozen  open_destructive_confirm(..) (§5.7 pattern: AlertDialog, Confirm context, y/n, Enter ignored)
      path_list.rs                WP0 frozen  PathList (files + folders with labels; used by WP3 delete/copy dialogs and WP5 tools)
      text_view.rs                WP0 frozen  read-only Textarea helper (agent text, reports, debug log; used by WP4 and WP5)
      time_range.rs               WP0 → WP1   time filter dropdown + custom range dialog (also used by WP4)
    workspace/
      mod.rs                      WP0 → WP-I  Workspace root: layout, pane switching, event wiring, focus, region cycling
      routing.rs                  WP0 frozen  shared forwarding helper used by Workspace and SingleFileWindow (§8.1)
      title_bar.rs                WP0 → WP6   title bar content (AppMenuBar, title label)
      startup.rs                  WP0 → WP7   LaunchContext mapping, workspace resolution (PD3), initial view resolution (PD17),
                                              config loading/errors, startup tools (AD14), run_startup_tool
    list/                         WP0 stub → WP1   mod.rs, rows.rs, toolbar.rs, footer.rs, context_menu.rs, workspace_picker.rs,
                                              semantic_details.rs, picker_banner.rs, tests.rs
    viewer/                       WP0 stub → WP2   mod.rs, worker.rs, items.rs, segments.rs, code.rs, seltext.rs, render.rs,
                                              header.rs, toolbar.rs, status.rs, search.rs, nav.rs, lru.rs, tests.rs
    single_file.rs                WP0 stub → WP2   SingleFileWindow root + open_window(path)
    ops/
      mod.rs                      WP0 frozen  `pub mod` declarations only
      resume.rs rename.rs delete.rs export.rs copy.rs select.rs tests.rs   WP0 stub → WP3
      console.rs                  WP0 stub → WP7   stdout/stderr kind, attach, write
    deep_search/                  WP0 stub → WP4   mod.rs, worker.rs, form.rs, results.rs, panel.rs, outline.rs,
                                              within.rs, read.rs, agent_text.rs, tests.rs
    tools/                        WP0 stub → WP5   mod.rs, delete_empty.rs, semantic_cache.rs, debug_search.rs,
                                              semantic_report.rs, debug_log.rs, delete_by_id.rs, projects_dir.rs,
                                              open_file.rs, updates.rs, about.rs, tests.rs
    settings/                     WP0 stub → WP6   mod.rs, draft.rs, pages.rs, keys_page.rs, tests.rs
    help/                         WP0 stub → WP6   mod.rs, shortcuts.rs
    qa/                           WP0 stub → WP7   (cfg feature "qa") mod.rs, dsl.rs, runner.rs, probe.rs, gate.rs
    test_support/                 (cfg(test))
      mod.rs                      WP0 frozen  harness (§15.2): TestWorld, open_main, open_single_file, wait_loaded,
                                              services handle, answer queues, fault guards, global test lock, synth generators
      self_tests.rs               WP0 frozen  (as built) harness self-tests (standard corpus, synth, fault guards)
      fixtures_list.rs            WP1         list fixtures/builders
      fixtures_viewer.rs          WP2         viewer fixtures (synthetic 3k/10k transcripts with 5k-line outputs)
      fixtures_ops.rs             WP3         two-copy UUID, cross-project fork, Pi/OMP resume, fake programs on PATH
      fixtures_agent.rs           WP4         25-message outline fixture, multi-ref read fixtures
      fixtures_tools.rs           WP5         empty transcripts, semantic cache fixtures
    integration_tests/            WP-I        (cfg(test)) cross-pane G tests (§17.10)
    skeleton_tests.rs             WP0 frozen  (cfg(test), as built) skeleton tests + WP0 FEATURES rows; skeleton_tests/contracts.rs
                                              (frozen cross-WP contract tests), skeleton_tests/safety_lint.rs (§16.2 lint as a test)
  tests/fixtures/                 committed fixture files: list/ WP1, viewer/ WP2, ops/ WP3, agent/ WP4, tools/ WP5,
                                  qa-home/ WP7 (the synthetic QA sandbox corpus used by CI's qa.yml)
  qa/scenarios/*.qa               owned by the WP whose DoD names the scenario (§15.3); smoke.qa WP7
.github/workflows/{ci.yml, qa.yml, release.yml}   WP7
packaging/macos/Info.plist.in, packaging/linux/claude-history-gui.desktop, packaging/windows/README.txt   WP7
tools/qa/{new-sandbox.ps1, run-gui.ps1, qa-shot-watch.ps1, shot.ps1}, tools/ci/{safety-lint.sh, check-delayload.ps1}   WP7
```

GUI fixtures reuse `crates/core/tests/fixtures` read-only (copied into temp dirs by `TestWorld`); new GUI-only fixtures
go into `crates/gui/tests/fixtures/<owner>/` or are generated by `test_support::synth`.

---

## 7. Entities, events, globals, services, background work, focus, targets

### 7.1 Entity graph

```
App
 ├─ Global AppState (globals.rs)       env Arc<HistoryEnv>, config + per-file errors, launch, settings, workspace,
 │                                     view defaults, UiState handle, keys, revision
 ├─ Global DebugBufferHandle           Arc<DebugBuffer> (ring of 5,000 debug-sink lines)
 ├─ Global Services                    Arc<dyn PlatformServices> (+ typed Arc<RecordingServices> in tests/QA)
 ├─ Global SharedEmbedderHandle        one embedding model per process (§7.6)
 ├─ Global SemanticGate                Open | Exclusive(op) (§7.6)
 ├─ Global MainPanes / ActivePane / OpenTargets / LastToast / UserBindingSet / PaletteReturnFocus
 │
 ├─ Main window: Root ─► Entity<Workspace>
 │     ├─ Entity<AppMenuBar>                (Win/Linux)
 │     ├─ Entity<ListPane>                  owns core Browser + HistoryLoader (exclusively), search InputState,
 │     │                                    poll task (waker), ticker task, UniformListScrollHandle, row-menu targets
 │     ├─ Entity<DeepSearchPane>            owns form InputStates, result set (with its query), AgentWorker client
 │     ├─ Entity<ViewerPane>                owns core ViewerModel (current), ConversationTarget, viewer worker client,
 │     │     │                              gpui ListState, items Arc, caches, find InputState, MessageNav, ViewSearch
 │     │     └─ Entity<AgentPanel>          owns outline/within/read result sets (each with its target), AgentWorker client
 │     └─ transient dialog entities         RenameDialog, ConfirmDialogs, CopyToWorkspaceDialog, ExportMenu, SettingsDraft,
 │                                          tool dialogs … (each captures its target and registers it in OpenTargets)
 └─ Single-file window: Root ─► Entity<SingleFileWindow> ─► Entity<ViewerPane> (mode SingleFile) ─► Entity<AgentPanel>
```

Ownership rules:
* `Browser` and `HistoryLoader` are mutated only inside `ListPane` methods (UI thread), and only with bounded work
  (K1–K4 move every O(corpus) or I/O step to core threads or GUI background tasks). Other entities read through
  `ListPane` methods (`conversation(path)`, `header_info(path)`, `project_paths()`).
* `ViewerModel` is owned by `ViewerPane`; documents/items/segments are built by the pane's `viewer-worker` from an
  `Arc<Vec<RenderableEntry>>` (the model's entries are an `Arc`).
* No entity holds `&mut Window`/`&mut Context`; cross-entity references are `Entity<_>` (owner → child) or
  `WeakEntity<_>` (child → owner, dialogs → panes).

### 7.2 Events

```rust
// list/mod.rs (WP1)
pub enum ListPaneEvent {
    /// Selection changed (any cause). `user` = keyboard/mouse navigation by the user.
    SelectionChanged { target: Option<ConversationTarget>, user: bool },
    /// Enter / double-click / row-menu Open; with `agent_tab` also the agent panel on that tab (row menu Search
    /// within conversation… / Outline, Session → Search within / Outline / Read range from the list, D-029),
    /// which then takes the focus.
    OpenRequested { target: ConversationTarget, focus_viewer: bool, agent_tab: Option<AgentTab> },
    /// Corpus changed: load finished, refresh, rename, delete, UUID load.
    CorpusChanged,
    /// Loading/refreshing state changed (footer, menus, palette enabled state).
    LoadStateChanged,
}
// viewer/mod.rs (WP2)
pub enum ViewerPaneEvent {
    /// Esc after unwinding / `q`: main window → focus list; single-file → close window.
    LeaveRequested,
    /// The viewer opened/cleared a conversation.
    TargetChanged(Option<ConversationTarget>),
}
// deep_search/mod.rs (WP4)
pub enum DeepSearchEvent {
    /// Open a hit: the target comes from the hit's AgentConversationInfo (source, session id, path). With
    /// `agent_tab` the viewer's agent panel also opens on that tab (Search within / Outline / Read range) and
    /// takes the focus.
    OpenHit { target: ConversationTarget, reveal: RevealRequest, focus_viewer: bool, agent_tab: Option<AgentTab> },
}
// deep_search/panel.rs (WP4)
pub enum AgentPanelEvent {
    /// Reveal a message of the conversation the result set was computed for (may differ from the viewer's).
    Reveal { target: ConversationTarget, reveal: RevealRequest },
}
// workspace bus (globals.rs, WP0): emitted by ops/tools after deletions
pub enum CorpusNotice { Deleted(Vec<PathBuf>) }
```

Workspace (`workspace/mod.rs`) subscribes with `subscribe_in`:
* `SelectionChanged` → if the list contains focus: debounce 150 ms (`_preview_task`), then
  `viewer.open(OpenRequest::new(target, false))` unless the target equals the viewer's path.
  `None` → leave the viewer as is. `flush_pending_preview()` runs the pending preview immediately (§7.8); the main
  window calls it in the **capture phase** of every list-target action (session actions included), so the pane's
  handler already sees the flushed preview (L-085).
* `OpenRequested` → `viewer.open(OpenRequest::new(target, focus_viewer).with_agent_tab(agent_tab))`; with a tab the
  viewer opens its agent panel on that tab and focuses it.
* `CorpusChanged` → `viewer.set_header(list.header_info(viewer_path))`.
* `CorpusNotice::Deleted(paths)` (from any deleting route) → `list.remove_paths(&paths)`; if the viewer shows one of
  them → `viewer.clear()` (focus list); agent result sets for those paths are marked deleted (clicks disabled).
  Every dialog registered in `OpenTargets` for one of the paths is closed with the toast `Closed: "<title>" was
  deleted` in its own window by the **app-level** `CorpusBus` handler (`CorpusBus::install`), with or without a main
  window; each window root only clears its own panes (single-file windows clear a deleted file).
* `ViewerPaneEvent::LeaveRequested` → show list (if collapsed) + `list.focus_search()`.
* `ViewerPaneEvent::TargetChanged` → `agent_panel.set_viewer_target(..)` (re-runs Outline if visible; marks
  Within/Read stale, §11.14).
* `DeepSearchEvent::OpenHit` / `AgentPanelEvent::Reveal` → if `target.path` equals the viewer's path → `viewer.reveal`;
  else select the row in the list if present (`list.select_path`) and `viewer.open(OpenRequest::new(target, focus)
  .with_reveal(reveal).with_agent_tab(agent_tab))`.

### 7.3 Globals (globals.rs, frozen) and init order (app.rs, frozen)

```rust
pub struct AppState {
    pub env: Arc<HistoryEnv>,                   // workspace_dir and env_path_base kept in sync
    pub config: LayeredConfig,                  // effective = gui.toml over config.toml (failed file → defaults)
    pub config_errors: ConfigErrors,            // { cli: Option<String>, gui: Option<String> } (per file)
    pub launch: LaunchContext,                  // model.rs
    pub settings: EffectiveSettings,            // core settings::effective_settings(config.effective, flags, mode)
    pub workspace: Option<WorkspaceScope>,
    pub view_defaults: ViewPrefs,               // for newly opened conversations (PD17 precedence result, then t/T/i)
    pub list_mode: ListSearchMode,              // initial list mode (PD17)
    pub ui: UiState,                            // persist.rs
    pub revision: u64,                          // bumped on every change; observers re-read
}
impl Global for AppState {}
pub fn app_state(cx: &App) -> &AppState;
pub fn update_app_state(cx: &mut App, f: impl FnOnce(&mut AppState, &mut App));
pub struct DebugBuffer { /* Mutex<VecDeque<DebugLine>>, min_level: AtomicU8 */ }  // Send + Sync
pub struct DebugLine { pub at: DateTime<Local>, pub level: DebugLevel, pub text: String }
pub struct DebugBufferHandle(pub Arc<DebugBuffer>); impl Global for DebugBufferHandle {}
pub struct MainPanes { pub list: Option<WeakEntity<ListPane>>, pub viewer: Option<WeakEntity<ViewerPane>>,
                       pub deep: Option<WeakEntity<DeepSearchPane>>, pub window: Option<AnyWindowHandle> }
impl MainPanes { pub fn list(&self) -> Option<Entity<ListPane>>; pub fn viewer(..); pub fn deep(..) }  // upgraded
pub fn main_panes(cx: &App) -> MainPanes;                                             // empty before the main window
pub struct ActivePane { pub window: Option<AnyWindowHandle>, pub pane: PaneFocus, /* per-window map */ }
impl ActivePane { pub fn set(cx, window, pane); pub fn of_window(cx, window) -> PaneFocus }  // updated by on_focus_in
pub type CloseFn = Box<dyn FnOnce(&mut Window, &mut App)>;   // runs deferred, in the dialog's own window
pub struct ClosedTarget { pub window: AnyWindowHandle, pub title: SharedString }
pub struct OpenTargets { /* dialog id → (window, path, title, close fn) */ }
impl OpenTargets {
    pub fn register(cx: &mut App, window: AnyWindowHandle, path: PathBuf, title: SharedString, close: CloseFn) -> u64;
    pub fn unregister(cx: &mut App, id: u64);                     // the dialog closed on its own
    pub fn close_for(cx: &mut App, paths: &[PathBuf]) -> Vec<ClosedTarget>;   // entries removed now, closes deferred
    pub fn holds(cx: &App, path: &Path) -> bool; pub fn len(cx: &App) -> usize;
}
pub fn same_path(a: &Path, b: &Path) -> bool;                 // exact, then case-insensitive on Windows
pub enum SemanticGateState { Open, Exclusive(SemanticOp) }
pub struct SemanticGate { pub state: SemanticGateState }      // current/is_open/enter_exclusive(op, cx) -> bool/leave(cx)
pub struct LastToast(pub SharedString);                       // read by QA probes (last_toast(cx))
pub enum CorpusNotice { Deleted(Vec<PathBuf>) }
pub struct CorpusBus;  // emits CorpusNotice through cx.emit on a small global Entity<CorpusBus> (CorpusBusHandle)
impl CorpusBus { pub fn install(cx); pub fn entity(cx) -> Entity<CorpusBus>; pub fn broadcast(notice, cx) }
// install() also registers the app-level handler: OpenTargets::close_for + `Closed: "<title>" was deleted` toast
// in each closed dialog's window (works with or without a main window).
```

`app::run(launch, env)` = `run_with_services(launch, env, Services::for_launch(&launch))` (§7.4). `app::init(cx,
launch, env, services)` (frozen) calls owner functions in this order (each exists as a WP0 stub with the final
signature):
0. (as built) the `Services` and `DebugBufferHandle` globals first (the debug sink needs them); recording services
   also install the clipboard guard (`services::install_clipboard_guard`, §7.4).
1. `debug_sink::install(cx, launch.args.debug)` (WP7) — core sink → DebugBuffer and, for lines at/above the
   `--debug` level, stderr through `Services`. Core filters by the `min_level` its callers pass, so the GUI passes the
   capture level = the lower of the `--debug` level and the Debug log dialog's level (default Info).
2. `startup::load_config(&env) -> (LayeredConfig, ConfigErrors)` (WP7) — `load_config_from` and
   `load_gui_config_from` independently; a failing file contributes defaults.
3. `persist::load(&env) -> UiState` (WP6).
4. `startup::resolve_workspace(&launch, &env, &config.gui, &ui) -> Option<WorkspaceScope>` (WP7, PD3).
5. `startup::resolve_initial_view(&launch, &config, &settings, &ui) -> (ViewPrefs, ListSearchMode)` (WP7, PD17).
6. set `AppState`, `Services`, `DebugBufferHandle`, `SemanticGate`, other globals.
7. `embedder::install(cx)` (WP7).
8. `keymap::install_all(cx)` (WP6), `menus::install(cx)` (WP6), `palette::install(cx)` (WP6).
9. `theme_tokens::apply_startup_theme(cx)` (WP6).
10. global `cx.on_action(|_: &Quit, cx| exit::quit(cx))` and, on Windows/Linux only (for the context-free `alt-f4`),
    `cx.on_action(|_: &CloseWindow, cx| …)` (WP0). Menu enabled state comes from **element** listeners
    (`Window::is_action_available` ignores global ones): both window roots also handle `Quit`, only single-file roots
    handle `CloseWindow`, and macOS registers no global `CloseWindow` (native menus count global listeners, which would
    enable File → Close window in the main window).

`app::init_for_test(cx, world, launch, recording: Arc<RecordingServices>)` (as built) = the same with
`RecordingServices`, persist writes disabled, no debug sink (process-wide: tests install it under `global_lock`), a
refusing embedder unless the test installs a fake, and the world's `[gui] theme` applied to the app
(`theme_tokens::apply_startup_theme_to_app`; core's process-wide palette is untouched) (§15.2).

### 7.4 PlatformServices (services.rs; trait frozen in WP0)

Every side effect that the GPUI test platform cannot perform, that touches the user's desktop, or that must never run
in tests/QA goes through this trait. CI's safety lint (§16.2) forbids the underlying calls anywhere else.

```rust
pub trait PlatformServices: Send + Sync + 'static {
    fn reveal_path(&self, path: &Path, cx: &mut App);
    fn open_url(&self, url: &str, cx: &mut App);
    fn open_with_system(&self, path: &Path, cx: &mut App);
    fn prompt_save_path(&self, dir: &Path, suggested_name: &str, window: &mut Window, cx: &mut App,
                        done: Box<dyn FnOnce(Result<Option<PathBuf>, String>, &mut Window, &mut App)>);
    fn prompt_open_paths(&self, options: OpenPathsOptions, window: &mut Window, cx: &mut App,
                         done: Box<dyn FnOnce(Result<Option<Vec<PathBuf>>, String>, &mut Window, &mut App)>);
    fn clipboard_write(&self, text: String, cx: &mut App);
    fn clipboard_read(&self, cx: &mut App) -> Option<String>;
    /// Blocking (≤ wait); call from a background task. Real: core terminal::spawn_terminal_checked (K17).
    /// Ok(None) = still running after `wait`; Ok(Some(code)) = exited early with `code`; Err = could not spawn.
    fn spawn_terminal(&self, command: &TerminalCommand, wait: Duration) -> Result<Option<i32>, String>;
    /// Unix picker resume: flush, close windows, chdir + execvp; the real one never returns on success (only Err).
    /// RecordingServices returns Ok(()) to stand for that success: callers treat Ok as "the launching terminal took
    /// over" and quit with exit 0 and no output.
    fn exec_in_current_terminal(&self, spec: &LaunchSpec) -> Result<(), String>;
    fn stdout_kind(&self) -> StdoutKind;                 // Redirected | Terminal | None (after AttachConsole on Windows)
    fn stdin_is_tty(&self) -> bool;                      // Unix picker exec decision (PD5); false on Windows
    fn write_stdout(&self, text: &str) -> Result<(), String>;
    fn write_stderr(&self, text: &str);
    /// Blocking; background. Real: core update::check_latest_release.
    fn fetch_latest_release(&self, current_version: &str) -> Result<UpdateStatus, String>;
    /// Blocking (≤ 3 s); background. macOS/Linux login+interactive shell PATH (§14.2); None on Windows.
    fn login_shell_path(&self) -> Option<OsString>;
}
pub type SaveDone = Box<dyn FnOnce(Result<Option<PathBuf>, String>, &mut Window, &mut App)>;   // prompt_save_path
pub type OpenDone = Box<dyn FnOnce(Result<Option<Vec<PathBuf>>, String>, &mut Window, &mut App)>; // prompt_open_paths
#[derive(Clone)] pub struct Services { pub inner: Arc<dyn PlatformServices>, pub recording: Option<Arc<RecordingServices>> }
impl Global for Services {}
impl Services {
    pub fn real() -> Self;
    pub fn recording(recording: Arc<RecordingServices>) -> Self;
    /// app::run: a `qa` build started with --qa-scenario records every side effect (with --qa-real-console its
    /// stdout/stderr are the real console's); every other run uses RealServices.
    pub fn for_launch(launch: &LaunchContext) -> Self;
    pub fn is_recording(&self) -> bool;
}
impl Deref for Services { type Target = dyn PlatformServices; }  // services(cx).write_stdout(..)
pub fn services(cx: &App) -> Services;
pub fn recorded_calls(cx: &App) -> Vec<ServiceCall>;            // tests/QA; panics without RecordingServices
pub struct RecordingServices { /* Mutex<state>: calls, answer queues, in-memory clipboard, scripted results */ }
impl RecordingServices {
    pub fn new() -> Self;                                         // stdout kind None, stdin not a tty
    pub fn with_real_console() -> Self;                           // --qa-real-console: real stdout/stderr, still recorded
    pub fn answer_save(&self, answer: Result<Option<PathBuf>, String>);
    pub fn answer_open(&self, answer: Result<Option<Vec<PathBuf>>, String>);
    pub fn set_stdout_kind(&self, kind: StdoutKind);
    pub fn set_stdin_is_tty(&self, tty: bool);
    pub fn set_release(&self, result: Result<UpdateStatus, String>);
    pub fn set_spawn_result(&self, result: Result<Option<i32>, String>);    // default Ok(None)
    pub fn set_exec_result(&self, result: Result<(), String>);              // default Ok(())
    pub fn set_login_shell_path(&self, path: Option<OsString>);
    pub fn set_clipboard(&self, text: Option<String>);
    pub fn calls(&self) -> Vec<ServiceCall>; pub fn clear_calls(&self);
    pub fn clipboard(&self) -> Option<String>; pub fn terminal_commands(&self) -> Vec<TerminalCommand>;
    pub fn stdout(&self) -> String; pub fn stderr(&self) -> String;          // concatenated recorded output
}
pub enum ServiceCall { Reveal(PathBuf), OpenUrl(String), OpenWithSystem(PathBuf), PromptSave { dir: PathBuf, name: String },
                       PromptOpen(OpenPathsOptions), ClipboardWrite(String), Terminal(TerminalCommand), Exec(LaunchSpec),
                       Stdout(String), StdoutFailed(String) /* write_stdout failed (no stdout); nothing written */,
                       Stderr(String), FetchRelease, LoginShellPath }
pub struct ClipboardGuard; impl RootPlugin for ClipboardGuard {}   // (as built) see below
pub fn install_clipboard_guard(cx: &mut App);
```
An unanswered prompt in `RecordingServices` answers `Ok(None)` (cancel) after the test's `run_until_parked`.
**Clipboard guard (as built):** whenever the services record (tests, QA), `app::init`/`init_for_test` register the
`ClipboardGuard` root plugin: its capture-phase listeners on every window root take over gpui-kit's own `Copy`,
`Cut` and `Paste` (inputs, textareas, dialogs, text views) and run them through `Services`, so a test or QA run never
touches the real clipboard whatever has focus. gpui-kit components that perform side effects themselves (`Link` with
an `href`, `PopupMenu::link`, a `TextView` without a link handler, the `Clipboard` copy button) must not be used
without routing them through `Services`. `RealServices` refuses every side effect in `cfg(test)` builds.
Debug builds of `RealServices` refuse `spawn_terminal`/`exec_in_current_terminal` for `claude`/`pi`/`omp` unless a
program override is configured or `CHG_ALLOW_REAL_LAUNCH=1` (§15.4).

### 7.5 Background work model

| Work | Mechanism | Result delivery | Stale guard |
|---|---|---|---|
| Initial load / refresh / reload | core `HistoryLoader` (`LoaderOptions::with_waker`), K1 off-thread preparation, K18 policy | waker → `async_channel::bounded::<()>(1)` → ListPane `_poll_task` → `loader.poll(&mut browser)` then `browser.poll()` | core |
| Lexical + semantic search | core `Browser` workers, same waker | same task → `browser.poll()` → `on_results` | core generations |
| UUID lookup (K2) | `cx.background_spawn(history::load_session_by_uuid)` when `uuid_lookup() == Pending` | `browser.resolve_uuid_lookup(uuid, conv)` | core ignores a stale UUID |
| Rename (K3) | `cx.background_spawn(history::rename_session(..))` | `browser.apply_renamed(path, conv)` | dialog id + path |
| Delete (K4) | `background_spawn(plan_session_delete)` → dialog → `background_spawn(execute_session_delete)` | `CorpusNotice::Deleted(plan files)` | dialog disabled while running |
| Viewer open / rebuild | `viewer-worker` std thread per ViewerPane: a single **latest-request slot** (Mutex + Condvar) and an `AtomicU64` generation; the worker checks the generation between entries/items and abandons stale work; result posted to an async channel | `this.update_in(v.install(gen, result))`; the previous document is dropped right after install | generation |
| Syntect highlighting | `cx.background_spawn` per batch of visible code segments; warm-up at startup on `syntect-warmup` | cache insert + `cx.notify()` | key `(item_id, chunk, seg, ThemeKind)` |
| Agent requests | one `agent-worker` std thread per process (`AgentWorkerHandle`, a global); **every consumer** (Deep search pane, each agent panel incl. single-file windows') takes its own `AgentClient` (as built) with its own reply channel; one pending slot **per (client, request kind)** (Search, Within, Read, Outline, KeyForPath); submitting a new job of a kind cancels that client's running job of that kind (`SemanticCancellationToken`, K7) and replaces its queued one, never touching other clients' jobs; Cancel button cancels | the client's `async_channel::unbounded::<AgentReply>` (`AgentClient::replies`) → DeepSearchPane / AgentPanel | job generation |
| Export / copy-as | `cx.background_spawn(generate_export / export_to_path)` | toast / clipboard (services) | n/a |
| Terminal spawn | `cx.background_spawn(services.spawn_terminal(cmd, 3 s))` per launcher candidate | toast | n/a |
| Tools (cache generation, delete-empty scan, reports, update check, fresh corpus loads) | std thread + `async_channel` progress; semantic tools take the semantic gate | dialog entity task | dialog lifetime; `SemanticCancellationToken` |
| Workspace picker project paths | `background_spawn` over `corpus_snapshot()` after each load/refresh (`is_dir` per distinct path) | cached on ListPane | load id |
| Relative-time ticker | `cx.spawn` loop: `timer(30 s)`; `now = Local::now(); notify` | | |
| state.json save | `persist::schedule_save(cx)` 500 ms debounce + `cx.on_app_quit` flush | | |
| Login-shell PATH | `login-shell-path` std thread at startup (macOS/Linux, not started from a terminal) | `AppState.env.path_var` updated; resume waits ≤ 3 s for it (§14.2) | once |

Rules: tasks are stored on their owner (`_xxx_task: Task<()>`), never bare `cx.spawn(..);`; `WeakEntity::update`
results are ignored when the entity is gone; `cx.notify()` once per coherent change; worker threads exit when their
channel closes; every named thread uses an 8 MiB stack.

### 7.6 Semantic gate and the shared embedder

* `embedder.rs` (WP7) owns one `SharedEmbedder`: `Arc<Mutex<Option<Box<dyn SemanticEmbedder + Send>>>>` loaded lazily
  (`FastembedEmbedder::new_in(env.semantic_model_dir(), false)`) on the first thread that needs it. It hands out
  adapters for the three core factory shapes: `browser::SemanticEmbedderFactory` (Arc, for `BrowserOptions`),
  `agent::service::EmbedderFactory` (Box, for `AgentService::with_embedder_factory`), and
  `semantic_ops::EmbedderFactory<'_>` (`&mut FnMut`, for Tools). Each adapter returns a thin `SemanticEmbedder` that
  locks the shared model per call. Result: one model in memory; Clear can drop it. Tests/QA install a fake through
  `embedder::install_fake(cx, factory)`; `init_for_test` installs a refusing factory by default (`semantic model is
  not available in tests`). `--qa-model-dir` points the real factory at a pre-downloaded model dir (feature `qa`).
  If `FastembedEmbedder` turns out not to be `Send`, WP7 falls back to per-consumer instances and records it (risk
  §19).
* `SemanticGate` (globals.rs): `Open` normally. Generate/Clear semantic cache (WP5) call
  `gate.enter_exclusive(op, cx)`: ListPane `browser.suspend_semantic()` (K6; joins the worker; footer `sem paused`),
  AgentWorker drops queued semantic/hybrid jobs with `Semantic search is paused while the cache is
  generated/cleared` and stops using the embedder, the shared model is dropped (Clear) or kept (Generate uses it).
  `gate.leave(cx)` → `browser.resume_semantic()` (reloads the cache from disk), AgentWorker accepts jobs again. While
  `Exclusive`, a second Generate/Clear is disabled. Cache files are additionally protected by core's write lock (K6),
  so a second GUI instance or a racing writer never corrupts them.

### 7.7 Focus model and key contexts

| Context | Element | Focus handle / focused element |
|---|---|---|
| `Workspace` | main window root `v_flex` | none itself (ancestor of all main-window regions) |
| `SingleFileWindow` | single-file window root | none itself |
| `ConversationList` | Conversations tab container (search + toolbar + list + footer) | the search `InputState` |
| `ListToolbar` | toolbar container (F6 region, `ListPane::focus_toolbar`) | its buttons (`tab_stop`); the list's keys are bound in `ConversationList && !ListToolbar`, so a focused toolbar button keeps Enter/Space/Tab/arrows |
| `DeepSearch` | Deep search tab container | query `InputState`, form inputs, or `deep_results_focus` (results list, `tab_stop`) |
| `DeepResults` (as built) | Deep search results list inside `DeepSearch` | `deep_results_focus`; hit keys (`up`/`down`/`enter`) are bound here |
| `Viewer` | transcript column `v_flex` (header, toolbar, transcript, status) | `viewer_focus` (`tab_stop(true)`), or the find `InputState` |
| `AgentPanel` | agent panel root — **sibling** of `Viewer` inside the viewer split | panel inputs or `panel_focus` |
| `AgentRows` (as built) | the agent panel's row list (outline/hit/read rows) inside `AgentPanel` | `panel_focus`; row keys (`up`/`down`/`j`/`k`/`enter`) are bound here |
| `Confirm` | focused content div inside destructive AlertDialogs (§5.7) | content focus handle (focused on open) |
| `RenameDialog`, `ExportMenu`, `SemanticDetails` | our dialog content roots | dialog content focus handle |
| `Input`, `Root`, `Dialog`, `List`, `PopupMenu`, command palette | gpui-kit | gpui-kit |

Focus rules:
* Initial focus: main window → list search input; single-file → `viewer_focus`.
* ListPane root has `on_mouse_down(Left)`: `if !window.default_prevented() { search.focus() }`; rows call
  `window.prevent_default()` on mouse down so clicks keep the search input focused (spike-list §4).
* `ViewerPane::focus(window, cx)` focuses `viewer_focus` (after the first frame of a newly opened conversation; if
  the viewer is empty it is a no-op, DG §5.4). Clicking inside the transcript focuses `viewer_focus` (text selection
  works; SelText does not take focus).
* Switching left tabs focuses the new tab's primary element. `cx.on_focus_lost(window, …)` on Workspace →
  `window.focus_lost_restore_target(cx)` if still rendered, else the **visible** left tab's primary element (search
  input or deep-search query; the list pane if collapsed is skipped), else `viewer_focus`.
* `on_focus_in` handlers on the list, deep search, viewer and agent panel roots update `ActivePane` (§7.8).
* Hiding the left pane while it holds focus moves focus to the viewer first.
* (as built) F6 / Shift+F6 regions include the list toolbar; leaving the viewer (`LeaveRequested`) returns to the
  Conversations tab's search input; Esc in the agent panel (nothing to clear or cancel) focuses the transcript.
  Test windows are activated like the OS activates the real one (`test_support::activate`), otherwise no focus-in
  events reach the panes and `ActivePane` never changes.
* (as built) With the agent panel focused, the viewer's session handlers (registered on the transcript column) use the
  panel result set's target; Deep search and list focus route session actions to their own target first.
* Dialogs restore focus to the previously focused element on close (gpui-kit behavior); the palette stores the
  focus it returns to.
* Tab/Shift-Tab: in `ConversationList`, Tab = ToggleScope (TUI) **only when a workspace exists**; otherwise the
  handler calls `cx.propagate()` and Root's focus cycling moves to the toolbar. Shift-Tab = Root focus-prev. F6 /
  Shift+F6 cycle regions (AD16). In other contexts Tab/Shift-Tab cycle focus (Root).

### 7.8 Target capture (PD15, ADD A1/A2)

```rust
// model.rs (frozen)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationTarget {
    pub path: PathBuf,
    pub source: Option<Source>,          // None = unknown: Resume/Fork/Rename/Delete disabled ("Unknown conversation type")
    pub session_id: String,              // Conversation.session_id (Pi/OMP: header id); file stem when unknown
    pub file_stem: String,               // path.file_stem() lossy (TUI "session ID")
    pub title: SharedString,             // custom_title → summary → sanitized preview → file stem, ≤ 80 chars
    pub project_name: Option<SharedString>,
    pub project_path: Option<PathBuf>,
    pub custom_title: Option<String>,
}
impl ConversationTarget {
    pub fn from_conversation(c: &Conversation) -> Self;                 // source = Some(c.source)
    pub fn from_agent_info(info: &AgentConversationInfo, title: Option<&str>) -> Self;  // source = Some(info.source)
    /// Single-file windows: source from the parsed conversation when parsing succeeded (background), else None.
    pub fn from_parsed_file(path: &Path, parsed: Option<&Conversation>) -> Self;
    pub fn describe(&self) -> String;                                    // "proj · Fix login bug · 3f2a19c0-…"
    pub fn header_id_differs(&self) -> bool { self.session_id != self.file_stem }
    pub fn can_modify(&self) -> bool { self.source.is_some() }
}
```
Where targets come from:
* **Pane handlers** resolve their own target synchronously in the action handler: ViewerPane → its open target;
  ListPane → `selected_target()` after `workspace.flush_pending_preview()`; DeepSearchPane → selected hit target;
  AgentPanel → the result set's target.
* **Menus, palette and keys** dispatch to the focused element's path, so the pane that has focus handles the action.
  The Workspace fallback (no pane on the dispatch path, e.g. focus lost) uses `ActivePane` (last pane that received
  focus in that window) and that pane's target; a list target flushes the pending preview first; no target → toast
  `No conversation selected`.
* **Row menu** items carry a token for the target captured when the menu was built (§5.2).
* **Deep Search hits** → `from_agent_info` (source and session id from core, never re-detected).
* **Dialogs and result sets** store the target; all later steps (confirm, execute, follow-up) use it, and they
  register in `OpenTargets` so deletions close them (§7.2).

### 7.9 Shared value types (model.rs, frozen; complete list)

`ConversationTarget` (§7.8); `HeaderInfo { title: SharedString, project_name: Option<SharedString>, custom_title:
Option<SharedString>, model: Option<String>, message_count: usize, duration_minutes: Option<u64>, total_tokens: u64,
timestamp: DateTime<Local>, summary: Option<SharedString>, parse_error_count: usize, file_stem: SharedString }`
(+`from_conversation`, `stem_only`); `OpenRequest { target: ConversationTarget, focus: bool, reveal:
Option<RevealRequest> }`; `RevealRequest { path: PathBuf, full_ref: Option<String>, kind: RevealKind }`;
`RevealKind::{AgentOrdinal { ordinal: usize, hint: ContentVisibilityHint }, EntryIndex(usize)}`;
`ContentVisibilityHint { tools, tool_results, thinking, subagents: bool }`; (as built) `OpenRequest` also has
`agent_tab: Option<AgentTab>` (open the agent panel on that tab and focus it) with `OpenRequest::{new(target, focus),
with_reveal(reveal), with_agent_tab(tab)}`; `ViewerMode::{Main, SingleFile}`;
`AgentTab::{Outline, Within, Read}`; `LeftTab::{Conversations, DeepSearch}`; `PaneFocus::{List, ListToolbar,
DeepSearch, Viewer, AgentPanel, Other}`; `HelpSection::{List, Viewer, AgentPanel, DeepSearch, Global, Dialogs}`;
`SettingsPage::{General, Display, Search, Keys, Resume, Agent}`; `KeyAction::{Resume, Fork, Rename, Delete,
Refresh}` (+`label`, TUI priority order); `KeyValidation { errors: Vec<(KeyAction, String)>, warnings: Vec<String> }`;
`SemanticReportMode::{Search { top: usize }, DebugDump}`; `DebugLogTab::{Messages, ParseErrors, File}`;
`SemanticOp::{Generate, Clear}`; `StartupTool::{ShowDir, DeleteSession(String), DebugSearch { query: String, local:
bool }, SemanticSearch { query: String, top: usize, local: bool }, SemanticDebug { query: String, local: bool },
GenerateSemanticCache { local: bool }, ClearSemanticCache, DeleteEmpty { local: bool }, CheckForUpdates}`;
`LaunchContext { args: GuiLaunchArgs (core), launch_cwd: Option<PathBuf>, time_filter: TimeFilter, picker:
Option<PickerAction>, startup_tool: Option<StartupTool>, file: Option<PathBuf>, qa: Option<QaArgs> }` (+`is_picker`);
`QaArgs { scenario: PathBuf, out_dir: PathBuf, model_dir: Option<PathBuf>, real_console: bool }` (`real_console` =
`--qa-real-console`, as built); `ConfigErrors { cli: Option<String>, gui:
Option<String> }`; `StdoutKind::{Redirected, Terminal, None}`; `OpenPathsOptions { files: bool, directories: bool,
multiple: bool, prompt: SharedString }`; `RowMenuItem::{Open, Resume, Fork, Rename, Export, CopyAs(ExportFormat),
CopyPath, CopySessionId, CopyHeaderSessionId, Reveal, SearchWithin, Outline, Delete}`. Core types are used directly
(`ToolDisplayMode`, `ViewPrefs`, `ExportFormat`, `ExportOptions`, `PickerAction`, `ListSearchMode`, `TerminalCommand`,
`LaunchSpec`, `UpdateStatus`); no GUI mirror types. Helpers added by WP0 (additive): `TITLE_MAX_CHARS` (80),
`file_stem_lossy`, `short_session_id` (`describe()` shortens the session ID with it), `truncate_chars`,
`HeaderInfo::is_stem_only`, `KeyAction::{ALL, PRIORITY, config_key, priority}`, `HelpSection::{ALL, title}`,
`SettingsPage::{ALL, title}`, `KeyValidation::is_ok`, `ConfigErrors::is_empty`.

---

## 8. Actions and keymap

### 8.1 Principles
* One command = one action type (DG §4.4). Key bindings, menu items, palette items, context-menu items, toolbar
  buttons and the QA runner all dispatch the same action.
* All actions live in `crates/gui/src/actions.rs` (WP0, frozen), namespace `history`, unit actions via
  `gpui_kit::actions!`, data actions via `#[derive(Clone, PartialEq, Debug, gpui_kit::Action)]
  #[action(namespace = history, no_json)]`. The QA runner builds unit actions by name
  (`cx.build_action("history::Resume", None)`).
* Scope every binding to a context (exceptions: `Quit` and `alt-f4` CloseWindow have no context and global
  handlers, so they work while a dialog or nothing has focus). Bindings that must work while the list search
  `Input` has focus and that the Input would otherwise consume are registered a second time with the predicate
  `ConversationList > Input` (spike-list; GK-INFRA §8.4).
* Plain-letter bindings use `Viewer && !Input`, `DeepSearch && !Input` or `AgentPanel && !Input` so typing in any
  input is never swallowed. Navigation keys that a single-line Input does not consume (`up`, `down`, `pageup`,
  `pagedown`) also use `… && !Input` in the viewer, so they never scroll while the find bar has focus (V-079).
* **Session actions are handled by panes** (ListPane, ViewerPane, DeepSearchPane, AgentPanel), each resolving its
  own target (§7.8). Menus and the palette dispatch to the element that had focus (AppMenuBar remembers it; the
  palette stores it), so the focused pane handles them. `workspace/routing.rs` (frozen, shared by Workspace and
  SingleFileWindow) provides the **fallback** for actions dispatched with no pane on the path: list-view actions
  (SetMode*, SetScope*, ToggleScope, ToggleSearchMode, ShowSemanticDetails, Refresh) → ListPane; viewer actions
  (CycleToolDisplay, SetTools*, ToggleThinking, ToggleTiming, StartSearch (focuses the viewer first), NextMatch,
  PrevMatch, ToggleOutline, YankOrCopyMessage, CopyFocusedMessage) → ViewerPane (toast `No conversation open` when
  empty); session actions → the `ActivePane` of that window (§7.8).
* **Routing API (as built, `workspace/routing.rs`):**
  * `run_session_action(action, target, options: ExportOptions, window, cx) -> SessionRoute` runs a session action
    on a captured target through the `ops` entry points and tells the pane what is left: `SessionRoute::Done`
    (handled; Resume/Fork/Rename/Delete on an unknown source do nothing), `Open { agent_tab: Option<AgentTab> }`
    (OpenConversation → `agent_tab: None`; Search within / Outline / Read range → that tab: the list emits
    `ListPaneEvent::OpenRequested`, Deep search `DeepSearchEvent::OpenHit`, the root's fallback returns
    `Forwarded::Open`; the viewer opens its own panel), `Viewer` (CopyFocusedMessage: a pane other than the viewer
    calls `cx.propagate()` so the root forwards it to its viewer), `NotSession`.
  * `forward(action, &RouteTargets { list, viewer, deep, active }, window, cx) -> Forwarded::{Unhandled, Handled,
    Open(Box<OpenRequest>)}` is the root fallback; a session action goes to the `ActivePane` first when that is the
    list, its toolbar or Deep search (their target, even for Outline / Copy message).
  * `row_menu_action(item: RowMenuItem) -> Box<dyn Action>` maps row-menu items to their session actions;
    `run_app_action` handles window-level commands (tools, settings, help, palette, theme, copy) for both roots;
    predicates `is_list_action`, `is_viewer_action`, `is_session_action`, `needs_known_source`, `is_main_window_only`,
    `agent_tab_for`; registration macros `on_actions!`, `capture_actions!`, `on_session_actions!`,
    `capture_session_actions!`.
  * Menu enabled state (§8.9, §8.10) is "the action has a listener on the dispatch path"
    (`Window::is_action_available`), so roots and panes register a listener only where the action can run: `Quit` on
    both roots (plus the global listener for focus-less dispatch), `CloseWindow` only on single-file roots,
    `OpenJsonlFile` only outside picker mode, `ShowConversations` only on single-file roots, and Resume/Fork/Rename/
    Delete/SelectAndExit never in single-file windows.
* Registration order inside `keymap::install_all(cx)` (after `gpui_kit::init`): (1) static table §8.3 in table
  order, (2) input-editing bindings §8.4, (3) user-configurable bindings §8.5 in the order rename, refresh, fork,
  resume, delete (later wins at equal depth, which reproduces the TUI check order delete > resume > fork > refresh >
  rename, C-028).

### 8.2 Action list (actions.rs)

```rust
gpui_kit::actions!(history, [
  // app / window
  Quit, CloseWindow, ShowConversations, OpenSettings, ShowShortcuts, CommandPalette, OpenJsonlFile,
  ChooseWorkspace, ClearWorkspace, FocusConversations, FocusDeepSearch, FocusViewer, FocusNextRegion,
  FocusPrevRegion, ToggleListPane, Refresh, SetThemeSystem, SetThemeLight, SetThemeDark, SmartCopy, EditCopy,
  // tools / help
  DeleteEmptyTranscripts, GenerateSemanticCache, ClearSemanticCache, DebugSearch, SemanticDebugSearch,
  ShowDebugLog, ShowProjectsDirectory, DeleteSessionById, CheckForUpdates, ShowAbout,
  // session (target-resolving)
  OpenConversation, Resume, Fork, Rename, Delete, ExportConversation, YankOrCopyMessage, CopyFocusedMessage,
  CopyPath, CopySessionId, CopyHeaderSessionId, ShowPath, RevealInFileManager, SelectAndExit,
  SearchWithinConversation, ToggleOutline, ReadRange, ShowSemanticDetails,
  CopyAsLedger, CopyAsPlain, CopyAsMarkdown, CopyAsJsonl,
  // list
  SelectPrev, SelectNext, SelectPageUp, SelectPageDown, SelectHalfPageDown, SelectFirst, SelectLast,
  ClearQuery, ToggleScope, ToggleSearchMode, SetModeLexical, SetModeSemantic, SetScopeAll, SetScopeWorkspace,
  OpenRowContextMenu,
  // viewer
  ScrollLineDown, ScrollLineUp, NextMessage, PrevMessage, ScrollHalfPageDown, ScrollHalfPageUp,
  ScrollPageDown, ScrollPageUp, ScrollTop, ScrollBottom, StartSearch, NextMatch, PrevMatch, CommitSearch,
  CancelSearch, FindClearQuery, CycleToolDisplay, SetToolsSummary, SetToolsTruncated, SetToolsFull,
  ToggleThinking, ToggleTiming, ToggleOutputs, ViewerEscape, ViewerBack,
  // deep search / agent panel
  RunDeepSearch, CancelDeepSearch, DeepSearchEscape, DeepSelectNext, DeepSelectPrev, DeepOpenHit,
  ToggleAgentText, AgentPanelEscape, AgentSelectNext, AgentSelectPrev, AgentOpenSelected,
  // dialogs
  ConfirmYes, ConfirmNo, ConfirmEnterIgnored, RenameClear, ExportPick1, ExportPick2, ExportPick3, ExportPick4,
]);
#[derive(Clone, PartialEq, Debug, gpui_kit::Action)] #[action(namespace = history, no_json)]
pub struct RowMenu { pub item: RowMenuItem, pub token: u64 }
```

### 8.3 Static key table (default keystrokes)

Columns: **Win/Linux** and **macOS** keystrokes (identical unless shown), **Context** predicate (— = none),
**Handler**, **Menu**. "secondary" = Ctrl / Cmd.

| Action | Win/Linux | macOS | Context | Handler | Menu / notes |
|---|---|---|---|---|---|
| Quit | `secondary-q` | `cmd-q` | — | global `cx.on_action` → `exit::quit` (picker: exit 0, no output) | File → Quit (mac: app menu) |
| CloseWindow | `secondary-w` | `cmd-w` | `SingleFileWindow` | SingleFileWindow → `window.remove_window()` | File → Close window (single-file only) |
| CloseWindow | `alt-f4` | — | — | global: close the active window (main window → Quit) | |
| OpenSettings | `secondary-,` | `cmd-,` | `Workspace`, `SingleFileWindow` | `settings::open_settings` | Tools → Settings… (mac: app menu) |
| ShowShortcuts | `f1` | `f1` | `Workspace`, `SingleFileWindow` | `help::open_shortcuts` | Help → Keyboard shortcuts |
| ShowShortcuts | `?` | `?` | `Viewer && !Input`, `DeepSearch && !Input`, `AgentPanel && !Input` | same | TUI `?` (not while typing) |
| CommandPalette | `secondary-shift-p`, `f10` | `cmd-shift-p`, `f10` | `Workspace`, `SingleFileWindow` | `palette::open` | Help → Command palette… |
| FocusNextRegion / FocusPrevRegion | `f6` / `shift-f6` | same | `Workspace` | Workspace | (AD16) |
| OpenJsonlFile | `secondary-shift-o` | `cmd-shift-o` | `Workspace`, `SingleFileWindow` | `tools::open_jsonl_file_dialog` (disabled in picker mode) | File → Open JSONL file… |
| ShowConversations | — | — | `SingleFileWindow` | open/focus the main window | File → Show conversations |
| ChooseWorkspace | `secondary-shift-w` | `cmd-shift-w` | `Workspace` | `ListPane::choose_workspace_folder` | File → Choose workspace… (AD17) |
| FocusConversations | `secondary-1` | `cmd-1` | `Workspace` | Workspace | View → Conversations |
| FocusDeepSearch | `secondary-2`, `secondary-shift-f` | `cmd-2`, `cmd-shift-f` | `Workspace` **and** `Workspace > Input` | Workspace | Edit → Find in all conversations…; View → Deep search. The `Workspace > Input` duplicate beats Input's macOS `cmd-shift-f` → Replace |
| FocusViewer | `secondary-3` | `cmd-3` | `Workspace` | Workspace | View → Viewer |
| ToggleListPane | `secondary-shift-b` | `cmd-shift-b` | `Workspace` | Workspace | View → Focus viewer (checked) |
| Refresh | `f5` | `f5` | `Workspace` | Workspace → `ListPane::refresh` | View → Refresh (configurable binding §8.5 too) |
| SmartCopy | `ctrl-c` | `cmd-c` | `Workspace`, `SingleFileWindow`, `ConversationList > Input` | Input selection non-empty → copy it; else viewer text selection (§9.5); both through `services.clipboard_write` | Edit → Copy (`EditCopy` → same handler) |
| OpenConversation | `enter` | `enter` | `ConversationList && !ListToolbar` | ListPane (target = selection; focus viewer) | Session → Open. Input emits PressEnter and propagates; ListPane ignores PressEnter |
| SelectAndExit | `ctrl-o` | `ctrl-o` | `ConversationList && !ListToolbar` | `ops::select::select_and_exit` (PD5) | Session → Print and exit / Select (picker) |
| ShowSemanticDetails | `ctrl-s` | `ctrl-s` | `Workspace` | routing → `ListPane::show_semantic_details` | Session → Semantic result details |
| RevealInFileManager | `secondary-shift-r` | `cmd-shift-r` | `Workspace`, `SingleFileWindow` | pane → `ops::copy::reveal` | Session → Reveal in file manager (AD17) |
| SearchWithinConversation | `secondary-shift-i` | `cmd-shift-i` | `Workspace` | pane → agent panel Within tab | Session → Search within conversation… |
| ReadRange | `secondary-shift-m` | `cmd-shift-m` | `Workspace` | pane → agent panel Read tab | Session → Read range… |
| ShowDebugLog | `secondary-shift-l` | `cmd-shift-l` | `Workspace`, `SingleFileWindow` | `tools::open_debug_log` | Tools → Debug log… |
| SelectPrev | `up`, `ctrl-p` | same | `ConversationList && !ListToolbar` | ListPane | single-line Input does not handle up |
| SelectNext | `down`, `ctrl-n` | same | `ConversationList && !ListToolbar` | ListPane | |
| SelectPageUp / SelectPageDown | `pageup` / `pagedown` | same | `ConversationList && !ListToolbar` | ListPane | visible page (AD4) |
| SelectHalfPageDown | `ctrl-d` | `ctrl-d` | `ConversationList && !ListToolbar` | ListPane | half visible page |
| SelectFirst / SelectLast | `home` / `end` | same | `ConversationList && !ListToolbar` **and** `ConversationList > Input` | ListPane | Input binds home/end → dup required |
| ClearQuery | `escape` | `escape` | `ConversationList && !ListToolbar` | ListPane (non-empty → clear+browse; empty → no-op; picker → `exit::quit_without_output`) | Input propagates Esc (no `clean_on_escape`) |
| ToggleScope | `tab` | `tab` | `ConversationList && !ListToolbar` | ListPane; without a workspace → `cx.propagate()` (focus moves on) | View → Scope ▸ |
| ToggleSearchMode | `ctrl-t` | `ctrl-t` | `ConversationList && !ListToolbar` | ListPane | **not registered** when `KeyBindings::conflicts_with_ctrl_t()` (C-029) |
| SetModeLexical / SetModeSemantic / SetScopeAll / SetScopeWorkspace | — | — | — | ListPane (+ routing fallback) | View menu items + toolbar |
| OpenRowContextMenu | `shift-f10`, `menu` | `shift-f10` | `ConversationList && !ListToolbar` | ListPane → NativeMenu at the selected row | |
| ScrollLineDown | `j`, `down` | same | `Viewer && !Input` | ViewerPane | |
| ScrollLineUp | `k`, `up` | same | `Viewer && !Input` | ViewerPane | |
| NextMessage | `shift-j`, `]` | same | `Viewer && !Input` | ViewerPane | |
| PrevMessage | `shift-k`, `[` | same | `Viewer && !Input` | ViewerPane | |
| ScrollHalfPageDown | `d`, `ctrl-d` | same | `Viewer && !Input` | ViewerPane | |
| ScrollHalfPageUp | `u`, `ctrl-u` | same | `Viewer && !Input` | ViewerPane | in the find input ctrl-u clears (§8.4) |
| ScrollPageDown / ScrollPageUp | `pagedown` / `pageup` | same | `Viewer && !Input` | ViewerPane | keys go to the find input while typing (V-079) |
| ScrollTop | `g`, `home` | same | `Viewer && !Input` | ViewerPane | |
| ScrollBottom | `shift-g`, `end` | same | `Viewer && !Input` | ViewerPane | |
| StartSearch | `/` | `/` | `Viewer && !Input` | ViewerPane | Edit → Find in conversation |
| NextMatch / PrevMatch | `n` / `shift-n` | same | `Viewer && !Input` | ViewerPane | only when search is Active |
| NextMatch / PrevMatch | `f3` / `shift-f3` | `cmd-g` / `cmd-shift-g` | `Viewer` | ViewerPane | desktop aliases (additive) |
| CommitSearch | `enter` | `enter` | `Viewer > Input` | ViewerPane | `ViewSearch::commit`; focus back to viewer |
| CancelSearch | `escape` | `escape` | `Viewer > Input` | ViewerPane | `view::escape` (clears a search being typed) |
| CycleToolDisplay | `t` | `t` | `Viewer && !Input` | ViewerPane | View → Tool display ▸ (SetTools*) |
| ToggleThinking | `shift-t` | `shift-t` | `Viewer && !Input` | ViewerPane | View → Show thinking (checked) |
| ToggleTiming | `i` | `i` | `Viewer && !Input` | ViewerPane | View → Show timestamps (checked) |
| ToggleOutputs | `x` | `x` | `Viewer && !Input` | ViewerPane | AD12 |
| ViewerEscape | `escape` | `escape` | `Viewer` | ViewerPane | PD2 unwinding (`view::escape`) |
| ViewerBack | `q` | `q` | `Viewer && !Input` | ViewerPane | → LeaveRequested |
| ExportConversation | `e` | `e` | `Viewer && !Input` | ViewerPane → `ops::export::open_export_menu` | Session/File → Export… |
| YankOrCopyMessage | `y` | `y` | `Viewer && !Input` | ViewerPane (nav active → copy message; else copy menu) | |
| CopyPath | `shift-y` | `shift-y` | `Viewer && !Input` | ViewerPane → `ops::copy::copy_path` | Session → Copy path |
| CopySessionId | `shift-i` | `shift-i` | `Viewer && !Input` | ViewerPane → `ops::copy::copy_session_id` | Session → Copy session ID |
| ShowPath | `p` | `p` | `Viewer && !Input` | ViewerPane → `ops::copy::show_path` | Session → Show file path |
| ToggleOutline | `o` | `o` | `Viewer && !Input`, `AgentPanel && !Input` | ViewerPane (agent panel, Outline tab) | Session → Outline |
| AgentPanelEscape | `escape` | `escape` | `AgentPanel` | AgentPanel (clear input / cancel running / focus transcript) | |
| AgentSelectNext / AgentSelectPrev | `down`, `j` / `up`, `k` | same | `AgentRows` | AgentPanel | outline/hit/read rows |
| AgentOpenSelected | `enter` | `enter` | `AgentRows` | AgentPanel → Reveal | |
| RunDeepSearch | `enter` | `enter` | `DeepSearch > Input`, `AgentPanel > Input` | DeepSearchPane / AgentPanel (current tab) | |
| CancelDeepSearch | — | — | `DeepSearch`, `AgentPanel` | cancel the running job (Cancel button) | |
| DeepSearchEscape | `escape` | `escape` | `DeepSearch` | running → cancel; query → clear; empty → Conversations tab | |
| DeepSelectNext / DeepSelectPrev | `down` / `up` | same | `DeepResults` | DeepSearchPane | |
| DeepOpenHit | `enter`, `secondary-enter` (the handler focuses the viewer when the secondary modifier is held) | same | `DeepResults` | DeepSearchPane | |
| ConfirmYes | `y`, `shift-y` | same | `Confirm` | the confirm dialog | AD18 |
| ConfirmNo | `n`, `shift-n` | same | `Confirm` | the confirm dialog | Esc = Dialog Cancel |
| ConfirmEnterIgnored | `enter` | `enter` | `Confirm` | no-op (beats Dialog's `enter → Confirm`, deeper context) | AD18 |
| RenameClear | `ctrl-u` | `ctrl-u` | `RenameDialog > Input` | clears the whole field | AD19 |
| ExportPick1..4 | `1`..`4` | same | `ExportMenu` | ExportMenu | j/k → base `SelectUp`/`SelectDown`, Enter → `Confirm`, Esc → `Cancel` |

Context notes (as built): the list rows above use `keymap::CTX_LIST_KEYS` = `ConversationList && !ListToolbar` so
a focused list-toolbar button keeps Enter/Space/Tab/arrows; Deep search hit keys use the `DeepResults` context of the
focusable results list and agent-panel row keys the `AgentRows` context of the panel's row list (instead of
`DeepSearch && !Input` / `AgentPanel && !Input`, which also matched the panes' buttons and form controls).

### 8.4 Input-editing bindings
* **Every input, Windows/Linux** (`Input` context, registered after gpui-kit's): `ctrl-w` →
  `DeleteToPreviousWordStart` (Ctrl+W never closes a window, PD6).
* **List search** (`ConversationList > Input`, TUI emacs keys, using gpui-kit Input actions):

| Key | Action | TUI meaning |
|---|---|---|
| `ctrl-e` | `MoveEnd` | cursor end |
| `ctrl-b` | `MoveLeft` | cursor left |
| `ctrl-f` | `MoveRight` — registered only while Fork is not bound to `ctrl+f` (re-evaluated by `install_user_bindings`) | cursor right (LIST §7.3) |
| `alt-b` / `alt-f` | `MoveToPreviousWord` / `MoveToNextWord` | word left/right |
| `ctrl-k` | `DeleteToEndOfLine` | kill to end |
| `ctrl-u` | `DeleteToBeginningOfLine` | kill to start (README's "half page up" is wrong; code wins) |
| `ctrl-w` | `DeleteToPreviousWordStart` | delete word |
| `ctrl-a` | Input default (Windows/Linux `SelectAll`; macOS `MoveHome`) | AD2 |
| `ctrl-left/right`, `left/right`, `backspace`, `delete`, `ctrl-home/end` | Input defaults | |

* **Viewer find input** (`Viewer > Input`): `ctrl-u` → `FindClearQuery` (TUI: clear query), `ctrl-w` → word delete
  (above).
* **Rename input** (`RenameDialog > Input`): `ctrl-u` → `RenameClear`, `ctrl-e` → `MoveEnd`; `ctrl-home`/`ctrl-end`
  are Input defaults; `ctrl-a` = select all on Windows/Linux (AD19). Deep-search and other dialog inputs keep Input
  defaults plus `ctrl-w`.

### 8.5 User-configurable bindings (`[keys]`)
Five actions — **Resume, Fork, Rename, Delete, Refresh** — take their keystroke from the effective `KeyBindings`
(`gui.toml [keys]` > `config.toml [keys]` > defaults ctrl+r, ctrl+f, f2, ctrl+x, ctrl+l; core
`settings::EffectiveSettings::keys`). Each is registered in `ConversationList && !ListToolbar`, `ConversationList >
Input` and `Viewer && !Input` (`keymap::USER_BINDING_CONTEXTS`; AD8 adds Rename/Refresh in the viewer). Single-file windows have no handler for
Resume/Fork/Rename/Delete → menu items disabled (parity). While loading, Resume/Fork/Rename/Delete and SelectAndExit
handlers return early and their hints are dimmed (AD6).

Validation and warnings (Settings → Keys, also applied at startup):
* Errors (binding rejected, previous value kept): core `parse_key_binding` messages (`Unknown modifier: …`,
  `Function keys do not support modifiers: …`, `Invalid key binding: …`, `Unknown key: …`).
* Warnings (binding accepted, shown in Settings and as one Info line in the debug sink at startup): plain character
  (`"<c>": a plain character can't be typed in the search field`), `ctrl+t` (`ctrl+t disables the Lexical/Semantic
  shortcut`), duplicates among the five (`<a> and <b> share <key>; <winner> wins` using TUI priority), shadowing a
  static binding in the same context (`<key> replaces "<label>" in the conversation list/viewer`).
* `ctrl+c`, `ctrl+v`, `ctrl+a`, `ctrl+z`, `ctrl+y`, `ctrl+w` are rejected with `<key> is reserved for clipboard and
  editing` **[FLAG: TUI allowed them; in the GUI they would break copy/paste/word delete in inputs]**.

### 8.6 Conversion and live rebinding (keymap.rs)
```rust
pub const CTX_LIST: &str = "ConversationList";
pub const CTX_LIST_KEYS: &str = "ConversationList && !ListToolbar";   // (as built) the list's keys
pub const CTX_LIST_TOOLBAR: &str = "ListToolbar";
pub const CTX_LIST_INPUT: &str = "ConversationList > Input";
pub const CTX_VIEWER_KEYS: &str = "Viewer && !Input";
pub const CTX_DEEP_RESULTS: &str = "DeepResults";                    // (as built)
pub const CTX_AGENT_ROWS: &str = "AgentRows";                        // (as built)
// … plus CTX_VIEWER, CTX_VIEWER_INPUT, CTX_WORKSPACE(_INPUT), CTX_SINGLE_FILE, CTX_DEEP(_KEYS/_INPUT),
//   CTX_AGENT(_KEYS/_INPUT), CTX_CONFIRM, CTX_RENAME_INPUT, CTX_EXPORT_MENU, CTX_INPUT, USER_BINDING_CONTEXTS
pub fn install_all(cx: &mut App);                                  // static + input editing + user (AppState.settings.keys)
pub fn static_bindings() -> Vec<KeyBinding>;                       // §8.3 + §8.4 tables, platform-filtered
pub fn user_bindings(keys: &KeyBindings) -> Result<Vec<KeyBinding>, String>;  // KeyChord::gpui_keystroke + KeyBinding::load(.., &DummyKeyboardMapper)
pub fn install_user_bindings(cx: &mut App, keys: &KeyBindings) -> Result<(), String>;
   // validate first; then (inside cx.defer): kept = cx.key_bindings() minus exactly the bindings recorded in the
   // `UserBindingSet` global at the last install (same keystrokes + predicate + action) and minus the conditional
   // static rows (ToggleSearchMode ctrl-t, list ctrl-f MoveRight); clear_key_bindings(); bind_keys(kept);
   // re-add the conditional rows per the new keys; bind_keys(user); record the new UserBindingSet;
   // menus::rebuild(cx); palette::rebuild(cx); cx.refresh_windows()
pub fn binding_labels(action: &dyn Action, context: &str, cx: &App) -> Vec<SharedString>;
pub fn help_rows() -> &'static [HelpRow];                          // §8.7
// (as built) a row lists key *pairs* of possibly different actions, a condition and the windows it applies to:
pub struct HelpKey { pub action: fn() -> Box<dyn Action>, pub keystroke: Option<&'static str> } // None = every live binding
pub enum HelpWhen { Always, Bound(fn() -> Box<dyn Action>) }      // Ctrl+T / Ctrl+S rows only while ToggleSearchMode is bound
pub enum HelpWindows { All, MainOnly, SingleFileOnly }
pub struct HelpRow { pub section: HelpSection, pub keys: &'static [HelpKey], pub context: &'static str,
                     pub label: &'static str, pub single_file_label: Option<&'static str>,
                     pub fallback_keys: &'static str, pub when: HelpWhen, pub windows: HelpWindows }
pub fn help_keys_for(row: &HelpRow, single_file: bool, cx: &App) -> SharedString;
pub fn help_row_visible(row: &HelpRow, single_file: bool, cx: &App) -> bool;
// validation (as built): sanitize_keys(&KeyBindings) -> (KeyBindings, KeyValidation) keeps the default for a
// rejected/reserved key (debug-log lines at startup); validate_keys(&KeysConfig), validate_bindings(&KeyBindings);
// install_user_bindings validates again on live rebinding; conditional_bindings(keys) = the ctrl-t / ctrl-f rows.
```
Static rows such as `f5 → Refresh` are never removed by a rebinding (C-034). Never pass user strings to
`KeyBinding::new` (panics); `KeyChord::gpui_keystroke()` already produces GPUI syntax.

### 8.7 Hints and Shortcuts dialog from the live keymap
* Footer/status hints use `Kbd::binding_for_action(&A, Some(ctx), window)` and render nothing when `None`. Hints
  are shown only for the region that has focus, after state (counts, activity), in priority order; chips that do
  not fit the measured width are dropped from the lowest priority up; `F1 help` always stays.
  * List footer (priority order): `⏎ open`, Resume `resume`, Fork `fork`, Rename `rename`, Delete `delete` (these
    five dimmed while loading), `Tab All|Prj` (only with a workspace; active value accent/bold), `Ctrl+T lex|sem`
    (only when ToggleSearchMode is bound; active value accent/bold), `F1 help`; picker mode puts `Esc cancel` first.
    Semantic activity or the refresh indicator replaces the hints (TUI parity).
  * Viewer status (priority order, search inactive): `/ search`, `e export`, `y yank`, Resume `resume`, Fork `fork`,
    Delete `del`, `q back` (single-file: `q close`, Resume/Fork/Delete omitted), `F1 help`. Search active with
    matches: `n next`, `N prev`, `<i>/<n>`, `Esc clear`.
* The Shortcuts dialog lists `help_rows()` grouped by section; keys = `binding_labels(...)` joined with ` / `,
  falling back to `fallback_keys` when no binding is found (typing keys handled by Input). Row labels are the TUI
  help texts:
  * **Conversation list** (TUI order): `↑ / ↓` Move selection; `← / →` Move cursor; `Ctrl+P / N` Move selection;
    `Ctrl+D` Half page down; `Ctrl+U` Kill to start of line; `Ctrl+K` Kill to end of line; `PgUp / PgDn` Jump by page;
    `Home / End` Jump to first/last; `Tab` Toggle scope (All/Project); `Ctrl+T` Toggle semantic search (only if
    bound); `Ctrl+S` Semantic details (only if Ctrl+T bound); `Enter` Open viewer; `Ctrl+O` Select and exit;
    `Ctrl+W` Delete word; Resume; Fork resume; Rename; Delete; Refresh conversations; `Esc` Clear search (TUI: Quit —
    PD6); plus GUI rows `Ctrl+E` End of line, `Alt+B / Alt+F` Word left/right, `Shift+F10` Row menu.
  * **Viewer** (TUI order): `j / ↓` Scroll down; `k / ↑` Scroll up; `J / ]` Next message; `K / [` Previous message;
    `d / Ctrl+D` Half page down; `u / Ctrl+U` Half page up; `g / Home` Jump to top; `G / End` Jump to bottom; `/`
    Search; `n / N` Next / prev match; `t` Cycle tools: off/trunc/full (TUI text kept); `T` Toggle thinking; `i`
    Toggle timing; `e` Export to file; `y` Copy to clipboard / message; `p` Show file path; `Y` Copy path; `I` Copy
    session ID; Resume; Fork resume; Delete; `q / Esc` Back to list (single-file: `Close`); plus GUI rows `x`
    Expand/collapse tool output, `o` Outline, `Ctrl+C` Copy selection.
  * **Agent panel**, **Deep search**, **Global** (Quit, Command palette, Settings, Keyboard shortcuts, Open JSONL
    file, Choose workspace, Focus conversations / deep search / viewer, Next/previous region (F6), Focus viewer mode,
    Refresh, Debug log), **Dialogs** (confirm `Y/N`, export `1-4`, `j/k`).

### 8.8 Conflicts and how they are resolved

| Conflict | Resolution |
|---|---|
| `ctrl-x` Delete vs Input Cut (consumed even with no selection) | `ConversationList > Input` duplicate (verified GK-INFRA §8.4). In the viewer find input, Cut wins. |
| `ctrl-f` Fork vs Input Search | No conflict for a non-searchable Input (spike-list); Fork duplicate registered; when Fork is rebound, `ctrl-f` = MoveRight in the list search. |
| `home`/`end` rows vs Input caret | `ConversationList > Input` duplicates; caret via `ctrl-home/end`. |
| `enter` Open vs Input PressEnter (emits and propagates) | bind `enter` → OpenConversation; ignore `PressEnter` in the list. |
| `enter` vs Dialog `enter → Confirm` in destructive confirms | `Confirm` content context binds `enter` → `ConfirmEnterIgnored` (deeper). |
| `tab` ToggleScope vs Root focus cycling | `ConversationList` deeper → wins; without a workspace the handler propagates. F6 always moves regions. |
| `escape` | Input propagates; ListPane ClearQuery; Viewer find uses deeper `Viewer > Input`; AgentPanel own context. |
| `ctrl-c` (TUI quit) | SmartCopy / Input Copy; never quit (PD6). |
| `ctrl-w` (TUI delete word) vs close window | `ctrl-w` = delete word in every Input (Win/Linux); CloseWindow only in `SingleFileWindow`. |
| `ctrl-a` (TUI cursor home) | Select all on Windows/Linux (AD2); macOS keeps Input's emacs binding. |
| `?` typed in the list query | `?` not bound in `ConversationList` → typed (LIST quirk 1 fix); help = F1. |
| Plain letters / arrows in viewer vs find input | `Viewer && !Input`. |
| Viewer keys vs agent panel focus | AgentPanel is a sibling context; viewer bindings never see its focus (F21). |
| `ctrl-d`/`ctrl-u` list vs viewer | different contexts; in the list ctrl-u is kill-to-start (TUI). |
| `ctrl-t` user binding | ToggleSearchMode not registered (TUI rule). |
| User binding = static key | user binding added later → wins (TUI: configurable keys checked first); Settings warns. |
| macOS `cmd-shift-f` (Input Replace) vs FocusDeepSearch | `Workspace > Input` duplicate registered after gpui-kit → wins. |
| `secondary-shift-*` AD17 shortcuts | unused by Input/Root on all platforms (checked against gpui-base input bindings). |
| macOS `cmd-` vs `ctrl-` | Configurable keys stay literal `ctrl`/`alt` (config syntax has no cmd); app shortcuts use `secondary`. |
| Dialog open | dialogs render outside the Workspace subtree; Workspace/List/Viewer bindings do not apply while a dialog has focus, except the context-free Quit and `alt-f4`. |

### 8.9 Command palette (palette.rs, WP6)
`palette::open(window, cx)` stores the focused element, opens a dialog with `Command::new(&state)` (searchable,
filterable), built from `menus::menu_tree(cx)` (same items, groups = top-level menus, keywords = the menu path).
Each `CommandItem` gets `.action(boxed)`, `.checked(..)` and `.disabled(!window.is_action_available(&*action, cx))`
evaluated against the stored focus, and its live shortcut as a right-aligned `Kbd`. On confirm the palette closes,
restores the stored focus, and dispatches the action there (so the pane resolves its target). Rebuilt on open (cheap)
— never cached across keymap/theme changes.

### 8.10 Menus (menus.rs, WP6): one tree for every window
`menus::menu_tree(cx) -> Vec<Menu>` is the single source for `cx.set_menus` (macOS), `GlobalState::set_app_menus`
(AppMenuBar) and the palette. There are no per-window variants: an item is enabled when its action has a handler on
the active window's dispatch path (macOS validates natively). gpui-component's `PopupMenu` greys an item only by its
`disabled` flag and never asks the dispatch tree, so on Windows/Linux `menus::sync_availability` (called by every
window's title bar while it renders; skipped while a menu is open or the window is inactive; a window's first
active render defers it to the next frame, when a rendered dispatch tree exists) evaluates
`window.is_action_available` for every command of the tree and, when the set changed, rebuilds the owned tree with
the `disabled` flags set (S-033). The
tree is rebuilt — and every open AppMenuBar `reload()`ed — after theme changes, keymap changes and window
activation (check marks reflect the active window's viewer, tracked in `ActivePane`).
* **File**: Open JSONL file…, Show conversations, Choose workspace…, Clear workspace, Export…, Close window, Quit.
* **Edit**: Copy, Find in conversation, Find in all conversations….
* **View**: Conversations, Deep search, Viewer, Focus viewer, Tool display ▸ Summary/Truncated/Full, Show thinking,
  Show timestamps, Search mode ▸ Lexical/Semantic, Scope ▸ All/Workspace, Theme ▸ System/Light/Dark, Refresh.
* **Session**: Open, Resume, Fork, Rename…, Delete…, Export…, Copy to clipboard ▸ 4 formats, Copy message, Copy path,
  Copy session ID, Copy header session ID, Show file path, Reveal in file manager, Search within conversation…,
  Outline, Read range…, Semantic result details, Print and exit (picker: Select and exit).
* **Tools**: Delete empty transcripts…, Generate semantic cache…, Clear semantic cache…, Debug search…, Semantic debug
  search…, Debug log…, Show projects directory, Delete conversation by session ID…, Open JSONL file…, Check for
  updates…, Settings….
* **Help**: Command palette…, Keyboard shortcuts, Check for updates…, About Claude History GUI; macOS app menu.

---

## 9. Viewer rendering pipeline (PD7, spike-viewer "B-first hybrid")

### 9.1 Overview
```
open(path) ──(viewer-worker) ViewerModel::open(path, prefs)      core: parse → Arc<Vec<RenderableEntry>>
           ──(viewer-worker) model.document()                     core: build_document(entries, prefs, expanded)
           ──(viewer-worker) items::build(&doc, &model, cols)     gui: ledger blocks → items, big blocks → ≈100-line chunks
           ──(viewer-worker) segments for every item              gui: markdown → Vec<Segment> (flags, not colors), tabs expanded
           ──(ui) install(gen): ListState::reset_with_uniform_height / splice-diff, anchor; old document dropped
           ──(ui, per frame, visible rows only) render.rs: lanes + SelText(StyledText(text, highlights(flags × theme, code tokens, matches)))
```
The `viewer-worker` (one std thread per ViewerPane, §7.5) has a single latest-request slot: a new open/rebuild
request replaces a queued one, bumps an `AtomicU64` generation, and the running build checks the generation between
entries and between items and abandons stale work. Arrowing through the list therefore never queues full parses.
`cols` = the content column width in mono cells: `floor(content_px / advance("0", mono, text_xs))`, recomputed
(debounced 100 ms) on width/rem change, which triggers an anchored rebuild (§9.6). `core::view::ledger` rendering is
not used; the ledger-equivalent counts come from core's `ledger_*` helpers.

### 9.2 ViewerItem (items.rs)
```rust
pub struct ViewerItem {
    pub id: ItemId,                    // stable across rebuilds: (entry_index, block_ordinal, detail_ix, chunk_ix) or (ToolOutputId, chunk_ix)
    pub version: u64,                  // hash of rendered content (changes when expanded/truncated text changes)
    pub nav_position: Option<usize>,   // ConversationDocument::nav_position of its message (None: Pi metadata etc.)
    pub entry_index: usize,
    pub first_of_message: bool,
    pub first_chunk: bool,             // lanes, header and chevron are drawn on the first chunk of a block only
    pub lanes: Lanes,
    pub body: ItemBody,
}
pub struct Lanes { pub timestamp: Option<SharedString>, pub label: Option<SharedString>, pub role: LabelRole, pub bold: bool, pub dimmed: bool }
pub enum ItemBody {
    Markdown { segments: Arc<Vec<Segment>>, thinking: bool },
    ToolSummary { sentence: SharedString, id: Option<ToolOutputId>, expanded: bool },
    ToolCall { id: ToolOutputId, header: Option<SharedString>, lines: Arc<Segment> /* SegKind::ToolBody, this chunk */,
               more: Option<usize>, expandable: bool, expanded: bool },
    ToolResult { id: ToolOutputId, segments: Arc<Vec<Segment>>, more: Option<usize>, expandable: bool, expanded: bool },
    SubagentToolResult { id: ToolOutputId, text: Arc<Segment>, more: Option<usize>, expandable: bool, expanded: bool },
}
pub fn build(doc: &ConversationDocument, model: &ViewerModel, cols: usize, cancel: &dyn Fn() -> bool) -> Option<Vec<ViewerItem>>;
```
* **Chunking**: a code block, tool body, fenced tool result or plain subagent result longer than 120 lines is split
  into items of 100 lines (`chunk_ix` 0, 1, …); a markdown block longer than 150 rendered lines is split at segment
  boundaries (a single over-long segment is split by lines). Search, reveal and selection map offsets to
  `(item, segment, byte)` across chunks.
* **Truncation** (Truncated mode, `Fold::Collapsed`) counts ledger rows at `cols`, never logical lines:
  * tool result: `more = result.ledger_hidden_lines(cols)`, shown text = `result.ledger_visible_rows(cols)` (K9),
    rendered as mono plain rows; expanded or Full mode renders the full markdown/fenced text (chunked);
  * tool call: `more = call.ledger_hidden_lines(cols)`, shown lines = `call.visible_body_lines()` each clipped to
    `cols` display cells with `…` (upstream clipped long lines);
  * subagent result: `more = result.hidden_lines()`, shown lines = `visible_text()` lines clipped to `cols`.
  So a 50 KB single-line output shows at most 4 visual rows plus the clickable `(N more lines…)` row.
* **Labels/lanes** (parity with VIEW §4.1, V-015), from `DocMessage { label, label_role, bold, dimmed }` and
  `DocBlock::timing`:

| DocBlock (message kind) | label | role | bold | dimmed | timestamp lane |
|---|---|---|---|---|---|
| Markdown, top-level user | `You` | TextPrimary | yes (skill: no) | skill: yes | `DocTiming::Stamp` when timing on |
| Markdown, assistant (label repeated per text block) | agent or `Claude`/`Pi`/`OMP` | Accent | yes | no | first block only (core `DocTiming`) |
| Markdown, subagent / agent progress | `↳…` (core label) | TextPrimary / Accent | no | yes | Pad |
| Markdown, Pi metadata | core label | TextSecondary | no | yes | Pad |
| Thinking | `Thinking` | AccentDim | no | no (italic + thinking role) | first block only |
| ToolSummary / ToolCall | message label | AccentDim | no | message dimmed | first block only |
| ToolResult (top-level) | `↳ Result` | ToolText | no | no | first block only |
| SubagentToolResult | `↳ Tool` (header `<Result>`) | AccentDim | no | yes | Pad |
| ToolSummaryDetails items | group label on the first item | AccentDim | no | yes | Pad |

  Label text is fitted with core `view::fitted_name` (9 display cells, `Branch s…`, V-014). The timing lane is
  reserved whenever timing is on (quirks 1–2 fixed); `DocTiming::Disabled` blocks show an empty lane.

### 9.3 Segments (segments.rs, gpui-free, unit-tested)
```rust
pub struct Segment { pub kind: SegKind, pub text: String, pub spans: Vec<(Range<usize>, Style)>,
                     pub code_lang: Option<String>, pub link_urls: Vec<(Range<usize>, String)> }
pub enum SegKind { Para, Heading(u8), Item { depth: u8, bullet: String, ordered: bool }, Quote, Code,
                   TableRow { cells: Vec<String>, widths: Arc<Vec<usize>>, last: bool }, Rule, ToolBody, Plain }
bitflags-like: Style { bold, italic, code, heading, link, quote, dimmed, strike, diff_add, diff_remove, thinking }
pub fn markdown(text: &str) -> Vec<Segment>;
pub fn code_block(text: &str, lang: &str) -> Segment;          // fenced ```text for tool results
pub fn tool_body(lines: &[ToolBodyLine], clip_cols: Option<usize>) -> Segment;  // diff_add / diff_remove / dimmed per line
pub fn plain(text: &str, dimmed: bool, clip_cols: Option<usize>) -> Segment;    // subagent results (pretty JSON)
pub struct SearchText { pub lower: String, pub origin: Option<Vec<u32>> }       // built lazily on the first search
pub fn search_text(seg: &Segment) -> SearchText;   // ASCII fast path: lower = to_ascii_lowercase, origin = None;
                                                   // origin only when lowercasing changes byte lengths (as core DocSearchIndex)
```
Rules (parity with `markdown/layout.rs` + TUI sink, V-033…V-046, V-099, V-100):
* pulldown-cmark 0.13 with only `ENABLE_STRIKETHROUGH | ENABLE_TABLES`.
* **Tabs** are expanded to 8-column stops, display-width aware, with core `markdown::expand_tabs(text, start_col,
  TAB_WIDTH)` (K10) in every text event: paragraphs, list items, code blocks, fenced tool results, tool bodies,
  plain results (GPUI `StyledText` does not lay out tab stops).
* Soft and hard breaks both become `\n` inside the segment (no reflow of single newlines).
* Paragraphs, headings, list items, quotes, code blocks, table rows, rules are separate segments; the renderer adds
  vertical gaps: blank-line equivalents before headings, code blocks, quotes, rules, tables and top-level lists, none
  between list items, none before the first paragraph of a list item or inside a quote.
* Headings: text only (marker hidden), style heading+bold. Lists: bullet `- ` (unordered) or `N. ` (ordered, dimmed,
  honoring the start number); nested depth indents by 2 bullet widths; hanging indent.
* Emphasis → italic; strong → bold; strikethrough → dimmed (TUI shows strikethrough as muted).
* Inline code → `code` style + mono font override. Links → `link` style; URL in `link_urls` (tooltip/click AD13).
  Autolinks/bare URLs are plain text (parity). Blockquote → `quote` style with a `> ` prefix run (dimmed).
* HTML (block/inline) → literal text. Rule → `Rule` segment rendered as a 40ch hairline.
* Code blocks → `Code` segment, fence hidden, language normalized by `core::syntax`; blank lines preserved.
* Tables → one `TableRow` segment per row: plain cell text (emphasis dropped, inline code without backticks,
  newlines → spaces), shared column widths in display cells; rendered in the mono font with cells padded to the
  column widths and separated by two spaces, a separator line under every row (parity: separators between all
  rows), no header bold, horizontally scrollable when wider than the content column. One SelText per row.
* Thinking blocks → every span italic + `thinking` role (overrides syntax colors). Dimmed blocks → every span
  `dimmed` (overrides all fg incl. syntax, V-016).
* Whole-message plain text for copy/search = segment texts joined with `\n`.

### 9.4 Item renderer (render.rs)
```
h_flex().w_full().items_start()                       // one ledger row per item
 ├─ focus mark  w(px(2)) h_full bg(accent 60%) + the whole item bg(list_active tint) when its message is focused and nav is active (PD7 flag)
 ├─ timestamp   w_24 mono text_xs, text_secondary role, right padding — only when show_timing
 ├─ label       w_20 mono text_xs text_right, role color, bold/dimmed — label on the first chunk only
 ├─ separator   border_l_1 border role (dimmed rows: 50% opacity), px_2
 └─ content     v_flex flex_1 min_w_0 gap per 9.3
       ├─ Markdown: one SelText per segment (Code: bg(muted) rounded p_2 mono; TableRow: mono row + bottom border)
       ├─ ToolSummary: SelText sentence (tool_text, dimmed); chevron ▸/▾ button when clickable
       ├─ ToolCall: header row on the first chunk (tool_text; chevron if expandable) + empty spacer + body chunk (mono)
       │            + "(N more lines…)" row on the last chunk (dimmed, cursor pointer, toggles) when truncated
       ├─ ToolResult: rows/segments + indicator row as above
       └─ SubagentToolResult: "<Result>" header on the first chunk + mono plain text + indicator
```
* Typography: content `text_sm` UI font; code/tool bodies/table rows `mono_font_family` `text_xs`; everything wraps
  (V-018 fix: no clipping) except collapsed tool lines, which are clipped to `cols` by design.
* Hover: items of an expandable tool output get `hover(|s| s.bg(hover role))` (V-017).
* Spacing between messages: `pt_3` on the first item of each message; `pt_1` between items of a message; chunk
  items of the same block have no gap.
* Search highlights: `HighlightStyle { background_color: match role, color: on-match role }`; current match
  `background_color: current-match role (warning token), color: on-current role` (V-075). Style runs are built by a
  deterministic overlay (base spans, then code tokens, then match ranges on top; spike-list §6).
* Colors: `theme_tokens::roles()` (§5 intro).

### 9.5 SelText, selection and copy (seltext.rs)
Port `spikes/viewer/src/seltext.rs`: a custom `Element` wrapping `StyledText` (highlights + mono overrides) that
registers with gpui-base `TextSelection` (`TextSelectionHandle`, `TextSelectionRegistration`, `TextSelectionRun`)
and can report the y position of a byte index after paint (`reveal`). One handle per rendered segment (one per table
row), taken from the handle LRU (§9.7).
* Copy = SmartCopy (`ctrl-c`/`cmd-c`, Edit → Copy): if the focused Input has a non-empty selection, that text; else
  `TextSelection::selected_text(window, cx)`; written through `services.clipboard_write`. ViewerPane records which
  items were painted in the last frame; when the selection's first or last participating item is no longer
  painted, the copy is partial and a toast says `Copied the visible part of the selection. Use Y, y or Export to
  copy more.`; an empty result with an active selection says `Nothing copied: the selection scrolled out of view.`
* Link ranges: hit-test on mouse move → tooltip with URL; click on a link range → "Open link?" dialog.

### 9.6 Container, scrolling, anchors
* `gpui_kit::ListState::new(n_items, ListAlignment::Top, px(600.)).with_uniform_item_height(rem * 3.0)` (overdraw is
  a measured runtime geometry exception); `gpui_kit::list(state, closure)` inside
  `div().relative().flex_1().min_h_0()` + `Scrollbar::vertical(&list_state)`. Never `measure_all()`.
* The list closure receives `&mut App`: it captures an `Rc<RenderCtx>` snapshot (items `Arc`, nav focus, matches per
  item, theme roles, hover id) and a `WeakEntity<ViewerPane>` for clicks.
* Scrolling: line = `rem * 1.5`; `ScrollLineDown/Up` → `scroll_by(±line)`; half page = `viewport_bounds().size.height
  / 2`; page = viewport height; top/bottom = `scroll_to(ListOffset{0,0})` / `scroll_to_end()`. Mouse wheel native.
* Scroll indicator `[i/N]`: N = navigable message count; i = 1 + the nav position of the message that owns the item at
  `logical_scroll_top()` (the first navigable message at or after it when that item has none); `[1/1]` when empty.
  At the bottom, i is the message at the top of the last viewport, so `[k/N]` with k ≤ N (V-009).
* Re-render with anchor (toggles t/T/i, expand/collapse, theme switch, `cols` change): capture `top =
  logical_scroll_top()`, the anchor item id (the focused message's first item if nav is active, else
  `items[top.item_ix].id`) and the anchor entry via core `capture_anchor_entry(nav_entries, focused, nav_active,
  first_at_or_below_top)`; rebuild in the worker; diff the old and new `(id, version)` lists into `ListState::splice`
  calls; resolve the anchor id in the new list (exact; else core `find_entry_or_prev` → that message's first item,
  else 0) and `scroll_to(ListOffset { item_ix, offset_in_item: same ? top.offset_in_item : 0 })`; then
  `MessageNav::refocus(resolved)` (V-081, F9). `remeasure()` alone is never used for width changes (it re-anchors
  proportionally).
* Opening a different conversation: `ListState::reset_with_uniform_height(n, rem * 3.0)` (keeps the size hint for a
  correct scrollbar) and scroll to top; expanded set cleared (V-060); search cleared; nav = `MessageNav::new(count)`.

### 9.7 Caches (lru.rs)
* Selection handles: `Lru<(ItemId, u16), TextSelectionHandle>` capped by **handle count** (1,500), evicting handles of
  the items farthest from `logical_scroll_top` after each scroll; a table's rows count one handle each.
* Syntect tokens: `Lru<(ItemId, u16 seg, ThemeKind), Arc<Vec<(Range<usize>, HighlightStyle)>>>`, 2,000 segments;
  computed on the background executor for visible code segments (first render shows plain code color, then
  re-renders when ready; syntect warmed at startup).
* Search texts: built lazily per segment on the first search after an open/rebuild and kept in a side table with the
  items; dropped on open. No per-segment lowercase copy exists before the user searches.
* All caches cleared on theme change (ThemeKind is part of the key anyway) and on open; the previous document and
  items are dropped as soon as the new ones are installed.

### 9.8 In-viewer search (search.rs)
* State machine = core `view::ViewSearch<SegMatch>` (`start`, `set_query`, `commit`, `next_match`, `prev_match`,
  `refresh`, `input_status`, `match_counter`) and `view::escape`. `SegMatch { item_ix, segment_ix, range, hidden:
  Option<ToolOutputId> }`.
* `/` → find bar visible (`Input` small, `cleanable`), focus it; `ViewSearch::start` (V-072).
* Every query change: `set_query(q, find)` where `find` = case-insensitive search over the items' lazy search texts
  (visible text) plus, for collapsed tool outputs, over their full text taken from the model on demand
  (`DocToolResult::text`, the full body lines, the subagent text) → `hidden: Some(id)`. Occurrences are counted
  (AD7). The first match is revealed and its message focused (`MessageNav::focus`, V-073).
* Reveal: if `hidden` → toggle that output expanded (anchored rebuild, then `refresh` recomputes matches) — PD7
  auto-expand; then `scroll_to(ListOffset{item_ix, 0})`; on the next frame SelText reports the match y;
  `scroll_by(y - viewport_top - 3 * line)` so the match sits a few lines from the top (exact, two frames). Only
  scroll if the match is outside the viewport (n/N parity).
* Enter → `commit` (Active if matches else Off); focus returns to `viewer_focus`. Esc in the find input → `escape`.
* `n`/`N` (or F3/Shift+F3) in Active: `next_match`/`prev_match` (wrap), reveal, focus message.
* Rebuilds keep Active matches: `refresh` (V-076).
* Find bar text: `/` + query + `input_status()`; status bar in Active shows `n next  N prev  <i>/<n>  Esc clear`.
* Throughput target: ≤ 10 ms for 2,000 matches on 3k messages; first search on a 10k-message transcript ≤ 60 ms
  including the lazy index build.

### 9.9 Message navigation (nav.rs)
* State = core `view::MessageNav`; positions = `ConversationDocument::navigable_indices()` (V-071).
* `J`/`]` and `K`/`[`: `focus_next(count, first_visible)` / `focus_prev(..)` where `first_visible` = nav position of
  the message owning the first item overlapping the viewport (quirk 11 kept by core); ensure visible: if the focused
  message's first item is above/below the viewport (`item_is_above_viewport`/`below`) → `scroll_to(ListOffset{first_item, 0})` (V-067).
* After any scroll while nav is active: `sync_after_scroll(first_visible)` (V-068).
* Clicking inside a message's item → `focus(position)` (V-059); clicking an expandable control also toggles it.
* Focus mark on all items of the focused message while active (`highlighted()`, V-069). Esc with nav active →
  `escape` deactivates (V-070).
* `y` with nav active → `copy_target(first_visible)` → `ops::copy::copy_message(target, model.message_text(entry))`;
  quirk 5 kept (summary group → `No text content in this message`).
* `reveal(RevealRequest)`: `AgentOrdinal { ordinal, hint }` → apply the hint to this view only (tools/tool_results →
  at least Truncated; thinking/subagents → show_thinking on; not persisted as defaults), map `ordinal → entry_index`
  with `ViewerModel::entry_for_agent_ordinal` (K5), find the nav position with that entry (or the next one),
  `MessageNav::focus`, scroll it to the top. Not found → toast `Message m<N> is not shown in the viewer`.
  `EntryIndex(i)` works the same without the hint.

### 9.10 Header, toolbar, status bar
* Header (header.rs) from `HeaderInfo` (model.rs), TUI order on one wrapping line: project (`Unknown` fallback,
  accent bold) · custom title (only if set and non-empty) · model (`format_model_name`, only if known) · `1 message`
  / `N messages` (`format_msg_count_long`) · duration (`format_duration_minutes`, only if known) · tokens (**only if
  `total_tokens > 0`**: `format_tokens_long` `926k tokens` if the line has room, else `format_tokens` `926k`) ·
  timestamp (`format_header_timestamp`); summary (only if set) on the same line when it fits, else on a second line
  (header_summary role). Conversation not in the corpus → file stem only (V-006). `parse_error_count > 0` → warning
  badge `⚠ N` with tooltip `1 malformed line` / `N malformed lines` that opens Tools → Debug log → Parse errors (V-008). Header is bound to
  the live corpus (V-007): Workspace pushes `set_header` on `CorpusChanged`.
* Toolbar (toolbar.rs): segmented `Summary | Truncated | Full` (SetTools*), toggles `Thinking`, `Timing`
  (`.selected(bool)`), spacer, find bar (when active), `Outline`, `Resume`, `Fork`, `⋯` dropdown (Export…, Copy to
  clipboard ▸, Copy message, Copy path, Copy session ID, Copy header session ID, Show file path, Reveal in file
  manager, Rename…, Search within conversation…, Read range…, Delete…). Single-file: no Resume/Fork/Rename/Delete;
  unknown-source targets: those items disabled with tooltip `Unknown conversation type`.
* Status bar (status.rs): `[i/N]` (mono), `tools·sum|trn|all  think·on|off  info·on|off` (`ToolDisplayMode::
  status_label`; key letter accent, rest muted — clickable), `│`, hints (§8.7, focus-dependent, priority-trimmed).

---

## 10. List rendering details

### 10.1 Row model and anatomy (rows.rs, AD20)
```rust
pub struct RowModel {                 // built per visible row each frame (cheap; no Vec clones)
    pub id: SharedString,             // path string (domain id; QA id "row:<file stem>")
    pub source_label: Option<&'static str>,      // "CC" | "Pi" | "OMP" when Browser::has_multiple_sources()
    pub header: RowHeader,                       // core display_format::build_row_header(width = usize::MAX / 4)
    pub preview: RowPreview,                     // core display_format::build_row_preview(max_width = preview chars)
}
```
```
ListItem::new(id).selected(is_sel).h(row_h).px_3().py_1()        row_h = rems(4.25); literal mode rems(5.5)
 ├─ line 1 (text_sm): [source lane w_10 mono text_xs muted] TITLE ············ time (mono text_xs, recency role)
 │      TITLE = custom title (custom_title role) → else summary (summary role) → else project (text_primary role);
 │      font_semibold when selected; highlight ranges of that field; truncated with …
 ├─ line 2 (text_xs): project (if not the title) · "42 msgs" (msg_count role) · duration (duration role) ·
 │      score "0.87" (accent; Semantic mode with metadata) · summary (when line 1 shows the custom title); truncated with …
 ├─ line 3 (text_xs, preview role): preview with highlights (accent)
 └─ line 4 (literal mode only, text_xs, context_base role): context snippets with context highlight
border_b_1 (separator role) between rows
```
* All header fields keep their per-field highlight ranges from `RowHeader` (project accent+bold, custom title,
  summary). TUI budget rules (title hidden under ~4 chars, summary under ~5, reserve clamp(left/3,10,24) on narrow
  rows, score only on rows ≥ 70 chars) do not apply: every field is present and the layout truncates with `…`
  **[dev, AD20]**; the score is hidden only when the row is narrower than 22rem.
* Tooltip on the row: full `project · title · summary` + full path. Accessibility label `"{title} · {project} ·
  {relative time}"`. Right-click → context menu (§5.2).

### 10.2 Preview text (L-024, L-025)
`max_chars` = floor(line width ÷ average char width of `text_xs`) − 2 (cap 400). Text, source and highlights come
from `build_row_preview` (`PreviewSource` = semantic evidence / lexical context / sanitized preview). The
`HighlightQuery` is parsed once per query change and stored on `ListPane`.

### 10.3 Literal mode (L-012)
`literal_mode = query_has_literals(query) || !HighlightQuery::parse(query).literal_context_specs().is_empty()`
(quoted literals **or** unquoted `_`-identifiers, which core treats as literal filters; fixes quirk 10; WP1's P test
asserts `a_b` → true and raises a CCR if `literal_context_specs` does not cover it). All rows use
`row_h = rems(5.5)` while it is on; rows without a context line render an empty fourth line.

### 10.4 Search input status (L-032)
`Browser::search_status_text()` (`Loading… <n>` after the `…` substitution of §12, `<i>/<n>`, `0/<n>`, `sem …`,
` sem failed` with the full error in a tooltip (`semantic_error()`), ` sem no text`, ` sem paused`), plus the spinner
while `has_search_work_in_flight()`.

### 10.5 Selection visibility, paging, row bounds
Selection changes call `scroll_handle.scroll_to_item(ix, ScrollStrategy::Nearest)`. Visible row count = `floor(viewport
height / row_h)` (from `UniformListScrollHandle` last item size and viewport, spike-list §5); PageUp/PageDown =
±visible rows (`select` by position), Ctrl+D = `select_half_page_down(visible)`. Row bounds of painted rows are
recorded each frame (for the keyboard context menu position).

---

## 11. Data flows

### 11.1 Startup
1. `main` (WP7): read `std::env::args_os()` and `std::env::current_dir()` (launch cwd) once; `HistoryEnv::from_process_env()`
   with `env_path_base = launch cwd` (K16); parse with `core::settings::parse_launch_args`.
   * `Help` / `Version` → `console::write_stdout(help_text("claude-history-gui"))` / `claude-history-gui <ver>`,
     exit 0. `LaunchArgsError` → stderr `error: …` + usage hint, exit 2 (Windows release: AttachConsole first,
     §14.3).
   * `Agent(cli)` (K15) → headless §11.23. `--show-dir`, `--render FILE` with stdout available → headless §11.23.
   * `args.time_filter()` error → stderr `Error: Invalid time range: <e>` (AppError Display), **exit 1** (upstream
     runtime error, C-005).
   * Debug builds and `--features qa`: the sandbox gate (§15.4) runs here, before any data is read.
2. `claude_history_gui::run(launch, env)` → gpui application → `app::init` (§7.3 order):
   * config: `config.toml` and `gui.toml` loaded **independently** (`load_config_from`, `load_gui_config_from`); a
     failing file → its error in `ConfigErrors`, defaults for its part; effective = `cli.overlaid_with(&gui_file)`;
     `settings = effective_settings(&effective, &args.display_flags(), args.search_mode_override())`;
   * UiState (state.json);
   * workspace (PD3);
   * **initial view and list mode** (`startup::resolve_initial_view`, P table test C-066), per field:

| Field | 1st | 2nd | 3rd (only if `[gui] restore_session`) | default |
|---|---|---|---|---|
| tool display | `--show-tools` → Full, `--no-tools` → Summary | config `[display] no_tools` set (gui.toml > config.toml): true → Summary, false → Full | state.json `view.tool_display` | Summary |
| show thinking | `--show-thinking` / `--hide-thinking` | config `[display] show_thinking` set | state.json `view.show_thinking` | false |
| show timing | — | — | state.json `view.show_timing` | false |
| show last (previews) | `--last` / `--first` | config `[display] last` set | — | true |
| list mode | `--semantic` / `--lexical` | config `[search] mode` or `[tui] semantic_search` set (core `list_search_mode`) | state.json `search_mode` | Lexical |
| time filter | `--since/--after/--before` | — | (never restored) | none |

   * keymap, menus, palette, theme, embedder.
3. Open the window: `file` given → `single_file::open_window(path, cx)`; else `workspace::open_main_window(cx)`
   (bounds from UiState validated against `cx.displays()`; else centered 1280×820 clamped to 85% of the primary
   display; left/panel widths clamped to the window, PD1).
4. After the first frame (`cx.defer`): one configuration-error dialog per failing file (§5.20); startup tool (AD14:
   `run_startup_tool`); QA runner start (feature `qa`); `login-shell-path` probe (macOS/Linux when not started from a
   terminal, §14.2).
5. `ListPane::new` starts the initial load (§11.2) immediately.

### 11.2 Initial load (streaming)
1. `ListPane::new`: `browser = Browser::new_loading(BrowserOptions::new(env).with_show_last(settings.show_last)
   .with_default_mode(app.list_mode).with_exclude_projects(settings.exclude_projects).with_workspace(workspace)
   .with_workspace_filter(args.local && workspace.is_some()).with_waker(waker).with_embedder_factory(shared.browser_factory()))`;
   `loader = HistoryLoader::new(env, LoaderOptions::new(show_last).with_debug_level(level).with_time_filter(launch.time_filter).with_waker(waker))`;
   `loader.start_initial_load()`. `level` = `--debug` level, else the Debug log dialog's selector (default Info).
2. Waker → `_poll_task` → `poll_all(cx)`: `let p = loader.poll(&mut browser)` (appends batches; at the end the K1
   preparation runs on `corpus-prepare` and a later poll installs it) then `browser.poll()`; notify once.
3. On `p.event`:
   * `Loaded { project_errors }` → selection row 0 → `SelectionChanged`; `project_errors > 0` → warning toast
     `N project(s) could not be loaded. See Tools → Debug log.`; `CorpusChanged`, `LoadStateChanged`; project paths
     for the workspace picker recomputed in the background.
   * `NoHistory(e)` → empty view with `e` text (`No conversation history found in selected scope[ within the
     requested time range]`) + [Refresh] [Clear time filter] (if active).
   * `LoadFailed(e)` → error view `Couldn't load history` + `e` + [Retry] (`start_refresh`) [Show debug log].
4. A query typed while loading is kept in the Input and passed to `browser.set_query` (core applies it at the end).

### 11.3 Refresh (Ctrl+L, F5, toolbar, menu, palette; also after delete-empty / delete-by-ID if the viewer is unaffected)
1. `loader.start_refresh()` returns false while a load runs (no-op, parity). Else the footer shows the persistent
   indicator `Refreshing conversations…` (no start toast, PD16).
2. Core loads into a hidden buffer; the current list stays interactive; preparation off-thread (K1). Rows renamed or
   removed while the load streams or prepares are replayed onto the installed corpus by core (K1 as built, §4.3).
3. `Refreshed { old, new, .. }` → toast `Refreshed conversations: <old> → <new>` (replaces the previous refresh toast,
   `id1::<RefreshToast>`); `CorpusChanged`. `RefreshIncomplete` → warning toast `Refresh incomplete; keeping the
   current list` + action "Show debug log". `RefreshFailed(e)` → error toast `Refresh failed: <e>` + "Retry".
   `RefreshInterrupted` → error toast `Refresh failed; keeping the current list` + "Retry". Zero rows → empty state
   (no exit, ADD C2).

### 11.4 Search as you type (list)
1. `InputEvent::Change` → `query = state.value()`; replace `\n`/`\t` by spaces if present (`set_value` + caret end,
   ADD C3); `browser.set_query(query)` (never touches the disk, K2); `highlight = HighlightQuery::parse`; literal mode
   recomputed; notify. UI cost per key ≤ 2 ms on 20k rows (§15.8).
2. Results arrive through the waker → `browser.poll()` → `on_results`: selection row 0 (L-080) → `SelectionChanged`;
   footer/status; if `semantic_error()` changed → error toast with the full text (once per error) + actions "Switch
   to lexical" and "Retry".
3. Lexical/semantic semantics are core's (LIST §5–§6, CORE §5.1): generation-tagged lexical worker; semantic with
   lexical fallback, progress, prewarm; worker failure → respawn once → `semantic worker unavailable`.
4. Mode toggle (Ctrl+T / toolbar / menu): `browser.toggle_list_search_mode()`; remembered in state.json
   `search_mode` (used next launch only when no config key is set, PD17). Scope toggle (Tab / toolbar):
   `browser.toggle_workspace_filter()`.
5. Footer activity = `semantic_activity_status_text()`.

### 11.5 UUID jump (K2)
After `set_query`, `uuid_lookup()`:
* `Found { .. }` → the single row is selected and previewed (`CorpusChanged` if `loaded_from_disk`).
* `Pending { uuid }` → background `history::load_session_by_uuid(env, uuid, show_last)` → `browser.resolve_uuid_lookup(uuid, result)`.
* `NotFound { uuid }` → error toast `Session not found: <uuid>` (`UuidLookup::status_message`, LIST quirk 6 fix);
  previous rows stay.
* After a refresh or reload (K2 as built, §4.3) a UUID result stays `Found` (core re-appends the session when the new
  corpus lacks it) or `NotFound`; it is `Pending` only while the GUI's lookup is in flight. No GUI handling needed.

### 11.6 Open in viewer (preview / Enter / double-click / deep-search hit / agent reveal)
1. Workspace → `viewer.open(OpenRequest { target, focus, reveal })`.
2. ViewerPane: same path → only focus/reveal. Else post the request to the `viewer-worker` (latest-request slot,
   generation++), keep showing the previous content with a thin loading bar (or `Loading conversation…` when
   empty).
3. `install(gen, result)`: stale → drop. `Err(e)` → error view `Failed to open: <e>` with [Retry] and [Show debug
   log] + toast with the same text (L-051/A-054). Ok → replace model (old document dropped),
   `ListState::reset_with_uniform_height`, header from `list.header_info(path)` (single-file: from
   `process_conversation_file`), clear search & expanded, `MessageNav::new`; apply `reveal`; if `focus` →
   `viewer_focus.focus()` after the frame; emit `TargetChanged`.

### 11.7 Viewer toggles and expansion
`t`/`T`/`i`/SetTools*/`x`/click → mutate `ViewerModel` (`cycle_tool_display`, `toggle_thinking`, `toggle_timing`,
`toggle_expanded(id)`); update `AppState.view_defaults` (t/T/i only; remembered in state.json); anchored rebuild in
the worker (§9.6); Active search `refresh`ed. Clicking an expandable control also focuses the message.

### 11.8 Rename (F2, menus, palette, row menu; list or viewer; K3)
1. The pane handler captures `target` (§7.8); while loading, unknown source or single-file → no-op (menu item
   disabled).
2. `ops::rename::open_rename_dialog(target, window, cx)`: `InputState` prefilled with `target.custom_title` (caret at
   end, focus once); dialog §5.6 registered in `OpenTargets`.
3. Save/Enter → buttons show a spinner; background `history::rename_session(env, path, source, input, show_last)` →
   UI: `list.apply_renamed(path, conv)` (K3). Ok → close dialog; row/header update via `CorpusChanged` (no toast).
   `Err(RenameError)` → inline error (`Failed to rename: conversation became empty` / `Failed to rename: <e>`;
   dialog stays open, input kept). `NoTarget` (the conversation left the list meanwhile) → inline `This conversation
   is no longer in the list`.

### 11.9 Delete (Ctrl+X, menus, palette, row menu; list or viewer; K4)
1. Capture `target`; while loading, unknown source or single-file → no-op.
2. Background `maintenance::plan_session_delete(env, &target.path, source)`. Error (e.g. `Session not found: <id>`) →
   error toast.
3. Open the confirmation (§5.7) with the plan (files, dirs, copy count); registered in `OpenTargets`. `y`/`Y`/Delete
   button → buttons disabled; background `execute_session_delete(env, &plan)`.
4. Ok(report) → `CorpusNotice::Deleted(plan.files)` (all deleted copies leave the list, the viewer is cleared if it
   shows one, other dialogs on those paths close, §7.2); toast `Deleted "<title>"` or, for several Claude copies,
   `Deleted session <id> (<n> copies across projects)` (core `delete_by_uuid_message`). Err(e) → error toast
   `Failed to delete session <path>: <e>` (core also appends that line to debug.log; LIST quirk 7 fix). As built
   (K4): `<path>` is `plan.path`, the listed conversation (also when another copy failed); a plan that
   `plan_session_delete` could not have produced is refused with `SessionNotFound` before anything is removed.
5. Pending searches: core keeps indices consistent (F8).

### 11.10 Resume / Fork
1. Capture `target`; loading, unknown source or single-file → no-op.
2. `conv` = `list.conversation(&target.path)` (live `project_path`) or, when not in the corpus, a background
   `process_conversation_file` → `ResumeTarget`.
3. `plan = core::resume::plan_resume(&env, &conv, workspace.as_ref(), fork, &settings.resume_default_args,
   &ProgramOverrides::from_gui_settings(&config.gui))`:
   * `Err(AppError::WorkspaceRequired(msg))` → toast `msg` + action "Choose workspace…"; other errors → toast with the
     error text.
   * `plan.warnings` → warning toast(s) (e.g. `Session directory <dir> does not exist`).
   * `plan.copy = Some(copy)` → confirmation §5.8 (danger when `existing_targets` is non-empty); confirmed →
     background `plan.execute_copy()`; error → toast `Failed to copy the conversation: <e>`.
4. macOS/Linux: if the login-shell PATH probe is still running, wait for it (≤ 3 s, spinner on the toast).
5. `spec = plan.launch_spec(&env)`: `Err` → toast `Could not find '<program>' on PATH. Set its path in Settings →
   General.` + action "Open Settings". Batch-script programs with arguments `cmd /C` cannot pass → core's error text.
6. For each `launcher in terminal::launcher_candidates(&env, &options)` (K17; options = `LaunchOptions` with
   `[gui] terminal` and `$TERMINAL` read by `main`): `cmd = prepare_launcher_command(&env, &launcher, &spec, &options)?` → background `services.spawn_terminal(&cmd, 3 s)`:
   `Ok(None)` or `Ok(Some(0))` → success toast (PD4 text, with `plan.display_command()`); `Ok(Some(code≠0))` → error
   toast `Failed to launch terminal: <launcher> exited with code <code>` (no retry: it ran); `Err(spawn error)` → try
   the next candidate; none left → `Failed to launch terminal: <last error>` + action "Terminal settings".
7. Picker mode (`--show-path`, `--show-id`, `--resume`) → §11.13 step 3 (fix round, A-001).

### 11.11 Export and copy
* Export: capture target + options (`ViewerModel::export_options()` in the viewer; `settings.show_tools` /
  `show_thinking` from the list) → menu §5.9 → format → `services.prompt_save_path(dir, default_name)` → extension
  check (PD8: append, then `Replace "<name>"?` when the appended path exists) → background
  `core::export::export_to_path` → toast `Exported to <path>` + Reveal, or `ExportError` Display (`Failed to read:
  <e>` / `Failed to write: <e>`); the directory is remembered. Save dialog `Err` → PD8 fallback.
* Copy as: same options → background `generate_export` → `services.clipboard_write` → `Copied to clipboard`.
* Copy message (`y` with nav): `ViewerModel::message_text(entry)` → non-empty → clipboard + `Message copied to
  clipboard`; empty → `No text content in this message`; Err → `Message not found`.
* Copy path → `Path copied to clipboard`; copy session ID (file stem) → `Session ID copied to clipboard`; copy header
  ID → `Session ID copied to clipboard`; show path → toast with the full path and Copy/Reveal buttons.

### 11.12 Single-file
`claude-history-gui FILE`, `--render FILE` without redirection, or File → Open JSONL file…: validate (`File not
found: <p>` / `Not a file: <p>` → window opens in an error state), open `SingleFileWindow` with
`ViewerPane(SingleFile)`, header from `history::process_conversation_file(path, None, debug)` in the background
(`Ok(None)` → stem-only header and the D1 error state `<file> contains no conversation messages`; `Err` → header stem
+ error), target `ConversationTarget::from_parsed_file`. The agent panel asks the agent worker for
`conversation_key_for_path` (K8) and is disabled with the D-035 text when `None`. Esc/q → `window.remove_window()`;
the app quits when no windows remain.

### 11.13 Picker mode and Ctrl+O
1. Ctrl+O / Select (not while loading; no selection → nothing): ListPane flushes the pending preview, captures the
   target → `ops::select::select_and_exit(target, window, cx)`.
2. Picker ShowPath/ShowId → `settings::picker_output(action, conv)` → `exit::quit_with_output(line + "\n")`
   (`services.write_stdout`, flush, `cx.quit()`, exit 0). Error → stderr `Error: <e>`, exit 1. No stdout → toast
   `Nothing to print to …` and stay open.
3. Picker Resume → §11.10 steps 2–5 (copy confirmation still required); then Unix + TTY on stdin and stdout →
   `services.exec_in_current_terminal(&spec)` after closing windows; else step 6 then quit 0. The same path serves
   Resume (Ctrl+R) and Fork resume (Ctrl+F) in every picker mode (`--show-path`, `--show-id`, `--resume`; fix round,
   A-001): upstream's picker returns after a resume, so the process quits with no stdout after a successful launch,
   and a failed launch keeps the picker open. Under `--resume` the fork flag is the key pressed (Ctrl+R / Ctrl+F),
   not `--fork-session`.
4. No picker → PD5 "Print and exit": `services.stdout_kind()`: Redirected → print; Terminal → confirmation first;
   None → toast. Text = `generate_export(Ledger, settings options)` or `plain_display(path, options)` with
   `--plain`/`[display] plain`; `--debug` diagnostics in the PD5 order; then quit 0.
5. Picker + Esc with an empty query, Quit, or closing the main window → `exit::quit_without_output` (exit 0).

### 11.14 Deep Search and the agent panel
1. Run (Enter / Search): build `AgentSearchArgs` from the form: `query` trimmed non-empty, `top`, `budget` /
   `no_budget`, `flat`, `hits_per_conv`, `all_hits`; scope **Default → neither `local` nor `all`** (core then uses
   `[agent] scope`), All → `all`, Workspace → `local`; mode Default → no override; `time` from the shared time widget;
   `args.validate()` errors inline. The form initializes from the effective `[agent]` config and shows the resolved
   defaults ("Default (Workspace)", "Default (hybrid)").
2. `agent_worker.submit(AgentJob { kind: Search, request, env: env_with_workspace, config: effective, cancel })`:
   a running Search job is cancelled (K7) and replaced; the worker builds
   `AgentService::new(env, config).with_embedder_factory(shared.agent_factory()).with_cancellation(token)
   .with_notice_sink(progress → channel)` → `respond(AgentRequest::Search(args))`. While the semantic gate is
   Exclusive, semantic/hybrid jobs fail fast with `Semantic search is paused while the cache is generated`.
3. Progress notices → status line (`Embedding 12/30`). `UncachedPassages { count, bounded_top_up: false }` → banner
   `<count> passage(s) are not cached. Generate the semantic cache to include them.` + [Generate semantic cache…]
   (the exact upstream line stays in the agent text view's stderr part).
4. Reply → `AgentResponse`: success → render `AgentResult::Search` (groups/hits from `output`, conversations from
   `conversations`), warnings banner, effective-mode hint (quoted-only → `Exact match forced`), stats; failure →
   `error: Some(AgentError)` → alert with kind + detail (ambiguous-ref candidates as clickable full refs) or the
   `error_text()`; cancelled → `Search cancelled`. "Show agent text output" shows `text()` + notices.
5. Open hit → `DeepSearchEvent::OpenHit { target: from_agent_info(conversation(hit.conversation_ref)), reveal:
   RevealRequest { path, full_ref, AgentOrdinal { ordinal: focus.start, hint: from options } } }`.
6. Agent panel (Outline / Within / Read): every result set stores `{ target, full_ref, generation }` captured when it
   ran. The viewer's conversation ref = `conversation_key_for_path(env, config, viewer path)` (K8, on the agent
   worker) → `key.conversation_ref().full_ref()`; `None` → panel disabled (D-035).
   * Outline: `AgentOutlineArgs { conversation: full_ref, output: flags }`; re-runs automatically when the viewer
     target changes while the Outline tab is visible.
   * Within: `AgentWithinArgs { conversation: viewer full_ref, query, top, budget/no_budget, search_mode }`.
   * Read: `AgentReadArgs { refs: parsed from the Refs field (split on whitespace/commas; default = viewer full ref),
     anchor, focus, lines | match_query + context, output }`.
   * Visibility toggles: `AgentOutputFlags` flags OR the `[agent] tools/tool_results/thinking/subagents` booleans
     (core ORs them); config-true flags render on and disabled with tooltip `Enabled by [agent] <key> in config`.
   * On `TargetChanged`, Within/Read sets for another conversation show the stale banner; their rows still reveal in
     **their own** conversation (`AgentPanelEvent::Reveal { target: set.target, … }` → Workspace opens it first,
     §7.2). Never `viewer.reveal` on a different conversation.

### 11.15 Tool deletions (delete-empty, delete by session ID)
Both broadcast `CorpusNotice::Deleted(paths)` with every removed transcript (from `DeleteEmptyOutcome::deleted` /
the K4 plan) — the same route as §11.9 — so the list drops them, the viewer is cleared if it shows one, and dialogs
and agent result sets holding them close. Then a background refresh runs (Pi/OMP untouched; Claude counts updated).

### 11.16 Semantic cache generation / clear (semantic gate, §7.6)
* Generate: dialog → `gate.enter_exclusive(Generate)` (list worker suspended + joined, agent semantic jobs paused) →
  std thread: `maintenance::load_sorted_conversations(env, show_last, None, &TimeFilter::default())` (fresh, no time
  filter, AD9) → `semantic_ops::generate_semantic_cache(env, &convs, scope_workspace, &TimeFilter::default(),
  &mut shared.ops_factory(), &mut progress, &cancel)`; progress lines into the dialog; Cancel → `cancel.cancel()` →
  `Semantic search cancelled`; capacity error text from core; done → `GenerateReport::text`; finally `gate.leave()`
  (list worker resumes and reloads the cache).
* Clear: AlertDialog §5.11 → `gate.enter_exclusive(Clear)` → shared model dropped (files closed) →
  `semantic_ops::clear_semantic_cache(env)` (under core's lock; empties `<cache root>/semantic` and keeps the directory
  and `embeddings.lock`, never follows links — K6 as built) → `ClearReport::message` / error (+Retry) →
  `gate.leave()`.

### 11.17 Update check
Help/Tools → Check for updates… (or `update` subcommand) → dialog `Checking for updates…` → background
`services.fetch_latest_release(env!("CARGO_PKG_VERSION"))` → §5.18 texts per `UpdateStatus`; `Available` also checks
`release.asset_for(std::env::consts::OS, ARCH)` and says when there is no build for this platform; [Open release page]
→ `services.open_url(release.url)` (else `RELEASES_PAGE_URL`). No automatic checks.

### 11.18 Settings save + live apply (K12)
1. Open → `SettingsDraft { base: config.gui_file.clone(), edited: same, sources }`; the draft subscribes to
   `AppState` changes: a field the user has not edited is refreshed from the new state (e.g. the theme changed from
   the View menu).
2. Edits update `edited` only; per-field validation (key bindings parse, nonzero numbers, existing paths).
3. Save (disabled while `config_errors.gui` is set) → background `config::save_gui_config_changes(env, &base,
   &edited)` → reload both files → `AppState` update → apply:
   * keys → `keymap::install_user_bindings(cx, &new_keys)` (menus/palette rebuilt, hints refresh);
   * theme → §11.19; terminal/program paths → next launch of a terminal;
   * `[tui] exclude_projects` → `browser.set_excluded_projects`;
   * `[display] last` → list refresh (previews are chosen at load);
   * `[display] no_tools` / `show_thinking`, `[search] mode` → new defaults for conversations opened later and for
     the next launch (they now beat remembered values, PD17); the open view keeps its toggles;
   * `[agent]` → the next Deep Search run;
   * toast `Settings saved to <path>`. Errors (incl. K12's refusal) → inline error, draft kept.
4. Cancel → discard draft.

### 11.19 Theme switching
Trigger: View → Theme ▸ System/Light/Dark, palette, Settings, OS appearance change (System only).
`theme_tokens::apply_theme(pref, window, cx)`: System → `Theme::sync_system_appearance(Some(window), cx)`, else
`Theme::change(mode, …)`; `core::theme::set_theme(kind)`; for every open window: viewer `apply_theme` (clear syntect
caches, anchored re-render), list notify; menus/palette rebuilt (check marks); the choice is saved with
`save_gui_config_changes` (only `[gui] theme` changes) when made through the UI. While gui.toml is unparseable the
theme applies to this session only and a warning toast says `Theme not saved: gui.toml has errors`.

### 11.20 Workspace change
Picker selection / Choose folder… / Clear → validate dir → `AppState.workspace` + `env.workspace_dir` (new
`Arc<HistoryEnv>`) → `browser.set_workspace(scope)` → recents (state.json) → title → reload with
`loader.start_refresh_with(RefreshPolicy::ApplyPartial)` (K18): the rows always match the new workspace;
`project_errors > 0` → warning toast `N project(s) could not be loaded. See Tools → Debug log.`; `RefreshFailed(e)`
→ the workspace, title and scope revert to the previous values (and the env/browser too) + error toast
`Couldn't switch the workspace: <e>`. If a load is already running, the change waits for it and then reloads.

### 11.21 Time filter change
Dropdown preset or custom dialog → `TimePoint::from_str` + `TimeFilter::resolve` (errors inline) →
`loader.set_time_filter(filter)` → reload with `RefreshPolicy::ApplyPartial` as in §11.20; success → chip label
updated; `RefreshFailed` → chip and loader filter revert + error toast `Couldn't apply the time filter: <e>`. The
filter is not persisted (PD17).

### 11.22 Quit, window close and persistence
* `exit::quit(cx)`: picker mode → `quit_without_output`; else `persist::flush`, `cx.quit()`.
* Main window should-close → `exit::quit`. Single-file window close → remove that window; quit mode
  `LastWindowClosed` ends the app when no window remains (single-file-only runs).
* `cx.on_app_quit`: flush `UiState` synchronously within the shutdown timeout. Picker-mode quits print nothing
  except the selected result.

### 11.23 Headless paths (no window)
* `help …` / `<subcommand> --help` (K14 as built): `SubcommandHelp(text)` → stdout, exit 0.
* `claude-history-gui agent …` (K15): `Help(text)` → stdout, exit 0; `Request(req)` →
  `core::agent::execute(&env, &config.effective, req)` with the env's workspace = launch cwd (upstream used the cwd)
  → write `stdout`/`stderr` verbatim, exit `exit_code`; parse error → stderr clap-style text, exit 2. Semantic
  requests load the real model (as upstream); no window is created.
* `--show-dir` with stdout available: workspace = PD3 rules 1–3 (no GUI state) → `maintenance::show_dir(env,
  workspace)` → stdout path + `\n`, exit 0; error → stderr `Error: <e>`, exit 1. Without stdout → open the GUI and
  show the projects-directory toast (AD14).
* `--render FILE` with stdout **redirected**: `generate_export(Ledger, settings options)` (no ANSI, `[dev]` vs
  upstream's ANSI ledger) → stdout, exit 0; errors → stderr, exit 1. Otherwise the single-file window (C-011).

### 11.24 One-shot tool corpora (P11)
Debug search, Semantic search report, Semantic debug dump and Generate cache work on a **fresh load** in the
background, never the list corpus: `maintenance::load_sorted_conversations(env, show_last, debug, &tf)` with `tf` =
the launch time filter (Generate: none, AD9). Their Scope control (All | Workspace) is preselected Workspace when the
app was started with `-L/--local` (or the startup tool says `local: true`) and a workspace exists.

---

## 12. Copy rules, error/empty/loading states and strings

Rules (§0): text that leaves the app is upstream-verbatim (stdout, stderr, clipboard exports, agent text, "Copy as
text" reports, debug.log). In-app text uses `…` (never `...`), sentence case, "conversation" for the object and
"session ID" for the identifier. Core status constants that the GUI shows in-app are mapped through
`notify::gui_text(core_text)` (`...` → `…`, `->` → `→`, "sessions" → "conversations" where it names the list
objects); the table below is normative. "(TUI)" = the TUI origin of the meaning. Toast kinds: I info, S success,
W warning, E error. Every error state offers a recovery action (DG §9.13).

| Where | Condition | UI | Text (in-app) | TUI / core origin |
|---|---|---|---|---|
| List area | initial load, 0 rows yet | centered Spinner + text | `Reading history…`; suffix `Loading… 0` | `Loading... 0` |
| List area | `LoadFailed(e)` | Empty (error icon) + [Retry] [Show debug log] | title `Couldn't load history`; body `e` (e.g. `Claude projects directory not found at <path>`) | TUI exited with `e` |
| List area | `NoHistory(e)` | Empty + [Refresh] [Clear time filter] (if active) | `No conversation history found in selected scope` (+ ` within the requested time range`) | (TUI, verbatim) |
| List area | query has no matches | Empty (muted) + [Clear search] | `No matching conversations` | blank list |
| List area | everything excluded / out of scope | Empty + [Show all] | `No conversations in this scope` + `Switch to All or change the workspace.` | blank list |
| List area | semantic failed | list cleared (TUI) + Empty + [Switch to lexical] [Retry] | `Semantic search failed` + `semantic_error()` text | suffix `sem failed` |
| List area | semantic no text | Empty + [Switch to lexical] | `No text to search semantically` | suffix `sem no text` |
| Search suffix | states | label | `Loading… <n>` / `<i>/<n>` / `0/<n>` / `sem <i>/<n>` / `sem failed` / `sem no text` / `sem paused` | `Browser::search_status_text` |
| Footer | semantic init / embedding | spinner / progress | `sem preparing embeddings` / `sem embedding <p>%  <d>/<t> chunks` | `semantic_activity_status_text` (verbatim) |
| Footer | refresh/reload running | spinner (persistent) | `Refreshing conversations…` | `Refreshing sessions...` (`REFRESHING_STATUS`) |
| Toast S | refresh done | replaces previous | `Refreshed conversations: <old> → <new>` | `Refreshed sessions: <old> -> <new>` |
| Toast W | refresh with project errors | + [Show debug log] | `Refresh incomplete; keeping the current list` | `Refresh incomplete; keeping existing sessions` |
| Toast E | refresh fatal / interrupted | + [Retry] | `Refresh failed: <err>` / `Refresh failed; keeping the current list` | `Refresh failed: <err>` / `Refresh failed; keeping existing sessions` |
| Toast W | load/reload with project errors | + [Show debug log] | `N project(s) could not be loaded. See Tools → Debug log.` | (ignored by TUI) |
| Toast E | UUID not found | | `Session not found: <uuid>` | `UuidLookup::status_message` |
| Rename dialog | errors (inline) | | `Failed to rename: conversation became empty` / `Failed to rename: <err>` / `This conversation is no longer in the list` | `RenameError` Display |
| Rename | success | row + header update, no toast | — | `Session renamed` |
| Toast S / E | delete | | `Deleted "<title>"` / `Deleted session <id> (<n> copies across projects)` / `Failed to delete session <path>: <err>` | CLI `delete_by_uuid_message`; TUI debug-log line |
| Toast I | a deletion closed open dialogs | | `Closed: "<title>" was deleted` | — |
| Viewer | nothing selected | Empty | `No conversation selected` + `Select a conversation on the left, or press Enter.` | — |
| Viewer | opening | thin progress / Spinner | `Loading conversation…` | — |
| Viewer | open failed | Empty error + [Retry] [Show debug log] | `Failed to open: <err>` | (TUI) |
| Viewer | no visible rows | Empty + [Show thinking] [Show tools in full] | `This conversation has no visible messages` | blank viewer |
| Viewer | truncated output | indicator row | `(N more lines…)` | `(N more lines...)` |
| Viewer find bar | typing | label | `/<query>` + ` (no matches)` / ` (<i>/<n>)` | `ViewSearch::input_status` (verbatim) |
| Toast | viewer copy/export | | `Path copied to clipboard`, `Session ID copied to clipboard`, `Message copied to clipboard`, `No text content in this message`, `Message not found`, `Copied to clipboard`, `Exported to <full path>`, `Failed to read: <e>`, `Failed to write: <e>` | core `export` constants (verbatim) |
| Toast I | partial selection copy | | `Copied the visible part of the selection. Use Y, y or Export to copy more.` / `Nothing copied: the selection scrolled out of view.` | — |
| Toast I | `p` | + [Copy] [Reveal] | `<full path>` | (TUI shows the path) |
| Toast E | resume/fork | + recovery action | core `WORKSPACE_REQUIRED_RESUME/FORK` + [Choose workspace…]; `Could not find '<p>' on PATH. Set its path in Settings → General.` + [Open Settings]; `Failed to launch terminal: <e>` + [Terminal settings]; `Failed to copy the conversation: <e>`; `Failed to run Claude CLI: <e>` | core `AppError` texts |
| Toast I | resume/fork ok | | `Opened "<title>" in <launcher>` + `<command>` | — |
| Toast W | resume plan warnings | | core `ResumePlan::warnings` lines (e.g. `Session directory <dir> does not exist`) | core |
| Single-file | bad path / empty | error view | `File not found: <p>` / `Not a file: <p>` / `<file> contains no conversation messages` | (TUI) / core D1 |
| Agent panel | file outside the history roots | panel body | `Outline, Within and Read work only for conversations inside the Claude, Pi or OMP history folders.` | — |
| Startup | config parse error | dialog per file | title `Couldn't read config.toml` / `Couldn't read gui.toml`; body = core `Configuration error: Failed to parse config file at '<path>': <err>` | (TUI exited with it) |
| Startup | bad CLI args | stderr | `error: …` (core `LaunchArgsError`), exit 2; `Error: Invalid time range: …`, exit 1 | clap / upstream main |
| Startup | `--workspace` missing dir | toast W | `Workspace not found: <DIR>` | — |
| Deep search | idle | Empty | `Search all conversations` + `Lexical, semantic, exact or hybrid search across transcripts, with evidence.` | — |
| Deep search | running | Spinner + [Cancel] | `Searching…` (+ `Embedding 12/30`) | `Semantic search: <progress>` |
| Deep search | 0 hits | Empty | `No hits` | agent text `… hits=0` |
| Deep search | typed error | alert | `<kind>: <detail>` (ambiguous candidates clickable) | agent envelope |
| Deep search | uncached passages | banner + [Generate semantic cache…] | `<n> passage(s) are not cached. Generate the semantic cache to include them.` | stderr line (kept verbatim in the agent text view) |
| Deep search | cancelled / paused | info line | `Search cancelled` / `Semantic search is paused while the cache is generated` | — |
| Agent panel | stale result set | banner + [Run for this conversation] | `Results are for <title>.` | — |
| Viewer | reveal target missing | toast I | `Message m<N> is not shown in the viewer` | — |
| Picker / Ctrl+O | no stdout | toast I + [Export…] | `Nothing to print to: start the app from a terminal, or use Export…` | — |
| Ctrl+O | stdout is a terminal | AlertDialog | `Print "<title>" to the terminal and quit?` [Cancel] [Print and quit] | — |
| Settings | saved / blocked | toast S / inline | `Settings saved to <path>` / `gui.toml has errors; fix or remove it before saving: <e>` | core K12 |
| Theme | gui.toml broken | toast W | `Theme not saved: gui.toml has errors` | — |
| Workspace / time filter | reload failed | toast E | `Couldn't switch the workspace: <e>` / `Couldn't apply the time filter: <e>` | — |
| Tools | §5.10–§5.18 | | strings listed there; report texts verbatim from core (`to_text`, `text`, `message`, `format_delete_empty_report`) | CLI |
| Disabled items | unknown source / no workspace | tooltip | `Unknown conversation type` / `Choose a workspace first` / `Choose a workspace to filter by project` | — |

---

## 13. CLI, configuration and persisted state

### 13.1 GUI command line (core `settings::parse_launch_args` + K14/K15; mapped to `LaunchContext` by `startup.rs`)
Usage lines: `claude-history-gui [OPTIONS] [FILE]` / `claude-history-gui delete-empty [--local|--all] [--yes]` /
`claude-history-gui update` / `claude-history-gui agent <search|within|read|outline> …` /
`claude-history-gui help [COMMAND]…`.

| Flag | Short | Value | Effect in the GUI | FEATURES |
|---|---|---|---|---|
| `[FILE]` | | path | single-file viewer window (PD14) | C-001 |
| `--workspace DIR` | `-w` | path | workspace (PD3 rule 1) | C-019 |
| `--local` | `-L` | | workspace = launch cwd (PD3 rule 2); start in Workspace scope; one-shot tools preselect Workspace | C-003 |
| `--global` | `-g` | | hidden, deprecated, no effect; conflicts with `--delete` | C-004 |
| `--since WHEN` / `--after WHEN` | | TimePoint | lower time bound (aliases, mutually exclusive) | C-005 |
| `--before WHEN` | | TimePoint | upper time bound; inverted range → `Error: Invalid time range: …`, exit 1 | C-005 |
| `--last` / `--first` | `-l` / – | | previews last/first messages (exclusive) | C-002 |
| `--show-tools` / `--no-tools` | `-t` / – | | initial tool display Full / Summary; Ctrl+O output `show_tools` | C-006 |
| `--show-thinking` / `--hide-thinking` | | | initial show_thinking; Ctrl+O output | C-007 |
| `--semantic` / `--lexical` | | | initial list mode (GUI-only, core) | C-062 |
| `--show-path` | `-p` | | picker: print path | C-008 |
| `--show-id` | `-i` | | picker: print session id | C-008 |
| `--resume` | `-c` | | picker: resume | C-008 |
| `--fork-session` | | | with `--resume`: fork (requires `--resume`) | C-008 |
| `--plain` | | | Ctrl+O prints the CLI plain format | C-008 |
| `--debug [LEVEL]` | | debug\|info\|warn\|warning\|error (case-insensitive; bare = debug) | debug sink level to stderr + Debug log default level; Ctrl+O diagnostics | C-009 |
| `--pager`, `--no-pager`, `--no-color` | | | accepted, no effect | C-010 |
| `--render FILE` | | path | stdout redirected → ledger to stdout, exit (headless); else single-file window | C-011 |
| `--show-dir` | `-d` | | stdout available → print `<projects root>/<encoded workspace>` and exit (headless); else toast in the GUI | C-012 |
| `--delete ID` | | string | after start: Delete conversation by session ID dialog, prefilled + Find (confirmation required) | C-013 |
| `--debug-search QUERY` | | string | after start: Debug search dialog prefilled + run (scope from `-L`, launch time filter) | C-014 |
| `--semantic-search QUERY` [`--semantic-top N`] | | hidden | Semantic debug search dialog, report mode, prefilled + run | C-015 |
| `--debug-semantic-search QUERY` | | hidden | same dialog, debug-dump mode | C-015 |
| `--generate-semantic-cache` | | hidden | Generate semantic cache dialog (not auto-started; scope from `-L`) | C-015 |
| `--clear-semantic-cache` | | hidden | Clear semantic cache confirmation | C-015 |
| `-h/--help`, `-V/--version` | | | print `help_text` / `claude-history-gui <version>`; exit 0 | C-017 |
| `help [COMMAND]`, `<subcommand> -h/--help` | | subcommand | `help` = `--help`; `help update\|delete-empty\|help` and `update --help`, `delete-empty -h` → `LaunchCommand::SubcommandHelp(text)`: print `text`, exit 0; `help agent [cmd]` → the agent help (K14/K15 as built) | C-017 |
| `delete-empty [--local\|--all] [--yes]` | | subcommand | Delete empty transcripts dialog with the scope; `--yes` ignored (confirmation in the UI) | C-018 |
| `update` | | subcommand | Check for updates dialog | C-018 |
| `agent search\|within\|read\|outline …` | | subcommand | headless: exact upstream protocol output and exit codes (K15, §11.23) | C-061 |
| `--qa-scenario PATH`, `--qa-out DIR`, `--qa-model-dir DIR` | | feature `qa` only | QA runner (§15.3) | X-027 |

Conflict groups (core K14; message `error: the argument '<a>' cannot be used with '<b>'`, exit 2): `one_shot_action`
(at most one of `--show-dir`, `--delete`, `--debug-search`, `--debug-semantic-search`, `--semantic-search`,
`--generate-semantic-cache`, `--clear-semantic-cache`, `--render`, `FILE`); `selection_conflicting_action` (those
except `--show-dir`, conflicting also with `--resume`, `--show-path`, `--show-id`, `--plain`); picker kinds mutually
exclusive; `--delete` × `--global`; `FILE` × `--local`/`--global`/picker flags; `--semantic-top` requires
`--semantic-search` (`value must be greater than zero`; non-number → `invalid positive integer: <v>`); invalid
`--debug` level → `invalid value '<x>' for '--debug [<LEVEL>]': …`; empty `QUERY` → `value must not be empty`.
Subcommands cannot be combined with top-level flags.

### 13.2 `gui.toml` and `config.toml`
* `config.toml` (`~/.config/claude-history/config.toml`): read-only, upstream schema with `deny_unknown_fields`;
  parse errors → §5.20 dialog. Never written (core refuses).
* `gui.toml` (`~/.config/claude-history/gui.toml`, core `GuiConfigFile`): may override `[display]`, `[resume]`,
  `[keys]`, `[tui]`, `[search]`, `[agent]` and holds `[gui]`. Effective value = gui.toml > config.toml > default.
  Written only by `save_gui_config_changes` (K12): changed keys only, comments preserved, refused while the file is
  unparseable.

Settings dialog pages → fields (all saved into gui.toml; "Reset" removes the gui.toml key). The source line reads
`From gui.toml`, `From config.toml: <value>`, `Remembered from last session: <value>` (view defaults/list mode, only
when no config key is set and restore_session is on) or `Default: <value>`; "Applies" says when a change takes effect.

| Page | Field | gui.toml key | Control | Values / validation | Applies |
|---|---|---|---|---|---|
| General | Theme | `[gui] theme` | Segmented | system / light / dark | immediately |
| General | Terminal | `[gui] terminal` | Select + custom text | core syntax: `auto` (default), `wt`, `cmd` (`console`), `terminal`, `osascript`, a Linux emulator name (`gnome-terminal`, `ptyxis`, `kgx`, `konsole`, `xfce4-terminal`, `tilix`, `alacritty`, `kitty`, `wezterm`, `xterm`, `x-terminal-emulator`), or a template with `{cwd}`, `{args}`, `{cmd}`; the field previews the command for a sample resume (`build_terminal_command`) | next launch |
| General | claude / pi / omp program | `[gui] claude_path` / `pi_path` / `omp_path` | path input + Browse… | must exist when set; shows the PATH resolution result | next launch |
| General | Default workspace | `[gui] default_workspace` | folder input + Browse… | existing dir | next start |
| General | Restore last session | `[gui] restore_session` | Switch | default on | next start |
| Display | Preview shows | `[display] last` | Segmented Last / First | | list refresh |
| Display | Default tool display | `[display] no_tools` | Segmented Summary / Full | true → Summary, false → Full, unset = remembered/Summary | next opened conversation + start |
| Display | Show thinking and subagents by default | `[display] show_thinking` | Switch | | same |
| Display | Ctrl+O prints plain text | `[display] plain` | Switch | | next Ctrl+O |
| Display | (pager) | `[display] pager` | read-only "not used by the GUI" | | — |
| Search | Default search mode | `[search] mode` | Select lexical/semantic/exact/hybrid | list maps exact/hybrid → Lexical; Deep search uses all 4 | next start; Deep search next run |
| Search | Excluded projects | `[tui] exclude_projects` | list editor | exact names; `repo` also hides `repo/<worktree>` | immediately |
| Keys | Resume, Fork, Rename, Delete, Refresh | `[keys] …` | text inputs (`ctrl+r` syntax) + live warnings | §8.5 | immediately |
| Resume | Default arguments | `[resume] default_args` | list editor | Claude only | next resume |
| Agent | Scope | `[agent] scope` | global / local | | next run |
| Agent | Mode | `[agent] mode` | lexical/semantic/exact/hybrid | | next run |
| Agent | Output budget (chars) | `[agent] output_chars` | NumberInput ≥ 1 | 6000 default | next run |
| Agent | Top / Within top / Hits per conversation | `[agent] top` / `within_top` / `hits_per_conversation` | NumberInput ≥ 1 | 10 / 20 / 2 | next run |
| Agent | Excluded projects (encoded dir names) | `[agent] exclude_projects` | list editor | e.g. `-Users-me-code-repo` | next run |
| Agent | Include tools / tool results / thinking / subagents | `[agent] tools` / `tool_results` / `thinking` / `subagents` | Switches | ORed with the panel toggles | next run |

Deprecated config keys (`global`, `display.relative_time`, `tui.semantic_search`) are shown read-only when present in
config.toml ("deprecated, ignored" / "used when [search] mode is unset").

### 13.3 `state.json` (persist.rs, PD17)
Path: `<home>/.local/state/claude-history-gui/state.json` (the directory of `env.debug_log_file()`). Schema (serde,
`#[serde(default)]` everywhere, unknown keys ignored):
```json
{
  "version": 2,
  "main_window": { "kind": "windowed|maximized|fullscreen", "bounds": {"x":0,"y":0,"w":1280,"h":820} },
  "single_file_window": { "kind": "windowed", "bounds": {"x":0,"y":0,"w":1000,"h":800} },
  "split": { "list_width_rem": 30.0, "deep_width_rem": 34.0, "agent_panel_width_rem": 22.0 },
  "list_collapsed": false,
  "agent_panel": { "visible": false, "tab": "outline" },
  "left_tab": "conversations",
  "deep_options_open": false,
  "workspace": "C:\\git\\proj",
  "recent_workspaces": ["C:\\git\\proj", "C:\\git\\api"],
  "search_mode": "lexical",
  "view": { "tool_display": "summary", "show_thinking": false, "show_timing": false },
  "last_export_dir": "C:\\Users\\me\\Downloads"
}
```
Restored values are clamped (window to current displays, widths to the ranges and the window, PD1). `restore_session =
false` → only `last_export_dir` and `recent_workspaces` are used. A version-1 file is read (its `time_filter_text` is
ignored).

### 13.4 Data locations (core `env.rs`)
| What | Location |
|---|---|
| Claude data | `$CLAUDE_CONFIG_DIR` or `<home>/.claude` (read; written by rename/delete/fork-copy/delete-empty) |
| Pi / OMP sessions | per core loaders (`PI_*`, `OMP_PROFILE`, `XDG_DATA_HOME`, relative values against the launch cwd (K16), workspace `.pi/settings.json`) |
| GUI caches | `<home>/.cache/claude-history-gui/{projects,config-<hash>/projects,pi,omp,semantic,launch}` |
| Embedding model | `<home>/.cache/claude-history-gui/semantic/fastembed` |
| CLI config | `<home>/.config/claude-history/config.toml` (read-only) |
| GUI config | `<home>/.config/claude-history/gui.toml` |
| Debug log | `<home>/.local/state/claude-history-gui/debug.log` |
| UI state | `<home>/.local/state/claude-history-gui/state.json` |

---

## 14. Platform specifics

### 14.1 Terminal launching (core `resume::terminal`, GUI `ops/resume.rs` + `services.rs`)
* Commands are built by core only (§4.1, PD4); the GUI holds no quoting code. The GUI passes `LaunchOptions {
  platform: Platform::current(), terminal: config.gui.terminal, terminal_var: env "TERMINAL" read in main, keep_open:
  true, script_dir: None }`.
* Order of attempts: `terminal::launcher_candidates` (K17): the configured launcher only, else every detected one
  (Windows: Windows Terminal (`wt.exe -w new -d <cwd> cmd.exe /S /K "<command>"`) → console (`cmd.exe /C start "" /D
  <cwd> cmd.exe /S /K …`); macOS: Terminal.app `.command` script (self-deleting) → osascript; Linux: `$TERMINAL`,
  then x-terminal-emulator, gnome-terminal (`--`), ptyxis, kgx, konsole, xfce4-terminal, tilix, alacritty, kitty,
  wezterm, xterm). A spawn error moves to the next candidate; an early non-zero exit within 3 s is reported (e.g.
  `open -a` failing); children are reaped by core.
* Debug builds refuse to spawn `claude`/`pi`/`omp` without an override or `CHG_ALLOW_REAL_LAUNCH=1` (§15.4).
* Never kill terminal processes (they may host the user's other tabs).

### 14.2 Program resolution and PATH
`env.resolve_program(name, override)` (core; PATHEXT, `.cmd` shims). On macOS/Linux, when the GUI was not started
from a terminal (stdin not a TTY), `login-shell-path` runs `$SHELL -ilc 'printf "__CHG_PATH__%s__CHG_END__" "$PATH"'`
(interactive + login so `.zshrc`/`.bashrc` entries such as nvm, volta, bun are included) with a 3 s timeout, parses
the text between the markers, and prepends it (deduplicated) to `HistoryEnv.path_var`, plus `~/.local/bin`,
`/opt/homebrew/bin`, `/usr/local/bin`, `~/.npm-global/bin`, `~/.bun/bin` when they exist. A resume issued while the
probe runs waits for it (≤ 3 s). Shell output outside the markers is ignored; failures leave PATH unchanged (debug
line).

### 14.3 stdout/stderr (ops/console.rs, WP7)
* `stdout_kind()`: Windows: `GetStdHandle(STD_OUTPUT_HANDLE)` valid → `GetFileType` (`FILE_TYPE_CHAR` → Terminal;
  disk/pipe → Redirected); invalid/null → `AttachConsole(ATTACH_PARENT_PROCESS)` → success → reopen `CONOUT$` →
  Terminal; failure → None. Unix: `isatty(1)` → Terminal; `fstat(1)` ok → Redirected; else None.
* Used for `--help`, `--version`, usage errors, headless paths, picker output, Ctrl+O and `--debug` stderr lines.
* Output is written with a single `write_all` + flush; nothing else is ever printed to stdout (F11).
* Debug builds keep the console subsystem.

### 14.4 Windows specifics
* `#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]` in main.rs.
* `/STACK:8000000` for every executable of both crates (test harnesses and examples included):
  `cargo:rustc-link-arg` from `crates/core/build.rs` and `crates/gui/build.rs` (target_env msvc), not rustflags in
  `.cargo/config.toml`, which cargo also merges into worktrees nested in the main checkout (doubled array values,
  or a failed merge of an array with a string). `.cargo/config.toml` keeps `[env] RUST_MIN_STACK = "8388608"`
  (libtest threads).
* `crates/gui/build.rs`, besides `/STACK` above (bins only, target_env msvc): `cargo:rustc-link-arg-bins=/DELAYLOAD:DirectML.dll`,
  `cargo:rustc-link-arg-bins=delayimp.lib` (verified spike-platform); embed `assets/windows/app.rc` (`1 ICON
  "app.ico"` + VERSIONINFO) with `embed-resource = "3"`, `manifest_optional()`, **no manifest** (GPUI embeds one).
* The static ONNX Runtime (`ort-download-binaries`) is downloaded by `ort-sys` at build time (CI caches
  `%LOCALAPPDATA%\ort.pyke.io`). Never enable `+crt-static` (LNK2038).
* The exe needs the VC++ 2015–2022 x64 runtime: the release zip ships `MSVCP140.dll`, `MSVCP140_1.dll`,
  `VCRUNTIME140.dll`, `VCRUNTIME140_1.dll` app-local, located with `vswhere` → `VC\Redist\MSVC\<ver>\x64\Microsoft.VC143.CRT`.
* `cx.set_app_identity` (taskbar grouping). `window.prompt` is not used (light-themed); in-app dialogs only.
* Path handling: workspace encoding via core (verbatim prefix and drive-letter normalization; X-029).

### 14.5 macOS specifics (arm64)
* `cx.set_menus(menu_tree)` + app menu; no AppMenuBar. `secondary` = Cmd. AD2: Ctrl+A/E stay emacs line start/end in
  inputs.
* `.app` bundle `Claude History.app/Contents/{Info.plist (version from Cargo.toml, LSMinimumSystemVersion 12.0),
  MacOS/claude-history-gui, MacOS/lib/libonnxruntime.<ver>.dylib, Resources/app.icns}`; `CFBundleIdentifier
  io.github.magicxor.claude-history-gui`; built with `--features release-dynamic-ort` (core finds the library in
  `<exe dir>/lib`); `MACOSX_DEPLOYMENT_TARGET=12.0`; ad-hoc signed (`codesign --force --deep -s -`); zipped with
  `ditto -c -k --keepParent`. Notarization out of scope (README).
* Login-shell PATH augmentation (§14.2).

### 14.6 Linux specifics (x86_64)
* AppMenuBar; client decorations; requires a Vulkan driver and an X11/Wayland session.
* Tarball: `claude-history-gui`, `lib/libonnxruntime.so.<ver>` (+ symlink), `.desktop`, 512 px icon, licenses; built
  with `--features release-dynamic-ort` on ubuntu-22.04 (glibc baseline).
* File dialogs need the xdg-desktop-portal FileChooser; without it the save/open prompts return `Err` and the PD8
  fallback / an error toast is used.

---

## 15. Testing strategy (PD19)

Absolute rules:
* No OS input injection (no SendKeys, no synthetic OS mouse) and no real-desktop automation.
* No test, QA run or agent dev run reads or writes the real `~/.claude`, `~/.pi`, `~/.omp`,
  `~/.config/claude-history`, `~/.cache/claude-history*`, `~/.local/state/claude-history*`, the real clipboard, or
  launches `claude`/`pi`/`omp`, URLs, file managers or native file dialogs. This is **enforced**, not only stated:
  core `hermetic` guards in every GUI test build (K13), the env redirect in the test binary, `PlatformServices` for
  every side effect, the CI safety lint, the QA/debug-build sandbox gate (§15.4).

### 15.1 Layers and verification codes (used in FEATURES.md "Acceptance")
| Code | Layer | Where | Runs |
|---|---|---|---|
| **U** | core unit test (`#[test]`, `HistoryEnv::sandbox(tempdir)`) | `crates/core` | CI, 3 OS |
| **P** | pure GUI-crate unit test (no gpui window): segments, items, keymap conversion, startup resolution tables, persistence, row layout, header builder | `crates/gui/src/**/tests.rs` | CI, 3 OS |
| **G** | gpui test: `#[gpui_kit::test]` with `TestAppContext`/`VisualTestContext`, production views opened via the harness, keys via `simulate_keystrokes`, actions via `dispatch_action`, fixtures in temp dirs, `RecordingServices` | `crates/gui/src/**/tests.rs`, `src/integration_tests/` | CI, 3 OS |
| **Q** | QA scenario in the real app window (feature `qa`), sandbox HOME, in-process runner | `crates/gui/qa/scenarios/*.qa` | dev machine (Windows) + `qa.yml` (manual dispatch, Windows runner) |
| **S** | screenshot review (light + dark) from QA `shot` steps | `qa-out/*.png` | dev machine (Windows), reviewer |
| **M** | automated CI artifact/smoke check (release workflow, `dumpbin`, terminal smoke, packaging) | `.github/workflows/*` | CI |
| **R** | review by the integrator against the §15.7 checklist, recorded in the merge note | — | per merge |
| **D** | deferred manual check by the user on a real macOS/Linux/Windows desktop, from the §15.9 checklist | — | before a release |

### 15.2 gpui tests (G) — hermetic harness (`src/test_support/mod.rs`, WP0, frozen)
* `crates/gui` `[dev-dependencies]` enables `claude-history-core/hermetic` (K13): in GUI test builds
  `from_process_env` panics, the default embedder factories refuse, `check_latest_release` panics and terminal
  spawning returns an error — even if GUI code bypasses a seam by mistake.
* A `#[ctor::ctor]` in `test_support` runs before any test: it creates a per-process temp root and sets (unsafe
  `set_var`, before threads exist) `HOME`, `USERPROFILE`, `CLAUDE_CONFIG_DIR`, `PI_CODING_AGENT_DIR`,
  `PI_CODING_AGENT_SESSION_DIR`, `PI_CONFIG_DIR`, `XDG_DATA_HOME`, `XDG_CONFIG_HOME`, `XDG_CACHE_HOME`,
  `XDG_STATE_HOME` inside it and removes `OMP_PROFILE`, `PI_PROFILE`, `TERMINAL`, `CLAUDE_HISTORY_CLIPBOARD`.
* `app::init_for_test` installs `RecordingServices`, a refusing embedder (`embedder::install_fake` to opt in),
  disables state.json writes, and uses an empty config unless the test writes one into its `TestWorld`.
```rust
pub struct TestWorld { pub tmp: tempfile::TempDir, pub env: HistoryEnv }   // HistoryEnv::sandbox(tmp)
impl TestWorld {
    pub fn new() -> Self;
    pub fn add_claude_session(&self, project_dir: &str, uuid: &str, lines: &[&str]) -> PathBuf;
    pub fn add_fixture(&self, rel: &str, bytes: &[u8]) -> PathBuf;     // copies from crates/core/tests/fixtures or crates/gui/tests/fixtures
    pub fn standard_corpus(&self) -> Vec<PathBuf>;                     // 6 Claude sessions in 3 projects (+ worktree), 1 Pi, 1 OMP, 1 empty, 1 malformed line
    pub fn write_config(&self, config_toml: Option<&str>, gui_toml: Option<&str>);
}
pub fn open_main(cx: &mut TestAppContext, world: &TestWorld, args: &[&str]) -> (AnyWindowHandle, Entity<Workspace>);
pub fn open_single_file(cx: &mut TestAppContext, world: &TestWorld, path: &Path) -> AnyWindowHandle;
pub fn open_single_file_with_args(cx: &mut TestAppContext, world: &TestWorld, path: &Path, args: &[&str]) -> AnyWindowHandle; // (as built)
pub fn launch_for_args(world: &TestWorld, args: &[&str]) -> LaunchContext;   // (as built) launch cwd = the world's home
pub fn activate(cx: &mut TestAppContext, handle: AnyWindowHandle);           // (as built) focus-in events need an active window
pub async fn wait_loaded(cx: &mut TestAppContext, ws: &Entity<Workspace>);
pub fn recording(cx: &TestAppContext) -> Arc<RecordingServices>;        // answer queues, calls(), clipboard
pub fn global_lock() -> MutexGuard<'static, ()>;                        // tests touching core::theme / core::debug globals hold it
pub mod synth { pub fn corpus(world: &TestWorld, n: usize); pub fn transcript(path: &Path, messages: usize, big_outputs: usize, lines_per_output: usize); }
pub mod fault { pub fn undeletable(path: &Path) -> FaultGuard; pub fn unwritable(path: &Path) -> FaultGuard; }  // §15.10
```
Test modules import names explicitly (never `use gpui_kit::*`). Per-WP fixture builders live in
`test_support/fixtures_<wp>.rs` (owned by that WP; WP0 declares all five: `fixtures_list`, `fixtures_viewer`,
`fixtures_ops`, `fixtures_agent`, `fixtures_tools`), and `src/integration_tests/mod.rs` (WP-I) is declared too.

As built (WP0): the test app is initialised **once per `TestAppContext`** by the first `open_main`/`open_single_file*`
call; a later call with another `TestWorld`, or with non-empty `args` that differ from the first ones, panics (the
app state cannot change after init — open the window that needs the arguments first). Windows opened by the harness
are activated and drawn (`activate`). Also: `TestWorld::{home, projects_root}`, `redirect_root()`, `redirect_home()`,
`REDIRECTED_VARS`, `REMOVED_VARS`, `synth::{uuid, cwd_base, encoded_project_dir, claude_conversation}` (the synthetic
transcript puts every big output on an assistant line), `fault::running_as_root()`; harness self-tests in
`test_support/self_tests.rs`. The redirect root (`%TEMP%/claude-history-gui-tests/pid-<pid>`) is removed when the test
process exits normally and roots of dead test processes are swept at the next start. Doc tests are off for the GUI
crate (`doctest = false`: they would run outside the redirect). The §16.2 safety lint also runs as a source test
(`skeleton_tests/safety_lint.rs`), so `cargo test` enforces it before CI exists.

Per-feature G test minimums (each WP lists them in §17): keyboard + pointer activation, focus transitions,
stale-result rejection, A1 races (inject a result set, refresh or semantic completion that resets the selection while
a dialog/viewer/menu holds a target, then confirm → assert the captured path is acted on, for rename, delete,
resume, fork, export, copy, Ctrl+S details), empty and error states with their recovery actions, toast texts,
clipboard contents (`recording(cx).clipboard()`), save/open dialogs (`answer_save`/`answer_open`), recorded terminal
commands (`ServiceCall::Terminal(cmd)` with `cmd.display()`).

### 15.3 QA scenario runner (feature `qa`, WP7)
* Build: `cargo build -p claude-history-gui --features qa` (release or debug).
* Start: `claude-history-gui --qa-scenario <file.qa> --qa-out <dir> [--qa-model-dir <dir>]` through
  `tools/qa/run-gui.ps1` (sandbox env, §15.4). The sandbox gate runs first.
* Services: `RecordingServices` (terminal launches, URLs, reveal, prompts answered from the scenario, in-memory
  clipboard, stdout recorded in the report). `--qa-real-console` switches only the stdout/stderr methods to the real
  console (used by the picker scenario run under `cmd /c "… > out.txt"` to verify A-051 end to end).
* Execution: after the first frame, a `cx.spawn_in(window, …)` task runs steps sequentially on the UI thread with
  `AsyncWindowContext`: `window.dispatch_keystroke(Keystroke::parse(k)?, cx)` for keys,
  `window.dispatch_action(cx.build_action(name, None)?, cx)` for actions, `window.dispatch_event(PlatformInput::Mouse…)`
  at the centre of a registered element for clicks (`qa_ext::QaExt::qa_id(id)`: for any `T: ParentElement + Styled`
  it appends an absolutely positioned canvas child that records the parent's bounds in a qa-only registry each frame;
  identity without the feature), timers for waits.
* DSL (`qa/dsl.rs`, one step per line, `#` comments):
```
wait-loaded 15s                      # until ListPane is Ready
type "token refresh"                 # text into the focused element (per char via dispatch_keystroke)
key down down enter                  # keystrokes (GPUI syntax, space-separated)
hold pagedown 800 [33ms]             # one key pressed N times at a key-repeat pace, no settle sleep (fix round)
action history::ToggleThinking       # dispatch by name
click row:3f2a19c0                   # qa_id click (left, single); dblclick / rclick variants
answer-save C:\sbx\out\x.md          # next save prompt answer (or: answer-save cancel)
answer-open C:\sbx\a.jsonl           # next open prompt answer
set-stdout redirected|terminal|none  # RecordingServices stdout kind
wait 300ms
wait-for viewer.items > 0 5s
expect focus == viewer
expect list.selected_title contains "Fix login"
expect toast.last == "Exported to C:\sbx\out\x.md"
expect clipboard contains "3f2a19c0"
expect calls.terminal.count == 1
expect stdout == "C:\sbx\.claude\projects\…\3f2a….jsonl\n"
timing list.ui_busy_max < 16ms       # performance probes (§15.8)
shot viewer-dark                     # screenshot handshake (below)
set-theme dark | light | system
resize 1280 820
quit 0
```
  Probe keys (`qa/probe.rs`, read-only accessors on the panes): `list.count`, `list.loaded`, `list.loading`,
  `list.query`, `list.mode`, `list.scope`, `list.selected_title`, `list.selected_path`, `list.ui_busy_max`,
  `viewer.path`, `viewer.items`, `viewer.messages`, `viewer.nav_focus`, `viewer.indicator`, `viewer.search.count`,
  `viewer.search.current`, `viewer.tool_display`, `viewer.thinking`, `viewer.timing`, `viewer.frame_p95`,
  `focus` (list|viewer|agent|deep|dialog|none), `dialog` (open dialog id or none), `toast.last`, `clipboard`,
  `stdout`, `calls.terminal.count`, `calls.terminal.last`, `calls.open_url.last`, `deep.hits`, `deep.groups`,
  `deep.mode`, `agent.tab`, `agent.target`, `theme`; since the fix round also `window.frame_p95` and
  `window.frame_max` (the whole main window's frame over its last 512 frames, X-008).
* Report: `<qa-out>/report.txt` (one `PASS|FAIL|SKIP <line> <step> [detail]` per step + summary) and
  `<qa-out>/report.json`; exit code 0 if no FAIL, else 1.
* Screenshot handshake (`shot NAME`): write `<qa-out>/NAME.request` (pid, window title), wait up to 15 s for
  `<qa-out>/NAME.png`; a timeout marks the step SKIP. `tools/qa/qa-shot-watch.ps1 -Pid <pid> -Dir <qa-out>` captures
  the process's main window with `PrintWindow` (read-only, same code as `scratchpad/tools/shot.ps1`, no input).
* Scenarios and owners (each WP writes and maintains the scenarios its DoD names): WP7 `smoke.qa`; WP1
  `startup-load.qa`, `list-search.qa`, `semantic-toggle.qa` (with `--qa-model-dir`), `perf-list.qa`; WP2
  `viewer-basics.qa`, `viewer-search-nav.qa`, `single-file.qa`, `perf-viewer.qa`, `perf-viewer-main.qa` (fix round:
  the viewer's frame in the main window with 20k rows, V-094); WP3 `rename.qa`, `delete.qa`,
  `resume-fork.qa`, `export-copy.qa`, `picker.qa`, `picker-exit.qa`; WP4 `deep-search.qa`, `agent-panel.qa`; WP5
  `tools.qa`; WP6
  `settings-keys.qa`, `theme.qa`, `palette.qa`; WP-I `cross-pane.qa`.

### 15.4 Sandbox gate, sandbox procedure (dev runs and QA)
* **Gate** (`qa/gate.rs`, runs in `main` for `--features qa` builds **and every debug build**): refuse with exit 3
  and `Refusing to run: <reason>. Create a sandbox with tools/qa/new-sandbox.ps1 or set CHG_ALLOW_REAL_HOME=1.`
  unless all hold: (1) the resolved home (`home::home_dir()`) contains a `.qa-sandbox` file; (2) the home is not the
  OS profile directory of the current user, obtained from the OS, not from variables (Windows
  `SHGetKnownFolderPath(FOLDERID_Profile)`, Unix `getpwuid_r(getuid())` → `pw_dir`); (3) each of
  `CLAUDE_CONFIG_DIR`, `PI_CODING_AGENT_DIR`, `PI_CODING_AGENT_SESSION_DIR`, `PI_CONFIG_DIR`, `XDG_DATA_HOME`,
  `XDG_CONFIG_HOME`, `XDG_CACHE_HOME`, `XDG_STATE_HOME` is unset or (canonically) inside the home; (4) `OMP_PROFILE`
  and `PI_PROFILE` are unset or plain names. As built (WP0): when the OS cannot tell the profile directory the gate
  refuses (fails closed), and the default data locations used while the variables are unset (`<home>/.claude`,
  `.claude.json`, `.pi`, `.omp`, `.config`, `.cache`, `.local` and their data subfolders,
  `qa::gate::DEFAULT_DATA_PATHS`) must canonically stay inside the home when they exist — a junction or symlink to
  real data is refused. Debug builds skip the gate only with `CHG_ALLOW_REAL_HOME=1` (the user's
  own choice); `--features qa` builds never skip it.
* Debug builds of `RealServices` also refuse to spawn a terminal whose program is a resolved `claude`/`pi`/`omp`
  unless `[gui] <prog>_path` is set to a harmless program inside the sandbox or `CHG_ALLOW_REAL_LAUNCH=1`.
* Procedure:
  1. `tools/qa/new-sandbox.ps1 -Source <scratchpad>/sandbox/home -Dest $env:TEMP\chg-sbx-<n>`: copies the corpus
     (never use the source in place), creates `<dest>\.qa-sandbox`, `<dest>\.config\claude-history\`,
     `<dest>\.local\state\`, and a `gui.toml` with `claude_path`/`pi_path`/`omp_path` pointing at a harmless
     recorder script in the sandbox.
  2. `tools/qa/run-gui.ps1 -Sandbox <dest> -Exe <path> [-Scenario x.qa -Out <dir>] [-Shots]` sets, for the child
     process only: `USERPROFILE`, `HOME` = `<dest>`; `CLAUDE_CONFIG_DIR` = `<dest>\.claude`; `PI_CODING_AGENT_DIR` =
     `<dest>\.pi\agent`; `XDG_*` under `<dest>`; removes `PI_CODING_AGENT_SESSION_DIR`, `PI_CONFIG_DIR`,
     `OMP_PROFILE`, `PI_PROFILE`; starts the watcher when `-Shots`; waits; prints the report.
  3. Semantic QA: the model downloads into the sandbox cache on first use unless `--qa-model-dir
     <scratchpad>\qa-model-cache` points at a pre-downloaded copy (feature `qa` only).
  4. Plain dev runs use the same script without `-Scenario`.

### 15.5 Screenshot QA procedure (Windows; macOS/Linux screenshots are D items)
For each scenario with `shot` steps: run with `-Shots` in light and dark (`set-theme`), collect PNGs, review against
the mockups in §5 and the checklist: TitleBar + menus visible; list rows show lane/title/meta/preview and highlights;
selected row fill; footer shows state and fitting hints; viewer lanes aligned (timestamp/label/separator/focus mark);
code highlighted; tables as rows; dialogs name their targets; light/dark contrast. Findings go into the hand-off note.

### 15.6 How FEATURES rows are verified
Each FEATURES row names exactly one owning WP and an "Acceptance" column with the code(s) and the concrete check.
The owning WP implements the check and makes it pass; a WP is done when all its rows pass (U/P/G in CI; Q/S evidence
in the hand-off note; R recorded by the integrator; D listed in the release checklist). Rows owned by WP-C are
verified by core tests that WP-C writes when missing.

### 15.7 Review checklist (R)
The integrator checks, per merge: (1) no string in the UI uses `...` or "session" for the list object (grep +
screenshots); (2) every icon-only button has a tooltip and accessible label; every row has an accessibility label;
focus is visible and restored after overlays; (3) tooltips/help texts for agent and time options equal ADD E4 texts;
(4) no file watcher, no polling loop besides the 30 s ticker; (5) non-goals untouched (no config.toml write, no skill
install); (6) the safety lint passes and no new allow-list entry was added without a reason; (7) the companion-skill
note and Windows build notes are in the README.

### 15.8 Performance targets and probes (goal 6)
Measured in release builds on the Windows dev machine by Q scenarios (`timing` steps; probes record UI-thread busy
time per handler with `Instant` and frame times with a canvas probe), with G tests asserting the structural
precondition (no O(corpus) work on the UI thread: e.g. K1/K2/K3 paths are async, the UUID query enters `Pending`).
* **List** (`perf-list.qa`, synthetic 20k-row sandbox via `synth::corpus`): UI-thread time ≤ 16 ms at load end
  (install), ≤ 16 ms at refresh end, ≤ 5 ms for rename apply, ≤ 5 ms for delete removal, ≤ 2 ms for a UUID paste,
  ≤ 2 ms per typed key; idle CPU ≈ 0 over 60 s (no timer besides the 30 s ticker).
* **Viewer** (`perf-viewer.qa`, synthetic 10k messages with 20 tool outputs of 5,000 lines, opened in Full mode):
  first content ≤ 3.5 s after open; install ≤ 50 ms UI time; page-through avg ≤ 12 ms, p95 ≤ 20 ms; idle frame ≤ 3 ms;
  toggle `t`/`T` ≤ 60 ms UI time; first search ≤ 60 ms; working set ≤ 400 MB, and within ±10% after opening 20
  conversations in a row (old documents released). 3k messages: page-through p95 ≤ 16 ms.
* **As built (Phase C)**: the list targets that span panes (load end, refresh end, rename apply, delete removal,
  UUID paste) and the idle check are measured by `cross-pane.qa` on 20k rows around the whole update an operation ran
  in (§17.11 "Phase C as built": `busy.*`, wall and UI-thread CPU, medians of several runs); the idle frame of
  `perf-viewer.qa` is the median of nine idle re-renders; the working set is read by the app itself
  (`process.*`: ≤ 400 MB in `perf-viewer.qa`, ±10 % after 20 conversations in `cross-pane.qa`) and
  `tools/qa/run-gui.ps1` prints the peak the OS reports.
* **As built (fix round)**: `perf-list.qa` holds PageDown and Down at the 33 ms key-repeat pace (`hold`) and also
  gates the whole main-window frame (`window.frame_p95` ≤ 16 ms over the last 512 frames; X-008);
  `perf-viewer-main.qa` gates the viewer's idle frame in the main window with 20k rows (median of nine < 12 ms; the
  probe spans the whole window's layout and paint there) and reports its page-through (V-094). Measured values and
  their stability: §17.11 "Fix round".

### 15.9 Deferred manual checks (D) — the user runs them before a release
* **Windows**: release zip on a clean VM starts (VC++ DLLs app-local); real terminal launch through Windows Terminal
  and through the console fallback with a harmless program override; `for /f %i in ('claude-history-gui --show-id')
  do echo %i` prints the id.
* **macOS (arm64)**: the `.app` opens (Gatekeeper right-click Open), Cmd shortcuts and the native menu work, a real
  resume in Terminal.app with a harmless override, login-shell PATH finds `claude` when launched from Finder, picker
  `--resume` from a terminal execs in place, light/dark screenshots.
* **Linux (x86_64)**: tarball runs on Ubuntu 22.04 (X11 and Wayland), a real resume through the first detected
  terminal, the portal save dialog, light/dark screenshots.
* **All OSes, first semantic search (S-030)**: with an empty GUI model cache and network access, run the first real
  semantic search; the model downloads into the GUI cache with no console output (the GUI passes `progress: false`),
  the toast/progress shows in the window, and the search completes. Then repeat offline: it uses the cache.
  `CLAUDE_HISTORY_GUI_ORT_SELFTEST=1 <exe>` prints `onnxruntime ok` in release builds with the bundled library (S-032).

### 15.10 Failure fixtures per OS (`test_support::fault`)
* `undeletable(path)`: Windows — open the file with `share_mode(0)` (deletion fails with a sharing violation) and keep
  the handle in the guard; Unix — `chmod 0o555` on the parent directory (restored on drop).
* `unwritable(path)`: Windows — set the file read-only attribute; Unix — `chmod 0o444` on the file.
* On Unix, tests that rely on permissions skip with a printed reason when running as root (`geteuid() == 0`).
* Missing/garbage files (no permissions involved) are used wherever they produce the same error path.

---

## 16. Build, CI, release, packaging (PD20)

### 16.1 Cargo
Root `Cargo.toml` (WP0, then integrator):
```toml
[workspace]
resolver = "3"
members = ["crates/core", "crates/gui"]
[workspace.dependencies]          # added to the existing list
gpui-kit = "=0.7.3"
async-channel = "2"
toml_edit = "0.25.17"             # core K12 (`toml_edit.workspace = true` in crates/core); same version as in gpui-kit's tree
ctor = "1.0"                      # dev only; 1.0.x is already in gpui-kit's tree
windows-sys = { version = "0.61", features = ["Win32_Foundation", "Win32_System_Console", "Win32_System_Threading", "Win32_Security", "Win32_Storage_FileSystem", "Win32_UI_Shell", "Win32_System_Com"] }
libc = "0.2"
embed-resource = "3"
[profile.dev]
debug = "line-tables-only"        # file:line in panics and backtraces; the release profile is unaffected
[profile.dev.package."*"]
debug = false                     # no debug info for dependencies
[profile.dev.package]
gpui-pre = { opt-level = 3 }
gpui-pre-platform = { opt-level = 3 }
gpui-pre-windows = { opt-level = 3 }
gpui-pre-macos = { opt-level = 3 }
gpui-pre-linux = { opt-level = 3 }
gpui-pre-wgpu = { opt-level = 3 }
gpui-base = { opt-level = 3 }
gpui-component = { opt-level = 3 }
gpui-kit = { opt-level = 3 }
gpui-kit-assets = { opt-level = 3 }
rustybuzz = { opt-level = 3 }
taffy = { opt-level = 3 }
ttf-parser = { opt-level = 3 }
resvg = { opt-level = 3 }
syntect = { opt-level = 3 }
[profile.release]
codegen-units = 4
```
(A `[profile.dev.package]` entry for a crate that is not in the target's graph only produces a cargo warning; the
exact crate names are re-checked by WP0 against the lock file.) As built: `Cargo.lock` keeps upstream's resolved
versions except where gpui-kit 0.7.3's tree forces a newer minimum of a package core shares (memchr 2.8.3,
regex-automata 0.4.18, futures-* 0.3.34, serde_spanned 1.1.2, toml_parser 1.1.5, toml_writer 1.1.3,
js-sys/web-sys 0.3.106, wasm-bindgen 0.2.129 (+ -macro, -macro-support, -shared), wasm-bindgen-futures 0.4.79;
recorded in the root `Cargo.toml`); every other shared package keeps upstream's version (`cargo update -p <name>
--precise <upstream version>`). `rust-toolchain.toml`: `channel = "1.99.0"`,
`components = ["rustfmt", "clippy"]`. `.cargo/config.toml`: `[env] RUST_MIN_STACK = "8388608"`; `/STACK:8000000`
comes from the build scripts (§14.4).

`crates/gui/Cargo.toml` (WP0; complete dependency list — WPs must not add crates without the integrator):
```toml
[package]
name = "claude-history-gui"
version = "0.1.0"
edition.workspace = true
license.workspace = true
repository.workspace = true
publish = false
build = "build.rs"

[lib]
name = "claude_history_gui"
path = "src/lib.rs"
doctest = false                      # (as built) doc tests would run outside the test env redirect (§15.2)

[[bin]]
name = "claude-history-gui"
path = "src/main.rs"

[features]
default = []
qa = []
release-dynamic-ort = ["claude-history-core/release-dynamic-ort"]

[dependencies]
claude-history-core = { path = "../core" }
gpui-kit.workspace = true            # default features (component, assets); syntect via core
async-channel.workspace = true
serde.workspace = true
serde_json.workspace = true
chrono.workspace = true
pulldown-cmark.workspace = true
unicode-width.workspace = true
home.workspace = true                # qa/gate.rs only (lint-allowed)

[target.'cfg(windows)'.dependencies]
windows-sys.workspace = true

[target.'cfg(unix)'.dependencies]
libc.workspace = true                # isatty/fstat, execvp, getpwuid_r, geteuid

[build-dependencies]
embed-resource.workspace = true

[dev-dependencies]
claude-history-core = { path = "../core", features = ["hermetic"] }
gpui-kit = { workspace = true, features = ["test-support"] }
tempfile.workspace = true
ctor.workspace = true
```

### 16.2 CI (`.github/workflows/ci.yml`, WP7)
* Triggers: push to main, pull requests; also `workflow_call` (used by release.yml).
* Matrix `os: [windows-latest, macos-latest, ubuntu-latest]`, `fail-fast: false`. Toolchain from
  `rust-toolchain.toml`. Linux: `sudo apt-get install -y gcc g++ clang libfontconfig-dev libwayland-dev
  libxkbcommon-x11-dev libx11-xcb-dev libssl-dev libzstd-dev libvulkan1 mesa-vulkan-drivers xvfb xterm`.
  `Swatinem/rust-cache@v2` (`cache-on-failure: true`); cache the ORT download dir per OS.
* Steps (all with `--locked`):
  1. `cargo fmt --all --check`
  2. `bash tools/ci/safety-lint.sh` — fails when, outside the allow-listed files: `from_process_env` (allowed:
     `main.rs`), `home::home_dir`/`dirs::home_dir`/`std::env::var(_os)?`/`current_dir(` (allowed: `main.rs`,
     `qa/gate.rs`, `services.rs`), `std::process::Command` (allowed: `services.rs`, `ops/console.rs`),
     `spawn_terminal`/`resume::launch(`/`check_latest_release` (allowed: `services.rs`), `execute_copy` (allowed:
     `ops/resume.rs`), `write_to_clipboard`/`read_from_clipboard`/`prompt_for_new_path`/`prompt_for_paths`/
     `reveal_path`/`open_url`/`open_with_system` (allowed: `services.rs`) appear in `crates/gui/src`. As built, the
     same rules run as the source test `skeleton_tests/safety_lint.rs`, whose allow lists are the reference: they
     also allow the WP0 guard tests in `skeleton_tests.rs` (`from_process_env`, `home::home_dir`, `env::var(_os)`,
     and — for X-036 — `terminal::spawn_terminal`, `spawn_terminal_checked`, `check_latest_release`) and
     `skeleton_tests/contracts.rs` (`read_from_clipboard`, proving the fake platform clipboard stays unused).
  3. `cargo clippy --workspace --all-targets -- -D warnings`
  4. `cargo clippy -p claude-history-gui --all-targets --features qa -- -D warnings`
  5. `cargo check -p claude-history-gui --features release-dynamic-ort`
  6. `cargo test -p claude-history-core` and `cargo test -p claude-history-core --features hermetic`
  7. `cargo test -p claude-history-gui`
  8. `cargo build -p claude-history-gui --release`
  9. Linux only, `continue-on-error: true`: `xvfb-run cargo run -p claude-history-core --example terminal_smoke --
     xterm` (K17; checks the marker file). Windows: same with `cmd` (console launcher).
* Before the gate is enabled for merges, WP7's first CI run on a branch reports pre-existing `fmt`/`clippy` failures
  of core on macOS/Linux; they are fixed through a CCR (§4.4).
* `qa.yml` (`workflow_dispatch`, windows-latest): build `--features qa --release`, copy
  `crates/gui/tests/fixtures/qa-home` into a temp sandbox, run every `qa/scenarios/*.qa` (no `--qa-model-dir`:
  semantic scenarios are skipped there), upload `qa-out/` (reports; screenshots SKIP without a watcher).

### 16.3 Release (`.github/workflows/release.yml`, WP7)
* Triggers: tag `v*` and `workflow_dispatch` with input `dry_run` (default true: builds and uploads workflow artifacts,
  creates no release). Job 0 calls `ci.yml` (`workflow_call`) and the build jobs `needs` it. The tag's version must
  equal `crates/gui/Cargo.toml` (job fails otherwise).
* Build jobs (frozen artifact names; core's `ReleaseInfo::asset_for(os, arch)` matches them):

| Runner | Target | ORT | Artifact |
|---|---|---|---|
| `windows-latest` | `x86_64-pc-windows-msvc` | static (`ort-download-binaries`), DirectML delay-loaded | `claude-history-gui-v<ver>-windows-x86_64.zip`: `claude-history-gui.exe`, VC++ runtime DLLs (via `vswhere`), `README.txt`, `LICENSE`, `LICENSE-MIT` |
| `macos-15` | `aarch64-apple-darwin` | `--features release-dynamic-ort`; official ONNX Runtime 1.24.2 `osx-arm64` (URL + sha256 as in upstream `release.yml`) → `Contents/MacOS/lib/` | `claude-history-gui-v<ver>-macos-aarch64.zip` (`ditto -c -k --keepParent "Claude History.app"`, ad-hoc signed) |
| `ubuntu-22.04` | `x86_64-unknown-linux-gnu` | `--features release-dynamic-ort`; official 1.24.2 `linux-x64` → `lib/` | `claude-history-gui-v<ver>-linux-x86_64.tar.gz` (binary, `lib/`, `.desktop`, icon, licenses) |

  Plus `<artifact>.sha256` for each. Intel macOS is not built in v1 (ort-sys has no static ORT for
  `x86_64-apple-darwin`; the `macos-13` image is retired); a follow-up may copy upstream's `macos-15-intel`
  source-build job (`ORT_SOURCE_COMMIT`).
* Windows job runs `tools/ci/check-delayload.ps1` (`dumpbin /imports` shows `DirectML.dll` under delay-load imports;
  fails otherwise).
* Release notes: the `## v<ver>` section of `CHANGELOG.md` extracted to a file and passed as `body_path` to
  `softprops/action-gh-release@v2` (it does not read tag messages).

### 16.4 Windows build notes (README section, WP7)
MSVC toolchain + VS 2022 C++ workload + Windows SDK + CMake; network on first build (ort-sys static ONNX Runtime
download); 8 MB main-thread stack (build scripts); debug builds show a console and run the sandbox gate (set
`CHG_ALLOW_REAL_HOME=1` to use your real history); the exe needs the VC++ runtime (bundled in the release zip);
DirectML is delay-loaded.

---

## 17. Work breakdown

Phases (§18): **Gate 0** core workflow's last merge → **Phase 0**: WP0 (skeleton) ∥ WP-C (core gaps) → **Phase A**:
WP1 (list) ∥ WP2 (viewer) ∥ WP6 (settings, keymap, menus, palette, theme, persistence) ∥ WP7 (platform, startup,
CLI, services, embedder, CI, release, QA runner) → **Phase B**: WP3 (session ops, picker) ∥ WP4 (Deep Search, agent
panel) ∥ WP5 (tools) → **Phase C**: WP-I (integration, cross-pane rows) ∥ WP-C3 (Phase-B CCRs) ∥ WP-L (cross-platform
verification, CI) → release.

Each package's FEATURES rows are exactly the rows whose WP column names it (the index at the end of FEATURES.md is
authoritative). Phase-A packages never need another Phase-A package's real behavior in their tests: they test their
own entity in isolation (events are asserted, collaborators are WP0 stubs). Phase-B packages build on the merged
Phase-A code. Cross-pane rows belong to WP-I.

General rules for every WP:
* Own only the files listed (§6). A change in someone else's file → "requested change" in the hand-off note; a core
  change → CCR (§4.4). Never copy core logic into the GUI.
* Use only gpui-kit 0.7.3 APIs verified in the registry sources (GK-INFRA §0); never HEAD-only test helpers.
* No raw colors outside `theme_tokens.rs`; rem sizing; stable domain ids; `render()` pure; tasks stored on owners;
  revision guards (DG §1); every side effect through `Services`.
* Tests per §15 (hermetic harness); every failure-path test uses `test_support::fault` (§15.10).
* **Definition of done (common)**: `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings`
  and with `--features qa`; `cargo test -p claude-history-core` (+ `--features hermetic`) and `-p claude-history-gui`
  green on Windows; the safety lint passes; every FEATURES row of the WP passes its Acceptance check (Q/S evidence
  paths in the hand-off note; D items listed, not blocking); the hand-off note lists verified rows, requested
  changes, CCRs and deviations. Dead code is not an issue (library crate, §3); no crate-level `allow(dead_code)`.

### 17.1 WP0 — Skeleton (one agent; Phase 0, parallel with WP-C)

Goal: the crate compiles as lib + bin; the main window opens with TitleBar + menus + left tabs + placeholder list /
viewer / deep-search panes; every action is defined, bound and routed; every interface of §17.3–§17.10 exists with its
final signature (stub body); the hermetic test harness works.

Precondition: Gate 0. References only **landed** core APIs (§4.1). Frozen signatures that would need a K-item type
avoid it (e.g. `PlatformServices::spawn_terminal` returns `Result<Option<i32>, String>`: `None` = still running).

Files and content:
1. Root `Cargo.toml` (members + workspace deps + profiles, §16.1; `toml_edit` is added by WP-C to the core manifest
   only), `rust-toolchain.toml`, `.cargo/config.toml` `[env]`, `crates/gui/Cargo.toml` (§16.1), `crates/gui/build.rs`
   (Windows MSVC DirectML delay-load only, as WP0 built it; it later also sets `/STACK`, §14.4, as does
   `crates/core/build.rs`).
2. `src/main.rs` (minimal: parse with `core::settings::parse_launch_args`, Help/Version/errors to the console via the
   `ops/console.rs` stub, `HistoryEnv::from_process_env()` + `env_path_base` left `None` until K16 lands,
   `claude_history_gui::run`), `src/lib.rs` (module tree, `run`, `run_with_services`). Integrated state: the
   integrator set `env_path_base` = launch cwd and made the `LaunchCommand` match exhaustive (§3; WP7 completes it,
   §17.11).
3. `src/app.rs` (frozen): `run`, `init` (§7.3 order, calling the owner stubs), `open_first_window`,
   `#[cfg(test)] init_for_test`.
4. `src/actions.rs` (frozen): exactly §8.2.
5. `src/model.rs` (frozen): every type of §7.9 with constructors (`ConversationTarget::{from_conversation,
   from_agent_info, from_parsed_file, describe, header_id_differs, can_modify}`, `HeaderInfo::{from_conversation,
   stem_only}`, `KeyAction::label`, `StartupTool`, …).
6. `src/globals.rs` (frozen): §7.3 globals with accessors; `OpenTargets::{register, unregister, close_for}`;
   `SemanticGate::{enter_exclusive, leave, is_open}` (calls the pane hooks through `MainPanes`); `CorpusBus` emitter.
7. `src/services.rs`: the frozen trait (§7.4); `RecordingServices` complete; `RealServices` complete for reveal/open/
   clipboard/prompts (gpui calls), `spawn_terminal` → core `terminal::spawn_terminal` → `Ok(None)`, console methods →
   `ops::console` stubs, `fetch_latest_release` → core `update::check_latest_release`, `login_shell_path` → `None`.
8. `src/notify.rs` (incl. `gui_text` mapping, `not_available`), `src/exit.rs`, `src/widgets/{mod, confirm, path_list,
   text_view}.rs` (frozen, complete); `widgets/time_range.rs` stub.
9. `src/keymap.rs`: complete static table (§8.3) incl. `ConversationList > Input` and `Workspace > Input` duplicates,
   input-editing table (§8.4), `install_all`, `user_bindings`, `install_user_bindings` (UserBindingSet logic),
   `binding_labels`, `help_rows()` with all rows of §8.7.
10. `src/menus.rs`: the complete single tree (§8.10), `menu_tree`, `install`, `rebuild`; `src/palette.rs`: `open`
    building a `Command` from the tree (no enabled/check refinements yet).
11. `src/theme_tokens.rs`: `Roles` (kit tokens + core roles, §5 intro), `roles(cx)`, `apply_theme`,
    `apply_startup_theme`, `follow_system`. `src/persist.rs`: `UiState` exactly §13.3 + stubs.
12. `src/embedder.rs`, `src/debug_sink.rs`, `src/qa_ext.rs`: stubs with final signatures (refusing embedder,
    `install_fake`; sink stub forwards to `DebugBuffer`).
13. `src/workspace/mod.rs` (complete, then WP-I): layout §5.1 with min widths and clamping (PD1), tab switching focus,
    event subscriptions §7.2 (preview debounce, flush, CorpusChanged, CorpusNotice, OpenHit/Reveal routing),
    focus rules §7.7 (focus-lost fallback, ActivePane, F6 regions), routing via `routing.rs`, should-close → quit,
    `MainPanes` set, `qa_probe` aggregation. `workspace/routing.rs` (frozen): the fallback table of §8.1.
    `workspace/title_bar.rs`, `workspace/startup.rs` stubs (FILE positional + `--workspace` only).
14. Pane stubs with final signatures (§17.3–§17.9): `list/mod.rs` (renders the search `Input` with the `❯` prefix,
    toolbar placeholders and an `Empty`), `viewer/mod.rs` ("No conversation selected" or the target path; `Viewer`
    and `AgentPanel` sibling contexts), `single_file.rs` (`open_window`, `SingleFileWindow` root), `deep_search/*`,
    `ops/*`, `tools/*`, `settings/*`, `help/*`, `qa/*` (stub bodies call `notify::not_available`).
15. `src/test_support/mod.rs` (frozen, §15.2) incl. the ctor env redirect, `fault`, `synth`, `global_lock`, and tests:
    `skeleton_opens_and_focuses_search`, `every_action_builds_by_name`, `static_keymap_installs_without_panic`,
    `menu_tree_builds`, `quit_action_quits_even_with_dialog_focus`, `ctrl_w_in_input_deletes_word`,
    `enter_moves_focus_to_viewer_stub`, `hermetic_guards_active` (calling core `from_process_env` panics inside a
    test), `env_redirect_points_into_temp`. Integrated state: `hermetic_guards_active` runs (the integrator enabled
    `claude-history-core/hermetic` in `[dev-dependencies]`) and also checks that the update check panics, terminal
    spawning returns `Unsupported` and the fastembed factory/`FastembedEmbedder::new_in` refuse.

WP0 FEATURES (index): L-086, L-087, L-088, A-053, C-056, C-063, C-064, X-036.
WP0 DoD: common DoD + `cargo build --release -p claude-history-gui` + a sandbox run through hand-set env vars with a
screenshot via `scratchpad/tools/shot.ps1` showing title bar, menus, both panes (debug builds run the gate, so the
run proves the gate passes on a sandbox).

### 17.2 WP-C — Core gaps (crates/core only; Phase 0, parallel with WP0)
Precondition: Gate 0. Owns: additions in `crates/core/**` limited to K1–K18 (§4.3); adds `toml_edit` to
`crates/core/Cargo.toml` and `examples/terminal_smoke.rs`. Provides: exactly the signatures of §4.3.
Also owns the verification of every FEATURES row whose WP is WP-C: the row names the core test; WP-C confirms it
exists and passes or writes it.
Tests: per K item (§4.3), plus `cargo test -p claude-history-core --features hermetic`.
DoD: common DoD (core part); breaking change K1 updates every core caller and test; hand-off lists each K item with
its tests and the WP-C FEATURES rows with their test names. Later CCRs run as WP-C2 tasks with the same DoD.

### 17.3 WP1 — Conversation list (Phase A)
Owns: `crates/gui/src/list/*`, `widgets/time_range.rs`, `test_support/fixtures_list.rs`, `tests/fixtures/list/`,
`qa/scenarios/{startup-load,list-search,semantic-toggle,perf-list}.qa`.
Consumes: core `browser` (§4.1 + K1, K2, K3 `apply_renamed`, K4 `remove_paths`, K6, K18), `display_format`
(`build_row_header`, `build_row_preview`, `HighlightQuery`), `time_filter`, `history::load_session_by_uuid` (K2);
GUI `AppState`, `Services`, `notify`, `embedder` (browser factory), `theme_tokens::roles`, `keymap` (hints via
`Kbd`), `qa_ext`, `model`, `exit::quit_without_output` (picker Esc).
Provides (final signatures; WP0 stubs them):
```rust
pub enum ListPaneEvent { SelectionChanged { target: Option<ConversationTarget>, user: bool }, OpenRequested { target: ConversationTarget, focus_viewer: bool, agent_tab: Option<AgentTab> }, CorpusChanged, LoadStateChanged }
impl EventEmitter<ListPaneEvent> for ListPane {}
impl ListPane {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self;
    pub fn focus_search(&self, window: &mut Window, cx: &mut App);
    pub fn focus_toolbar(&self, window: &mut Window, cx: &mut App);             // (as built) F6 region
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool;
    pub fn toolbar_contains_focus(&self, window: &Window, cx: &App) -> bool;    // (as built)
    pub fn query(&self, cx: &App) -> SharedString;                              // (as built)
    pub fn selected_target(&self) -> Option<ConversationTarget>;
    pub fn conversation(&self, path: &Path) -> Option<Arc<Conversation>>;
    pub fn header_info(&self, path: &Path) -> Option<HeaderInfo>;
    pub fn select_path(&mut self, path: &Path, cx: &mut Context<Self>) -> bool;
    pub fn is_loading(&self) -> bool;
    pub fn mode(&self) -> ListSearchMode;
    pub fn scope_is_workspace(&self) -> bool;
    pub fn has_workspace(&self) -> bool;
    pub fn refresh(&mut self, cx: &mut Context<Self>);
    pub fn apply_renamed(&mut self, path: &Path, renamed: Conversation, cx: &mut Context<Self>) -> Result<(), RenameError>;
    pub fn remove_paths(&mut self, paths: &[PathBuf], cx: &mut Context<Self>);
    pub fn set_workspace(&mut self, workspace: Option<WorkspaceScope>, cx: &mut Context<Self>);   // reload ApplyPartial, revert on failure
    pub fn set_time_filter(&mut self, filter: TimeFilter, label: Option<String>, cx: &mut Context<Self>);
    pub fn set_exclude_projects(&mut self, excluded: Vec<String>, cx: &mut Context<Self>);
    pub fn suspend_semantic(&mut self, cx: &mut Context<Self>);
    pub fn resume_semantic(&mut self, cx: &mut Context<Self>);
    pub fn project_paths(&self) -> &[PathBuf];                    // cached, computed off-thread
    pub fn choose_workspace_folder(&mut self, window: &mut Window, cx: &mut Context<Self>);
    pub fn show_semantic_details(&mut self, window: &mut Window, cx: &mut Context<Self>);
    pub fn handle_forwarded(&mut self, action: &dyn Action, window: &mut Window, cx: &mut Context<Self>) -> bool;
    #[cfg(any(test, feature = "qa"))] pub fn qa_probe(&self, key: &str, cx: &App) -> Option<String>;
}
```
List-local handlers: SelectPrev/Next/PageUp/PageDown/HalfPageDown/First/Last, ClearQuery, ToggleScope (propagate
without workspace), ToggleSearchMode, SetMode*/SetScope*, OpenRowContextMenu (NativeMenu, row-menu tokens), RowMenu
items Open/Copy*/Reveal/Search within/Outline are dispatched as the corresponding session actions with the captured
target through `ops` (stubs until WP3), the picker banner.
Tests (G unless noted): loading stream/order/events/empty/failed with recovery buttons; async preparation keeps the UI
responsive (G: poll returns while preparing); search typing + generations; UUID found/pending/not found; scope toggle +
disabled without workspace + Tab propagation; time filter preset/custom/errors + revert on failed reload; workspace
change revert; exclusions; literal mode row height (P for the predicate); footer state/hints priority and dimming;
refresh outcomes (toasts exact); semantic flows with a fake embedder (activity text, fallback, failed → error toast +
recovery actions, suspended → `sem paused`); Ctrl+S details exact lines; paste sanitation; row menu (pointer +
Shift+F10) dispatches with the captured target after the selection changed; keyboard map (home/end with Input focus,
emacs keys incl. ctrl-f MoveRight when fork is rebound); preview-related event emission; P tests for the row layout
and the preview width computation.
DoD: common + Q `startup-load.qa`, `list-search.qa`, `semantic-toggle.qa`, `perf-list.qa` pass; S light/dark list
screenshots reviewed.

### 17.4 WP2 — Viewer and single-file window (Phase A)
Owns: `crates/gui/src/viewer/*`, `crates/gui/src/single_file.rs`, `test_support/fixtures_viewer.rs`,
`tests/fixtures/viewer/`, `qa/scenarios/{viewer-basics,viewer-search-nav,single-file,perf-viewer}.qa`.
Consumes: core `view` (§4.1 + K5, K9), `markdown::expand_tabs` (K10), `syntax`, `display_format` (header pieces),
`history::process_conversation_file`, `export::ExportOptions`; GUI `model`, `notify`, `services`, `theme_tokens`,
`keymap`, `qa_ext`, `deep_search::panel::AgentPanel` (constructed as the sibling element; stub until WP4), `ops::*`
stubs (copy/export/rename/delete/resume entry points, real in WP3).
Provides:
```rust
pub enum ViewerPaneEvent { LeaveRequested, TargetChanged(Option<ConversationTarget>) }
impl EventEmitter<ViewerPaneEvent> for ViewerPane {}
impl ViewerPane {
    pub fn new(mode: ViewerMode, window: &mut Window, cx: &mut Context<Self>) -> Self;
    pub fn mode(&self) -> ViewerMode;                                           // (as built)
    pub fn open(&mut self, req: OpenRequest, window: &mut Window, cx: &mut Context<Self>);   // honours req.agent_tab
    pub fn clear(&mut self, cx: &mut Context<Self>);
    pub fn target(&self) -> Option<&ConversationTarget>;
    pub fn prefs(&self) -> Option<ViewPrefs>;                     // for menu check marks
    pub fn focus(&self, window: &mut Window, cx: &mut App);
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool;
    pub fn set_header(&mut self, info: Option<HeaderInfo>, cx: &mut Context<Self>);
    pub fn export_options(&self) -> Option<ExportOptions>;
    pub fn apply_theme(&mut self, cx: &mut Context<Self>);
    pub fn agent_panel(&self) -> &Entity<AgentPanel>;
    pub fn open_agent_panel(&mut self, tab: AgentTab, window: &mut Window, cx: &mut Context<Self>);
    pub fn reveal(&mut self, reveal: RevealRequest, window: &mut Window, cx: &mut Context<Self>) -> bool;
    pub fn handle_forwarded(&mut self, action: &dyn Action, window: &mut Window, cx: &mut Context<Self>) -> bool;
    #[cfg(any(test, feature = "qa"))] pub fn qa_probe(&self, key: &str, cx: &App) -> Option<String>;
}
pub fn open_window(path: PathBuf, cx: &mut App) -> AnyWindowHandle;          // single_file.rs
impl SingleFileWindow { pub fn new(path, window, cx) -> Self; pub fn path(&self) -> &PathBuf; pub fn viewer(&self) -> &Entity<ViewerPane>; }
```
Viewer-local handlers: every viewer row of §8.3; session actions (Resume, Fork, Rename, Delete, Export, Copy*, Show
path, Reveal) resolve the viewer target and call the `ops` entry points.
Tests: P for `segments` (every markdown rule of §9.3 incl. tabs, table rows), `items` (label table, lanes, ids stable
across rebuild, chunking, visual-row truncation with K9), header builder (omission rules), search over items incl.
hidden text, splice-diff; G for open/install/stale generation (rapid opens abandon work), toggles with anchor kept,
width change keeps anchor, expansion click, nav J/K, search typing/commit/next/prev/wrap/auto-expand, Esc order, `q`,
SmartCopy incl. partial-copy toast, link dialog, single-file window (disabled actions, close on Esc, error file,
out-of-root agent panel disabled once WP4 lands → WP-I), deleted-file toggles (V-002).
DoD: common + Q `viewer-basics.qa`, `viewer-search-nav.qa`, `single-file.qa`, `perf-viewer.qa`; S light/dark viewer
screenshots (code, tables, tool output, thinking, subagent).

### 17.5 WP6 — Settings, keymap, menus, palette, theme, persistence, shortcuts (Phase A)
Owns: `crates/gui/src/{keymap.rs, menus.rs, palette.rs, theme_tokens.rs, persist.rs}`, `workspace/title_bar.rs`,
`settings/*`, `help/*`, `qa/scenarios/{settings-keys,theme,palette}.qa`.
Consumes: core `config` (`load_*`, K12 `save_gui_config_changes`, `parse_key_binding`, `KeyChord`, `KeyBindings`),
`theme`, `settings::effective_settings`; GUI `AppState`, `MainPanes`, `ActivePane`, typed pane getters
(`ListPane::{mode, scope_is_workspace, has_workspace, is_loading}`, `ViewerPane::prefs`), `notify`.
Provides (same signatures as WP0's stubs): validated `install_user_bindings`, `binding_labels`, `help_rows`;
`pub fn validate_keys(keys: &KeysConfig) -> KeyValidation`; `menus::{menu_tree, install, rebuild}` with check states;
`palette::open` with enabled/check states and focus restore; `theme_tokens::set_theme_preference(pref, save: bool,
window, cx)`; `persist::{load, schedule_save, flush}`; `pub fn open_settings(page: Option<SettingsPage>, window, cx)`;
`pub fn open_shortcuts(section: Option<HelpSection>, window, cx)`.
Tests: P persistence roundtrip/corrupt/clamp/v1 upgrade, key validation messages, registration order (C-028), contrast
of every role pair in both themes (X-039); G table-driven keymap test (every static row dispatches from its context,
C-052), live rebinding keeps F5 and static rows, Ctrl+T conflict, settings save writes only changed keys
(config.toml byte-identical; comments preserved; a View-menu theme change during an open Settings dialog survives),
gui.toml parse error blocks Save and theme saving, theme switch (core kind + kit mode + menu check), palette lists
items with live shortcuts and disabled states and dispatches to the stored focus, shortcuts dialog rows and rebinding
reflected, title bar.
DoD: common + Q `settings-keys.qa`, `theme.qa`, `palette.qa`.

### 17.6 WP7 — Platform, startup, CLI, services, embedder, packaging, CI, QA infrastructure (Phase A)
Owns: `crates/gui/{build.rs, assets/*}`, `src/{main.rs, services.rs (RealServices), embedder.rs, debug_sink.rs,
qa_ext.rs}`, `workspace/startup.rs`, `ops/console.rs`, `qa/*`, `qa/scenarios/smoke.qa`, `tests/fixtures/qa-home/`,
`.github/workflows/*`, `packaging/*`, `tools/qa/*`, `tools/ci/*`, repo `README.md` (GUI section).
Consumes: core `settings` (+K14, K15), `agent::execute` (headless), `maintenance::show_dir`, `export` (headless
render), `update`, `resume::terminal` (+K17), `env` (+K16), `semantic::fastembed`, `debug`; all panes' `qa_probe`.
Provides: `main` (incl. headless paths §11.23, time-filter error exit 1); `startup::{to_launch_context, load_config,
resolve_workspace, resolve_initial_view, run_startup_tool, show_config_errors}`; `ops::console::{stdout_kind,
ensure_console, write_stdout, write_stderr}`; complete `RealServices` (spawn via `spawn_terminal_checked`, debug-build
guard, `exec_in_current_terminal` Unix exec, `login_shell_path`); `embedder::{install, install_fake, browser_factory,
agent_factory, ops_factory, drop_model}`; `debug_sink::install`; the QA runner (§15.3) + gate (§15.4); CI/release
(§16); icons + `assets/windows/app.rc` + build.rs resource compile; macOS bundle script; Linux `.desktop`.
Tests: P launch mapping (`GuiLaunchArgs` → `LaunchContext`/`StartupTool`), PD3 workspace resolution table (incl. root
/home/exe-dir exclusions, Pi/OMP cwd, `-L`), PD17 initial view resolution table (C-066), console kind logic with
mocked handles, login-shell marker parsing; G startup tool mapping calls the right `tools::*` stub, config error
dialogs per file, embedder sharing (one model instance across browser/agent/ops adapters with a counting fake);
headless agent/show-dir/render via a test of the `main` helper functions with recorded stdout; QA runner passes
`smoke.qa` on the sandbox; CI workflows run green on a branch; `release.yml` `workflow_dispatch` dry run produces the
three artifacts.
DoD: common + CI green on 3 OSes + dry-run artifacts downloadable + `smoke.qa`; D items (§15.9) listed for the user.

### 17.7 WP3 — Session actions, export/copy, picker (Phase B)
Owns: `crates/gui/src/ops/{resume, rename, delete, export, copy, select, tests}.rs`,
`test_support/fixtures_ops.rs`, `tests/fixtures/ops/`,
`qa/scenarios/{rename,delete,resume-fork,export-copy,picker,picker-exit}.qa`.
Consumes: core `resume` (`plan_resume`, `ResumePlan`, `CopyPlan`, `ProgramOverrides`, `terminal::{launcher_candidates,
prepare_launcher_command, LaunchOptions}`), `history::rename_session` (K3),
`maintenance::{plan_session_delete, execute_session_delete, SessionDeleteFailure, delete_by_uuid_message}` (K4), `export::*`,
`settings::{picker_output, EffectiveSettings}`, `export::display_parse_errors` (K11), `maintenance::log_parse_errors`;
GUI `ListPane::{apply_renamed, conversation, selected_target}`, `ViewerPane::{target, export_options}`,
`Services`, `notify`, `exit`, `widgets::confirm`, `OpenTargets`, `CorpusBus`, `persist` (last export dir).
Provides:
```rust
// ops/resume.rs
pub fn resume(target: ConversationTarget, fork: bool, window: &mut Window, cx: &mut App);
pub fn plan_and_confirm(target: ConversationTarget, fork: bool, window: &mut Window, cx: &mut App,
                        then: Box<dyn FnOnce(Result<ResumePlan, String>, &mut Window, &mut App)>);
pub fn launch_in_new_terminal(plan: &ResumePlan, title: SharedString, window: &mut Window, cx: &mut App);
// ops/rename.rs
pub fn open_rename_dialog(target: ConversationTarget, window: &mut Window, cx: &mut App);
// ops/delete.rs
pub fn open_delete_dialog(target: ConversationTarget, window: &mut Window, cx: &mut App);   // uses widgets::PathList
// ops/export.rs
pub fn open_export_menu(target: ConversationTarget, opts: ExportOptions, window: &mut Window, cx: &mut App);
pub fn open_copy_menu(target: ConversationTarget, opts: ExportOptions, window: &mut Window, cx: &mut App);
pub fn copy_as(target: ConversationTarget, format: ExportFormat, opts: ExportOptions, window: &mut Window, cx: &mut App);
// ops/copy.rs
pub fn copy_path(target: &ConversationTarget, window: &mut Window, cx: &mut App);
pub fn copy_session_id(target: &ConversationTarget, window: &mut Window, cx: &mut App);
pub fn copy_header_session_id(target: &ConversationTarget, window: &mut Window, cx: &mut App);
pub fn show_path(target: &ConversationTarget, window: &mut Window, cx: &mut App);
pub fn reveal(target: &ConversationTarget, window: &mut Window, cx: &mut App);
pub fn copy_message(target: &ConversationTarget, text: Result<String, String>, window: &mut Window, cx: &mut App);
// ops/select.rs
pub fn select_and_exit(target: Option<ConversationTarget>, window: &mut Window, cx: &mut App);
```
Tests: G resume/fork recorded terminal commands for Claude same-project, cross-project (dialog + copy + overwrite
warning, Enter ignored with overwrites), self-copy never planned, Pi, OMP, missing program, missing workspace, spawn
error → next candidate, early non-zero exit toast; rename success/error/empty title/target left the list; delete plan
listing, y/Y/n/N, Enter ignored, A1/A2 race tests (rename, delete, resume, fork, export, copy — list and viewer
entry points), unknown source disabled, failure toast + debug.log line (`fault::undeletable`), viewer cleared and
other dialogs closed via CorpusBus; export menu keys + save prompt answers + extension/replace prompt + portal error
fallback + file content prefix per format; copy-as clipboard; copy message (normal, summary-group quirk, missing
entry); copy path/id/header id; show path; reveal recorded; picker ShowPath/ShowId/Resume with stdout kinds,
non-UTF-8 stem, Ctrl+O print-and-exit with each stdout kind, `--plain`, `--debug` diagnostics order.
DoD: common + Q `rename.qa`, `delete.qa`, `resume-fork.qa`, `export-copy.qa`, `picker.qa`, `picker-exit.qa` (evidence:
exit code 0 and exactly one printed line, see `qa.yml`); D real-terminal checks
listed (§15.9).

### 17.8 WP4 — Deep Search and agent panel (Phase B)
Owns: `crates/gui/src/deep_search/*`, `test_support/fixtures_agent.rs`, `tests/fixtures/agent/`,
`qa/scenarios/{deep-search,agent-panel}.qa`.
Consumes: core `agent` (`AgentService` + `with_cancellation` K7, `respond`, `AgentResponse`, `AgentResult`,
`AgentConversationInfo`, `refs::conversation_key_for_path` K8, args), `time_filter`, `view` K5 (through
`ViewerPane::reveal`); GUI `AppState` (env with workspace, effective config), `embedder::agent_factory`,
`SemanticGate`, `widgets::{time_range, text_view}`, `notify`, `tools::open_generate_semantic_cache`.
Provides:
```rust
pub enum DeepSearchEvent { OpenHit { target: ConversationTarget, reveal: RevealRequest, focus_viewer: bool, agent_tab: Option<AgentTab> } }
impl DeepSearchPane {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self;
    pub fn focus_query(&self, window: &mut Window, cx: &mut App);
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool;
    pub fn selected_hit_target(&self, cx: &App) -> Option<ConversationTarget>;
    pub fn pause_semantic(&mut self, paused: bool, cx: &mut Context<Self>);
    #[cfg(any(test, feature = "qa"))] pub fn qa_probe(&self, key: &str, cx: &App) -> Option<String>;
}
pub enum AgentPanelEvent { Reveal { target: ConversationTarget, reveal: RevealRequest } }
impl AgentPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self;
    pub fn set_viewer_target(&mut self, target: Option<ConversationTarget>, cx: &mut Context<Self>);
    pub fn show_tab(&mut self, tab: AgentTab, window: &mut Window, cx: &mut Context<Self>);
    pub fn mark_deleted(&mut self, paths: &[PathBuf], cx: &mut Context<Self>);
    // (as built) also: selected_target(), is_visible(), tab(), hide(cx), contains_focus(window, cx)
}
pub struct AgentWorkerHandle;                                     // global; created lazily (one worker per process)
impl AgentWorkerHandle { pub fn global(cx: &mut App) -> AgentWorkerHandle; pub fn client(&self) -> AgentClient;
                         pub fn pause_semantic(&self, paused: bool); pub fn is_semantic_paused(&self) -> bool; }
pub struct AgentClient;                                           // (as built) one per consumer; keep it on the entity
impl AgentClient { pub fn id(&self) -> u64; pub fn submit(&self, job: AgentJob) -> u64; pub fn cancel(&self, kind: AgentJobKind);
                   pub fn replies(&self) -> async_channel::Receiver<AgentReply>;
                   pub fn reply_sender(&self) -> async_channel::Sender<AgentReply>; pub fn is_semantic_paused(&self) -> bool; }
pub struct AgentJob { pub kind: AgentJobKind, pub request: AgentRequest, pub env: HistoryEnv, pub config: ConfigFile }
pub enum AgentJobKind { Search, Within, Read, Outline, KeyForPath(PathBuf) }
pub enum AgentReply { Progress { kind: AgentJobKind, generation: u64, notice: AgentNotice },
                      Done { kind: AgentJobKind, generation: u64, response: AgentResponse },
                      Key { generation: u64, path: PathBuf, full_ref: Option<String> } }
```
Tests: P form → args mapping (scope Default/All/Workspace, mode, budget, flat, hits, duplicates, time) and validation;
G with the sandbox corpus: lexical search groups/hits render, flat mode, exact-forced hint, agent text toggle equals
`AgentResponse::text()`, typed error alert (invalid ref, ambiguous candidates clickable), warnings banner
(malformed fixture), uncached banner (fake embedder, empty cache), cancellation (fake slow embedder → Cancel →
`Search cancelled`; a new query cancels the old), paused under the semantic gate, open hit → `OpenHit` with the
expected ordinal and target from AgentConversationInfo, Within/Outline/Read incl. budget controls, visibility OR with
config, 25-message outline segments, Lines/Match slices, multi-ref Read grouped by conversation, stale results after a
viewer change still reveal in their own conversation, out-of-root file → panel disabled.
DoD: common + Q `deep-search.qa`, `agent-panel.qa`.

### 17.9 WP5 — Tools and maintenance (Phase B)
Owns: `crates/gui/src/tools/*`, `test_support/fixtures_tools.rs`, `tests/fixtures/tools/`,
`qa/scenarios/tools.qa`.
Consumes: core `maintenance` (+K4), `semantic_ops`, `update` (via services), `history`, `debug_log`; GUI `MainPanes`
(list refresh/suspend), `SemanticGate`, `embedder::{ops_factory, drop_model}`, `single_file::open_window`,
`Services`, `notify`, `DebugBufferHandle`, `widgets::{confirm, path_list, text_view}`, `CorpusBus`,
`startup::run_startup_tool` (tests of C-013…C-015 call it).
Provides:
```rust
pub fn open_delete_empty(local: bool, window: &mut Window, cx: &mut App);
pub fn open_generate_semantic_cache(local: bool, window: &mut Window, cx: &mut App);
pub fn open_clear_semantic_cache(window: &mut Window, cx: &mut App);
pub fn open_debug_search(prefill: Option<String>, local: bool, window: &mut Window, cx: &mut App);
pub fn open_semantic_debug(prefill: Option<String>, mode: SemanticReportMode, local: bool, window: &mut Window, cx: &mut App);
pub fn open_debug_log(tab: DebugLogTab, window: &mut Window, cx: &mut App);
pub fn show_projects_directory(window: &mut Window, cx: &mut App);
pub fn open_delete_by_id(prefill: Option<String>, window: &mut Window, cx: &mut App);
pub fn open_jsonl_file_dialog(window: &mut Window, cx: &mut App);
pub fn check_for_updates(window: &mut Window, cx: &mut App);
pub fn open_about(window: &mut Window, cx: &mut App);
```
Tests: G per dialog with sandbox fixtures and fake embedders (`embedder::install_fake`); delete-empty with recent
files, confirm keys, partial failure (`fault::undeletable`), broadcast of deleted paths; generate/clear under the
semantic gate (list worker suspended and resumed, `DeepSearchPane::pause_semantic` requested, Clear succeeds after a
semantic list search with the shared model dropped); debug search on a fresh load with the launch time filter and `local`; update check with
scripted `set_release` results for every `UpdateStatus` and no-asset; file prompts via `answer_open`; startup tools
open the right dialog prefilled.
DoD: common + Q `tools.qa`.

### 17.10 WP-I — Integration (Phase C)
Owns: `workspace/mod.rs` (from WP0, after Phase B merges), `src/integration_tests/*`, `qa/scenarios/cross-pane.qa`;
applies queued requested changes to frozen files.
Rows (index): preview debounce and flush (L-085), list ↔ viewer round trip (L-098), header live update (V-007),
SmartCopy routing (V-103), menu dispatch equals key dispatch (A-055), Deep Search open-at-hit end to end (D-011), region cycling (C-058), main-window close and
Show conversations (C-065), tool deletions across panes (T-023), A1/A2 through menus, palette and hits (X-001, X-002),
idle CPU (X-007), error surfacing table (X-023), focus restore (X-024), multi-window semantics (X-033), UI-thread
timings on 20k rows (X-038), pane minimum widths (X-040), focus-lost fallback (X-043).
Tests: G in `integration_tests/` using the real panes; Q `cross-pane.qa`.
DoD: common + every QA scenario of every WP passes in one run on a fresh sandbox; S review of all screenshots; the
release dry run (WP7) succeeds on the integrated branch.

### 17.11 Phase 0, Phase A, Phase B, Phase C and the fix round as built — deviations from the revision-2 text (read before Phase B / Phase C / the release)

WP-C and WP0 are merged into master (WP-C first, then WP0, then the integrator's adaptations). The sections above
were updated in place; this list is the index of every frozen-contract change, so a package can check what it
consumes:

* **Core (WP-C)** — §4.3 "As built": K1 edit replay, K2 UUID carry-over across refreshes, K4 `DeletePlan.path` and
  plan re-validation, K6 Clear empties the semantic dir and keeps `embeddings.lock`, K12 missing `[gui]` = defaults,
  K13 `FastembedEmbedder::new_in` refuses under `hermetic`, K14 `help` subcommand + `LaunchCommand::SubcommandHelp`,
  K16 relative `XDG_DATA_HOME` against the launch dir.
* **Events (§7.2)**: `ListPaneEvent::OpenRequested` and `DeepSearchEvent::OpenHit` carry `agent_tab:
  Option<AgentTab>`; the main window flushes a pending preview in the capture phase of every list-target action
  (L-085); the app-level `CorpusBus` handler closes `OpenTargets` dialogs (with or without a main window), window roots
  clear their own panes, single-file windows clear a deleted file.
* **Model (§7.9)**: `OpenRequest.agent_tab` + `OpenRequest::{new, with_reveal, with_agent_tab}`; `QaArgs.real_console`;
  additive helpers (`short_session_id`, `truncate_chars`, …); `describe()` shortens the session ID.
* **Globals (§7.3)**: `OpenTargets::register(cx, window, path, title, close)` with `CloseFn = Box<dyn FnOnce(&mut
  Window, &mut App)>` run deferred in the dialog's window, `close_for` returns `Vec<ClosedTarget>`; `ActivePane` per
  window (`set`, `of_window`); `CorpusBus::{install, entity, broadcast}`; init step 0 (services + debug buffer first);
  `init_for_test(cx, world, launch, recording)` applies the world's `[gui] theme`.
* **Services (§7.4)**: `Services::{real, recording, for_launch, is_recording}` + `Deref`; `app::run` uses
  `Services::for_launch` (recording for a `qa` run with `--qa-scenario`, `--qa-real-console` →
  `RecordingServices::with_real_console`); `lib::run_with_services`; `ServiceCall::StdoutFailed`; extra
  `RecordingServices` accessors; the `ClipboardGuard` root plugin takes over gpui-kit's Copy/Cut/Paste whenever the
  services record; `RealServices` refuses side effects in test builds.
* **Routing (§8.1)**: `run_session_action -> SessionRoute::{Done, Open { agent_tab }, Viewer, NotSession}`, `forward ->
  Forwarded::{Unhandled, Handled, Open(Box<OpenRequest>)}`, `row_menu_action`; menu enabled state = element listeners
  (Quit on both roots, CloseWindow only on single-file roots and no global CloseWindow on macOS, OpenJsonlFile not in
  picker mode, ShowConversations only in single-file windows, no Resume/Fork/Rename/Delete/SelectAndExit there).
* **Keymap (§8.3, §8.5–§8.7)**: list keys in `ConversationList && !ListToolbar`, Deep search hit keys in `DeepResults`,
  agent rows in `AgentRows` (buttons keep their keys); user bindings validated at startup (reserved keys keep the
  default, debug-log lines) and on live rebinding; `HelpRow` carries key pairs, a `when` condition (Ctrl+T rows) and
  single-file labels.
* **Focus (§7.7)**: new contexts `DeepResults`, `AgentRows`; F6 includes the list toolbar (`ListPane::focus_toolbar`);
  leaving the viewer returns to the Conversations tab's search; Esc in the agent panel focuses the transcript; the
  viewer's session handlers live on the transcript column and use the agent panel's result-set target while the panel
  has focus; the palette lists Settings and Quit on macOS too.
* **Agent worker (§7.5, §17.8)**: per-consumer `AgentClient` (own reply channel and slots) from
  `AgentWorkerHandle::client()`; `submit`/`cancel` live on the client; `AgentReply::Done` carries the `AgentResponse`.
* **Widgets**: `ConfirmControl` documents its top-most-dialog closing contract and gains `close_then(cx, then)` (open
  a follow-up dialog only after the confirmation is gone).
* **Harness (§15.2)**: per-WP fixture modules and `integration_tests` declared; `open_single_file_with_args`,
  `launch_for_args`, `activate`; one world/argument set per test app; redirect root cleanup; doctests off; the safety
  lint as a source test.
* **Gate (§15.4)**: fails closed without the OS profile; refuses default data folders that resolve outside the home.
* **Manifests (§16.1)**: shared-package version drift forced by gpui-kit recorded in the root `Cargo.toml`;
  `toml_edit` is a workspace dependency used by core; the GUI's `[dev-dependencies]` enable `hermetic`.

**Open requested changes for Phase A/B (owners in brackets):**
* [WP7] `main.rs` (integrator stop-gap in place): `LaunchCommand` is matched exhaustively — `SubcommandHelp(text)` and
  `Agent(AgentCli::Help(text))` print and exit 0; `DeleteEmpty { local, .. }` / `Update` start the GUI with
  `StartupTool::DeleteEmpty { local }` (and `args.local = local`, so PD3 rule 2 makes the launch cwd the workspace) /
  `CheckForUpdates`; `Agent(AgentCli::Request)` runs headless via
  `core::agent::execute` (workspace = launch cwd, config errors → `Error: <e>`, exit 1). WP7 owns the final version:
  tests of the headless agent path (§17.6), the agent subcommand's own usage line on parse errors (main still appends
  the top-level `Usage: claude-history-gui [OPTIONS] [FILE]`), `ensure_console`/AttachConsole, `startup_tool_for`
  (K14 one-shot flags), `--show-dir`/`--render` headless paths.
* [WP3] `DeletePlan` has the extra field `path`; build plans only with `plan_session_delete`;
  `execute_session_delete` refuses tampered plans.
* [WP5] Clear semantic cache empties the directory and keeps it and `embeddings.lock` (§5.11, §11.16); a second Clear
  reports `Semantic cache is already empty.`
* [WP1] no change needed for K1 replay or K2 carry-over; keep polling on the waker (§11.2, §11.5).

Status of the list above after Phase A: [WP7] done (WP7's `startup::dispatch` replaced the stop-gap; headless paths,
`startup_tool_for`, AttachConsole and their tests landed); [WP1] done; [WP3] and [WP5] still open for Phase B.

#### Phase A as built (integrator: WP7 → WP6 → WP1 → WP2 merged into master, read before Phase B)

Every merge was followed by a build and the GUI tests. The integrator then applied the cross-package requested
changes below (commits `integrator: …` on master). Contract changes a Phase-B package must know:

* **Colours (X-019, WP6 lint `theme_tests::no_color_literal_outside_theme_tokens`)**: raw colours are converted only
  in `theme_tokens.rs`. New helpers: `theme_tokens::data_color(Rgb) -> Hsla` (colours that are core data, e.g.
  syntect tokens) and `theme_tokens::legible_on(background, foreground) -> Hsla` (V-075 match text; WP2 had both
  locally). The lint skips test modules (`tests.rs`, `*_tests.rs`, `*/tests/*`, `test_support/`, `skeleton_tests*`,
  `integration_tests/`) and return types (`-> Hsla {`); production code in `ops/`, `deep_search/`, `tools/` must use
  `theme_tokens::roles(cx)` or the kit theme.
* **Input-injection lint (X-027, `qa/lint.rs` + `tools/ci/safety-lint.sh`)**: the Win32 name is matched as a whole
  identifier only, so gpui's `window.on_mouse_event(…)` listener is allowed. Do not name the injection APIs in
  comments either (the shell lint does not skip comments).
* **QA probes (§15.3)**: `dialog` = `help::dialog_id` (`none` | `settings` | `shortcuts` | `palette`), else the id an
  owner registered with `workspace::startup::register_dialog(cx, "<id>")` (clear it with
  `cx.set_global(LastDialog::default())` when the dialog closes), else `other`. **Phase-B dialog owners (WP3 rename,
  delete, copy-to-workspace, export/copy menus; WP4 if any; WP5 tools dialogs) register their ids** if their
  scenarios compare `dialog` (requested by WP7). Probes now also reach single-file windows
  (`SingleFileWindow::qa_probe`: `viewer.*`, `focus`, `window.kind`); `window.kind` is `main` | `single-file`.
* **QA DSL / runner**: `""` is an explicit empty expected value (`expect list.query == ""`); every committed
  scenario must parse (`qa::dsl::tests::every_committed_scenario_parses`, feature `qa`); a passing `timing` step
  reports `measured <value>` (§15.8 evidence); `shot NAME` deletes a `NAME.png` left by an earlier run into the same
  `--qa-out` folder before it asks for a capture. `tools/qa/add-synth-corpus.ps1 -Sandbox <sbx> -Count 20000` writes
  the `synth::corpus` layout into a sandbox (perf-list.qa; requested by WP1).
* **Window placement (C-037, WP6 request)**: `workspace::main_window_options` and `single_file::open_window` wrap
  their options with `persist::with_restored_bounds` (remembered placement when `restore_session`, clamped to the
  displays; the default size and the minimum size otherwise). Every single-file window opens at the one remembered
  single-file placement.
* **Title bar (`workspace/title_bar.rs`, screenshot review)**: gpui-kit's title-bar row never shrinks below its
  content's min-content width, so the window title is laid out absolutely between the menu bar and the window
  controls and truncates there (at 640 px the single-file window had clipped its close button).
* **PD3 rule 3 (c) (L-082, WP7 request)**: after the first completed load the main window calls
  `startup::apply_recorded_cwd_workspace(cx, ListPane::recorded_agent_cwds())` once (new `ListPane` method: the
  distinct cwds of the loaded Pi/OMP conversations); on a match `AppState.workspace` is set and the list re-scopes
  through `ListPane::set_workspace` (a background reload).
* **Keymap (WP6)**: `keymap::install_all` also binds `help::shortcuts::bindings()` (context `ShortcutsDialog`); code
  that clears and rebuilds the keymap keeps them (`install_user_bindings` does). Status-bar hints look bindings up with
  plain contexts (`CTX_VIEWER`): a predicate string such as `Viewer && !Input` passed to
  `highest_precedence_binding_for_action_in_context` overflowed the stack in a test (WP2 note).
* **Settings (WP6)**: path fields (programs, workspace) are validated in the background and Save is disabled while a
  check is pending (tests call `run_until_parked` after typing); Settings invoked from another window closes the open
  dialog (its draft is discarded) and reopens in the invoking window; shadow warnings for app-wide static keys say
  "in the window". Since C-033 (WP-C2) `[gui] theme` / `restore_session` are optional: Reset removes the key and an
  explicit default reads as "From gui.toml".
* **Focus (§7.7, §9.5, WP2)**: `ViewerPane::focus` moves focus to the viewer even when it is empty (the frozen
  `f6_cycles_through_the_list_toolbar` contract requires F6 to reach the empty viewer); §7.7/§9.5 text saying "no-op
  when empty" is superseded.
* **Viewer (WP2)**: a hovered link shows its URL in the status bar (no tooltip; deviation from AD13/V-041's GUI
  column; click → `Open link?` → OpenUrl / Copy link as specified); the parse worker runs on the gpui background
  executor with a single-flight gate (a stale parse is abandoned mid-file: `ViewerModel::open_cancellable` polls the generation every 64 lines, V-101); viewer probes `viewer.status`,
  `viewer.loading`, `viewer.find`, `viewer.search.mode`, `viewer.expanded`, `viewer.nav_active`,
  `viewer.agent_panel`, `viewer.first_content_ms`, `viewer.install_ms`, `viewer.toggle_ms`, `viewer.search_ms`,
  `viewer.frame_*` exist in addition to §15.3's list.
* **List (WP1)**: list probes add `list.total`, `list.searching`, `list.status`, `list.time`,
  `list.install_pane_max`, `list.loader_install_max`, `list.frame_p95`; `test_support::fixtures_list::ListFixture`
  is the list test fixture; `listed_corpus` gives its files distinct modification times in write order (equal coarse
  mtimes tied and made `refresh_reports_and_keeps_the_selection` flaky under a loaded run).
* **Harness (frozen files, accepted as integrator)**: `test_support::ensure_app` calls
  `cx.executor().allow_parking()` (the list's core worker threads wake the poll task); `skeleton_tests`:
  `enter_moves_focus_to_viewer_stub` writes its session after the first Enter, `palette_dispatches_to_the_stored_focus`
  expects the Keyboard shortcuts dialog, and `skeleton_tests::phase_a` tests the integrator wiring.
* **FEATURES C-009**: `--debug x` is Debug + FILE `x` (core treats a non-level word as the FILE); only
  `--debug=loud` gives the invalid-level error.

Phase-A QA (release `--features qa` build, fresh copies of the scratchpad sandbox; viewer scenarios after
`prepare-sandbox.ps1`, perf-list after `add-synth-corpus.ps1`): every scenario passes except three steps that need
Phase-B behaviour — `settings-keys.qa` lines 41/44 (`expect calls.terminal.count == 1` after the rebound Resume key:
`ops::resume` is still the WP0 stub, **WP3**) and `palette.qa` line 52 (`expect dialog != none` after Tools → Debug log:
`tools::open_debug_log` is still a stub, **WP5**). Those packages make them pass; do not weaken the steps.
Measured (release, Windows dev machine): perf-list on 20k rows — pane install 0.03 ms, typed key ≤ 0.8 ms, frame p95
1.6 ms; perf-viewer 10k — first content 244 ms, install 1.6 ms, idle frame 2.0–2.6 ms, page-through avg 3.3 ms /
p95 6.1 ms, toggles ≤ 12 ms, first search 19 ms, peak working set 205 MB (one transcript; the 20-conversation
working-set check of §15.8 is not scripted yet); the 3k run passes p95 (its stated target), but its idle-frame step (< 3 ms,
typically 2.3–2.8 ms) failed in 2 of 6 runs (3.40 / 3.44 ms). `.github/workflows/qa.yml` now prepares each scenario's sandbox
(`prepare-sandbox.ps1` for the viewer scenarios with the tour or perf transcript as `FILE`, `add-synth-corpus.ps1` for
perf-list); its run on the committed `qa-home` corpus has not been checked locally.

**Open requested changes and CCRs after Phase A (owners in brackets):**
* [WP3] `settings-keys.qa` lines 41/44 and [WP5] `palette.qa` line 52 (above); [WP3, WP4, WP5] `register_dialog` ids.
* [WP3] `DeletePlan.path` / [WP5] Clear semantic cache (Phase-0 items above).
* [WP7 → WP-I] not run yet: the Docker Linux build of the merged tree, a Windows release launch confirming the console
  no longer attaches for a plain launch, CI on three OSes and the release dry run (GitHub CI cannot run here).
* [WP7/WP-I] the DSL has no `paste` step (perf-list types the UUID, the worst case) and no row clicks by stem yet:
  `qa_ext::qa_id` row ids (L-011) work now that the registry exists, but no scenario clicks a row.
* [WP6, optional] `theme_tokens`: a dark `current_match_foreground` on the amber current-match background in dark
  mode (V-075); `legible_on` already compensates.
* [WP2 → WP-I] acceptance depth of V-089, V-098, V-092, V-096, V-008 not strengthened; V-088: viewer Select All is built (Ctrl/Cmd+A,
  Edit > Select All: `ViewerPane.select_all` paints every segment selected, Copy takes `all_text()`; gpui-kit has no
  select-all for the window selection); a dedicated viewer-worker thread is not used.
* [WP2/WP7 → WP-I] the idle-frame step of perf-viewer.qa has little headroom (see above); the §15.8 working-set
  checks (≤ 400 MB, ±10 % after 20 conversations) are not scripted in `run-gui.ps1`; semantic-toggle.qa passes
  while the model downloads or the corpus is still being embedded (it quits before `sem` results arrive), so semantic
  result rows are not exercised end to end in QA (G tests cover them with a fake embedder).
* [user] a leftover junction `C:\t1` (to `scratchpad/targets/wp1`) from WP1: `cmd /c rmdir C:\t1` removes only the
  link.
* [WP-C2] CCR C-033: `config::GuiSettings.theme` and `restore_session` become `Option<…>` (or skip-if-default) so
  `save_gui_config_changes` can remove the keys (Reset) and an explicit default reads as "From gui.toml".
* [WP-C2] CCR-1: installing a finished load/refresh of 20k rows takes about 7 ms (refresh) to 27–65 ms (initial load)
  inside `Browser`/loader poll (target 16 ms, §15.8); make the install incremental or move it off the UI thread. The
  pane's own share measured 0.04 ms in Phase-A QA.
* [WP-C2] CCR-2: `Browser::apply_renamed` (~3 ms) and `remove_paths` (~10.6 ms; a delete totals ~19.6 ms against
  5 ms) do O(n) work per edit at 20k rows; index paths.
* [WP-C2] CCR-3: `Browser::has_search_work_in_flight` stays true for an empty semantic corpus (`list/states.rs` works
  around it).

Status of the list above after Phase B: [WP3] `settings-keys.qa` lines 41/44 and [WP5] `palette.qa` line 52 pass;
`register_dialog` ids exist for every Phase-B dialog (below); `DeletePlan.path` and Clear semantic cache are done;
C-033, CCR-1, CCR-2 and CCR-3 are done (WP-C2, below). The other items stay open (list at the end of "Phase B as
built").

#### Phase B as built (integrator: WP-C2 → WP3 → WP4 → WP5 merged into master, read before Phase C)

Merged with `git merge --no-ff` in that order; no textual conflicts and no compile conflicts (the Phase-B packages
did not use the APIs WP-C2 changed). Every merge was followed by a build and the GUI tests: 421 → 508 → 581 → 644
passing (core 1239). The integrator then applied the requested changes below (commits `integrator: …`). Contract
changes WP-I must know:

* **Core (WP-C2)**:
  * C-033: `config::GuiSettings.theme: Option<ThemePreference>` and `restore_session: Option<bool>` (read them with
    `GuiSettings::{theme(), restore_session()}`); unset fields are not serialized, so Reset in Settings removes the
    `gui.toml` key and an explicit default reads as "From gui.toml". WP-C2 edited files it does not own for this
    type change; **accepted by the integrator**: `workspace/mod.rs` (one line), `skeleton_tests.rs`, WP6
    `persist.rs`, `settings/{draft, mod}.rs`, `theme_tokens.rs` and the settings tests, WP7 `workspace/startup.rs`
    and `startup/tests.rs`.
  * CCR-1 is recorded as **moved off the UI thread except an O(n) pointer pass**: the initial-load and refresh
    installs take a scope prepared on `corpus-prepare`; a replaced corpus is dropped on the new `corpus-release`
    thread (§3). Still on the UI thread: `reindex` when the loading rows were shared, `remove_indices`
    (`KeyIndex::shift` over every slot, `ScopeIndex::remove`, `remove_positions_in_place`) and the `scope_rows()`
    copy. Measured at 20k rows (core `browser::perf_tests::browser_timings_on_20k_rows`, ignored, release): install
    1.2–1.4 ms, delete 0.4–0.6 ms.
  * CCR-2: `browser/index.rs` indexes paths and session ids; `Browser::conversation_by_path(path) -> Option<(usize,
    &Conversation)>` (path equality; session ids ignore ASCII case). A delete during an in-flight lexical search
    re-dispatches the query (Lexical mode, non-empty, not a UUID). The idle lexical worker holds no corpus snapshot.
  * CCR-3: `has_search_work_in_flight()` ends for an empty semantic corpus (the `list/states.rs` workaround is now
    redundant but harmless). L-015/X-029: Windows worktree paths are detected with backslashes.
* **Session actions (WP3, `ops/*`)**: the §17.7 entry points; dialog ids `rename`, `delete`, `copy-to-workspace`,
  `export-menu`, `copy-menu`, `export-replace`, `print-and-exit` (cleared with `ops::copy::clear_dialog(cx, id)`).
  `ops::resume::{TerminalVar, set_terminal_var}`: **`main` reads `$TERMINAL` once into the new
  `LaunchContext.terminal_var` and `app::init` stores it** (integrator). A Resume/Fork is guarded per path while it
  is planned or launched (a window closed in between keeps the guard until restart; accepted). `ops::resume`
  used to duplicate core's per-launcher preparation and `ops::delete` to detect a partial delete by checking which
  files remain (both replaced in WP-C3: `prepare_launcher_command`, `SessionDeleteFailure.partial`). `ops::select::NOTHING_TO_PRINT` is the picker banner's answer without stdout
  (WP3's one-line edit of WP1's `list/tests/ui.rs` accepted). New scenario `picker-exit.qa` (§15.3, §17.7): its
  evidence is exit code 0 and exactly one printed line, checked by `qa.yml`.
* **Deep search / agent panel (WP4)**: `DeepSearchPane` subscribes to `CorpusBus` itself (`mark_deleted(paths,
  cx)`); `Workspace::on_corpus_notice` still marks the agent panel. The agent panel's hit menu leaves out Search
  within / Outline / Read range. A multi-conversation Read keeps the viewer's conversation as its target when it was
  read, else the first one read (titles of other conversations fall back to the file stem). Ambiguous-ref
  candidates are expanded to full refs for every source (Pi/OMP through `discover_agent_keys` and `full_ref_for_candidate`, WP-C3; skipped for a cancelled job). Result
  rows stay `div`s (multi-line cards with nested buttons) with stable domain ids. Probes: `deep.*`, and `agent.*` —
  **the main window now routes `agent.*` to the panel** (integrator; `deep.agent.*` still works); single-file
  windows route only `viewer.*`, `focus`, `window.kind`.
* **Tools (WP5, `tools/*`)**: dialog ids `about`, `debug-log`, `debug-search`, `delete-by-id`, `delete-empty`,
  `generate-semantic-cache`, `clear-semantic-cache`, `semantic-debug`, `updates`. Tools dialogs are per window
  (`tools::common::place_here`: invoked from another window, the open one is closed there and reopens here; it
  closes that window's top-most dialog, which may be another one stacked on top — accepted). Edits to other
  owners' files, **accepted**: `DebugBuffer::pushed()` (globals.rs), the `ShowDebugLog` route calls
  `tools::open_debug_log_with(tab, viewer, …)` (routing.rs; the window's own viewer, fixes the single-file crash),
  WP1 `list/load.rs` rebuilds the loader when the Debug log capture level changed (T-022), WP2's `viewer/tests.rs`
  V-008 assertion now checks the Debug log dialog. T-016 deviation: Show projects directory without a workspace
  shows the toast `Choose a workspace first` with a Choose workspace… action instead of a disabled item.
* **Integrator changes in Phase B** (`skeleton_tests::phase_b` tests them):
  * `notify`: every toast records its buttons (`LastToastActions`, the action button first); `last_toast_actions(cx)`
    and, in tests/QA, `trigger_last_toast_action(label, window, cx)` press one. WP3's A-010 and A-061 tests now
    press `Choose workspace…` / `Export to Downloads` (WP3 request).
  * `menus`: `MenuChecks.header_session_id`; Session → Copy header session ID is listed only when the active
    region's target (list selection, Deep search hit, agent result set, else the viewer's conversation) has a
    header id that differs from its stem (A-042, WP3 request). The menus refresh when that flag changes.
  * X-038 (WP-C2 request): `ListPane::{conversation, select_path}` and the row lookups use
    `Browser::conversation_by_path` before the case/separator-insensitive scan (kept for another spelling of the
    same file on Windows); `ListPaneEvent::CorpusChanged` calls `menus::refresh_if_changed` instead of a full
    `menus::rebuild` (LoadStateChanged still rebuilds). Not re-measured: `cross-pane.qa` is still to be written.
  * Agent panel focus: when the panel shows a message instead of a tab body (file outside the history roots,
    failed key lookup, no conversation) it focuses its own root, also when the key reply arrives before the
    panel's first frame (viewer-basics.qa V-097 failed after WP4 landed: focus `none`, `o` dead).
  * Screenshot review fixes: Deep search hit previews end with `…` on their second line and their labels align
    with the first line; Read cards span the list width; the Within Top / Read Context fields fit `Default (n)`;
    the Debug log and Semantic debug texts fill their boxes (they were one or two lines tall).
  * `qa.yml` plans: `resume-fork` → `--workspace "{0}"`, `picker` → `--show-path`, `picker-exit` →
    `--show-path --qa-real-console` with `-StdoutFile` and the one-line check (WP3 request).
* **FEATURES**: A-062's acceptance names where the `Unknown conversation type` tooltip can be checked (single-file
  windows have no Resume/Fork/Rename/Delete handlers).

Phase-B QA (release `--features qa` build of the integrated head, each scenario on a FRESH copy of the scratchpad
sandbox; `deep-search`/`agent-panel` with `crates/gui/tests/fixtures/qa-home` merged into the copy; viewer scenarios
after `prepare-sandbox.ps1`, perf-list after `add-synth-corpus.ps1`, semantic-toggle with the QA model cache):
every scenario passes in one run — smoke 17, startup-load 19, list-search 26, semantic-toggle 14, perf-list 23
(pane install 0.03 ms, typed key ≤ 0.05 ms UI busy, frame p95 0.57 ms), viewer-basics 77, viewer-search-nav 67,
single-file 28, perf-viewer 3k 49, settings-keys 37 (lines 41/44 now pass), theme 40, palette 40 (line 52 now
passes), rename 36, delete 31, resume-fork 33, export-copy 43, picker 17, picker-exit (exit 0, exactly one printed
line: the selected transcript's path), deep-search 57, agent-panel 60, tools 79 steps — except the perf-viewer
idle-frame steps (`viewer.frame_last < 3ms`, lines 26/29; perf-viewer 10k was 47/49 in that run). They sample one
frame: over 11 perf-viewer runs (10k and 3k) the typical value was 1.9–2.8 ms and 8 of 22 samples were 3.1–4.3 ms
(Phase A: 2 of 6 runs) — **WP-I**: judge the median of several idle frames instead of the last one (do not raise
the target). Other perf-viewer 10k numbers: first content 306 ms, install 2.2 ms, page-through avg 3.1 ms / p95
4.5 ms, toggles ≤ 8.5 ms, first search 11.7 ms. Screenshots of every new dialog (rename, delete, copy to workspace
incl. the overwrite warning, export menu and replace prompt, picker banner, Deep search empty / options / results
light and dark / no hits / agent text, agent panel Outline light and dark / Within / Read / agent text, Tools:
delete empty, generate and clear semantic cache, debug search, semantic debug, debug log, delete by ID, about,
updates) were reviewed; the defects found are fixed above.

**Open requested changes and CCRs after Phase B (owners in brackets; WP-I unless noted):**
* [WP-I] X-038 acceptance: write `cross-pane.qa` (load end, refresh end, rename apply, delete removal, UUID paste on
  20k rows). WP-C2 measured GUI delete at 5.2–8.4 ms and rename at 4.5–5.0 ms before the integrator's lookup and
  menu changes; re-measure. Remaining UI-thread O(n): the CCR-1 pointer pass (above), `ListPane::header_info` for
  a path that is not loaded (a scan), `menus::rebuild` on LoadStateChanged.
* [WP-L, done] CI only: `settings-keys.qa` lines 41/44 need a listed conversation whose project folder exists. `tools/qa/new-sandbox.ps1 -WithWorkspace` now creates `<sandbox>/work/resume-demo` and a newest Claude conversation whose cwd is that folder; `qa.yml` passes it for `settings-keys` only (other row counts are unchanged). Verified: passes with the switch, fails at line 44 without it.
* [WP-I, WP5 request] T-016: disable Show projects directory without a workspace (window roots register the
  listener only with a workspace, like `OpenJsonlFile`; WP6 menus), then drop the toast branch of
  `tools/projects_dir.rs` and update `tools/tests/misc.rs::show_projects_directory_needs_a_workspace` and
  `tools.qa` lines 82–86.
* [WP-I, WP5 request] `widgets/text_view.rs`: an append / keep-scroll-position API so the live Debug log keeps its
  scroll position and selection (each refresh scrolls to the top now).
* [WP-I] Package deviations to close: WP3 — no test injects a result set (`ListPane::inject_load_event`) for the
  A1/A2 races, red-before-green not run for its production fixes; WP4 — D-014 click, D-024 anchor-copy button and
  D-019 "No hits" are checked at model/action level only, D-035's body text is not inspectable; WP5 — T-024, T-008,
  T-011 acceptance depth, the confirmation-title row strings, delete-empty row virtualization and keyboard reach,
  and no per-row FEATURES sweep (its verified/unverified row lists are empty).
* [WP-I] Phase-A items still open: ci.yml and qa.yml on GitHub runners (the matrix never ran; macOS was never compiled: the first macos-latest clippy/tests/release build); the release dry run on GitHub (WP-L did the Docker Linux build/test/clippy of the merged tree, the
  Linux release packaging in ubuntu:22.04 and the Windows console checks, see "WP-L as built" below); the DSL has no `paste` step and no row click by stem; V-089,
  V-098, V-092, V-096, V-008 acceptance depth; the perf-viewer idle-frame headroom; the §15.8 working-set checks;
  semantic-toggle.qa still quits before semantic result rows arrive.
* [done, WP-C3] CCRs of WP3/WP4: `resume::terminal::prepare_launcher_command(env, &TerminalLauncher, &LaunchSpec,
  &LaunchOptions)` (cwd default + fresh macOS `.command` path; `prepare_terminal_command` uses it) replaces the
  duplicate in `ops::resume`; `maintenance::execute_session_delete` returns `Result<DeleteReport,
  Box<SessionDeleteFailure { error, partial: DeleteReport }>>` (`From` into `AppError`) and `ops::delete` /
  `tools::delete_by_id` take the removed copies from `partial`; `agent::service::discover_agent_keys` is public and
  `agent::refs::full_ref_for_candidate` maps an ambiguous-ref candidate line to its durable full ref, so
  `deep_search::worker::expand_handles` fills Pi/OMP candidates as full refs too.
* [user] the junction `C:\t1` left by WP1 (Phase A note) is still to be removed by the user.

Status of the list above after Phase C: X-038 (`cross-pane.qa`, re-measured), T-016, the `text_view` append /
keep-scroll API, the DSL `paste` step and row clicks by id (`cross-pane.qa`), V-089/V-098/V-092/V-096/V-008 depth,
the perf-viewer idle-frame median, the §15.8 working-set checks and semantic-toggle's semantic rows are done (WP-I);
the CI-only `settings-keys` item, the Docker Linux build of the merged tree and the Windows no-console launch are
done (WP-L; the Linux run was repeated on the integrated head); the three CCRs are done (WP-C3); `C:\t1` no longer
exists. Still open: CI on GitHub runners (three OSes) and the release dry run, and the package deviations of WP3,
WP4 and WP5 that WP-I did not close (consolidated list at the end of "Phase C integration").

#### WP-L as built (cross-platform verification, CI hygiene)

* **Linux (ubuntu:24.04 container, the `ubuntu-latest` family; rustup 1.99.0)**: `cargo build`, core tests (hermetic), GUI tests
  (GPUI test platform, no X server needed), GUI tests with `qa`, clippy `-D warnings` (workspace and `qa`), fmt and the safety lint
  are green. The static ONNX Runtime that `ort-download-binaries` links needs glibc 2.38 / GCC 13 libstdc++: it does not link on
  Debian bookworm (`rust:1-bookworm`: `__isoc23_strtoull`, `_M_replace_cold` undefined). Release builds use `release-dynamic-ort`
  (no static ORT), which is why the release job builds on ubuntu-22.04; that job was reproduced end to end in an
  ubuntu:22.04 container (exact apt list, ORT download + sha256, `make-tarball.sh`, tarball checks, `--version`/`--help`).
* **Linux test fixes** (tests only): `write_gui_config` names `terminal = "xterm"` on Linux (the test world's `PATH` has no
  emulator, so every resume test hit `no terminal emulator found`); `process_alive` treats a pid that does not fit `pid_t` as
  gone; the permission-based failure tests skip as root like the existing ones; `omp_rename_failure_is_inline` makes the directory
  read-only on Unix (the OMP rewrite is a temp-file rename); `synth::corpus` sets distinct mtimes (conversation timestamps are
  mtimes and tied conversations load in a different order on Linux, which made `the_debug_dump_is_core_text` flaky).
* **macOS**: read, not built. Every `#[cfg]` platform item is `windows`/`unix`/`linux`; macOS-specific behaviour is `cfg!`
  (type-checked everywhere); `Info.plist.in`, `make-app.sh` and the release job were reviewed. No change was needed.
* **Windows console (A-051, C-017)**: the release exe is windows-subsystem and never allocates a console; a normal launch has
  none (`AttachConsole(pid)` fails with ERROR_INVALID_HANDLE, from a console parent and via ShellExecute). Bug found and fixed in
  `ops/console.rs`: `exe --help > file` (stdout redirected, stderr the parent console) printed nothing, because the attach
  for the unusable stderr re-pointed stdout at the console; the usable standard handles are now restored after the attach, and
  the attach runs once. `tools/ci/check-console.ps1` (used by ci.yml and release.yml) checks the no-console launch and four redirect
  shapes (`> f`, `> f 2>&1`, `| more > f`, `--version > f`); it fails on the previous build and passes on the fix.
  Picker mode prints one existing transcript path to a redirected stdout (`picker-exit.qa`, `--qa-real-console`, passes).
* Review fixes: check-console.ps1 launches via `cmd.exe /c` and probes from a redirected child pwsh (it can fail on an attached console), applies the full run-gui sandbox variable set and requires a `.qa-sandbox` profile; ORT cache path for macOS added; the macOS `CFBundleDocumentTypes` entry was dropped (the app has no `on_open_urls` handler; a Finder open-document route is a WP1 item); `polls_stay_short_while_the_corpus_is_prepared` bound raised 250 ms to 1 s (it measured 336 ms on loaded Linux).
* **Not done**: `qa.yml` stays Windows-only (the run/shot scripts are PowerShell + Win32); the release dry run and the
  macOS bundle need GitHub runners.

#### Phase C as built (WP-I: integration on the Phase-B head, read before the release)

WP-I owned `workspace/mod.rs`, the frozen files and `integration_tests/`; as the integrator it also edited the
package files listed below (each edit is small and named here). Contract changes:

* **Workspace (PD1, X-040)**: the left pane makes room for the open agent panel — its width is clamped so the
  viewer keeps 32rem next to the panel (`effective_left_width_rem` subtracts the panel), and when even the tab's
  minimum does not fit (`workspace::left_fits_with_panel`) the left pane is hidden *automatically*: not remembered
  in `state.json`, shown again when the panel closes, ended by any explicit show/hide (`set_left_collapsed`). Checked
  when the panel opens and on every window resize (`observe_in` the panel, `observe_window_bounds`).
  `deep_search::panel::panel_width_rem` is the one panel width. New: `Workspace::{auto_collapsed, frame_stats}`,
  probes `layout.{left_collapsed, auto_collapsed, left_width_rem, left_tab}`, `window.{frames, frame_ms_total,
  frame_cpu_ms_total, frame_last_ms, frame_age_ms, activations, input_events}` (a canvas as the root's last child
  times every frame, render to paint end, and counts the mouse events the window receives; the workspace counts
  its window's activation changes).
* **T-016**: `ShowProjectsDirectory` left `on_app_actions!`; both window roots register it only while a workspace
  is set (menus and the palette show it disabled otherwise). `tools/projects_dir.rs` lost its toast branch; the
  only way to run it without a workspace is the `--show-dir` startup tool of a launch without stdout, which shows
  core's error (`Choose a workspace folder to show its Claude projects directory`) instead of nothing (PD16).
  `tools/tests/misc.rs` and `tools.qa` follow.
* **`widgets/text_view.rs`**: `ReadOnlyText::{append_text, update_text, selected_range, scroll_offset}` keep the
  reader's scroll position and selection (the scroll offset is pinned for the next layout, after the caret-follow
  scroll a selection change asks for); the Debug log's 1 s live tick uses `update_text` (WP5 request); explicit
  refreshes (tab, level, Clear) still start at the top.
* **UI-thread spans (X-038, §15.8)**: `globals::{UiBusy, BusySpan, BusyTimer, BusyMeasure}` time an operation
  around the *outermost* update that runs it — the handler plus every subscriber, observer and menu refresh it set
  off: `LoadEnd`/`RefreshEnd` in the list's poll task (`poll_all` returns the install it applied), `RenameApply` in
  `ops/rename.rs`, `DeleteRemoval` in `ops/delete.rs`, `Paste` in the QA `paste` step. Each run records wall time
  and the UI thread's own CPU cycles (`QueryThreadCycleTime`, Windows; the QA runner calibrates cycles per ms at
  start), keeps the last 9 runs (median) and, in test/QA builds, the *parts* that ran inside it
  (`globals::{SubSpans, record_sub_span, sub_span}`: `paste.guard`, `paste.replace`, `list.search_changed`,
  `list.apply_renamed`, `list.remove_paths`, `ws.list_event`, `ws.corpus_notice`, `viewer.clear`,
  `panel.mark_deleted`, `deep.mark_deleted`, `app.close_targets`, `menus.refresh`, `menus.rebuild`, `toast.show`,
  `delete.close_dialog`). `menus::RebuildStats` counts rebuilds.
* **List (WP1 files)**: the 30 s ticker repaints only when a painted row's time label (text or recency colour)
  changes (`rendered_rows`, `row_times_change`): an idle window draws nothing (X-007). Probes `list.{ticks,
  tick_repaints, selected_index, semantic_rows, selected_semantic, frame_last}`; the Semantic result dialog
  registers `semantic-details`.
* **Viewer (WP2 files)**: `viewer::TranscriptState` + `ViewerPane::transcript_state()` — `render_transcript` draws
  exactly that state (V-098); probes `viewer.header`, `viewer.frame_median_<n>` and, from each frame's UI-thread
  CPU time, `viewer.frame_cpu_median_<n>`, `viewer.frame_cpu_{avg,p95}_100`; test accessors `cached_tokens`,
  `drawn_theme`, `lru::Lru::entries`.
* **Deep search (WP4 files)**: hit rows are observable in gpui tests (`test_support()`), `results::hit_element_id`.
* **notify / globals (frozen)**: test-only `notify::{ToastLog, toast_log}` (which window got which toast) and
  `OpenTargets::entries`.
* **QA runner / DSL / probes (WP7 files)**: steps `paste "text"` (the recording clipboard, then one `secondary-v`;
  the report lists the span's parts), `idle <duration>` (a window shorter than the 30 s ticker period starts right
  after a ticker run; frames — ticker repaints counted apart — and their UI time, wall and CPU: `idle.{frames,
  frames_total, ticks, busy_ms, busy_cpu_ms}`, and the frames of the wait for that run: `idle.{ticker_frames,
  ticker_frame_ms}`; a window that input from outside the run reached — an activation change or mouse events, the
  desktop is shared — is reported and measured again, up to three attempts, the last one counting),
  `mark-memory <name>` (`memory.<name>.{mb, delta_pct, change_pct}`), `clear-toasts` (earlier toasts out of a
  screenshot); probes `busy.<span>[_max|_median|_runs|_cpu|_parts]`,
  `process.{working_set_mb, peak_working_set_mb}` (`K32GetProcessMemoryInfo`, Linux `/proc/self/status`),
  `menus.{rebuilds, rebuild_ms_last}`. `tools/qa/run-gui.ps1` prints the child's peak working set and no longer
  fails when the screenshot watcher exits first. `qa.yml` runs `cross-pane` on the synthetic 20k sandbox (900 s).
* **Scenarios**: new `cross-pane.qa` (WP-I); `perf-viewer.qa` judges the median of nine idle re-renders
  (`viewer.frame_median_9 < 3ms`, target unchanged; the same frames' UI-thread CPU time is reported next to every
  wall-clock frame target) and checks the working set (current and peak ≤ 400 MB);
  `semantic-toggle.qa` waits for the semantic result rows (`list.semantic_rows > 0`) and the details dialog;
  `perf-list.qa` adds a real paste; `tools.qa` checks T-016's disabled palette item; `list-search`, `startup-load`
  and `semantic-toggle` set the light theme before their `-light` shots (they showed the system theme).
* **Search-match text (WP2 file `viewer/render.rs`, `theme_tokens.rs`; V-075, X-039)**: `theme_tokens::legible_on`
  picks the match text by WCAG contrast on the fill as drawn (the translucent fill composited over the transcript
  background, a third argument) instead of comparing HSL lightness, which drew white on the pale light-theme fill
  and on the dark theme's yellow current match (screenshot review); `theme_tests::search_match_text_is_legible_
  in_both_themes` guards both styles in both themes.
* **FEATURES**: V-103's acceptance moves the focus by keyboard (in gpui-kit 0.7.3 any left mouse-down clears the
  window's text selection in its capture phase, so a click on a row drops the selection first); X-007's idle probe
  runs with the transcript focused (a focused input repaints its blinking caret twice a second) in the 25 s after a
  ticker run (a repaint the ticker needs — a `N min ago` label changed — is reported, not counted).

**Measurement rule (X-038).** Load end is one sample (one load). Refresh end, UUID paste, rename apply and delete
removal run three to five times in `cross-pane.qa` and are judged by their median, with the maximum and the
UI-thread CPU time reported: about one sample in twenty carries a 14–70 ms hiccup of UI-thread CPU (CPU ≈ wall, so
not preemption) that lies outside every handler of the operation — the parts of such a paste summed to 0.25 ms of
14.2 ms. The targets are not raised; the hiccup is an open item below.

**Tests.** `integration_tests/` (real panes, 24 G tests): `focus` (L-085, L-098, C-058, X-043, X-024, X-040),
`dispatch` (A-055, X-001, X-002, V-103), `corpus` (V-007, T-023, D-011 with WP4's D-024 anchor click, X-023 ×3),
`windows` (C-065 ×3, X-033), `measure` (X-038 spans, X-007 frames and ticker, the live Debug log). Strengthened
WP2 rows: V-008 (badge in both windows, the click opens Parse errors in the clicking window with the error listed),
V-089 (the real export menu and a pointer click, positive control after Esc), V-092 (`global_lock`, the app's theme
path, cached tokens equal core's highlighting in the new theme), V-096 (every toolbar control incl. Resume/Fork
against the key bound to its action in the transcript, same action-log entries and launches), V-098 (each state's
text from `transcript_state`, Loading caught right after the open).

**QA results (Phase C, Windows 11, release `--features qa`, 2026-10-17).** One run of every scenario of every WP,
each on a fresh copy of the scratchpad sandbox template (`.qa-sandbox`), 23 runs, all passed (exit 0, no FAIL, no
SKIP; the final run 18:44–18:50 on the committed tree, the two before it with the same result): smoke 17,
startup-load 20, list-search 27, semantic-toggle 23 (model from the QA model cache, the sandbox corpus embedded on
first use, 172 s), perf-list 28 (20k rows), viewer-basics 77, viewer-search-nav 67, single-file 28, perf-viewer 68
(10k messages) and 68 (3k), settings-keys 37, theme 40, palette 40, rename 36, delete 31, resume-fork 33,
export-copy 43, picker 17, picker-exit (one stdout line, an existing `.jsonl`; the scenario writes no report by
design), deep-search 57, agent-panel 60, tools 83, cross-pane 238 (20,000 synthetic rows + the template's 30).
Measured in the final run (the run before it within ±30 %):
* X-038 (`cross-pane`, ms, median / max / UI-thread CPU): load end 2.16 (one sample; CPU 2.15), refresh end
  0.41 / 0.42 / 0.31, UUID paste 0.21 / 0.28 / 0.21, rename apply 0.09 / 0.09 / 0.08, delete removal 0.66 / 0.66 /
  0.45 — every target met by more than an order of magnitude; `perf-list`: paste 0.22, install 0.03, frame p95 0.55.
* X-007 (`cross-pane`): 25 s with the transcript focused — 0 frames, 0.000 ms UI time (the run before: the same,
  with 8 frames / 50 ms of earlier work settling while waiting for the ticker run).
* §15.8 viewer (10k / 3k): first content 221 / 92 ms, install 2.2 / 0.4 ms, idle frame (median of 9) 2.41 / 2.18 ms,
  page-through avg 3.1 / 3.3 ms and p95 4.6 / 6.2 ms (UI-thread CPU the same within 0.05 ms), toggles ≤ 9.0 ms,
  first search 13.4 / 4.2 ms; working set 162 → 126 MB, peak 202 / 129 MB.
* §15.8 working set (`cross-pane`): 190 MB after the first conversation, +4.1 % after 20 more in a row; the
  process peak, with the 20k-row load and a Deep search over every transcript, 388 MB (reported, not a target of
  this scenario). The OS-reported peaks of the scenarios on the 30-conversation template were 288–327 MB in this
  run and 220–270 MB two hours earlier with the same code (an A/B run of the two commits gave 302–307 MB for both:
  the machine's state, not the code); the semantic-toggle run with the embedding model peaked at 1.3–2.0 GB.
* Earlier runs of the same day failed `idle` twice — the window had received activation changes and mouse events
  from the shared desktop (15 frames, 88 ms) — and `perf-viewer` once (page-through p95 56 ms while other programs
  used the CPU); the idle step now measures such a window again, the frame targets are unchanged.
* Screenshots (78, light and dark, §15.5 checklist) reviewed: title bar and menus, rows (lane/title/meta/preview,
  highlights, selected fill), footers, viewer lanes, highlighted code, tables, dialogs naming their targets. Fixed
  from the review: the search-match text contrast (above), stale toasts and the system theme in `-light` shots.
  Checked, not a defect: the Semantic result preview's `&lt;entity&gt;` is in the transcript itself. Open: the
  Debug search table has no column-header row (§5.12 mockup) [WP5].
* Linux (Docker `rust:1-bookworm`, x86_64, a copy of the read-only mounted tree, `release-dynamic-ort`, `--locked`):
  fmt, clippy `-D warnings` (with and without `qa`), core tests with `hermetic` (1240), gui tests (674) and with
  `qa` (693), the release build and its `--version` — all green. Fixed on the way (the first run had 26 gui
  failures): the fixture `gui.toml` names `terminal = "xterm"` on Linux (launcher detection reads `PATH`, which the
  sandbox has none of), the permission-based failure tests skip as root (chmod does not stop root), a stale pid
  beyond `pid_t` counts as dead, and `synth::corpus` gives each file its own mtime (order ties).

**WP-I review round (after 772b640):**
* X-040: `clamp_left_width` keeps the tab's minimum (a visible pane is never narrower than 18rem / 24rem; the viewer
  gives way instead); the auto-collapse is mirrored in `AppState.list_auto_collapsed` so View > Focus viewer reads
  checked while the list is auto-hidden. The x040 test asserts both.
* T-022: `ReadOnlyText::update_text` recognizes a full ring buffer (the old text minus whole dropped lines starts
  the new one) and shifts the selection by the dropped bytes and the scroll by the dropped lines.
* T-016: `--show-dir` without a workspace shows core's error with the Choose workspace… action again.
* X-038: `cross-pane.qa` runs rename and delete in the unfiltered 20k-row list (rename apply 0.15 ms, delete removal
  0.73 ms there); `perf-list.qa` judges the paste by the median of three. X-007: `idle.process_cpu_ms` (CPU of the
  whole process over the window) is asserted < 2.5 s so a polling thread or a task waking without a repaint is seen
  (the baseline is 0.4-1.1 s per 25 s on one non-UI thread, not attributed); a second, report-only idle probe with the
  list search input focused measures the caret blink (about 2 frames/s, ~6 ms each at 20k rows), which only gpui-kit
  could stop (`BlinkCursor` is private, read-only here).
* X-024 R item 2: the viewer toolbar's `⋯` has an accessibility label; the Deep search warnings and segment toggles
  have tooltips.

**Open items after WP-I (as handed off, before the merge; the consolidated list after Phase C closes "Phase C
integration" below):**
* [user/WP7] not run here: CI on three OSes, the `release.yml` dry run (GitHub only). The Windows release launch
  without a console was checked locally (PE subsystem 2, no child process, window up) and `check-delayload.ps1`
  passes on the release exe.
* [WP-I follow-up] the sporadic 14–70 ms UI-thread CPU hiccup inside an input dispatch, outside every handler of
  ours (gpui dispatch or work an earlier frame deferred): profile it.
* [WP-I follow-up] a full main-window frame costs about 10–16 ms at 20k rows with a conversation open (the viewer's
  share 8–12 ms, the list's about 1 ms; `window.frame_last_ms`, `viewer.frame_last`, `list.frame_last`).
* [WP3] no test injects a result set through `ListPane::inject_load_event` (it applies no rows; X-001 uses the
  selection change a new result set emits, `select_path`); red-before-green not run for WP3's fixes.
* [WP4] D-014 click and D-019 "No hits" are checked at model/action level, D-035's body text is not inspectable
  (D-024's anchor button is now clicked for real, `corpus::d011_…`).
* [WP5] T-024, T-008, T-011 acceptance depth, the confirmation-title row strings, delete-empty row virtualization and
  keyboard reach; the Debug search table's column-header row (§5.12).
* [WP-I review round; superseded at the merge] `qa.yml` created `C:\sbx\{proj,api,proj\.worktrees\feature}` for
  `settings-keys` (a `--workspace` run opens the copy-to-workspace dialog instead, checked locally: 6 failures). The
  merge kept WP-L's `new-sandbox.ps1 -WithWorkspace` preparation instead (no folders outside the sandbox on the
  runner; "Phase C integration" below). On the committed qa-home corpus `cross-pane.qa`'s working-set block reopens
  its last row (4 conversations match `the`, 30 in the scratchpad template).
* [WP-C2 → done by WP-C3] the Phase-B CCRs (per-launcher preparation, partial `DeleteReport`, full refs for
  ambiguous Pi/OMP candidates): WP-C3 was developed in parallel and merged first ("Phase C integration").

#### Phase C integration (integrator: WP-C3 → WP-L → WP-I merged into master, read before the release)

WP-C3 (the three Phase-B CCRs: `crates/core` and their GUI call sites, §4.3 K4/K17, §4.4), WP-L (Linux and macOS
verification, the Windows console fix, CI hygiene; "WP-L as built") and WP-I ("Phase C as built") worked in parallel
on the Phase-B head `14faa9e`. Merged with `git merge --no-ff` in that order, each merge followed by a build and the
GUI tests: 644 (Phase B) → 655 (WP-C3) → 655 (WP-L) → 681 (WP-I) passing, 1 ignored. WP-C3 and WP-L merged without
conflicts. The WP-I merge conflicted only where WP-I and WP-L had made the same Linux fixes:
* `ops/tests/delete_tests.rs`, `tools/tests/delete_empty.rs`, `test_support/fixtures_ops.rs` and `process_alive` in
  `test_support/mod.rs`: the same code in both, WP-L's text kept. `synth::corpus`: both give every file its own
  mtime; WP-L's spacing (a minute apart, `fixtures_list::set_mtime`) kept, with WP-I's single `now` for the corpus.
* `qa.yml` `settings-keys`: WP-L's `new-sandbox.ps1 -WithWorkspace` kept (a newest conversation whose cwd is
  `<sandbox>\work\resume-demo`); WP-I's `C:\sbx\…` folders on the runner dropped. Checked locally in the CI shape
  (the committed qa-home corpus + `-WithWorkspace`): 37 of 37 steps pass.
* This section: both "WP-L as built" and "Phase C as built" kept; WP-I's stale open items marked above.

There were no compile conflicts. WP-I calls none of the APIs WP-C3 changed (`execute_session_delete` now returns
`Result<DeleteReport, Box<SessionDeleteFailure>>`, K4 marked breaking), and WP-I's `DeleteRemoval` span times the
update that WP-C3's partial report feeds. Integrator commit `integrator: Phase-C cross-package fixes …`:
* root skips for WP-C3's two new permission-based GUI tests (`delete_tests::a_vanished_copy_leaves_the_list_with_
  a_later_failure`, `tools/tests/delete_by_id::a_partial_failure_announces_removed_and_vanished_copies`). The Linux
  container runs the tests as root, and chmod does not stop root. The Linux check below runs them as uid 65534;
* control bytes in text: a BEL in `new-sandbox.ps1`'s help (`C:\sbx\api`) and a form feed in WP-I's open items
  (`.worktrees\feature`). No tracked text file has any now;
* the `settings-keys.qa` header (what `-WithWorkspace` provides), FEATURES A-051 (`tools/ci/check-console.ps1` is
  the CI check) and D-014 (Pi/OMP candidates are full refs too).

**DoD on the integrated head** (Windows 11, MSVC, rustc 1.99, `targets/master`): `cargo fmt --check`; clippy
`-D warnings` for the workspace and for the GUI with `qa`; core tests 1243 passed / 3 ignored (+ the manifest test
and 1 doctest), with `hermetic` the same + 5 hermetic guards; GUI tests 681 passed / 1 ignored, with `qa` 700 / 1;
release builds with and without `qa`; the safety lint and its self-test. All green. On the plain release exe:
`check-delayload.ps1` passes (PE subsystem 2), and `check-console.ps1 -ShellExecute` passes with a fresh sandbox
copy as the profile. A normal launch through `cmd.exe /c` and through ShellExecute owns no console, and `--help > f`,
`--version > f`, `--help > f 2>&1` and `--version | more > f` reach the file.

**QA (Phase C integration, release `--features qa` of the integrated head, 2026-10-17 19:45–19:52).** Every
scenario of every WP in one run, each on a fresh copy of the scratchpad sandbox template (`.qa-sandbox`). The viewer
scenarios ran after `prepare-sandbox.ps1`, perf-list and cross-pane after `add-synth-corpus.ps1 -Count 20000`,
deep-search and agent-panel with the committed qa-home corpus merged in, and semantic-toggle with the QA model
cache. All 22 scenario files in 24 runs (perf-viewer at 10k and 3k, settings-keys also in the CI shape), all
passed (exit 0, no FAIL, no SKIP). Steps passed: smoke 17, startup-load 20, list-search 27,
semantic-toggle 23, perf-list 36, viewer-basics 77, viewer-search-nav 67, single-file 28, perf-viewer 68 (10k
messages) and 68 (3k), settings-keys 37 (and 37 in the CI shape above), theme 40, palette 40, rename 36, delete 31,
resume-fork 33, export-copy 43, picker 17, picker-exit (exactly one printed line, an existing `.jsonl`), deep-search
57, agent-panel 60, tools 83, cross-pane 247. Measured:
* X-038 (`cross-pane`, 20,030 rows, ms, median / max / UI-thread CPU): load end 1.89 (one sample; CPU 1.92), refresh
  end 0.32 / 0.32 / 0.27, UUID paste 0.16 / 0.28 / 0.22, rename apply 0.16 / 0.17 / 0.18, delete removal 0.81 / 0.94
  / 0.63. `perf-list`: install 0.03, paste median 0.19 (first paste 1.12), frame p95 0.73.
* X-007 (`cross-pane`): 25 s with the transcript focused gave 0 frames and 0.000 ms of UI time. The whole process
  used 0.86 s of CPU in that window (bound 2.5 s). The report-only probe with the search input focused shows the
  caret blink (20 frames, 127 ms over 10 s).
* §15.8 viewer (10k / 3k): first content 219 / 101 ms, install 1.2 / 0.9 ms, idle frame (median of 9) 2.19 / 2.23
  ms, page-through avg 3.3 / 3.4 ms and p95 5.9 / 5.6 ms, toggles ≤ 9.5 ms, first search 14.2 / 4.7 ms, working
  set 163 → 126 MB, peak 199 / 129 MB.
* §15.8 working set (`cross-pane`): 192 MB after the first conversation, +1.7 % after 20 more in a row. The process
  peak, with the 20k-row load and a Deep search over every transcript, was 399.8 MB (reported, not a target of this
  scenario; WP-I measured 388 MB). The OS-reported peaks of the other scenarios on the template were 267–311 MB
  (perf-list with 20k rows 338 MB), and semantic-toggle with the embedding model peaked at 1.3 GB.

**Screenshots** (`scratchpad/shots/phaseC/`, a scenario on a fresh sandbox copy with qa-home merged in, 1440 × 900):
the list alone, the list with the selected conversation in the viewer (highlighted code), a list search with
highlighted matches, and Deep search results with a hit open in the viewer and its anchor marked. Each was taken in
light and dark and reviewed against the §15.5 checklist (title bar and menus, tabs, search field and counts, rows
with lane, title, meta, preview and time, the selected fill, footers and hints, the viewer header, toolbar,
lanes and status bar, the Deep search cards, scores, hits, warnings line and stats footer), together with the
cross-pane QA shots (X-040's auto-hidden list at 880 px next to the agent panel). One minor defect, filed below
[WP4] and not fixed here: a Deep search group names the project `gamma` where the list says `alpha-beta-gamma`.
`deep_search::results::project_label` decodes the project folder name (`C--git-personal-alpha-beta-gamma`, where
every `-` may be a separator), while the list uses core's `project_name` from the transcript's cwd. The labels
match only for paths without a `-` (all of qa-home).

**Linux** (Docker `ubuntu:24.04`, the `ubuntu-latest` family, rustup 1.99.0; the ci.yml Linux job on a copy of the
read-only mounted integrated tree, `--locked`): fmt, the safety lint and its self-test, clippy `-D warnings`
(workspace, and the GUI with `qa`), `cargo check --features release-dynamic-ort`, core tests 1243 passed / 3 ignored
(+ the manifest test and 1 doctest) and with `hermetic` the same + 5 guards, GUI tests 680 passed / 1 ignored and
with `qa` 699 / 1, the release build and its `--version` (`claude-history-gui 0.1.0`). All green (glibc 2.39, static
ONNX Runtime). The GUI counts differ from Windows because the picker-resume and `cmd /C` argument tests are
platform-specific: 2 run on Windows, 1 on Unix. The GUI lib tests were run again as uid 65534 (680 passed), so the
permission-based failure tests that skip as root ran for real, including WP-C3's two new ones.

**Open items after Phase C (consolidated; owners in brackets; historical — the fix round below closed several, see
"Final status" at the end of this section):**
* [user/WP7, GitHub only] `ci.yml` on the three-OS matrix has never run. macOS has never been compiled, so the
  first `macos-latest` run is its first build. Also not yet run: `qa.yml` on the Windows runner (including
  `settings-keys` with `-WithWorkspace` and `cross-pane` at 900 s), the `release.yml` dry run and the macOS bundle.
  `qa.yml` stays Windows-only.
* [WP-I follow-ups] the sporadic 14–70 ms UI-thread CPU hiccup inside an input dispatch; a full main-window frame
  of 10–16 ms at 20k rows with a conversation open (now gated by `perf-viewer-main.qa`: the viewer probe spans the
  whole window's layout and paint, idle median 7.4–8.7 ms vs ~2 ms in a single-file window; page-through not gated,
  too noisy); 0.3–1.1 s of idle process CPU per 25 s on one non-UI thread,
  not attributed; the 2 Hz caret blink of a focused input (gpui-kit's `BlinkCursor` is private).
* [WP-I, not done] X-001/A-055 through gpui-kit's real `AppMenuBar`/`PopupMenu` dispatch and `inject_load_event`
  in the A1/A2 race tests; X-024 coverage of the remaining dialogs.
* [WP4, screenshot review] Deep search group and hit project labels decode the project folder name
  (`results::project_label`), so they differ from the list's cwd-based `project_name` for paths that contain a
  `-` (`gamma` vs `alpha-beta-gamma`). The fix needs the cwd: a core field on `AgentConversationInfo` (CCR) or a lookup
  through the loaded corpus.
* [WP3] red-before-green not run for WP3's production fixes. [WP4] D-014 click and D-019 "No hits" are checked at
  model/action level; D-035's body text is not inspectable. [WP5] T-024, T-008, T-011 acceptance depth, the
  confirmation-title row strings, delete-empty row virtualization and keyboard reach, the Debug search table's
  column-header row (§5.12), no per-row FEATURES sweep.
* [WP-C3 follow-up, low] `discover_agent_keys` takes no cancellation token: a cancelled Deep search job skips the
  second discovery and keeps the short handles. Core could also emit full refs from `finish_resolution`, which
  would mean a core API change.
* [WP1] a macOS Finder open-document route (`on_open_urls`) before `CFBundleDocumentTypes` returns to
  `Info.plist.in`.

#### Fix round (integrator: list → platform_qa_ci → settings_ui → core → viewer → ops → deep_search → tools, read before the release)

Eight fix branches, one commit each on the Phase-C head `8763a4d`, one per area, closed parity and acceptance gaps
left after Phase C. Merged with `git merge --no-ff` in that order, each merge followed by a build and the GUI tests
(§18 step 6). FEATURES rows were updated in place (implementation and acceptance columns). What each brought:
* **list**: a click on the already selected row previews it again (L-065: the viewer may show another
  conversation); when `exclude_projects` hides every row the empty state says `Every conversation is in an excluded
  project`, names them and offers Open Settings (Workspace scope: Show all + Open Settings; L-079). Tests: the
  non-row states rendered with real button clicks (L-005, L-006, L-078, L-079), saving `exclude_projects` reaches
  the open list (L-042), typing and coalescing (S-005, S-007; core `search_state::latest_command`), the L-062
  control. QA: the DSL step `hold <key> <count> [<interval>]` and the probes `window.frame_p95` / `window.frame_max`
  (§15.3); `perf-list.qa` holds PageDown (800) and Down (300) at the 33 ms key-repeat pace and gates the whole
  main-window frame as well as the pane's (X-008).
* **platform_qa_ci**: `CLAUDE_HISTORY_GUI_ORT_SELFTEST=1 <exe>` loads the bundled ONNX Runtime
  (core `semantic::fastembed::verify_onnx_runtime`), prints `onnxruntime ok` and exits without a window or data
  access; `release.yml` verifies the macOS bundle and the Linux tarball with it; `ci.yml` runs the
  `release-dynamic-ort` fastembed tests on every OS (S-032). Real clicks replace handler calls in the D-006, D-013
  and D-014 tests; the D-032 read-only test also reads a Pi and an OMP session; §15.9 gains the S-030 check.
* **settings_ui**: S-033 (`menus::sync_availability`, §8.10: the in-window menu bar greys out the commands the
  active window cannot handle); C-038 (the agent panel's resize handle, 18–28 rem; its tab, visibility and width
  follow the UI and are restored); C-023 (a successful config Retry applies `[keys]`, `exclude_projects`,
  `show_last` and an explicit gui.toml theme live: `settings::apply_reloaded`, `ListPane::set_show_last`, core
  `Browser::set_show_last`); X-026 (the Shortcuts rows say "Refresh conversations").
* **core**: tests only (refresh prewarm S-029, agent_progress export skip A-056).
* **viewer**: V-088 Select All (`SelectAllTranscript`: Ctrl/Cmd+A in the `Viewer` context and Edit → Select All;
  a focused input keeps its own select-all; Copy takes the whole transcript, painted or not); V-101 a superseded
  parse is abandoned mid-file (core `ViewerModel::open_cancellable`, polled every 64 lines); V-045 tables with
  non-ASCII cells lay out one fixed-width element per cell; V-094 the new `perf-viewer-main.qa` (also in
  `qa.yml`); A-055 the toolbar ⋯ dropdown tested through the real PopupMenu.
* **ops**: A-001 Resume and Fork resume in every picker mode take the picker path and quit after a successful
  launch (§11.13 step 3); tests of the toast Reveal / Copy buttons (A-035, A-043).
* **deep_search**: D-010 group and hit project names are the list's (core `project_name` from the recorded cwd),
  so `C--git-personal-alpha-beta-gamma` reads `alpha-beta-gamma`, not `gamma`; hit rows show the score with the sem/lex
  breakdown and a ref/score tooltip; X-024 `Role::ListItem` and accessible names on Deep search cards, hit and
  outline rows, Debug search rows and Delete-empty rows.
* **tools**: T-003 the Delete-empty table is virtualized (`uniform_list`) with the §5.10 headers and keyboard
  selection; T-004 / T-017 tests of what the confirmations list; T-008 the progress percent while running, and
  Generate chosen in a second window never cancels a running one; T-009 a deeper test; T-011 / T-012 a scope
  change after a run reruns it; the Debug search table's header row (§5.12).

Merge conflicts and integrator commits (`integrator: …` on master):
* `tools/delete_empty.rs` (tools vs deep_search): the tools branch moved the row elements into the virtualized
  `build_rows`; the deep_search branch's `Role::ListItem` and accessible name were carried over there.
* **Startup panic** (from the settings_ui branch, found by the first QA run of the integrated build):
  `sync_availability` runs while the title bar renders, and the OS activates a window before its first draw, so
  the first sync queried an empty rendered frame and gpui's `is_action_available` indexed node 0 of an empty
  dispatch tree (`key_dispatch.rs:384`, index out of bounds). Every launch panicked; `smoke.qa` and the console
  check failed. The first active render of each window now schedules the sync for the next frame. The G harness
  cannot reach this (a test window draws its first frame inactive; `TestWindow::is_active` is false until an
  asynchronous activation); every QA scenario starts the real app and does.
* Two new tests raced real threads after the merges (both passed in their branches): L-006's Retry click (the
  failing reload ends at once on the loader thread; the poll loop is now paused around the click) and D-010's
  label test (it compares against the list, so it now waits for the list's load: `DeepFixture::wait_list_loaded`).
  Both passed in every rerun.
* Raw line breaks inside two string literals (the ORT self-test output, the viewer's `all_text` join) written as
  `\n`.
* Seen once under load and not changed (timing-dependent, pre-existing): `list::tests::loading::
  rows_stream_in_while_loading` (the loader delivered every batch and `Loaded` before the first manual poll; 4 of 4
  isolated reruns passed); the ops branch reported `skeleton_tests::contracts::
  a_list_action_flushes_the_pending_preview_first` failing once in its full run.

**DoD on the integrated head** (`00977f5`; Windows 11, MSVC, rustc 1.99, `targets/master`): `cargo fmt --check`;
clippy `-D warnings` for the workspace and for the GUI with `qa`; core tests 1248 passed / 3 ignored (+ the manifest
test and 1 doctest), with `hermetic` the same + 5 hermetic guards, and ci.yml's new `release-dynamic-ort` fastembed
tests (8 passed); GUI tests 716 passed / 1 ignored, with `qa` 736 / 1; release builds with and without `qa`; the
safety lint and its self-test. On the plain release exe: the ORT self-test prints `onnxruntime ok`,
`check-delayload.ps1` passes, `check-console.ps1 -ShellExecute` passes with a fresh sandbox copy as the profile
(no console on a normal launch through `cmd.exe /c` or ShellExecute; `--help`/`--version` reach the redirect in all
four shapes). All green.

**QA (fix round, release `--features qa` of `00977f5`, 2026-10-17 23:40–23:50, timing reruns until 00:35).** Every
scenario of every WP in one run, each on a fresh copy of the scratchpad sandbox template (`.qa-sandbox`), prepared as
in Phase C; the new `perf-viewer-main` like `qa.yml` (`add-synth-corpus.ps1 -Count 20000`, `prepare-sandbox.ps1`,
`add-perf-session.ps1` with `perf-3k.jsonl`). All 23 scenario files in 25 runs passed (exit 0, no FAIL, no SKIP):
smoke 17, startup-load 20, list-search 27, semantic-toggle 23, perf-list 36, viewer-basics 77, viewer-search-nav 67,
single-file 28, perf-viewer 68 (10k) and 68 (3k), settings-keys 37 (and 37 in the CI shape), theme 40, palette 40,
rename 36, delete 31, resume-fork 33, export-copy 43, picker 17, picker-exit (exactly one printed line, an existing
`.jsonl`), deep-search 57, agent-panel 60, tools 83, cross-pane 247, perf-viewer-main 39. The screenshots of the
changed UI were looked at: the Delete-empty table with its header row, the Debug search header row, and Deep search
cards and rows naming `alpha-beta-gamma` with hit scores.

The timing scenarios ran again on fresh copies (perf-list, perf-viewer 10k and perf-viewer-main five times in all,
perf-viewer 3k and cross-pane three times) on the shared, loaded machine; medians of the runs (ms unless stated):
* `perf-list` (20k rows): install (pane) 0.04–0.05; paste median 0.21; `list.frame_p95` 0.78. `window.frame_p95`
  (new, < 16): 11.5, 11.4, **29.4**, **21.5**, 9.6, median 11.5. Not reliably measured: two of five runs failed the
  step; the probe has no UI-thread CPU counterpart, so load and work cannot be told apart. The bound was not changed.
* `perf-viewer` 10k: first content 274; idle frame (median of nine, < 3) 2.51, 2.12, **3.27**, **3.16**, 2.38 —
  median 2.51, passes by the median; in the two failing runs the UI-thread CPU time was as high as the wall time
  (3.42, 3.17), so the frame really took longer there (a throttled CPU raises both). Page-through avg 3.6, p95 6.8;
  toggle totals ≤ 21; first search 19. An interleaved A/B against the Phase-C build (four pairs, fresh sandboxes): idle
  frame median 2.12 (Phase C) vs 2.15 (fix round), page-through avg 3.22 vs 3.68 and p95 5.56 vs 6.65 — the
  page-through is higher in every pair, about 0.4 ms avg (the viewer's per-segment selection and grid code, V-045 /
  V-088, is the likely cause), well inside the 12 / 16 ms bounds; not optimized.
* `perf-viewer` 3k: first content 115; idle frame 2.66; page-through avg 4.0, p95 7.6.
* `perf-viewer-main` (V-094, 20k rows + the 3k transcript, main window): idle frame median 6.70, **46.03**, 3.92,
  6.50, 3.89 (< 12; median 6.50). The failing run's UI-thread CPU median was 8.86 ms against 46 ms wall: the
  thread was not running (load), not working. Page-through (reported): avg 7.5, p95 11.9 (medians; the failing run
  25.6 / 56.6).
* `cross-pane` (X-038, 20,030 rows; median / UI-thread CPU of the medians): load end 2.23 / 2.20, refresh end 0.44 /
  0.57, UUID paste 0.22 / 0.26, rename apply 0.21 / 0.23, delete removal 1.25 / 1.13. Idle (X-007): 0 frames and
  0.000 ms UI time in each 25 s window; whole-process CPU 1.25, 0.50, 0.80 s (bound 2.5 s). Working set 190 MB after
  the first conversation, +2.8 to +4.0 % after 20 more; the reported process peak 401.7, 382.2, 398.2 MB (20k-row
  load plus a Deep search over every transcript; not a target of this scenario).
* Process peaks (OS-reported): `semantic-toggle` with the embedding model 1.3 GB; the other main-window scenarios
  on the template 243–314 MB (perf-list with 20k rows 351 MB); the single-file viewer scenarios 91–94 MB.

**Linux** (Docker `ubuntu:24.04`, rustup 1.99.0, the ci.yml Linux job on a copy of the read-only mounted tree,
`--locked`, now with the `release-dynamic-ort` fastembed tests): fmt, the safety lint and its self-test, clippy
`-D warnings` (workspace, and the GUI with `qa`), `cargo check --features release-dynamic-ort`, the
`release-dynamic-ort` fastembed tests 9 passed (Windows 8: the versioned-name fallback is Unix-only), core tests
1248 passed / 3 ignored (+ the manifest test and 1 doctest) and with `hermetic` the same + 5 guards, GUI tests 712
passed / 1 ignored and with `qa` 732 / 1, the release build and its `--version` (`claude-history-gui 0.1.0`), and
the GUI lib tests again as uid 65534 (712 passed). All green (glibc 2.39). Windows runs 4 GUI tests more than
Linux (716 vs 712; Phase C: 1): the three new A-001 picker launch tests are Windows-only (Unix with a TTY takes the
exec path, A-052's test).

#### Final status (after the fix round; read before the release)

**Done.** As far as Windows and the Linux CI job can verify it, the FEATURES rows are implemented and checked as
each row says, except the open items below; the DoD, every QA scenario on Windows and the ci.yml Linux job pass on
the integrated head (above). Closed by the fix round, from the Phase-C list above: the Deep search project labels
([WP4]); the [WP5] items (T-008 and T-011 acceptance depth, the confirmation strings, Delete-empty virtualization
and keyboard reach, the Debug search header row); the D-014 click; the in-window menu bar's disabled state
(S-033); A-055 through the real PopupMenu for the toolbar ⋯ dropdown; a gate for the main-window viewer frame
(V-094). The parity gaps closed in the fix round are listed per branch above.

**Open, and why:**
* **Needs GitHub runners, a macOS host or the user** [user/WP7]: `ci.yml` on the three-OS matrix, `qa.yml` on the
  Windows runner, the `release.yml` dry run (three archives and their `.sha256`, codesign and dylib checks, the
  tarball check, the new ORT self-test steps) have never run (S-032, X-020, X-021, X-022, X-028). macOS has never
  been compiled: the Terminal.app launch and the Unix `exec_in_current_terminal` resume (A-009, A-052), the
  `cmd-*` keymap rows, `cmd-q`/`cmd-w`, the native menu and the bundle (C-017) are unverified, and the Finder
  open-document route (`on_open_urls` before `CFBundleDocumentTypes` returns to `Info.plist.in`) is not built.
  `qa.yml` stays Windows-only; the §15.9 D checklist (real launches, clean VMs, the S-030 first semantic search)
  is the user's.
* **Performance, not attributable or not reliably measured on the shared dev machine** [WP-I follow-ups]: the
  sporadic 14–70 ms UI-thread CPU spike in about one input dispatch in twenty, outside the app's handlers (S-037;
  `perf-list.qa` judges the paste by the median of three); 0.3–1.1 s of idle process CPU per 25 s on one non-UI
  thread (X-007; the 2.5 s bound is unchanged; `process_cpu_ms` has Windows and Linux implementations, macOS
  returns none); the 2 Hz caret blink of a focused input (gpui-kit's `BlinkCursor` is private); the main-window
  frame with 20k rows and a conversation open costs 4–7 ms when idle (V-094: median 6.5 over five runs, gated
  < 12 ms by `perf-viewer-main.qa`; the page-through there is reported, not gated); `perf-list`'s new
  `window.frame_p95` step is not reliably measured (9.6–29.4 ms over five runs, median 11.5, two runs over the
  16 ms bound; no CPU-time counterpart); the viewer page-through is about 0.4 ms (avg) slower than the Phase-C
  build in an interleaved A/B, inside its bounds. All need a profiler run of the release build on a quiet machine,
  not a code change made from these numbers.
* **Not built** [WP2]: column alignment of fenced code blocks with wide glyphs (V-045 fixes tables only): a fenced
  block is one `SelText` and the fallback glyph advance cannot be forced; it needs a per-cell grid text element
  with selection runs.
* **Test depth** [WP4, WP-I, WP5]: D-019 `No hits` and D-035's out-of-root body text are asserted through model and
  key state (no render-text probe in the harness); the new Debug search and Delete-empty header rows are checked at
  label/cell level (QA ids `debug-search:header`, `empty:header`); the row-menu route is tested for Open only, with
  keyboard selection in the popup (gpui-kit's popup items have no test ids), and the Copy submenu is not exercised
  (A-055); X-001 through the real AppMenuBar dispatch, `inject_load_event` in the A1/A2 race tests and X-024's
  focus restore for the remaining dialogs are not done; red-before-green was not run for WP3's production fixes.
* **Known limitations by design**: the agent panel's drag math assumes the panel is docked at the window's right
  edge (true for both window kinds), and a width dragged in a single-file window is saved to the shared
  `ui.split.agent_panel_width_rem`; single-file panel visibility and tab are not persisted (C-038). The disabled
  set of the in-window menu bar follows the active window's last rendered frame and is not recomputed while a menu
  popup is open (S-033). `ListPane::set_show_last` rebuilds the loader only when no load runs; otherwise the new
  preview mode applies on the next refresh (C-023). The Debug search header row scrolls with the results (not
  sticky). Under `--resume`, Ctrl+R / Ctrl+F use the key pressed, not `--fork-session` (A-001). Deep search
  results that arrive before the list has loaded fall back to the decoded folder name for their project labels
  (D-010).
* **Low** [WP-C3 follow-up]: `discover_agent_keys` takes no cancellation token; core could emit full refs from
  `finish_resolution` (a core API change).
* **Flaky under load (timing)**: `list::tests::loading::rows_stream_in_while_loading` and
  `skeleton_tests::contracts::a_list_action_flushes_the_pending_preview_first` each failed once in a full run on
  the loaded machine and passed in isolated reruns; neither was changed.

---

## 18. Integration order and expected merge conflicts

1. **Gate 0**: core workflow's last merge.
2. **Phase 0** (done): WP0 and WP-C in parallel (disjoint: WP-C touches only `crates/core/**`; WP0 everything else).
   Merged WP-C first, then WP0 (only `Cargo.lock` conflicted; `main.rs` was adapted to the new `LaunchCommand`
   variants, see §17.11). From now on frozen files: `lib.rs`, `app.rs`,
   `actions.rs`, `model.rs`, `globals.rs`, `notify.rs`, `exit.rs`, `widgets/{mod, confirm, path_list, text_view}.rs`, `workspace/mod.rs`
   (until Phase C), `workspace/routing.rs`, `ops/mod.rs`, `test_support/mod.rs`; root manifests, `Cargo.lock`,
   `.cargo/config.toml`, `rust-toolchain.toml` → integrator.
3. **Phase A** (done): WP1, WP2, WP6, WP7 in parallel. Merge order WP7 (CI + QA runner; makes later merges gated) → WP6 →
   WP1 → WP2, each followed by CI + the merged scenarios. As merged: no textual conflicts (the frozen-file edits of WP6
   and WP1 in `skeleton_tests.rs`/`test_support/mod.rs` were disclosed and accepted); the cross-package lints, QA probes
   and requested wiring were fixed by the integrator afterwards (§17.11 "Phase A as built").
4. **Phase B** (done): WP3, WP4, WP5 (and WP-C2 for the Phase-A CCRs) in parallel on the Phase-A head. Merged
   WP-C2 → WP3 → WP4 → WP5, each followed by a build and the GUI tests; no textual or compile conflicts. The
   integrator then applied the requested changes and the screenshot fixes (§17.11 "Phase B as built").
5. **Phase C** (done): WP-C3 (the Phase-B CCRs), WP-L (cross-platform verification, CI) and WP-I (integration) in
   parallel on the Phase-B head. Merged WP-C3 → WP-L → WP-I, each followed by a build and the GUI tests (655 → 655
   → 681). The only textual conflicts were in the WP-I merge, where WP-I and WP-L had made the same Linux test fixes
   and different `settings-keys` CI preparations (WP-L's kept). There were no compile conflicts. The integrator then
   ran the DoD, every QA scenario, the Linux job and the screenshot review (§17.11 "WP-L as built", "Phase C as
   built", "Phase C integration").
6. **Fix round** (done): eight fix branches on the Phase-C head, one per area (list, platform_qa_ci, settings_ui, core,
   viewer, ops, deep_search, tools). Merged in that order, each followed by a build and the GUI tests (681 → 691 → 691
   → 695 → 695 → 701 → 705 → 707 → 716). The only textual conflict was `tools/delete_empty.rs` (the tools branch
   virtualized the rows the deep_search branch had labelled). The integrator then ran the DoD, every QA scenario
   and the Linux job (§17.11 "Fix round" and "Final status").
7. Between merges the integrator applies "requested changes" to frozen files and schedules WP-C2 for CCRs.

Expected conflicts and mitigations:
| Area | Why | Mitigation |
|---|---|---|
| `crates/gui/Cargo.toml`, root manifests, `Cargo.lock` | a WP needs a new crate | complete dependency list in WP0; new deps only via the integrator, `Cargo.lock` regenerated once per merge |
| `workspace/mod.rs` routing | a WP needs a new action/route | all actions and routes exist in WP0; changes via integrator; WP-I owns it in Phase C |
| `model.rs` | a WP needs a new shared type | §7.9 is complete; additions via integrator |
| `menus.rs`/`palette.rs` (WP6) vs other WPs' commands | new commands | complete tree in WP0; WP6 only adds checks/enabled state |
| `services.rs` (WP7) vs callers | new side effect | trait frozen in WP0 (§7.4); new methods via integrator |
| `theme_tokens.rs` (WP6) vs WP1/WP2 colors | new color role | WP0 maps every needed role; additions requested |
| `keymap.rs` (WP6) vs viewer/list bindings | binding tweaks | WP0 writes the complete §8.3/§8.4 tables; WP6 owns changes; others request |
| core (WP-C/WP-C2) vs core workflow | both edit core | WP-C starts after Gate 0; additive APIs except K1 (coordinated) |
| `qa_probe` keys | QA DSL needs probes | key list fixed in §15.3; each pane owner implements its keys |
| test fixtures | several WPs need corpora | per-WP fixture modules and folders (§6) |

---

## 19. Risks and open items

1. **K1 breaking change** (`Browser::conversations()` element type) touches many core tests; WP-C coordinates it in
   one commit. If the async preparation proves hard to fit into `HistoryLoader`, WP-C may expose
   `prepare_corpus`/`install_prepared` only and the GUI runs preparation in `background_spawn` (same UI-thread budget).
2. **Shared embedder**: relies on `FastembedEmbedder: Send`; otherwise per-consumer models (≈80 MB each) and Clear
   must drop each consumer's model (the gate already suspends them).
3. **SelText relies on gpui-base text-selection internals** (public, thinly documented; spike-verified on 0.7.3).
   Pin `gpui-kit = "=0.7.3"`.
4. **Picker mode on Windows**: GUI-subsystem stdout requires redirection or AttachConsole; interactive cmd.exe does
   not wait for GUI processes (README).
5. **macOS/Linux terminal launching and login-shell PATH** are built from tested pure command builders, but real
   launches are D items (§15.9); CI runs only the optional terminal smoke.
6. **Intel macOS** is not shipped in v1 (§16.3); `asset_for("macos", "x86_64")` finds nothing and the update dialog
   says so.
7. **gpui-kit menu/palette details** (`is_action_available` for the palette's disabled state, AppMenuBar `reload`)
   are verified by WP0/WP6 against 0.7.3; fallbacks: palette items always enabled with a no-op toast when unhandled.
8. **Flags taken in this design** (for the orchestrator): theme in gui.toml (PD9), default_workspace before last-used
   and Pi/OMP cwd heuristics (PD3), CreateProcessW dropped in favor of core's verified launchers (PD4), Ctrl+O
   "Print and exit" desktop adaptation (PD5), main-window close quits (PD6), focus mark = tint + thin bar (PD7),
   copy rules over verbatim TUI texts in-app (PD16), config beats remembered state and no time-filter persistence
   (PD17), command palette and AD15–AD20, reserved clipboard/editing keys rejected as user bindings (§8.5),
   occurrence-based viewer search (AD7), AD9 cache generation ignores the time filter, headless `agent`/`--show-dir`/
   `--render` (AD14), Intel macOS dropped.

---

## 20. Appendix: Review resolutions

Every review finding of revision 1, in the order received (lens / title), with the decision and a one-line reason.
"Accepted" means the design and FEATURES were changed as stated; references point to where.

### 20.1 Parity lens

| # | Finding | Decision | Resolution (where) |
|---|---|---|---|
| P1 | Enter confirms destructive dialogs; TUI accepted only y/Y and n/N | Accepted | Destructive confirms bind `enter` → no-op, `y`/`shift-y` confirm, `n`/`shift-n`/Esc cancel (AD18, §5.7, §8.3); applies to delete, delete-empty, delete-by-ID, Clear cache, copy with overwrites, export Replace; A-025 acceptance as requested. |
| P2 | Remembered state.json values override config defaults | Accepted | Precedence CLI > config key > remembered (restore_session) > default; time filter never persisted (PD17, §11.1 table); P table tests C-066, S-034; Settings shows "applies" and the remembered source. |
| P3 | gui.toml read/parse failure unhandled; later save overwrites it | Accepted | Files loaded independently; dialog names the failing file; every gui.toml write blocked while unparseable (K12 refuses); comment-preserving change-set saves (§5.20, §11.18–11.19; C-023, C-059). |
| P4 | Truncated tool output counted in logical lines | Accepted | Truncation in ledger rows at the estimated column width (K9 `ledger_visible_rows`, `ledger_hidden_lines`), long call lines clipped, recomputed on width change (§9.2; V-056, V-057, V-064). |
| P5 | Agent panel results not tied to their conversation | Accepted | Result sets store target + full ref; `AgentPanelEvent::Reveal { target, .. }` opens the right conversation; Outline re-runs and Within/Read show a stale banner on `TargetChanged` (§7.2, §11.14; D-034, D-011). |
| P6 | ~200 rows without concrete checks; R undefined; V-009 wrong; A1 races only for delete | Accepted | Every row now has a concrete check (machine-validated), R and D are defined (§15.1, §15.7, §15.9), V-009 corrected, A1 races cover rename, resume/fork (list and viewer), export/copy (A-028), menus/palette (X-001) and Ctrl+S snapshot (L-070). |
| P7 | Deep Search: `[agent] scope` ignored, within/outline budget missing, OR semantics, no cancellation | Accepted | Scope Default → config; Budget/No budget on Within, Outline, Read; config-true flags shown on and disabled; per-kind single-flight with cancellation (K7) and a Cancel button (§5.4, §11.14; D-003, D-015, D-019, D-033, D-036). |
| P8 | Within/Read inputs inconsistent; multi-ref Read unspecified | Accepted | Within always targets the viewer's conversation (no field); Read accepts several refs (whitespace/comma separated), renders them grouped per conversation, paging only for single-ref reads (§5.4; D-019, D-021, D-023). |
| P9 | Agent panel cannot resolve files outside the storage roots | Accepted | K8 `conversation_key_for_path` returns a key only for the exact in-root file; otherwise the panel is disabled with an explanation (§11.12; D-035). |
| P10 | Ctrl+O output options and `--debug` display diagnostics unspecified | Accepted | Options from startup `EffectiveSettings`, never live toggles; upstream order of `[INFO] Logged …`, `[ERROR] Failed to parse line …` and display-error blocks (K11) (PD5; A-048, A-059, A-060). |
| P11 | `--local` and the time filter not passed to one-shot tools | Accepted | `StartupTool` variants carry `local`; tools use a fresh load with the launch time filter, never the list corpus (§11.24; C-014, C-015, T-011). |
| P12 | Show projects directory with no workspace undefined | Accepted | Item disabled with `Choose a workspace first`; `--show-dir` without a workspace → core error on stderr, exit 1, or a toast without a console (§5.20; C-012, T-016). |
| P13 | Tabs not expanded | Accepted | K10 exposes `markdown::expand_tabs`; every segment builder uses it (§9.3; V-100). |
| P14 | Header shows tokens when zero | Accepted | Omission rules for tokens (0), model, duration, custom title, summary + P test (§9.10; V-004). |
| P15 | Rename Ctrl+U/E and list Ctrl+F parity gaps | Accepted | `RenameDialog > Input` binds Ctrl+U clear and Ctrl+E end; list `ctrl-f` → MoveRight registered while Fork is not on Ctrl+F (AD19, §8.4; A-018, L-060). |
| P16 | CLI parser deviations (exit code, validator message, missing conflict) | Accepted | Inverted range = runtime `Error: Invalid time range: …` exit 1; `invalid positive integer: <v>`; `--delete`×`--global` (K14, §11.1, §13.1; C-005, C-016, C-020). |
| P17 | Window-close semantics contradict PD5/PD6 | Accepted | Closing the main window always quits (picker: no output) via its should-close handler (PD6, §11.22; C-065, X-033). |
| P18 | Uncached-passages banner tells users to run the CLI | Accepted | Banner `<n> passage(s) are not cached. Generate the semantic cache to include them.` + button; the upstream line stays only in the agent text (§12; D-006). |
| P19 | Clear/Generate don't reset the agent embedder; concurrent cache writers | Accepted | Semantic gate suspends and joins the list worker (K6), pauses agent semantic jobs, drops the shared model before Clear; core write lock (K6) (§7.6, §11.16; T-010, T-024, X-041). |
| P20 | Viewer scroll keys active while typing in the find bar | Accepted | All viewer navigation keys use `Viewer && !Input`, matching V-079 parity (§8.1, §8.3; V-079). |
| P21 | Time-filter/workspace reloads keep the old list on errors | Accepted | Reloads use K18 `RefreshPolicy::ApplyPartial` with a warning; a fatal failure reverts the chip/workspace with an error toast (§11.20–11.21). |
| P22 | Deletes from Tools don't clear the viewer or dialogs | Accepted | Every deleting route broadcasts `CorpusNotice::Deleted`; Workspace removes rows, clears the viewer, closes registered dialogs and marks agent result sets (§7.2, §11.15; T-023). |
| P23 | Reconsider Ctrl+O quit, stdout one-shots, headless agent | Accepted | (a) Ctrl+O prints at once only when stdout is redirected, confirms with a terminal, toasts without a console; (b) `--show-dir` and `--render` print headless when stdout is available; (c) headless `agent` subcommand through K15 (PD5, AD14, §11.23; A-063, C-011, C-012, C-061). |
| P24 | PD3 changes env-variable and cwd semantics | Accepted | Relative env paths resolve against the launch cwd (K16); a cwd recorded by Pi/OMP sessions counts as meaningful; `-L` always uses the launch cwd (PD3; L-082, C-044). |

### 20.2 Feasibility lens

| # | Finding | Decision | Resolution (where) |
|---|---|---|---|
| F1 | Core Browser mutations freeze the UI thread with 10k+ conversations | Accepted | K1 off-thread preparation + single `Arc<Conversation>` storage + `corpus_snapshot`, K2 async UUID load, K3 async rename, K4 async delete; UI-thread timing probes on 20k rows (§4.3, §7.5, §15.8; X-038). |
| F2 | x86_64 macOS release cannot work; static ORT on macOS/Linux unverified | Accepted | Windows static ORT; macOS arm64 and Linux bundle official ORT 1.24.2 with `release-dynamic-ort` (upstream's approach); Intel macOS dropped for v1; frozen artifact names; extra `[profile.dev.package]` entries (§16.1, §16.3; X-020…X-022, X-028). |
| F3 | Menu bar is mouse-only on Windows/Linux | Accepted | Command palette (`secondary-shift-p`, `f10`) from the menu tree + AD17 default shortcuts; X-025 rewritten (§8.9; C-057, X-025). |
| F4 | Per-window menu variants unsupported | Accepted (option a) | One tree for all windows; items without a handler are disabled; rebuild + `reload()` on activation (§8.10; C-055). |
| F5 | Ctrl+W closes the main window from text fields | Accepted | `ctrl-w` = delete word in every Input (Windows/Linux); CloseWindow only in `SingleFileWindow` (§8.3–§8.4; C-056, C-064). |
| F6 | A large tool output/code block is one list item | Accepted | ≈100-line chunk items with stable `(block, chunk)` ids, lanes on the first chunk, search/reveal mapped to chunks; 5k-line outputs in the perf fixture (§9.2, §15.8; V-099). |
| F7 | Viewer open pipeline has no cancellation; search buffers multiply memory | Accepted | Single-flight `viewer-worker` with generation checks; lazy search texts with an ASCII fast path; previous document released on install (§9.1, §9.7; V-101, V-102). |
| F8 | Several uncoordinated cache writers and embedders | Partly accepted | Adopted: exclusive gate with suspend+join for Generate/Clear (K6), one shared embedder, core cross-process write lock. Not adopted: a single SemanticService thread for Browser/agent/tools (large core refactor); remaining list-vs-agent writes are serialized by the lock and only cost cache entries (§7.6). |
| F9 | Pane minimums don't fit the minimum window | Accepted | Viewer min 32rem, list 18–40rem, agent panel auto-collapses the list, widths clamped on startup/resize (PD1; L-088, X-040). |
| F10 | Irreversible deletes should use AlertDialog naming the target | Accepted | `widgets/confirm.rs` AlertDialog pattern with `Delete "<title>"?`, `Clear semantic cache?`, `Delete N empty transcripts?` (§5.7, §5.10, §5.11, §5.15). |
| F11 | Notification action API mismatch | Accepted | `.autohide(true)` after `.action`, two-button toasts via `.content`, `id1` replace keys (PD16, §6 notify.rs). |
| F12 | Shift+F10 cannot open a ContextMenuExt menu | Accepted | NativeMenu for both right-click and keyboard, positioned at recorded row bounds; token-captured targets (§5.2; L-068). |
| F13 | Tab = ToggleScope keeps focus stuck | Accepted | Propagate without a workspace; F6/Shift+F6 region cycling listed in Shortcuts (§7.7, AD16; L-046, C-058). |
| F14 | Quit/help shortcuts don't work while a dialog has focus | Accepted | `secondary-q` with no context + global handler; macOS Quit item handled globally (§8.3; C-063). |
| F15 | Live rebinding deletes the static F5 Refresh binding | Accepted | Only the remembered user bindings (UserBindingSet) are removed (§8.6; C-034). |
| F16 | ListState reset/remeasure don't do what was assumed | Accepted | `reset_with_uniform_height` on open; width changes use an anchored rebuild, never `remeasure()` alone (§9.6; V-080). |
| F17 | Appended extension skips the overwrite prompt; Linux portal | Accepted | Replace prompt when the appended path exists; save-dialog `Err` fallback "Export to Downloads" (PD8; A-035, A-061). |
| F18 | Footer and status-bar hints overflow | Accepted | State first; hints only for the focused region; priority trimming keeping `F1 help`; activity replaces hints (§5.1, §8.7; L-040). |
| F19 | List row line 1 too dense | Accepted | tig-style three-line rows and a 30rem default width (AD20, §10.1; L-016…L-018). |
| F20 | Deep Search crammed into the left pane | Accepted | Options disclosure, compact cards (refs in tooltips/copy buttons), own 34rem width, results kept across tab switches (PD1, §5.4). |
| F21 | AgentPanel inherits the viewer's key bindings | Accepted | The panel is a sibling of the `Viewer` context with its own bindings (§3, §7.7, §8.3; X-044). |
| F22 | Focus-lost fallback ignores the visible tab | Accepted | Fallback = the visible tab's primary element, then the viewer; tab switches focus their primary element (§7.7; X-043). |
| F23 | macOS Cmd+Shift+F collides; AD2 doesn't hold on macOS | Accepted | `Workspace > Input` duplicate; AD2 is Windows/Linux only, macOS keeps emacs Ctrl+A/E (AD2, §8.8). |
| F24 | Viewer focus gutter conflicts with the rulebook | Accepted with flag | Background tint is the marker; a thin 2 px accent bar is kept because PD7 asks for a gutter (PD7, §9.4; V-069). |
| F25 | UI copy and terminology conflicts | Accepted | Copy rules: `…` and "conversation" in-app, verbatim only for text leaving the app; no duplicated feedback (§0, §12, PD16 flag). |
| F26 | Error states have no recovery action | Accepted | Every §12 error state lists its recovery (Retry, Show debug log, Switch to lexical, Choose workspace…, Open Settings). |
| F27 | Selection/copy limits; too many selection handles | Accepted | Partial-copy toasts, SmartCopy routing from the list input, handle-count LRU, one SelText per table row (§9.3–§9.7; V-088, V-103). |
| F28 | Large plain-text views should use a read-only Textarea | Accepted | `widgets/text_view.rs` read-only Textarea for agent text, reports and the debug log (§5.4, §5.13, §5.14). |
| F29 | Test-isolation claims wrong; global state races | Accepted | K13 `hermetic` via dev-dependency, ctor env redirect, `test_support::global_lock` for theme/debug globals (§15.2; X-036). |
| F30 | Performance acceptance doesn't cover the goal | Accepted | 10k-message fixture with 5k-line outputs and explicit targets; 20k-row list UI-thread timings (§15.8; V-094, X-038). |
| F31 | Terminal launching gaps on every OS | Accepted | K17: ptyxis/kgx/tilix, candidate fallback on spawn errors, early-exit check (e.g. missing iTerm), reaping (core); Windows falls back to the console (§14.1; A-015). |
| F32 | Login-shell PATH probe misses rc files; races early resumes | Accepted | `$SHELL -ilc` with markers and a 3 s timeout; resume waits for the probe (§14.2). |
| F33 | "Meaningful launch cwd" can pick `/` or home | Accepted | Filesystem roots, home and the exe/.app directory are excluded (PD3; L-082). |
| F34 | Picker with extra windows hangs; no way back to the main window | Accepted | Main-window close quits; Open JSONL disabled in picker mode; File → Show conversations (PD5, PD6; C-065, T-019). |
| F35 | Non-picker Ctrl+O clashes with desktop Open | Accepted | Print at once only with redirected stdout; confirmation with a terminal; toast otherwise; menu label "Print and exit" (PD5; A-063). |
| F36 | Content colors from the TUI palette; light-theme contrast | Accepted | Kit tokens wherever they exist; core palette only for missing roles; ≥ 4.5:1 contrast test (§5 intro; X-039). |
| F37 | gui.toml writes lose concurrent changes and comments | Accepted | K12 change-set save with `toml_edit`; Settings drafts refresh unedited fields from AppState (§11.18; C-060). |
| F38 | Workspace picker entries computed in the menu builder | Accepted | Project paths computed off-thread after each load/refresh and cached on ListPane (PD3, §7.5; L-081). |

### 20.3 Breakdown lens

| # | Finding | Decision | Resolution (where) |
|---|---|---|---|
| B1 | Core baseline out of date; WP-C would rewrite landed APIs; GUI duplicates core | Accepted | Re-baselined at `3ba9f90` with real signatures (§4.1–§4.2); WP-C = genuine gaps K1–K18, additive except K1; duplicated GUI logic and mirror types removed; core-only rows owned by WP-C; CCR path (§4.4). |
| B2 | Terminal launching/resume planning duplicated and conflicting with core | Accepted | Core owns planning and command construction; seam `PlatformServices::spawn_terminal(&TerminalCommand)`; `resume::launch` forbidden by lint; `[gui] terminal` uses core syntax; core texts (PD4, §14.1, §16.2). PD4's raw CreateProcessW requirement is dropped with a flag. |
| B3 | GUI tests not covered by core's `cfg(test)` guards | Accepted | K13 `hermetic` feature from gui dev-dependencies, ctor env redirect, refusing defaults in `init_for_test`, CI safety lint; single owners X-036 (WP0), X-042 (WP7) (§15.2, §16.2). |
| B4 | PlatformServices misses dialogs, clipboard, update fetch, TTY detection | Accepted | Trait extended with prompts, clipboard, `fetch_latest_release`, `stdout_kind`, `login_shell_path`; Recording answers + DSL steps; `--qa-real-console` (§7.4, §15.3). |
| B5 | Menu-bar session actions can act on the live list selection | Accepted | Session actions handled by the focused pane; ActivePane fallback; pending preview flushed; title in toasts; G tests through the AppMenuBar path (§7.8, §8.1; X-001, L-085). |
| B6 | `from_path` defaults an unknown source to Claude | Accepted | `source: Option<Source>`; hits use `AgentConversationInfo`; unknown source disables destructive/resume actions (§7.8; A-062, X-002). |
| B7 | Parallel WPs cannot meet acceptance in isolation; merge order contradicts deps | Accepted | Phases 0/A/B/C; Phase-A packages test their own entity; scenarios owned by their WP; WP-I owns cross-pane rows and `workspace/mod.rs` in Phase C (§17, §18). |
| B8 | Frozen `app.rs` contains other packages' logic | Accepted | `app.rs` only calls owner functions in a fixed order (§7.3); picker cancel via frozen `exit::quit_without_output`; L-089, A-049, C-003 reassigned to WP1. |
| B9 | FEATURES IDs not mapped to exactly one WP; unverifiable acceptances; R undefined | Accepted | One owner per row (validated against the index), core-only acceptances owned by WP-C, R/D defined, concrete checks in every row. |
| B10 | Binary-only crate fails clippy on dead code | Accepted | `crates/gui` = library + thin binary; no crate-level `allow(dead_code)` (§3, §16.1). |
| B11 | Agent worker and Tools bypass the embedder seam | Accepted | One `SharedEmbedder` with adapters for the three core factory shapes; refusing default in tests; `--qa-model-dir` applies everywhere (§7.6). |
| B12 | QA sandbox gate incomplete; dev runs not gated | Accepted | Gate checks every path variable and the OS-derived profile, runs in every debug build unless `CHG_ALLOW_REAL_HOME=1`; debug launch guard (§15.4; X-004, X-037). |
| B13 | CI workflow incomplete | Accepted | `qa`/`release-dynamic-ort` feature builds, `qa.yml` dispatch job, `RUST_MIN_STACK`, pinned toolchain, safety lint, hermetic core run, pre-gate core check via CCR (§16.2). |
| B14 | Release workflow cannot build x86_64 macOS; packaging details missing | Accepted | Intel macOS dropped for v1; dynamic ORT bundling; `ditto`; deployment target and `LSMinimumSystemVersion`; Info.plist version from Cargo; `vswhere`; CI before publish; dry run; notes from CHANGELOG; `dumpbin` check (§16.3). |
| B15 | Types used in WP0 stubs missing or contradictory | Accepted | Complete §7.9 type list; `AgentRun`/`AgentFailure` removed (core `AgentResponse`); WP0 avoids K-item types in frozen signatures; typed `Services.recording`; QaExt for any `ParentElement + Styled`; typed pane getters (§7.4, §7.9, §17). |
| B16 | Shared GUI behavior duplicated across packages | Accepted | `widgets/time_range.rs` (WP1, Phase A, used by WP4), `widgets/{path_list, text_view, confirm}.rs` frozen in WP0 (used by WP3, WP4, WP5 in parallel), `workspace/routing.rs` for both window roots, core `ExportOptions` used directly, per-WP fixture modules (§6). |
| B17 | Failure-path fixtures not cross-platform | Accepted | `test_support::fault` per OS (share-mode lock / read-only attribute on Windows, chmod on Unix, root skip) (§15.10). |
| B18 | Manual DoD items impossible on the Windows-only dev machine | Accepted | Code D = deferred user checklist (§15.9), not blocking; CI smoke steps and the release dry run automate what they can (§16.2–§16.3). |
| B19 | Shared manifests and fixture locations have no owner | Accepted | WP0 then the integrator own root manifests, lock, `.cargo/config.toml`, toolchain; fixtures in `crates/gui/tests/fixtures/<owner>/` (§6, §18). |
