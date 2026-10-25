//! The workspace scope: which conversations belong to the directory
//! claude-history runs in. The TUI's project scope, `--local`, local agent
//! search and `--debug-search --local` all decide membership here.
//!
//! # Differences from upstream
//!
//! * **No process state.** Upstream's `Workspace::new(dir)` read the home
//!   directory itself and `Workspace::current()` the process cwd. Here a
//!   workspace is built from the validated [`WorkspaceScope`] (the GUI's
//!   workspace folder) and a home directory the caller passes:
//!   [`Workspace::new`], or [`Workspace::of_env`] for the
//!   [`HistoryEnv`]'s workspace and home (upstream `current()`).
//! * **Project directory name (W1).** It is the scope's
//!   ([`WorkspaceScope::project_dir_name`]), encoded from the normalized,
//!   non-canonical path, the form upstream's process cwd always had.
//! * **Comparable paths (W1).** The resolved directory and path-keyed
//!   projects are compared without a `\\?\` verbatim prefix
//!   ([`normalize_windows_path`]): Windows' `canonicalize` adds it, and a
//!   `\\?\C:` prefix component never equals the `C:` of a recorded or typed
//!   path. Elsewhere this changes nothing.
//! * **One comparison when the paths agree.** A recorded cwd is compared
//!   with the resolved directory only when it differs from the workspace
//!   directory (the same answer; the list's card scope runs this for every
//!   card outside the workspace's projects).

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use super::Conversation;
use super::model::SourceKind;
use super::path::{is_same_project, path_is_within, tilde_path};
use crate::env::{HistoryEnv, WorkspaceScope, normalize_windows_path};
use crate::error::Result;

/// The directory the workspace scope is anchored to.
#[derive(Clone, Debug)]
pub struct Workspace {
    dir: PathBuf,
    /// `dir` with symlinks resolved, which is what non-Claude project keys
    /// are compared against (fork: in comparable form).
    canonical_dir: PathBuf,
    /// Claude's project directory name for `dir`.
    project_dir_name: String,
    /// `dir` with the home directory written as `~`.
    display: String,
    /// Whether `canonical_dir` differs from `dir` (fork).
    resolves_elsewhere: bool,
}

impl Workspace {
    /// The workspace of `scope`, written with `~` for `home` (fork: upstream
    /// took the directory and read the home directory itself). Resolves the
    /// directory once, a file system call.
    pub fn new(scope: &WorkspaceScope, home: Option<&Path>) -> Self {
        let dir = scope.dir().to_path_buf();
        let canonical_dir = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        let canonical_dir = comparable(&canonical_dir).into_owned();
        Self {
            resolves_elsewhere: canonical_dir != dir,
            canonical_dir,
            project_dir_name: scope.project_dir_name().to_owned(),
            display: tilde_path(&dir, home),
            dir,
        }
    }

    /// The workspace of `env` (upstream `current()`, the process's working
    /// directory): `Ok(None)` when it has none, an error when it is not an
    /// absolute path ([`HistoryEnv::workspace`]).
    pub fn of_env(env: &HistoryEnv) -> Result<Option<Self>> {
        Ok(env
            .workspace()?
            .map(|scope| Self::new(&scope, env.home_dir.as_deref())))
    }

    /// The workspace directory with the home directory written as `~`.
    pub fn display(&self) -> &str {
        &self.display
    }

    /// Whether a conversation belongs to the workspace: its project is the
    /// workspace (for Claude, stored under the workspace's project directory
    /// or one of its worktrees'), or any cwd it recorded is the workspace or
    /// a directory below it.
    pub fn contains(&self, conversation: &Conversation) -> bool {
        let project_matches = if conversation.source == SourceKind::Claude {
            crate::history::sources::claude::project_dir(&conversation.path)
                .and_then(Path::file_name)
                .is_some_and(|name| self.contains_project_key(&name.to_string_lossy(), false))
        } else {
            conversation
                .project_path
                .as_ref()
                .or(conversation.cwd.as_ref())
                .is_some_and(|path| {
                    let path = path.canonicalize().unwrap_or_else(|_| path.clone());
                    self.contains_project_key(&path.to_string_lossy(), true)
                })
        };
        project_matches || conversation.cwds.iter().any(|cwd| self.contains_cwd(cwd))
    }

    /// Whether a project belongs to the workspace, given its workspace key:
    /// a Claude project directory name, or a canonical project path when
    /// `key_is_path`.
    pub fn contains_project_key(&self, key: &str, key_is_path: bool) -> bool {
        if key_is_path {
            path_is_within(&comparable(Path::new(key)), &self.canonical_dir)
        } else {
            is_same_project(key, &self.project_dir_name)
        }
    }

    /// Whether a recorded cwd is the workspace or a directory below it.
    pub fn contains_cwd(&self, cwd: &Path) -> bool {
        let cwd = comparable(cwd);
        path_is_within(&cwd, &self.dir)
            || (self.resolves_elsewhere && path_is_within(&cwd, &self.canonical_dir))
    }
}

/// `path` without a `\\?\` verbatim prefix (fork, W1; see the module notes).
/// Borrowed unless it has one, so recorded paths cost no allocation.
fn comparable(path: &Path) -> Cow<'_, Path> {
    if path.as_os_str().as_encoded_bytes().starts_with(br"\\?\") {
        Cow::Owned(normalize_windows_path(path))
    } else {
        Cow::Borrowed(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::path::convert_path_to_project_dir_name;

    fn conversation(source: SourceKind, project_dir_name: &str, cwds: &[&str]) -> Conversation {
        let cwds: Vec<PathBuf> = cwds.iter().map(PathBuf::from).collect();
        let mut conversation = crate::search::test_fixtures::one_message_conversation(
            "text",
            chrono::Local::now(),
            None,
            None,
            None,
        );
        conversation.path = PathBuf::from(format!(
            "/home/u/.claude/projects/{project_dir_name}/s.jsonl"
        ));
        conversation.source = source;
        conversation.project_path = cwds.first().cloned();
        conversation.cwd = cwds.first().cloned();
        conversation.cwds = cwds;
        conversation
    }

    fn claude(project_dir_name: &str, cwds: &[&str]) -> Conversation {
        conversation(SourceKind::Claude, project_dir_name, cwds)
    }

    /// Fork: the scope is built from its parts, so upstream's Unix-style
    /// paths work unchanged on Windows (where `WorkspaceScope::new` would
    /// refuse them as not absolute).
    fn workspace(dir: &str) -> Workspace {
        let scope =
            WorkspaceScope::from_parts(dir, convert_path_to_project_dir_name(Path::new(dir)));
        Workspace::new(&scope, None)
    }

    #[test]
    fn matches_claude_session_stored_under_workspace_or_worktree() {
        let ws = workspace("/nonexistent/group/repo-a");
        assert!(ws.contains(&claude("-nonexistent-group-repo-a", &[])));
        assert!(ws.contains(&claude("-nonexistent-group-repo-a--worktrees-fix", &[])));
        assert!(!ws.contains(&claude("-nonexistent-group-repo-b", &[])));
    }

    #[test]
    fn matches_session_that_moved_into_workspace() {
        let ws = workspace("/nonexistent/group/repo-a");
        let moved = claude(
            "-nonexistent-group",
            &[
                "/nonexistent/group",
                "/nonexistent/group/repo-b",
                "/nonexistent/group/repo-a/docs",
            ],
        );
        assert!(ws.contains(&moved));
        let sibling_only = claude(
            "-nonexistent-group",
            &["/nonexistent/group", "/nonexistent/group/repo-b"],
        );
        assert!(!ws.contains(&sibling_only));
    }

    #[test]
    fn cwd_must_match_on_component_boundary() {
        let ws = workspace("/nonexistent/group/repo");
        let other = claude("-nonexistent-group", &["/nonexistent/group/repo-other"]);
        assert!(!ws.contains(&other));
    }

    #[test]
    fn non_claude_sessions_match_by_cwd_below_workspace() {
        let ws = workspace("/nonexistent/group");
        for source in [SourceKind::Pi, SourceKind::Omp, SourceKind::Codex] {
            let inside = conversation(source, "ignored", &["/nonexistent/group/repo-a"]);
            assert!(ws.contains(&inside), "{source:?}");
            let outside = conversation(source, "ignored", &["/nonexistent/groupie"]);
            assert!(!ws.contains(&outside), "{source:?}");
        }
        let codex_moved = conversation(
            SourceKind::Codex,
            "ignored",
            &["/nonexistent/other", "/nonexistent/group/repo-a"],
        );
        assert!(ws.contains(&codex_moved));
    }

    #[test]
    fn project_keys_match_by_name_or_path() {
        let ws = workspace("/nonexistent/group");
        assert!(ws.contains_project_key("-nonexistent-group--worktrees-x", false));
        assert!(!ws.contains_project_key("-nonexistent-group-repo-a", false));
        assert!(ws.contains_project_key("/nonexistent/group/repo-a", true));
        assert!(!ws.contains_project_key("/nonexistent/group-b", true));
    }

    /// Fork: the display abbreviates the home the caller passes.
    #[test]
    fn display_writes_the_home_directory_as_a_tilde() {
        let home = Path::new(if cfg!(windows) {
            r"C:\Users\me"
        } else {
            "/home/me"
        });
        let dir = home.join("code").join("repo");
        let scope = WorkspaceScope::new(&dir).unwrap();
        let ws = Workspace::new(&scope, Some(home));
        assert_eq!(
            ws.display(),
            Path::new("~")
                .join("code")
                .join("repo")
                .display()
                .to_string()
        );
        assert_eq!(
            Workspace::new(&scope, None).display(),
            dir.display().to_string()
        );
        assert_eq!(
            Workspace::new(&WorkspaceScope::new(home).unwrap(), Some(home)).display(),
            "~"
        );
    }

    /// Fork (W1): Windows' canonical spelling of a path-keyed project and a
    /// recorded cwd in other letter case still belong to the workspace.
    #[cfg(windows)]
    #[test]
    fn windows_paths_match_in_canonical_form_and_any_case() {
        let scope = WorkspaceScope::new(r"C:\no-such-dir\group").unwrap();
        let ws = Workspace::new(&scope, None);
        assert!(ws.contains_project_key(r"\\?\C:\no-such-dir\group\repo", true));
        assert!(ws.contains_project_key(r"c:\No-Such-Dir\GROUP", true));
        assert!(!ws.contains_project_key(r"\\?\C:\no-such-dir\groupie", true));
        assert!(ws.contains_cwd(Path::new(r"c:\NO-SUCH-DIR\Group\repo")));
        assert!(ws.contains_cwd(Path::new(r"\\?\C:\no-such-dir\group")));
        assert!(ws.contains_cwd(Path::new("C:/no-such-dir/group/repo")));
        assert!(!ws.contains_cwd(Path::new(r"D:\no-such-dir\group")));
        let mut pi = conversation(SourceKind::Pi, "ignored", &[r"C:\no-such-dir\group\x"]);
        assert!(ws.contains(&pi));
        pi.cwds = vec![PathBuf::from(r"C:\no-such-dir\elsewhere")];
        pi.cwd = pi.cwds.first().cloned();
        pi.project_path = pi.cwd.clone();
        assert!(!ws.contains(&pi));
    }

    /// Fork: a workspace that exists is resolved, and recorded cwds below
    /// either spelling belong to it.
    #[test]
    fn existing_workspace_matches_cwds_through_its_resolved_path() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("repo");
        std::fs::create_dir(&dir).unwrap();
        let ws = Workspace::new(&WorkspaceScope::new(&dir).unwrap(), None);
        assert!(ws.contains_cwd(&dir.join("sub")));
        assert!(ws.contains_cwd(&dir.canonicalize().unwrap().join("sub")));
        assert!(ws.contains_project_key(&dir.canonicalize().unwrap().to_string_lossy(), true));
        assert!(!ws.contains_cwd(&temp.path().join("repo-other")));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_workspace_matches_cwds_through_either_path() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let ws = Workspace::new(&WorkspaceScope::new(&link).unwrap(), None);
        assert!(ws.contains_cwd(&link.join("sub")));
        assert!(ws.contains_cwd(&real.canonicalize().unwrap().join("sub")));
    }
}
