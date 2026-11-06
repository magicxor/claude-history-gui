//! The conversations related to one: the conversation it was started or
//! branched from, and its subagents and `/btw` forks (upstream 3c2ffeb
//! `refresh_view_family` and `ViewFamily`, which the TUI's viewer header
//! names and its `P` and `s` keys open).
//!
//! The family is looked up over every conversation the browser has, in
//! scope or not and whatever the hidden-kinds toggle says (a session's
//! subagents are hidden from the list by default), leaving out the ones
//! deleted here: in generation mode through the shown generation's cards
//! (`store::Snapshot::{parent_of, children_of}`), else over the loaded
//! conversations with `history::is_parent_of`.
//!
//! # Differences from upstream
//!
//! * **By conversation, not by row.** [`ViewFamily`] holds the related
//!   conversations, so a GUI can name and open them without them being rows
//!   of the list; upstream's holds rows of the app's conversations, whose
//!   cards it materializes for that. A card that is not a row yet (only the
//!   first cards of a generation are shown) is read from the generation
//!   with the title renamed here, without becoming one.
//! * **Status texts as constants.** The texts upstream's viewer shows when
//!   there is nothing to open are [`NO_PARENT_STATUS`],
//!   [`PARENT_NOT_FOUND_STATUS`], [`NO_CHILDREN_STATUS`] and
//!   [`PARENT_GONE_STATUS`].

use super::Browser;
use crate::history::{Conversation, is_parent_of};
use std::path::Path;
use std::sync::Arc;

/// Upstream's status when the conversation in the viewer names no parent.
pub const NO_PARENT_STATUS: &str = "This session has no parent";
/// Upstream's status when the parent the conversation names is not there.
pub const PARENT_NOT_FOUND_STATUS: &str = "Parent session not found";
/// Upstream's status when the conversation has no subagents or forks.
pub const NO_CHILDREN_STATUS: &str = "No subagents or forks in this session";
/// Upstream's status when a sidecar's session file is gone, so resuming it
/// has nothing to resume.
pub const PARENT_GONE_STATUS: &str = "The session this came from no longer exists";

/// The conversations related to one (upstream 3c2ffeb `ViewFamily`).
#[derive(Clone, Debug, Default)]
pub struct ViewFamily {
    /// The conversation it was started or branched from.
    pub parent: Option<Arc<Conversation>>,
    /// Its subagents and forks, oldest first.
    pub children: Vec<Arc<Conversation>>,
}

impl Browser {
    /// The parent and the children of the conversation stored at `path`
    /// (see the module docs); empty when the browser does not have it, and
    /// in single-file mode, which has no other conversation.
    pub fn family_of(&self, path: &Path) -> ViewFamily {
        let Some(conversation) = self.family_member(path) else {
            return ViewFamily::default();
        };
        match &self.snapshot {
            Some(snapshot) => ViewFamily {
                parent: snapshot
                    .parent_of(&conversation)
                    .and_then(|doc| self.card_conversation(doc)),
                children: snapshot
                    .children_of(&conversation)
                    .into_iter()
                    .filter_map(|doc| self.card_conversation(doc))
                    .collect(),
            },
            None => {
                let parent = self
                    .conversations
                    .iter()
                    .find(|candidate| is_parent_of(candidate, &conversation))
                    .cloned();
                let mut children: Vec<Arc<Conversation>> = self
                    .conversations
                    .iter()
                    .filter(|child| is_parent_of(&conversation, child))
                    .cloned()
                    .collect();
                children.sort_by_key(|child| child.timestamp);
                ViewFamily { parent, children }
            }
        }
    }

    /// The conversation stored at `path`: its row, else (generation mode)
    /// its card.
    fn family_member(&self, path: &Path) -> Option<Arc<Conversation>> {
        if let Some(index) = self.position_of_path(path) {
            return Some(self.conversations[index].clone());
        }
        let doc = self.snapshot.as_ref()?.find(path)?;
        self.card_conversation(doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::BrowserOptions;
    use crate::browser::test_helpers::plain_conversation;
    use crate::env::HistoryEnv;
    use chrono::{Local, TimeZone};
    use std::path::PathBuf;

    fn conversation(path: &str, parent: Option<&str>, day: u32) -> Conversation {
        let mut conversation = plain_conversation(PathBuf::from(path));
        if let Some(agent) = conversation.session_id.strip_prefix("agent-") {
            conversation.session_id = agent.to_owned();
        }
        conversation.timestamp = Local.with_ymd_and_hms(2024, 1, day, 0, 0, 0).unwrap();
        conversation.parent = parent.map(str::to_owned);
        conversation
    }

    #[test]
    fn a_session_names_its_subagents_and_forks_and_they_name_it() {
        let session = "/p/-work/s1.jsonl";
        let fork = "/p/-work/s1/subagents/agent-afork.jsonl";
        let subagent = "/p/-work/s1/subagents/agent-a1.jsonl";
        // A copy of the session in another project does not cross-link.
        let copied = "/p/-other/s1/subagents/agent-a2.jsonl";
        let mut conversations = vec![
            conversation(session, None, 1),
            conversation(fork, Some("s1"), 3),
            conversation(subagent, Some("s1"), 2),
            conversation(copied, Some("s1"), 4),
        ];
        conversations[1].provenance.branch = true;
        conversations[2].provenance.child = true;
        let browser = Browser::new(
            conversations,
            BrowserOptions::new(Arc::new(HistoryEnv::default())),
        );

        let family = browser.family_of(Path::new(session));
        assert!(family.parent.is_none());
        let children: Vec<&Path> = family
            .children
            .iter()
            .map(|child| child.path.as_path())
            .collect();
        assert_eq!(children, [Path::new(subagent), Path::new(fork)]);

        let family = browser.family_of(Path::new(fork));
        assert_eq!(
            family.parent.map(|parent| parent.path.clone()),
            Some(PathBuf::from(session))
        );
        assert!(family.children.is_empty());
        assert!(browser.family_of(Path::new(copied)).parent.is_none());
        assert!(
            browser
                .family_of(Path::new("/p/-work/missing.jsonl"))
                .children
                .is_empty()
        );
    }
}
