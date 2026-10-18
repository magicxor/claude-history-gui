//! The QA scenario language (DESIGN §15.3): one step per line, `#` comments.
//! WP7 (feature `qa`; the parser is gpui-free and unit-tested).
//!
//! ```text
//! wait-loaded 15s                      # until ListPane is Ready
//! type "token refresh"                 # text into the focused element (per char)
//! paste "3f2a19c0-…"                   # clipboard = text, then one paste keystroke (busy.paste)
//! key down down enter                  # keystrokes (GPUI syntax, space-separated)
//! action history::ToggleThinking       # dispatch by name
//! click row:3f2a19c0                   # qa_id click (left, single); dblclick / rclick variants
//! answer-save C:\sbx\out\x.md          # next save prompt answer (or: answer-save cancel)
//! answer-open C:\sbx\a.jsonl           # next open prompt answer (or: answer-open cancel)
//! set-stdout redirected|terminal|none  # RecordingServices stdout kind
//! wait 300ms
//! wait-for viewer.items > 0 5s
//! expect focus == viewer
//! expect list.selected_title contains "Fix login"
//! timing list.ui_busy_max < 16ms       # performance probes (§15.8)
//! idle 25s                             # count frames and UI time while nothing happens (idle.*)
//! mark-memory one                      # remember the working set (memory.one.delta_pct)
//! clear-toasts                         # dismiss the window's toasts (before a shot)
//! shot viewer-dark                     # screenshot handshake
//! set-theme dark | light | system
//! resize 1280 820
//! quit 0
//! ```
//!
//! The last four additions are the integrator's (Phase C): `paste` measures
//! a paste as the UI handles it (one change event; the X-038 UUID paste),
//! `idle` the frames and UI time of a quiet window (X-007), `mark-memory` a
//! working-set baseline for the §15.8 checks, `clear-toasts` keeps earlier
//! toasts out of a screenshot.
//!
//! Values are bare words (the rest of the line, trimmed) or `"quoted"` with
//! `\"`, `\\`, `\n` and `\t` escapes; bare Windows paths keep their
//! backslashes.

use std::path::PathBuf;
use std::time::Duration;

use claude_history_core::theme::ThemePreference;

use crate::model::StdoutKind;

/// How `click` presses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickKind {
    Single,
    Double,
    Right,
}

/// A comparison of an `expect`/`wait-for`/`timing` step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmp {
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
    Contains,
    NotContains,
}

impl Cmp {
    fn parse(token: &str) -> Option<Self> {
        Some(match token {
            "==" => Self::Eq,
            "!=" => Self::Ne,
            ">" => Self::Gt,
            "<" => Self::Lt,
            ">=" => Self::Ge,
            "<=" => Self::Le,
            "contains" => Self::Contains,
            "!contains" | "not-contains" => Self::NotContains,
            _ => return None,
        })
    }

    /// The operator as written.
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Gt => ">",
            Self::Lt => "<",
            Self::Ge => ">=",
            Self::Le => "<=",
            Self::Contains => "contains",
            Self::NotContains => "!contains",
        }
    }

    /// Compare an observed probe value with the expected one. Numbers
    /// (including `16ms` / `2s` durations, in milliseconds) compare
    /// numerically when both sides parse; everything else as text.
    pub fn holds(self, actual: &str, expected: &str) -> bool {
        match self {
            Self::Contains => return actual.contains(expected),
            Self::NotContains => return !actual.contains(expected),
            _ => {}
        }
        if let (Some(a), Some(b)) = (number(actual), number(expected)) {
            return match self {
                Self::Eq => (a - b).abs() < f64::EPSILON,
                Self::Ne => (a - b).abs() >= f64::EPSILON,
                Self::Gt => a > b,
                Self::Lt => a < b,
                Self::Ge => a >= b,
                Self::Le => a <= b,
                Self::Contains | Self::NotContains => unreachable!("handled above"),
            };
        }
        match self {
            Self::Eq => actual == expected,
            Self::Ne => actual != expected,
            Self::Gt => actual > expected,
            Self::Lt => actual < expected,
            Self::Ge => actual >= expected,
            Self::Le => actual <= expected,
            Self::Contains | Self::NotContains => unreachable!("handled above"),
        }
    }
}

/// A number, or a duration in milliseconds (`16ms`, `1.5s`).
pub fn number(text: &str) -> Option<f64> {
    let text = text.trim();
    if let Some(ms) = text.strip_suffix("ms") {
        return ms.trim().parse().ok();
    }
    if let Some(s) = text.strip_suffix('s') {
        return s.trim().parse::<f64>().ok().map(|s| s * 1000.0);
    }
    text.parse().ok()
}

/// A duration literal: `300ms`, `15s`, `2m` (or a bare number of seconds).
pub fn duration(text: &str) -> Option<Duration> {
    let text = text.trim();
    let (digits, unit) = match text.find(|c: char| !(c.is_ascii_digit() || c == '.')) {
        Some(at) => text.split_at(at),
        None => (text, "s"),
    };
    let value: f64 = digits.parse().ok()?;
    let seconds = match unit {
        "ms" => value / 1000.0,
        "s" => value,
        "m" => value * 60.0,
        _ => return None,
    };
    (seconds >= 0.0 && seconds.is_finite()).then(|| Duration::from_secs_f64(seconds))
}

/// One scenario step.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    WaitLoaded(Duration),
    Type(String),
    /// Put the text on the (recording) clipboard and press the paste key once.
    Paste(String),
    Key(Vec<String>),
    /// `hold <key> <count> [<interval>]`: `count` presses of one key at a
    /// key-repeat pace (default 33 ms) without the per-key settle sleep.
    Hold {
        key: String,
        count: u32,
        interval: Duration,
    },
    Action(String),
    Click {
        id: String,
        kind: ClickKind,
    },
    /// `None` = cancel.
    AnswerSave(Option<PathBuf>),
    /// `None` = cancel.
    AnswerOpen(Option<Vec<PathBuf>>),
    SetStdout(StdoutKind),
    Wait(Duration),
    WaitFor {
        probe: String,
        cmp: Cmp,
        value: String,
        timeout: Duration,
    },
    Expect {
        probe: String,
        cmp: Cmp,
        value: String,
    },
    Timing {
        probe: String,
        cmp: Cmp,
        value: String,
    },
    /// Wait this long without input and record the frames and UI time of the
    /// window meanwhile (probes `idle.*`).
    Idle(Duration),
    /// Remember the process working set under a name (probes `memory.<name>.*`).
    MarkMemory(String),
    /// Dismiss the window's toasts (they would cover a screenshot).
    ClearToasts,
    Shot(String),
    SetTheme(ThemePreference),
    Resize {
        width: f32,
        height: f32,
    },
    Quit(i32),
}

/// A parsed step with its source line.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// 1-based line number.
    pub number: usize,
    /// The step text as written (trimmed, without a trailing comment).
    pub text: String,
    pub step: Step,
}

/// A parse error with its line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub number: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.number, self.message)
    }
}

/// Parse a scenario. Errors are collected for every bad line.
pub fn parse(text: &str) -> Result<Vec<Line>, Vec<ParseError>> {
    let mut lines = Vec::new();
    let mut errors = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        let stripped = strip_comment(raw);
        let trimmed = stripped.trim();
        if trimmed.is_empty() {
            continue;
        }
        match parse_step(trimmed) {
            Ok(step) => lines.push(Line {
                number,
                text: trimmed.to_string(),
                step,
            }),
            Err(message) => errors.push(ParseError { number, message }),
        }
    }
    if errors.is_empty() {
        Ok(lines)
    } else {
        Err(errors)
    }
}

/// Drop a `#` comment (a `#` outside quotes that starts the line or follows
/// whitespace).
fn strip_comment(line: &str) -> &str {
    let mut in_quotes = false;
    let mut escaped = false;
    let mut previous_space = true;
    for (at, c) in line.char_indices() {
        if in_quotes {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_quotes = false;
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == '#' && previous_space {
            return &line[..at];
        }
        previous_space = c.is_whitespace();
    }
    line
}

/// Split into words; a quoted word keeps its inner spaces and escapes.
fn words(text: &str) -> Result<Vec<(String, bool)>, String> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '"' {
            chars.next();
            let mut word = String::new();
            let mut closed = false;
            while let Some(c) = chars.next() {
                match c {
                    '\\' => match chars.next() {
                        Some('n') => word.push('\n'),
                        Some('t') => word.push('\t'),
                        Some(escaped @ ('"' | '\\')) => word.push(escaped),
                        // Other backslashes stay (Windows paths in quotes).
                        Some(other) => {
                            word.push('\\');
                            word.push(other);
                        }
                        None => return Err("a quoted value ends with a backslash".to_string()),
                    },
                    '"' => {
                        closed = true;
                        break;
                    }
                    other => word.push(other),
                }
            }
            if !closed {
                return Err("a quoted value is not closed".to_string());
            }
            out.push((word, true));
        } else {
            let mut word = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                word.push(c);
                chars.next();
            }
            out.push((word, false));
        }
    }
    Ok(out)
}

/// Everything after the first word (the verb), unquoted when quoted whole.
fn rest_of_line(text: &str) -> &str {
    text.split_once(char::is_whitespace)
        .map(|(_, rest)| rest.trim())
        .unwrap_or("")
}

fn unquote(value: &str) -> Result<String, String> {
    let words = words(value)?;
    match words.as_slice() {
        [(word, true)] => Ok(word.clone()),
        _ => Ok(value.trim().to_string()),
    }
}

fn need_duration(token: &str, what: &str) -> Result<Duration, String> {
    duration(token)
        .ok_or_else(|| format!("{what} must be a duration like 300ms or 5s, got '{token}'"))
}

fn parse_step(text: &str) -> Result<Step, String> {
    let verb = text.split_whitespace().next().unwrap_or_default();
    let rest = rest_of_line(text);
    match verb {
        "wait-loaded" => {
            let timeout = if rest.is_empty() {
                Duration::from_secs(15)
            } else {
                need_duration(rest, "the timeout")?
            };
            Ok(Step::WaitLoaded(timeout))
        }
        "type" => {
            if rest.is_empty() {
                return Err("type needs a text".to_string());
            }
            Ok(Step::Type(unquote(rest)?))
        }
        "paste" => {
            let text = unquote(rest)?;
            if text.is_empty() {
                return Err("paste needs a text".to_string());
            }
            Ok(Step::Paste(text))
        }
        "idle" => Ok(Step::Idle(need_duration(rest, "the idle time")?)),
        "clear-toasts" if rest.is_empty() => Ok(Step::ClearToasts),
        "clear-toasts" => Err("clear-toasts takes no value".to_string()),
        "mark-memory" => {
            let valid = !rest.is_empty()
                && rest
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
            if valid {
                Ok(Step::MarkMemory(rest.to_string()))
            } else {
                Err("mark-memory needs a name of letters, digits, '-' or '_'".to_string())
            }
        }
        "key" => {
            let keys: Vec<String> = rest.split_whitespace().map(str::to_string).collect();
            if keys.is_empty() {
                return Err("key needs at least one keystroke".to_string());
            }
            Ok(Step::Key(keys))
        }
        "hold" => {
            let parts: Vec<&str> = rest.split_whitespace().collect();
            let usage = "hold needs <key> <count> [<interval>]";
            let (Some(key), Some(count)) = (parts.first(), parts.get(1)) else {
                return Err(usage.to_string());
            };
            let count: u32 = count
                .parse()
                .ok()
                .filter(|count| (1..=100_000).contains(count))
                .ok_or_else(|| "hold count must be 1..=100000".to_string())?;
            let interval = match parts.get(2) {
                Some(text) => need_duration(text, "the hold interval")?,
                None => Duration::from_millis(33),
            };
            if parts.len() > 3 {
                return Err(usage.to_string());
            }
            Ok(Step::Hold {
                key: (*key).to_string(),
                count,
                interval,
            })
        }
        "action" => {
            if rest.is_empty() || rest.contains(char::is_whitespace) {
                return Err("action needs exactly one action name".to_string());
            }
            Ok(Step::Action(rest.to_string()))
        }
        "click" | "dblclick" | "rclick" => {
            if rest.is_empty() || rest.contains(char::is_whitespace) {
                return Err(format!("{verb} needs exactly one element id"));
            }
            let kind = match verb {
                "click" => ClickKind::Single,
                "dblclick" => ClickKind::Double,
                _ => ClickKind::Right,
            };
            Ok(Step::Click {
                id: rest.to_string(),
                kind,
            })
        }
        "answer-save" => match rest {
            "" => Err("answer-save needs a path or 'cancel'".to_string()),
            "cancel" => Ok(Step::AnswerSave(None)),
            path => Ok(Step::AnswerSave(Some(PathBuf::from(unquote(path)?)))),
        },
        "answer-open" => match rest {
            "" => Err("answer-open needs a path or 'cancel'".to_string()),
            "cancel" => Ok(Step::AnswerOpen(None)),
            _ => {
                let paths: Vec<PathBuf> = words(rest)?
                    .into_iter()
                    .map(|(word, _)| PathBuf::from(word))
                    .collect();
                Ok(Step::AnswerOpen(Some(paths)))
            }
        },
        "set-stdout" => match rest {
            "redirected" => Ok(Step::SetStdout(StdoutKind::Redirected)),
            "terminal" => Ok(Step::SetStdout(StdoutKind::Terminal)),
            "none" => Ok(Step::SetStdout(StdoutKind::None)),
            other => Err(format!(
                "set-stdout takes redirected, terminal or none, got '{other}'"
            )),
        },
        "wait" => Ok(Step::Wait(need_duration(rest, "the wait")?)),
        "wait-for" => {
            let (probe, cmp, value, timeout) = comparison(rest, true)?;
            Ok(Step::WaitFor {
                probe,
                cmp,
                value,
                timeout: timeout.unwrap_or(Duration::from_secs(5)),
            })
        }
        "expect" => {
            let (probe, cmp, value, _) = comparison(rest, false)?;
            Ok(Step::Expect { probe, cmp, value })
        }
        "timing" => {
            let (probe, cmp, value, _) = comparison(rest, false)?;
            Ok(Step::Timing { probe, cmp, value })
        }
        "shot" => {
            let valid = !rest.is_empty()
                && rest
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
            if valid {
                Ok(Step::Shot(rest.to_string()))
            } else {
                Err("shot needs a name of letters, digits, '-', '_' or '.'".to_string())
            }
        }
        "set-theme" => match rest {
            "dark" => Ok(Step::SetTheme(ThemePreference::Dark)),
            "light" => Ok(Step::SetTheme(ThemePreference::Light)),
            "system" => Ok(Step::SetTheme(ThemePreference::System)),
            other => Err(format!(
                "set-theme takes dark, light or system, got '{other}'"
            )),
        },
        "resize" => {
            let sizes: Vec<&str> = rest.split_whitespace().collect();
            match sizes.as_slice() {
                [w, h] => {
                    let width: f32 = w.parse().map_err(|_| format!("bad width '{w}'"))?;
                    let height: f32 = h.parse().map_err(|_| format!("bad height '{h}'"))?;
                    if width < 200.0 || height < 200.0 {
                        return Err("resize needs at least 200 x 200".to_string());
                    }
                    Ok(Step::Resize { width, height })
                }
                _ => Err("resize needs a width and a height".to_string()),
            }
        }
        "quit" => {
            let code = if rest.is_empty() {
                0
            } else {
                rest.parse()
                    .map_err(|_| format!("quit needs an exit code, got '{rest}'"))?
            };
            Ok(Step::Quit(code))
        }
        other => Err(format!("unknown step '{other}'")),
    }
}

/// `<probe> <cmp> <value> [timeout]` (the timeout only for `wait-for`).
fn comparison(
    rest: &str,
    allow_timeout: bool,
) -> Result<(String, Cmp, String, Option<Duration>), String> {
    let (probe, after) = rest
        .split_once(char::is_whitespace)
        .ok_or_else(|| "expected '<probe> <comparison> <value>'".to_string())?;
    let after = after.trim();
    let (op, value) = after
        .split_once(char::is_whitespace)
        .ok_or_else(|| "expected '<probe> <comparison> <value>'".to_string())?;
    let cmp = Cmp::parse(op).ok_or_else(|| {
        format!("unknown comparison '{op}' (use ==, !=, >, <, >=, <=, contains, !contains)")
    })?;
    let mut value = value.trim().to_string();
    let mut timeout = None;
    if allow_timeout
        && let Some((head, last)) = value.rsplit_once(char::is_whitespace)
        && let Some(found) =
            duration(last).filter(|_| last.ends_with(|c: char| c.is_ascii_alphabetic()))
    {
        timeout = Some(found);
        value = head.trim().to_string();
    }
    // `""` is an explicit empty value (`expect list.query == ""`).
    let quoted_empty = matches!(words(&value)?.as_slice(), [(word, true)] if word.is_empty());
    let value = unquote(&value)?;
    if value.is_empty() && !quoted_empty {
        return Err("the expected value is empty".to_string());
    }
    Ok((probe.to_string(), cmp, value, timeout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_design_example_parses() {
        let text = r#"
wait-loaded 15s                      # until ListPane is Ready
type "token refresh"                 # text into the focused element
key down down enter
action history::ToggleThinking
click row:3f2a19c0
dblclick row:3f2a19c0
rclick row:3f2a19c0
answer-save C:\sbx\out\x.md
answer-save cancel
answer-open C:\sbx\a.jsonl
set-stdout redirected
wait 300ms
wait-for viewer.items > 0 5s
expect focus == viewer
expect list.selected_title contains "Fix login"
expect toast.last == "Exported to C:\sbx\out\x.md"
expect clipboard contains "3f2a19c0"
expect calls.terminal.count == 1
expect stdout == "C:\\sbx\\.claude\\projects\\3f2a.jsonl\n"
timing list.ui_busy_max < 16ms
shot viewer-dark
set-theme dark
resize 1280 820
quit 0
"#;
        let lines = parse(text).unwrap();
        assert_eq!(lines.len(), 24);
        assert_eq!(lines[0].step, Step::WaitLoaded(Duration::from_secs(15)));
        assert_eq!(lines[1].step, Step::Type("token refresh".to_string()));
        assert_eq!(
            lines[2].step,
            Step::Key(vec!["down".into(), "down".into(), "enter".into()])
        );
        assert_eq!(
            lines[7].step,
            Step::AnswerSave(Some(PathBuf::from("C:\\sbx\\out\\x.md")))
        );
        assert_eq!(lines[8].step, Step::AnswerSave(None));
        assert_eq!(
            lines[12].step,
            Step::WaitFor {
                probe: "viewer.items".into(),
                cmp: Cmp::Gt,
                value: "0".into(),
                timeout: Duration::from_secs(5)
            }
        );
        assert_eq!(
            lines[14].step,
            Step::Expect {
                probe: "list.selected_title".into(),
                cmp: Cmp::Contains,
                value: "Fix login".into()
            }
        );
        // A bare value keeps backslashes; a quoted one processes escapes.
        assert_eq!(
            lines[15].step,
            Step::Expect {
                probe: "toast.last".into(),
                cmp: Cmp::Eq,
                value: "Exported to C:\\sbx\\out\\x.md".into()
            }
        );
        assert_eq!(
            lines[18].step,
            Step::Expect {
                probe: "stdout".into(),
                cmp: Cmp::Eq,
                value: "C:\\sbx\\.claude\\projects\\3f2a.jsonl\n".into()
            }
        );
        assert_eq!(lines[20].step, Step::Shot("viewer-dark".into()));
        assert_eq!(
            lines[22].step,
            Step::Resize {
                width: 1280.0,
                height: 820.0
            }
        );
        assert_eq!(lines[23].step, Step::Quit(0));
        assert_eq!(
            lines[4].number, 6,
            "line numbers count blank and comment lines"
        );
    }

    #[test]
    fn comments_and_blank_lines_are_ignored_and_hashes_in_quotes_stay() {
        let lines = parse("# only a comment\n\n   \ntype \"a # b\" # trailing\n").unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].step, Step::Type("a # b".to_string()));
        assert_eq!(lines[0].number, 4);
    }

    #[test]
    fn errors_name_the_line_and_are_all_reported() {
        let errors = parse("wait 5x\nfly away\nexpect a ?? b\nresize 10 10\nquit x").unwrap_err();
        assert_eq!(errors.len(), 5);
        assert_eq!(errors[0].number, 1);
        assert!(errors[0].message.contains("duration"));
        assert_eq!(errors[1].message, "unknown step 'fly'");
        assert!(errors[2].message.contains("unknown comparison '??'"));
        assert!(errors[3].message.contains("at least 200"));
        assert!(errors[4].message.contains("exit code"));
        assert_eq!(errors[1].to_string(), "line 2: unknown step 'fly'");
    }

    #[test]
    fn hold_repeats_one_key() {
        let lines = parse(
            "hold pagedown 800
hold down 5 10ms
",
        )
        .unwrap();
        assert_eq!(
            lines[0].step,
            Step::Hold {
                key: "pagedown".into(),
                count: 800,
                interval: Duration::from_millis(33)
            }
        );
        assert_eq!(
            lines[1].step,
            Step::Hold {
                key: "down".into(),
                count: 5,
                interval: Duration::from_millis(10)
            }
        );
        for bad in [
            "hold",
            "hold down",
            "hold down 0",
            "hold down x",
            "hold down 3 fast",
            "hold down 3 1ms 2",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn missing_arguments_are_rejected() {
        for bad in [
            "type",
            "key",
            "action",
            "action a b",
            "click",
            "click a b",
            "answer-save",
            "set-stdout x",
            "set-theme x",
            "shot",
            "shot a/b",
            "expect focus",
            "expect focus ==",
            "type \"open",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn wait_for_takes_an_optional_timeout() {
        let lines =
            parse("wait-for list.count >= 3\nwait-for toast.last == \"Copied 5s\"\n").unwrap();
        assert_eq!(
            lines[0].step,
            Step::WaitFor {
                probe: "list.count".into(),
                cmp: Cmp::Ge,
                value: "3".into(),
                timeout: Duration::from_secs(5)
            }
        );
        // A quoted value that ends like a duration is still the value.
        assert_eq!(
            lines[1].step,
            Step::WaitFor {
                probe: "toast.last".into(),
                cmp: Cmp::Eq,
                value: "Copied 5s".into(),
                timeout: Duration::from_secs(5)
            }
        );
    }

    #[test]
    fn comparisons_are_numeric_when_both_sides_are_numbers() {
        assert!(Cmp::Gt.holds("12", "9"), "numeric, not text order");
        assert!(Cmp::Lt.holds("4.5ms", "16ms"));
        assert!(Cmp::Le.holds("1s", "1000ms"));
        assert!(Cmp::Eq.holds("3", "3.0"));
        assert!(Cmp::Eq.holds("viewer", "viewer"));
        assert!(Cmp::Ne.holds("list", "viewer"));
        assert!(Cmp::Contains.holds("Fix login bug", "login"));
        assert!(Cmp::NotContains.holds("Fix login bug", "logout"));
        assert!(!Cmp::Ge.holds("2", "3"));
    }

    #[test]
    fn a_quoted_empty_value_is_an_empty_expectation() {
        let lines =
            parse("expect list.query == \"\"\nexpect list.selected_title != \"\"\n").unwrap();
        assert_eq!(
            lines[0].step,
            Step::Expect {
                probe: "list.query".into(),
                cmp: Cmp::Eq,
                value: String::new(),
            }
        );
        assert!(Cmp::Eq.holds("", ""));
        assert!(Cmp::Ne.holds("Fix login bug", ""));
    }

    /// Every committed scenario parses (integrator, Phase A: the packages'
    /// scenarios are written against this DSL).
    #[test]
    fn every_committed_scenario_parses() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("qa/scenarios");
        let mut count = 0;
        for entry in std::fs::read_dir(&dir).expect("the scenarios folder") {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|ext| ext != "qa") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            if let Err(errors) = parse(&text) {
                let errors: Vec<String> = errors.iter().map(ToString::to_string).collect();
                panic!("{} does not parse:\n{}", path.display(), errors.join("\n"));
            }
            count += 1;
        }
        assert!(count >= 12, "found {count} scenarios");
    }

    /// Integrator, Phase C: `paste`, `idle`, `mark-memory` and `clear-toasts`.
    #[test]
    fn paste_idle_and_mark_memory_parse() {
        let lines = parse(
            "paste \"00000000-1c2d-4e5f-8a9b-0c1d2e3f4a5b\"\n\
             paste two words\n\
             idle 25s\n\
             mark-memory first_open\n\
             clear-toasts\n",
        )
        .unwrap();
        assert_eq!(
            lines[0].step,
            Step::Paste("00000000-1c2d-4e5f-8a9b-0c1d2e3f4a5b".into())
        );
        assert_eq!(lines[1].step, Step::Paste("two words".into()));
        assert_eq!(lines[2].step, Step::Idle(Duration::from_secs(25)));
        assert_eq!(lines[3].step, Step::MarkMemory("first_open".into()));
        assert_eq!(lines[4].step, Step::ClearToasts);
        for bad in [
            "paste",
            "paste \"\"",
            "idle",
            "idle soon",
            "mark-memory",
            "mark-memory a.b",
            "clear-toasts now",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn durations_parse() {
        assert_eq!(duration("300ms"), Some(Duration::from_millis(300)));
        assert_eq!(duration("15s"), Some(Duration::from_secs(15)));
        assert_eq!(duration("2m"), Some(Duration::from_secs(120)));
        assert_eq!(duration("5"), Some(Duration::from_secs(5)));
        assert_eq!(duration("5x"), None);
        assert_eq!(duration("ms"), None);
        assert_eq!(number("16ms"), Some(16.0));
        assert_eq!(number("abc"), None);
    }
}
