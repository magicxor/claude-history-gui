//! A session's `workspace.yaml`: the metadata the Copilot CLI keeps beside
//! the event log, of which the titles and the fallbacks for the working
//! directory and the last activity are read.
//!
//! The file is a flat YAML mapping of scalars. Only that subset is read
//! (plain, quoted and block scalars of top-level keys), so no YAML parser is
//! needed; anything else in it is skipped. A missing or unreadable file
//! reads as an empty one.

use std::fs::File;
use std::io::Read;
use std::path::Path;

/// More than any `workspace.yaml` holds; a larger file is cut off here.
const MAX_BYTES: u64 = 1 << 20;

#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct Workspace {
    pub cwd: Option<String>,
    /// The session's name: the user's when `user_named`, else generated.
    pub name: Option<String>,
    pub user_named: bool,
    /// The title older CLI versions kept.
    pub summary: Option<String>,
    pub updated_at: Option<String>,
}

impl Workspace {
    pub(super) fn read(path: &Path) -> Self {
        let mut bytes = Vec::new();
        let read = File::open(path).and_then(|file| file.take(MAX_BYTES).read_to_end(&mut bytes));
        match read {
            Ok(_) => Self::parse(&String::from_utf8_lossy(&bytes)),
            Err(_) => Self::default(),
        }
    }

    fn parse(text: &str) -> Self {
        let mut workspace = Self::default();
        let mut lines = text.lines().peekable();
        while let Some(line) = lines.next() {
            // A top-level key starts in the first column.
            if line.starts_with([' ', '\t', '#', '-']) || line.trim().is_empty() {
                continue;
            }
            let Some((key, first)) = line.split_once(':') else {
                continue;
            };
            let mut continuation = Vec::new();
            while let Some(next) = lines.peek() {
                if !next.starts_with([' ', '\t']) && !next.trim().is_empty() {
                    break;
                }
                continuation.push(*next);
                lines.next();
            }
            let value = scalar(first.trim(), &continuation);
            match key.trim() {
                "cwd" => workspace.cwd = value,
                "name" => workspace.name = value,
                "summary" => workspace.summary = value,
                "updated_at" => workspace.updated_at = value,
                "user_named" => workspace.user_named = value.as_deref() == Some("true"),
                _ => {}
            }
        }
        workspace
    }
}

/// The value of a key: `first` is the rest of the key's line, `continuation`
/// the indented lines after it. `None` for an empty or null value.
fn scalar(first: &str, continuation: &[&str]) -> Option<String> {
    let value = if first.starts_with(['|', '>']) {
        block(first.starts_with('|'), continuation)
    } else {
        let folded = std::iter::once(first)
            .chain(continuation.iter().map(|line| line.trim()))
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        unquote(&folded)
    };
    (!value.is_empty() && value != "null" && value != "~").then_some(value)
}

/// A block scalar's lines without their common indentation: kept as lines
/// when `literal` (`|`), joined by spaces when folded (`>`).
fn block(literal: bool, lines: &[&str]) -> String {
    let indent = lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    let lines: Vec<&str> = lines
        .iter()
        .map(|line| line.get(indent..).unwrap_or("").trim_end())
        .collect();
    let joined = if literal {
        lines.join("\n")
    } else {
        lines
            .iter()
            .filter(|line| !line.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" ")
    };
    joined.trim().to_owned()
}

/// A flow scalar without its quotes and escapes; a plain one as it is.
fn unquote(value: &str) -> String {
    let inner = |quote: char| {
        value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
            .filter(|_| value.len() >= 2)
    };
    if let Some(inner) = inner('\'') {
        return inner.replace("''", "'");
    }
    let Some(inner) = inner('"') else {
        return value.to_owned();
    };
    let mut unescaped = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(next) = chars.next() {
        if next != '\\' {
            unescaped.push(next);
            continue;
        }
        match chars.next() {
            Some('n') => unescaped.push('\n'),
            Some('t') => unescaped.push('\t'),
            Some('r') => unescaped.push('\r'),
            Some(other) => unescaped.push(other),
            None => unescaped.push('\\'),
        }
    }
    unescaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_scalars_of_top_level_keys_are_read() {
        let workspace = Workspace::parse(
            "id: 0c0a0000-0000-4000-8000-000000000001\n\
             cwd: C:\\work\\demo\n\
             name: Ocelot cache eviction\n\
             user_named: true\n\
             summary_count: 0\n\
             updated_at: 2026-10-01T08:03:00.000Z\n",
        );
        assert_eq!(
            workspace,
            Workspace {
                cwd: Some("C:\\work\\demo".to_owned()),
                name: Some("Ocelot cache eviction".to_owned()),
                user_named: true,
                summary: None,
                updated_at: Some("2026-10-01T08:03:00.000Z".to_owned()),
            }
        );
    }

    #[test]
    fn quoted_scalars_lose_their_quotes_and_escapes() {
        let double = Workspace::parse("summary: \"Refactor: \\\"queue\\\" first\\\\last\"\n");
        assert_eq!(
            double.summary.as_deref(),
            Some("Refactor: \"queue\" first\\last")
        );
        let single = Workspace::parse("summary: 'It''s the heron: again'\nuser_named: false\n");
        assert_eq!(single.summary.as_deref(), Some("It's the heron: again"));
        assert!(!single.user_named);
        let lone = Workspace::parse("summary: \"\n");
        assert_eq!(lone.summary.as_deref(), Some("\""));
    }

    #[test]
    fn block_scalars_keep_their_lines_and_folded_ones_join_them() {
        let literal = Workspace::parse(
            "summary: |-\n  Survey the lemur parser\n\n    with subagents\ncwd: /work/demo\n",
        );
        assert_eq!(
            literal.summary.as_deref(),
            Some("Survey the lemur parser\n\n  with subagents")
        );
        assert_eq!(literal.cwd.as_deref(), Some("/work/demo"));

        let folded = Workspace::parse("summary: >-\n  Survey the\n  lemur parser\n");
        assert_eq!(folded.summary.as_deref(), Some("Survey the lemur parser"));

        let wrapped = Workspace::parse("name: Survey the\n  lemur parser\nuser_named: true\n");
        assert_eq!(wrapped.name.as_deref(), Some("Survey the lemur parser"));
        assert!(wrapped.user_named);
    }

    #[test]
    fn anything_else_reads_as_nothing() {
        assert_eq!(Workspace::parse(""), Workspace::default());
        assert_eq!(
            Workspace::parse("# a comment\n- a list item\nno colon here\n  cwd: /nested\n"),
            Workspace::default()
        );
        let empty = Workspace::parse("name:\nsummary: null\ncwd: ~\nupdated_at: ''\n");
        assert_eq!(empty, Workspace::default());
        assert_eq!(
            Workspace::read(Path::new("no-such-dir/workspace.yaml")),
            Workspace::default()
        );
    }
}
