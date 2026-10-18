# Design documents

This folder holds the specification the GUI is built against.

| File | What it is |
|---|---|
| [`DESIGN.md`](DESIGN.md) | The design spec of claude-history-gui: product decisions, architecture, the core contract (APIs the GUI uses and the core gaps K1–K18), windows and dialogs, file ownership, entities and events, actions and the keymap, the viewer pipeline, data flows, strings, CLI/config/state, platform notes, testing and QA, build/CI/release, and the work packages with their integration order. It is authoritative: code follows it, and a deliberate deviation is recorded in the work package's hand-off note. |
| [`FEATURES.md`](FEATURES.md) | The parity checklist against claude-history v0.1.76: one row per user-visible feature, behavior, string, key binding, config option and CLI flag (plus the GUI-only additions), each with a stable ID (`L-012`, `V-045`, …), exactly one owning work package and a concrete acceptance check (test layer codes U/P/G/Q/S/M/R/D, DESIGN §15.1). |

Both files come from the design phase. The integrator updates them when a
merged work package changes a frozen contract (after Phase 0: DESIGN §4.3
"As built" and §17.11). Change them only deliberately (through the
integrator), and keep the FEATURES row IDs stable: tests, hand-off notes and
commit messages refer to them.
