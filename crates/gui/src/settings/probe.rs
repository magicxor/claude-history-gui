//! Background probes of the Settings dialog (DESIGN §15.8: no file-system or
//! PATH access on the UI thread). Resolving the programs, detecting the
//! terminal launcher and checking typed paths run on the background executor;
//! `render` only reads the stored [`Probes`].

use std::collections::BTreeMap;
use std::path::PathBuf;

use claude_history_core::HistoryEnv;
use claude_history_core::config::{ConfigFile, GuiConfigFile};
use claude_history_core::history::model::SourceKind;

use super::draft::{Field, check_path_text};
use super::pages::{program_status_for, terminal_preview_for};

/// The program path fields (General) and the agent each one starts; every
/// agent's `command` field (Resume, upstream 8a99ccc) shows the same probe
/// (`Field::command_of`).
pub const PROGRAM_FIELDS: [(Field, SourceKind); 4] = [
    (Field::ClaudePath, SourceKind::Claude),
    (Field::PiPath, SourceKind::Pi),
    (Field::OmpPath, SourceKind::Omp),
    (Field::CodexPath, SourceKind::Codex),
];

/// The agent whose program a program path or `command` field sets.
pub fn program_agent(field: Field) -> Option<SourceKind> {
    field.command_agent().or_else(|| {
        PROGRAM_FIELDS
            .iter()
            .find(|(path_field, _)| *path_field == field)
            .map(|(_, source)| *source)
    })
}

/// What a probe needs, copied off the UI thread.
#[derive(Clone)]
pub struct ProbeInput {
    pub env: std::sync::Arc<HistoryEnv>,
    /// The configured terminal setting (`None` = auto-detect).
    pub terminal: Option<String>,
    /// The folder a sample resume would run in.
    pub cwd: Option<PathBuf>,
    /// config.toml and the draft's gui.toml: what starts each agent
    /// (`config::agent_launcher`).
    pub cli: ConfigFile,
    pub draft: GuiConfigFile,
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
    let programs = PROGRAM_FIELDS
        .iter()
        .flat_map(|(path_field, source)| {
            [
                (*path_field, *source),
                (Field::command_of(*source), *source),
            ]
        })
        .map(|(field, source)| {
            let status = program_status_for(&input.env, &input.cli, &input.draft, field, source);
            (field, status)
        })
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
