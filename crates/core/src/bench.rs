//! Startup timing marks, printed to a sink when benchmarking is enabled.
//!
//! Differences from upstream: core reads no process state, so there is no `CLAUDE_HISTORY_BENCH`
//! lookup and no `--bench-startup` flag. [`start`] takes the sink explicitly (the same rules as
//! upstream's variable: empty or `1` means stderr, anything else is a file the marks are appended
//! to). Marks are no-ops until a caller starts the clock with a sink.

use std::ffi::OsStr;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();
static SINK: OnceLock<Option<Sink>> = OnceLock::new();

#[derive(Debug, PartialEq, Eq)]
enum Sink {
    Stderr,
    File(PathBuf),
}

fn parse_sink(value: Option<&OsStr>) -> Option<Sink> {
    match value {
        None => None,
        Some(value) if value.is_empty() || value == "1" => Some(Sink::Stderr),
        Some(value) => Some(Sink::File(value.into())),
    }
}

/// Starts the clock and fixes the sink for the rest of the process (the first call wins).
/// `None` disables the marks; `""` or `"1"` writes them to stderr; any other value is a file
/// path that marks are appended to.
pub fn start(sink: Option<&OsStr>) {
    START.get_or_init(Instant::now);
    SINK.get_or_init(|| parse_sink(sink));
}

/// Whether a sink was configured through [`start`].
pub fn enabled() -> bool {
    matches!(SINK.get(), Some(Some(_)))
}

fn write_line(sink: &Sink, line: std::fmt::Arguments) {
    match sink {
        Sink::Stderr => eprintln!("{line}"),
        Sink::File(path) => {
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(file, "{line}");
            }
        }
    }
}

fn emit(line: std::fmt::Arguments) {
    if let Some(Some(sink)) = SINK.get() {
        write_line(sink, line);
    }
}

fn format_mark(label: &str, elapsed: std::time::Duration) -> String {
    format!("bench {label} {:.1}ms", elapsed.as_secs_f64() * 1e3)
}

/// Reports the elapsed time since [`start`] under `label`.
pub fn mark(label: &str) {
    if !enabled() {
        return;
    }
    let elapsed = START.get_or_init(Instant::now).elapsed();
    emit(format_args!("{}", format_mark(label, elapsed)));
}

/// Reports the process's peak resident set size.
pub fn mark_peak_rss() {
    if !enabled() {
        return;
    }
    if let Some(bytes) = peak_rss_bytes() {
        emit(format_args!(
            "bench peak_rss {:.1}MB",
            bytes as f64 / (1024.0 * 1024.0)
        ));
    }
}

#[cfg(unix)]
fn peak_rss_bytes() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage fully initializes `usage` when it returns 0.
    let usage = unsafe {
        if libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) != 0 {
            return None;
        }
        usage.assume_init()
    };
    let max_rss = u64::try_from(usage.ru_maxrss).ok()?;
    // macOS reports bytes, Linux reports kilobytes.
    if cfg!(target_os = "macos") {
        Some(max_rss)
    } else {
        Some(max_rss * 1024)
    }
}

#[cfg(not(unix))]
fn peak_rss_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn sink_rules() {
        assert_eq!(parse_sink(None), None);
        assert_eq!(parse_sink(Some(OsStr::new(""))), Some(Sink::Stderr));
        assert_eq!(parse_sink(Some(OsStr::new("1"))), Some(Sink::Stderr));
        assert_eq!(
            parse_sink(Some(OsStr::new("marks.txt"))),
            Some(Sink::File(PathBuf::from("marks.txt")))
        );
    }

    #[test]
    fn formats_marks_and_appends_to_a_file() {
        assert_eq!(
            format_mark("first_rows", Duration::from_micros(12_345)),
            "bench first_rows 12.3ms"
        );
        let dir = tempfile::tempdir().unwrap();
        let sink = Sink::File(dir.path().join("marks.txt"));
        write_line(&sink, format_args!("one"));
        write_line(&sink, format_args!("two"));
        let text = std::fs::read_to_string(dir.path().join("marks.txt")).unwrap();
        assert_eq!(text, "one\ntwo\n");
    }

    #[test]
    fn marks_are_no_ops_without_start() {
        // `start` is never called in unit tests (the sink is process-wide).
        assert!(!enabled());
        mark("ignored");
        mark_peak_rss();
    }
}
