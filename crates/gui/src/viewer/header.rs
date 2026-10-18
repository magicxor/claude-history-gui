//! The viewer header (DESIGN §9.10) from `HeaderInfo`: TUI order and
//! omission rules, parse-error badge. WP2.
//!
//! Order (upstream `render_view_header`): project (accent bold, `Unknown`
//! fallback) · custom title (only when set) · model (only when known) ·
//! `1 message` / `N messages` · duration (only when known) · tokens (only
//! when `total_tokens > 0`: `926k tokens` when the line has room, else
//! `926k`) · timestamp; the summary follows on the same line when the whole
//! line fits, else on a second line. A conversation that is not in the
//! corpus shows its file stem only (V-006). `parse_error_count > 0` adds a
//! `⚠ N` badge that opens Debug log → Parse errors (V-008).

use claude_history_core::display_format::{
    format_duration_minutes, format_header_timestamp, format_model_name, format_msg_count_long,
    format_tokens, format_tokens_long,
};

use crate::model::HeaderInfo;

/// The color role of a header part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderRole {
    Project,
    CustomTitle,
    Model,
    Secondary,
    Duration,
}

/// One `·`-separated part of the header line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderPart {
    pub text: String,
    pub role: HeaderRole,
}

/// The laid-out header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderLayout {
    pub parts: Vec<HeaderPart>,
    pub summary: Option<String>,
    /// The summary fits on the first line.
    pub summary_inline: bool,
    pub parse_errors: usize,
}

impl HeaderLayout {
    /// The header as plain text (`·`-joined; the summary after a line break
    /// when it does not fit).
    pub fn text(&self) -> String {
        let mut line = self
            .parts
            .iter()
            .map(|part| part.text.as_str())
            .collect::<Vec<_>>()
            .join(" · ");
        if let Some(summary) = &self.summary {
            line.push_str(if self.summary_inline { " · " } else { "\n" });
            line.push_str(summary);
        }
        line
    }
}

/// The tooltip of the parse-error badge.
pub fn parse_error_tooltip(count: usize) -> String {
    if count == 1 {
        "1 malformed line".to_string()
    } else {
        format!("{count} malformed lines")
    }
}

/// The badge text (`⚠ N`).
pub fn parse_error_badge(count: usize) -> String {
    format!("⚠ {count}")
}

/// Lay out the header for a line of `width` cells (`None` → file stem only).
pub fn layout(info: Option<&HeaderInfo>, file_stem: &str, width: usize) -> HeaderLayout {
    let Some(info) = info.filter(|info| !info.is_stem_only()) else {
        return HeaderLayout {
            parts: vec![HeaderPart {
                text: if file_stem.is_empty() {
                    "Unknown".to_string()
                } else {
                    file_stem.to_string()
                },
                role: HeaderRole::Project,
            }],
            summary: None,
            summary_inline: true,
            parse_errors: info.map_or(0, |info| info.parse_error_count),
        };
    };
    let project = info
        .project_name
        .as_ref()
        .map(|p| p.to_string())
        .unwrap_or_else(|| "Unknown".to_string());
    let custom_title = info.custom_title.as_ref().map(|t| t.to_string());
    let model = info.model.as_deref().map(format_model_name);
    let messages = format_msg_count_long(info.message_count);
    let duration = info.duration_minutes.map(format_duration_minutes);
    let timestamp = format_header_timestamp(info.timestamp);

    // Upstream's width heuristic (bytes, " · " separators, 2 leading spaces).
    let base_len = 2
        + project.len()
        + 3
        + custom_title
            .as_ref()
            .map_or(0, |title| title.chars().count() + 3)
        + model.as_ref().map_or(0, |m| m.len() + 3)
        + messages.len()
        + duration.as_ref().map_or(0, |d| d.len() + 3)
        + 3
        + timestamp.len();
    let tokens = (info.total_tokens > 0).then(|| {
        let long = format_tokens_long(info.total_tokens);
        if base_len + 3 + long.len() <= width {
            long
        } else {
            format_tokens(info.total_tokens)
        }
    });
    let summary = info.summary.as_ref().map(|s| s.to_string());
    let summary_inline = summary.as_ref().is_none_or(|summary| {
        let tokens_len = if info.total_tokens > 0 {
            format_tokens_long(info.total_tokens).len() + 3
        } else {
            0
        };
        base_len + tokens_len + 3 + summary.len() <= width
    });

    let mut parts = vec![HeaderPart {
        text: project,
        role: HeaderRole::Project,
    }];
    if let Some(title) = custom_title {
        parts.push(HeaderPart {
            text: title,
            role: HeaderRole::CustomTitle,
        });
    }
    if let Some(model) = model {
        parts.push(HeaderPart {
            text: model,
            role: HeaderRole::Model,
        });
    }
    parts.push(HeaderPart {
        text: messages,
        role: HeaderRole::Secondary,
    });
    if let Some(duration) = duration {
        parts.push(HeaderPart {
            text: duration,
            role: HeaderRole::Duration,
        });
    }
    if let Some(tokens) = tokens {
        parts.push(HeaderPart {
            text: tokens,
            role: HeaderRole::Secondary,
        });
    }
    parts.push(HeaderPart {
        text: timestamp,
        role: HeaderRole::Secondary,
    });
    HeaderLayout {
        parts,
        summary,
        summary_inline,
        parse_errors: info.parse_error_count,
    }
}
