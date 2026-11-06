//! The sandbox gate (DESIGN §15.4). WP0 → WP7.
//!
//! Runs in `main` for every `--features qa` build **and every debug build**,
//! before any data is read, on the environment the app runs with (`main`
//! builds that `HistoryEnv` first, which reads no data, and hands the same
//! value to the app). It refuses (exit 3) unless all of these hold:
//!
//! 1. the home directory of that environment (`home::home_dir()`) contains a
//!    `.qa-sandbox` file;
//! 2. that home is not the current user's OS profile directory, obtained from
//!    the OS and not from variables (Windows `SHGetKnownFolderPath(FOLDERID_Profile)`,
//!    Unix `getpwuid_r(getuid())`); when the OS cannot tell, the gate refuses
//!    (it fails closed);
//! 3. each of `CLAUDE_CONFIG_DIR`, `PI_CODING_AGENT_DIR`,
//!    `PI_CODING_AGENT_SESSION_DIR`, `PI_CONFIG_DIR`, `CODEX_HOME`,
//!    `CODEX_SQLITE_HOME`, `XDG_DATA_HOME`, `XDG_CONFIG_HOME`,
//!    `XDG_CACHE_HOME`, `XDG_STATE_HOME` is unset or (canonically) inside the
//!    home — set but empty, it counts as unset only where the app reads it so
//!    ([`EMPTY_MEANS_UNSET`]); the app reads an empty `PI_CODING_AGENT_DIR`,
//!    `PI_CODING_AGENT_SESSION_DIR` or `PI_CONFIG_DIR` as a relative path, so
//!    it is refused — and the default data locations used while they are unset
//!    (`<home>/.claude`, `.claude.json`, `.pi`, `.omp`, `.codex`, `.config`,
//!    `.cache`, `.local` and their data subfolders) canonically
//!    stay inside the home when they exist — a junction or symlink to real
//!    data is refused;
//! 4. `OMP_PROFILE` and `PI_PROFILE` are unset or plain names;
//! 5. the data locations the app reads, derived from exactly that
//!    environment and from config files inside the sandbox, canonically stay
//!    inside the home: the Claude data and projects directories
//!    (`HistoryEnv::claude_dir`, `claude_projects_root`), the Codex home
//!    with its `sessions` and `archived_sessions` directories and its state
//!    database directory (the `sqlite_home` of the Codex home's
//!    `config.toml`, else `CODEX_SQLITE_HOME`, else the Codex home: core
//!    `sources::codex::state_db_dir`), the store and semantic cache
//!    directories (`cache_base_dir`, `semantic_cache_dir`), the `sessionDir`
//!    of Pi's `settings.json` in the agent directory (core
//!    `pi_loader::session_dir_settings`; a relative value resolves against
//!    the workspace, so it is checked once one is applied, rule 6), and the
//!    Pi and OMP session roots (`pi_loader::session_root`,
//!    `omp_loader::session_root`; OMP reads no settings file). A config file
//!    that is missing or does not parse adds nothing to check (the app then
//!    uses the locations checked above); a refusal names the file and the
//!    setting;
//! 6. every workspace the app applies passes [`check_workspace`]: rule 5
//!    again with that workspace (Pi also reads `<workspace>/.pi/settings.json`
//!    and resolves a relative `sessionDir` against the workspace). At startup
//!    (the GUI's startup workspace, the launch directory of the headless
//!    `agent` commands) a refusal exits 3 like the gate; at runtime (the
//!    workspace picker, PD3 rule 3 (c)) the change is refused with an error
//!    toast and the previous workspace stays;
//! 7. every file opened by path (`FILE`, `--render FILE`, File → Open JSONL
//!    file…) passes [`check_opened_file`]. The file itself is read as named,
//!    wherever it lies (opening it is the user's explicit choice; QA
//!    scenarios open copies inside the sandbox), but a Codex rollout decodes
//!    through the Codex home holding it, which may be another home than the
//!    environment's: that home (listed, its session index read), its
//!    `sessions` and `archived_sessions` directories and the state database
//!    directory decoding the rollout opens (that home's `config.toml`
//!    `sqlite_home`, else the home: core
//!    `sources::codex::view::state_db_dir_of`) must canonically stay inside
//!    the sandbox home. At startup a refusal exits 3 like the gate; File →
//!    Open JSONL file… shows an error toast and does not open that file.
//!
//! Debug builds skip the gate, rules 6 and 7 included, only with
//! `CHG_ALLOW_REAL_HOME=1` (the user's own choice, see [`enforced`]);
//! `--features qa` builds never skip it and release builds never run it. A
//! sandbox is any copy of a home with an (empty) `.qa-sandbox` file in it;
//! the shared template (`scratchpad/sandbox/home`) has none, so whoever
//! copies it adds the file.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use claude_history_core::HistoryEnv;
use claude_history_core::history::sources::codex::{self, discover};
use claude_history_core::history::{omp_loader, pi_loader};

/// The marker file a sandbox home must contain.
pub const SANDBOX_MARKER: &str = ".qa-sandbox";
/// The variable that lets a debug build use the real home.
pub const ALLOW_REAL_HOME_VAR: &str = "CHG_ALLOW_REAL_HOME";
/// Path variables that must stay inside the sandbox home.
pub const PATH_VARS: [&str; 10] = [
    "CLAUDE_CONFIG_DIR",
    "PI_CODING_AGENT_DIR",
    "PI_CODING_AGENT_SESSION_DIR",
    "PI_CONFIG_DIR",
    "CODEX_HOME",
    "CODEX_SQLITE_HOME",
    "XDG_DATA_HOME",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
];
/// The path variables whose empty value counts as unset (rule 3): the app
/// reads these so (core's `HistoryEnv` built from the process environment;
/// the `XDG_*` directories by the XDG spec). Any other path variable that is set but
/// empty is refused: the app reads an empty `PI_*` value as a relative path
/// (an empty `PI_CODING_AGENT_SESSION_DIR` is the launch directory).
pub const EMPTY_MEANS_UNSET: [&str; 7] = [
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "CODEX_SQLITE_HOME",
    "XDG_DATA_HOME",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
];
/// Profile-name variables that must be plain names.
pub const NAME_VARS: [&str; 2] = ["OMP_PROFILE", "PI_PROFILE"];
/// Default data locations under the home (relative), checked when present:
/// each must canonically stay inside the home.
pub const DEFAULT_DATA_PATHS: [&str; 19] = [
    ".claude",
    ".claude/projects",
    ".claude.json",
    ".pi",
    ".pi/agent",
    ".omp",
    ".omp/agent",
    ".codex",
    ".codex/sessions",
    ".codex/archived_sessions",
    ".config",
    ".config/claude-history",
    ".cache",
    ".cache/claude-history",
    ".cache/claude-history-gui",
    ".local",
    ".local/share",
    ".local/state",
    ".local/state/claude-history-gui",
];

/// Everything the gate looks at (gathered from the process, or built by tests).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GateInputs {
    /// The environment the app runs with (`main`'s, built from the process
    /// environment): its home is the one checked (rules 1–3), and rule 5
    /// checks the data locations it makes the app read.
    pub env: HistoryEnv,
    /// The OS profile directory of the current user (from the OS, not variables).
    pub os_profile: Option<PathBuf>,
    /// The values of [`PATH_VARS`], as set (an empty value too).
    pub path_vars: Vec<(&'static str, Option<OsString>)>,
    /// The values of [`NAME_VARS`] (an empty value counts as unset, as the
    /// app reads it).
    pub name_vars: Vec<(&'static str, Option<OsString>)>,
}

/// The refusal text printed to stderr.
pub fn refusal_message(reason: &str) -> String {
    format!(
        "Refusing to run: {reason}. Create a sandbox with tools/qa/new-sandbox.ps1 or set {ALLOW_REAL_HOME_VAR}=1."
    )
}

/// Whether the gate runs in this build.
pub fn gate_applies() -> bool {
    cfg!(debug_assertions) || cfg!(feature = "qa")
}

/// Whether this process enforces the gate: it applies to the build, and a
/// debug build was not started with `CHG_ALLOW_REAL_HOME=1`. `main` passes it
/// on for rules 6 and 7 (`workspace::startup::ProcessInfo::sandbox_gate`).
pub fn enforced() -> bool {
    if !gate_applies() {
        return false;
    }
    let allow_real_home =
        !cfg!(feature = "qa") && std::env::var(ALLOW_REAL_HOME_VAR).as_deref() == Ok("1");
    !allow_real_home
}

/// Run the gate for this process on `env`, the environment the app will run
/// with: `Ok` to continue, `Err(message)` to exit 3.
pub fn run(env: &HistoryEnv) -> Result<(), String> {
    if !enforced() {
        return Ok(());
    }
    check(&gather(env)).map_err(|reason| refusal_message(&reason))
}

/// Read the gate inputs from this process, for the app's environment `env`.
pub fn gather(env: &HistoryEnv) -> GateInputs {
    GateInputs {
        env: env.clone(),
        os_profile: os_profile_dir(),
        // Empty values too: rule 3 decides what an empty value means.
        path_vars: PATH_VARS
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect(),
        name_vars: NAME_VARS
            .iter()
            .map(|name| (*name, std::env::var_os(name).filter(|v| !v.is_empty())))
            .collect(),
    }
}

/// The gate's decision for `inputs` (`Err` = the reason to refuse).
pub fn check(inputs: &GateInputs) -> Result<(), String> {
    let home = sandbox_home(&inputs.env)?;
    if !home.join(SANDBOX_MARKER).is_file() {
        return Err(format!(
            "the home directory {} is not a QA sandbox (no {SANDBOX_MARKER} file)",
            home.display()
        ));
    }
    let home_canonical = canonical(home);
    let Some(profile) = &inputs.os_profile else {
        return Err(
            "the OS did not report your user profile directory, so the home cannot be checked"
                .to_string(),
        );
    };
    if same_dir(&home_canonical, &canonical(profile)) {
        return Err(format!(
            "the home directory {} is your real user profile",
            home.display()
        ));
    }
    for rel in DEFAULT_DATA_PATHS {
        let path = home.join(rel);
        if std::fs::symlink_metadata(&path).is_err() {
            continue;
        }
        let resolved = canonical(&path);
        if !is_inside(&resolved, &home_canonical) {
            return Err(format!(
                "{} points outside the sandbox home (to {})",
                path.display(),
                resolved.display()
            ));
        }
    }
    for (name, value) in &inputs.path_vars {
        let Some(value) = value else { continue };
        if value.is_empty() {
            if EMPTY_MEANS_UNSET.contains(name) {
                continue;
            }
            return Err(format!(
                "{name} is set but empty, which the app reads as a relative path"
            ));
        }
        let path = PathBuf::from(value);
        if !path.is_absolute() {
            return Err(format!("{name} is a relative path ({})", path.display()));
        }
        if !is_inside(&canonical(&path), &home_canonical) {
            return Err(format!(
                "{name} ({}) is outside the sandbox home",
                path.display()
            ));
        }
    }
    for (name, value) in &inputs.name_vars {
        let Some(value) = value else { continue };
        let text = value.to_string_lossy();
        let plain =
            !text.is_empty() && !text.contains(['/', '\\', ':']) && text != "." && text != "..";
        if !plain {
            return Err(format!("{name} is not a plain profile name ({text})"));
        }
    }
    check_inside(&data_locations(&inputs.env), &home_canonical)
}

/// Rule 6: the data locations `env` makes the app read (rule 5) with the
/// workspace `workspace` applied canonically stay inside `env`'s home. `Err`
/// is the reason: a startup caller words it with [`refusal_message`] and
/// exits 3, a runtime caller refuses the change. Callers check only when the
/// process [`enforced`] the gate.
pub fn check_workspace(env: &HistoryEnv, workspace: Option<&Path>) -> Result<(), String> {
    let mut env = env.clone();
    env.workspace_dir = workspace.map(Path::to_path_buf);
    let home = sandbox_home(&env)?;
    check_inside(&data_locations(&env), &canonical(home))
}

/// Rule 7: what opening `file` by path in `env` reads besides the file
/// canonically stays inside `env`'s home: for a Codex rollout, the Codex
/// home holding it (listed and its session index read) and the state
/// database directory decoding it opens, in `env` (a
/// single-file window) and without it (`--render`, the history service's
/// loose files). Any other file adds nothing. `Err` is the reason: a startup
/// caller words it with [`refusal_message`] and exits 3, a runtime caller
/// does not open the file. Callers check only when the process [`enforced`]
/// the gate. The Codex home is checked by path alone before any of its files
/// (config.toml) is read.
pub fn check_opened_file(env: &HistoryEnv, file: &Path) -> Result<(), String> {
    // Path-only checks first: nothing in a Codex home outside the sandbox
    // (its config.toml included) is read before it is refused.
    let Some(codex_home) = discover::home_of(file) else {
        return Ok(());
    };
    if discover::rollout_of(file, false).is_none() {
        return Ok(());
    }
    let home = canonical(sandbox_home(env)?);
    let of = format!(" of {}", file.display());
    check_inside(&codex_locations(codex_home, &[], &of), &home)?;
    // The home is inside: its config.toml may be read to find the state
    // database directories.
    let mut db_dirs: Vec<PathBuf> = Vec::new();
    for in_env in [Some(env), None] {
        if let Some(dir) = codex::view::state_db_dir_of(in_env, file)
            && !db_dirs.contains(&dir)
        {
            db_dirs.push(dir);
        }
    }
    check_inside(&codex_locations(codex_home, &db_dirs, &of), &home)
}

/// The home of `env` (`Err` when it is unknown).
fn sandbox_home(env: &HistoryEnv) -> Result<&Path, String> {
    env.home_dir
        .as_deref()
        .ok_or_else(|| "the home directory is unknown".to_string())
}

/// Rule 5: every data location `env` makes the app read, named after what
/// chose it (see the module docs). A location `env` cannot resolve (the app
/// then reads nothing there) adds nothing.
fn data_locations(env: &HistoryEnv) -> Vec<(String, PathBuf)> {
    let mut paths = Vec::new();
    if let Ok(dir) = env.claude_dir() {
        paths.push(("the Claude data directory".to_string(), dir));
    }
    if let Ok(dir) = env.claude_projects_root() {
        paths.push(("the Claude projects directory".to_string(), dir));
    }
    if let Some(home) = env.codex_home() {
        let db_dir = codex::state_db_dir(&home, env.codex_sqlite_home().as_deref());
        paths.extend(codex_locations(&home, &[db_dir], ""));
    }
    if let Some(dir) = env.cache_base_dir() {
        paths.push(("the store directory".to_string(), dir));
    }
    if let Some(dir) = env.semantic_cache_dir() {
        paths.push(("the semantic cache directory".to_string(), dir));
    }
    paths.extend(pi_paths(env));
    if let Ok(root) = omp_loader::session_root(env) {
        paths.push(("the OMP session directory".to_string(), root.path));
    }
    paths
}

/// The Codex home `home`, the directories a scan of it lists and its state
/// database directories `db_dirs` (as `codex::state_db_dir` resolved them),
/// each named after what chose it; `of` ends every name but the setting's
/// (` of <file>` for a rollout opened by path).
fn codex_locations(home: &Path, db_dirs: &[PathBuf], of: &str) -> Vec<(String, PathBuf)> {
    let mut paths = vec![
        (format!("the Codex home{of}"), home.to_path_buf()),
        (
            format!("the Codex sessions directory{of}"),
            home.join(discover::SESSIONS),
        ),
        (
            format!("the archived Codex sessions directory{of}"),
            home.join(discover::ARCHIVED_SESSIONS),
        ),
    ];
    for dir in db_dirs.iter().filter(|dir| dir.as_path() != home) {
        // `state_db_dir`: the home's own setting wins, else CODEX_SQLITE_HOME.
        let what = if codex::configured_sqlite_home(home).is_some() {
            format!("sqlite_home in {}", home.join("config.toml").display())
        } else {
            "CODEX_SQLITE_HOME".to_string()
        };
        paths.push((what, dir.clone()));
    }
    paths
}

/// The `sessionDir` of every Pi settings file of `env`, and the Pi session
/// root `env` reads.
fn pi_paths(env: &HistoryEnv) -> Vec<(String, PathBuf)> {
    let mut paths: Vec<(String, PathBuf)> = pi_loader::session_dir_settings(env)
        .into_iter()
        .map(|(file, dir)| (format!("sessionDir in {}", file.display()), dir))
        .collect();
    if let Ok(root) = pi_loader::session_root(env) {
        paths.push(("the Pi session directory".to_string(), root.path));
    }
    paths
}

/// `Err` naming the first of `paths` that is not canonically inside `home`
/// (canonical already).
fn check_inside(paths: &[(String, PathBuf)], home: &Path) -> Result<(), String> {
    for (what, path) in paths {
        if !is_inside(&canonical(path), home) {
            return Err(format!(
                "{what} ({}) is outside the sandbox home",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Canonical form of `path`: the file system's when it exists, else the
/// lexically normalized path with its longest existing ancestor canonicalized,
/// so a missing path still resolves the links and Windows 8.3 short names
/// (`C:\Users\RUNNER~1`, macOS `/var` → `/private/var`) above it like the home does.
fn canonical(path: &Path) -> PathBuf {
    if let Ok(path) = std::fs::canonicalize(path) {
        return strip_verbatim(path);
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    for ancestor in out.ancestors().skip(1) {
        if let (Ok(base), Ok(rest)) = (std::fs::canonicalize(ancestor), out.strip_prefix(ancestor))
        {
            return strip_verbatim(base).join(rest);
        }
    }
    out
}

/// `\\?\C:\x` → `C:\x` (canonicalize adds the verbatim prefix on Windows).
fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path
}

fn key(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text.trim_end_matches('/').to_string();
    if cfg!(any(windows, target_os = "macos")) {
        text.to_lowercase()
    } else {
        text
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    key(a) == key(b)
}

fn is_inside(path: &Path, dir: &Path) -> bool {
    let path = key(path);
    let dir = key(dir);
    path == dir || path.starts_with(&format!("{dir}/"))
}

/// The current user's profile directory, from the OS.
#[cfg(windows)]
pub fn os_profile_dir() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt as _;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_Profile, SHGetKnownFolderPath};

    let mut raw: windows_sys::core::PWSTR = std::ptr::null_mut();
    // SAFETY: FOLDERID_Profile is a valid GUID; on success `raw` points to a
    // NUL-terminated wide string that we free with CoTaskMemFree (also on failure,
    // as the API requires).
    unsafe {
        let hr = SHGetKnownFolderPath(&FOLDERID_Profile, 0, std::ptr::null_mut(), &mut raw);
        let result = if hr >= 0 && !raw.is_null() {
            let mut len = 0usize;
            while *raw.add(len) != 0 {
                len += 1;
            }
            let slice = std::slice::from_raw_parts(raw, len);
            Some(PathBuf::from(OsString::from_wide(slice)))
        } else {
            None
        };
        CoTaskMemFree(raw as *const core::ffi::c_void);
        result
    }
}

/// The current user's profile directory, from the OS.
#[cfg(unix)]
pub fn os_profile_dir() -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt as _;

    let mut buffer = vec![0 as libc::c_char; 16 * 1024];
    // SAFETY: getpwuid_r writes into `pwd` and `buffer` (sized above) and
    // sets `result` to `&mut pwd` on success; `pw_dir` then points into `buffer`.
    unsafe {
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let status = libc::getpwuid_r(
            libc::getuid(),
            &mut pwd,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut result,
        );
        if status != 0 || result.is_null() || pwd.pw_dir.is_null() {
            return None;
        }
        let dir = CStr::from_ptr(pwd.pw_dir);
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes())))
    }
}

#[cfg(not(any(windows, unix)))]
pub fn os_profile_dir() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(SANDBOX_MARKER), "").unwrap();
        dir
    }

    /// The inputs of a process whose environment has `home` as its home and
    /// sets no variable.
    fn inputs(home: &Path) -> GateInputs {
        GateInputs {
            env: HistoryEnv::sandbox(home),
            os_profile: Some(PathBuf::from("/definitely/not/the/sandbox")),
            path_vars: Vec::new(),
            name_vars: Vec::new(),
        }
    }

    /// `gate` with the path variable `name` set to `value` as a process would
    /// have it: in the variables the gate reads (rule 3) and in the
    /// environment the app builds from them (core's `HistoryEnv` from the
    /// process environment, rule 5).
    fn with_var(mut gate: GateInputs, name: &'static str, value: impl Into<PathBuf>) -> GateInputs {
        let value: PathBuf = value.into();
        gate.path_vars
            .push((name, Some(value.clone().into_os_string())));
        if value.as_os_str().is_empty() && EMPTY_MEANS_UNSET.contains(&name) {
            // The app's environment leaves these unset.
            return gate;
        }
        let env = &mut gate.env;
        let field = match name {
            "CLAUDE_CONFIG_DIR" => &mut env.claude_config_dir,
            "PI_CODING_AGENT_DIR" => &mut env.pi_coding_agent_dir,
            "PI_CODING_AGENT_SESSION_DIR" => &mut env.pi_coding_agent_session_dir,
            "PI_CONFIG_DIR" => &mut env.pi_config_dir,
            "CODEX_HOME" => &mut env.codex_home,
            "CODEX_SQLITE_HOME" => &mut env.codex_sqlite_home,
            "XDG_DATA_HOME" => &mut env.xdg_data_home,
            // Read by the gate only.
            _ => return gate,
        };
        *field = Some(value);
        gate
    }

    #[test]
    fn a_marked_sandbox_passes() {
        let home = sandbox();
        assert_eq!(check(&inputs(home.path())), Ok(()));
    }

    #[test]
    fn a_home_without_marker_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let error = check(&inputs(home.path())).unwrap_err();
        assert!(error.contains(SANDBOX_MARKER), "{error}");
    }

    #[test]
    fn the_os_profile_is_refused_even_with_a_marker() {
        let home = sandbox();
        let mut gate = inputs(home.path());
        gate.os_profile = Some(home.path().to_path_buf());
        assert!(check(&gate).unwrap_err().contains("real user profile"));
    }

    #[test]
    fn path_variables_must_stay_inside_the_home() {
        let home = sandbox();
        let mut gate = inputs(home.path());
        gate.path_vars = vec![(
            "CLAUDE_CONFIG_DIR",
            Some(home.path().join(".claude").into_os_string()),
        )];
        assert_eq!(check(&gate), Ok(()));
        let outside = tempfile::tempdir().unwrap();
        gate.path_vars = vec![(
            "CLAUDE_CONFIG_DIR",
            Some(outside.path().join(".claude").into_os_string()),
        )];
        assert!(check(&gate).unwrap_err().contains("CLAUDE_CONFIG_DIR"));
        // The Codex home and its state database directory are checked too.
        for name in ["CODEX_HOME", "CODEX_SQLITE_HOME"] {
            assert!(PATH_VARS.contains(&name), "{name} is gathered");
            gate.path_vars = vec![(name, Some(outside.path().join(".codex").into_os_string()))];
            assert!(check(&gate).unwrap_err().contains(name));
        }
        gate.path_vars = vec![("XDG_DATA_HOME", Some(OsString::from("relative/dir")))];
        assert!(check(&gate).unwrap_err().contains("relative"));
    }

    /// A missing directory under a home spelled through a link (as `RUNNER~1`
    /// or macOS `/var` spell the temp folder) is still inside the home.
    #[test]
    fn a_missing_path_variable_under_a_linked_home_is_inside() {
        let home = sandbox();
        let links = tempfile::tempdir().unwrap();
        let alias = links.path().join("home");
        if !make_dir_link(home.path(), &alias) {
            eprintln!("skipped: cannot create a directory link here");
            return;
        }
        let mut gate = inputs(&alias);
        gate.path_vars = vec![(
            "CLAUDE_CONFIG_DIR",
            Some(alias.join(".claude").into_os_string()),
        )];
        assert_eq!(check(&gate), Ok(()));
    }

    #[test]
    fn an_unknown_os_profile_is_refused() {
        let home = sandbox();
        let mut gate = inputs(home.path());
        gate.os_profile = None;
        assert!(check(&gate).unwrap_err().contains("user profile"));
    }

    #[test]
    fn default_data_dirs_must_stay_inside_the_home() {
        let home = sandbox();
        std::fs::create_dir_all(home.path().join(".claude").join("projects")).unwrap();
        assert_eq!(check(&inputs(home.path())), Ok(()));
        // A link from <home>/.config to a directory outside the home.
        let outside = tempfile::tempdir().unwrap();
        let link = home.path().join(".config");
        if make_dir_link(outside.path(), &link) {
            let error = check(&inputs(home.path())).unwrap_err();
            assert!(error.contains("outside the sandbox home"), "{error}");
        } else {
            eprintln!("skipped the link case: cannot create a directory link here");
        }
    }

    /// A directory symlink (Windows: needs developer mode or the privilege;
    /// the case is skipped without it).
    fn make_dir_link(target: &Path, link: &Path) -> bool {
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(target, link).is_ok()
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(not(any(windows, unix)))]
        {
            let _ = (target, link);
            false
        }
    }

    #[test]
    fn profile_names_must_be_plain() {
        let home = sandbox();
        let mut gate = inputs(home.path());
        gate.name_vars = vec![("OMP_PROFILE", Some(OsString::from("work")))];
        assert_eq!(check(&gate), Ok(()));
        gate.name_vars = vec![("PI_PROFILE", Some(OsString::from("../x")))];
        assert!(check(&gate).is_err());
    }

    #[test]
    fn the_os_profile_lookup_works() {
        assert!(os_profile_dir().is_some());
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A TOML `sqlite_home` line (a literal string: Windows paths keep their
    /// backslashes).
    fn sqlite_home(path: &Path) -> String {
        format!("sqlite_home = '{}'\n", path.display())
    }

    /// A Pi `settings.json` naming `dir` as the session directory.
    fn session_dir(dir: &Path) -> String {
        serde_json::json!({ "sessionDir": dir }).to_string()
    }

    /// Rule 5: the state database directory named by the Codex home's
    /// `config.toml` (absolute, or relative to the Codex home) stays inside.
    #[test]
    fn the_codex_config_sqlite_home_must_stay_inside_the_home() {
        let home = sandbox();
        let outside = tempfile::tempdir().unwrap();
        let config = home.path().join(".codex").join("config.toml");
        write(&config, "sqlite_home = \"state\"\n");
        assert_eq!(check(&inputs(home.path())), Ok(()));
        write(&config, &sqlite_home(&home.path().join("db")));
        assert_eq!(check(&inputs(home.path())), Ok(()));

        write(&config, &sqlite_home(outside.path()));
        let error = check(&inputs(home.path())).unwrap_err();
        assert!(
            error.contains(&format!("sqlite_home in {}", config.display())),
            "{error}"
        );
        assert!(error.contains("outside the sandbox home"), "{error}");

        // A relative value that climbs out of the home.
        write(&config, "sqlite_home = \"../../elsewhere\"\n");
        assert!(
            check(&inputs(home.path()))
                .unwrap_err()
                .contains("sqlite_home")
        );

        // It wins over CODEX_SQLITE_HOME, which is inside.
        let gate = with_var(
            inputs(home.path()),
            "CODEX_SQLITE_HOME",
            home.path().join("state"),
        );
        assert!(check(&gate).unwrap_err().contains("sqlite_home"));

        // A config file that does not parse, or names no string, adds nothing.
        for text in ["sqlite_home = [", "sqlite_home = 3\n", "model = \"x\"\n"] {
            write(&config, text);
            assert_eq!(check(&inputs(home.path())), Ok(()), "{text}");
        }
    }

    /// Rule 5 for the effective Codex home: with `CODEX_HOME` set, its own
    /// `config.toml` counts, the default home's does not.
    #[test]
    fn the_sqlite_home_of_codex_home_is_checked() {
        let home = sandbox();
        let outside = tempfile::tempdir().unwrap();
        let codex_home = home.path().join("codex-home");
        let gate = with_var(inputs(home.path()), "CODEX_HOME", codex_home.clone());
        write(
            &home.path().join(".codex").join("config.toml"),
            &sqlite_home(outside.path()),
        );
        assert_eq!(check(&gate), Ok(()), "not the effective Codex home");
        let config = codex_home.join("config.toml");
        write(&config, &sqlite_home(outside.path()));
        let error = check(&gate).unwrap_err();
        assert!(
            error.contains(&format!("sqlite_home in {}", config.display())),
            "{error}"
        );
        write(&config, "sqlite_home = \"db\"\n");
        assert_eq!(check(&gate), Ok(()));
    }

    /// Rule 5 for Pi: the `sessionDir` of the agent directory's
    /// `settings.json` (`~` expands to the home); a relative value waits for
    /// a workspace (rule 6).
    #[test]
    fn the_pi_settings_session_dir_must_stay_inside_the_home() {
        let home = sandbox();
        let outside = tempfile::tempdir().unwrap();
        let settings = home.path().join(".pi").join("agent").join("settings.json");
        write(&settings, r#"{"sessionDir":"~/pi-sessions"}"#);
        assert_eq!(check(&inputs(home.path())), Ok(()));
        write(&settings, r#"{"sessionDir":"relative-sessions"}"#);
        assert_eq!(check(&inputs(home.path())), Ok(()), "no workspace yet");

        write(&settings, &session_dir(outside.path()));
        let error = check(&inputs(home.path())).unwrap_err();
        assert!(
            error.contains(&format!("sessionDir in {}", settings.display())),
            "{error}"
        );

        // PI_CODING_AGENT_DIR moves the settings file.
        let agent = home.path().join("agent");
        let gate = with_var(inputs(home.path()), "PI_CODING_AGENT_DIR", agent.clone());
        write(&agent.join("settings.json"), r#"{"sessionDir":"~/s"}"#);
        // The default agent directory's file (naming a directory outside)
        // no longer counts: the app does not read it then.
        assert_eq!(check(&gate), Ok(()));
        write(&agent.join("settings.json"), &session_dir(outside.path()));
        assert!(check(&gate).unwrap_err().contains("sessionDir in"));

        // A settings file that is not JSON adds nothing.
        write(&settings, "{ not json");
        assert_eq!(check(&inputs(home.path())), Ok(()));
    }

    /// Rule 5: the Pi and OMP session roots themselves (a link below a
    /// checked default location, an OMP profile directory).
    #[test]
    fn the_session_roots_must_stay_inside_the_home() {
        let home = sandbox();
        let outside = tempfile::tempdir().unwrap();
        let agent = home.path().join(".pi").join("agent");
        std::fs::create_dir_all(&agent).unwrap();
        if make_dir_link(outside.path(), &agent.join("sessions")) {
            let error = check(&inputs(home.path())).unwrap_err();
            assert!(error.contains("the Pi session directory"), "{error}");
        } else {
            eprintln!("skipped the Pi link case: cannot create a directory link here");
        }

        let home = sandbox();
        let profiles = home.path().join(".omp").join("profiles");
        std::fs::create_dir_all(&profiles).unwrap();
        let mut gate = inputs(home.path());
        gate.name_vars = vec![("OMP_PROFILE", Some(OsString::from("work")))];
        gate.env.omp_profile = Some(OsString::from("work"));
        assert_eq!(check(&gate), Ok(()));
        if make_dir_link(outside.path(), &profiles.join("work")) {
            let error = check(&gate).unwrap_err();
            assert!(error.contains("the OMP session directory"), "{error}");
        } else {
            eprintln!("skipped the OMP link case: cannot create a directory link here");
        }
    }

    /// Rule 6: a workspace's `.pi/settings.json` and a relative global
    /// `sessionDir` (resolved against the workspace) stay inside the home.
    #[test]
    fn a_workspace_whose_pi_settings_lead_outside_is_refused() {
        let home = sandbox();
        let outside = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(home.path());
        let workspace = home.path().join("project");
        std::fs::create_dir_all(&workspace).unwrap();
        assert_eq!(check_workspace(&env, None), Ok(()));
        assert_eq!(check_workspace(&env, Some(&workspace)), Ok(()));

        let settings = workspace.join(".pi").join("settings.json");
        write(&settings, r#"{"sessionDir":"sessions"}"#);
        assert_eq!(check_workspace(&env, Some(&workspace)), Ok(()));
        write(&settings, &session_dir(outside.path()));
        let error = check_workspace(&env, Some(&workspace)).unwrap_err();
        assert!(
            error.contains(&format!("sessionDir in {}", settings.display())),
            "{error}"
        );
        assert!(error.contains("outside the sandbox home"), "{error}");
        write(&settings, r#"{"sessionDir":"../../elsewhere"}"#);
        assert!(check_workspace(&env, Some(&workspace)).is_err());
        write(&settings, "not json");
        assert_eq!(check_workspace(&env, Some(&workspace)), Ok(()));
        assert_eq!(check_workspace(&env, None), Ok(()));

        // A relative global value resolves against the workspace: fine for a
        // workspace inside the home, refused for one outside it.
        write(
            &home.path().join(".pi").join("agent").join("settings.json"),
            r#"{"sessionDir":"pi-sessions"}"#,
        );
        assert_eq!(check_workspace(&env, Some(&workspace)), Ok(()));
        let error = check_workspace(&env, Some(outside.path())).unwrap_err();
        assert!(error.contains("sessionDir in "), "{error}");
    }

    /// Rule 3: an empty path variable counts as unset only where the app
    /// reads it so; an empty `PI_*` value, a relative path to the app, is
    /// refused (fails closed), even when launched inside the home.
    #[test]
    fn an_empty_path_variable_counts_as_unset_only_where_the_app_reads_it_so() {
        let home = sandbox();
        assert!(
            EMPTY_MEANS_UNSET
                .iter()
                .all(|name| PATH_VARS.contains(name))
        );
        for name in PATH_VARS {
            let mut gate = with_var(inputs(home.path()), name, "");
            gate.env.env_path_base = Some(home.path().to_path_buf());
            let result = check(&gate);
            if EMPTY_MEANS_UNSET.contains(&name) {
                assert_eq!(result, Ok(()), "{name}");
            } else {
                let error = result.unwrap_err();
                assert!(
                    error.starts_with(&format!("{name} is set but empty")),
                    "{error}"
                );
            }
        }
    }

    /// Rule 5 checks the environment the app runs with (`main` hands the
    /// gate the same value): an empty `PI_CODING_AGENT_SESSION_DIR` or
    /// `PI_CODING_AGENT_DIR` makes the launch directory (the env's
    /// `env_path_base`) Pi's session root or agent directory, so a launch
    /// from outside the sandbox is refused even where rule 3 saw no value;
    /// the same values launched inside the home stay inside.
    #[test]
    fn rule_5_checks_the_environment_the_app_reads() {
        let home = sandbox();
        let outside = tempfile::tempdir().unwrap();
        for name in ["PI_CODING_AGENT_SESSION_DIR", "PI_CODING_AGENT_DIR"] {
            let mut gate = inputs(home.path());
            match name {
                "PI_CODING_AGENT_SESSION_DIR" => {
                    gate.env.pi_coding_agent_session_dir = Some(PathBuf::new())
                }
                _ => gate.env.pi_coding_agent_dir = Some(PathBuf::new()),
            }
            gate.env.env_path_base = Some(outside.path().to_path_buf());
            let error = check(&gate).unwrap_err();
            assert!(
                error.starts_with("the Pi session directory ("),
                "{name}: {error}"
            );
            assert!(error.ends_with("is outside the sandbox home"), "{error}");
            gate.env.env_path_base = Some(home.path().to_path_buf());
            assert_eq!(check(&gate), Ok(()), "{name}");
        }
        // A relative CODEX_HOME resolves against the launch directory too.
        let mut gate = inputs(home.path());
        gate.env.codex_home = Some(PathBuf::from("codex"));
        gate.env.env_path_base = Some(outside.path().to_path_buf());
        let error = check(&gate).unwrap_err();
        assert!(error.starts_with("the Codex home ("), "{error}");
    }

    /// Rule 5 checks the Claude projects folder, the Codex session folders
    /// and the store where the variables move them, not only at their
    /// default locations: a link there to a folder outside is refused.
    #[test]
    fn moved_data_folders_must_stay_inside_the_home() {
        let home = sandbox();
        let outside = tempfile::tempdir().unwrap();
        let claude = home.path().join("claude-moved");
        let codex = home.path().join("codex-moved");
        let gate = with_var(
            with_var(inputs(home.path()), "CLAUDE_CONFIG_DIR", claude.clone()),
            "CODEX_HOME",
            codex.clone(),
        );
        let store = gate.env.cache_base_dir().unwrap();
        assert!(store.starts_with(home.path().join(".cache").join("claude-history-gui")));
        assert_ne!(store, home.path().join(".cache").join("claude-history-gui"));
        assert_eq!(check(&gate), Ok(()));
        for (link, what) in [
            (claude.join("projects"), "the Claude projects directory"),
            (codex.join("sessions"), "the Codex sessions directory"),
            (
                codex.join("archived_sessions"),
                "the archived Codex sessions directory",
            ),
            (store, "the store directory"),
        ] {
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            if !make_dir_link(outside.path(), &link) {
                eprintln!("skipped the link cases: cannot create a directory link here");
                return;
            }
            let error = check(&gate).unwrap_err();
            assert!(error.starts_with(&format!("{what} (")), "{error}");
            remove_dir_link(&link);
            assert_eq!(check(&gate), Ok(()), "{what}");
        }
    }

    /// Remove the directory link `link` (not its target).
    fn remove_dir_link(link: &Path) {
        #[cfg(windows)]
        std::fs::remove_dir(link).unwrap();
        #[cfg(not(windows))]
        std::fs::remove_file(link).unwrap();
    }

    /// Rule 7: a Codex rollout opened by path decodes through the Codex home
    /// holding it, whose folders and state database directory must stay
    /// inside; any other file, wherever it lies, adds nothing.
    #[test]
    fn files_opened_by_path_read_nothing_outside_the_home() {
        let home = sandbox();
        let outside = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(home.path());
        let rollout_in = |codex_home: &Path| {
            codex_home
                .join("sessions")
                .join("2026")
                .join("09")
                .join("03")
                .join("rollout-2026-09-03T10-00-00-01a00000-0000-7000-8000-0000000000aa.jsonl")
        };
        for file in [
            outside.path().join("session.jsonl"),
            home.path().join("session.jsonl"),
            outside.path().join("sessions").join("notes.jsonl"),
        ] {
            assert_eq!(check_opened_file(&env, &file), Ok(()), "{file:?}");
        }
        // A rollout of the env's own Codex home, and of a second one inside.
        let second = home.path().join("second-codex");
        for codex_home in [home.path().join(".codex"), second.clone()] {
            assert_eq!(check_opened_file(&env, &rollout_in(&codex_home)), Ok(()));
        }

        // The second home's config.toml names a state database directory
        // outside (absolute, or relative and climbing out).
        let config = second.join("config.toml");
        write(&config, &sqlite_home(outside.path()));
        let error = check_opened_file(&env, &rollout_in(&second)).unwrap_err();
        assert!(
            error.starts_with(&format!("sqlite_home in {} (", config.display())),
            "{error}"
        );
        assert!(error.ends_with("is outside the sandbox home"), "{error}");
        write(&config, "sqlite_home = \"../../elsewhere\"\n");
        assert!(check_opened_file(&env, &rollout_in(&second)).is_err());
        write(&config, "sqlite_home = \"db\"\n");
        assert_eq!(check_opened_file(&env, &rollout_in(&second)), Ok(()));

        // A rollout of a Codex home outside: its folders and database.
        let file = rollout_in(&outside.path().join("codex"));
        let error = check_opened_file(&env, &file).unwrap_err();
        assert!(
            error.starts_with(&format!("the Codex home of {} (", file.display())),
            "{error}"
        );

        // Nothing in a Codex home outside is read before it is refused: its
        // config.toml, here a directory, would fail to read.
        let unreadable = outside.path().join("unreadable-codex");
        std::fs::create_dir_all(unreadable.join("config.toml")).unwrap();
        let file = rollout_in(&unreadable);
        let error = check_opened_file(&env, &file).unwrap_err();
        assert!(
            error.starts_with(&format!("the Codex home of {} (", file.display())),
            "{error}"
        );

        // A rollout of the env's own home decodes with its CODEX_SQLITE_HOME.
        let mut moved = env.clone();
        moved.codex_sqlite_home = Some(outside.path().to_path_buf());
        let error =
            check_opened_file(&moved, &rollout_in(&home.path().join(".codex"))).unwrap_err();
        assert!(error.starts_with("CODEX_SQLITE_HOME ("), "{error}");
    }
}
