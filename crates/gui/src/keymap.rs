//! The keymap (DESIGN §8.3–§8.8). WP0 → WP6.
//!
//! Registration order inside [`install_all`] (after `gpui_kit::init`):
//! 1. the static table (§8.3) in table order, incl. the `ConversationList > Input`
//!    and `Workspace > Input` duplicates that beat the focused Input;
//! 2. the input-editing bindings (§8.4);
//! 3. the user-configurable bindings (§8.5) in the order rename, refresh,
//!    fork, resume, delete — later wins at equal depth, which reproduces the
//!    TUI check order delete > resume > fork > refresh > rename (C-028).
//!    They are validated first, at startup too: a rejected binding (reserved
//!    clipboard/editing keys, §8.5) keeps the default and is reported in the
//!    debug log, like every warning.
//!
//! Two static rows are conditional on the user bindings and re-evaluated by
//! [`install_user_bindings`]: `ctrl-t` ToggleSearchMode (not registered when an
//! action is bound to Ctrl+T, C-029) and the list search's `ctrl-f` MoveRight
//! (only while Fork is not on Ctrl+F). Static rows such as `f5 → Refresh` are
//! never removed by a rebinding (C-034). User strings never reach
//! `KeyBinding::new` (it panics); `KeyChord::gpui_keystroke` produces GPUI syntax.
//!
//! Region keys never steal a focused button's keys (GPUI dispatches key
//! bindings before a button's key-down activation): the list's keys are bound
//! in `ConversationList && !ListToolbar` ([`CTX_LIST_KEYS`]), Deep search's
//! hit keys in its results list ([`CTX_DEEP_RESULTS`]) and the agent panel's
//! row keys in its row list ([`CTX_AGENT_ROWS`]), so Enter/Space/Tab/arrows on
//! a toolbar or form button reach the button.

use std::rc::Rc;

use claude_history_core::config::{Key, KeyBindings, KeyChord, KeysConfig};
use claude_history_core::debug::DebugLevel;
use gpui_kit::base::actions::{Cancel, Confirm, SelectDown, SelectUp};
use gpui_kit::component::input::{
    DeleteToBeginningOfLine, DeleteToEndOfLine, DeleteToPreviousWordStart, MoveEnd, MoveLeft,
    MoveRight, MoveToNextWord, MoveToPreviousWord,
};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::{
    Action, App, DummyKeyboardMapper, Global, KeyBinding, KeyBindingContextPredicate, KeyContext,
    Keymap, SharedString,
};

use crate::actions::*;
use crate::model::{HelpSection, KeyAction, KeyValidation};

/// The conversation list's key context (search input + toolbar + rows + footer).
pub const CTX_LIST: &str = "ConversationList";
/// The list's keys: anywhere in the list except its toolbar (whose buttons
/// keep Enter/Space/Tab/arrows).
pub const CTX_LIST_KEYS: &str = "ConversationList && !ListToolbar";
/// The list toolbar's key context (an F6 region, AD16).
pub const CTX_LIST_TOOLBAR: &str = "ListToolbar";
/// The list's search input (beats the Input's own bindings).
pub const CTX_LIST_INPUT: &str = "ConversationList > Input";
/// Viewer keys that must not fire while typing in the find input.
pub const CTX_VIEWER_KEYS: &str = "Viewer && !Input";
/// The viewer's find input.
pub const CTX_VIEWER_INPUT: &str = "Viewer > Input";
pub const CTX_VIEWER: &str = "Viewer";
pub const CTX_WORKSPACE: &str = "Workspace";
pub const CTX_WORKSPACE_INPUT: &str = "Workspace > Input";
pub const CTX_SINGLE_FILE: &str = "SingleFileWindow";
pub const CTX_DEEP: &str = "DeepSearch";
pub const CTX_DEEP_KEYS: &str = "DeepSearch && !Input";
pub const CTX_DEEP_INPUT: &str = "DeepSearch > Input";
/// Deep search's focusable results list (hit navigation and open).
pub const CTX_DEEP_RESULTS: &str = "DeepResults";
pub const CTX_AGENT: &str = "AgentPanel";
pub const CTX_AGENT_KEYS: &str = "AgentPanel && !Input";
pub const CTX_AGENT_INPUT: &str = "AgentPanel > Input";
/// The agent panel's focusable row list (outline/hit/read rows).
pub const CTX_AGENT_ROWS: &str = "AgentRows";
pub const CTX_CONFIRM: &str = "Confirm";
pub const CTX_RENAME_INPUT: &str = "RenameDialog > Input";
pub const CTX_EXPORT_MENU: &str = "ExportMenu";
pub const CTX_INPUT: &str = "Input";

/// The contexts a user-configurable binding is registered in (§8.5).
pub const USER_BINDING_CONTEXTS: [&str; 3] = [CTX_LIST_KEYS, CTX_LIST_INPUT, CTX_VIEWER_KEYS];

const MAC: bool = cfg!(target_os = "macos");

fn kb<A: Action>(keys: &str, action: A, context: &str) -> KeyBinding {
    KeyBinding::new(keys, action, (!context.is_empty()).then_some(context))
}

/// One binding of `action` for each of `keys` in each of `contexts`.
fn rows<A: Action + Clone>(out: &mut Vec<KeyBinding>, keys: &[&str], action: A, contexts: &[&str]) {
    for context in contexts {
        for key in keys {
            out.push(kb(key, action.clone(), context));
        }
    }
}

/// The static table (§8.3) followed by the input-editing table (§8.4),
/// platform-filtered, without the two conditional rows (see [`conditional_bindings`]).
pub fn static_bindings() -> Vec<KeyBinding> {
    let mut b = Vec::with_capacity(200);
    let ws_sf = [CTX_WORKSPACE, CTX_SINGLE_FILE];

    // --- app / window -------------------------------------------------------------
    rows(&mut b, &["secondary-q"], Quit, &[""]);
    rows(&mut b, &["secondary-w"], CloseWindow, &[CTX_SINGLE_FILE]);
    if !MAC {
        rows(&mut b, &["alt-f4"], CloseWindow, &[""]);
    }
    rows(&mut b, &["secondary-,"], OpenSettings, &ws_sf);
    rows(&mut b, &["f1"], ShowShortcuts, &ws_sf);
    rows(
        &mut b,
        &["?"],
        ShowShortcuts,
        &[CTX_VIEWER_KEYS, CTX_DEEP_KEYS, CTX_AGENT_KEYS],
    );
    rows(
        &mut b,
        &["secondary-shift-p", "f10"],
        CommandPalette,
        &ws_sf,
    );
    rows(&mut b, &["f6"], FocusNextRegion, &[CTX_WORKSPACE]);
    rows(&mut b, &["shift-f6"], FocusPrevRegion, &[CTX_WORKSPACE]);
    rows(&mut b, &["secondary-shift-o"], OpenJsonlFile, &ws_sf);
    rows(
        &mut b,
        &["secondary-shift-w"],
        ChooseWorkspace,
        &[CTX_WORKSPACE],
    );
    rows(
        &mut b,
        &["secondary-1"],
        FocusConversations,
        &[CTX_WORKSPACE],
    );
    rows(
        &mut b,
        &["secondary-2", "secondary-shift-f"],
        FocusDeepSearch,
        &[CTX_WORKSPACE, CTX_WORKSPACE_INPUT],
    );
    rows(&mut b, &["secondary-3"], FocusViewer, &[CTX_WORKSPACE]);
    rows(
        &mut b,
        &["secondary-shift-b"],
        ToggleListPane,
        &[CTX_WORKSPACE],
    );
    rows(&mut b, &["f5"], Refresh, &[CTX_WORKSPACE]);
    rows(
        &mut b,
        &[if MAC { "cmd-c" } else { "ctrl-c" }],
        SmartCopy,
        &[CTX_WORKSPACE, CTX_SINGLE_FILE, CTX_LIST_INPUT],
    );
    // Select All over the transcript (V-088). Inputs bind their own
    // `ctrl-a` in a deeper context, which wins over this one.
    rows(
        &mut b,
        &[if MAC { "cmd-a" } else { "ctrl-a" }],
        SelectAllTranscript,
        &[CTX_VIEWER],
    );

    // --- session ------------------------------------------------------------------
    rows(&mut b, &["enter"], OpenConversation, &[CTX_LIST_KEYS]);
    rows(&mut b, &["ctrl-o"], SelectAndExit, &[CTX_LIST_KEYS]);
    rows(&mut b, &["ctrl-s"], ShowSemanticDetails, &[CTX_WORKSPACE]);
    rows(&mut b, &["secondary-shift-r"], RevealInFileManager, &ws_sf);
    rows(
        &mut b,
        &["secondary-shift-i"],
        SearchWithinConversation,
        &[CTX_WORKSPACE],
    );
    rows(&mut b, &["secondary-shift-m"], ReadRange, &[CTX_WORKSPACE]);
    rows(&mut b, &["secondary-shift-l"], ShowDebugLog, &ws_sf);

    // --- list ---------------------------------------------------------------------
    rows(&mut b, &["up", "ctrl-p"], SelectPrev, &[CTX_LIST_KEYS]);
    rows(&mut b, &["down", "ctrl-n"], SelectNext, &[CTX_LIST_KEYS]);
    rows(&mut b, &["pageup"], SelectPageUp, &[CTX_LIST_KEYS]);
    rows(&mut b, &["pagedown"], SelectPageDown, &[CTX_LIST_KEYS]);
    rows(&mut b, &["ctrl-d"], SelectHalfPageDown, &[CTX_LIST_KEYS]);
    rows(
        &mut b,
        &["home"],
        SelectFirst,
        &[CTX_LIST_KEYS, CTX_LIST_INPUT],
    );
    rows(
        &mut b,
        &["end"],
        SelectLast,
        &[CTX_LIST_KEYS, CTX_LIST_INPUT],
    );
    rows(&mut b, &["escape"], ClearQuery, &[CTX_LIST_KEYS]);
    rows(&mut b, &["tab"], ToggleScope, &[CTX_LIST_KEYS]);
    // ctrl-t ToggleSearchMode: conditional, see `conditional_bindings`.
    if MAC {
        rows(&mut b, &["shift-f10"], OpenRowContextMenu, &[CTX_LIST_KEYS]);
    } else {
        rows(
            &mut b,
            &["shift-f10", "menu"],
            OpenRowContextMenu,
            &[CTX_LIST_KEYS],
        );
    }

    // --- viewer -------------------------------------------------------------------
    let vk = [CTX_VIEWER_KEYS];
    rows(&mut b, &["j", "down"], ScrollLineDown, &vk);
    rows(&mut b, &["k", "up"], ScrollLineUp, &vk);
    rows(&mut b, &["shift-j", "]"], NextMessage, &vk);
    rows(&mut b, &["shift-k", "["], PrevMessage, &vk);
    rows(&mut b, &["d", "ctrl-d"], ScrollHalfPageDown, &vk);
    rows(&mut b, &["u", "ctrl-u"], ScrollHalfPageUp, &vk);
    rows(&mut b, &["pagedown"], ScrollPageDown, &vk);
    rows(&mut b, &["pageup"], ScrollPageUp, &vk);
    rows(&mut b, &["g", "home"], ScrollTop, &vk);
    rows(&mut b, &["shift-g", "end"], ScrollBottom, &vk);
    rows(&mut b, &["/"], StartSearch, &vk);
    rows(&mut b, &["n"], NextMatch, &vk);
    rows(&mut b, &["shift-n"], PrevMatch, &vk);
    if MAC {
        rows(&mut b, &["cmd-g"], NextMatch, &[CTX_VIEWER]);
        rows(&mut b, &["cmd-shift-g"], PrevMatch, &[CTX_VIEWER]);
    } else {
        rows(&mut b, &["f3"], NextMatch, &[CTX_VIEWER]);
        rows(&mut b, &["shift-f3"], PrevMatch, &[CTX_VIEWER]);
    }
    rows(&mut b, &["enter"], CommitSearch, &[CTX_VIEWER_INPUT]);
    rows(&mut b, &["escape"], CancelSearch, &[CTX_VIEWER_INPUT]);
    rows(&mut b, &["t"], CycleToolDisplay, &vk);
    rows(&mut b, &["shift-t"], ToggleThinking, &vk);
    rows(&mut b, &["i"], ToggleTiming, &vk);
    rows(&mut b, &["x"], ToggleOutputs, &vk);
    rows(&mut b, &["escape"], ViewerEscape, &[CTX_VIEWER]);
    rows(&mut b, &["q"], ViewerBack, &vk);
    rows(&mut b, &["e"], ExportConversation, &vk);
    rows(&mut b, &["y"], YankOrCopyMessage, &vk);
    rows(&mut b, &["shift-y"], CopyPath, &vk);
    rows(&mut b, &["shift-i"], CopySessionId, &vk);
    rows(&mut b, &["p"], ShowPath, &vk);
    rows(
        &mut b,
        &["o"],
        ToggleOutline,
        &[CTX_VIEWER_KEYS, CTX_AGENT_KEYS],
    );

    // --- agent panel / deep search ------------------------------------------------
    rows(&mut b, &["escape"], AgentPanelEscape, &[CTX_AGENT]);
    rows(&mut b, &["down", "j"], AgentSelectNext, &[CTX_AGENT_ROWS]);
    rows(&mut b, &["up", "k"], AgentSelectPrev, &[CTX_AGENT_ROWS]);
    rows(&mut b, &["enter"], AgentOpenSelected, &[CTX_AGENT_ROWS]);
    rows(
        &mut b,
        &["enter"],
        RunDeepSearch,
        &[CTX_DEEP_INPUT, CTX_AGENT_INPUT],
    );
    rows(&mut b, &["escape"], DeepSearchEscape, &[CTX_DEEP]);
    rows(&mut b, &["down"], DeepSelectNext, &[CTX_DEEP_RESULTS]);
    rows(&mut b, &["up"], DeepSelectPrev, &[CTX_DEEP_RESULTS]);
    // `secondary-enter` also opens the hit; the handler moves focus to the
    // viewer when the secondary modifier is held.
    rows(
        &mut b,
        &["enter", "secondary-enter"],
        DeepOpenHit,
        &[CTX_DEEP_RESULTS],
    );

    // --- dialogs ------------------------------------------------------------------
    rows(&mut b, &["y", "shift-y"], ConfirmYes, &[CTX_CONFIRM]);
    rows(&mut b, &["n", "shift-n"], ConfirmNo, &[CTX_CONFIRM]);
    rows(&mut b, &["enter"], ConfirmEnterIgnored, &[CTX_CONFIRM]);
    rows(&mut b, &["ctrl-u"], RenameClear, &[CTX_RENAME_INPUT]);
    rows(&mut b, &["1"], ExportPick1, &[CTX_EXPORT_MENU]);
    rows(&mut b, &["2"], ExportPick2, &[CTX_EXPORT_MENU]);
    rows(&mut b, &["3"], ExportPick3, &[CTX_EXPORT_MENU]);
    rows(&mut b, &["4"], ExportPick4, &[CTX_EXPORT_MENU]);
    rows(&mut b, &["j", "down"], SelectDown, &[CTX_EXPORT_MENU]);
    rows(&mut b, &["k", "up"], SelectUp, &[CTX_EXPORT_MENU]);
    rows(
        &mut b,
        &["enter"],
        Confirm { secondary: false },
        &[CTX_EXPORT_MENU],
    );
    rows(&mut b, &["escape"], Cancel, &[CTX_EXPORT_MENU]);

    b.extend(input_editing_bindings());
    b
}

/// The input-editing bindings (§8.4), registered after gpui-kit's own.
pub fn input_editing_bindings() -> Vec<KeyBinding> {
    let mut b = Vec::with_capacity(16);
    if !MAC {
        // Ctrl+W deletes a word in every input; it never closes a window (PD6).
        rows(&mut b, &["ctrl-w"], DeleteToPreviousWordStart, &[CTX_INPUT]);
    }
    // List search: TUI emacs keys (ctrl-f MoveRight is conditional).
    rows(&mut b, &["ctrl-e"], MoveEnd, &[CTX_LIST_INPUT]);
    rows(&mut b, &["ctrl-b"], MoveLeft, &[CTX_LIST_INPUT]);
    rows(&mut b, &["alt-b"], MoveToPreviousWord, &[CTX_LIST_INPUT]);
    rows(&mut b, &["alt-f"], MoveToNextWord, &[CTX_LIST_INPUT]);
    rows(&mut b, &["ctrl-k"], DeleteToEndOfLine, &[CTX_LIST_INPUT]);
    rows(
        &mut b,
        &["ctrl-u"],
        DeleteToBeginningOfLine,
        &[CTX_LIST_INPUT],
    );
    rows(
        &mut b,
        &["ctrl-w"],
        DeleteToPreviousWordStart,
        &[CTX_LIST_INPUT],
    );
    // Viewer find input.
    rows(&mut b, &["ctrl-u"], FindClearQuery, &[CTX_VIEWER_INPUT]);
    // Rename input (ctrl-u RenameClear is in the static table).
    rows(&mut b, &["ctrl-e"], MoveEnd, &[CTX_RENAME_INPUT]);
    b
}

/// The static rows that depend on the user bindings: `ctrl-t` ToggleSearchMode
/// unless an action is bound to Ctrl+T, and the list search's `ctrl-f`
/// MoveRight while Fork is not on Ctrl+F.
pub fn conditional_bindings(keys: &KeyBindings) -> Vec<KeyBinding> {
    let mut b = Vec::new();
    if !keys.conflicts_with_ctrl_t() {
        rows(&mut b, &["ctrl-t"], ToggleSearchMode, &[CTX_LIST_KEYS]);
    }
    if keys.fork != KeyChord::ctrl('f') {
        rows(&mut b, &["ctrl-f"], MoveRight, &[CTX_LIST_INPUT]);
    }
    b
}

fn is_conditional_static(binding: &KeyBinding) -> bool {
    let action = binding.action().as_any();
    action.is::<ToggleSearchMode>()
        || (action.is::<MoveRight>()
            && binding
                .predicate()
                .is_some_and(|p| p.to_string().replace(' ', "") == CTX_LIST_INPUT.replace(' ', "")))
}

/// One user binding of `action` per context of [`USER_BINDING_CONTEXTS`].
fn user_binding_rows(chord: &KeyChord, action: Box<dyn Action>) -> Result<Vec<KeyBinding>, String> {
    let keystroke = chord.gpui_keystroke();
    USER_BINDING_CONTEXTS
        .iter()
        .map(|context| {
            let predicate = KeyBindingContextPredicate::parse(context)
                .map(Rc::new)
                .map_err(|error| error.to_string())?;
            KeyBinding::load(
                &keystroke,
                action.boxed_clone(),
                Some(predicate),
                false,
                None,
                &DummyKeyboardMapper,
            )
            .map_err(|error| error.to_string())
        })
        .collect()
}

/// The user-configurable bindings (§8.5) in registration order rename,
/// refresh, fork, resume, delete. Does not validate (see [`sanitize_keys`]).
pub fn user_bindings(keys: &KeyBindings) -> Result<Vec<KeyBinding>, String> {
    let ordered: [(&KeyChord, Box<dyn Action>); 5] = [
        (&keys.rename, Box::new(Rename)),
        (&keys.refresh, Box::new(Refresh)),
        (&keys.fork, Box::new(Fork)),
        (&keys.resume, Box::new(Resume)),
        (&keys.delete, Box::new(Delete)),
    ];
    let mut out = Vec::with_capacity(15);
    for (chord, action) in ordered {
        out.extend(user_binding_rows(chord, action)?);
    }
    Ok(out)
}

/// The user bindings recorded at the last install (removed exactly on rebind).
#[derive(Clone, Default)]
pub struct UserBindingSet {
    pub bindings: Vec<KeyBinding>,
    pub keys: Option<KeyBindings>,
}

impl Global for UserBindingSet {}

fn same_binding(a: &KeyBinding, b: &KeyBinding) -> bool {
    a.keystrokes() == b.keystrokes()
        && a.predicate() == b.predicate()
        && a.action().partial_eq(b.action())
}

/// The keys `keys` really installs: every binding [`validate_bindings`]
/// rejects (reserved keys, keys GPUI cannot parse) is replaced by its
/// default (at startup the default is the "previous value", §8.5). Returns
/// the validation too (errors name the rejected bindings).
pub fn sanitize_keys(keys: &KeyBindings) -> (KeyBindings, KeyValidation) {
    let validation = validate_bindings(keys);
    let defaults = KeyBindings::default();
    let mut effective = *keys;
    for (action, _) in &validation.errors {
        let default = chord_of(&defaults, *action);
        match action {
            KeyAction::Resume => effective.resume = default,
            KeyAction::Fork => effective.fork = default,
            KeyAction::Rename => effective.rename = default,
            KeyAction::Delete => effective.delete = default,
            KeyAction::Refresh => effective.refresh = default,
        }
    }
    (effective, validation)
}

/// Register everything (§7.3 step 8): static + input editing + conditional +
/// user bindings from `AppState.settings.keys`, validated like Settings →
/// Keys does (§8.5): rejected bindings keep their default; every error and
/// warning is one line in the debug log.
pub fn install_all(cx: &mut App) {
    let configured = cx
        .try_global::<crate::globals::AppState>()
        .map(|state| state.settings.keys)
        .unwrap_or_default();
    let (keys, validation) = sanitize_keys(&configured);
    report_validation(&configured, &validation, cx);
    cx.bind_keys(static_bindings());
    cx.bind_keys(crate::help::shortcuts::bindings());
    cx.bind_keys(conditional_bindings(&keys));
    let user = user_bindings(&keys).unwrap_or_else(|_| {
        user_bindings(&KeyBindings::default()).expect("default key bindings are valid")
    });
    cx.bind_keys(user.clone());
    cx.set_global(UserBindingSet {
        bindings: user,
        keys: Some(keys),
    });
}

/// Startup: one debug-log line per rejected binding (Warn) and per warning (Info).
fn report_validation(configured: &KeyBindings, validation: &KeyValidation, cx: &App) {
    let Some(buffer) = cx
        .try_global::<crate::globals::DebugBufferHandle>()
        .map(|handle| handle.0.clone())
    else {
        return;
    };
    let defaults = KeyBindings::default();
    for (action, error) in &validation.errors {
        buffer.push(
            DebugLevel::Warn,
            format!(
                "[keys] {} = \"{}\" was not applied: {error}; using {}",
                action.config_key(),
                chord_of(configured, *action).config_string(),
                chord_of(&defaults, *action).config_string()
            ),
        );
    }
    for warning in &validation.warnings {
        buffer.push(DebugLevel::Info, format!("[keys] {warning}"));
    }
}

/// Rebind the five configurable actions live (Settings → Keys). Validates
/// first: a rejected binding (reserved key, unparseable key) is an error
/// and changes nothing. The keymap update runs deferred.
pub fn install_user_bindings(cx: &mut App, keys: &KeyBindings) -> Result<(), String> {
    let validation = validate_bindings(keys);
    if !validation.errors.is_empty() {
        return Err(validation
            .errors
            .iter()
            .map(|(action, error)| format!("{}: {error}", action.label()))
            .collect::<Vec<_>>()
            .join("; "));
    }
    let user = user_bindings(keys)?;
    let keys = *keys;
    cx.defer(move |cx| {
        let previous = cx
            .try_global::<UserBindingSet>()
            .map(|set| set.bindings.clone())
            .unwrap_or_default();
        let kept: Vec<KeyBinding> = cx
            .key_bindings()
            .borrow()
            .bindings()
            .filter(|binding| !previous.iter().any(|old| same_binding(old, binding)))
            .filter(|binding| !is_conditional_static(binding))
            .cloned()
            .collect();
        cx.clear_key_bindings();
        cx.bind_keys(kept);
        cx.bind_keys(conditional_bindings(&keys));
        cx.bind_keys(user.clone());
        cx.set_global(UserBindingSet {
            bindings: user,
            keys: Some(keys),
        });
        crate::menus::rebuild(cx);
        crate::palette::rebuild(cx);
        cx.refresh_windows();
    });
    Ok(())
}

/// Context stack from a declaration such as `Workspace ConversationList` or
/// `Workspace > ConversationList` (outermost first).
fn context_stack(context: &str) -> Vec<KeyContext> {
    context
        .split(|c: char| c == '>' || c.is_whitespace())
        .filter(|part| !part.is_empty())
        .filter_map(|part| KeyContext::parse(part).ok())
        .collect()
}

/// Whether `binding` applies somewhere in `stack` (empty stack: every binding).
fn applies_in(binding: &KeyBinding, stack: &[KeyContext]) -> bool {
    match binding.predicate() {
        None => true,
        Some(predicate) => stack.is_empty() || predicate.depth_of(stack).is_some(),
    }
}

/// The display label of a binding's keystrokes (`Ctrl+R`, `F5`, `↑`).
fn keystrokes_label(binding: &KeyBinding) -> SharedString {
    binding
        .keystrokes()
        .iter()
        .map(|keystroke| Kbd::format(keystroke.inner()))
        .collect::<Vec<_>>()
        .join(" ")
        .into()
}

/// Whether, in `stack`, the binding that wins `keystrokes` (deepest context,
/// then registered last: GPUI's precedence) belongs to an action other than
/// `action` (a user binding took the key over).
fn shadowed_by_other_action(
    keymap: &Keymap,
    keystrokes: &[gpui_kit::KeybindingKeystroke],
    action: &dyn Action,
    stack: &[KeyContext],
) -> bool {
    let winner = keymap
        .bindings()
        .enumerate()
        .filter(|(_, binding)| binding.keystrokes() == keystrokes && applies_in(binding, stack))
        .max_by_key(|(index, binding)| {
            let depth = binding
                .predicate()
                .and_then(|predicate| predicate.depth_of(stack))
                .unwrap_or(0);
            (depth, *index)
        });
    winner.is_some_and(|(_, binding)| !binding.action().partial_eq(action))
}

/// Labels of every binding of `action` that applies in `stack`, most recent first.
fn labels_in(keymap: &Keymap, action: &dyn Action, stack: &[KeyContext]) -> Vec<SharedString> {
    let mut labels: Vec<SharedString> = Vec::new();
    for binding in keymap.bindings().rev() {
        if !binding.action().partial_eq(action) || !applies_in(binding, stack) {
            continue;
        }
        let label = keystrokes_label(binding);
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    labels
}

/// The labels (`Ctrl+R`, `F5`) of every binding of `action` that applies in
/// `context` (a context stack in declaration syntax, outermost first, e.g.
/// `Workspace ConversationList`; empty = any), most recent binding first.
pub fn binding_labels(action: &dyn Action, context: &str, cx: &App) -> Vec<SharedString> {
    let stack = context_stack(context);
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    labels_in(&keymap, action, &stack)
}

/// One key of a help row: a binding of `action`.
pub struct HelpKey {
    pub action: fn() -> Box<dyn Action>,
    /// The default keystroke (GPUI syntax) shown while the live keymap still
    /// binds it to `action` in the row's context; `None` = every live binding
    /// of `action` there (the configurable keys).
    pub keystroke: Option<&'static str>,
}

/// When a help row is listed.
pub enum HelpWhen {
    Always,
    /// Only while this action has a live binding in the row's context
    /// (`Ctrl+T` Toggle semantic search and `Ctrl+S` Semantic details are
    /// listed only while ToggleSearchMode is bound, C-029).
    Bound(fn() -> Box<dyn Action>),
}

/// Which windows list a help row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelpWindows {
    All,
    /// Not in single-file windows (Resume, Fork, Rename, Delete, Refresh:
    /// no handler there).
    MainOnly,
    /// Only in single-file windows (Close window).
    SingleFileOnly,
}

/// A row of the Keyboard shortcuts dialog (§8.7).
pub struct HelpRow {
    pub section: HelpSection,
    /// The keys, in display order, joined with ` / ` (`↑ / ↓`).
    pub keys: &'static [HelpKey],
    /// The context stack the keys are looked up in (declaration syntax,
    /// outermost first). Single-file windows read `Workspace` as
    /// `SingleFileWindow`.
    pub context: &'static str,
    pub label: &'static str,
    /// The label in single-file windows when it differs (`Close` for
    /// `Back to list`).
    pub single_file_label: Option<&'static str>,
    /// Shown when none of `keys` is bound (typing keys handled by the Input).
    pub fallback_keys: &'static str,
    pub when: HelpWhen,
    pub windows: HelpWindows,
}

impl HelpRow {
    /// The row's first action.
    pub fn action(&self) -> Box<dyn Action> {
        (self.keys[0].action)()
    }

    /// The label for a main (`single_file = false`) or single-file window.
    pub fn label_for(&self, single_file: bool) -> &'static str {
        match self.single_file_label {
            Some(label) if single_file => label,
            _ => self.label,
        }
    }
}

macro_rules! key {
    ($action:expr, $keystroke:expr) => {
        HelpKey {
            action: || Box::new($action),
            keystroke: Some($keystroke),
        }
    };
    ($action:expr) => {
        HelpKey {
            action: || Box::new($action),
            keystroke: None,
        }
    };
}

macro_rules! help {
    ($section:ident, [$($key:expr),+ $(,)?], $context:expr, $label:expr, $fallback:expr) => {
        help!($section, [$($key),+], $context, $label, $fallback, HelpWhen::Always, HelpWindows::All, None)
    };
    ($section:ident, [$($key:expr),+ $(,)?], $context:expr, $label:expr, $fallback:expr, $when:expr, $windows:expr, $single:expr) => {
        HelpRow {
            section: HelpSection::$section,
            keys: &[$($key),+],
            context: $context,
            label: $label,
            single_file_label: $single,
            fallback_keys: $fallback,
            when: $when,
            windows: $windows,
        }
    };
}

const L: &str = "Workspace ConversationList";
const LI: &str = "Workspace ConversationList Input";
const V: &str = "Workspace Viewer";
const A: &str = "Workspace AgentPanel";
const AI: &str = "Workspace AgentPanel Input";
const AR: &str = "Workspace AgentPanel AgentRows";
const D: &str = "Workspace DeepSearch";
const DI: &str = "Workspace DeepSearch Input";
const DR: &str = "Workspace DeepSearch DeepResults";
const G: &str = "Workspace";
const NEXT_MATCH_ALIAS: &str = if MAC { "cmd-g" } else { "f3" };
const PREV_MATCH_ALIAS: &str = if MAC { "cmd-shift-g" } else { "shift-f3" };

static HELP_ROWS: &[HelpRow] = &[
    // Conversation list (TUI order, then GUI rows).
    help!(
        List,
        [key!(SelectPrev, "up"), key!(SelectNext, "down")],
        L,
        "Move selection",
        "↑ / ↓"
    ),
    help!(
        List,
        [key!(MoveLeft, "left"), key!(MoveRight, "right")],
        LI,
        "Move cursor",
        "← / →"
    ),
    help!(
        List,
        [key!(SelectPrev, "ctrl-p"), key!(SelectNext, "ctrl-n")],
        L,
        "Move selection",
        "Ctrl+P / Ctrl+N"
    ),
    help!(
        List,
        [key!(SelectHalfPageDown, "ctrl-d")],
        L,
        "Half page down",
        "Ctrl+D"
    ),
    help!(
        List,
        [key!(DeleteToBeginningOfLine, "ctrl-u")],
        LI,
        "Kill to start of line",
        "Ctrl+U"
    ),
    help!(
        List,
        [key!(DeleteToEndOfLine, "ctrl-k")],
        LI,
        "Kill to end of line",
        "Ctrl+K"
    ),
    help!(
        List,
        [
            key!(SelectPageUp, "pageup"),
            key!(SelectPageDown, "pagedown")
        ],
        L,
        "Jump by page",
        "PgUp / PgDn"
    ),
    help!(
        List,
        [key!(SelectFirst, "home"), key!(SelectLast, "end")],
        L,
        "Jump to first/last",
        "Home / End"
    ),
    help!(
        List,
        [key!(ToggleScope, "tab")],
        L,
        "Toggle scope (All/Project)",
        "Tab"
    ),
    help!(
        List,
        [key!(ToggleSearchMode, "ctrl-t")],
        L,
        "Toggle semantic search",
        "Ctrl+T",
        HelpWhen::Bound(|| Box::new(ToggleSearchMode)),
        HelpWindows::MainOnly,
        None
    ),
    help!(
        List,
        [key!(ShowSemanticDetails, "ctrl-s")],
        L,
        "Semantic details",
        "Ctrl+S",
        HelpWhen::Bound(|| Box::new(ToggleSearchMode)),
        HelpWindows::MainOnly,
        None
    ),
    help!(
        List,
        [key!(OpenConversation, "enter")],
        L,
        "Open viewer",
        "Enter"
    ),
    help!(
        List,
        [key!(SelectAndExit, "ctrl-o")],
        L,
        "Select and exit",
        "Ctrl+O"
    ),
    help!(
        List,
        [key!(DeleteToPreviousWordStart, "ctrl-w")],
        LI,
        "Delete word",
        "Ctrl+W"
    ),
    help!(List, [key!(Resume)], L, "Resume", ""),
    help!(List, [key!(Fork)], L, "Fork resume", ""),
    help!(List, [key!(Rename)], L, "Rename", ""),
    help!(List, [key!(Delete)], L, "Delete", ""),
    help!(List, [key!(Refresh)], L, "Refresh conversations", ""),
    help!(List, [key!(ClearQuery, "escape")], L, "Clear search", "Esc"),
    help!(List, [key!(MoveEnd, "ctrl-e")], LI, "End of line", "Ctrl+E"),
    help!(
        List,
        [
            key!(MoveToPreviousWord, "alt-b"),
            key!(MoveToNextWord, "alt-f")
        ],
        LI,
        "Word left/right",
        "Alt+B / Alt+F"
    ),
    help!(
        List,
        [key!(OpenRowContextMenu, "shift-f10")],
        L,
        "Row menu",
        "Shift+F10"
    ),
    // Viewer (TUI order, then GUI rows).
    help!(
        Viewer,
        [key!(ScrollLineDown, "j"), key!(ScrollLineDown, "down")],
        V,
        "Scroll down",
        "j / ↓"
    ),
    help!(
        Viewer,
        [key!(ScrollLineUp, "k"), key!(ScrollLineUp, "up")],
        V,
        "Scroll up",
        "k / ↑"
    ),
    help!(
        Viewer,
        [key!(NextMessage, "shift-j"), key!(NextMessage, "]")],
        V,
        "Next message",
        "J / ]"
    ),
    help!(
        Viewer,
        [key!(PrevMessage, "shift-k"), key!(PrevMessage, "[")],
        V,
        "Previous message",
        "K / ["
    ),
    help!(
        Viewer,
        [
            key!(ScrollHalfPageDown, "d"),
            key!(ScrollHalfPageDown, "ctrl-d")
        ],
        V,
        "Half page down",
        "d / Ctrl+D"
    ),
    help!(
        Viewer,
        [
            key!(ScrollHalfPageUp, "u"),
            key!(ScrollHalfPageUp, "ctrl-u")
        ],
        V,
        "Half page up",
        "u / Ctrl+U"
    ),
    help!(
        Viewer,
        [key!(ScrollTop, "g"), key!(ScrollTop, "home")],
        V,
        "Jump to top",
        "g / Home"
    ),
    help!(
        Viewer,
        [key!(ScrollBottom, "shift-g"), key!(ScrollBottom, "end")],
        V,
        "Jump to bottom",
        "G / End"
    ),
    help!(Viewer, [key!(StartSearch, "/")], V, "Search", "/"),
    help!(
        Viewer,
        [key!(NextMatch, "n"), key!(PrevMatch, "shift-n")],
        V,
        "Next / prev match",
        "n / N"
    ),
    help!(
        Viewer,
        [key!(CycleToolDisplay, "t")],
        V,
        "Cycle tools: off/trunc/full",
        "t"
    ),
    help!(
        Viewer,
        [key!(ToggleThinking, "shift-t")],
        V,
        "Toggle thinking",
        "T"
    ),
    help!(Viewer, [key!(ToggleTiming, "i")], V, "Toggle timing", "i"),
    help!(
        Viewer,
        [key!(ExportConversation, "e")],
        V,
        "Export to file",
        "e"
    ),
    help!(
        Viewer,
        [key!(YankOrCopyMessage, "y")],
        V,
        "Copy to clipboard / message",
        "y"
    ),
    help!(Viewer, [key!(ShowPath, "p")], V, "Show file path", "p"),
    help!(Viewer, [key!(CopyPath, "shift-y")], V, "Copy path", "Y"),
    help!(
        Viewer,
        [key!(CopySessionId, "shift-i")],
        V,
        "Copy session ID",
        "I"
    ),
    help!(
        Viewer,
        [key!(Resume)],
        V,
        "Resume",
        "",
        HelpWhen::Always,
        HelpWindows::MainOnly,
        None
    ),
    help!(
        Viewer,
        [key!(Fork)],
        V,
        "Fork resume",
        "",
        HelpWhen::Always,
        HelpWindows::MainOnly,
        None
    ),
    help!(
        Viewer,
        [key!(Delete)],
        V,
        "Delete",
        "",
        HelpWhen::Always,
        HelpWindows::MainOnly,
        None
    ),
    help!(
        Viewer,
        [key!(ViewerBack, "q"), key!(ViewerEscape, "escape")],
        V,
        "Back to list",
        "q / Esc",
        HelpWhen::Always,
        HelpWindows::All,
        Some("Close")
    ),
    help!(
        Viewer,
        [
            key!(NextMatch, NEXT_MATCH_ALIAS),
            key!(PrevMatch, PREV_MATCH_ALIAS)
        ],
        V,
        "Next / prev match",
        "F3 / Shift+F3"
    ),
    help!(
        Viewer,
        [key!(ToggleOutputs, "x")],
        V,
        "Expand/collapse tool output",
        "x"
    ),
    help!(Viewer, [key!(ToggleOutline, "o")], V, "Outline", "o"),
    help!(
        Viewer,
        [key!(Rename)],
        V,
        "Rename",
        "",
        HelpWhen::Always,
        HelpWindows::MainOnly,
        None
    ),
    help!(
        Viewer,
        [key!(Refresh)],
        V,
        "Refresh conversations",
        "",
        HelpWhen::Always,
        HelpWindows::MainOnly,
        None
    ),
    help!(
        Viewer,
        [key!(SmartCopy, "secondary-c")],
        V,
        "Copy selection",
        "Ctrl+C"
    ),
    help!(
        Viewer,
        [key!(SelectAllTranscript, "secondary-a")],
        V,
        "Select all",
        "Ctrl+A"
    ),
    // Agent panel.
    help!(
        AgentPanel,
        [
            key!(AgentSelectNext, "down"),
            key!(AgentSelectPrev, "up"),
            key!(AgentSelectNext, "j"),
            key!(AgentSelectPrev, "k")
        ],
        AR,
        "Next / previous row",
        "↓ / ↑ / j / k"
    ),
    help!(
        AgentPanel,
        [key!(AgentOpenSelected, "enter")],
        AR,
        "Show in viewer",
        "Enter"
    ),
    help!(
        AgentPanel,
        [key!(RunDeepSearch, "enter")],
        AI,
        "Run",
        "Enter"
    ),
    help!(
        AgentPanel,
        [key!(ToggleOutline, "o")],
        A,
        "Close outline",
        "o"
    ),
    help!(
        AgentPanel,
        [key!(AgentPanelEscape, "escape")],
        A,
        "Clear / cancel / back to transcript",
        "Esc"
    ),
    // Deep search.
    help!(
        DeepSearch,
        [key!(RunDeepSearch, "enter")],
        DI,
        "Search",
        "Enter"
    ),
    help!(
        DeepSearch,
        [key!(DeepSelectNext, "down"), key!(DeepSelectPrev, "up")],
        DR,
        "Next / previous hit",
        "↓ / ↑"
    ),
    help!(
        DeepSearch,
        [key!(DeepOpenHit, "enter")],
        DR,
        "Open hit",
        "Enter"
    ),
    help!(
        DeepSearch,
        [key!(DeepOpenHit, "secondary-enter")],
        DR,
        "Open hit and focus the viewer",
        "Ctrl+Enter"
    ),
    help!(
        DeepSearch,
        [key!(DeepSearchEscape, "escape")],
        D,
        "Cancel / clear / back to conversations",
        "Esc"
    ),
    // Global.
    help!(Global, [key!(Quit, "secondary-q")], G, "Quit", "Ctrl+Q"),
    help!(
        Global,
        [key!(CloseWindow, "secondary-w")],
        CTX_SINGLE_FILE,
        "Close window",
        "Ctrl+W",
        HelpWhen::Always,
        HelpWindows::SingleFileOnly,
        None
    ),
    help!(
        Global,
        [
            key!(CommandPalette, "secondary-shift-p"),
            key!(CommandPalette, "f10")
        ],
        G,
        "Command palette",
        "Ctrl+Shift+P / F10"
    ),
    help!(
        Global,
        [key!(OpenSettings, "secondary-,")],
        G,
        "Settings",
        "Ctrl+,"
    ),
    help!(
        Global,
        [key!(ShowShortcuts, "f1")],
        G,
        "Keyboard shortcuts",
        "F1"
    ),
    help!(
        Global,
        [key!(OpenJsonlFile, "secondary-shift-o")],
        G,
        "Open JSONL file",
        "Ctrl+Shift+O"
    ),
    help!(
        Global,
        [key!(ChooseWorkspace, "secondary-shift-w")],
        G,
        "Choose workspace",
        "Ctrl+Shift+W",
        HelpWhen::Always,
        HelpWindows::MainOnly,
        None
    ),
    help!(
        Global,
        [key!(FocusConversations, "secondary-1")],
        G,
        "Focus conversations",
        "Ctrl+1",
        HelpWhen::Always,
        HelpWindows::MainOnly,
        None
    ),
    help!(
        Global,
        [
            key!(FocusDeepSearch, "secondary-2"),
            key!(FocusDeepSearch, "secondary-shift-f")
        ],
        G,
        "Focus deep search",
        "Ctrl+2 / Ctrl+Shift+F",
        HelpWhen::Always,
        HelpWindows::MainOnly,
        None
    ),
    help!(
        Global,
        [key!(FocusViewer, "secondary-3")],
        G,
        "Focus viewer",
        "Ctrl+3",
        HelpWhen::Always,
        HelpWindows::MainOnly,
        None
    ),
    help!(
        Global,
        [
            key!(FocusNextRegion, "f6"),
            key!(FocusPrevRegion, "shift-f6")
        ],
        G,
        "Next/previous region",
        "F6 / Shift+F6",
        HelpWhen::Always,
        HelpWindows::MainOnly,
        None
    ),
    help!(
        Global,
        [key!(ToggleListPane, "secondary-shift-b")],
        G,
        "Focus viewer mode",
        "Ctrl+Shift+B",
        HelpWhen::Always,
        HelpWindows::MainOnly,
        None
    ),
    help!(
        Global,
        [key!(Refresh, "f5")],
        G,
        "Refresh",
        "F5",
        HelpWhen::Always,
        HelpWindows::MainOnly,
        None
    ),
    help!(
        Global,
        [key!(RevealInFileManager, "secondary-shift-r")],
        G,
        "Reveal in file manager",
        "Ctrl+Shift+R"
    ),
    help!(
        Global,
        [key!(SearchWithinConversation, "secondary-shift-i")],
        G,
        "Search within conversation",
        "Ctrl+Shift+I",
        HelpWhen::Always,
        HelpWindows::MainOnly,
        None
    ),
    help!(
        Global,
        [key!(ReadRange, "secondary-shift-m")],
        G,
        "Read range",
        "Ctrl+Shift+M",
        HelpWhen::Always,
        HelpWindows::MainOnly,
        None
    ),
    help!(
        Global,
        [key!(ShowDebugLog, "secondary-shift-l")],
        G,
        "Debug log",
        "Ctrl+Shift+L"
    ),
    // Dialogs.
    help!(
        Dialogs,
        [key!(ConfirmYes, "y"), key!(ConfirmNo, "n")],
        CTX_CONFIRM,
        "Confirm / cancel",
        "Y / N"
    ),
    help!(
        Dialogs,
        [
            key!(ExportPick1, "1"),
            key!(ExportPick2, "2"),
            key!(ExportPick3, "3"),
            key!(ExportPick4, "4")
        ],
        CTX_EXPORT_MENU,
        "Export format",
        "1-4"
    ),
    help!(
        Dialogs,
        [key!(SelectDown, "j"), key!(SelectUp, "k")],
        CTX_EXPORT_MENU,
        "Move in menus",
        "j / k"
    ),
];

/// Every row of the Keyboard shortcuts dialog, grouped by section in order.
pub fn help_rows() -> &'static [HelpRow] {
    HELP_ROWS
}

/// The context stack of `row` in a main or single-file window.
fn row_stack(row: &HelpRow, single_file: bool) -> Vec<KeyContext> {
    let mut stack = context_stack(row.context);
    let workspace = KeyContext::parse(CTX_WORKSPACE).ok();
    if single_file
        && let Some(first) = stack.first_mut()
        && workspace.as_ref() == Some(&*first)
        && let Ok(single) = KeyContext::parse(CTX_SINGLE_FILE)
    {
        *first = single;
    }
    stack
}

/// The keys shown for a help row in a main window (`single_file = false`) or
/// a single-file window: each [`HelpKey`] that is live, joined with ` / `,
/// else the row's fallback.
pub fn help_keys_for(row: &HelpRow, single_file: bool, cx: &App) -> SharedString {
    let stack = row_stack(row, single_file);
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    let mut labels: Vec<SharedString> = Vec::new();
    for key in row.keys {
        let action = (key.action)();
        let found: Vec<SharedString> = match key.keystroke {
            Some(keystroke) => {
                let Ok(probe) = KeyBinding::load(
                    keystroke,
                    action.boxed_clone(),
                    None,
                    false,
                    None,
                    &DummyKeyboardMapper,
                ) else {
                    continue;
                };
                let bound = keymap.bindings().any(|binding| {
                    binding.action().partial_eq(action.as_ref())
                        && binding.keystrokes() == probe.keystrokes()
                        && applies_in(binding, &stack)
                }) && !shadowed_by_other_action(
                    &keymap,
                    probe.keystrokes(),
                    action.as_ref(),
                    &stack,
                );
                if bound {
                    vec![keystrokes_label(&probe)]
                } else {
                    Vec::new()
                }
            }
            None => labels_in(&keymap, action.as_ref(), &stack),
        };
        for label in found {
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
    }
    if labels.is_empty() {
        row.fallback_keys.into()
    } else {
        labels.join(" / ").into()
    }
}

/// [`help_keys_for`] in the main window.
pub fn help_keys(row: &HelpRow, cx: &App) -> SharedString {
    help_keys_for(row, false, cx)
}

/// Whether `row` is listed in a main or single-file window right now
/// ([`HelpWindows`], [`HelpWhen`]).
pub fn help_row_visible(row: &HelpRow, single_file: bool, cx: &App) -> bool {
    let window_ok = match row.windows {
        HelpWindows::All => true,
        HelpWindows::MainOnly => !single_file,
        HelpWindows::SingleFileOnly => single_file,
    };
    if !window_ok {
        return false;
    }
    // A single-file window has a viewer (and its agent panel) only: the
    // conversation list and Deep search sections do not apply there.
    if single_file && matches!(row.section, HelpSection::List | HelpSection::DeepSearch) {
        return false;
    }
    match &row.when {
        HelpWhen::Always => true,
        HelpWhen::Bound(action) => {
            let stack = row_stack(row, single_file);
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            !labels_in(&keymap, action().as_ref(), &stack).is_empty()
        }
    }
}

/// Keys rejected as user bindings: they belong to clipboard and editing (§8.5).
pub const RESERVED_CHORDS: [char; 6] = ['c', 'v', 'a', 'z', 'y', 'w'];

/// Validate `[keys]` (Settings → Keys and startup, §8.5): reserved keys and
/// keys GPUI cannot parse are errors; plain characters, Ctrl+T and
/// duplicates are warnings. WP6 adds the "replaces a static binding" warnings.
pub fn validate_keys(keys: &KeysConfig) -> KeyValidation {
    validate_bindings(&KeyBindings::from_config(Some(keys.clone())))
}

/// The static rows a user binding of `chord` replaces (a user binding is
/// registered later, so it wins at equal depth): `(label, place)` with the
/// shortcuts-dialog label of the row and `conversation list` / `viewer`.
/// Only rows in the user bindings' own contexts count (§8.5).
fn shadowed_static_rows(chord: &KeyChord) -> Vec<(&'static str, &'static str)> {
    let Ok(probe) = gpui_kit::Keystroke::parse(&chord.gpui_keystroke()) else {
        return Vec::new();
    };
    let wanted = probe.unparse();
    let mut found: Vec<(&'static str, &'static str)> = Vec::new();
    for row in shadowable_static_rows() {
        if row.keystroke != wanted {
            continue;
        }
        let Some(label) = row.label else {
            continue;
        };
        if !found.contains(&(label, row.place)) {
            found.push((label, row.place));
        }
    }
    found
}

/// A static binding a user binding can take over, as plain data.
struct ShadowableRow {
    keystroke: String,
    place: &'static str,
    label: Option<&'static str>,
}

/// The static rows a user binding (registered in [`USER_BINDING_CONTEXTS`],
/// later than every static row) beats: rows in those very contexts, in the
/// list search, in the viewer's own context (F3) and in every ancestor
/// context (the window roots and the app-wide rows). Built once.
fn shadowable_static_rows() -> &'static [ShadowableRow] {
    static ROWS: std::sync::OnceLock<Vec<ShadowableRow>> = std::sync::OnceLock::new();
    ROWS.get_or_init(|| {
        let normalize = |text: &str| text.replace(' ', "");
        let places = [
            (normalize(CTX_LIST_KEYS), "conversation list"),
            (normalize(CTX_LIST_INPUT), "list search"),
            (normalize(CTX_VIEWER_KEYS), "viewer"),
            (normalize(CTX_VIEWER), "viewer"),
            (normalize(CTX_WORKSPACE), "window"),
            (normalize(CTX_SINGLE_FILE), "window"),
            (String::new(), "window"),
        ];
        let mut rows = Vec::new();
        for binding in static_bindings() {
            let [keystroke] = binding.keystrokes() else {
                continue;
            };
            let predicate = binding
                .predicate()
                .map(|p| normalize(&p.to_string()))
                .unwrap_or_default();
            let Some((_, place)) = places.iter().find(|(context, _)| *context == predicate) else {
                continue;
            };
            let label = HELP_ROWS.iter().find_map(|row| {
                row.keys
                    .iter()
                    .any(|key| (key.action)().partial_eq(binding.action()))
                    .then_some(row.label)
            });
            rows.push(ShadowableRow {
                keystroke: keystroke.inner().unparse(),
                place,
                label,
            });
        }
        rows
    })
}

/// [`validate_keys`] for resolved bindings (startup, live rebinding).
pub fn validate_bindings(keys: &KeyBindings) -> KeyValidation {
    let mut validation = KeyValidation::default();
    let chords: Vec<(KeyAction, KeyChord)> = KeyAction::ALL
        .iter()
        .map(|action| (*action, chord_of(keys, *action)))
        .collect();
    for (action, chord) in &chords {
        let label = chord.config_string();
        if let Key::Char(c) = chord.key
            && chord.mods.ctrl
            && !chord.mods.alt
            && RESERVED_CHORDS.contains(&c.to_ascii_lowercase())
        {
            validation.errors.push((
                *action,
                format!("{label} is reserved for clipboard and editing"),
            ));
            continue;
        }
        if let Err(error) = gpui_kit::Keystroke::parse(&chord.gpui_keystroke()) {
            validation
                .errors
                .push((*action, format!("Invalid key binding: {label} ({error})")));
            continue;
        }
        if let Key::Char(c) = chord.key
            && chord.mods.is_empty()
        {
            validation.warnings.push(format!(
                "\"{c}\": a plain character can't be typed in the search field"
            ));
        }
        if *chord == KeyChord::ctrl('t') {
            validation
                .warnings
                .push(format!("{label} disables the Lexical/Semantic shortcut"));
        }
        for (shadowed, place) in shadowed_static_rows(chord) {
            validation
                .warnings
                .push(format!("{label} replaces \"{shadowed}\" in the {place}"));
        }
    }
    for (i, (a, chord_a)) in chords.iter().enumerate() {
        for (b, chord_b) in chords.iter().skip(i + 1) {
            if chord_a == chord_b {
                let winner = if a.priority() <= b.priority() { a } else { b };
                validation.warnings.push(format!(
                    "{} and {} share {}; {} wins",
                    a.label(),
                    b.label(),
                    chord_a.config_string(),
                    winner.label()
                ));
            }
        }
    }
    validation
}

/// The chord bound to `action` in `keys`.
pub fn chord_of(keys: &KeyBindings, action: KeyAction) -> KeyChord {
    match action {
        KeyAction::Resume => keys.resume,
        KeyAction::Fork => keys.fork,
        KeyAction::Rename => keys.rename,
        KeyAction::Delete => keys.delete,
        KeyAction::Refresh => keys.refresh,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_history_core::config::parse_key_binding;

    #[test]
    fn user_bindings_cover_every_context_in_registration_order() {
        let bindings = user_bindings(&KeyBindings::default()).unwrap();
        assert_eq!(bindings.len(), 15);
        assert!(bindings[0].action().as_any().is::<Rename>());
        assert!(bindings[14].action().as_any().is::<Delete>());
    }

    #[test]
    fn conditional_rows_follow_the_user_keys() {
        let defaults = KeyBindings::default();
        let rows = conditional_bindings(&defaults);
        // Fork is on Ctrl+F by default: no MoveRight; ToggleSearchMode is there.
        assert_eq!(rows.len(), 1);
        assert!(rows[0].action().as_any().is::<ToggleSearchMode>());

        let mut keys = defaults;
        keys.fork = parse_key_binding("alt+f").unwrap();
        keys.refresh = parse_key_binding("ctrl+t").unwrap();
        let rows = conditional_bindings(&keys);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].action().as_any().is::<MoveRight>());
    }

    #[test]
    fn validation_reports_reserved_plain_and_duplicate_keys() {
        let keys = KeysConfig {
            resume: Some(parse_key_binding("ctrl+c").unwrap()),
            fork: Some(parse_key_binding("r").unwrap()),
            rename: Some(parse_key_binding("f2").unwrap()),
            delete: Some(parse_key_binding("f2").unwrap()),
            refresh: None,
        };
        let validation = validate_keys(&keys);
        assert_eq!(
            validation.errors,
            vec![(
                KeyAction::Resume,
                "ctrl+c is reserved for clipboard and editing".to_string()
            )]
        );
        assert!(
            validation.warnings.contains(
                &"\"r\": a plain character can't be typed in the search field".to_string()
            )
        );
        assert!(
            validation
                .warnings
                .contains(&"Rename and Delete share f2; Delete wins".to_string())
        );
    }

    #[test]
    fn sanitizing_replaces_rejected_bindings_with_their_defaults() {
        let keys = KeyBindings {
            resume: parse_key_binding("ctrl+c").unwrap(),
            delete: parse_key_binding("ctrl+v").unwrap(),
            ..KeyBindings::default()
        };
        let (effective, validation) = sanitize_keys(&keys);
        assert_eq!(effective, KeyBindings::default());
        assert_eq!(validation.errors.len(), 2);
        // A valid configuration is kept as is.
        let keys = KeyBindings {
            resume: parse_key_binding("alt+r").unwrap(),
            ..KeyBindings::default()
        };
        assert_eq!(sanitize_keys(&keys).0, keys);
    }

    #[test]
    fn help_rows_cover_every_section() {
        for section in HelpSection::ALL {
            assert!(
                help_rows().iter().any(|row| row.section == section),
                "{section:?}"
            );
        }
    }

    #[test]
    fn help_row_keystrokes_parse() {
        for row in help_rows() {
            assert!(!row.keys.is_empty(), "{}", row.label);
            for key in row.keys {
                if let Some(keystroke) = key.keystroke {
                    assert!(
                        KeyBinding::load(
                            keystroke,
                            (key.action)(),
                            None,
                            false,
                            None,
                            &DummyKeyboardMapper,
                        )
                        .is_ok(),
                        "{keystroke} ({})",
                        row.label
                    );
                }
            }
        }
    }
}
