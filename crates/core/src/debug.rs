//! Leveled debug messages with a pluggable sink.
//!
//! Upstream printed `[DEBUG] …` lines with `eprintln!`. Library code now
//! routes them through a process-wide sink: stderr by default (same format
//! as upstream), or whatever the embedding application installs with
//! [`set_sink`] (for example a GUI "Debug log" panel). Messages are only
//! produced when the caller passes a minimum level (`Some(level)`).

use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, RwLock};

/// Log level for debug output filtering
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DebugLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl DebugLevel {
    /// Upper-case tag used in the stderr format (`[DEBUG] message`).
    pub fn tag(self) -> &'static str {
        match self {
            DebugLevel::Debug => "DEBUG",
            DebugLevel::Info => "INFO",
            DebugLevel::Warn => "WARN",
            DebugLevel::Error => "ERROR",
        }
    }
}

impl FromStr for DebugLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "debug" => Ok(DebugLevel::Debug),
            "info" => Ok(DebugLevel::Info),
            "warn" | "warning" => Ok(DebugLevel::Warn),
            "error" => Ok(DebugLevel::Error),
            _ => Err(format!(
                "invalid log level '{}', expected: debug, info, warn, error",
                s
            )),
        }
    }
}

impl fmt::Display for DebugLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DebugLevel::Debug => write!(f, "debug"),
            DebugLevel::Info => write!(f, "info"),
            DebugLevel::Warn => write!(f, "warn"),
            DebugLevel::Error => write!(f, "error"),
        }
    }
}

/// Receives every message that passes the level filter.
pub type DebugSink = Arc<dyn Fn(DebugLevel, &str) + Send + Sync>;

static SINK: RwLock<Option<DebugSink>> = RwLock::new(None);

/// Install a process-wide sink for debug messages (replaces any previous one).
pub fn set_sink(sink: impl Fn(DebugLevel, &str) + Send + Sync + 'static) {
    let sink: DebugSink = Arc::new(sink);
    *SINK
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(sink);
}

/// Restore the default sink (stderr, upstream format `[LEVEL] message`).
pub fn reset_sink() {
    *SINK
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

fn emit(level: DebugLevel, message: &str) {
    let sink = SINK
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    match sink {
        Some(sink) => sink(level, message),
        None => eprintln!("[{}] {}", level.tag(), message),
    }
}

/// Check if a message at `msg_level` should be printed given the configured `min_level`
pub fn should_log(min_level: DebugLevel, msg_level: DebugLevel) -> bool {
    msg_level >= min_level
}

/// Emit `message` at `level` if the minimum level allows it.
pub fn log(min_level: Option<DebugLevel>, level: DebugLevel, message: &str) {
    if let Some(min_level) = min_level
        && should_log(min_level, level)
    {
        emit(level, message);
    }
}

/// Emit a debug-level message if the minimum level allows it
pub fn debug(min_level: Option<DebugLevel>, message: &str) {
    log(min_level, DebugLevel::Debug, message);
}

/// Emit an info-level message if the minimum level allows it
pub fn info(min_level: Option<DebugLevel>, message: &str) {
    log(min_level, DebugLevel::Info, message);
}

/// Emit a warn-level message if the minimum level allows it
pub fn warn(min_level: Option<DebugLevel>, message: &str) {
    log(min_level, DebugLevel::Warn, message);
}

/// Emit an error-level message if the minimum level allows it
pub fn error(min_level: Option<DebugLevel>, message: &str) {
    log(min_level, DebugLevel::Error, message);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_log_same_level() {
        assert!(should_log(DebugLevel::Debug, DebugLevel::Debug));
        assert!(should_log(DebugLevel::Info, DebugLevel::Info));
        assert!(should_log(DebugLevel::Warn, DebugLevel::Warn));
        assert!(should_log(DebugLevel::Error, DebugLevel::Error));
    }

    #[test]
    fn test_should_log_higher_level() {
        assert!(should_log(DebugLevel::Debug, DebugLevel::Info));
        assert!(should_log(DebugLevel::Debug, DebugLevel::Warn));
        assert!(should_log(DebugLevel::Debug, DebugLevel::Error));
        assert!(should_log(DebugLevel::Info, DebugLevel::Warn));
        assert!(should_log(DebugLevel::Info, DebugLevel::Error));
        assert!(should_log(DebugLevel::Warn, DebugLevel::Error));
    }

    #[test]
    fn test_should_not_log_lower_level() {
        assert!(!should_log(DebugLevel::Info, DebugLevel::Debug));
        assert!(!should_log(DebugLevel::Warn, DebugLevel::Debug));
        assert!(!should_log(DebugLevel::Warn, DebugLevel::Info));
        assert!(!should_log(DebugLevel::Error, DebugLevel::Debug));
        assert!(!should_log(DebugLevel::Error, DebugLevel::Info));
        assert!(!should_log(DebugLevel::Error, DebugLevel::Warn));
    }

    #[test]
    fn debug_level_parses_and_displays_like_upstream() {
        assert_eq!("debug".parse::<DebugLevel>(), Ok(DebugLevel::Debug));
        assert_eq!("INFO".parse::<DebugLevel>(), Ok(DebugLevel::Info));
        assert_eq!("warning".parse::<DebugLevel>(), Ok(DebugLevel::Warn));
        assert_eq!("Error".parse::<DebugLevel>(), Ok(DebugLevel::Error));
        assert_eq!(
            "loud".parse::<DebugLevel>(),
            Err("invalid log level 'loud', expected: debug, info, warn, error".to_string())
        );
        assert_eq!(DebugLevel::Warn.to_string(), "warn");
        assert_eq!(DebugLevel::Warn.tag(), "WARN");
    }

    #[test]
    fn sink_receives_only_messages_passing_the_level_filter() {
        use std::sync::Mutex;
        // Other tests may log concurrently through the global sink, so only
        // messages carrying this test's marker are inspected.
        const MARKER: &str = "sink-filter-test-marker";
        let seen = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&seen);
        set_sink(move |level, message| {
            if message.contains(MARKER) {
                captured.lock().unwrap().push((level, message.to_string()));
            }
        });

        debug(Some(DebugLevel::Info), &format!("{MARKER} debug"));
        info(Some(DebugLevel::Info), &format!("{MARKER} info"));
        warn(None, &format!("{MARKER} disabled"));
        error(Some(DebugLevel::Warn), &format!("{MARKER} error"));
        reset_sink();

        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                (DebugLevel::Info, format!("{MARKER} info")),
                (DebugLevel::Error, format!("{MARKER} error")),
            ]
        );
    }
}
