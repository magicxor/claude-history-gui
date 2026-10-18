//! The single menu tree of every window (DESIGN §8.10). WP0 → WP6.
//!
//! [`menu_tree`] is the source for `cx.set_menus` (macOS native bar, with a
//! leading app menu), `GlobalState::set_app_menus` (the in-window `AppMenuBar`
//! on Windows/Linux) and the command palette. There are no per-window
//! variants: an item is enabled when its action has a handler on the active
//! window's dispatch path. The tree is rebuilt (and every open `AppMenuBar`
//! reloaded) after theme changes, keymap changes and window activation.
//! WP6 adds the remaining check marks (viewer toggles, list mode/scope, focus
//! viewer mode) from the active window.

use claude_history_core::browser::ListSearchMode;
use claude_history_core::export::ExportFormat;
use claude_history_core::theme::ThemePreference;
use claude_history_core::view::ToolDisplayMode;
use gpui_kit::component::GlobalState;
use gpui_kit::component::menu::AppMenuBar;
use gpui_kit::{AnyWindowHandle, App, Context, Entity, Global, Menu, MenuItem, WeakEntity, Window};

use crate::actions::*;
use crate::globals::ActivePane;
use crate::list::ListPane;
use crate::model::PaneFocus;
use crate::single_file::SingleFileWindow;
use crate::viewer::ViewerPane;

/// The menu names, in order (Windows/Linux menu bar).
pub const MENU_NAMES: [&str; 6] = ["File", "Edit", "View", "Session", "Tools", "Help"];
/// The macOS application menu name.
pub const APP_MENU_NAME: &str = "Claude History";

const MAC: bool = cfg!(target_os = "macos");

/// Check marks of the tree, from the active window and the app state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MenuChecks {
    pub theme: ThemePreference,
    pub list_collapsed: bool,
    /// 0 = Summary, 1 = Truncated, 2 = Full (None: no conversation open).
    pub tool_display: Option<u8>,
    pub show_thinking: bool,
    pub show_timing: bool,
    pub semantic_mode: bool,
    pub workspace_scope: bool,
    pub picker: bool,
    /// The active conversation's header session ID differs from its file
    /// stem (Pi/OMP): Session → Copy header session ID is listed (A-042;
    /// hidden for Claude conversations and without a target).
    pub header_session_id: bool,
}

/// The checks for the current state: theme and picker from the app state,
/// view toggles from the active window's viewer, mode and scope from the
/// list (main window), focus viewer mode from `ui.list_collapsed`.
pub fn current_checks(cx: &App) -> MenuChecks {
    let mut checks = MenuChecks {
        theme: crate::theme_tokens::preference(cx),
        ..MenuChecks::default()
    };
    if let Some(state) = cx.try_global::<crate::globals::AppState>() {
        checks.picker = state.is_picker();
        checks.list_collapsed = state.ui.list_collapsed || state.list_auto_collapsed;
    }
    let (window, viewer, list) = active_panes(cx);
    checks.header_session_id = active_target_has_header_id(cx, window, &viewer, &list);
    if let Some(prefs) = viewer.and_then(|viewer| viewer.read(cx).prefs()) {
        checks.tool_display = Some(match prefs.tool_display {
            ToolDisplayMode::Hidden => 0,
            ToolDisplayMode::Truncated => 1,
            ToolDisplayMode::Full => 2,
        });
        checks.show_thinking = prefs.show_thinking;
        checks.show_timing = prefs.show_timing;
    }
    if let Some(list) = list {
        let list = list.read(cx);
        checks.semantic_mode = list.mode() == ListSearchMode::Semantic;
        checks.workspace_scope = list.scope_is_workspace();
    }
    checks
}

/// The panes the menus reflect.
type ActivePanes = (
    Option<AnyWindowHandle>,
    Option<Entity<ViewerPane>>,
    Option<Entity<ListPane>>,
);

/// The window the menus reflect (the active window, else the last one that
/// had focus, else the main window), its viewer and, in the main window, the
/// list. Single-file windows have no list.
fn active_panes(cx: &App) -> ActivePanes {
    let main = crate::globals::main_panes(cx);
    let window = cx
        .active_window()
        .or_else(|| {
            cx.try_global::<ActivePane>()
                .and_then(|active| active.window)
        })
        .or(main.window);
    if let Some(window) = window
        && Some(window) != main.window
        && let Some(viewer) = cx
            .try_global::<SingleFileViewers>()
            .and_then(|viewers| viewers.of(window))
    {
        return (Some(window), Some(viewer), None);
    }
    (window, main.viewer(), main.list())
}

/// Whether the conversation the Session menu acts on (the active region's
/// target: the list's selection, the Deep search hit, the agent panel's
/// result set, else the viewer's conversation) has a header session ID
/// that differs from its file stem (A-042).
fn active_target_has_header_id(
    cx: &App,
    window: Option<AnyWindowHandle>,
    viewer: &Option<Entity<ViewerPane>>,
    list: &Option<Entity<ListPane>>,
) -> bool {
    let pane = window
        .map(|window| ActivePane::of_window(cx, window))
        .unwrap_or_default();
    let target = match pane {
        PaneFocus::List | PaneFocus::ListToolbar if list.is_some() => list
            .as_ref()
            .and_then(|list| list.read(cx).selected_target()),
        PaneFocus::DeepSearch => crate::globals::main_panes(cx)
            .deep()
            .and_then(|deep| deep.read(cx).selected_hit_target(cx)),
        PaneFocus::AgentPanel => viewer
            .as_ref()
            .and_then(|viewer| viewer.read(cx).agent_panel().read(cx).selected_target()),
        _ => viewer
            .as_ref()
            .and_then(|viewer| viewer.read(cx).target().cloned()),
    };
    target.is_some_and(|target| target.header_id_differs())
}

/// The viewers of the open single-file windows (their root views cannot be
/// read while they are being updated, so the entities are registered).
#[derive(Default)]
struct SingleFileViewers(Vec<(AnyWindowHandle, WeakEntity<ViewerPane>)>);

impl Global for SingleFileViewers {}

impl SingleFileViewers {
    fn of(&self, window: AnyWindowHandle) -> Option<Entity<ViewerPane>> {
        self.0
            .iter()
            .find(|(handle, _)| *handle == window)
            .and_then(|(_, viewer)| viewer.upgrade())
    }
}

/// The checks the installed tree was built from (change detection for
/// [`refresh_if_changed`]).
#[derive(Default)]
struct InstalledChecks(Option<MenuChecks>);

impl Global for InstalledChecks {}

/// Rebuild the tree when the checks differ from the installed ones (called
/// whenever a viewer or list notifies; cheap when nothing changed).
pub fn refresh_if_changed(cx: &mut App) {
    let timer = crate::globals::BusyTimer::start();
    refresh_if_changed_inner(cx);
    crate::globals::record_sub_span(cx, "menus.refresh", timer.stop());
}

fn refresh_if_changed_inner(cx: &mut App) {
    let checks = current_checks(cx);
    let installed = cx
        .try_global::<InstalledChecks>()
        .and_then(|installed| installed.0);
    if installed != Some(checks) {
        rebuild(cx);
    }
}

/// Keep the check marks current without the cooperation of the panes: every
/// viewer and list that notifies triggers [`refresh_if_changed`]; a
/// single-file window registers its viewer and rebuilds the tree when it is
/// activated (the main window does that itself). Idempotent.
fn install_observers(cx: &mut App) {
    if cx.try_global::<InstalledChecks>().is_some() {
        return;
    }
    cx.set_global(InstalledChecks::default());
    cx.set_global(SingleFileViewers::default());
    cx.observe_new(|_: &mut ViewerPane, _, cx: &mut Context<ViewerPane>| {
        let entity = cx.entity();
        let app: &mut App = cx;
        app.observe(&entity, |_, cx| {
            refresh_if_changed(cx);
        })
        .detach();
    })
    .detach();
    cx.observe_new(|_: &mut ListPane, _, cx: &mut Context<ListPane>| {
        let entity = cx.entity();
        let app: &mut App = cx;
        app.observe(&entity, |_, cx| {
            refresh_if_changed(cx);
        })
        .detach();
    })
    .detach();
    cx.observe_new(
        |single: &mut SingleFileWindow, window, cx: &mut Context<SingleFileWindow>| {
            let Some(window) = window else {
                return;
            };
            let handle = window.window_handle();
            let viewer = single.viewer().downgrade();
            let viewers = cx.global_mut::<SingleFileViewers>();
            viewers.0.retain(|(_, viewer)| viewer.upgrade().is_some());
            viewers.0.push((handle, viewer));
            cx.observe_window_activation(window, |_, window, cx| {
                if window.is_window_active() {
                    rebuild(cx);
                }
            })
            .detach();
        },
    )
    .detach();
}

fn item<A: gpui_kit::Action>(name: &str, action: A) -> MenuItem {
    MenuItem::action(name.to_string(), action)
}

/// The six menus of every window (File, Edit, View, Session, Tools, Help).
/// On macOS, Settings… and Quit live in the app menu ([`app_menu`]) instead.
pub fn menu_tree(cx: &App) -> Vec<Menu> {
    build_tree(&current_checks(cx))
}

/// The tree the command palette lists (§8.9, PD12): [`menu_tree`] plus, on
/// macOS, the app menu's Settings… (Tools) and Quit (File), so every command
/// is in the palette on every platform.
pub fn palette_tree(cx: &App) -> Vec<Menu> {
    build_tree_with(&current_checks(cx), true)
}

/// [`menu_tree`] for explicit checks (tests).
pub fn build_tree(checks: &MenuChecks) -> Vec<Menu> {
    build_tree_with(checks, !MAC)
}

/// The tree; `app_menu_items` puts Settings… under Tools and Quit under File
/// (Windows/Linux menu bar, every palette).
pub fn build_tree_with(checks: &MenuChecks, app_menu_items: bool) -> Vec<Menu> {
    let copy_formats: Vec<MenuItem> = ExportFormat::ALL
        .iter()
        .map(|format| {
            let label = format.label();
            match format {
                ExportFormat::Ledger => item(label, CopyAsLedger),
                ExportFormat::Plain => item(label, CopyAsPlain),
                ExportFormat::Markdown => item(label, CopyAsMarkdown),
                ExportFormat::Jsonl => item(label, CopyAsJsonl),
            }
        })
        .collect();

    let mut file = vec![
        item("Open JSONL file…", OpenJsonlFile),
        item("Show conversations", ShowConversations),
        MenuItem::separator(),
        item("Choose workspace…", ChooseWorkspace),
        item("Clear workspace", ClearWorkspace),
        MenuItem::separator(),
        item("Export…", ExportConversation),
        MenuItem::separator(),
        item("Close window", CloseWindow),
    ];
    if app_menu_items {
        file.push(item("Quit", Quit));
    }

    let edit = vec![
        item("Copy", EditCopy),
        item("Select All", SelectAllTranscript),
        MenuItem::separator(),
        item("Find in conversation", StartSearch),
        item("Find in all conversations…", FocusDeepSearch),
    ];

    let tools_display = [("Summary", 0u8), ("Truncated", 1u8), ("Full", 2u8)]
        .into_iter()
        .map(|(name, value)| {
            let checked = checks.tool_display == Some(value);
            match value {
                0 => item(name, SetToolsSummary).checked(checked),
                1 => item(name, SetToolsTruncated).checked(checked),
                _ => item(name, SetToolsFull).checked(checked),
            }
        });

    let view = vec![
        item("Conversations", FocusConversations),
        item("Deep search", FocusDeepSearch),
        item("Viewer", FocusViewer),
        item("Focus viewer", ToggleListPane).checked(checks.list_collapsed),
        MenuItem::separator(),
        MenuItem::submenu(Menu::new("Tool display").items(tools_display)),
        item("Show thinking", ToggleThinking).checked(checks.show_thinking),
        item("Show timestamps", ToggleTiming).checked(checks.show_timing),
        MenuItem::separator(),
        MenuItem::submenu(Menu::new("Search mode").items([
            item("Lexical", SetModeLexical).checked(!checks.semantic_mode),
            item("Semantic", SetModeSemantic).checked(checks.semantic_mode),
        ])),
        MenuItem::submenu(Menu::new("Scope").items([
            item("All", SetScopeAll).checked(!checks.workspace_scope),
            item("Workspace", SetScopeWorkspace).checked(checks.workspace_scope),
        ])),
        MenuItem::separator(),
        MenuItem::submenu(Menu::new("Theme").items([
            item("System", SetThemeSystem).checked(checks.theme == ThemePreference::System),
            item("Light", SetThemeLight).checked(checks.theme == ThemePreference::Light),
            item("Dark", SetThemeDark).checked(checks.theme == ThemePreference::Dark),
        ])),
        MenuItem::separator(),
        item("Refresh", Refresh),
    ];

    let mut session = vec![
        item("Open", OpenConversation),
        item("Resume", Resume),
        item("Fork", Fork),
        item("Rename…", Rename),
        item("Delete…", Delete),
        MenuItem::separator(),
        item("Export…", ExportConversation),
        MenuItem::submenu(Menu::new("Copy to clipboard").items(copy_formats)),
        item("Copy message", CopyFocusedMessage),
        item("Copy path", CopyPath),
        item("Copy session ID", CopySessionId),
    ];
    if checks.header_session_id {
        session.push(item("Copy header session ID", CopyHeaderSessionId));
    }
    session.extend([
        item("Show file path", ShowPath),
        item("Reveal in file manager", RevealInFileManager),
        MenuItem::separator(),
        item("Search within conversation…", SearchWithinConversation),
        item("Outline", ToggleOutline),
        item("Read range…", ReadRange),
        item("Semantic result details", ShowSemanticDetails),
        MenuItem::separator(),
        item(
            if checks.picker {
                "Select and exit"
            } else {
                "Print and exit"
            },
            SelectAndExit,
        ),
    ]);

    let mut tools = vec![
        item("Delete empty transcripts…", DeleteEmptyTranscripts),
        item("Generate semantic cache…", GenerateSemanticCache),
        item("Clear semantic cache…", ClearSemanticCache),
        MenuItem::separator(),
        item("Debug search…", DebugSearch),
        item("Semantic debug search…", SemanticDebugSearch),
        item("Debug log…", ShowDebugLog),
        MenuItem::separator(),
        item("Show projects directory", ShowProjectsDirectory),
        item("Delete conversation by session ID…", DeleteSessionById),
        item("Open JSONL file…", OpenJsonlFile),
        MenuItem::separator(),
        item("Check for updates…", CheckForUpdates),
    ];
    if app_menu_items {
        tools.push(item("Settings…", OpenSettings));
    }

    let help = vec![
        item("Command palette…", CommandPalette),
        item("Keyboard shortcuts", ShowShortcuts),
        MenuItem::separator(),
        item("Check for updates…", CheckForUpdates),
        item("About Claude History GUI", ShowAbout),
    ];

    vec![
        Menu::new("File").items(file),
        Menu::new("Edit").items(edit),
        Menu::new("View").items(view),
        Menu::new("Session").items(session),
        Menu::new("Tools").items(tools),
        Menu::new("Help").items(help),
    ]
}

/// The macOS application menu (About, Check for updates…, Settings…, Quit).
pub fn app_menu() -> Menu {
    Menu::new(APP_MENU_NAME).items([
        item("About Claude History GUI", ShowAbout),
        item("Check for updates…", CheckForUpdates),
        MenuItem::separator(),
        item("Settings…", OpenSettings),
        MenuItem::separator(),
        item("Quit", Quit),
    ])
}

/// Open in-window menu bars (reloaded on [`rebuild`]).
#[derive(Default)]
pub struct MenuBars(pub Vec<WeakEntity<AppMenuBar>>);

impl Global for MenuBars {}

/// Track an `AppMenuBar` so [`rebuild`] reloads it.
pub fn register_menu_bar(bar: &Entity<AppMenuBar>, cx: &mut App) {
    let bars = cx.default_global::<MenuBars>();
    bars.0.retain(|bar| bar.upgrade().is_some());
    bars.0.push(bar.downgrade());
}

/// Install the tree (§7.3 step 8; key bindings are already registered so the
/// native menu caches the right accelerators).
pub fn install(cx: &mut App) {
    install_observers(cx);
    let checks = current_checks(cx);
    cx.set_global(InstalledChecks(Some(checks)));
    let mut native = Vec::with_capacity(7);
    if MAC {
        native.push(app_menu());
    }
    native.extend(build_tree(&checks));
    cx.set_menus(native);
    let tree = build_tree(&checks);
    let actions = collect_actions(&tree);
    let disabled = cx
        .try_global::<Availability>()
        .map(|availability| availability.disabled.clone())
        .unwrap_or_default();
    cx.set_global(Availability { disabled, actions });
    let disabled = &cx.global::<Availability>().disabled.clone();
    let owned = tree
        .into_iter()
        .map(|menu| mark_disabled(menu, disabled).owned())
        .collect();
    GlobalState::global_mut(cx).set_app_menus(owned);
}

/// The unavailable commands of the in-window menu bar (S-033). gpui-component's
/// `PopupMenu` greys an item only by its `disabled` flag and never asks the
/// dispatch tree, so the flags are set from the active window's focus path
/// ([`sync_availability`]); the macOS native bar validates by itself.
#[derive(Default)]
struct Availability {
    /// Action names without a handler on the active window's focus path.
    disabled: std::collections::BTreeSet<&'static str>,
    /// One instance of every command in the installed tree.
    actions: Vec<Box<dyn gpui_kit::Action>>,
}

impl Global for Availability {}

/// The windows [`sync_availability`] has seen in an active render (their
/// first one is deferred to the next frame).
#[derive(Default)]
struct DrawnWindows(std::collections::HashSet<AnyWindowHandle>);

impl Global for DrawnWindows {}

fn collect_actions(tree: &[Menu]) -> Vec<Box<dyn gpui_kit::Action>> {
    fn walk(menu: &Menu, out: &mut Vec<Box<dyn gpui_kit::Action>>) {
        for entry in &menu.items {
            match entry {
                MenuItem::Action { action, .. } => out.push(action.boxed_clone()),
                MenuItem::Submenu(sub) => walk(sub, out),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    for menu in tree {
        walk(menu, &mut out);
    }
    out
}

/// `menu` with every command in `disabled` greyed out.
pub fn mark_disabled(mut menu: Menu, disabled: &std::collections::BTreeSet<&'static str>) -> Menu {
    menu.items = menu
        .items
        .into_iter()
        .map(|entry| match entry {
            MenuItem::Action { ref action, .. } => {
                let off = disabled.contains(action.name());
                entry.disabled(off)
            }
            MenuItem::Submenu(sub) => MenuItem::Submenu(mark_disabled(sub, disabled)),
            other => other,
        })
        .collect();
    menu
}

/// The commands of the installed tree that `window` cannot handle now.
fn unavailable(window: &Window, cx: &App) -> std::collections::BTreeSet<&'static str> {
    cx.try_global::<Availability>()
        .map(|availability| {
            availability
                .actions
                .iter()
                .filter(|action| !window.is_action_available(action.as_ref(), cx))
                .map(|action| action.name())
                .collect()
        })
        .unwrap_or_default()
}

/// Keep the in-window menu bar's disabled flags in step with the active
/// window (called from every window's title bar while it renders). Skipped
/// while a popup owns the focus (an open menu), and in inactive windows.
pub fn sync_availability(window: &Window, cx: &mut App) {
    if MAC || !window.is_window_active() || GlobalState::is_in_deferred_context(cx) {
        return;
    }
    if cx
        .default_global::<DrawnWindows>()
        .0
        .insert(window.window_handle())
    {
        // The first active render of this window may run inside its very
        // first draw: the rendered frame has no dispatch tree yet, and gpui's
        // `is_action_available` indexes its root node (an index-out-of-bounds
        // panic at startup). Sync once a frame has been drawn.
        window.on_next_frame(|window, cx| sync_availability(window, cx));
        return;
    }
    let now = unavailable(window, cx);
    let Some(availability) = cx.try_global::<Availability>() else {
        return;
    };
    if availability.disabled == now {
        return;
    }
    cx.global_mut::<Availability>().disabled = now;
    cx.defer(rebuild);
}

/// Rebuild the tree and reload every open `AppMenuBar`.
pub fn rebuild(cx: &mut App) {
    let timer = crate::globals::BusyTimer::start();
    install(cx);
    let bars: Vec<Entity<AppMenuBar>> = cx
        .try_global::<MenuBars>()
        .map(|bars| bars.0.iter().filter_map(WeakEntity::upgrade).collect())
        .unwrap_or_default();
    for bar in bars {
        bar.update(cx, |bar, cx| bar.reload(cx));
    }
    let measured = timer.stop();
    crate::globals::record_sub_span(cx, "menus.rebuild", measured);
    let stats = cx.default_global::<RebuildStats>();
    stats.count += 1;
    stats.last = measured;
}

/// How often the menus were rebuilt and what the last rebuild cost (QA
/// probes `menus.rebuilds`, `menus.rebuild_ms_last`; integrator, Phase C).
#[derive(Clone, Copy, Debug, Default)]
pub struct RebuildStats {
    pub count: u64,
    pub last: crate::globals::BusyMeasure,
}

impl Global for RebuildStats {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_has_the_six_menus_in_order() {
        let tree = build_tree(&MenuChecks::default());
        let names: Vec<&str> = tree.iter().map(|menu| menu.name.as_ref()).collect();
        assert_eq!(names, MENU_NAMES);
    }

    /// V-088: Edit lists Select All right after Copy.
    #[test]
    fn the_edit_menu_lists_select_all() {
        let tree = build_tree(&MenuChecks::default());
        let edit = tree
            .iter()
            .find(|menu| menu.name.as_ref() == "Edit")
            .unwrap();
        let labels: Vec<&str> = edit
            .items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { name, .. } => Some(name.as_ref()),
                _ => None,
            })
            .collect();
        assert_eq!(&labels[..2], ["Copy", "Select All"], "{labels:?}");
    }

    #[test]
    fn the_palette_tree_has_settings_and_quit_on_every_platform() {
        let tree = build_tree_with(&MenuChecks::default(), true);
        let names: Vec<String> = tree
            .iter()
            .flat_map(|menu| menu.items.iter())
            .filter_map(|item| match item {
                MenuItem::Action { name, .. } => Some(name.to_string()),
                _ => None,
            })
            .collect();
        assert!(names.contains(&"Settings…".to_string()));
        assert!(names.contains(&"Quit".to_string()));
    }

    /// A-042 (integrator, WP3 request): Copy header session ID is listed
    /// only when the active conversation's header ID differs (Pi/OMP).
    #[test]
    fn copy_header_session_id_is_listed_only_when_the_header_id_differs() {
        let listed = |checks: &MenuChecks| {
            build_tree(checks)
                .iter()
                .find(|menu| menu.name.as_ref() == "Session")
                .unwrap()
                .items
                .iter()
                .any(|item| {
                    matches!(item, MenuItem::Action { name, .. } if name.as_ref() == "Copy header session ID")
                })
        };
        assert!(!listed(&MenuChecks::default()));
        assert!(listed(&MenuChecks {
            header_session_id: true,
            ..MenuChecks::default()
        }));
    }

    #[test]
    fn picker_mode_renames_print_and_exit() {
        let tree = build_tree(&MenuChecks {
            picker: true,
            ..MenuChecks::default()
        });
        let session = tree
            .iter()
            .find(|menu| menu.name.as_ref() == "Session")
            .unwrap();
        let has_select = session.items.iter().any(|item| {
            matches!(item, MenuItem::Action { name, .. } if name.as_ref() == "Select and exit")
        });
        assert!(has_select);
    }
}
