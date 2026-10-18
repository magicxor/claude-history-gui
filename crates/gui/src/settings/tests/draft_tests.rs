//! P tests of the Settings draft and the key validation (DESIGN §8.5, §11.18,
//! §13.2): C-027, C-024, C-030, C-033, C-060.

use claude_history_core::HistoryEnv;
use claude_history_core::config::{
    AgentScopeConfig, ConfigFile, GuiConfigFile, KeysConfig, LayeredConfig, load_config_from,
    load_gui_config_from, load_layered_config_from, parse_key_binding, save_gui_config_changes,
};
use claude_history_core::search::mode::SearchMode;
use claude_history_core::theme::ThemePreference;

use crate::keymap;
use crate::model::KeyAction;
use crate::persist::{RememberedSearchMode, RememberedToolDisplay, UiState};
use crate::settings::draft::*;
use crate::settings::keys_page;

/// Both files parsed by core's loader from temp files.
fn layered(config_toml: &str, gui_toml: &str) -> LayeredConfig {
    let tmp = tempfile::tempdir().unwrap();
    let cli_path = tmp.path().join("config.toml");
    let gui_path = tmp.path().join("gui.toml");
    std::fs::write(&cli_path, config_toml).unwrap();
    std::fs::write(&gui_path, gui_toml).unwrap();
    load_layered_config_from(Some(&cli_path), Some(&gui_path)).expect("valid test files")
}

fn gui_from(gui_toml: &str) -> GuiConfigFile {
    layered("", gui_toml).gui_file
}

/// C-027: key syntax → GPUI keystrokes (`ctrl+r` → `ctrl-r`, `alt+r` →
/// `alt-r`, `f2` → `f2`) and the labels the TUI used.
#[test]
fn key_syntax_converts_to_gpui_keystrokes() {
    for (text, keystroke, label) in [
        ("ctrl+r", "ctrl-r", "Ctrl+R"),
        ("alt+r", "alt-r", "Alt+R"),
        ("f2", "f2", "F2"),
        ("Ctrl+X", "ctrl-x", "Ctrl+X"),
        ("meta+b", "alt-b", "Alt+B"),
        ("control+k", "ctrl-k", "Ctrl+K"),
    ] {
        let chord = parse_key_text(text).unwrap_or_else(|e| panic!("{text}: {e}"));
        assert_eq!(chord.gpui_keystroke(), keystroke, "{text}");
        assert_eq!(chord.help_label(), label, "{text}");
    }
    assert_eq!(
        parse_key_text("ctrl+r").unwrap().short_label(),
        "^R",
        "status-bar label"
    );
}

/// C-027: the core parser's messages reach the Keys page.
#[test]
fn key_validation_messages_are_the_core_ones() {
    let cfg = ConfigFile::default();
    let mut draft = SettingsDraft::new(&GuiConfigFile::default());
    let resume = KeyAction::Resume;
    assert_eq!(
        keys_page::apply_key_text(&mut draft, &cfg, resume, "hyper+x").unwrap_err(),
        "Unknown modifier: hyper"
    );
    assert_eq!(
        keys_page::apply_key_text(&mut draft, &cfg, resume, "ctrl+f2").unwrap_err(),
        "Function keys do not support modifiers: ctrl+f2"
    );
    assert!(
        keys_page::apply_key_text(&mut draft, &cfg, resume, "a+b+c")
            .unwrap_err()
            .starts_with("Invalid key binding")
    );
    assert!(
        keys_page::apply_key_text(&mut draft, &cfg, resume, "ctrl+f13")
            .unwrap_err()
            .starts_with("Unknown key")
    );
    assert_eq!(
        keys_page::apply_key_text(&mut draft, &cfg, resume, "ctrl+c").unwrap_err(),
        "ctrl+c is reserved for clipboard and editing"
    );
    assert!(
        draft.value(Field::Key(resume)).is_unset(),
        "errors keep the draft"
    );
    // Valid text is taken, empty text resets.
    keys_page::apply_key_text(&mut draft, &cfg, resume, "alt+r").unwrap();
    assert_eq!(
        draft.value(Field::Key(resume)),
        Value::Key(parse_key_binding("alt+r").unwrap())
    );
    keys_page::apply_key_text(&mut draft, &cfg, resume, "  ").unwrap();
    assert!(draft.value(Field::Key(resume)).is_unset());
}

/// §8.5: warnings are shown for plain characters, Ctrl+T, duplicates and
/// bindings that replace a static one; defaults give none.
#[test]
fn key_warnings_cover_the_documented_cases() {
    let none = ConfigFile::default();
    let draft = SettingsDraft::new(&GuiConfigFile::default());
    assert!(
        keys_page::warnings(&draft, &none).is_empty(),
        "defaults are clean"
    );

    let mut draft = SettingsDraft::new(&GuiConfigFile::default());
    let cfg = ConfigFile::default();
    keys_page::apply_key_text(&mut draft, &cfg, KeyAction::Resume, "r").unwrap();
    keys_page::apply_key_text(&mut draft, &cfg, KeyAction::Fork, "ctrl+t").unwrap();
    keys_page::apply_key_text(&mut draft, &cfg, KeyAction::Rename, "f9").unwrap();
    keys_page::apply_key_text(&mut draft, &cfg, KeyAction::Delete, "f9").unwrap();
    keys_page::apply_key_text(&mut draft, &cfg, KeyAction::Refresh, "ctrl+n").unwrap();
    let warnings = keys_page::warnings(&draft, &cfg);
    for expected in [
        "\"r\": a plain character can't be typed in the search field",
        "ctrl+t disables the Lexical/Semantic shortcut",
        "Rename and Delete share f9; Delete wins",
        "ctrl+n replaces \"Move selection\" in the conversation list",
    ] {
        assert!(
            warnings.contains(&expected.to_string()),
            "{expected}\n{warnings:#?}"
        );
    }
    // A viewer key.
    let mut draft = SettingsDraft::new(&GuiConfigFile::default());
    keys_page::apply_key_text(&mut draft, &cfg, KeyAction::Delete, "ctrl+d").unwrap();
    let warnings = keys_page::warnings(&draft, &cfg);
    assert!(
        warnings
            .iter()
            .any(|w| w.starts_with("ctrl+d replaces \"Half page down\" in the")),
        "{warnings:#?}"
    );
}

/// The startup validation (`keymap::validate_keys`) is the same code.
#[test]
fn validate_keys_matches_the_settings_page() {
    let keys = KeysConfig {
        resume: Some(parse_key_binding("ctrl+t").unwrap()),
        ..KeysConfig::default()
    };
    let validation = keymap::validate_keys(&keys);
    assert!(validation.errors.is_empty());
    assert_eq!(
        validation.warnings,
        ["ctrl+t disables the Lexical/Semantic shortcut"]
    );
}

fn sample(field: Field) -> Value {
    match field {
        Field::Theme => Value::Theme(ThemePreference::Dark),
        Field::Terminal => Value::Text("kitty".into()),
        Field::ClaudePath | Field::PiPath | Field::OmpPath | Field::DefaultWorkspace => {
            Value::Text(std::env::temp_dir().join("somewhere").display().to_string())
        }
        Field::RestoreSession => Value::Bool(false),
        Field::PreviewLast | Field::ToolDisplay | Field::ShowThinking | Field::Plain => {
            Value::Bool(true)
        }
        Field::SearchMode | Field::AgentMode => Value::Mode(SearchMode::Hybrid),
        Field::ExcludeProjects | Field::ResumeArgs | Field::AgentExclude => {
            Value::List(vec!["a".into(), "b c".into()])
        }
        Field::Key(_) => Value::Key(parse_key_binding("alt+z").unwrap()),
        Field::AgentScope => Value::Scope(AgentScopeConfig::Local),
        Field::AgentOutputChars | Field::AgentTop | Field::AgentWithinTop | Field::AgentHits => {
            Value::Number(7)
        }
        Field::AgentTools
        | Field::AgentToolResults
        | Field::AgentThinking
        | Field::AgentSubagents => Value::Bool(true),
    }
}

/// Every field round-trips through the gui.toml model and Reset removes it.
#[test]
fn every_field_sets_gets_and_resets() {
    assert_eq!(Field::ALL.len(), 30);
    for field in Field::ALL {
        let mut gui = GuiConfigFile::default();
        assert!(get(field, &gui).is_unset(), "{field:?} starts unset");
        set(field, &mut gui, sample(field));
        assert_eq!(get(field, &gui), sample(field), "{field:?}");
        // The model saves to a valid gui.toml and reads back the same.
        let (_tmp, env) = sandbox_with(None, None);
        let path = save_gui_config_changes(&env, &GuiConfigFile::default(), &gui)
            .unwrap_or_else(|e| panic!("{field:?}: {e}"));
        let back = load_gui_config_from(Some(&path)).unwrap_or_else(|e| panic!("{field:?}: {e}"));
        assert_eq!(get(field, &back), sample(field), "{field:?} via gui.toml");
        set(field, &mut gui, Value::Unset);
        assert!(get(field, &gui).is_unset(), "{field:?} resets");
        assert!(!field.label().is_empty() && !field.key_path().is_empty());
        assert!(field.applies().starts_with("Applies"));
    }
    // Ordering follows ALL (used by the dialog's maps).
    let mut sorted = Field::ALL.to_vec();
    sorted.sort();
    assert_eq!(sorted, Field::ALL.to_vec());
}

/// §13.2: the source line of a field.
#[test]
fn source_lines_name_where_the_value_comes_from() {
    let ui = UiState::default();
    let cfg = layered("[display]\nno_tools = false\n", "");
    assert_eq!(
        source_of(Field::ToolDisplay, &cfg, &ui).text,
        "From config.toml: Full"
    );
    assert_eq!(
        source_of(Field::ShowThinking, &cfg, &ui).text,
        "Default: off"
    );
    assert_eq!(
        source_of(Field::Key(KeyAction::Resume), &cfg, &ui).text,
        "Default: ctrl+r"
    );
    let cfg = layered(
        "[display]\nno_tools = false\n[keys]\nresume = \"ctrl+g\"\n",
        "[display]\nno_tools = true\n[gui]\ntheme = \"dark\"\n",
    );
    let source = source_of(Field::ToolDisplay, &cfg, &ui);
    assert_eq!(source.text, "From gui.toml");
    assert_eq!(source.kind, SourceKind::Gui);
    assert_eq!(
        source_of(Field::Key(KeyAction::Resume), &cfg, &ui).text,
        "From config.toml: ctrl+g"
    );
    assert_eq!(source_of(Field::Theme, &cfg, &ui).text, "From gui.toml");
    assert_eq!(
        source_of(Field::Theme, &layered("", ""), &ui).text,
        "Default: system"
    );
}

/// C-033: an explicit default in gui.toml reads as set, and Unset removes it.
#[test]
fn explicit_gui_defaults_read_as_from_gui_toml_and_unset_clears_them() {
    let ui = UiState::default();
    let mut gui = gui_from(
        "[gui]
theme = \"system\"
restore_session = true
",
    );
    assert_eq!(
        get(Field::Theme, &gui),
        Value::Theme(ThemePreference::System)
    );
    assert_eq!(get(Field::RestoreSession, &gui), Value::Bool(true));
    let cfg = layered(
        "",
        "[gui]
theme = \"system\"
restore_session = true
",
    );
    assert_eq!(source_of(Field::Theme, &cfg, &ui).text, "From gui.toml");
    assert_eq!(
        source_of(Field::RestoreSession, &cfg, &ui).text,
        "From gui.toml"
    );
    set(Field::Theme, &mut gui, Value::Unset);
    set(Field::RestoreSession, &mut gui, Value::Unset);
    assert_eq!(get(Field::Theme, &gui), Value::Unset);
    assert_eq!(get(Field::RestoreSession, &gui), Value::Unset);
}

/// PD17: remembered values show as such only with `restore_session` and no
/// config key; a config key always wins.
#[test]
fn remembered_values_never_mask_a_config_key() {
    let mut ui = UiState::default();
    ui.view.tool_display = Some(RememberedToolDisplay::Full);
    ui.view.show_thinking = Some(true);
    ui.search_mode = Some(RememberedSearchMode::Semantic);
    let cfg = layered("", "");
    assert_eq!(
        source_of(Field::ToolDisplay, &cfg, &ui).text,
        "Remembered from last session: Full"
    );
    assert_eq!(
        source_of(Field::ShowThinking, &cfg, &ui).text,
        "Remembered from last session: on"
    );
    assert_eq!(
        source_of(Field::SearchMode, &cfg, &ui).text,
        "Remembered from last session: semantic"
    );
    let configured = layered(
        "[search]\nmode = \"exact\"\n[display]\nno_tools = true\n",
        "",
    );
    assert_eq!(
        source_of(Field::SearchMode, &configured, &ui).text,
        "From config.toml: exact"
    );
    assert_eq!(
        source_of(Field::ToolDisplay, &configured, &ui).text,
        "From config.toml: Summary"
    );
    let off = layered("", "[gui]\nrestore_session = false\n");
    assert_eq!(
        source_of(Field::SearchMode, &off, &ui).text,
        "Default: lexical",
        "restore_session off: nothing is remembered"
    );
}

/// C-024: deprecated config.toml keys are listed read-only with their notes.
#[test]
fn deprecated_keys_are_listed_with_their_notes() {
    let cfg = layered(
        "global = true\n[tui]\nsemantic_search = true\n[display]\nrelative_time = false\n",
        "",
    );
    let notes = deprecated_notes(&cfg.cli);
    let lines: Vec<String> = notes
        .iter()
        .map(|n| format!("{} = {} ({})", n.key, n.value, n.note))
        .collect();
    assert_eq!(
        lines,
        [
            "global = true (deprecated, ignored)",
            "[display] relative_time = false (deprecated, ignored)",
            "[tui] semantic_search = true (used when [search] mode is unset)",
        ]
    );
    assert!(deprecated_notes(&ConfigFile::default()).is_empty());
}

/// §11.18 step 1: a field the user has not edited follows the new gui.toml
/// (the View menu changed the theme); an edited field keeps the user's value.
#[test]
fn unedited_fields_follow_the_state_and_edited_ones_do_not() {
    let start = gui_from("[gui]\nterminal = \"wt\"\n");
    let mut draft = SettingsDraft::new(&start);
    draft.set(
        Field::Key(KeyAction::Fork),
        Value::Key(parse_key_binding("alt+f").unwrap()),
    );
    assert!(draft.is_edited(Field::Key(KeyAction::Fork)));
    assert!(!draft.is_edited(Field::Theme));

    let updated =
        gui_from("[gui]\nterminal = \"wt\"\ntheme = \"dark\"\n[keys]\nfork = \"ctrl+g\"\n");
    let changed = draft.refresh_from(&updated);
    assert_eq!(
        draft.value(Field::Theme),
        Value::Theme(ThemePreference::Dark)
    );
    assert!(changed.contains(&Field::Theme));
    assert_eq!(
        draft.value(Field::Key(KeyAction::Fork)),
        Value::Key(parse_key_binding("alt+f").unwrap()),
        "the user's edit survives"
    );
    assert!(!changed.contains(&Field::Key(KeyAction::Fork)));
    // The new base knows the theme: it is not an edit of the user's.
    assert!(!draft.is_edited(Field::Theme));
    assert!(draft.is_edited(Field::Key(KeyAction::Fork)));
    assert!(draft.has_changes());
    draft.reset(Field::Key(KeyAction::Fork));
    // Reset removes the key, which differs from the base's ctrl+g.
    assert!(draft.is_edited(Field::Key(KeyAction::Fork)));
}

/// Text helpers: numbers, lists, paths.
#[test]
fn text_helpers_validate_input() {
    assert_eq!(parse_number_text(" 12 "), Ok(12));
    assert!(parse_number_text("0").is_err());
    assert!(parse_number_text("-3").is_err());
    assert!(parse_number_text("1.5").is_err());
    assert!(parse_number_text("").is_err());
    assert_eq!(lines_to_list(" a \n\n b c\n"), ["a", "b c"]);
    assert_eq!(list_to_lines(&["x".into(), "y".into()]), "x\ny");
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("prog.exe");
    std::fs::write(&file, b"x").unwrap();
    assert!(check_path_text(&file.display().to_string(), false).is_ok());
    assert!(check_path_text("", false).is_ok());
    assert!(
        check_path_text(&tmp.path().join("nope").display().to_string(), false)
            .unwrap_err()
            .starts_with("File not found")
    );
    assert!(check_path_text(&tmp.path().display().to_string(), true).is_ok());
    assert!(
        check_path_text(&file.display().to_string(), true)
            .unwrap_err()
            .starts_with("Folder not found")
    );
    assert!(
        check_path_text("relative/dir", true)
            .unwrap_err()
            .contains("absolute")
    );
}

fn sandbox_with(gui: Option<&str>, cli: Option<&str>) -> (tempfile::TempDir, HistoryEnv) {
    let tmp = tempfile::tempdir().unwrap();
    let env = HistoryEnv::sandbox(tmp.path());
    for (text, path) in [(gui, env.gui_config_file()), (cli, env.config_file())] {
        if let (Some(text), Some(path)) = (text, path) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
    }
    (tmp, env)
}

/// C-033 / C-060 (file level): a draft's change set writes only the keys that
/// changed, keeps comments and a concurrent change of the file (the View menu
/// saved the theme after the draft was taken), never touches config.toml.
#[test]
fn saving_a_draft_writes_only_changed_keys_and_keeps_concurrent_changes() {
    let (_tmp, env) = sandbox_with(
        Some("# my settings\n[gui]\nterminal = \"wt\" # keep\n"),
        Some("[display]\nlast = false\n"),
    );
    let gui_path = env.gui_config_file().unwrap();
    let cli_path = env.config_file().unwrap();
    let cli_bytes = std::fs::read(&cli_path).unwrap();
    let base = load_gui_config_from(Some(&gui_path)).unwrap();
    let mut draft = SettingsDraft::new(&base);

    // Meanwhile the View menu saves the theme (another writer).
    let mut theme_edit = base.clone();
    let mut gui = theme_edit.gui_settings();
    gui.theme = Some(ThemePreference::Dark);
    theme_edit.gui = Some(gui);
    save_gui_config_changes(&env, &base, &theme_edit).unwrap();

    // The user changes a key in Settings and saves.
    draft.set(
        Field::Key(KeyAction::Fork),
        Value::Key(parse_key_binding("alt+f").unwrap()),
    );
    draft.set(Field::SearchMode, Value::Mode(SearchMode::Exact));
    save_gui_config_changes(&env, &draft.base, &draft.edited).unwrap();

    let text = std::fs::read_to_string(&gui_path).unwrap();
    assert!(text.contains("# my settings"), "{text}");
    assert!(text.contains("terminal = \"wt\" # keep"), "{text}");
    assert!(
        text.contains("theme = \"dark\""),
        "the concurrent change survives: {text}"
    );
    assert!(text.contains("fork = \"alt+f\""), "{text}");
    assert!(text.contains("mode = \"exact\""), "{text}");
    assert!(
        !text.contains("restore_session"),
        "defaults are not written: {text}"
    );
    assert_eq!(
        std::fs::read(&cli_path).unwrap(),
        cli_bytes,
        "config.toml untouched"
    );
    let reread = load_gui_config_from(Some(&gui_path)).unwrap();
    assert_eq!(
        get(Field::Theme, &reread),
        Value::Theme(ThemePreference::Dark)
    );
    let _ = load_config_from(Some(&cli_path)).unwrap();
}

/// C-030 (file level): saving `exact` as the search mode writes only
/// `[search] mode = "exact"` into a missing gui.toml.
#[test]
fn saving_the_search_mode_writes_only_that_key() {
    let (_tmp, env) = sandbox_with(None, Some("[search]\nmode = \"semantic\"\n"));
    let gui_path = env.gui_config_file().unwrap();
    let base = GuiConfigFile::default();
    let mut draft = SettingsDraft::new(&base);
    draft.set(Field::SearchMode, Value::Mode(SearchMode::Exact));
    save_gui_config_changes(&env, &draft.base, &draft.edited).unwrap();
    let text = std::fs::read_to_string(&gui_path).unwrap();
    assert_eq!(text.trim(), "[search]\nmode = \"exact\"");
}

/// Effective keys: gui.toml over config.toml over the defaults.
#[test]
fn effective_keys_layer_gui_over_config() {
    let cfg = layered(
        "[keys]\nresume = \"ctrl+g\"\nfork = \"ctrl+h\"\n",
        "[keys]\nfork = \"alt+f\"\n",
    );
    let keys = effective_keys(&cfg.cli, &cfg.gui_file);
    assert_eq!(keys.resume, parse_key_binding("ctrl+g").unwrap());
    assert_eq!(keys.fork, parse_key_binding("alt+f").unwrap());
    assert_eq!(
        keys.rename,
        claude_history_core::config::KeyBindings::default().rename
    );
}

/// C-030/C-040: the deprecated `[tui] semantic_search` key is a config source
/// of the search mode and beats the remembered value, like `[search] mode`.
#[test]
fn legacy_semantic_search_key_is_the_search_mode_source() {
    let ui = UiState {
        search_mode: Some(RememberedSearchMode::Lexical),
        ..UiState::default()
    };
    let cfg = layered("[tui]\nsemantic_search = true\n", "");
    let source = source_of(Field::SearchMode, &cfg, &ui);
    assert_eq!(source.kind, SourceKind::Config);
    assert_eq!(source.value, Value::Mode(SearchMode::Semantic));
    assert!(source.text.contains("semantic_search"), "{}", source.text);
    // gui.toml's key wins over config.toml's; `[search] mode` wins over both.
    let cfg = layered(
        "[tui]\nsemantic_search = true\n",
        "[tui]\nsemantic_search = false\n",
    );
    let source = source_of(Field::SearchMode, &cfg, &ui);
    assert_eq!(source.kind, SourceKind::Gui);
    assert_eq!(source.value, Value::Mode(SearchMode::Lexical));
    let cfg = layered(
        "[tui]\nsemantic_search = true\n[search]\nmode = \"lexical\"\n",
        "",
    );
    assert_eq!(
        source_of(Field::SearchMode, &cfg, &ui).value,
        Value::Mode(SearchMode::Lexical)
    );
}

/// §8.5/C-034: a user binding also replaces static keys of ancestor contexts
/// (app-wide), the viewer's own context and the list search.
#[test]
fn key_warnings_cover_app_wide_viewer_and_list_search_rows() {
    let cfg = ConfigFile::default();
    for (key, needle) in [
        ("f1", "in the window"),
        ("f6", "in the window"),
        ("f10", "in the window"),
        ("ctrl+s", "in the window"),
        ("f3", "in the viewer"),
        ("ctrl+k", "in the list search"),
        ("ctrl+u", "in the list search"),
    ] {
        let mut draft = SettingsDraft::new(&GuiConfigFile::default());
        keys_page::apply_key_text(&mut draft, &cfg, KeyAction::Rename, key).unwrap();
        let warnings = keys_page::warnings(&draft, &cfg);
        assert!(
            warnings
                .iter()
                .any(|w| w.starts_with(&format!("{key} replaces")) && w.contains(needle)),
            "{key}: {warnings:#?}"
        );
    }
}

/// §8.5: a config.toml binding the keymap rejected at startup is reported on
/// the Keys page (the default stays in effect), not shown as in effect.
#[test]
fn a_rejected_config_binding_is_reported_on_the_keys_page() {
    let cli = load_cfg("[keys]\nresume = \"ctrl+c\"\n");
    let draft = SettingsDraft::new(&GuiConfigFile::default());
    let warnings = keys_page::warnings(&draft, &cli);
    assert!(
        warnings
            .iter()
            .any(|w| w.starts_with("resume = ctrl+c was not applied") && w.contains("ctrl+r")),
        "{warnings:#?}"
    );
}

fn load_cfg(text: &str) -> ConfigFile {
    layered(text, "").cli
}
