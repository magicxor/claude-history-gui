//! claude-history-core: the UI-agnostic core of claude-history-gui.
//!
//! Derived from claude-history (<https://github.com/raine/claude-history>,
//! v0.1.76, commit 8d4cf67), Copyright (c) 2024 Raine, MIT licensed; see
//! `LICENSE-MIT` in this crate. The upstream terminal UI, CLI parsing and
//! self-updater are not part of this crate.
//!
//! Every input that upstream read from the process (home directory,
//! `CLAUDE_CONFIG_DIR`, Pi/OMP variables, the working directory) comes from
//! an explicit [`env::HistoryEnv`]; see that module for the default data
//! locations, which are isolated from the installed CLI.

// Style lints that newer clippy versions raise on code kept verbatim from
// upstream. Allowed crate-wide to keep the diff against upstream minimal.
#![allow(
    clippy::derivable_impls,
    clippy::needless_return,
    clippy::obfuscated_if_else,
    clippy::unnecessary_sort_by,
    clippy::useless_vec
)]

pub mod agent;
pub mod claude;
pub mod commands;
pub mod config;
pub mod debug;
pub mod debug_log;
pub mod env;
pub mod error;
pub mod history;
pub mod markdown;
pub mod program;
pub mod search;
pub mod semantic;
pub mod syntax;
pub mod text_match;
pub mod theme;
pub mod time_filter;
pub mod tool_format;

// List controller, viewer, export and actions (built on the modules above).
pub mod browser;
pub mod display_format;
pub mod export;
pub mod maintenance;
pub mod resume;
pub mod semantic_ops;
pub mod semantic_worker;
pub mod settings;
pub mod update;
pub mod view;

pub use env::{HistoryEnv, WorkspaceScope};
pub use error::{AppError, Result};
