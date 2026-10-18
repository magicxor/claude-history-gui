//! The process console: stdout/stderr kind, attach, write (DESIGN §14.3).
//! WP0 → WP7.
//!
//! * [`stdout_kind`]: Windows: `GetStdHandle(STD_OUTPUT_HANDLE)` valid →
//!   `GetFileType` (character device with a console mode → Terminal; disk or
//!   pipe → Redirected); invalid/null → `AttachConsole(ATTACH_PARENT_PROCESS)`
//!   → success → `CONOUT$` becomes the standard handle → Terminal; failure →
//!   None. Unix: `isatty(1)` → Terminal; `fstat(1)` ok → Redirected; else None.
//! * [`ensure_console`] does the Windows attach for GUI-subsystem release
//!   builds before anything is printed (usage errors, `--help`, headless paths).
//! * Output is written with a single `write_all` + flush ([`write_all_to`]);
//!   nothing else is ever printed to stdout (F11).
//!
//! The decision logic is separated from the OS calls ([`classify_windows`],
//! [`classify_unix`]) so it is tested with mocked handles.

use std::io::{IsTerminal as _, Write};
use std::sync::OnceLock;

use crate::model::StdoutKind;

/// What `GetFileType` (plus `GetConsoleMode`) says about a standard handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleKind {
    /// A console (character device that has a console mode).
    Console,
    /// A disk file.
    Disk,
    /// A pipe.
    Pipe,
    /// Any other valid handle (a `NUL` device, a socket, …).
    Other,
    /// The handle is null or invalid (GUI-subsystem process without redirection).
    Invalid,
}

/// Windows: map the stdout handle to a [`StdoutKind`]; `attach` is called
/// only for an invalid handle and reports whether a parent console could be
/// attached and opened.
pub fn classify_windows(handle: HandleKind, attach: impl FnOnce() -> bool) -> StdoutKind {
    match handle {
        HandleKind::Console => StdoutKind::Terminal,
        HandleKind::Disk | HandleKind::Pipe | HandleKind::Other => StdoutKind::Redirected,
        HandleKind::Invalid => {
            if attach() {
                StdoutKind::Terminal
            } else {
                StdoutKind::None
            }
        }
    }
}

/// Unix: `isatty(1)` → Terminal; `fstat(1)` ok → Redirected; else None.
pub fn classify_unix(is_tty: bool, fstat_ok: bool) -> StdoutKind {
    if is_tty {
        StdoutKind::Terminal
    } else if fstat_ok {
        StdoutKind::Redirected
    } else {
        StdoutKind::None
    }
}

#[cfg(windows)]
mod sys {
    use std::sync::OnceLock;

    use super::HandleKind;
    use windows_sys::Win32::Foundation::{
        GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_CHAR, FILE_TYPE_DISK,
        FILE_TYPE_PIPE, GetFileType, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetConsoleMode, GetStdHandle, STD_ERROR_HANDLE,
        STD_OUTPUT_HANDLE, SetStdHandle,
    };

    fn kind_of(handle: HANDLE) -> HandleKind {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return HandleKind::Invalid;
        }
        // SAFETY: plain queries on a handle returned by `GetStdHandle`.
        unsafe {
            match GetFileType(handle) {
                FILE_TYPE_CHAR => {
                    let mut mode = 0u32;
                    if GetConsoleMode(handle, &mut mode) != 0 {
                        HandleKind::Console
                    } else {
                        HandleKind::Other
                    }
                }
                FILE_TYPE_DISK => HandleKind::Disk,
                FILE_TYPE_PIPE => HandleKind::Pipe,
                _ => HandleKind::Other,
            }
        }
    }

    /// The kind of the stdout (`error = false`) or stderr handle.
    pub fn handle_kind(error: bool) -> HandleKind {
        let which = if error {
            STD_ERROR_HANDLE
        } else {
            STD_OUTPUT_HANDLE
        };
        // SAFETY: `GetStdHandle` has no preconditions.
        kind_of(unsafe { GetStdHandle(which) })
    }

    /// Attach to the parent's console and make `CONOUT$` the standard output
    /// (and the standard error when that handle is invalid too). Runs once;
    /// later calls return the first result.
    pub fn attach_parent_console() -> bool {
        static RESULT: OnceLock<bool> = OnceLock::new();
        *RESULT.get_or_init(attach_parent_console_once)
    }

    fn attach_parent_console_once() -> bool {
        // The handles of a redirected `exe > file` (stdout a file, stderr the
        // parent's console, which a GUI-subsystem process cannot use). Windows
        // re-points the standard handles at the console when a process
        // attaches (the redirected output then went to the console instead of
        // the file), so the usable ones are put back below.
        // SAFETY: `GetStdHandle` has no preconditions.
        let (saved_out, saved_err) = unsafe {
            (
                GetStdHandle(STD_OUTPUT_HANDLE),
                GetStdHandle(STD_ERROR_HANDLE),
            )
        };
        let out_usable = kind_of(saved_out) != HandleKind::Invalid;
        let err_usable = kind_of(saved_err) != HandleKind::Invalid;
        // SAFETY: `AttachConsole` has no preconditions; failure is reported.
        if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
            return false;
        }
        let name: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
        // SAFETY: `name` is NUL-terminated and outlives the call.
        let conout = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if conout.is_null() || conout == INVALID_HANDLE_VALUE {
            return false;
        }
        // SAFETY: `conout` is a valid console output handle we keep open for
        // the life of the process.
        unsafe {
            SetStdHandle(
                STD_OUTPUT_HANDLE,
                if out_usable { saved_out } else { conout },
            );
            SetStdHandle(
                STD_ERROR_HANDLE,
                if err_usable { saved_err } else { conout },
            );
        }
        true
    }
}

#[cfg(unix)]
fn unix_stdout_kind() -> StdoutKind {
    // SAFETY: `isatty`/`fstat` only inspect file descriptor 1.
    unsafe {
        let is_tty = libc::isatty(1) == 1;
        let mut stat: libc::stat = std::mem::zeroed();
        let fstat_ok = libc::fstat(1, &mut stat) == 0;
        classify_unix(is_tty, fstat_ok)
    }
}

static STDOUT_KIND: OnceLock<StdoutKind> = OnceLock::new();

/// What stdout is connected to (the Windows attach runs once).
pub fn stdout_kind() -> StdoutKind {
    *STDOUT_KIND.get_or_init(|| {
        #[cfg(windows)]
        {
            let kind = classify_windows(sys::handle_kind(false), sys::attach_parent_console);
            // A redirected stdout with an invalid stderr still gets a console
            // for error text (`foo.exe > out.txt` from a shell).
            if sys::handle_kind(true) == HandleKind::Invalid {
                let _ = sys::attach_parent_console();
            }
            kind
        }
        #[cfg(unix)]
        {
            unix_stdout_kind()
        }
        #[cfg(not(any(windows, unix)))]
        {
            if std::io::stdout().is_terminal() {
                StdoutKind::Terminal
            } else {
                StdoutKind::Redirected
            }
        }
    })
}

/// Whether stdin is a terminal (Unix picker exec decision; `false` on Windows).
pub fn stdin_is_tty() -> bool {
    !cfg!(windows) && std::io::stdin().is_terminal()
}

/// Make sure a console is attached for writing (Windows GUI-subsystem
/// release builds); a no-op elsewhere. Idempotent.
pub fn ensure_console() {
    let _ = stdout_kind();
}

/// The message of a write without any stdout.
pub const NO_STDOUT: &str = "standard output is not available";

/// Write `text` with one `write_all` and flush.
pub fn write_all_to(writer: &mut impl Write, text: &str) -> Result<(), String> {
    writer
        .write_all(text.as_bytes())
        .and_then(|()| writer.flush())
        .map_err(|error| error.to_string())
}

/// Write `text` to stdout in one call and flush. Fails when the process has
/// no stdout.
pub fn write_stdout(text: &str) -> Result<(), String> {
    if stdout_kind() == StdoutKind::None {
        return Err(NO_STDOUT.to_string());
    }
    let mut stdout = std::io::stdout().lock();
    write_all_to(&mut stdout, text)
}

/// Write `text` to stderr (errors are ignored: there is nowhere to report them).
pub fn write_stderr(text: &str) {
    ensure_console();
    let mut stderr = std::io::stderr().lock();
    let _ = write_all_to(&mut stderr, text);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A writer that counts `write` and `flush` calls.
    #[derive(Default)]
    struct Mock {
        bytes: Vec<u8>,
        writes: usize,
        flushes: usize,
    }

    impl Write for Mock {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.writes += 1;
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    /// X-017: exactly the given bytes, in one write, then a flush.
    #[test]
    fn write_all_to_writes_exactly_the_bytes_in_one_write() {
        let mut mock = Mock::default();
        let text = "C:\\sbx\\.claude\\projects\\x\\3f2a19c0.jsonl\nsecond line\n";
        write_all_to(&mut mock, text).unwrap();
        assert_eq!(mock.bytes, text.as_bytes());
        assert_eq!(mock.writes, 1);
        assert_eq!(mock.flushes, 1);
    }

    #[test]
    fn write_all_to_reports_errors() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("closed pipe"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert_eq!(
            write_all_to(&mut Broken, "x"),
            Err("closed pipe".to_string())
        );
    }

    /// A-051: char/pipe/disk/invalid handles map to Terminal/Redirected/
    /// Redirected/None (an invalid handle attaches first).
    #[test]
    fn windows_handle_kinds_map_to_stdout_kinds() {
        let never = || panic!("only an invalid handle attaches");
        assert_eq!(
            classify_windows(HandleKind::Console, never),
            StdoutKind::Terminal
        );
        assert_eq!(
            classify_windows(HandleKind::Pipe, never),
            StdoutKind::Redirected
        );
        assert_eq!(
            classify_windows(HandleKind::Disk, never),
            StdoutKind::Redirected
        );
        assert_eq!(
            classify_windows(HandleKind::Other, never),
            StdoutKind::Redirected
        );
        assert_eq!(
            classify_windows(HandleKind::Invalid, || true),
            StdoutKind::Terminal
        );
        assert_eq!(
            classify_windows(HandleKind::Invalid, || false),
            StdoutKind::None
        );
    }

    #[test]
    fn unix_descriptor_states_map_to_stdout_kinds() {
        assert_eq!(classify_unix(true, true), StdoutKind::Terminal);
        assert_eq!(classify_unix(false, true), StdoutKind::Redirected);
        assert_eq!(classify_unix(false, false), StdoutKind::None);
    }
}
