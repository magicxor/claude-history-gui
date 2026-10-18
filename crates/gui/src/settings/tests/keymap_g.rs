//! G tests of the keymap (DESIGN §8): the §8.3 table row by row (C-052),
//! user bindings and their registration order (C-028, L-063, C-029, C-034),
//! hints from the live keymap (C-054). Key resolution uses GPUI's own
//! `Keymap::bindings_for_input` on the real context stacks, i.e. exactly what a
//! key event does before an action reaches a handler.

use claude_history_core::config::{KeyBindings, parse_key_binding};
use gpui_kit::component::input::{
    DeleteToBeginningOfLine, DeleteToEndOfLine, DeleteToPreviousWordStart, MoveEnd, MoveLeft,
    MoveRight, MoveToNextWord, MoveToPreviousWord,
};
use gpui_kit::{Action, App, KeyContext, Keystroke, TestAppContext};

use crate::actions::*;
use crate::globals::last_toast;
use crate::keymap;
use crate::test_support::{TestWorld, open_main};
use gpui_kit::base::actions::{Cancel, Confirm, SelectDown, SelectUp};

const MAC: bool = cfg!(target_os = "macos");

fn stack(declaration: &str) -> Vec<KeyContext> {
    declaration
        .split_whitespace()
        .map(|name| KeyContext::parse(name).expect("valid context name"))
        .collect()
}

/// The bindings a keystroke resolves to in `declaration` (a context stack,
/// outermost first), highest precedence first.
fn resolve(cx: &App, keystroke: &str, declaration: &str) -> Vec<Box<dyn Action>> {
    let keystroke = Keystroke::parse(keystroke).expect("valid keystroke");
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    let (bindings, _) = keymap.bindings_for_input(&[keystroke], &stack(declaration));
    bindings
        .iter()
        .map(|binding| binding.action().boxed_clone())
        .collect()
}

fn winner(cx: &App, keystroke: &str, declaration: &str) -> Option<Box<dyn Action>> {
    resolve(cx, keystroke, declaration).into_iter().next()
}

fn name_of(action: &Option<Box<dyn Action>>) -> String {
    action
        .as_ref()
        .map_or_else(|| "nothing".to_string(), |a| a.name().to_string())
}

/// One row of the §8.3 table: the keystroke typed with focus in the context
/// stack reaches the action.
struct Row {
    keys: &'static str,
    stack: &'static str,
    action: Box<dyn Action>,
}

macro_rules! row {
    ($keys:expr, $stack:expr, $action:expr) => {
        Row {
            keys: $keys,
            stack: $stack,
            action: Box::new($action),
        }
    };
}

// Context stacks as the panes build them (outermost first).
const WS: &str = "Workspace";
const SF: &str = "SingleFileWindow";
const LIST: &str = "Workspace ConversationList";
const LIST_INPUT: &str = "Workspace ConversationList Input";
const VIEWER: &str = "Workspace Viewer";
const VIEWER_INPUT: &str = "Workspace Viewer Input";
const SF_VIEWER: &str = "SingleFileWindow Viewer";
const AGENT: &str = "Workspace AgentPanel";
const AGENT_INPUT: &str = "Workspace AgentPanel Input";
const AGENT_ROWS: &str = "Workspace AgentPanel AgentRows";
const DEEP: &str = "Workspace DeepSearch";
const DEEP_INPUT: &str = "Workspace DeepSearch Input";
const DEEP_RESULTS: &str = "Workspace DeepSearch DeepResults";

/// The static table of DESIGN §8.3 (Windows/Linux column; macOS differs only in
/// the rows marked below), written out independently of `keymap.rs`.
fn table() -> Vec<Row> {
    let mut rows = vec![
        // App / window.
        row!("secondary-q", "", Quit),
        row!("secondary-q", LIST_INPUT, Quit),
        row!("secondary-w", SF, CloseWindow),
        row!("secondary-,", WS, OpenSettings),
        row!("secondary-,", SF, OpenSettings),
        row!("secondary-,", LIST_INPUT, OpenSettings),
        row!("f1", WS, ShowShortcuts),
        row!("f1", SF, ShowShortcuts),
        row!("f1", LIST_INPUT, ShowShortcuts),
        row!("?", VIEWER, ShowShortcuts),
        row!("?", DEEP, ShowShortcuts),
        row!("?", AGENT, ShowShortcuts),
        row!("secondary-shift-p", WS, CommandPalette),
        row!("secondary-shift-p", SF, CommandPalette),
        row!("f10", WS, CommandPalette),
        row!("f10", SF, CommandPalette),
        row!("f6", WS, FocusNextRegion),
        row!("shift-f6", WS, FocusPrevRegion),
        row!("secondary-shift-o", WS, OpenJsonlFile),
        row!("secondary-shift-o", SF, OpenJsonlFile),
        row!("secondary-shift-w", WS, ChooseWorkspace),
        row!("secondary-1", WS, FocusConversations),
        row!("secondary-2", WS, FocusDeepSearch),
        row!("secondary-shift-f", WS, FocusDeepSearch),
        row!("secondary-shift-f", LIST_INPUT, FocusDeepSearch),
        row!("secondary-shift-f", DEEP_INPUT, FocusDeepSearch),
        row!("secondary-3", WS, FocusViewer),
        row!("secondary-shift-b", WS, ToggleListPane),
        row!("f5", WS, Refresh),
        row!("f5", LIST_INPUT, Refresh),
        row!("f5", VIEWER, Refresh),
        // Session.
        row!("enter", LIST, OpenConversation),
        row!("enter", LIST_INPUT, OpenConversation),
        row!("ctrl-o", LIST, SelectAndExit),
        row!("ctrl-o", LIST_INPUT, SelectAndExit),
        row!("ctrl-s", WS, ShowSemanticDetails),
        row!("ctrl-s", LIST_INPUT, ShowSemanticDetails),
        row!("secondary-shift-r", WS, RevealInFileManager),
        row!("secondary-shift-r", SF, RevealInFileManager),
        row!("secondary-shift-i", WS, SearchWithinConversation),
        row!("secondary-shift-m", WS, ReadRange),
        row!("secondary-shift-l", WS, ShowDebugLog),
        row!("secondary-shift-l", SF, ShowDebugLog),
        // List.
        row!("up", LIST, SelectPrev),
        row!("ctrl-p", LIST, SelectPrev),
        row!("ctrl-p", LIST_INPUT, SelectPrev),
        row!("up", LIST_INPUT, SelectPrev),
        row!("down", LIST, SelectNext),
        row!("ctrl-n", LIST, SelectNext),
        row!("ctrl-n", LIST_INPUT, SelectNext),
        row!("down", LIST_INPUT, SelectNext),
        row!("pageup", LIST, SelectPageUp),
        row!("pageup", LIST_INPUT, SelectPageUp),
        row!("pagedown", LIST, SelectPageDown),
        row!("pagedown", LIST_INPUT, SelectPageDown),
        row!("ctrl-d", LIST, SelectHalfPageDown),
        row!("ctrl-d", LIST_INPUT, SelectHalfPageDown),
        row!("home", LIST, SelectFirst),
        row!("home", LIST_INPUT, SelectFirst),
        row!("end", LIST, SelectLast),
        row!("end", LIST_INPUT, SelectLast),
        row!("escape", LIST, ClearQuery),
        row!("escape", LIST_INPUT, ClearQuery),
        row!("tab", LIST, ToggleScope),
        row!("tab", LIST_INPUT, ToggleScope),
        row!("ctrl-t", LIST, ToggleSearchMode),
        row!("ctrl-t", LIST_INPUT, ToggleSearchMode),
        row!("shift-f10", LIST, OpenRowContextMenu),
        row!("shift-f10", LIST_INPUT, OpenRowContextMenu),
        // Viewer.
        row!("j", VIEWER, ScrollLineDown),
        row!("down", VIEWER, ScrollLineDown),
        row!("k", VIEWER, ScrollLineUp),
        row!("up", VIEWER, ScrollLineUp),
        row!("shift-j", VIEWER, NextMessage),
        row!("]", VIEWER, NextMessage),
        row!("shift-k", VIEWER, PrevMessage),
        row!("[", VIEWER, PrevMessage),
        row!("d", VIEWER, ScrollHalfPageDown),
        row!("ctrl-d", VIEWER, ScrollHalfPageDown),
        row!("u", VIEWER, ScrollHalfPageUp),
        row!("ctrl-u", VIEWER, ScrollHalfPageUp),
        row!("pagedown", VIEWER, ScrollPageDown),
        row!("pageup", VIEWER, ScrollPageUp),
        row!("g", VIEWER, ScrollTop),
        row!("home", VIEWER, ScrollTop),
        row!("shift-g", VIEWER, ScrollBottom),
        row!("end", VIEWER, ScrollBottom),
        row!("/", VIEWER, StartSearch),
        row!("n", VIEWER, NextMatch),
        row!("shift-n", VIEWER, PrevMatch),
        row!("enter", VIEWER_INPUT, CommitSearch),
        row!("escape", VIEWER_INPUT, CancelSearch),
        row!("t", VIEWER, CycleToolDisplay),
        row!("shift-t", VIEWER, ToggleThinking),
        row!("i", VIEWER, ToggleTiming),
        row!("x", VIEWER, ToggleOutputs),
        row!("escape", VIEWER, ViewerEscape),
        row!("q", VIEWER, ViewerBack),
        row!("e", VIEWER, ExportConversation),
        row!("y", VIEWER, YankOrCopyMessage),
        row!("shift-y", VIEWER, CopyPath),
        row!("shift-i", VIEWER, CopySessionId),
        row!("p", VIEWER, ShowPath),
        row!("o", VIEWER, ToggleOutline),
        row!("o", AGENT, ToggleOutline),
        // The same keys in a single-file window.
        row!("j", SF_VIEWER, ScrollLineDown),
        row!("q", SF_VIEWER, ViewerBack),
        row!("escape", SF_VIEWER, ViewerEscape),
        // Agent panel and Deep search.
        row!("escape", AGENT, AgentPanelEscape),
        row!("down", AGENT_ROWS, AgentSelectNext),
        row!("j", AGENT_ROWS, AgentSelectNext),
        row!("up", AGENT_ROWS, AgentSelectPrev),
        row!("k", AGENT_ROWS, AgentSelectPrev),
        row!("enter", AGENT_ROWS, AgentOpenSelected),
        row!("enter", DEEP_INPUT, RunDeepSearch),
        row!("enter", AGENT_INPUT, RunDeepSearch),
        row!("escape", DEEP, DeepSearchEscape),
        row!("down", DEEP_RESULTS, DeepSelectNext),
        row!("up", DEEP_RESULTS, DeepSelectPrev),
        row!("enter", DEEP_RESULTS, DeepOpenHit),
        row!("secondary-enter", DEEP_RESULTS, DeepOpenHit),
        // Dialogs.
        row!("y", "Confirm", ConfirmYes),
        row!("shift-y", "Confirm", ConfirmYes),
        row!("n", "Confirm", ConfirmNo),
        row!("shift-n", "Confirm", ConfirmNo),
        row!("enter", "Confirm", ConfirmEnterIgnored),
        row!("ctrl-u", "RenameDialog Input", RenameClear),
        row!("1", "ExportMenu", ExportPick1),
        row!("2", "ExportMenu", ExportPick2),
        row!("3", "ExportMenu", ExportPick3),
        row!("4", "ExportMenu", ExportPick4),
        row!("j", "ExportMenu", SelectDown),
        row!("down", "ExportMenu", SelectDown),
        row!("k", "ExportMenu", SelectUp),
        row!("up", "ExportMenu", SelectUp),
        row!("enter", "ExportMenu", Confirm { secondary: false }),
        row!("escape", "ExportMenu", Cancel),
        // Input editing (§8.4).
        row!("ctrl-e", LIST_INPUT, MoveEnd),
        row!("ctrl-k", LIST_INPUT, DeleteToEndOfLine),
        row!("ctrl-u", LIST_INPUT, DeleteToBeginningOfLine),
        row!("ctrl-w", LIST_INPUT, DeleteToPreviousWordStart),
        row!("ctrl-b", LIST_INPUT, MoveLeft),
        row!("ctrl-f", LIST_INPUT, Fork),
        row!("alt-b", LIST_INPUT, MoveToPreviousWord),
        row!("alt-f", LIST_INPUT, MoveToNextWord),
        row!("ctrl-e", "RenameDialog Input", MoveEnd),
        row!("ctrl-u", VIEWER_INPUT, FindClearQuery),
    ];
    if MAC {
        rows.push(row!("cmd-c", WS, SmartCopy));
        rows.push(row!("cmd-a", VIEWER, SelectAllTranscript));
        rows.push(row!("cmd-g", VIEWER, NextMatch));
        rows.push(row!("cmd-shift-g", VIEWER, PrevMatch));
    } else {
        rows.push(row!("ctrl-c", WS, SmartCopy));
        rows.push(row!("ctrl-a", VIEWER, SelectAllTranscript));
        rows.push(row!("ctrl-c", SF, SmartCopy));
        rows.push(row!("ctrl-c", LIST_INPUT, SmartCopy));
        rows.push(row!("f3", VIEWER, NextMatch));
        rows.push(row!("shift-f3", VIEWER, PrevMatch));
        rows.push(row!("shift-f10", LIST, OpenRowContextMenu));
        rows.push(row!("menu", LIST, OpenRowContextMenu));
        rows.push(row!("alt-f4", "", CloseWindow));
        // Ctrl+W deletes a word in every input and never closes a window (PD6).
        rows.push(row!("ctrl-w", "Workspace Input", DeleteToPreviousWordStart));
        rows.push(row!(
            "ctrl-w",
            "Workspace DeepSearch Input",
            DeleteToPreviousWordStart
        ));
    }
    rows
}

/// C-052: every row of the §8.3 table, dispatched in its context, reaches its
/// action (the row's action is the highest-precedence binding of the key).
#[gpui_kit::test]
fn every_static_row_dispatches_from_its_context(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    let rows = table();
    assert!(rows.len() > 120, "the table is complete ({})", rows.len());
    cx.update(|cx| {
        let mut failures = Vec::new();
        for row in &rows {
            // GPUI dispatches the matching bindings in precedence order until
            // a handler stops propagation. A single-line Input binds the same
            // keys (`enter`, `up`, `tab`, …) and propagates them, so the row's
            // action may come after `input::*` bindings and nothing else.
            let found = resolve(cx, row.keys, row.stack);
            let position = found
                .iter()
                .position(|action| action.partial_eq(row.action.as_ref()));
            let ok = position.is_some_and(|ix| {
                found[..ix]
                    .iter()
                    .all(|before| before.name().starts_with("input::"))
            });
            if !ok {
                failures.push(format!(
                    "{} in [{}]: expected {}, got [{}]",
                    row.keys,
                    row.stack,
                    row.action.name(),
                    found
                        .iter()
                        .map(|action| action.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    });
}

/// C-052 (negative half): a key never reaches an action in a negated context:
/// `j` typed into the find input is text, not Scroll down; list keys do not
/// fire from the toolbar; viewer letters do not fire in the agent panel.
#[gpui_kit::test]
fn keys_do_not_fire_in_negated_contexts(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    cx.update(|cx| {
        let none = |keys: &str, declaration: &str, action: &dyn Action| {
            let found = resolve(cx, keys, declaration);
            assert!(
                !found.iter().any(|found| found.partial_eq(action)),
                "{keys} in [{declaration}] must not reach {}",
                action.name()
            );
        };
        none("j", VIEWER_INPUT, &ScrollLineDown);
        none("down", VIEWER_INPUT, &ScrollLineDown);
        none("pagedown", VIEWER_INPUT, &ScrollPageDown);
        none("q", VIEWER_INPUT, &ViewerBack);
        none("t", VIEWER_INPUT, &CycleToolDisplay);
        none("/", VIEWER_INPUT, &StartSearch);
        none("?", VIEWER_INPUT, &ShowShortcuts);
        // The list's keys stay away from its toolbar buttons.
        for keys in ["enter", "tab", "escape", "up", "down", "ctrl-p"] {
            let toolbar = "Workspace ConversationList ListToolbar";
            for action in [
                &OpenConversation as &dyn Action,
                &ToggleScope,
                &ClearQuery,
                &SelectPrev,
                &SelectNext,
            ] {
                none(keys, toolbar, action);
            }
        }
        // The agent panel is a sibling of the viewer: its focus never sees
        // viewer letters.
        none("j", AGENT, &ScrollLineDown);
        none("t", AGENT, &CycleToolDisplay);
        none("q", AGENT, &ViewerBack);
        // A letter typed into the list search is text.
        for letter in ["r", "j", "t", "q", "e"] {
            assert!(
                resolve(cx, letter, LIST_INPUT).is_empty(),
                "{letter} in the list search is typed"
            );
        }
        // Viewer letters do not fire in the list.
        none("j", LIST, &ScrollLineDown);
        // Deep search keys need the results list.
        none("down", DEEP_INPUT, &DeepSelectNext);
        none("enter", DEEP, &DeepOpenHit);
    });
}

fn rebind(cx: &mut TestAppContext, keys: KeyBindings) {
    cx.update(|cx| keymap::install_user_bindings(cx, &keys).expect("valid bindings"));
    cx.run_until_parked();
}

fn chords(rename: &str, refresh: &str, fork: &str, resume: &str, delete: &str) -> KeyBindings {
    KeyBindings {
        rename: parse_key_binding(rename).unwrap(),
        refresh: parse_key_binding(refresh).unwrap(),
        fork: parse_key_binding(fork).unwrap(),
        resume: parse_key_binding(resume).unwrap(),
        delete: parse_key_binding(delete).unwrap(),
    }
}

/// L-063: `rename = "r"` → typing `r` in the list search dispatches Rename and
/// the query stays empty (configured keys are checked before text editing).
#[gpui_kit::test]
fn a_plain_character_binding_beats_text_editing(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    world.write_config(Some("[keys]\nrename = \"r\"\n"), None);
    let (handle, ws) = open_main(cx, &world, &[]);
    let list = cx.read(|cx| ws.read(cx).list().clone());
    cx.update(|cx| {
        let got = winner(cx, "r", LIST_INPUT);
        assert!(
            got.as_ref().is_some_and(|got| got.partial_eq(&Rename)),
            "{}",
            name_of(&got)
        );
    });
    cx.simulate_keystrokes(handle, "r");
    cx.run_until_parked();
    assert_eq!(
        cx.read(|cx| list.read(cx).query(cx)).as_ref(),
        "",
        "`r` was not typed"
    );
    assert_eq!(
        cx.read(last_toast).as_ref(),
        "No conversation selected",
        "Rename reached the list pane (no row to rename)"
    );
    // Other letters still type.
    cx.simulate_input(handle, "abc");
    assert_eq!(cx.read(|cx| list.read(cx).query(cx)).as_ref(), "abc");
}

/// C-028: Resume and Delete both on `ctrl-x` → `ctrl-x` dispatches Delete
/// (TUI check order delete > resume > fork > refresh > rename), in every
/// context the user bindings cover.
#[gpui_kit::test]
fn duplicate_bindings_resolve_by_the_tui_check_order(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    rebind(cx, chords("f2", "ctrl+l", "ctrl+f", "ctrl+x", "ctrl+x"));
    cx.update(|cx| {
        for declaration in [LIST, LIST_INPUT, VIEWER] {
            let got = winner(cx, "ctrl-x", declaration);
            assert!(
                got.as_ref().is_some_and(|got| got.partial_eq(&Delete)),
                "ctrl-x in [{declaration}]: {}",
                name_of(&got)
            );
        }
    });
    // The whole chain: all five on one key → Delete; without Delete → Resume.
    rebind(cx, chords("ctrl+k", "ctrl+k", "ctrl+k", "ctrl+k", "ctrl+k"));
    cx.update(|cx| {
        let got = winner(cx, "ctrl-k", LIST);
        assert!(got.is_some_and(|got| got.partial_eq(&Delete)));
    });
    rebind(cx, chords("ctrl+k", "ctrl+k", "ctrl+k", "ctrl+k", "f9"));
    cx.update(|cx| {
        let got = winner(cx, "ctrl-k", LIST);
        assert!(got.is_some_and(|got| got.partial_eq(&Resume)));
    });
    rebind(cx, chords("ctrl+k", "ctrl+k", "ctrl+k", "f8", "f9"));
    cx.update(|cx| {
        let got = winner(cx, "ctrl-k", LIST);
        assert!(got.is_some_and(|got| got.partial_eq(&Fork)));
    });
    rebind(cx, chords("ctrl+k", "ctrl+k", "f7", "f8", "f9"));
    cx.update(|cx| {
        let got = winner(cx, "ctrl-k", LIST);
        assert!(got.is_some_and(|got| got.partial_eq(&Refresh)));
    });
}

/// C-029: `resume = "ctrl+t"` → `ctrl-t` dispatches Resume, no
/// ToggleSearchMode binding exists, and the Shortcuts dialog has no Toggle
/// semantic search row.
#[gpui_kit::test]
fn ctrl_t_user_binding_disables_the_mode_toggle(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    world.write_config(Some("[keys]\nresume = \"ctrl+t\"\n"), None);
    let (_handle, _ws) = open_main(cx, &world, &[]);
    cx.update(|cx| {
        let got = winner(cx, "ctrl-t", LIST);
        assert!(
            got.as_ref().is_some_and(|got| got.partial_eq(&Resume)),
            "{}",
            name_of(&got)
        );
        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        assert_eq!(
            keymap.bindings_for_action(&ToggleSearchMode).count(),
            0,
            "no ToggleSearchMode binding"
        );
        let rows = crate::help::shortcuts::shortcut_rows(false, cx);
        assert!(
            !rows
                .iter()
                .any(|row| row.label.as_ref() == "Toggle semantic search"),
            "{rows:?}"
        );
        assert!(
            !rows
                .iter()
                .any(|row| row.label.as_ref() == "Semantic details")
        );
    });
}

/// C-034 (keymap half): after rebinding Resume to `alt+r`, `alt-r` dispatches
/// Resume and `ctrl-r` does not, `f5` still dispatches Refresh, every static
/// row survives, and `binding_labels(Resume)` is `Alt+R`.
#[gpui_kit::test]
fn live_rebinding_keeps_the_static_rows(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    let before = cx.update(|cx| keymap::binding_labels(&Resume, LIST, cx));
    assert_eq!(
        before.iter().map(|l| l.as_ref()).collect::<Vec<_>>(),
        ["Ctrl+R"]
    );
    rebind(cx, chords("f2", "ctrl+l", "ctrl+f", "alt+r", "ctrl+x"));
    cx.update(|cx| {
        for declaration in [LIST, LIST_INPUT, VIEWER] {
            let got = winner(cx, "alt-r", declaration);
            assert!(
                got.as_ref().is_some_and(|got| got.partial_eq(&Resume)),
                "alt-r in [{declaration}]: {}",
                name_of(&got)
            );
            let old = resolve(cx, "ctrl-r", declaration);
            assert!(
                !old.iter().any(|a| a.partial_eq(&Resume)),
                "ctrl-r no longer resumes in [{declaration}]"
            );
        }
        let f5 = winner(cx, "f5", LIST_INPUT);
        assert!(f5.is_some_and(|a| a.partial_eq(&Refresh)));
        let labels = keymap::binding_labels(&Resume, LIST, cx);
        assert_eq!(
            labels.iter().map(|l| l.as_ref()).collect::<Vec<_>>(),
            ["Alt+R"]
        );
        // Every static row of the table is still bound.
        let still_bound = |binding: &gpui_kit::KeyBinding| {
            cx.key_bindings().borrow().bindings().any(|bound| {
                bound.keystrokes() == binding.keystrokes()
                    && bound.predicate() == binding.predicate()
                    && bound.action().partial_eq(binding.action())
            })
        };
        for binding in keymap::static_bindings() {
            assert!(
                still_bound(&binding),
                "static row lost: {:?} {}",
                binding.keystrokes(),
                binding.action().name()
            );
        }
        // The Ctrl+T toggle (conditional) is still there.
        let toggle = winner(cx, "ctrl-t", LIST);
        assert!(toggle.is_some_and(|a| a.partial_eq(&ToggleSearchMode)));
    });
    // Rebinding back restores the defaults without duplicating bindings.
    rebind(cx, KeyBindings::default());
    cx.update(|cx| {
        let labels = keymap::binding_labels(&Resume, LIST, cx);
        assert_eq!(
            labels.iter().map(|l| l.as_ref()).collect::<Vec<_>>(),
            ["Ctrl+R"]
        );
        let count = cx
            .key_bindings()
            .borrow()
            .bindings()
            .filter(|binding| binding.action().partial_eq(&Resume))
            .count();
        assert_eq!(count, 3, "one Resume binding per user context");
    });
}

/// C-034 / C-029: rebinding Fork frees `ctrl-f` for the cursor in the list
/// search; Ctrl+T on any action removes the mode toggle and restoring brings
/// it back.
#[gpui_kit::test]
fn conditional_rows_follow_live_rebinding(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    cx.update(|cx| {
        let got = winner(cx, "ctrl-f", LIST_INPUT);
        assert!(got.is_some_and(|a| a.partial_eq(&Fork)), "Fork on ctrl-f");
    });
    rebind(cx, chords("f2", "ctrl+l", "alt+f", "ctrl+r", "ctrl+x"));
    cx.update(|cx| {
        let got = winner(cx, "ctrl-f", LIST_INPUT);
        assert!(
            got.as_ref().is_some_and(|a| a.partial_eq(&MoveRight)),
            "ctrl-f moves the cursor once Fork is elsewhere: {}",
            name_of(&got)
        );
    });
    rebind(cx, chords("f2", "ctrl+t", "ctrl+f", "ctrl+r", "ctrl+x"));
    cx.update(|cx| {
        let got = winner(cx, "ctrl-t", LIST);
        assert!(got.is_some_and(|a| a.partial_eq(&Refresh)));
        assert_eq!(
            cx.key_bindings()
                .borrow()
                .bindings_for_action(&ToggleSearchMode)
                .count(),
            0
        );
    });
    rebind(cx, KeyBindings::default());
    cx.update(|cx| {
        let got = winner(cx, "ctrl-t", LIST);
        assert!(got.is_some_and(|a| a.partial_eq(&ToggleSearchMode)));
    });
}

/// C-054: hints come from the live keymap: `binding_labels(Rename)` is `F2`
/// by default and `F3` after rebinding; nothing is hard-coded.
#[gpui_kit::test]
fn hints_follow_the_live_keymap(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    cx.update(|cx| {
        let labels = keymap::binding_labels(&Rename, "Workspace ConversationList", cx);
        assert_eq!(
            labels.iter().map(|l| l.as_ref()).collect::<Vec<_>>(),
            ["F2"]
        );
        let refresh = keymap::binding_labels(&Refresh, "Workspace ConversationList", cx);
        assert!(refresh.iter().any(|l| l.as_ref() == "F5"), "{refresh:?}");
        assert!(
            refresh.iter().any(|l| l.as_ref() == "Ctrl+L"),
            "{refresh:?}"
        );
    });
    rebind(cx, chords("f3", "ctrl+l", "ctrl+f", "ctrl+r", "ctrl+x"));
    cx.update(|cx| {
        let labels = keymap::binding_labels(&Rename, "Workspace ConversationList", cx);
        assert_eq!(
            labels.iter().map(|l| l.as_ref()).collect::<Vec<_>>(),
            ["F3"]
        );
    });
}

/// A rejected binding changes nothing (reserved keys keep their editing
/// meaning), at startup (the default stays) and live.
#[gpui_kit::test]
fn reserved_keys_are_never_bound(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    for reserved in ["ctrl+c", "ctrl+v", "ctrl+a", "ctrl+z", "ctrl+y", "ctrl+w"] {
        let keys = KeyBindings {
            delete: parse_key_binding(reserved).unwrap(),
            ..KeyBindings::default()
        };
        let result = cx.update(|cx| keymap::install_user_bindings(cx, &keys));
        let error = result.expect_err(reserved);
        assert!(
            error.contains("reserved for clipboard and editing"),
            "{error}"
        );
    }
    cx.run_until_parked();
    cx.update(|cx| {
        let got = winner(cx, "ctrl-x", LIST);
        assert!(got.is_some_and(|a| a.partial_eq(&Delete)));
    });
}

/// C-053: a static key a user binding took over is not listed as live.
#[gpui_kit::test]
fn shortcuts_dialog_omits_keys_a_user_binding_took_over(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_handle, _ws) = open_main(cx, &world, &[]);
    let keys = KeyBindings {
        rename: parse_key_binding("f3").unwrap(),
        ..KeyBindings::default()
    };
    cx.update(|cx| keymap::install_user_bindings(cx, &keys).unwrap());
    cx.run_until_parked();
    cx.update(|cx| {
        let next = keymap::help_rows()
            .iter()
            .rfind(|row| row.label == "Next / prev match")
            .expect("row exists");
        let label = keymap::help_keys_for(next, false, cx);
        assert!(
            !label.split(" / ").any(|key| key == "F3"),
            "F3 now renames, not shown for next match: {label}"
        );
    });
}
