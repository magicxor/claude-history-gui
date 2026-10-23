//! Build script of the GUI binary (DESIGN §14.4).
//!
//! Windows MSVC: every executable of this package, test harnesses and
//! examples included, gets an 8 MB main-thread stack (`/STACK:8000000`): the
//! default 1 MiB is too small for some deep recursion and for GPUI's deep
//! element trees. It is set here, like in core's build script, and not as
//! rustflags in `.cargo/config.toml`: cargo also merges that file into
//! worktrees nested in the main checkout (`.claude/worktrees/*`).
//!
//! Windows MSVC: the statically linked ONNX Runtime (ort-sys, through core's
//! fastembed) always links DirectML, so the executable would import
//! `DirectML.dll` even though only the CPU execution provider is used.
//! Delay-loading it means the DLL is resolved only if DirectML is ever used,
//! so the app starts on systems without it (verified in spike-platform).
//! The delay-load arguments apply to binary targets only
//! (`rustc-link-arg-bins`): the app and the bin's own unit-test harness, not
//! the library's test executables.
//!
//! Windows: `assets/windows/app.rc` (icon + version information) is compiled
//! with `embed-resource` and linked into the `claude-history-gui` binary
//! only. No manifest is embedded (GPUI brings its own).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/windows/app.rc");
    println!("cargo:rerun-if-changed=assets/app.ico");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if windows && msvc {
        println!("cargo:rustc-link-arg=/STACK:8000000");
        println!("cargo:rustc-link-arg-bins=/DELAYLOAD:DirectML.dll");
        println!("cargo:rustc-link-arg-bins=delayimp.lib");
        // LNK4199 ("no imports found from DirectML.dll") is expected for
        // binaries that never reach the embedder (the bin's unit-test
        // harness, builds before the embedder is wired up).
        println!("cargo:rustc-link-arg-bins=/IGNORE:4199");
    }
    if windows {
        embed_windows_resources();
    }
}

/// Compile the icon and version resource into the binary.
fn embed_windows_resources() {
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| "0".to_string());
    let macros = [
        format!("VERSION_MAJOR={}", var("CARGO_PKG_VERSION_MAJOR")),
        format!("VERSION_MINOR={}", var("CARGO_PKG_VERSION_MINOR")),
        format!("VERSION_PATCH={}", var("CARGO_PKG_VERSION_PATCH")),
        format!(
            "VERSION_STR=\"{}\"",
            std::env::var("CARGO_PKG_VERSION").unwrap_or_default()
        ),
    ];
    embed_resource::compile_for(
        "assets/windows/app.rc",
        ["claude-history-gui"],
        macros.iter().map(String::as_str),
    )
    .manifest_optional()
    .expect("failed to compile assets/windows/app.rc");
}
