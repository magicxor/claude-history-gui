use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON parsing error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Claude projects directory not found at {0}")]
    ProjectsDirNotFound(String),

    #[error("No conversation history found in {0}")]
    NoHistoryFound(String),

    #[error("User cancelled selection")]
    SelectionCancelled,

    #[error("Session not found: {0}")]
    SessionNotFound(String),

    #[error("Failed to run Claude CLI: {0}")]
    ClaudeExecutionError(String),

    /// The action needs a workspace folder and none is selected (GUI; the
    /// upstream CLI used its process cwd). The text is shown verbatim.
    #[error("{0}")]
    WorkspaceRequired(String),

    /// A workspace folder that is not an absolute path. Claude Code encodes
    /// its own absolute working directory, so a relative workspace can never
    /// be mapped to a projects directory (and core never resolves paths
    /// against the process cwd).
    #[error("Workspace folder must be an absolute path: {}", .0.display())]
    InvalidWorkspace(std::path::PathBuf),

    /// `CLAUDE_CONFIG_DIR` set to a relative path. Upstream resolved it
    /// against the shell's cwd; a GUI's process cwd is arbitrary, so core
    /// refuses it instead of listing, deleting or copying sessions in an
    /// unrelated directory.
    #[error("CLAUDE_CONFIG_DIR must be an absolute path: {}", .0.display())]
    InvalidClaudeConfigDir(std::path::PathBuf),

    #[error("Failed to launch terminal: {0}")]
    TerminalLaunch(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Update error: {0}")]
    UpdateError(String),

    #[error("Agent command error: {0}")]
    Agent(#[from] crate::agent::diagnostic::AgentError),

    #[error("{0}")]
    AgentProtocol(String),

    #[error("Invalid time range: {0}")]
    TimeFilter(#[from] crate::time_filter::TimeFilterError),

    #[error("Semantic search cancelled")]
    SemanticSearchCancelled,
}

pub type Result<T> = std::result::Result<T, AppError>;

#[cfg(test)]
mod tests {
    //! C-021: the Display strings are upstream's (`src/error.rs:3-43`); the
    //! GUI-only variants have their own fixed wording.
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn c021_display_strings_match_upstream() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let json = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let json_text = json.to_string();
        let cases: Vec<(AppError, String)> = vec![
            (AppError::Io(io), "I/O error: gone".into()),
            (
                AppError::Json(json),
                format!("JSON parsing error: {json_text}"),
            ),
            (
                AppError::ProjectsDirNotFound("/h/.claude/projects".into()),
                "Claude projects directory not found at /h/.claude/projects".into(),
            ),
            (
                AppError::NoHistoryFound("selected scope".into()),
                "No conversation history found in selected scope".into(),
            ),
            (
                AppError::SelectionCancelled,
                "User cancelled selection".into(),
            ),
            (
                AppError::SessionNotFound("abc".into()),
                "Session not found: abc".into(),
            ),
            (
                AppError::ClaudeExecutionError("boom".into()),
                "Failed to run Claude CLI: boom".into(),
            ),
            (
                AppError::ConfigError("bad".into()),
                "Configuration error: bad".into(),
            ),
            (
                AppError::UpdateError("offline".into()),
                "Update error: offline".into(),
            ),
            (AppError::AgentProtocol("raw".into()), "raw".into()),
            (
                AppError::TimeFilter(crate::time_filter::TimeFilterError::Empty),
                format!(
                    "Invalid time range: {}",
                    crate::time_filter::TimeFilterError::Empty
                ),
            ),
            (
                AppError::SemanticSearchCancelled,
                "Semantic search cancelled".into(),
            ),
            // GUI-only variants.
            (
                AppError::WorkspaceRequired("Choose a workspace".into()),
                "Choose a workspace".into(),
            ),
            (
                AppError::InvalidWorkspace(PathBuf::from("rel")),
                "Workspace folder must be an absolute path: rel".into(),
            ),
            (
                AppError::InvalidClaudeConfigDir(PathBuf::from("rel")),
                "CLAUDE_CONFIG_DIR must be an absolute path: rel".into(),
            ),
            (
                AppError::TerminalLaunch("no terminal".into()),
                "Failed to launch terminal: no terminal".into(),
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
        }
        let agent = crate::agent::diagnostic::AgentError::invalid_ref("x", "detail");
        let expected = format!("Agent command error: {agent}");
        assert_eq!(AppError::Agent(agent).to_string(), expected);
    }
}
