//! QA infrastructure (DESIGN §15.3, §15.4). WP0 stub → WP7.
//!
//! * [`gate`]: the sandbox gate. It runs in `main` for every debug build and
//!   every `--features qa` build, before any data is read.
//! * `dsl`, `runner`, `probe` (feature `qa`): the in-process scenario runner.

pub mod gate;

/// The extra source lints of WP7 (PAGER/NO_COLOR, OSC 52, three-dot strings,
/// OS input APIs, the companion skill), mirroring `tools/ci/safety-lint.sh`.
#[cfg(test)]
mod lint;

#[cfg(feature = "qa")]
pub mod dsl;
#[cfg(feature = "qa")]
pub mod probe;
#[cfg(feature = "qa")]
pub mod runner;
