//! Shared widgets used by several work packages (DESIGN §6). `confirm`,
//! `path_list` and `text_view` are frozen (WP0); `time_range` belongs to WP1.

pub mod confirm;
pub mod path_list;
pub mod text_view;
pub mod time_range;

pub use confirm::{ConfirmControl, ConfirmOptions, ConfirmOutcome, open_destructive_confirm};
pub use path_list::{PathItem, PathKind, PathList};
pub use text_view::ReadOnlyText;
