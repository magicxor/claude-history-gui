use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Search mode. Upstream derived `clap::ValueEnum`; core provides
/// `FromStr`/`Display` with the same lowercase spellings instead.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    Lexical,
    Semantic,
    Exact,
    Hybrid,
}

impl SearchMode {
    /// Every mode, in upstream declaration order.
    pub const ALL: [SearchMode; 4] = [
        SearchMode::Lexical,
        SearchMode::Semantic,
        SearchMode::Exact,
        SearchMode::Hybrid,
    ];

    /// The lowercase spelling used by the CLI, config files and the agent
    /// protocol (`mode=lexical`).
    pub fn as_str(self) -> &'static str {
        match self {
            SearchMode::Lexical => "lexical",
            SearchMode::Semantic => "semantic",
            SearchMode::Exact => "exact",
            SearchMode::Hybrid => "hybrid",
        }
    }
}

impl FromStr for SearchMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        SearchMode::ALL
            .into_iter()
            .find(|mode| mode.as_str() == value)
            .ok_or_else(|| {
                format!(
                    "invalid search mode '{value}', expected one of: lexical, semantic, exact, hybrid"
                )
            })
    }
}

impl fmt::Display for SearchMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Default for SearchMode {
    fn default() -> Self {
        Self::Lexical
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SearchModeResolution {
    pub cli_mode: Option<SearchMode>,
    pub config_mode: Option<SearchMode>,
    pub tui_semantic_search: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TuiSearchMode {
    #[default]
    Lexical,
    Semantic,
}

pub fn resolve_search_mode(resolution: SearchModeResolution) -> SearchMode {
    resolution
        .cli_mode
        .or(resolution.config_mode)
        .or_else(|| resolution.tui_semantic_search.map(legacy_tui_mode))
        .unwrap_or_default()
}

pub fn resolve_tui_search_mode(resolution: SearchModeResolution) -> TuiSearchMode {
    match resolve_search_mode(resolution) {
        SearchMode::Semantic => TuiSearchMode::Semantic,
        SearchMode::Lexical | SearchMode::Exact | SearchMode::Hybrid => TuiSearchMode::Lexical,
    }
}

fn legacy_tui_mode(semantic_search: bool) -> SearchMode {
    if semantic_search {
        SearchMode::Semantic
    } else {
        SearchMode::Lexical
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_mode_config_wins_over_tui_semantic_search() {
        let mode = resolve_search_mode(SearchModeResolution {
            cli_mode: None,
            config_mode: Some(SearchMode::Lexical),
            tui_semantic_search: Some(true),
        });

        assert_eq!(mode, SearchMode::Lexical);
    }

    #[test]
    fn cli_mode_wins_over_search_mode_config() {
        let mode = resolve_search_mode(SearchModeResolution {
            cli_mode: Some(SearchMode::Exact),
            config_mode: Some(SearchMode::Semantic),
            tui_semantic_search: Some(true),
        });

        assert_eq!(mode, SearchMode::Exact);
    }

    #[test]
    fn tui_semantic_search_aliases_to_semantic() {
        let mode = resolve_search_mode(SearchModeResolution {
            cli_mode: None,
            config_mode: None,
            tui_semantic_search: Some(true),
        });

        assert_eq!(mode, SearchMode::Semantic);
    }

    #[test]
    fn tui_semantic_search_false_aliases_to_lexical() {
        let mode = resolve_search_mode(SearchModeResolution {
            cli_mode: None,
            config_mode: None,
            tui_semantic_search: Some(false),
        });

        assert_eq!(mode, SearchMode::Lexical);
    }

    #[test]
    fn unset_search_mode_defaults_to_lexical() {
        assert_eq!(
            resolve_search_mode(SearchModeResolution::default()),
            SearchMode::Lexical
        );
    }

    #[test]
    fn tui_resolution_keeps_supported_semantic_mode() {
        let mode = resolve_tui_search_mode(SearchModeResolution {
            cli_mode: None,
            config_mode: Some(SearchMode::Semantic),
            tui_semantic_search: Some(false),
        });

        assert_eq!(mode, TuiSearchMode::Semantic);
    }

    #[test]
    fn tui_resolution_maps_hybrid_to_lexical() {
        let mode = resolve_tui_search_mode(SearchModeResolution {
            cli_mode: None,
            config_mode: Some(SearchMode::Hybrid),
            tui_semantic_search: Some(true),
        });

        assert_eq!(mode, TuiSearchMode::Lexical);
    }

    #[test]
    fn tui_resolution_maps_exact_to_lexical() {
        let mode = resolve_tui_search_mode(SearchModeResolution {
            cli_mode: None,
            config_mode: Some(SearchMode::Exact),
            tui_semantic_search: Some(true),
        });

        assert_eq!(mode, TuiSearchMode::Lexical);
    }

    #[test]
    fn search_mode_parses_and_displays_lowercase_names() {
        for mode in SearchMode::ALL {
            assert_eq!(mode.to_string().parse::<SearchMode>(), Ok(mode));
        }
        assert_eq!("hybrid".parse::<SearchMode>(), Ok(SearchMode::Hybrid));
        // clap's ValueEnum was case-sensitive by default; so is FromStr.
        assert!("Hybrid".parse::<SearchMode>().is_err());
        assert!("vector".parse::<SearchMode>().is_err());
        assert_eq!(SearchMode::Exact.as_str(), "exact");
    }
}
