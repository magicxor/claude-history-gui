//! G tests of the Settings dialog (DESIGN §5.16, §11.18, §13.2): C-024,
//! C-027, C-030, C-033, C-034, C-059, C-060, L-063 (settings half).

use std::path::PathBuf;

use claude_history_core::config::{KeyBindings, parse_key_binding};
use claude_history_core::search::mode::SearchMode;
use claude_history_core::theme::{self as core_theme, ThemeKind, ThemePreference};
use gpui_kit::component::WindowExt as _;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, BorrowAppContext as _, Entity, KeyContext, Keystroke,
    TestAppContext,
};

use crate::actions::{OpenSettings, Refresh, Resume, SetThemeDark, SetThemeLight};
use crate::globals::{app_state, last_toast};
use crate::keymap;
use crate::model::{KeyAction, OpenPathsOptions, SettingsPage};
use crate::services::ServiceCall;
use crate::settings::draft::{Field, Value};
use crate::settings::{self, SettingsDialog, current_dialog};
use crate::test_support::{TestWorld, global_lock, kbd, open_main, recording};

fn render(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    use gpui_kit::test::TestWindowExt as _;
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
}

/// Let the dialog's open animation (wall-clock) finish, then draw it at rest
/// so a simulated click lands where the button really is.
fn settle(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    std::thread::sleep(std::time::Duration::from_millis(400));
    render(cx, handle);
    cx.run_until_parked();
    render(cx, handle);
}

fn has_dialog(cx: &mut TestAppContext, handle: AnyWindowHandle) -> bool {
    cx.update_window(handle, |_, window, cx| window.has_active_dialog(cx))
        .unwrap()
}

fn open_dialog(cx: &mut TestAppContext, handle: AnyWindowHandle) -> Entity<SettingsDialog> {
    cx.dispatch_action(handle, OpenSettings);
    cx.run_until_parked();
    render(cx, handle);
    cx.read(current_dialog)
        .expect("the Settings dialog is open")
}

fn set_text(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    dialog: &Entity<SettingsDialog>,
    field: Field,
    text: &str,
) {
    let dialog = dialog.clone();
    cx.update_window(handle, |_, window, cx| {
        dialog.update(cx, |dialog, cx| dialog.set_text(field, text, window, cx))
    })
    .unwrap();
    // Path checks and probes run in the background.
    cx.run_until_parked();
}

fn set_value(cx: &mut TestAppContext, dialog: &Entity<SettingsDialog>, field: Field, value: Value) {
    dialog.update(cx, |dialog, cx| dialog.set_value(field, value, cx));
}

fn save(cx: &mut TestAppContext, handle: AnyWindowHandle, dialog: &Entity<SettingsDialog>) {
    let dialog = dialog.clone();
    cx.update_window(handle, |_, window, cx| {
        dialog.update(cx, |dialog, cx| dialog.save(window, cx))
    })
    .unwrap();
    cx.run_until_parked();
}

fn gui_toml(world: &TestWorld) -> PathBuf {
    world.env.gui_config_file().expect("gui.toml path")
}

fn read_gui(world: &TestWorld) -> String {
    std::fs::read_to_string(gui_toml(world)).unwrap_or_default()
}

/// `secondary-,` opens Settings (General page), `open_settings(Some(page))`
/// switches the page, a second open re-uses the dialog and Esc closes it.
#[gpui_kit::test]
fn settings_opens_on_a_page_and_closes_with_escape(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (handle, _ws) = open_main(cx, &world, &[]);
    cx.simulate_keystrokes(handle, "secondary-,");
    cx.run_until_parked();
    render(cx, handle);
    let dialog = cx.read(current_dialog).expect("opened by the shortcut");
    assert_eq!(dialog.read_with(cx, |d, _| d.page()), SettingsPage::General);
    assert!(has_dialog(cx, handle));
    cx.update_window(handle, |_, window, cx| {
        settings::open_settings(Some(SettingsPage::Keys), window, cx)
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(dialog.read_with(cx, |d, _| d.page()), SettingsPage::Keys);
    assert_eq!(
        cx.read(current_dialog).map(|d| d.entity_id()),
        Some(dialog.entity_id()),
        "one dialog"
    );
    cx.simulate_keystrokes(handle, "escape");
    cx.run_until_parked();
    assert!(!has_dialog(cx, handle));
    assert!(cx.read(current_dialog).is_none());
    assert!(
        read_gui(&world).is_empty(),
        "closing writes nothing: {}",
        read_gui(&world)
    );
    // Every page renders.
    let dialog = open_dialog(cx, handle);
    for page in SettingsPage::ALL {
        dialog.update(cx, |d, cx| d.set_page(page, cx));
        render(cx, handle);
    }
}

/// C-033: change theme + Save → gui.toml has `theme = "dark"`, config.toml is
/// byte-identical, a comment line of gui.toml survives, the toast names the
/// path; Reset removes a key; Cancel writes nothing.
#[gpui_kit::test]
fn save_writes_only_gui_toml_and_reset_and_cancel_behave(cx: &mut TestAppContext) {
    let _guard = global_lock();
    let world = TestWorld::new();
    world.write_config(
        Some("[display]\nlast = false\n"),
        Some("# keep me\n[gui]\nterminal = \"wt\"\n"),
    );
    let config_path = world.env.config_file().unwrap();
    let config_bytes = std::fs::read(&config_path).unwrap();
    let (handle, _ws) = open_main(cx, &world, &[]);

    let dialog = open_dialog(cx, handle);
    set_value(
        cx,
        &dialog,
        Field::Theme,
        Value::Theme(ThemePreference::Dark),
    );
    assert!(dialog.read_with(cx, |d, _| d.draft().has_changes()));
    save(cx, handle, &dialog);
    let text = read_gui(&world);
    assert!(text.contains("theme = \"dark\""), "{text}");
    assert!(text.contains("# keep me"), "{text}");
    assert!(text.contains("terminal = \"wt\""), "{text}");
    assert_eq!(std::fs::read(&config_path).unwrap(), config_bytes);
    assert_eq!(
        cx.read(last_toast).as_ref(),
        format!("Settings saved to {}", gui_toml(&world).display())
    );
    assert!(!has_dialog(cx, handle), "Save closes the dialog");
    assert!(dialog.read_with(cx, |d, _| d.is_closed()));
    // The theme applies live, and AppState knows the saved file.
    assert_eq!(core_theme::kind(), ThemeKind::Dark);
    assert_eq!(
        cx.read(|cx| app_state(cx).config.gui.theme()),
        ThemePreference::Dark
    );

    // Cancel discards.
    let before = read_gui(&world);
    let dialog = open_dialog(cx, handle);
    set_text(cx, handle, &dialog, Field::Terminal, "kitty");
    set_value(cx, &dialog, Field::ShowThinking, Value::Bool(true));
    cx.update_window(handle, |_, window, cx| {
        dialog.update(cx, |d, cx| d.cancel(window, cx))
    })
    .unwrap();
    cx.run_until_parked();
    assert!(!has_dialog(cx, handle));
    assert_eq!(read_gui(&world), before, "Cancel writes nothing");

    // Reset removes the key; the rest of the file stays.
    let dialog = open_dialog(cx, handle);
    assert_eq!(
        dialog.read_with(cx, |d, _| d.draft().value(Field::Terminal)),
        Value::Text("wt".into())
    );
    cx.update_window(handle, |_, window, cx| {
        dialog.update(cx, |d, cx| d.reset(Field::Terminal, window, cx))
    })
    .unwrap();
    save(cx, handle, &dialog);
    let text = read_gui(&world);
    assert!(!text.contains("terminal"), "{text}");
    assert!(
        text.contains("# keep me") && text.contains("theme = \"dark\""),
        "{text}"
    );
    assert_eq!(std::fs::read(&config_path).unwrap(), config_bytes);

    // C-033: explicit defaults are written and read back as set; Reset on
    // Theme / Restore last session removes the keys.
    let dialog = open_dialog(cx, handle);
    set_value(
        cx,
        &dialog,
        Field::Theme,
        Value::Theme(ThemePreference::System),
    );
    set_value(cx, &dialog, Field::RestoreSession, Value::Bool(true));
    save(cx, handle, &dialog);
    let text = read_gui(&world);
    assert!(text.contains("theme = \"system\""), "{text}");
    assert!(text.contains("restore_session = true"), "{text}");
    let dialog = open_dialog(cx, handle);
    assert_eq!(
        dialog.read_with(cx, |d, _| d.draft().value(Field::Theme)),
        Value::Theme(ThemePreference::System)
    );
    cx.update_window(handle, |_, window, cx| {
        dialog.update(cx, |d, cx| {
            d.reset(Field::Theme, window, cx);
            d.reset(Field::RestoreSession, window, cx);
        })
    })
    .unwrap();
    save(cx, handle, &dialog);
    let text = read_gui(&world);
    assert!(
        !text.contains("theme") && !text.contains("restore_session"),
        "{text}"
    );

    // Saving without a change closes without writing or toasting.
    let before = read_gui(&world);
    let dialog = open_dialog(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        cx.update_global::<crate::globals::LastToast, _>(|toast, _| toast.0 = "none".into());
        dialog.update(cx, |d, cx| d.save(window, cx))
    })
    .unwrap();
    cx.run_until_parked();
    assert!(!has_dialog(cx, handle));
    assert_eq!(read_gui(&world), before);
    assert_eq!(cx.read(last_toast).as_ref(), "none");

    // Back to the light theme for the other tests of this process.
    cx.dispatch_action(handle, SetThemeLight);
    cx.run_until_parked();
}

/// C-033: the Save button of the rendered dialog drives the same code
/// (clicked through the test window).
#[gpui_kit::test]
fn the_save_and_cancel_buttons_work(cx: &mut TestAppContext) {
    use gpui_kit::test::TestWindowExt as _;
    let world = TestWorld::new();
    let (handle, _ws) = open_main(cx, &world, &[]);
    let dialog = open_dialog(cx, handle);
    set_text(cx, handle, &dialog, Field::Terminal, "kitty");
    settle(cx, handle);
    cx.update_window(handle, |_, window, cx| window.click("settings-save", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(
        read_gui(&world).contains("terminal = \"kitty\""),
        "{}",
        read_gui(&world)
    );
    assert!(!has_dialog(cx, handle));

    let dialog = open_dialog(cx, handle);
    set_text(cx, handle, &dialog, Field::Terminal, "xterm");
    settle(cx, handle);
    cx.update_window(handle, |_, window, cx| window.click("settings-cancel", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(read_gui(&world).contains("terminal = \"kitty\""));
    assert!(!has_dialog(cx, handle));
}

/// C-034: set Resume to `alt+r`, Save → `alt-r` dispatches Resume, `ctrl-r`
/// does not, `f5` still dispatches Refresh, `binding_labels(Resume)` = `Alt+R`.
#[gpui_kit::test]
fn saved_key_bindings_apply_live(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (handle, _ws) = open_main(cx, &world, &[]);
    let dialog = open_dialog(cx, handle);
    dialog.update(cx, |d, cx| d.set_page(SettingsPage::Keys, cx));
    set_text(cx, handle, &dialog, Field::Key(KeyAction::Resume), "alt+r");
    assert_eq!(
        dialog.read_with(cx, |d, _| d
            .field_error(Field::Key(KeyAction::Resume))
            .map(str::to_string)),
        None
    );
    save(cx, handle, &dialog);
    let text = read_gui(&world);
    assert!(text.contains("resume = \"alt+r\""), "{text}");
    cx.update(|cx| {
        let resolve = |keys: &str, ctx: &str| {
            let keystroke = Keystroke::parse(keys).unwrap();
            let stack: Vec<KeyContext> = ctx
                .split_whitespace()
                .map(|c| KeyContext::parse(c).unwrap())
                .collect();
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            let (found, _) = keymap.bindings_for_input(&[keystroke], &stack);
            found
                .iter()
                .map(|b| b.action().boxed_clone())
                .collect::<Vec<_>>()
        };
        let ctx = "Workspace ConversationList";
        assert!(
            resolve("alt-r", ctx)
                .first()
                .is_some_and(|a| a.partial_eq(&Resume))
        );
        assert!(!resolve("ctrl-r", ctx).iter().any(|a| a.partial_eq(&Resume)));
        assert!(
            resolve("f5", ctx)
                .first()
                .is_some_and(|a| a.partial_eq(&Refresh))
        );
        let labels = keymap::binding_labels(&Resume, ctx, cx);
        assert_eq!(
            labels.iter().map(|l| l.as_ref()).collect::<Vec<_>>(),
            [kbd("alt-r").as_str()]
        );
        assert_eq!(
            app_state(cx).settings.keys.resume,
            parse_key_binding("alt+r").unwrap()
        );
    });
}

/// C-027: Settings shows `Unknown modifier: hyper` for `hyper+x`, Save is
/// disabled while a field is invalid, and typing a valid key clears it.
#[gpui_kit::test]
fn an_invalid_key_blocks_save_with_the_core_message(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (handle, _ws) = open_main(cx, &world, &[]);
    let dialog = open_dialog(cx, handle);
    let field = Field::Key(KeyAction::Resume);
    set_text(cx, handle, &dialog, field, "hyper+x");
    assert_eq!(
        dialog
            .read_with(cx, |d, _| d.field_error(field).map(str::to_string))
            .as_deref(),
        Some("Unknown modifier: hyper")
    );
    assert!(!dialog.read_with(cx, |d, _| d.can_save()));
    save(cx, handle, &dialog);
    assert!(read_gui(&world).is_empty(), "a blocked save writes nothing");
    assert!(has_dialog(cx, handle));
    set_text(cx, handle, &dialog, field, "ctrl+c");
    assert!(
        dialog
            .read_with(cx, |d, _| d.field_error(field).map(str::to_string))
            .unwrap()
            .contains("reserved for clipboard and editing")
    );
    set_text(cx, handle, &dialog, field, "alt+r");
    assert!(dialog.read_with(cx, |d, _| d.field_error(field).is_none() && d.can_save()));
    // The Keys page warns about a plain character.
    set_text(cx, handle, &dialog, Field::Key(KeyAction::Fork), "r");
    let warnings = dialog.read_with(cx, |d, _| d.key_warnings());
    assert!(
        warnings
            .contains(&"\"r\": a plain character can't be typed in the search field".to_string()),
        "{warnings:?}"
    );
}

/// C-030: the Search page shows the effective mode with its source line;
/// saving `exact` writes `[search] mode = "exact"` only.
#[gpui_kit::test]
fn the_search_page_shows_the_source_and_saves_only_the_mode(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    world.write_config(Some("[search]\nmode = \"semantic\"\n"), None);
    let (handle, _ws) = open_main(cx, &world, &[]);
    let dialog = open_dialog(cx, handle);
    let (line, shown) = dialog.read_with(cx, |d, cx| {
        (
            d.source_line(Field::SearchMode, cx),
            d.shown(Field::SearchMode, cx),
        )
    });
    assert_eq!(line, "From config.toml: semantic");
    assert_eq!(shown, Value::Mode(SearchMode::Semantic));
    let note = dialog.read_with(cx, |d, cx| d.note_line(Field::SearchMode, cx));
    assert!(note.contains("Applies at the next start"), "{note}");
    set_value(
        cx,
        &dialog,
        Field::SearchMode,
        Value::Mode(SearchMode::Exact),
    );
    save(cx, handle, &dialog);
    assert_eq!(read_gui(&world).trim(), "[search]\nmode = \"exact\"");
    assert_eq!(
        cx.read(|cx| app_state(cx).settings.search_mode),
        SearchMode::Exact
    );
}

/// C-024: deprecated config.toml keys are shown read-only with their notes.
#[gpui_kit::test]
fn deprecated_keys_are_shown_read_only(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    world.write_config(Some("global = true\n[tui]\nsemantic_search = true\n"), None);
    let (handle, _ws) = open_main(cx, &world, &[]);
    let dialog = open_dialog(cx, handle);
    let lines = dialog.read_with(cx, |d, _| d.deprecated_lines());
    assert_eq!(
        lines,
        [
            "global = true (deprecated, ignored)",
            "[tui] semantic_search = true (used when [search] mode is unset)",
        ]
    );
    for page in [
        SettingsPage::General,
        SettingsPage::Search,
        SettingsPage::Display,
    ] {
        dialog.update(cx, |d, cx| d.set_page(page, cx));
        render(cx, handle);
    }
}

/// C-059: with a malformed gui.toml the dialog shows the parse error, Save is
/// disabled and the bytes stay unchanged.
#[gpui_kit::test]
fn a_broken_gui_toml_disables_save(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let broken = "[gui\nterminal = ";
    world.write_config(None, Some(broken));
    let (handle, _ws) = open_main(cx, &world, &[]);
    let dialog = open_dialog(cx, handle);
    let error = dialog.read_with(cx, |d, _| d.gui_error().map(str::to_string));
    assert!(
        error
            .as_deref()
            .is_some_and(|e| e.contains("Failed to parse config file")),
        "{error:?}"
    );
    assert!(!dialog.read_with(cx, |d, _| d.can_save()));
    set_text(cx, handle, &dialog, Field::Terminal, "kitty");
    save(cx, handle, &dialog);
    assert_eq!(read_gui(&world), broken);
    assert!(has_dialog(cx, handle), "the dialog stays open");

    // Open gui.toml / Reveal go through the services.
    let calls = recording(cx);
    calls.clear_calls();
    // The banner buttons exist while the error is set.
    render(cx, handle);
    // Fixing the file and pressing Retry re-reads it.
    std::fs::write(gui_toml(&world), "[gui]\nterminal = \"wt\"\n").unwrap();
    dialog.update(cx, |d, cx| d.retry_config(cx));
    cx.run_until_parked();
    assert!(dialog.read_with(cx, |d, _| d.gui_error().is_none() && d.can_save()));
    assert_eq!(
        dialog.read_with(cx, |d, _| d.draft().base.gui_settings().terminal),
        Some("wt".to_string())
    );
}

/// §11.18: a refusal at save time (the file broke since the dialog opened)
/// stays inline, the draft is kept and the dialog stays open.
#[gpui_kit::test]
fn a_save_error_stays_inline_and_keeps_the_draft(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (handle, _ws) = open_main(cx, &world, &[]);
    let dialog = open_dialog(cx, handle);
    set_text(cx, handle, &dialog, Field::Terminal, "kitty");
    std::fs::create_dir_all(gui_toml(&world).parent().unwrap()).unwrap();
    std::fs::write(gui_toml(&world), "[gui\nterminal = ").unwrap();
    save(cx, handle, &dialog);
    let error = dialog
        .read_with(cx, |d, _| d.save_error().map(str::to_string))
        .expect("an inline error");
    assert!(
        error.starts_with("gui.toml has errors; fix or remove it before saving:"),
        "{error}"
    );
    assert!(has_dialog(cx, handle));
    assert!(!dialog.read_with(cx, |d, _| d.is_saving()));
    assert_eq!(
        dialog.read_with(cx, |d, _| d.draft().value(Field::Terminal)),
        Value::Text("kitty".into()),
        "the draft is kept"
    );
    assert_eq!(
        read_gui(&world),
        "[gui\nterminal = ",
        "the file is untouched"
    );
}

/// C-060: change the theme from the View menu while Settings is open, then
/// change a key in Settings and Save → gui.toml holds both, and its comments;
/// the draft's untouched theme field follows the menu's change.
#[gpui_kit::test]
fn a_view_menu_theme_change_survives_a_settings_save(cx: &mut TestAppContext) {
    let _guard = global_lock();
    let world = TestWorld::new();
    world.write_config(None, Some("# my notes\n[gui]\nterminal = \"wt\"\n"));
    let (handle, _ws) = open_main(cx, &world, &[]);
    let dialog = open_dialog(cx, handle);
    // The View menu changes the theme while the dialog is open. The dialog
    // is the top-most dialog, but the action reaches the window root.
    cx.update_window(handle, |_, window, cx| {
        crate::theme_tokens::set_theme_preference(ThemePreference::Dark, true, window, cx)
    })
    .unwrap();
    cx.run_until_parked();
    assert!(read_gui(&world).contains("theme = \"dark\""));
    assert_eq!(
        dialog.read_with(cx, |d, _| d.draft().value(Field::Theme)),
        Value::Theme(ThemePreference::Dark),
        "the unedited theme field followed the menu"
    );
    set_text(cx, handle, &dialog, Field::Key(KeyAction::Rename), "f3");
    save(cx, handle, &dialog);
    let text = read_gui(&world);
    assert!(text.contains("theme = \"dark\""), "{text}");
    assert!(text.contains("rename = \"f3\""), "{text}");
    assert!(text.contains("# my notes"), "{text}");
    assert!(text.contains("terminal = \"wt\""), "{text}");
    cx.dispatch_action(handle, SetThemeDark);
    cx.dispatch_action(handle, SetThemeLight);
    cx.run_until_parked();
}

/// Typing into a rendered field (not the test API) validates and updates the
/// draft: the subscription of every input is wired.
#[gpui_kit::test]
fn typing_into_a_field_updates_the_draft(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (handle, _ws) = open_main(cx, &world, &[]);
    let dialog = open_dialog(cx, handle);
    dialog.update(cx, |d, cx| d.set_page(SettingsPage::Keys, cx));
    render(cx, handle);
    let field = Field::Key(KeyAction::Resume);
    let input = dialog.read_with(cx, |d, _| d.inputs[&field].clone());
    cx.update_window(handle, |_, window, cx| {
        input.update(cx, |state, cx| state.focus(window, cx))
    })
    .unwrap();
    render(cx, handle);
    cx.simulate_input(handle, "alt+r");
    cx.run_until_parked();
    assert_eq!(
        dialog.read_with(cx, |d, _| d.draft().value(field)),
        Value::Key(parse_key_binding("alt+r").unwrap())
    );
    assert!(dialog.read_with(cx, |d, _| d.can_save()));
    // Replace the text with an invalid key.
    cx.simulate_keystrokes(handle, "secondary-a");
    cx.simulate_input(handle, "hyper+x");
    cx.run_until_parked();
    assert_eq!(
        dialog
            .read_with(cx, |d, _| d.field_error(field).map(str::to_string))
            .as_deref(),
        Some("Unknown modifier: hyper")
    );
    assert!(!dialog.read_with(cx, |d, _| d.can_save()));
    // A list field: one entry per line.
    let area = dialog.read_with(cx, |d, _| d.areas[&Field::ExcludeProjects].clone());
    dialog.update(cx, |d, cx| d.set_page(SettingsPage::Search, cx));
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        use gpui_kit::Focusable as _;
        area.read(cx).focus_handle(cx).focus(window, cx)
    })
    .unwrap();
    render(cx, handle);
    cx.simulate_input(handle, "one");
    cx.simulate_keystrokes(handle, "enter");
    cx.simulate_input(handle, "two");
    cx.run_until_parked();
    assert_eq!(
        dialog.read_with(cx, |d, _| d.draft().value(Field::ExcludeProjects)),
        Value::List(vec!["one".into(), "two".into()])
    );
}

/// Browse… asks the platform for one file (programs) or folder (workspace)
/// and puts the answer into the field.
#[gpui_kit::test]
fn browse_fills_the_field_from_the_platform_prompt(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let program = world.add_fixture("bin/claude-harmless.cmd", b"@echo off\r\n");
    let (handle, _ws) = open_main(cx, &world, &[]);
    let dialog = open_dialog(cx, handle);
    let services = recording(cx);
    services.answer_open(Ok(Some(vec![program.clone()])));
    let d = dialog.clone();
    cx.update_window(handle, |_, window, cx| {
        d.update(cx, |d, cx| d.browse(Field::ClaudePath, window, cx))
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        dialog.read_with(cx, |d, _| d.draft().value(Field::ClaudePath)),
        Value::Text(program.display().to_string())
    );
    assert!(
        services
            .calls()
            .contains(&ServiceCall::PromptOpen(OpenPathsOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: "Choose program".into(),
            }))
    );
    let status = dialog.read_with(cx, |d, cx| {
        d.program_status(Field::ClaudePath, "claude", cx)
    });
    assert_eq!(status, format!("resolved: {}", program.display()));
    // A folder for the workspace; a cancelled prompt changes nothing.
    let folder = world.home().join("work");
    std::fs::create_dir_all(&folder).unwrap();
    services.answer_open(Ok(Some(vec![folder.clone()])));
    let d = dialog.clone();
    cx.update_window(handle, |_, window, cx| {
        d.update(cx, |d, cx| d.browse(Field::DefaultWorkspace, window, cx))
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        dialog.read_with(cx, |d, _| d.draft().value(Field::DefaultWorkspace)),
        Value::Text(folder.display().to_string())
    );
    let d = dialog.clone();
    cx.update_window(handle, |_, window, cx| {
        d.update(cx, |d, cx| d.browse(Field::PiPath, window, cx))
    })
    .unwrap();
    cx.run_until_parked();
    assert!(
        dialog
            .read_with(cx, |d, _| d.draft().value(Field::PiPath))
            .is_unset()
    );
    // A path that does not exist is an error and blocks Save.
    set_text(
        cx,
        handle,
        &dialog,
        Field::OmpPath,
        "Z:\\definitely\\missing\\omp.exe",
    );
    assert!(
        dialog
            .read_with(cx, |d, _| d.field_error(Field::OmpPath).map(str::to_string))
            .unwrap()
            .starts_with("File not found")
    );
    assert!(!dialog.read_with(cx, |d, _| d.can_save()));
}

/// The terminal preview shows the command a sample resume would run, and a
/// number field rejects a non-number and blocks Save.
#[gpui_kit::test]
fn terminal_preview_and_number_validation(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (handle, _ws) = open_main(cx, &world, &[]);
    let dialog = open_dialog(cx, handle);
    let preview = dialog.read_with(cx, |d, cx| d.terminal_preview(cx));
    assert!(preview.starts_with("Preview:"), "{preview}");
    set_text(cx, handle, &dialog, Field::Terminal, "cmd");
    let preview = dialog.read_with(cx, |d, cx| d.terminal_preview(cx));
    assert!(preview.starts_with("Preview:"), "{preview}");
    set_text(cx, handle, &dialog, Field::AgentTop, "0");
    assert_eq!(
        dialog
            .read_with(cx, |d, _| d
                .field_error(Field::AgentTop)
                .map(str::to_string))
            .as_deref(),
        Some("Enter a whole number of 1 or more")
    );
    assert!(!dialog.read_with(cx, |d, _| d.can_save()));
    set_text(cx, handle, &dialog, Field::AgentTop, "25");
    assert_eq!(
        dialog.read_with(cx, |d, _| d.draft().value(Field::AgentTop)),
        Value::Number(25)
    );
    // Lists take one entry per line.
    set_text(
        cx,
        handle,
        &dialog,
        Field::ExcludeProjects,
        "repo\n\n  other \n",
    );
    assert_eq!(
        dialog.read_with(cx, |d, _| d.draft().value(Field::ExcludeProjects)),
        Value::List(vec!["repo".into(), "other".into()])
    );
    save(cx, handle, &dialog);
    let text = read_gui(&world);
    assert!(text.contains("top = 25"), "{text}");
    assert!(text.contains("exclude_projects"), "{text}");
    let keys_after = cx.read(|cx| app_state(cx).settings.keys);
    assert_eq!(keys_after, KeyBindings::default());
    assert_eq!(
        cx.read(|cx| app_state(cx).settings.exclude_projects.clone()),
        vec!["repo".to_string(), "other".to_string()]
    );
}

/// The General page renders from stored probe results: no program lookup and
/// no path check runs while rendering (§15.8); results arrive from the
/// background and Save waits for a pending path check.
#[gpui_kit::test]
fn probes_run_in_the_background_and_save_waits_for_path_checks(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (handle, _ws) = open_main(cx, &world, &[]);
    let dialog = open_dialog(cx, handle);
    cx.run_until_parked();
    let (status, preview) = dialog.read_with(cx, |d, _| {
        (
            d.program_status_text(Field::ClaudePath),
            d.probes.terminal.clone(),
        )
    });
    assert!(
        status.starts_with("resolved:") || status == "not found on PATH",
        "{status}"
    );
    assert!(preview.starts_with("Preview:") && !preview.contains("checking"));
    // A typed path is pending until the background check finished.
    let d = dialog.clone();
    cx.update_window(handle, |_, window, cx| {
        d.update(cx, |d, cx| {
            d.set_text(Field::PiPath, r"Z:\missing\pi.exe", window, cx);
            assert!(!d.can_save(), "Save waits for the check");
            assert!(d.field_error(Field::PiPath).is_none());
        })
    })
    .unwrap();
    cx.run_until_parked();
    assert!(dialog.read_with(cx, |d, _| d.field_error(Field::PiPath).is_some()));
}

/// L-042: saving `exclude_projects` in Settings reaches the open list pane
/// (`apply_saved` -> `ListPane::set_exclude_projects`): the rows of the
/// excluded project disappear without a restart, and clearing the field
/// brings them back.
#[gpui_kit::test]
fn saving_excluded_projects_updates_the_open_list(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    crate::test_support::fixtures_list::listed_corpus(&world);
    let (handle, ws) = open_main(cx, &world, &[]);
    let list = cx.read(|cx| ws.read(cx).list().clone());
    let wait = |cx: &mut TestAppContext| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            cx.run_until_parked();
            let idle = list.read_with(cx, |list, _| {
                !list.is_loading()
                    && !list.loader().is_running()
                    && !list.browser().has_search_work_in_flight()
            });
            if idle {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the list never went idle"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    let projects = |cx: &TestAppContext| -> Vec<Option<String>> {
        list.read_with(cx, |list, _| {
            (0..list.browser().filtered().len())
                .map(|pos| list.browser().row(pos).unwrap().project_name.clone())
                .collect()
        })
    };
    wait(cx);
    let before = projects(cx);
    assert!(
        before.iter().flatten().any(|name| name == "api"),
        "{before:?}"
    );
    let dialog = open_dialog(cx, handle);
    set_text(cx, handle, &dialog, Field::ExcludeProjects, "api");
    save(cx, handle, &dialog);
    wait(cx);
    let hidden = projects(cx);
    assert!(
        !hidden.iter().flatten().any(|name| name == "api"),
        "the save hid the project: {hidden:?}"
    );
    assert!(!hidden.is_empty() && hidden.len() < before.len());
    // Clearing the field brings the rows back.
    let dialog = open_dialog(cx, handle);
    set_text(cx, handle, &dialog, Field::ExcludeProjects, "");
    save(cx, handle, &dialog);
    wait(cx);
    assert_eq!(projects(cx).len(), before.len());
}
