//! The Tools menu (PD12, DESIGN §5.10–§5.18, §11.15–§11.17, §11.24). WP5.
//!
//! Every entry point opens its dialog (or toast) in `window`:
//!
//! | entry point | module |
//! |---|---|
//! | `open_delete_empty` | [`delete_empty`] |
//! | `open_generate_semantic_cache`, `open_clear_semantic_cache` | [`semantic_cache`] |
//! | `open_debug_search` | [`debug_search`] |
//! | `open_semantic_debug` | [`semantic_report`] |
//! | `open_debug_log` | [`debug_log`] |
//! | `show_projects_directory` | [`projects_dir`] |
//! | `open_delete_by_id` | [`delete_by_id`] |
//! | `open_jsonl_file_dialog` | [`open_file`] |
//! | `check_for_updates` | [`updates`] |
//! | `open_about` | [`about`] |
//!
//! Shared plumbing (dialog opening, worker threads, the scope control) is in
//! [`common`].

pub mod about;
pub mod common;
pub mod debug_log;
pub mod debug_search;
pub mod delete_by_id;
pub mod delete_empty;
pub mod open_file;
pub mod projects_dir;
pub mod semantic_cache;
pub mod semantic_report;
pub mod updates;

#[cfg(test)]
mod tests;

pub use about::open_about;
pub use debug_log::{open_debug_log, open_debug_log_with};
pub use debug_search::open_debug_search;
pub use delete_by_id::open_delete_by_id;
pub use delete_empty::open_delete_empty;
pub use open_file::open_jsonl_file_dialog;
pub use projects_dir::show_projects_directory;
pub use semantic_cache::{open_clear_semantic_cache, open_generate_semantic_cache};
pub use semantic_report::open_semantic_debug;
pub use updates::check_for_updates;
