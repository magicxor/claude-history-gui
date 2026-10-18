//! Background probes of the Settings dialog (DESIGN §15.8: no file-system or
//! PATH access on the UI thread). Resolving the programs, detecting the
//! terminal launcher and checking typed paths run on the background executor;
//! `render` only reads the stored [`Probes`].

use std::collections::BTreeMap;
use std::path::PathBuf;

use claude_history_core::HistoryEnv;

use super::draft::{Field, Value, check_path_text};
use super::pages::{program_status_for, terminal_preview_for};

/// The program fields and the executable each one resolves.
pub const PROGRAM_FIELDS: [(Field, &str); 3] = [
    (Field::ClaudePath, "claude"),
    (Field::PiPath, "pi"),
    (Field::OmpPath, "omp"),
];

/// What a probe needs, copied off the UI thread.
#[derive(Clone)]
pub struct ProbeInput {
    pub env: std::sync::Arc<HistoryEnv>,
    /// The configured terminal setting (`None` = auto-detect).
    pub terminal: Option<String>,
    /// The folder a sample resume would run in.
    pub cwd: Option<PathBuf>,
    /// The program fields' path settings.
    pub programs: Vec<(Field, Value, &'static str)>,
    /// Typed path texts to check: `(field, text, must_be_dir)`.
    pub paths: Vec<(Field, String, bool)>,
}

/// The outcome of one probe.
#[derive(Clone, Debug, Default)]
pub struct ProbeResult {
    pub programs: BTreeMap<Field, String>,
    pub terminal: String,
    /// `(field, checked text, outcome)` for every path in the input.
    pub paths: Vec<(Field, String, Result<(), String>)>,
}

/// The stored results the dialog renders from.
#[derive(Clone, Debug)]
pub struct Probes {
    pub programs: BTreeMap<Field, String>,
    pub terminal: String,
    /// Generation of the newest probe started (older results are dropped).
    pub generation: u64,
}

impl Default for Probes {
    fn default() -> Self {
        Self {
            programs: BTreeMap::new(),
            terminal: "Preview: checking…".to_string(),
            generation: 0,
        }
    }
}

/// Run a probe (blocking; background thread only).
pub fn run(input: ProbeInput) -> ProbeResult {
    let programs = input
        .programs
        .iter()
        .map(|(field, own, name)| (*field, program_status_for(&input.env, own.clone(), name)))
        .collect();
    let paths = input
        .paths
        .iter()
        .map(|(field, text, dir)| (*field, text.clone(), check_path_text(text, *dir)))
        .collect();
    ProbeResult {
        programs,
        terminal: terminal_preview_for(&input),
        paths,
    }
}
