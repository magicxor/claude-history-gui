//! Conversation list tests (G and P, DESIGN §17.3). WP1.
//!
//! Modules: `rows` (P: row model, layout, formatting), `loading` (streaming
//! load, empty/error states, refresh, reloads, UUID jump), `search` (query
//! input, keys, selection, pointer, scope, exclusions), `semantic` (fake
//! embedder flows), `ui` (footer, toolbar, row menu, workspace picker, time
//! filter, picker banner), `misc` (ticker, prompt, status, probes), `cjk`
//! (X-013), `perf` (ignored 20k-row probes, run in release), `smoke`.
//! Row ids in the comments are FEATURES.md rows.

mod cjk;
mod loading;
mod misc;
mod perf;
mod rows;
mod search;
mod semantic;
mod smoke;
mod states_ui;
mod ui;
