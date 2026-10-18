//! The extra source lints of WP7 as a test, mirroring `tools/ci/safety-lint.sh`
//! (DESIGN §16.2) so `cargo test` enforces them before CI exists:
//!
//! * C-048 / C-049: no `PAGER`, `NO_COLOR` or `CLICOLOR` reads;
//! * X-018: no OSC 52 clipboard sequence;
//! * X-026: no three-dot `...` in a user-visible string literal (use `…`);
//! * X-027: no OS input injection API anywhere (the QA runner dispatches
//!   events inside the process only);
//! * X-031: nothing writes the companion skill (the skills folder under `.claude`).
//!
//! The patterns are assembled with `concat!` so this file does not match.

use std::path::{Path, PathBuf};

/// Files whose string literals may hold `...` (the core-text mapper and the
/// tests that feed it, test modules and fixtures, this file).
const ELLIPSIS_ALLOWED: &[&str] = &["notify.rs", "qa/lint.rs"];

fn is_ellipsis_allowed(rel: &str) -> bool {
    ELLIPSIS_ALLOWED.contains(&rel)
        || rel.starts_with("skeleton_tests")
        || rel.starts_with("test_support/")
        || rel.ends_with("/tests.rs")
        || rel.ends_with("_tests.rs")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn sources() -> Vec<(String, String)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    files
        .into_iter()
        .map(|file| {
            let rel = file
                .strip_prefix(&src)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let text = std::fs::read_to_string(&file).unwrap();
            (rel, text)
        })
        .collect()
}

/// The string literals of one line (`"…"` with backslash escapes; raw
/// strings and char literals are not special-cased: good enough for a lint).
fn string_literals(line: &str) -> Vec<String> {
    let mut literals = Vec::new();
    let mut current: Option<String> = None;
    let mut escaped = false;
    let mut previous = '\0';
    for c in line.chars() {
        match &mut current {
            Some(text) => {
                if escaped {
                    escaped = false;
                    text.push(c);
                } else if c == '\\' {
                    escaped = true;
                    text.push(c);
                } else if c == '"' {
                    literals.push(current.take().unwrap_or_default());
                } else {
                    text.push(c);
                }
            }
            None => {
                // A quote after a single quote is a char literal ('"').
                if c == '"' && previous != '\'' {
                    current = Some(String::new());
                }
            }
        }
        previous = c;
    }
    literals
}

fn violations(check: impl Fn(&str, &str) -> bool) -> Vec<String> {
    let mut found = Vec::new();
    for (rel, text) in sources() {
        for (number, line) in text.lines().enumerate() {
            if check(&rel, line) {
                found.push(format!("{rel}:{}: {}", number + 1, line.trim()));
            }
        }
    }
    found
}

fn assert_clean(what: &str, found: Vec<String>) {
    assert!(
        found.is_empty(),
        "{what} (DESIGN §16.2):\n{}",
        found.join("\n")
    );
}

#[test]
fn no_pager_or_color_variable_reads() {
    let reads = [
        concat!("var(\"", "PAGER"),
        concat!("var_os(\"", "PAGER"),
        concat!("var(\"", "NO_COLOR"),
        concat!("var_os(\"", "NO_COLOR"),
        concat!("var(\"", "CLICOLOR"),
        concat!("var_os(\"", "CLICOLOR"),
    ];
    assert_clean(
        "PAGER / NO_COLOR / CLICOLOR are not read by the GUI (C-048, C-049)",
        violations(|_, line| reads.iter().any(|read| line.contains(read))),
    );
}

#[test]
fn no_osc52_clipboard_sequences() {
    let sequences = [
        concat!("\\x1b", "]52"),
        concat!("\\u{1b}", "]52"),
        concat!("\\033", "]52"),
        concat!("\\e", "]52"),
    ];
    assert_clean(
        "no OSC 52 clipboard sequence (X-018)",
        violations(|_, line| sequences.iter().any(|seq| line.contains(seq))),
    );
}

#[test]
fn no_three_dot_ellipsis_in_user_visible_strings() {
    assert_clean(
        "use the ellipsis character, not three dots, in string literals (X-026)",
        violations(|rel, line| {
            if is_ellipsis_allowed(rel) || line.trim_start().starts_with("//") {
                return false;
            }
            string_literals(line)
                .iter()
                .any(|literal| literal.contains("..."))
        }),
    );
}

#[test]
fn no_os_input_injection() {
    let apis = [
        concat!("Send", "Input"),
        concat!("Send", "Keys"),
        concat!("keybd", "_event"),
        concat!("mouse", "_event"),
        concat!("SetCursor", "Pos"),
        concat!("CGEvent", "Post"),
        concat!("XTest", "Fake"),
        concat!("en", "igo"),
        concat!("UI", "Automation"),
    ];
    assert_clean(
        "the QA runner never calls OS input APIs (X-027)",
        violations(|rel, line| {
            // Comments may name the APIs (this file's own docs are exempt anyway).
            rel != "qa/lint.rs"
                && !line.trim_start().starts_with("//")
                && apis.iter().any(|api| names_api(line, api))
        }),
    );
}

/// Whether `line` names `api` as a whole identifier start: gpui's in-window
/// mouse listener (`window.on_mouse_…`) is not the Win32 input API.
fn names_api(line: &str, api: &str) -> bool {
    line.match_indices(api).any(|(at, _)| {
        !line[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

#[test]
fn the_input_api_matcher_skips_in_window_listeners() {
    let api = concat!("mouse", "_event");
    assert!(names_api(
        &format!("unsafe {{ {api}(0, 0, 0, 0, 0) }}"),
        api
    ));
    assert!(names_api(&format!("use windows_sys::{api};"), api));
    assert!(!names_api(&format!("window.on_{api}(move |e| {{}})"), api));
}

#[test]
fn nothing_writes_the_companion_skill() {
    let skills = concat!(".claude", "/skills");
    assert_clean(
        "nothing installs the companion skill (X-031)",
        violations(|rel, line| {
            rel != "qa/lint.rs" && !line.trim_start().starts_with("//") && line.contains(skills)
        }),
    );
}

#[test]
fn the_literal_scanner_finds_literals_and_skips_comments_of_char_quotes() {
    assert_eq!(
        string_literals(r#"let a = "x..."; let b = '"'; let c = "y";"#),
        vec!["x...".to_string(), "y".to_string()]
    );
    assert_eq!(
        string_literals(r#"call("a \"b\" ...", 1)"#),
        vec![r#"a \"b\" ..."#.to_string()]
    );
}
