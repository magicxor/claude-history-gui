//! Build script of claude-history-core (DESIGN §14.4).
//!
//! Windows MSVC: every executable of this package (test harnesses, examples)
//! gets an 8 MB main-thread stack (`/STACK:8000000`), like the GUI's: the
//! default 1 MiB is too small for some deep recursion (markdown layout, serde
//! of large transcripts). It is set here and not as rustflags in
//! `.cargo/config.toml`: cargo also merges that file into worktrees nested in
//! the main checkout (`.claude/worktrees/*`).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if windows && msvc {
        println!("cargo:rustc-link-arg=/STACK:8000000");
    }
}
