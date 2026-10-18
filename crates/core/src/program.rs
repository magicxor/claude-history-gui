//! Resolution of external programs (`claude`, `pi`, `omp`) for resume and
//! fork launches (Windows issue W9).
//!
//! Upstream ran `Command::new("claude")`. On Windows that only finds
//! `claude.exe`, while an npm global install provides `claude.cmd`; and a GUI
//! started from the Start menu or Finder may not have the user's shell
//! `PATH`. Core therefore resolves programs explicitly, from an optional
//! override path (`gui.toml`: `claude_path`, `pi_path`, `omp_path`) or by
//! searching the `PATH` captured in [`crate::env::HistoryEnv`] with
//! `PATHEXT` semantics on Windows, and reports batch scripts so the caller
//! can run them through `cmd.exe` ([`ResolvedProgram::command`] spawns them
//! with a fully escaped `cmd /S /C` command line).

use crate::env::HistoryEnv;
use crate::error::{AppError, Result};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Windows' default `PATHEXT` when the variable is unset.
pub const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// [`windows_system_dir`] on hosts that are not Windows, where Windows
/// command lines are only built (for tests), never run.
pub const DEFAULT_WINDOWS_SYSTEM_DIR: &str = r"C:\Windows\System32";

/// The Windows system directory (`GetSystemDirectoryW`, normally
/// `C:\Windows\System32`).
///
/// Only administrators can write there. `cmd.exe` (and its `start`) look a
/// bare program name up in the CURRENT directory before `PATH` (unless
/// `NoDefaultCurrentDirectoryInExePath` is set, which it is not by default),
/// so core names `cmd.exe` by its absolute path ([`windows_cmd_exe`]) and
/// runs a `cmd.exe` that has to find programs in this directory instead of
/// a session or workspace directory, where an untrusted repository could
/// have planted a `cmd.exe`/`cmd.bat`.
pub fn windows_system_dir() -> PathBuf {
    #[cfg(windows)]
    if let Some(dir) = system_directory() {
        return dir;
    }
    PathBuf::from(DEFAULT_WINDOWS_SYSTEM_DIR)
}

/// `cmd.exe` by absolute path: `<system dir>\cmd.exe`
/// ([`windows_system_dir`]).
pub fn windows_cmd_exe() -> PathBuf {
    windows_system_dir().join("cmd.exe")
}

#[cfg(windows)]
fn system_directory() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetSystemDirectoryW(buffer: *mut u16, size: u32) -> u32;
    }

    let mut buffer = vec![0u16; 260];
    // The second round has the size the first one asked for.
    for _ in 0..2 {
        let capacity = u32::try_from(buffer.len()).ok()?;
        // SAFETY: `buffer` is valid for writes of `capacity` UTF-16 units.
        let len = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), capacity) } as usize;
        if len == 0 {
            return None;
        }
        if len < buffer.len() {
            buffer.truncate(len);
            let dir = PathBuf::from(OsString::from_wide(&buffer));
            return dir.is_absolute().then_some(dir);
        }
        // Too small: `len` is the required size, including the terminator.
        buffer.resize(len, 0);
    }
    None
}

/// How a resolved program has to be started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgramKind {
    /// A native executable: run it directly.
    Executable,
    /// A Windows batch script (`.cmd`/`.bat`): run it via `cmd.exe`.
    BatchScript,
}

/// A program found on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedProgram {
    pub path: PathBuf,
    pub kind: ProgramKind,
}

impl ResolvedProgram {
    fn new(path: PathBuf) -> Self {
        let kind = if is_batch_script(&path) {
            ProgramKind::BatchScript
        } else {
            ProgramKind::Executable
        };
        Self { path, kind }
    }

    /// The words to run this program with `args` through a launcher that
    /// quotes each word with the MSVC rules (a terminal emulator's argv): the
    /// program itself, or `<system dir>\cmd.exe /C <script> <args...>` for a
    /// batch script (`cmd.exe` by absolute path, see [`windows_cmd_exe`]).
    ///
    /// `cmd /C` re-reads that command line with its own rules, which no
    /// per-word quoting controls: with more than two quotes it strips the
    /// first and the last one, and it interprets `& | < > ^ %` outside
    /// quotes. For a batch script this therefore fails unless the result is
    /// unambiguous: the script path contains no quote or `cmd.exe` special
    /// character (spaces are fine: a launcher quotes the path and `cmd`
    /// keeps a single quoted pair), and no argument is empty or contains a
    /// space, tab, quote or one of `& | < > ^ ( ) % !`. Spawn a batch script
    /// with [`Self::command`], or open a terminal with a launcher that builds
    /// a `cmd.exe` command line ([`crate::resume::terminal`]), to pass any
    /// argument.
    pub fn argv(&self, args: &[OsString]) -> Result<Vec<OsString>> {
        let mut argv = Vec::with_capacity(args.len() + 3);
        if self.kind == ProgramKind::BatchScript {
            self.check_plain_batch_command(args)?;
            argv.push(windows_cmd_exe().into_os_string());
            argv.push(OsString::from("/C"));
        }
        argv.push(self.path.clone().into_os_string());
        argv.extend(args.iter().cloned());
        Ok(argv)
    }

    fn check_plain_batch_command(&self, args: &[OsString]) -> Result<()> {
        let path = self.path.to_string_lossy();
        if path.chars().any(|ch| {
            ch.is_control() || "\"&|<>^()@%!,;=".contains(ch) || ch == char::REPLACEMENT_CHARACTER
        }) {
            return Err(AppError::ClaudeExecutionError(format!(
                "Cannot run the batch script '{path}' through `cmd /C`: its path contains a \
                 cmd.exe special character"
            )));
        }
        for arg in args {
            let text = arg.to_string_lossy();
            if text.is_empty()
                || text.chars().any(|ch| {
                    ch.is_whitespace()
                        || ch.is_control()
                        || "\"&|<>^()%!".contains(ch)
                        || ch == char::REPLACEMENT_CHARACTER
                })
            {
                return Err(AppError::ClaudeExecutionError(format!(
                    "Cannot pass the argument {text:?} to the batch script '{path}' through \
                     `cmd /C`: it is empty or contains a space, a quote or a cmd.exe special \
                     character. Use Windows Terminal or the console launcher, or set the \
                     program path to an executable"
                )));
            }
        }
        Ok(())
    }

    /// The arguments of `cmd.exe` that run this batch script with `args`:
    /// `/D /S /C "<command>"`, where `<command>` is
    /// [`windows_cmd_command`](crate::resume::terminal::windows_cmd_command)
    /// (the script path quoted, every argument MSVC-quoted when needed and
    /// every `cmd.exe` metacharacter `^`-escaped). Pass it as one raw
    /// command-line tail (`CommandExt::raw_arg`), never as an argv word.
    pub fn batch_command_arguments(&self, args: &[OsString]) -> Result<String> {
        let utf8 = |value: &OsStr, what: &str| {
            value.to_str().map(str::to_string).ok_or_else(|| {
                AppError::ClaudeExecutionError(format!(
                    "{what} is not valid Unicode: {}",
                    value.to_string_lossy()
                ))
            })
        };
        let program = utf8(self.path.as_os_str(), "program path")?;
        let args = args
            .iter()
            .map(|arg| utf8(arg, "argument"))
            .collect::<Result<Vec<_>>>()?;
        let command = crate::resume::terminal::windows_cmd_command(&program, &args)?;
        Ok(format!("/D /S /C \"{command}\""))
    }

    /// A [`std::process::Command`] that runs this program with `args`. On
    /// Windows a batch script runs as `cmd.exe` (by absolute path,
    /// [`windows_cmd_exe`]) with [`Self::batch_command_arguments`] passed
    /// raw, so arguments with spaces, quotes or `cmd.exe` metacharacters
    /// arrive intact; anything else is spawned directly.
    pub fn command(&self, args: &[OsString]) -> Result<std::process::Command> {
        #[cfg(windows)]
        if self.kind == ProgramKind::BatchScript {
            use std::os::windows::process::CommandExt;
            let mut command = std::process::Command::new(windows_cmd_exe());
            command.raw_arg(self.batch_command_arguments(args)?);
            return Ok(command);
        }
        let mut command = std::process::Command::new(&self.path);
        command.args(args);
        Ok(command)
    }
}

fn is_batch_script(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        })
}

impl HistoryEnv {
    /// Resolve `name` (e.g. `"claude"`): an explicit `override_path` wins
    /// (it must exist), otherwise `PATH` is searched (with `PATHEXT` on
    /// Windows).
    pub fn resolve_program(
        &self,
        name: &str,
        override_path: Option<&Path>,
    ) -> Result<ResolvedProgram> {
        if let Some(path) = override_path {
            if path.is_file() {
                return Ok(ResolvedProgram::new(path.to_path_buf()));
            }
            return Err(AppError::ClaudeExecutionError(format!(
                "Configured program '{}' does not exist",
                path.display()
            )));
        }
        find_program(
            name,
            self.path_var.as_deref(),
            self.pathext.as_deref(),
            cfg!(windows),
        )
        .ok_or_else(|| AppError::ClaudeExecutionError(format!("Could not find '{name}' on PATH")))
    }
}

/// Search for `name` like a shell would.
///
/// * A name containing a path separator is checked as given.
/// * Otherwise every directory of `path_var` is tried in order.
/// * With `windows`, a name without an extension is tried with each
///   `PATHEXT` extension (default [`DEFAULT_PATHEXT`]); a name with an
///   extension is tried as given first. Without `windows`, the candidate must
///   be an executable file.
pub fn find_program(
    name: &str,
    path_var: Option<&OsStr>,
    pathext: Option<&OsStr>,
    windows: bool,
) -> Option<ResolvedProgram> {
    if name.is_empty() {
        return None;
    }
    let extensions = if windows {
        windows_extensions(pathext)
    } else {
        Vec::new()
    };
    let has_separator = name.contains('/') || (windows && name.contains('\\'));
    if has_separator {
        return candidates(Path::new(name), &extensions, windows)
            .into_iter()
            .find(|candidate| is_runnable(candidate, windows))
            .map(ResolvedProgram::new);
    }
    let path_var = path_var?;
    std::env::split_paths(path_var)
        .filter(|dir| !dir.as_os_str().is_empty())
        .flat_map(|dir| candidates(&dir.join(name), &extensions, windows))
        .find(|candidate| is_runnable(candidate, windows))
        .map(ResolvedProgram::new)
}

fn windows_extensions(pathext: Option<&OsStr>) -> Vec<String> {
    let value = pathext
        .and_then(OsStr::to_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(DEFAULT_PATHEXT);
    value
        .split(';')
        .map(str::trim)
        .filter(|extension| extension.starts_with('.') && extension.len() > 1)
        .map(str::to_string)
        .collect()
}

fn candidates(base: &Path, extensions: &[String], windows: bool) -> Vec<PathBuf> {
    if !windows {
        return vec![base.to_path_buf()];
    }
    let mut out = Vec::new();
    if base.extension().is_some() {
        out.push(base.to_path_buf());
    }
    for extension in extensions {
        // Windows file systems are case-insensitive; also try lower case so
        // the lookup behaves the same on case-sensitive test file systems.
        for spelling in [extension.to_ascii_lowercase(), extension.clone()] {
            let mut candidate = base.as_os_str().to_os_string();
            candidate.push(&spelling);
            let candidate = PathBuf::from(candidate);
            if !out.contains(&candidate) {
                out.push(candidate);
            }
        }
    }
    out
}

fn is_runnable(path: &Path, windows: bool) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    if windows {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn path_var(dirs: &[&Path]) -> OsString {
        std::env::join_paths(dirs).unwrap()
    }

    #[test]
    fn windows_lookup_finds_npm_cmd_shims_in_path_order() {
        let root = tempfile::tempdir().unwrap();
        let npm = root.path().join("npm");
        let native = root.path().join("native");
        touch(&npm.join("claude.cmd"));
        touch(&native.join("claude.exe"));
        let path = path_var(&[&root.path().join("missing"), &npm, &native]);

        let found = find_program("claude", Some(&path), None, true).unwrap();
        assert_eq!(found.path, npm.join("claude.cmd"));
        assert_eq!(found.kind, ProgramKind::BatchScript);
        assert_eq!(
            found
                .argv(&[OsString::from("--resume"), OsString::from("abc")])
                .unwrap(),
            vec![
                windows_cmd_exe().into_os_string(),
                OsString::from("/C"),
                npm.join("claude.cmd").into_os_string(),
                OsString::from("--resume"),
                OsString::from("abc"),
            ]
        );

        let reversed = path_var(&[&native, &npm]);
        let found = find_program("claude", Some(&reversed), None, true).unwrap();
        assert_eq!(found.path, native.join("claude.exe"));
        assert_eq!(found.kind, ProgramKind::Executable);
        assert_eq!(
            found.argv(&[]).unwrap(),
            vec![native.join("claude.exe").into_os_string()]
        );
    }

    #[test]
    fn batch_argv_refuses_what_cmd_c_would_mangle() {
        let batch = |path: &str| ResolvedProgram {
            path: PathBuf::from(path),
            kind: ProgramKind::BatchScript,
        };
        let args = |args: &[&str]| args.iter().map(OsString::from).collect::<Vec<_>>();
        let npm = batch(r"C:\Users\John Doe\AppData\Roaming\npm\pi.cmd");

        // A quoted script path and plain arguments: cmd keeps the one pair.
        let plain = args(&["--session", r"C:\Users\x\.pi\s.jsonl", "--model=opus"]);
        assert_eq!(npm.argv(&plain).unwrap().len(), 6);
        // Arguments that would need quotes or contain cmd metacharacters.
        for bad in [
            r"C:\Users\John Doe\.pi\s.jsonl",
            "a&echo INJECTED",
            "say \"hi\"",
            "100%PATH%",
            "x|y",
            "(x)",
            "",
            "tab\there",
            "line\nbreak",
        ] {
            assert!(
                npm.argv(&args(&["--session", bad])).is_err(),
                "{bad:?} must be refused"
            );
            // An executable takes any argument (no cmd.exe in between).
            let exe = ResolvedProgram {
                path: PathBuf::from(r"C:\Program Files\pi\pi.exe"),
                kind: ProgramKind::Executable,
            };
            assert_eq!(exe.argv(&args(&["--session", bad])).unwrap().len(), 3);
        }
        // Script paths cmd.exe would misread.
        for bad in [
            r"C:\R&D\pi.cmd",
            r"C:\Program Files (x86)\npm\pi.cmd",
            r"C:\100%\pi.cmd",
        ] {
            assert!(batch(bad).argv(&[]).is_err(), "{bad}");
        }
    }

    #[test]
    fn batch_command_arguments_quote_and_escape_for_cmd_s_c() {
        let npm = ResolvedProgram {
            path: PathBuf::from(r"C:\Users\John Doe\npm\pi.cmd"),
            kind: ProgramKind::BatchScript,
        };
        let line = npm
            .batch_command_arguments(&[
                OsString::from("--session"),
                OsString::from(r"C:\Users\John Doe\.pi\s.jsonl"),
                OsString::from("a&echo INJECTED"),
            ])
            .unwrap();
        assert_eq!(
            line,
            r#"/D /S /C ""C:\Users\John Doe\npm\pi.cmd" --session ^"C:\Users\John Doe\.pi\s.jsonl^" ^"a^&echo INJECTED^"""#
        );
        assert!(
            npm.batch_command_arguments(&[OsString::from("a\nb")])
                .is_err()
        );
    }

    /// The real `cmd.exe` running a temporary shim that echoes `%*`, in a
    /// directory with a space: the arguments arrive quoted and nothing runs
    /// as a separate command.
    #[cfg(windows)]
    #[test]
    fn batch_command_runs_through_real_cmd_intact() {
        let root = tempfile::tempdir().unwrap();
        let shim = root.path().join("dir with space").join("shim.cmd");
        std::fs::create_dir_all(shim.parent().unwrap()).unwrap();
        std::fs::write(&shim, "@echo off\r\necho [%*]\r\n").unwrap();
        let program = ResolvedProgram::new(shim);
        assert_eq!(program.kind, ProgramKind::BatchScript);
        let output = program
            .command(&[
                OsString::from("--session"),
                OsString::from(r"C:\Users\John Doe\s.jsonl"),
                OsString::from("a&echo INJECTED"),
            ])
            .unwrap()
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            stdout.trim_end(),
            r#"[--session "C:\Users\John Doe\s.jsonl" "a&echo INJECTED"]"#
        );
    }

    #[test]
    fn cmd_exe_is_named_by_absolute_path_in_the_system_directory() {
        let dir = windows_system_dir();
        assert!(dir.is_absolute() || !cfg!(windows), "{dir:?}");
        assert_eq!(windows_cmd_exe(), dir.join("cmd.exe"));
        if cfg!(windows) {
            assert!(windows_cmd_exe().is_file(), "{:?}", windows_cmd_exe());
        } else {
            assert_eq!(dir, PathBuf::from(DEFAULT_WINDOWS_SYSTEM_DIR));
        }
    }

    #[test]
    fn windows_lookup_honors_pathext_and_explicit_extensions() {
        let root = tempfile::tempdir().unwrap();
        touch(&root.path().join("omp.bat"));
        touch(&root.path().join("pi.ps1"));
        let path = path_var(&[root.path()]);

        assert!(find_program("omp", Some(&path), Some(OsStr::new(".EXE")), true).is_none());
        let found = find_program("omp", Some(&path), Some(OsStr::new(".EXE;.BAT")), true).unwrap();
        assert_eq!(found.kind, ProgramKind::BatchScript);
        // PowerShell shims are not in PATHEXT and cannot be run directly.
        assert!(find_program("pi", Some(&path), None, true).is_none());
        assert!(find_program("pi.ps1", Some(&path), None, true).is_some());
        assert!(find_program("claude", None, None, true).is_none());
        assert!(find_program("", Some(&path), None, true).is_none());
    }

    #[test]
    fn names_with_separators_are_checked_directly() {
        let root = tempfile::tempdir().unwrap();
        let tool = root.path().join("bin").join("claude.exe");
        touch(&tool);
        let name = tool.to_string_lossy().into_owned();
        assert_eq!(find_program(&name, None, None, true).unwrap().path, tool);
        let without_extension = root.path().join("bin").join("claude");
        assert_eq!(
            find_program(&without_extension.to_string_lossy(), None, None, true)
                .unwrap()
                .path,
            tool
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_lookup_requires_the_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        touch(&first.join("claude"));
        std::fs::set_permissions(first.join("claude"), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        touch(&second.join("claude"));
        let path = path_var(&[&first, &second]);
        let found = find_program("claude", Some(&path), None, false).unwrap();
        assert_eq!(found.path, second.join("claude"));
        assert_eq!(found.kind, ProgramKind::Executable);
    }

    #[test]
    fn env_override_wins_and_must_exist() {
        let root = tempfile::tempdir().unwrap();
        let custom = root.path().join("custom").join("claude.cmd");
        touch(&custom);
        let env = HistoryEnv::sandbox(root.path());

        let resolved = env.resolve_program("claude", Some(&custom)).unwrap();
        assert_eq!(resolved.path, custom);
        assert_eq!(resolved.kind, ProgramKind::BatchScript);
        assert!(
            env.resolve_program("claude", Some(&root.path().join("nope.exe")))
                .is_err()
        );
        // The sandbox has no PATH, so nothing is found by name.
        assert!(env.resolve_program("claude", None).is_err());

        let bin = root.path().join("bin");
        let program = if cfg!(windows) { "pi.exe" } else { "pi" };
        touch(&bin.join(program));
        let env = HistoryEnv {
            path_var: Some(path_var(&[&bin])),
            ..env
        };
        assert_eq!(
            env.resolve_program("pi", None).unwrap().path,
            bin.join(program)
        );
    }
}
