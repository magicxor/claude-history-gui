//! Explicit injection of everything upstream read from the process
//! environment (refactor R1).
//!
//! Upstream `claude-history` read `HOME`/`USERPROFILE`, `CLAUDE_CONFIG_DIR`,
//! the Pi/OMP variables and the process working directory from many places.
//! In core every such read goes through a [`HistoryEnv`] value instead:
//!
//! * the GUI builds one with [`HistoryEnv::from_process_env`] in `main` and
//!   sets [`HistoryEnv::workspace_dir`] from the user's workspace choice (a
//!   GUI has no meaningful process cwd, so core never calls
//!   `std::env::current_dir()`);
//! * tests build one with [`HistoryEnv::sandbox`], which keeps every path
//!   (Claude data, Pi/OMP sessions, caches, config, logs) under a temporary
//!   directory. `from_process_env` panics under `cfg(test)` so a test can
//!   never reach the real home directory by accident.
//!
//! Default locations (all relative to the home directory) keep this crate
//! isolated from the installed `claude-history` CLI:
//!
//! | What | Default |
//! |---|---|
//! | Claude data | `$CLAUDE_CONFIG_DIR` (absolute only) or `~/.claude` |
//! | caches (history, Pi/OMP, semantic embeddings, fastembed model) | `~/.cache/claude-history-gui` |
//! | CLI config (read-only, shared settings) | `~/.config/claude-history/config.toml` |
//! | GUI config (GUI settings and overrides) | `~/.config/claude-history/gui.toml` |
//! | debug log | `~/.local/state/claude-history-gui/debug.log` |

use crate::error::{AppError, Result};
use crate::history::path::convert_path_to_project_dir_name;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf, Prefix};

/// Directory name of this crate's cache root under `~/.cache`.
pub const CACHE_DIR_NAME: &str = "claude-history-gui";
/// Directory name of the debug-log state directory under `~/.local/state`.
pub const STATE_DIR_NAME: &str = "claude-history-gui";
/// Directory name of the (CLI-shared) config directory under `~/.config`.
pub const CONFIG_DIR_NAME: &str = "claude-history";
/// File name of the CLI's config file (never written by core).
pub const CLI_CONFIG_FILE_NAME: &str = "config.toml";
/// File name of the GUI's own config file.
pub const GUI_CONFIG_FILE_NAME: &str = "gui.toml";
/// File name of the semantic embedding cache inside the semantic cache dir.
pub const EMBEDDING_CACHE_FILE_NAME: &str = "embeddings-v1.bin";
/// Directory name of the fastembed model cache inside the semantic cache dir.
pub const MODEL_DIR_NAME: &str = "fastembed";

/// Every environment-derived input of the history/search/agent code.
///
/// All fields are public overrides. Accessor methods derive the documented
/// defaults from [`HistoryEnv::home_dir`] when an override is `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryEnv {
    /// Home directory (`home::home_dir()`, i.e. `%USERPROFILE%` on Windows).
    pub home_dir: Option<PathBuf>,
    /// Value of `CLAUDE_CONFIG_DIR`, taken verbatim (it also namespaces the
    /// per-project history cache, as upstream did). A blank value counts as
    /// unset and a relative one is an error ([`HistoryEnv::claude_dir`]).
    pub claude_config_dir: Option<PathBuf>,
    /// `PI_CODING_AGENT_DIR`
    pub pi_coding_agent_dir: Option<PathBuf>,
    /// `PI_CODING_AGENT_SESSION_DIR` (Pi and OMP)
    pub pi_coding_agent_session_dir: Option<PathBuf>,
    /// `PI_CONFIG_DIR` (OMP)
    pub pi_config_dir: Option<PathBuf>,
    /// `OMP_PROFILE`
    pub omp_profile: Option<OsString>,
    /// `PI_PROFILE`
    pub pi_profile: Option<OsString>,
    /// `XDG_DATA_HOME` (OMP only). A relative value resolves like the
    /// `PI_*` paths ([`HistoryEnv::env_value_base_dir`]: the launch
    /// directory, as upstream resolved it against its cwd, else the
    /// workspace) and is ignored without either (the XDG spec calls it
    /// invalid).
    pub xdg_data_home: Option<PathBuf>,
    /// `PATH`, used only to resolve external programs (see [`crate::program`]).
    pub path_var: Option<OsString>,
    /// `PATHEXT` (Windows), used only to resolve external programs.
    pub pathext: Option<OsString>,
    /// Replaces every upstream `std::env::current_dir()`: the GUI's current
    /// workspace directory, if any. Used for the workspace ("local") scope,
    /// Pi's `.pi/settings.json` files and their relative `sessionDir`, and as
    /// the base of relative paths in Pi/OMP environment variables when
    /// [`HistoryEnv::env_path_base`] is unset.
    pub workspace_dir: Option<PathBuf>,
    /// The directory relative values of the `PI_*` variables and of
    /// `XDG_DATA_HOME` were given in: the GUI's launch directory (upstream
    /// resolved them against its process cwd, which is where the variables
    /// were set). [`HistoryEnv::from_process_env`] leaves it unset; the
    /// GUI's `main` sets it. Only an absolute path is used (K16).
    pub env_path_base: Option<PathBuf>,
    /// Root of all caches. Default: `<home>/.cache/claude-history-gui`.
    pub cache_root: Option<PathBuf>,
    /// CLI config file (read-only). Default:
    /// `<home>/.config/claude-history/config.toml`.
    pub config_file: Option<PathBuf>,
    /// GUI config file. Default: `<home>/.config/claude-history/gui.toml`.
    pub gui_config_file: Option<PathBuf>,
    /// Debug log. Default: `<home>/.local/state/claude-history-gui/debug.log`.
    pub debug_log_file: Option<PathBuf>,
}

impl HistoryEnv {
    /// Read the real process environment. Call this once, from the GUI's
    /// `main`; library code must never call it.
    ///
    /// The workspace is left unset: a GUI's process cwd is meaningless (for
    /// example `C:\Windows\System32` when started from the Start menu), so
    /// the caller decides the workspace explicitly.
    #[cfg(not(any(test, feature = "hermetic")))]
    pub fn from_process_env() -> Self {
        Self {
            home_dir: home::home_dir(),
            // Upstream used `std::env::var` here (unset when not Unicode).
            // An empty value counts as unset (see `claude_dir`).
            claude_config_dir: std::env::var("CLAUDE_CONFIG_DIR")
                .ok()
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            pi_coding_agent_dir: std::env::var_os("PI_CODING_AGENT_DIR").map(PathBuf::from),
            pi_coding_agent_session_dir: std::env::var_os("PI_CODING_AGENT_SESSION_DIR")
                .map(PathBuf::from),
            pi_config_dir: std::env::var_os("PI_CONFIG_DIR").map(PathBuf::from),
            omp_profile: std::env::var_os("OMP_PROFILE"),
            pi_profile: std::env::var_os("PI_PROFILE"),
            xdg_data_home: std::env::var_os("XDG_DATA_HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            path_var: std::env::var_os("PATH"),
            pathext: std::env::var_os("PATHEXT"),
            workspace_dir: None,
            env_path_base: None,
            cache_root: None,
            config_file: None,
            gui_config_file: None,
            debug_log_file: None,
        }
    }

    /// Guard rail: unit tests (and every build with the `hermetic` feature,
    /// which the GUI enables for its own tests) must never read the real
    /// environment.
    #[cfg(any(test, feature = "hermetic"))]
    pub fn from_process_env() -> Self {
        panic!("tests must use HistoryEnv::sandbox, never the real process environment");
    }

    /// An environment rooted entirely inside `root` (for tests and demos).
    ///
    /// `root` acts as the home directory, so Claude data lives in
    /// `root/.claude`, Pi in `root/.pi`, OMP in `root/.omp`, caches in
    /// `root/.cache/claude-history-gui`, config in `root/.config/...` and the
    /// debug log in `root/.local/state/...`. No process environment variable
    /// is consulted. Override individual fields (or use the `with_*`
    /// helpers) to point at other temporary directories.
    pub fn sandbox(root: &Path) -> Self {
        Self {
            home_dir: Some(root.to_path_buf()),
            ..Self::default()
        }
    }

    /// Set the workspace directory (replaces the upstream process cwd).
    pub fn with_workspace(mut self, dir: impl Into<PathBuf>) -> Self {
        self.workspace_dir = Some(dir.into());
        self
    }

    /// Set `CLAUDE_CONFIG_DIR`.
    pub fn with_claude_config_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.claude_config_dir = Some(dir.into());
        self
    }

    /// Set `PI_CODING_AGENT_DIR`.
    pub fn with_pi_coding_agent_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.pi_coding_agent_dir = Some(dir.into());
        self
    }

    /// Set `PI_CODING_AGENT_SESSION_DIR` (flat Pi/OMP session directory).
    pub fn with_pi_session_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.pi_coding_agent_session_dir = Some(dir.into());
        self
    }

    /// Set the cache root.
    pub fn with_cache_root(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cache_root = Some(dir.into());
        self
    }

    fn missing_home() -> AppError {
        AppError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Could not determine home directory",
        ))
    }

    /// `CLAUDE_CONFIG_DIR` unless it is blank (an empty or whitespace-only
    /// value is treated as unset).
    fn claude_config_override(&self) -> Option<&Path> {
        self.claude_config_dir
            .as_deref()
            .filter(|dir| !dir.as_os_str().to_string_lossy().trim().is_empty())
    }

    /// The Claude data directory: `CLAUDE_CONFIG_DIR`, else `<home>/.claude`.
    ///
    /// A blank `CLAUDE_CONFIG_DIR` counts as unset. A relative one is an
    /// [`AppError::InvalidClaudeConfigDir`] error: upstream resolved it
    /// against the shell's cwd, but a GUI's process cwd is arbitrary (D2),
    /// and listing, deletes and resume copies would act on an unrelated
    /// directory. It is not resolved against the workspace either, which
    /// has nothing to do with where Claude Code ran.
    pub fn claude_dir(&self) -> Result<PathBuf> {
        if let Some(config_dir) = self.claude_config_override() {
            if !config_dir.is_absolute() {
                return Err(AppError::InvalidClaudeConfigDir(config_dir.to_path_buf()));
            }
            return Ok(config_dir.to_path_buf());
        }
        let home = self.home_dir.as_ref().ok_or_else(Self::missing_home)?;
        Ok(home.join(".claude"))
    }

    /// The root Claude projects directory (`<claude dir>/projects`).
    pub fn claude_projects_root(&self) -> Result<PathBuf> {
        Ok(self.claude_dir()?.join("projects"))
    }

    /// The Claude projects directory for a working directory
    /// (`<projects root>/<encoded dir>`).
    pub fn claude_projects_dir_for(&self, dir: &Path) -> Result<PathBuf> {
        Ok(self
            .claude_projects_root()?
            .join(workspace_project_dir_name(dir)))
    }

    /// Root of every cache: `<home>/.cache/claude-history-gui` by default.
    pub fn cache_root(&self) -> Option<PathBuf> {
        self.cache_root.clone().or_else(|| {
            self.home_dir
                .as_ref()
                .map(|home| home.join(".cache").join(CACHE_DIR_NAME))
        })
    }

    /// Directory of the per-project Claude history caches. Namespaced by
    /// `CLAUDE_CONFIG_DIR` exactly like upstream (`config-{hash}/projects`);
    /// a blank value uses the default namespace, like [`Self::claude_dir`].
    pub fn history_cache_dir(&self) -> Option<PathBuf> {
        let base = self.cache_root()?;
        if let Some(config_dir) = self.claude_config_override() {
            // Upstream hashed the `String` value of CLAUDE_CONFIG_DIR; hashing
            // the same text as `&str` yields the same DefaultHasher value.
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(config_dir.to_string_lossy().as_ref(), &mut hasher);
            let hash = std::hash::Hasher::finish(&hasher);
            Some(base.join(format!("config-{:016x}", hash)).join("projects"))
        } else {
            Some(base.join("projects"))
        }
    }

    /// Cache file of a Pi/OMP session root:
    /// `<cache root>/<source>/root-{hash}/sessions.bin`.
    pub fn source_cache_path(&self, root: &Path, source: &str) -> Option<PathBuf> {
        use std::hash::{Hash, Hasher};

        let resolved = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        resolved.hash(&mut hasher);
        Some(
            self.cache_root()?
                .join(source)
                .join(format!("root-{:016x}", hasher.finish()))
                .join("sessions.bin"),
        )
    }

    /// Semantic cache directory (`<cache root>/semantic`): holds the
    /// embedding cache file and the fastembed model directory.
    pub fn semantic_cache_dir(&self) -> Option<PathBuf> {
        self.cache_root().map(|root| root.join("semantic"))
    }

    /// The semantic embedding cache file (`<semantic dir>/embeddings-v1.bin`).
    pub fn embedding_cache_file(&self) -> Option<PathBuf> {
        self.semantic_cache_dir()
            .map(|dir| dir.join(EMBEDDING_CACHE_FILE_NAME))
    }

    /// The fastembed model directory (`<semantic dir>/fastembed`). It is the
    /// only model location: `HF_HOME`, `HF_ENDPOINT` and the Hugging Face
    /// token in the real home are never consulted
    /// ([`crate::semantic::fastembed::FastembedEmbedder::new_in`]).
    pub fn semantic_model_dir(&self) -> Option<PathBuf> {
        self.semantic_cache_dir()
            .map(|dir| dir.join(MODEL_DIR_NAME))
    }

    /// The CLI's config file (read-only).
    pub fn config_file(&self) -> Option<PathBuf> {
        self.config_file.clone().or_else(|| {
            self.home_dir.as_ref().map(|home| {
                home.join(".config")
                    .join(CONFIG_DIR_NAME)
                    .join(CLI_CONFIG_FILE_NAME)
            })
        })
    }

    /// The GUI's own config file.
    pub fn gui_config_file(&self) -> Option<PathBuf> {
        self.gui_config_file.clone().or_else(|| {
            self.home_dir.as_ref().map(|home| {
                home.join(".config")
                    .join(CONFIG_DIR_NAME)
                    .join(GUI_CONFIG_FILE_NAME)
            })
        })
    }

    /// The debug log file.
    pub fn debug_log_file(&self) -> Option<PathBuf> {
        self.debug_log_file.clone().or_else(|| {
            self.home_dir.as_ref().map(|home| {
                home.join(".local")
                    .join("state")
                    .join(STATE_DIR_NAME)
                    .join("debug.log")
            })
        })
    }

    /// The workspace scope derived from [`HistoryEnv::workspace_dir`]:
    /// `Ok(None)` when no workspace is set, [`AppError::InvalidWorkspace`]
    /// when it is not an absolute path (see [`WorkspaceScope::new`]).
    pub fn workspace(&self) -> Result<Option<WorkspaceScope>> {
        self.workspace_dir
            .as_deref()
            .map(WorkspaceScope::new)
            .transpose()
    }

    /// The workspace directory as the base of relative paths (Pi's settings
    /// files, and Pi/OMP environment variables without
    /// [`HistoryEnv::env_path_base`]): the normalized workspace, or `None`
    /// when none is set or it is not absolute (a relative base would
    /// silently resolve against the process cwd).
    pub fn workspace_base_dir(&self) -> Option<PathBuf> {
        self.workspace()
            .ok()
            .flatten()
            .map(|workspace| workspace.dir)
    }

    /// Set [`HistoryEnv::env_path_base`] (the launch directory).
    pub fn with_env_path_base(mut self, dir: impl Into<PathBuf>) -> Self {
        self.env_path_base = Some(dir.into());
        self
    }

    /// The launch directory as the base of relative environment-variable
    /// paths: [`HistoryEnv::env_path_base`] when it is absolute (normalized),
    /// else `None`.
    pub fn launch_base_dir(&self) -> Option<PathBuf> {
        self.env_path_base
            .as_deref()
            .map(normalize_workspace_path)
            .filter(|dir| dir.is_absolute())
    }

    /// The base of relative paths in the `PI_*` variables (K16): the launch
    /// directory ([`HistoryEnv::launch_base_dir`]), else the workspace
    /// ([`HistoryEnv::workspace_base_dir`]).
    pub fn env_value_base_dir(&self) -> Option<PathBuf> {
        self.launch_base_dir().or_else(|| self.workspace_base_dir())
    }
}

/// A workspace directory plus its Claude project directory name.
///
/// This replaces upstream's "current project" (derived from the process
/// cwd). Upstream always encoded `std::env::current_dir()`, which is
/// absolute and normalized; a workspace typed by the user (`--workspace`,
/// `gui.toml`) may be neither. [`WorkspaceScope::new`] therefore only
/// accepts absolute paths and normalizes them first
/// ([`normalize_workspace_path`]), so `C:\proj\`, `C:\proj\.` and
/// `C:\x\..\proj` all map to Claude Code's `C--proj`. The fields are private
/// so that every scope goes through that validation.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WorkspaceScope {
    dir: PathBuf,
    project_dir_name: String,
}

impl WorkspaceScope {
    /// The scope of the workspace folder `dir`.
    ///
    /// `dir` must be absolute ([`Path::is_absolute`]; on Windows that means
    /// a drive with a root, `C:\...`, or a UNC path): Claude Code encodes its
    /// own absolute working directory, and core never resolves a relative
    /// path against the process cwd. A GUI that accepts a relative
    /// `--workspace` makes it absolute against its own launch directory
    /// first. Errors with [`AppError::InvalidWorkspace`].
    pub fn new(dir: impl AsRef<Path>) -> Result<Self> {
        let given = dir.as_ref();
        let dir = normalize_workspace_path(given);
        if !dir.is_absolute() {
            return Err(AppError::InvalidWorkspace(given.to_path_buf()));
        }
        let project_dir_name = convert_path_to_project_dir_name(&dir);
        Ok(Self {
            dir,
            project_dir_name,
        })
    }

    /// A scope with an arbitrary directory and project name (tests only).
    #[cfg(test)]
    pub(crate) fn from_parts(dir: impl Into<PathBuf>, project_dir_name: impl Into<String>) -> Self {
        Self {
            dir: dir.into(),
            project_dir_name: project_dir_name.into(),
        }
    }

    /// The normalized, absolute workspace directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `convert_path_to_project_dir_name(dir)`, computed from the normalized
    /// but NON-canonicalized path (see [`workspace_project_dir_name`]).
    pub fn project_dir_name(&self) -> &str {
        &self.project_dir_name
    }

    /// The workspace directory, canonicalized when possible (for path
    /// equality checks against Pi/OMP session working directories).
    pub fn canonical_dir(&self) -> PathBuf {
        self.dir.canonicalize().unwrap_or_else(|_| self.dir.clone())
    }
}

/// Claude's project directory name for a directory.
///
/// Claude Code encodes the path it was started in, which is absolute, has no
/// `.`/`..` components or trailing separator, and on Windows has no `\\?\`
/// verbatim prefix and an upper-case drive letter (`C:\...` becomes
/// `C--...`). A canonicalized path (`\\?\C:\...`), a lower-case drive letter
/// (Windows issue W1) or a trailing separator would encode differently and
/// never match, so the path is normalized first
/// ([`normalize_workspace_path`]). Other paths encode exactly as upstream's
/// `convert_path_to_project_dir_name`.
pub fn workspace_project_dir_name(dir: &Path) -> String {
    convert_path_to_project_dir_name(&normalize_workspace_path(dir))
}

/// Lexically normalize a directory path to the form of a process working
/// directory, without touching the file system:
///
/// * a `\\?\` verbatim prefix is removed and the drive letter upper-cased
///   ([`normalize_windows_path`]);
/// * `.` components and repeated separators are dropped, and `..` removes
///   the previous component (at the root it is dropped);
/// * trailing separators are removed, except on a root (`/`, `C:\`); a UNC
///   share root is written without one (`\\server\share`), as Node's
///   `process.cwd()` (which Claude Code encodes) reports it.
///
/// Separators are written in the platform's style. `..` is resolved
/// lexically, as Windows does for a working directory; on Unix a `..` after
/// a symbolic link may therefore name a different directory than the kernel
/// would (pass such paths canonicalized). Relative paths stay relative, with
/// leading `..` kept.
pub fn normalize_workspace_path(path: &Path) -> PathBuf {
    let path = normalize_windows_path(path);
    let mut prefix: Option<std::path::PrefixComponent<'_>> = None;
    let mut root = false;
    let mut parts: Vec<Component<'_>> = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(value) => prefix = Some(value),
            Component::RootDir => root = true,
            Component::CurDir => {}
            Component::ParentDir => match parts.last() {
                Some(Component::Normal(_)) => {
                    parts.pop();
                }
                // `..` at the root stays at the root.
                _ if root || prefix.is_some_and(|value| has_implicit_root(value.kind())) => {}
                _ => parts.push(component),
            },
            Component::Normal(_) => parts.push(component),
        }
    }
    let mut normalized = PathBuf::new();
    let implicit_root = prefix.is_some_and(|value| has_implicit_root(value.kind()));
    if let Some(prefix) = prefix {
        normalized.push(prefix.as_os_str());
    }
    if root && !implicit_root {
        normalized.push(Component::RootDir.as_os_str());
    }
    for part in parts {
        normalized.push(part.as_os_str());
    }
    normalized
}

/// UNC, device and verbatim prefixes imply a root; a drive (`C:`) does not.
fn has_implicit_root(prefix: Prefix<'_>) -> bool {
    !matches!(prefix, Prefix::Disk(_))
}

/// Strip a `\\?\` verbatim prefix in front of a drive (`\\?\C:\...`) or a
/// UNC path (`\\?\UNC\server\share` becomes `\\server\share`) and
/// upper-case a leading drive letter. Other paths, including other verbatim
/// paths (`\\?\Volume{...}\`), are returned unchanged on every OS.
pub fn normalize_windows_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let mut text: String = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text
        .strip_prefix(r"\\?\")
        .filter(|rest| has_drive_prefix(rest))
    {
        rest.to_string()
    } else {
        return uppercase_drive_letter(path);
    };
    if has_drive_prefix(&text) {
        text[..1].make_ascii_uppercase();
    }
    PathBuf::from(text)
}

fn uppercase_drive_letter(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if has_drive_prefix(&text) && text.as_bytes()[0].is_ascii_lowercase() {
        let mut text = text.into_owned();
        text[..1].make_ascii_uppercase();
        PathBuf::from(text)
    } else {
        path.to_path_buf()
    }
}

/// `X:\` or `X:/` at the start of the string.
fn has_drive_prefix(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "tests must use HistoryEnv::sandbox")]
    fn process_env_is_forbidden_in_tests() {
        let _ = HistoryEnv::from_process_env();
    }

    #[test]
    fn sandbox_keeps_every_path_under_root() {
        let root = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(root.path());
        let paths = [
            env.claude_dir().unwrap(),
            env.claude_projects_root().unwrap(),
            env.cache_root().unwrap(),
            env.history_cache_dir().unwrap(),
            env.source_cache_path(root.path(), "pi").unwrap(),
            env.semantic_cache_dir().unwrap(),
            env.embedding_cache_file().unwrap(),
            env.semantic_model_dir().unwrap(),
            env.config_file().unwrap(),
            env.gui_config_file().unwrap(),
            env.debug_log_file().unwrap(),
        ];
        for path in paths {
            assert!(
                path.starts_with(root.path()),
                "{path:?} escapes the sandbox"
            );
        }
    }

    #[test]
    fn default_locations_are_isolated_from_the_cli() {
        let home = PathBuf::from("home-dir");
        let env = HistoryEnv {
            home_dir: Some(home.clone()),
            ..HistoryEnv::default()
        };
        assert_eq!(env.claude_dir().unwrap(), home.join(".claude"));
        assert_eq!(
            env.claude_projects_root().unwrap(),
            home.join(".claude").join("projects")
        );
        let cache = home.join(".cache").join("claude-history-gui");
        assert_eq!(env.cache_root().unwrap(), cache);
        assert_eq!(env.history_cache_dir().unwrap(), cache.join("projects"));
        assert_eq!(
            env.embedding_cache_file().unwrap(),
            cache.join("semantic").join("embeddings-v1.bin")
        );
        assert_eq!(
            env.semantic_model_dir().unwrap(),
            cache.join("semantic").join("fastembed")
        );
        let config = home.join(".config").join("claude-history");
        assert_eq!(env.config_file().unwrap(), config.join("config.toml"));
        assert_eq!(env.gui_config_file().unwrap(), config.join("gui.toml"));
        assert_eq!(
            env.debug_log_file().unwrap(),
            home.join(".local")
                .join("state")
                .join("claude-history-gui")
                .join("debug.log")
        );
    }

    fn absolute(name: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!(r"C:\{name}"))
        } else {
            PathBuf::from(format!("/{name}"))
        }
    }

    #[test]
    fn overrides_win_over_home_defaults() {
        let env = HistoryEnv {
            home_dir: None,
            claude_config_dir: Some(absolute("cfg")),
            cache_root: Some(PathBuf::from("cache")),
            config_file: Some(PathBuf::from("c.toml")),
            gui_config_file: Some(PathBuf::from("g.toml")),
            debug_log_file: Some(PathBuf::from("d.log")),
            ..HistoryEnv::default()
        };
        assert_eq!(env.claude_dir().unwrap(), absolute("cfg"));
        assert_eq!(
            env.claude_projects_root().unwrap(),
            absolute("cfg").join("projects")
        );
        assert_eq!(env.cache_root().unwrap(), PathBuf::from("cache"));
        assert_eq!(env.config_file().unwrap(), PathBuf::from("c.toml"));
        assert_eq!(env.gui_config_file().unwrap(), PathBuf::from("g.toml"));
        assert_eq!(env.debug_log_file().unwrap(), PathBuf::from("d.log"));
        // CLAUDE_CONFIG_DIR namespaces the history cache like upstream.
        let history = env.history_cache_dir().unwrap();
        assert!(history.starts_with("cache"));
        assert!(history.ends_with("projects"));
        let namespace = history
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap();
        assert!(namespace.starts_with("config-") && namespace.len() == "config-".len() + 16);
    }

    #[test]
    fn missing_home_is_an_error_for_claude_dir_and_none_elsewhere() {
        let env = HistoryEnv::default();
        assert!(env.claude_dir().is_err());
        assert!(env.cache_root().is_none());
        assert!(env.config_file().is_none());
        assert!(env.debug_log_file().is_none());
    }

    #[test]
    fn blank_claude_config_dir_is_unset_and_a_relative_one_is_refused() {
        // D2: `rel/projects` would resolve against the GUI's arbitrary
        // process cwd for listing, deletes and resume copies.
        let home = tempfile::tempdir().unwrap();
        let default = HistoryEnv::sandbox(home.path());
        for blank in ["", "  "] {
            let env = HistoryEnv::sandbox(home.path()).with_claude_config_dir(blank);
            assert_eq!(env.claude_dir().unwrap(), home.path().join(".claude"));
            assert_eq!(
                env.claude_projects_root().unwrap(),
                home.path().join(".claude").join("projects")
            );
            assert_eq!(env.history_cache_dir(), default.history_cache_dir());
        }
        for relative in ["rel", ".claude-work", "."] {
            let env = HistoryEnv::sandbox(home.path()).with_claude_config_dir(relative);
            assert!(
                matches!(
                    env.claude_dir(),
                    Err(AppError::InvalidClaudeConfigDir(ref path)) if path == Path::new(relative)
                ),
                "{relative}"
            );
            assert!(env.claude_projects_root().is_err(), "{relative}");
            assert!(
                env.claude_projects_dir_for(home.path()).is_err(),
                "{relative}"
            );
            // The destructive paths refuse instead of touching `<cwd>/rel`.
            assert!(matches!(
                crate::history::delete_session_by_uuid(
                    &env,
                    "12345678-1234-4234-9234-123456789abc"
                ),
                Err(AppError::InvalidClaudeConfigDir(_))
            ));
        }
        if cfg!(windows) {
            // Drive-relative (`\dir`) is not absolute on Windows either.
            let env = HistoryEnv::sandbox(home.path()).with_claude_config_dir(r"\claude");
            assert!(env.claude_dir().is_err());
        }
    }

    #[test]
    fn config_dir_hash_matches_upstream_string_hash() {
        use std::hash::{Hash, Hasher};
        let value = "/tmp/claude config";
        let mut upstream = std::collections::hash_map::DefaultHasher::new();
        value.to_string().hash(&mut upstream);
        let expected = format!("config-{:016x}", upstream.finish());
        let env = HistoryEnv {
            claude_config_dir: Some(PathBuf::from(value)),
            cache_root: Some(PathBuf::from("cache")),
            ..HistoryEnv::default()
        };
        assert_eq!(
            env.history_cache_dir().unwrap(),
            PathBuf::from("cache").join(expected).join("projects")
        );
    }

    #[test]
    fn workspace_scope_encodes_the_normalized_non_canonical_path() {
        let (dir, encoded) = if cfg!(windows) {
            (r"C:\Users\me\code\proj", "C--Users-me-code-proj")
        } else {
            ("/Users/me/code/proj", "-Users-me-code-proj")
        };
        let scope = WorkspaceScope::new(dir).unwrap();
        assert_eq!(scope.project_dir_name(), encoded);
        assert_eq!(scope.dir(), Path::new(dir));
        let trailing = format!("{dir}{}", std::path::MAIN_SEPARATOR);
        let env = HistoryEnv::default().with_workspace(&trailing);
        assert_eq!(env.workspace().unwrap(), Some(scope));
        assert_eq!(env.workspace_base_dir(), Some(PathBuf::from(dir)));
        let home = HistoryEnv {
            home_dir: Some(PathBuf::from("home")),
            ..HistoryEnv::default()
        };
        assert_eq!(
            home.claude_projects_dir_for(Path::new(&trailing)).unwrap(),
            home.claude_projects_root().unwrap().join(encoded)
        );
        assert_eq!(HistoryEnv::default().workspace().unwrap(), None);
        assert_eq!(HistoryEnv::default().workspace_base_dir(), None);
    }

    #[test]
    fn environment_values_use_the_launch_directory_before_the_workspace() {
        let (launch, workspace) = if cfg!(windows) {
            (r"C:\launch\dir\", r"C:\work")
        } else {
            ("/launch/dir/", "/work")
        };
        let env = HistoryEnv::default().with_workspace(workspace);
        assert_eq!(env.env_path_base, None);
        assert_eq!(env.launch_base_dir(), None);
        assert_eq!(env.env_value_base_dir(), Some(PathBuf::from(workspace)));
        let env = env.with_env_path_base(launch);
        let normalized = normalize_workspace_path(Path::new(launch));
        assert_eq!(env.launch_base_dir(), Some(normalized.clone()));
        assert_eq!(env.env_value_base_dir(), Some(normalized));
        // Only an absolute launch directory counts.
        let relative = env.clone().with_env_path_base("rel");
        assert_eq!(relative.launch_base_dir(), None);
        assert_eq!(
            relative.env_value_base_dir(),
            Some(PathBuf::from(workspace))
        );
        // The workspace scope is unaffected by the launch directory.
        assert_eq!(env.workspace_base_dir(), Some(PathBuf::from(workspace)));
    }

    #[test]
    fn workspace_scope_rejects_relative_paths() {
        // Claude Code encodes its absolute cwd; a relative workspace would be
        // encoded verbatim (`.` -> `-`) or resolved against the process cwd.
        let mut relative = vec!["", ".", "proj", "./proj", "proj/", "../x"];
        if cfg!(windows) {
            relative.extend([r".\proj", r"..\x", r"\proj", "C:proj", "C:"]);
        }
        for input in relative {
            let error = WorkspaceScope::new(input).unwrap_err();
            assert!(
                matches!(&error, AppError::InvalidWorkspace(path) if path == Path::new(input)),
                "{input:?}: {error}"
            );
            let env = HistoryEnv::default().with_workspace(input);
            assert!(env.workspace().is_err(), "{input:?}");
            assert_eq!(env.workspace_base_dir(), None, "{input:?}");
        }
        assert_eq!(
            WorkspaceScope::new("proj").unwrap_err().to_string(),
            "Workspace folder must be an absolute path: proj"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_workspace_scope_is_normalized_like_claude_codes_cwd() {
        for input in [
            r"C:\git\proj",
            r"C:\git\proj\",
            r"C:\git\proj\\",
            r"C:/git/proj/",
            r"c:\git\proj",
            r"C:\git\.\proj",
            r"C:\git\proj\.",
            r"C:\git\other\..\proj",
            r"C:\git\\proj",
            r"\\?\C:\git\proj\",
            r"\\?\c:\git\proj",
        ] {
            let scope = WorkspaceScope::new(input).unwrap();
            assert_eq!(scope.dir(), Path::new(r"C:\git\proj"), "{input}");
            assert_eq!(scope.project_dir_name(), "C--git-proj", "{input}");
            assert_eq!(
                workspace_project_dir_name(Path::new(input)),
                "C--git-proj",
                "{input}"
            );
        }
        // A drive root keeps its separator; `..` stays at the root.
        for input in [r"C:\", "C:/", r"C:\..", r"c:\.\"] {
            let scope = WorkspaceScope::new(input).unwrap();
            assert_eq!(scope.dir(), Path::new(r"C:\"), "{input}");
            assert_eq!(scope.project_dir_name(), "C--", "{input}");
        }
        for input in [
            r"\\server\share\dir\",
            r"\\?\UNC\server\share\dir",
            r"\\server\share\x\..\dir",
        ] {
            let scope = WorkspaceScope::new(input).unwrap();
            assert_eq!(scope.dir(), Path::new(r"\\server\share\dir"), "{input}");
            assert_eq!(scope.project_dir_name(), "--server-share-dir", "{input}");
        }
        assert_eq!(
            WorkspaceScope::new(r"\\server\share\").unwrap().dir(),
            Path::new(r"\\server\share")
        );
        // Other verbatim paths keep their prefix.
        assert_eq!(
            normalize_windows_path(Path::new(r"\\?\Volume{1234}\x")),
            PathBuf::from(r"\\?\Volume{1234}\x")
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_workspace_scope_is_normalized_like_claude_codes_cwd() {
        for input in [
            "/Users/me/code/proj",
            "/Users/me/code/proj/",
            "/Users/me/code/proj//",
            "/Users/me/./code/proj",
            "/Users/me/code/proj/.",
            "/Users/me/x/../code/proj",
            "//Users//me/code/proj",
        ] {
            let scope = WorkspaceScope::new(input).unwrap();
            assert_eq!(scope.dir(), Path::new("/Users/me/code/proj"), "{input}");
            assert_eq!(scope.project_dir_name(), "-Users-me-code-proj", "{input}");
        }
        for input in ["/", "/.", "/..", "//"] {
            let scope = WorkspaceScope::new(input).unwrap();
            assert_eq!(scope.dir(), Path::new("/"), "{input}");
            assert_eq!(scope.project_dir_name(), "-", "{input}");
        }
        // A backslash is an ordinary file-name character on Unix.
        assert_eq!(
            WorkspaceScope::new(r"/srv/odd\")
                .unwrap()
                .project_dir_name(),
            "-srv-odd-"
        );
    }

    #[test]
    fn relative_paths_stay_relative_when_normalized() {
        assert_eq!(
            normalize_workspace_path(Path::new("./a/./b/../c/")),
            PathBuf::from("a").join("c")
        );
        assert_eq!(
            normalize_workspace_path(Path::new("../../a")),
            PathBuf::from("..").join("..").join("a")
        );
        assert_eq!(normalize_workspace_path(Path::new(".")), PathBuf::new());
    }

    #[test]
    fn windows_workspace_names_ignore_verbatim_prefix_and_drive_case() {
        // W1: canonicalized (`\\?\C:\...`) and lower-case drive paths must
        // encode like the path Claude Code itself was started in.
        for input in [
            r"C:\Users\alice\road-trip",
            r"c:\Users\alice\road-trip",
            r"\\?\C:\Users\alice\road-trip",
            r"\\?\c:\Users\alice\road-trip",
        ] {
            assert_eq!(
                workspace_project_dir_name(Path::new(input)),
                "C--Users-alice-road-trip",
                "{input}"
            );
        }
        assert_eq!(
            normalize_windows_path(Path::new(r"\\?\UNC\server\share\x")),
            PathBuf::from(r"\\server\share\x")
        );
        assert_eq!(
            normalize_windows_path(Path::new("/home/user/c:/x")),
            PathBuf::from("/home/user/c:/x")
        );
    }
}
