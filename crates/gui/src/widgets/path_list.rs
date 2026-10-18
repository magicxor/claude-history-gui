//! A list of files and folders with labels, for confirmations that name what
//! they touch (delete, copy to workspace, delete empty transcripts, delete by
//! session ID; DESIGN §5.7–§5.15). Frozen: written by WP0.
//!
//! Rows are monospace `text_xs`; folders end with a separator and the muted
//! note `(folder)`; an optional prefix (`from`, `to`) sits in a fixed lane.
//! The list scrolls when it has more than `max_rows` rows.

use std::path::{MAIN_SEPARATOR, Path, PathBuf};

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, rems,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PathKind {
    File,
    Folder,
}

/// One row of a [`PathList`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathItem {
    pub path: PathBuf,
    pub kind: PathKind,
    /// A short label in a fixed lane before the path (`from`, `to`).
    pub prefix: Option<SharedString>,
    /// A muted note after the path (e.g. `will be overwritten`).
    pub note: Option<SharedString>,
}

impl PathItem {
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            kind: PathKind::File,
            prefix: None,
            note: None,
        }
    }

    pub fn folder(path: impl Into<PathBuf>) -> Self {
        Self {
            kind: PathKind::Folder,
            ..Self::file(path)
        }
    }

    pub fn with_prefix(mut self, prefix: impl Into<SharedString>) -> Self {
        self.prefix = Some(prefix.into());
        self
    }

    pub fn with_note(mut self, note: impl Into<SharedString>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// The path as shown: folders end with a separator.
    pub fn display_path(&self) -> String {
        display_path(&self.path, self.kind)
    }
}

/// `path`, with a trailing separator for folders.
pub fn display_path(path: &Path, kind: PathKind) -> String {
    let mut text = path.display().to_string();
    if kind == PathKind::Folder && !text.ends_with(MAIN_SEPARATOR) && !text.ends_with('/') {
        text.push(MAIN_SEPARATOR);
    }
    text
}

/// A scrollable list of paths.
#[derive(IntoElement)]
pub struct PathList {
    id: ElementId,
    items: Vec<PathItem>,
    max_rows: usize,
}

impl PathList {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            items: Vec::new(),
            max_rows: 8,
        }
    }

    pub fn item(mut self, item: PathItem) -> Self {
        self.items.push(item);
        self
    }

    pub fn items(mut self, items: impl IntoIterator<Item = PathItem>) -> Self {
        self.items.extend(items);
        self
    }

    pub fn file(self, path: impl Into<PathBuf>) -> Self {
        self.item(PathItem::file(path))
    }

    pub fn folder(self, path: impl Into<PathBuf>) -> Self {
        self.item(PathItem::folder(path))
    }

    /// Rows shown before the list scrolls (default 8).
    pub fn max_rows(mut self, rows: usize) -> Self {
        self.max_rows = rows.max(1);
        self
    }

    /// The rows as plain text (one per line), e.g. for "Copy as text".
    pub fn to_text(&self) -> String {
        self.items
            .iter()
            .map(|item| {
                let mut line = String::new();
                if let Some(prefix) = &item.prefix {
                    line.push_str(prefix);
                    line.push(' ');
                }
                line.push_str(&item.display_path());
                if item.kind == PathKind::Folder {
                    line.push_str("  (folder)");
                }
                if let Some(note) = &item.note {
                    line.push_str("  ");
                    line.push_str(note);
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl RenderOnce for PathList {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let mono = theme.mono_font_family.clone();
        let has_prefix = self.items.iter().any(|item| item.prefix.is_some());
        let max_h = rems(1.25 * self.max_rows as f32);
        v_flex()
            .id(self.id)
            .w_full()
            .max_h(max_h)
            .overflow_y_scroll()
            .gap_0p5()
            .font_family(mono)
            .text_xs()
            .children(self.items.into_iter().map(move |item| {
                let path_text = item.display_path();
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_start()
                    .when(has_prefix, |row| {
                        row.child(
                            div()
                                .w(rems(2.5))
                                .flex_none()
                                .text_color(muted)
                                .child(item.prefix.clone().unwrap_or_default()),
                        )
                    })
                    .child(div().flex_1().min_w_0().child(path_text))
                    .when(item.kind == PathKind::Folder, |row| {
                        row.child(div().flex_none().text_color(muted).child("(folder)"))
                    })
                    .when_some(item.note.clone(), |row, note| {
                        row.child(div().flex_none().text_color(muted).child(note))
                    })
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_end_with_a_separator_and_are_marked_in_text() {
        let list = PathList::new("paths")
            .file(PathBuf::from("a").join("b.jsonl"))
            .folder(PathBuf::from("a").join("b"));
        let text = list.to_text();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with("b.jsonl"));
        assert!(lines[1].ends_with("(folder)"));
        assert!(lines[1].contains(&format!("b{MAIN_SEPARATOR}")));
    }
}
