//! The safety lint of DESIGN §16.2 as a test of the GUI crate's sources, so
//! the DoD (`cargo test`) enforces it before CI exists and in addition to
//! core's `hermetic` guards (K13): process-state reads, desktop side effects
//! and launch/network calls appear only in their allow-listed files.
//! The patterns are assembled with `concat!` so this file does not match.

use std::path::{Path, PathBuf};

/// (pattern, files allowed to contain it, relative to `crates/gui/src`).
const RULES: &[(&str, &[&str])] = &[
    // Process state (the launch environment is read once, in `main`).
    (
        concat!("from_process", "_env"),
        &["main.rs", "skeleton_tests.rs"],
    ),
    (
        concat!("home::home", "_dir"),
        &["main.rs", "qa/gate.rs", "services.rs", "skeleton_tests.rs"],
    ),
    (
        concat!("dirs::home", "_dir"),
        &["main.rs", "qa/gate.rs", "services.rs"],
    ),
    (
        concat!("env::", "var("),
        &["main.rs", "qa/gate.rs", "services.rs", "skeleton_tests.rs"],
    ),
    (
        concat!("env::", "var_os("),
        &["main.rs", "qa/gate.rs", "services.rs", "skeleton_tests.rs"],
    ),
    (
        concat!("current", "_dir("),
        &["main.rs", "qa/gate.rs", "services.rs"],
    ),
    // Child processes, terminals, the network.
    (
        concat!("process::", "Command"),
        &["services.rs", "ops/console.rs"],
    ),
    // `skeleton_tests.rs` holds X-036, which proves core's hermetic guards
    // refuse these calls in GUI test builds.
    (
        concat!("terminal::spawn", "_terminal"),
        &["services.rs", "skeleton_tests.rs"],
    ),
    (
        concat!("spawn_terminal", "_checked"),
        &["services.rs", "skeleton_tests.rs"],
    ),
    (
        concat!("check_latest", "_release"),
        &["services.rs", "skeleton_tests.rs"],
    ),
    (concat!("resume::", "launch("), &["services.rs"]),
    (concat!("execute", "_copy"), &["ops/resume.rs"]),
    // Desktop side effects (only through `PlatformServices`).
    (concat!("write_to", "_clipboard"), &["services.rs"]),
    (
        concat!("read_from", "_clipboard"),
        // The guard test reads the fake platform clipboard to prove it is unused.
        &["services.rs", "skeleton_tests/contracts.rs"],
    ),
    (concat!("prompt_for", "_new_path"), &["services.rs"]),
    (concat!("prompt_for", "_paths"), &["services.rs"]),
    (concat!("cx.reveal", "_path("), &["services.rs"]),
    (concat!("cx.open", "_url("), &["services.rs"]),
    (concat!("cx.open_with", "_system("), &["services.rs"]),
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir)
        .expect("read the source dir")
        .flatten()
    {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn side_effects_stay_in_their_allow_listed_files() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(files.len() > 50, "found the sources ({})", files.len());
    let mut violations = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&file).unwrap();
        for (pattern, allowed) in RULES {
            if allowed.contains(&rel.as_str()) {
                continue;
            }
            for (number, line) in text.lines().enumerate() {
                if line.contains(pattern) {
                    violations.push(format!("{rel}:{}: `{pattern}`", number + 1));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "side effects outside their allow-listed files (DESIGN §16.2):\n{}",
        violations.join("\n")
    );
}
