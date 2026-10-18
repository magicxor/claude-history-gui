//! Session actions: resume/fork, rename, delete, export/copy, picker
//! select, console output (DESIGN §11.8–§11.13). Frozen module list (WP0);
//! the modules belong to WP3 (`console`: WP7).

pub mod console;
pub mod copy;
pub mod delete;
pub mod export;
pub mod rename;
pub mod resume;
pub mod select;

#[cfg(test)]
mod tests;
