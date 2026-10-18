//! G tests of Check for updates and About (T-020, T-021).

use claude_history_core::update::{UpdateStatus, update_status_from_response};
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext};

use super::main_window;
use crate::services::ServiceCall;
use crate::test_support::fixtures_tools::{settle, wait_for};
use crate::test_support::{TestWorld, recording};
use crate::tools::about::{self, current_dialog as current_about, open_about};
use crate::tools::updates::{CheckState, UpdatesDialog, current_dialog, describe};

const GUI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A GitHub release JSON for `tag` with the given asset names.
fn release_json(tag: &str, assets: &[String]) -> String {
    let assets: Vec<String> = assets
        .iter()
        .map(|name| {
            format!(
                r#"{{"name":"{name}","browser_download_url":"https://example.invalid/{name}","size":10,"content_type":"application/zip"}}"#
            )
        })
        .collect();
    format!(
        r#"{{"tag_name":"{tag}","name":"{tag}","html_url":"https://github.com/magicxor/claude-history-gui/releases/tag/{tag}","body":"Notes for {tag}","published_at":"2026-10-01T00:00:00Z","prerelease":false,"draft":false,"assets":[{}]}}"#,
        assets.join(",")
    )
}

fn status(tag: &str, assets: &[String]) -> UpdateStatus {
    update_status_from_response(GUI_VERSION, 200, &release_json(tag, assets)).expect("status")
}

fn this_platform_asset() -> String {
    format!(
        "claude-history-gui-{}-{}.zip",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

fn check(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    result: Result<UpdateStatus, String>,
) -> Entity<UpdatesDialog> {
    recording(cx).set_release(result);
    cx.update_window(handle, |_, window, cx| {
        crate::tools::check_for_updates(window, cx)
    })
    .unwrap();
    wait_for(cx, "the check", |cx| {
        current_dialog(cx).is_some_and(|d| !matches!(d.read(cx).state(), CheckState::Checking))
    });
    settle(cx, handle);
    cx.read(current_dialog).expect("the dialog is open")
}

fn state(cx: &TestAppContext, dialog: &Entity<UpdatesDialog>) -> CheckState {
    cx.read(|cx| dialog.read(cx).state().clone())
}

fn close(cx: &mut TestAppContext, handle: AnyWindowHandle, dialog: &Entity<UpdatesDialog>) {
    cx.update_window(handle, |_, window, cx| {
        dialog.update(cx, |d, cx| d.close(window, cx))
    })
    .unwrap();
    cx.run_until_parked();
}

/// T-020: an available release shows the §5.18 text and notes; Open release
/// page opens `release.url`.
#[gpui_kit::test]
fn an_available_release(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_f, handle) = main_window(cx, &world, &[]);
    let url = "https://github.com/magicxor/claude-history-gui/releases/tag/v99.0.0".to_string();
    let dialog = check(cx, handle, Ok(status("v99.0.0", &[this_platform_asset()])));
    let CheckState::Done(view) = state(cx, &dialog) else {
        panic!("{:?}", state(cx, &dialog))
    };
    assert_eq!(
        view.message,
        format!("Claude History GUI 99.0.0 is available (you have {GUI_VERSION}).")
    );
    assert_eq!(view.notes.as_deref(), Some("Notes for v99.0.0"));
    assert_eq!(view.release_url.as_deref(), Some(url.as_str()));
    assert!(recording(cx).calls().contains(&ServiceCall::FetchRelease));
    dialog.update(cx, |d, cx| d.open_release_page(cx));
    assert!(recording(cx).calls().contains(&ServiceCall::OpenUrl(url)));
}

#[gpui_kit::test]
fn an_available_release_without_a_build_for_this_platform(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_f, handle) = main_window(cx, &world, &[]);
    let dialog = check(
        cx,
        handle,
        Ok(status(
            "v99.0.0",
            &["claude-history-gui-plan9-mips.zip".to_string()],
        )),
    );
    let CheckState::Done(view) = state(cx, &dialog) else {
        panic!()
    };
    assert_eq!(
        view.message,
        format!(
            "Claude History GUI 99.0.0 is available (you have {GUI_VERSION}), but there is no build for this platform."
        )
    );
}

#[gpui_kit::test]
fn up_to_date_newer_and_no_release(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_f, handle) = main_window(cx, &world, &[]);
    let dialog = check(cx, handle, Ok(status(&format!("v{GUI_VERSION}"), &[])));
    let CheckState::Done(view) = state(cx, &dialog) else {
        panic!()
    };
    assert_eq!(
        view.message,
        format!("Claude History GUI is up to date ({GUI_VERSION}).")
    );
    assert!(view.release_url.is_none());
    close(cx, handle, &dialog);

    let dialog = check(cx, handle, Ok(status("v0.0.1", &[])));
    let CheckState::Done(view) = state(cx, &dialog) else {
        panic!()
    };
    assert_eq!(
        view.message,
        format!("This build ({GUI_VERSION}) is newer than the latest release (0.0.1).")
    );
    close(cx, handle, &dialog);

    let none = update_status_from_response(GUI_VERSION, 404, "").unwrap();
    let dialog = check(cx, handle, Ok(none));
    let CheckState::Done(view) = state(cx, &dialog) else {
        panic!()
    };
    assert_eq!(view.message, "No release has been published yet.");
}

/// T-020: an error shows `Update error: <e>` and Retry checks again.
#[gpui_kit::test]
fn an_error_can_be_retried(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_f, handle) = main_window(cx, &world, &[]);
    let dialog = check(cx, handle, Err("boom".to_string()));
    assert_eq!(
        state(cx, &dialog),
        CheckState::Failed("Update error: boom".to_string())
    );
    recording(cx).set_release(Ok(status("v99.0.0", &[this_platform_asset()])));
    dialog.update(cx, |d, cx| d.check(cx));
    wait_for(cx, "the retry", |cx| {
        matches!(dialog.read(cx).state(), CheckState::Done(_))
    });
    // Core's own text keeps its prefix once.
    assert_eq!(
        crate::tools::updates::error_message("Update error: x"),
        "Update error: x"
    );
}

#[test]
fn describe_has_no_notes_for_empty_release_notes() {
    let json = release_json("v99.0.0", &[]).replace("Notes for v99.0.0", "  ");
    let status = update_status_from_response(GUI_VERSION, 200, &json).unwrap();
    assert_eq!(describe(&status, "linux", "x86_64").notes, None);
}

/// T-021: About shows the versions and the sandbox data/cache/config/state paths.
#[gpui_kit::test]
fn about_shows_versions_and_paths(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    let (_f, handle) = main_window(cx, &world, &[]);
    cx.update_window(handle, |_, window, cx| open_about(window, cx))
        .unwrap();
    settle(cx, handle);
    let dialog = cx.read(current_about).expect("About is open");
    let lines = cx.read(|cx| dialog.read(cx).lines().to_vec());
    assert_eq!(lines[0], "Claude History GUI 0.1.0 (core 0.1.76)");
    assert_eq!(lines[1], about::TAGLINE);
    let env = &world.env;
    assert!(
        lines.contains(&format!("Data: {}", env.claude_dir().unwrap().display())),
        "{lines:?}"
    );
    assert!(
        lines.contains(&format!("Cache: {}", env.cache_root().unwrap().display())),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("Config: ")
            && l.contains(&env.gui_config_file().unwrap().display().to_string())),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("State: ") && l.ends_with("state.json")),
        "{lines:?}"
    );
    // Every path is inside the sandbox home.
    for line in &lines[3..] {
        assert!(line.contains(&world.home().display().to_string()), "{line}");
    }
}
