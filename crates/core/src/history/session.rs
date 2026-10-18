//! Blocking single-session operations a GUI runs off its UI thread and then
//! applies to a [`crate::browser::Browser`]: the UUID jump's disk lookup
//! (K2) and rename (K3).

use super::{Conversation, Source, find_jsonl_by_uuid, format_short_name_from_path, pi};
use crate::browser::RenameError;
use crate::env::HistoryEnv;
use crate::error::Result;
use std::path::Path;

/// Load the Claude session `<uuid>.jsonl` (the first one found in any
/// Claude project directory; the file-name match is the file system's)
/// for a UUID jump: parsed, with its project name and path (the
/// transcript's `cwd`, else the decoded project directory name) and the
/// preview chosen by `show_last` (addendum C1 fix: upstream always used
/// the first messages). `Ok(None)` when there is no such file or it parses
/// to nothing listable. Blocking; pass the result to
/// [`crate::browser::Browser::resolve_uuid_lookup`].
pub fn load_session_by_uuid(
    env: &HistoryEnv,
    uuid: &str,
    show_last: bool,
) -> Result<Option<Conversation>> {
    let Some(path) = find_jsonl_by_uuid(env, uuid)? else {
        return Ok(None);
    };
    let modified = path.metadata().ok().and_then(|m| m.modified().ok());
    let Some(mut conversation) = super::process_conversation_file(path, modified, None)? else {
        return Ok(None);
    };
    let fallback_path = conversation
        .path
        .parent()
        .and_then(|parent| parent.file_name())
        .map(|name| super::path::decode_project_dir_name_to_path(&name.to_string_lossy()))
        .unwrap_or_default();
    let project_path = conversation.cwd.clone().unwrap_or(fallback_path);
    conversation.project_name = Some(format_short_name_from_path(&project_path));
    conversation.project_path = Some(project_path);
    apply_preview_mode(&mut conversation, show_last);
    Ok(Some(conversation))
}

/// Rename the session at `path` (blocking; the GUI's background half of
/// a rename, applied with [`crate::browser::Browser::apply_renamed`]).
///
/// `title = title.trim()`; an empty title clears the custom title. The
/// rename is written in the session's own format: Claude `custom-title` +
/// `agent-name` records, Pi a `session_info` entry, OMP the title slot plus
/// a `title_change` record. A transcript that does not end with a newline
/// gets one first (Claude and Pi; upstream's Pi rename appended to the
/// last line). The file is then re-parsed; the preview follows `show_last`
/// (quirk 9 fix). The returned conversation has no project name or path:
/// the caller keeps the ones it has. `env` is the environment the session
/// belongs to (not consulted by the current formats).
pub fn rename_session(
    env: &HistoryEnv,
    path: &Path,
    source: Source,
    title: &str,
    show_last: bool,
) -> std::result::Result<Conversation, RenameError> {
    let _ = env;
    let title = title.trim();
    let written = match source {
        Source::Claude => super::append_session_rename(path, title),
        Source::Pi => pi::append_session_rename(path, title),
        Source::Omp => pi::append_omp_session_rename(path, title),
    };
    match written.and_then(|()| super::process_conversation_file(path.to_path_buf(), None, None)) {
        Ok(Some(mut conversation)) => {
            apply_preview_mode(&mut conversation, show_last);
            Ok(conversation)
        }
        Ok(None) => Err(RenameError::BecameEmpty),
        Err(error) => Err(RenameError::Failed(error)),
    }
}

/// Use the preview matching the loader's `show_last` setting
/// (`process_conversation_file` always picks the first messages).
pub(crate) fn apply_preview_mode(conversation: &mut Conversation, show_last: bool) {
    conversation.preview = if show_last {
        conversation.preview_last.clone()
    } else {
        conversation.preview_first.clone()
    };
}
