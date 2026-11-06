//! The search hit in the selected conversation (upstream 411e227
//! `tui/app/list_preview.rs`: whether the list previews its selected
//! result, and what the preview looks for in it).
//!
//! # Differences from upstream
//!
//! Upstream's `App` also renders the selected conversation and finds the
//! hit in it (`fill_list_preview`). The browser only says what to find:
//! [`Browser::list_hit`]; a GUI shows the conversation itself and anchors it
//! with [`crate::view::preview::anchor`].

use super::Browser;

/// What the list's preview looks for in the selected conversation: the
/// query, and the semantic evidence the row shows, which the hit is found by
/// before the query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListHit {
    pub query: String,
    pub evidence: Option<String>,
}

impl Browser {
    /// Whether the list previews its selected result: the rows are listed,
    /// the query is not blank and a row is selected.
    pub fn list_preview_wanted(&self) -> bool {
        self.readiness.list
            && !self.query.trim().is_empty()
            && self.selected_conversation_index().is_some()
    }

    /// The hit to show the selected conversation at; `None` when the list
    /// does not preview it ([`Browser::list_preview_wanted`]).
    pub fn list_hit(&self) -> Option<ListHit> {
        if !self.list_preview_wanted() {
            return None;
        }
        let evidence = self
            .semantic_result_metadata_for_selection()
            .map(|metadata| metadata.explanation.evidence_preview.clone());
        Some(ListHit {
            query: self.query.clone(),
            evidence,
        })
    }
}
