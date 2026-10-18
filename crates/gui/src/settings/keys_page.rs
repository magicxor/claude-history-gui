//! Settings → Keys: the five configurable bindings with live validation
//! (`keymap::validate_keys`, DESIGN §8.5). WP6. Pure logic: the dialog feeds
//! it the text of each key field and shows its result.

use claude_history_core::config::ConfigFile;

use super::draft::{Field, SettingsDraft, Value, effective_keys, parse_key_text};
use crate::keymap;
use crate::model::KeyAction;

/// A key field's new text: an empty text resets the binding; a text that does
/// not parse, or that the keymap rejects (reserved keys), is an error and the
/// draft keeps its previous value. The error is the message shown under the
/// field (`Unknown modifier: hyper`, `ctrl+c is reserved for clipboard and
/// editing`, …).
pub fn apply_key_text(
    draft: &mut SettingsDraft,
    cli: &ConfigFile,
    action: KeyAction,
    text: &str,
) -> Result<(), String> {
    let field = Field::Key(action);
    if text.trim().is_empty() {
        draft.reset(field);
        return Ok(());
    }
    let chord = parse_key_text(text)?;
    let mut probe = draft.edited.clone();
    super::draft::set(field, &mut probe, Value::Key(chord));
    let keys = effective_keys(cli, &probe);
    let validation = keymap::validate_bindings(&keys);
    if let Some((_, message)) = validation.errors.iter().find(|(a, _)| *a == action) {
        return Err(message.clone());
    }
    draft.set(field, Value::Key(chord));
    Ok(())
}

/// The warnings of the draft's effective keys: plain characters, Ctrl+T,
/// duplicates and replaced static bindings (§8.5), computed on the keys the
/// keymap really installs. A binding it rejected (a config.toml key, reserved
/// or unparseable; typed keys are refused at entry) is listed first: the
/// default stays in effect.
pub fn warnings(draft: &SettingsDraft, cli: &ConfigFile) -> Vec<String> {
    let configured = effective_keys(cli, &draft.edited);
    let (installed, validation) = keymap::sanitize_keys(&configured);
    let mut lines: Vec<String> = validation
        .errors
        .iter()
        .map(|(action, error)| {
            format!(
                "{} = {} was not applied: {error}; {} stays in effect",
                action.config_key(),
                keymap::chord_of(&configured, *action).config_string(),
                keymap::chord_of(&installed, *action).config_string()
            )
        })
        .collect();
    lines.extend(keymap::validate_bindings(&installed).warnings);
    lines
}

/// The text a key field starts with: the draft's own binding, else empty (the
/// field's placeholder shows the effective one).
pub fn initial_text(draft: &SettingsDraft, action: KeyAction) -> String {
    match draft.value(Field::Key(action)) {
        Value::Key(chord) => chord.config_string(),
        _ => String::new(),
    }
}
