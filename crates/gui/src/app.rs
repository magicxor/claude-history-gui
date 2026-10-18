//! Application startup (DESIGN §3, §7.3, §11.1). Frozen: written by WP0,
//! afterwards changed only by the integrator.
//!
//! [`init`] only calls owner functions, in a fixed order; the logic lives in
//! the owners' modules. Order (§7.3, with the plain-value globals the debug
//! sink needs set first):
//!
//! 0. `Services`, `DebugBufferHandle` (no dependencies); recording services
//!    (tests, QA) also get the clipboard guard (`services::ClipboardGuard`);
//! 1. `debug_sink::install` (WP7);
//! 2. `startup::load_config` (WP7) — config.toml and gui.toml independently;
//! 3. `persist::load` (WP6);
//! 4. `startup::resolve_workspace` (WP7, PD3);
//! 5. `startup::resolve_initial_view` (WP7, PD17);
//! 6. `AppState`, `SemanticGate`, `MainPanes`, `ActivePane`, `OpenTargets`,
//!    `LastToast`, `CorpusBus`; `ops::resume::TerminalVar` (`$TERMINAL` as
//!    `main` read it);
//! 7. `embedder::install` (WP7);
//! 8. `keymap::install_all`, `menus::install`, `palette::install` (WP6);
//! 9. `theme_tokens::apply_startup_theme` (WP6);
//! 10. the global `Quit` and (Windows/Linux, for `alt-f4`) `CloseWindow`
//!     handlers (work while a dialog or nothing has focus). Menu enabled
//!     state comes from element listeners (`Window::is_action_available`
//!     ignores global ones): both window roots also handle `Quit`, only
//!     single-file roots handle `CloseWindow`, and macOS registers no global
//!     `CloseWindow` (its native menus count global listeners, which would
//!     enable File → Close window in the main window).

use std::sync::Arc;

use claude_history_core::HistoryEnv;
use gpui_kit::{AnyWindowHandle, App, QuitMode};

use crate::actions::{CloseWindow, Quit};
use crate::globals::{
    ActivePane, AppState, CorpusBus, DebugBuffer, DebugBufferHandle, LastToast, MainPanes,
    OpenTargets, SemanticGate, main_panes,
};
use crate::model::LaunchContext;
use crate::services::Services;
use crate::{
    debug_sink, embedder, exit, keymap, menus, palette, persist, single_file, theme_tokens,
    workspace,
};

/// The app identity (Windows AppUserModelID, taskbar grouping).
pub const APP_ID: &str = "io.github.magicxor.claude-history-gui";
/// The app's display name.
pub const APP_NAME: &str = "Claude History";

/// Run the GUI until it quits (called by `main` through `lib::run`) with the
/// services of this launch (`Services::for_launch`: recording for a QA run,
/// else the real ones).
pub fn run(launch: LaunchContext, env: HistoryEnv) {
    let services = Services::for_launch(&launch);
    run_with_services(launch, env, services);
}

/// [`run`] with explicit services (QA runner, embedding tests of `main`).
pub fn run_with_services(launch: LaunchContext, env: HistoryEnv, services: Services) {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(move |cx| {
            gpui_kit::init(cx);
            cx.set_app_identity(APP_ID, APP_NAME);
            init(cx, launch, env, services);
            open_first_window(cx);
            cx.activate(true);
        });
}

/// Install every global, the keymap, menus and theme (§7.3).
pub fn init(cx: &mut App, launch: LaunchContext, env: HistoryEnv, services: Services) {
    // 0. Plain-value globals the debug sink needs.
    install_services(cx, services);
    cx.set_global(DebugBufferHandle(Arc::new(DebugBuffer::default())));
    // 1. Debug sink.
    debug_sink::install(cx, launch.args.debug);
    // 2.–6. Configuration, persisted state, workspace, initial view, app state.
    install_state(cx, launch, env);
    // 7. The shared embedder.
    embedder::install(cx);
    // 8. Keymap before menus (native menus cache accelerators), then the palette.
    keymap::install_all(cx);
    menus::install(cx);
    palette::install(cx);
    // 9. Theme.
    theme_tokens::apply_startup_theme(cx);
    // 10. Quit and close work while a dialog or nothing has focus.
    install_global_handlers(cx);
}

/// Set the `Services` global; recording services also take over gpui-kit's
/// clipboard actions in every window (no real clipboard in tests and QA).
fn install_services(cx: &mut App, services: Services) {
    if services.is_recording() {
        crate::services::install_clipboard_guard(cx);
    }
    cx.set_global(services);
}

/// The global `Quit` handler, and the context-free `alt-f4` `CloseWindow`
/// handler on Windows/Linux (§8.3).
fn install_global_handlers(cx: &mut App) {
    cx.on_action(|_: &Quit, cx: &mut App| exit::quit(cx));
    if !cfg!(target_os = "macos") {
        cx.on_action(|_: &CloseWindow, cx: &mut App| close_active_window(cx));
    }
}

/// Steps 2–6 of [`init`]: both config files (independently), the persisted
/// UI state, the workspace, the initial view, then `AppState` and the other
/// app globals.
fn install_state(cx: &mut App, launch: LaunchContext, env: HistoryEnv) {
    let (config, config_errors) = workspace::startup::load_config(&env);
    let ui = persist::load(&env);
    let workspace_scope = workspace::startup::resolve_workspace(&launch, &env, &config.gui, &ui);
    let settings = launch.args.settings(&config.effective);
    // `$TERMINAL` as `main` read it, for the launcher detection (§11.10 step 6).
    crate::ops::resume::set_terminal_var(cx, launch.terminal_var.clone());
    let (view_defaults, list_mode) =
        workspace::startup::resolve_initial_view(&launch, &config, &settings, &ui);
    let mut state = AppState {
        env: Arc::new(env),
        config,
        config_errors,
        launch,
        settings,
        workspace: None,
        view_defaults,
        list_mode,
        ui,
        list_auto_collapsed: false,
        revision: 0,
    };
    state.set_workspace(workspace_scope);
    cx.set_global(state);
    cx.set_global(SemanticGate::default());
    cx.set_global(MainPanes::default());
    cx.set_global(ActivePane::default());
    cx.set_global(OpenTargets::default());
    cx.set_global(LastToast::default());
    CorpusBus::install(cx);
}

/// `alt-f4` / CloseWindow with no window root handling it: closing the main
/// window quits (PD6); another window just closes.
fn close_active_window(cx: &mut App) {
    let main = main_panes(cx).window;
    let target = cx.active_window().or(main);
    match target {
        Some(window) if Some(window) == main => exit::quit(cx),
        Some(window) => {
            window
                .update(cx, |_, window, _| window.remove_window())
                .ok();
        }
        None => exit::quit(cx),
    }
}

/// Open the first window: a single-file window for `[FILE]`, else the main
/// window; after its first frame show config errors, run the startup tool,
/// start the QA runner (feature `qa`) and the login-shell PATH probe.
pub fn open_first_window(cx: &mut App) -> AnyWindowHandle {
    let launch = crate::globals::app_state(cx).launch.clone();
    let handle = match launch.file.clone() {
        Some(file) => single_file::open_window(file, cx),
        None => workspace::open_main_window(cx),
    };
    handle
        .update(cx, move |_, window, _| {
            window.on_next_frame(move |window, cx| {
                workspace::startup::show_config_errors(window, cx);
                if let Some(tool) = launch.startup_tool.clone() {
                    workspace::startup::run_startup_tool(tool, window, cx);
                }
                #[cfg(feature = "qa")]
                if let Some(qa) = launch.qa.clone() {
                    crate::qa::runner::start(qa, window, cx);
                }
                workspace::startup::start_login_shell_probe(cx);
            });
        })
        .ok();
    handle
}

/// The test version of [`init`] (§15.2): `RecordingServices`, state.json
/// writes disabled, a refusing embedder, the world's sandbox environment.
#[cfg(test)]
pub fn init_for_test(
    cx: &mut App,
    world: &crate::test_support::TestWorld,
    launch: LaunchContext,
    recording: Arc<crate::services::RecordingServices>,
) {
    let first_init = cx
        .try_global::<crate::test_support::KitInitialized>()
        .is_none();
    if first_init {
        gpui_kit::init(cx);
        cx.set_global(crate::test_support::KitInitialized);
    }
    cx.set_global(persist::PersistWrites(false));
    let services = Services::recording(recording);
    // The debug sink and the core theme are process-wide: tests that need
    // them install them under `test_support::global_lock`.
    install_services(cx, services);
    cx.set_global(DebugBufferHandle(Arc::new(DebugBuffer::default())));
    install_state(cx, launch, world.env.clone());
    embedder::install_fake(
        cx,
        embedder::refusing_factory(embedder::NOT_AVAILABLE_IN_TESTS),
    );
    if first_init {
        keymap::install_all(cx);
        install_global_handlers(cx);
    }
    menus::install(cx);
    palette::install(cx);
    // The world's `[gui] theme` applies to this app (gpui-kit theme and the
    // preference); core's process-wide palette stays untouched (tests that
    // need it call `theme_tokens::apply_startup_theme` under `global_lock`).
    theme_tokens::apply_startup_theme_to_app(cx);
}
