//! What the tool that wrote a conversation can do with it, as its adapter
//! plans it: resume or fork it in that tool, delete it, rename it.
//!
//! # Differences from upstream
//!
//! * **The fork's plans.** Upstream's `Resume` (a `std::process::Command`
//!   plus an optional `SessionCopy` that the CLI applies before it runs the
//!   command in its own terminal) is not ported. A resume plans into
//!   [`crate::resume::ResumePlan`] (program, arguments, working directory, a
//!   [`crate::resume::CopyPlan`] the GUI confirms before it runs, warnings),
//!   which the GUI launches in a new terminal window; `resume::execute_copy`
//!   and its tests cover `SessionCopy::apply` and its test. Planning takes the
//!   [`crate::env::HistoryEnv`] and the workspace that replaces upstream's
//!   process cwd (see [`super::adapter::Adapter::resume`]).
//! * **[`refusal`]** also formats the texts of the core errors that carry a
//!   refusal (`RenameError::Unsupported`, `DeleteError::Unsupported`,
//!   [`crate::error::AppError::Unsupported`]); upstream shows them in the
//!   TUI's status line only.

use crate::history::Provenance;
use crate::history::model::SourceKind;
use crate::history::sources::Registry;
use std::path::Path;

/// The actions an adapter supports for its conversations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Capabilities {
    pub resume: bool,
    pub fork: bool,
    pub delete: bool,
    pub rename: bool,
}

impl Capabilities {
    pub const ALL: Self = Self {
        resume: true,
        fork: true,
        delete: true,
        rename: true,
    };

    pub const NONE: Self = Self {
        resume: false,
        fork: false,
        delete: false,
        rename: false,
    };
}

/// The actions the adapter of `source` supports for a conversation of
/// `provenance`: none for an orphaned one, which its tool no longer has.
pub fn capabilities_of(source: SourceKind, provenance: Provenance) -> Capabilities {
    if provenance.orphaned {
        return Capabilities::NONE;
    }
    // Capabilities belong to the kind of source, not to an instance.
    Registry::with_roots(None, None, None)
        .for_source(source)
        .capabilities()
}

/// Why `doing` ("Deleting", ...) is unavailable for a conversation of
/// `source` and `provenance`.
pub fn refusal(source: SourceKind, provenance: Provenance, doing: &str) -> String {
    if provenance.orphaned {
        format!(
            "{doing} is unavailable: {} no longer has this conversation's current history",
            source.list_label()
        )
    } else {
        format!("{doing} {} sessions is not supported", source.list_label())
    }
}

/// A conversation to act on.
#[derive(Clone, Copy, Debug)]
pub struct Target<'a> {
    /// The conversation's own artifact.
    pub path: &'a Path,
    /// The project directory the conversation is listed under.
    pub project: Option<&'a Path>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::model::SourceKind;
    use crate::history::omp_loader::OmpSessionRoot;
    use crate::history::pi_loader::PiSessionRoot;
    use crate::history::sources::Registry;
    use std::path::PathBuf;

    #[test]
    fn every_source_has_an_acting_adapter() {
        let registry = Registry::with_roots(None, None, None);
        for source in [
            SourceKind::Claude,
            SourceKind::Pi,
            SourceKind::Omp,
            SourceKind::Codex,
        ] {
            let adapter = registry.for_source(source);
            assert_eq!(adapter.instance().kind, source);
            let capabilities = adapter.capabilities();
            assert_eq!(capabilities.rename, source != SourceKind::Codex);
            assert!(capabilities.resume && capabilities.fork && capabilities.delete);
        }
    }

    /// Fork: the capabilities of a conversation, as the GUI gates its actions.
    #[test]
    fn orphaned_conversations_support_nothing() {
        let orphaned = Provenance {
            orphaned: true,
            ..Provenance::default()
        };
        for source in [
            SourceKind::Claude,
            SourceKind::Pi,
            SourceKind::Omp,
            SourceKind::Codex,
        ] {
            assert_eq!(capabilities_of(source, orphaned), Capabilities::NONE);
        }
        assert_eq!(
            capabilities_of(SourceKind::Claude, Provenance::default()),
            Capabilities::ALL
        );
        assert_eq!(
            capabilities_of(SourceKind::Codex, Provenance::default()),
            Capabilities {
                rename: false,
                ..Capabilities::ALL
            }
        );
        // Hidden threads still have their tool's actions.
        let child = Provenance {
            child: true,
            ..Provenance::default()
        };
        assert!(capabilities_of(SourceKind::Codex, child).resume);
    }

    #[test]
    fn the_configured_instance_acts_on_its_sources_conversations() {
        let root = |name: &str| PathBuf::from("roots").join(name);
        let registry = Registry::with_roots(
            Some(root("claude")),
            Some(PiSessionRoot {
                path: root("pi"),
                flat: false,
            }),
            Some(OmpSessionRoot {
                path: root("omp"),
                flat: true,
            }),
        );
        for (source, name) in [
            (SourceKind::Claude, "claude"),
            (SourceKind::Pi, "pi"),
            (SourceKind::Omp, "omp"),
        ] {
            let adapter = registry.for_source(source);
            assert_eq!(adapter.instance().kind, source);
            assert_eq!(adapter.instance().root, root(name));
        }
    }

    #[test]
    fn refusals_name_the_source_as_the_list_does() {
        let plain = Provenance::default();
        assert_eq!(
            refusal(SourceKind::Pi, plain, "Deleting"),
            "Deleting Pi sessions is not supported"
        );
        assert_eq!(
            refusal(SourceKind::Codex, plain, "Renaming"),
            "Renaming CX sessions is not supported"
        );
        let orphaned = Provenance {
            orphaned: true,
            ..plain
        };
        assert_eq!(
            refusal(SourceKind::Codex, orphaned, "Resuming"),
            "Resuming is unavailable: CX no longer has this conversation's current history"
        );
    }
}
