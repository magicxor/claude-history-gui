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
//! * **[`refusal`]** formats this commit's refusal texts (the TUI's status
//!   messages) for the core errors that carry them; upstream adds the same
//!   function, with a provenance parameter, in a later commit.

use crate::history::model::SourceKind;
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
}

/// Why `doing` ("Deleting", "Renaming") is unavailable for a conversation of
/// `source`: its adapter does not support it.
pub fn refusal(source: SourceKind, doing: &str) -> String {
    format!("{doing} {} sessions is not supported", source.list_label())
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
    use crate::history::Source;
    use crate::history::omp_loader::OmpSessionRoot;
    use crate::history::pi_loader::PiSessionRoot;
    use crate::history::sources::Registry;
    use std::path::PathBuf;

    #[test]
    fn every_source_has_an_acting_adapter() {
        let registry = Registry::with_roots(None, None, None);
        for source in [Source::Claude, Source::Pi, Source::Omp] {
            let adapter = registry.for_source(source);
            assert_eq!(adapter.instance().kind, source);
            assert_eq!(adapter.capabilities(), Capabilities::ALL);
        }
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
            (Source::Claude, "claude"),
            (Source::Pi, "pi"),
            (Source::Omp, "omp"),
        ] {
            let adapter = registry.for_source(source);
            assert_eq!(adapter.instance().kind, source);
            assert_eq!(adapter.instance().root, root(name));
        }
    }

    #[test]
    fn refusals_name_the_source_as_the_list_does() {
        assert_eq!(
            refusal(Source::Pi, "Deleting"),
            "Deleting Pi sessions is not supported"
        );
        assert_eq!(
            refusal(Source::Claude, "Renaming"),
            "Renaming CC sessions is not supported"
        );
    }
}
