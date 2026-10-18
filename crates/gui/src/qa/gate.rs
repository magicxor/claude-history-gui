//! The sandbox gate (DESIGN §15.4). WP0 → WP7.
//!
//! Runs in `main` for every `--features qa` build **and every debug build**,
//! before any data is read. It refuses (exit 3) unless all of these hold:
//!
//! 1. the resolved home directory (`home::home_dir()`) contains a
//!    `.qa-sandbox` file;
//! 2. that home is not the current user's OS profile directory, obtained from
//!    the OS and not from variables (Windows `SHGetKnownFolderPath(FOLDERID_Profile)`,
//!    Unix `getpwuid_r(getuid())`); when the OS cannot tell, the gate refuses
//!    (it fails closed);
//! 3. each of `CLAUDE_CONFIG_DIR`, `PI_CODING_AGENT_DIR`,
//!    `PI_CODING_AGENT_SESSION_DIR`, `PI_CONFIG_DIR`, `XDG_DATA_HOME`,
//!    `XDG_CONFIG_HOME`, `XDG_CACHE_HOME`, `XDG_STATE_HOME` is unset or
//!    (canonically) inside the home, and the default data locations used
//!    while they are unset (`<home>/.claude`, `.claude.json`, `.pi`, `.omp`,
//!    `.config`, `.cache`, `.local` and their data subfolders) canonically
//!    stay inside the home when they exist — a junction or symlink to real
//!    data is refused;
//! 4. `OMP_PROFILE` and `PI_PROFILE` are unset or plain names.
//!
//! Debug builds skip the gate only with `CHG_ALLOW_REAL_HOME=1` (the user's
//! own choice); `--features qa` builds never skip it. A sandbox is any copy
//! of a home with an (empty) `.qa-sandbox` file in it; the shared template
//! (`scratchpad/sandbox/home`) has none, so whoever copies it adds the file.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// The marker file a sandbox home must contain.
pub const SANDBOX_MARKER: &str = ".qa-sandbox";
/// The variable that lets a debug build use the real home.
pub const ALLOW_REAL_HOME_VAR: &str = "CHG_ALLOW_REAL_HOME";
/// Path variables that must stay inside the sandbox home.
pub const PATH_VARS: [&str; 8] = [
    "CLAUDE_CONFIG_DIR",
    "PI_CODING_AGENT_DIR",
    "PI_CODING_AGENT_SESSION_DIR",
    "PI_CONFIG_DIR",
    "XDG_DATA_HOME",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
];
/// Profile-name variables that must be plain names.
pub const NAME_VARS: [&str; 2] = ["OMP_PROFILE", "PI_PROFILE"];
/// Default data locations under the home (relative), checked when present:
/// each must canonically stay inside the home.
pub const DEFAULT_DATA_PATHS: [&str; 16] = [
    ".claude",
    ".claude/projects",
    ".claude.json",
    ".pi",
    ".pi/agent",
    ".omp",
    ".omp/agent",
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
    pub home: Option<PathBuf>,
    /// The OS profile directory of the current user (from the OS, not variables).
    pub os_profile: Option<PathBuf>,
    pub path_vars: Vec<(&'static str, Option<OsString>)>,
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

/// Run the gate for this process: `Ok` to continue, `Err(message)` to exit 3.
pub fn run() -> Result<(), String> {
    if !gate_applies() {
        return Ok(());
    }
    let allow_real_home =
        !cfg!(feature = "qa") && std::env::var(ALLOW_REAL_HOME_VAR).as_deref() == Ok("1");
    if allow_real_home {
        return Ok(());
    }
    check(&gather()).map_err(|reason| refusal_message(&reason))
}

/// Read the gate inputs from this process.
pub fn gather() -> GateInputs {
    GateInputs {
        home: home::home_dir(),
        os_profile: os_profile_dir(),
        path_vars: PATH_VARS
            .iter()
            .map(|name| (*name, std::env::var_os(name).filter(|v| !v.is_empty())))
            .collect(),
        name_vars: NAME_VARS
            .iter()
            .map(|name| (*name, std::env::var_os(name).filter(|v| !v.is_empty())))
            .collect(),
    }
}

/// The gate's decision for `inputs` (`Err` = the reason to refuse).
pub fn check(inputs: &GateInputs) -> Result<(), String> {
    let home = inputs
        .home
        .as_deref()
        .ok_or_else(|| "the home directory is unknown".to_string())?;
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
    Ok(())
}

/// Canonical form of `path`: the file system's when it exists, else a
/// lexically normalized absolute path.
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

    fn inputs(home: &Path) -> GateInputs {
        GateInputs {
            home: Some(home.to_path_buf()),
            os_profile: Some(PathBuf::from("/definitely/not/the/sandbox")),
            path_vars: Vec::new(),
            name_vars: Vec::new(),
        }
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
        gate.path_vars = vec![("XDG_DATA_HOME", Some(OsString::from("relative/dir")))];
        assert!(check(&gate).unwrap_err().contains("relative"));
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
}
