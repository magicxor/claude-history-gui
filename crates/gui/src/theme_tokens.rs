//! App color roles and theme switching (DESIGN §5 intro, PD9, §11.19).
//! WP0 → WP6.
//!
//! Chrome colors come from `cx.theme()`. Content colors (ledger labels, list
//! fields, diff, code) come from [`roles`], which takes a gpui-kit token
//! wherever one exists and core's palette (`core::theme`) only for roles
//! gpui-kit lacks. This is the only file that converts raw colors; call sites
//! never use color literals.

use claude_history_core::display_format::Recency;
use claude_history_core::theme::{self as core_theme, Rgb, ThemeKind, ThemePreference};
use claude_history_core::view::LabelRole;
use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};
use gpui_kit::{App, Global, Hsla, Window, WindowAppearance, hsla, rgb};

/// Every content color role of the app, for the active theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Roles {
    pub kind: ThemeKind,
    // Kit tokens.
    pub background: Hsla,
    pub foreground: Hsla,
    pub muted_foreground: Hsla,
    pub border: Hsla,
    pub sidebar: Hsla,
    pub selection: Hsla,
    pub list_active: Hsla,
    pub list_hover: Hsla,
    pub code_background: Hsla,
    pub link: Hsla,
    pub warning: Hsla,
    pub warning_foreground: Hsla,
    pub danger: Hsla,
    pub success: Hsla,
    pub info: Hsla,
    // Text roles.
    pub text_primary: Hsla,
    pub text_secondary: Hsla,
    pub text_muted: Hsla,
    /// Accent text (assistant labels, search prompt `❯`, scores, highlights).
    pub accent: Hsla,
    pub accent_dim: Hsla,
    // Viewer roles (core palette).
    pub thinking: Hsla,
    pub tool_text: Hsla,
    pub heading: Hsla,
    pub code: Hsla,
    pub diff_add: Hsla,
    pub diff_remove: Hsla,
    pub quote: Hsla,
    pub dimmed: Hsla,
    /// Focused-message bar (accent at 60%).
    pub focus_mark: Hsla,
    /// Focused-message background tint.
    pub focus_tint: Hsla,
    pub separator: Hsla,
    // Search matches.
    pub match_background: Hsla,
    pub match_foreground: Hsla,
    pub current_match_background: Hsla,
    pub current_match_foreground: Hsla,
    // List roles (core palette).
    pub custom_title: Hsla,
    pub custom_title_highlight: Hsla,
    pub summary: Hsla,
    pub summary_highlight: Hsla,
    pub model: Hsla,
    pub duration: Hsla,
    pub msg_count: Hsla,
    pub preview: Hsla,
    pub context_base: Hsla,
    pub context_highlight: Hsla,
    pub dot_separator: Hsla,
    pub header_summary: Hsla,
    pub source_label: Hsla,
    pub recency_now: Hsla,
    pub recency_minutes: Hsla,
    pub recency_hours: Hsla,
    pub recency_days: Hsla,
    pub recency_old: Hsla,
}

impl Roles {
    /// The color of a ledger label role.
    pub fn label(&self, role: LabelRole) -> Hsla {
        match role {
            LabelRole::TextPrimary => self.text_primary,
            LabelRole::Accent => self.accent,
            LabelRole::AccentDim => self.accent_dim,
            LabelRole::TextSecondary => self.text_secondary,
        }
    }

    /// The color of a relative timestamp.
    pub fn recency(&self, recency: Recency) -> Hsla {
        match recency {
            Recency::Now => self.recency_now,
            Recency::Minutes => self.recency_minutes,
            Recency::Hours => self.recency_hours,
            Recency::Days => self.recency_days,
            Recency::Old => self.recency_old,
        }
    }
}

fn color(value: Rgb) -> Hsla {
    let (r, g, b) = value;
    rgb((u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)).into()
}

/// A color that is data from core rather than a palette role (the syntect
/// token colors of core's syntax themes, V-037) as a theme color. Raw colors
/// are converted only in this file (X-019).
pub fn data_color(value: Rgb) -> Hsla {
    color(value)
}

/// The legible text color on a search-match background (V-075: the original
/// draws black on yellow; a light foreground on amber is unreadable):
/// `foreground` while it reaches [`MIN_CONTRAST`] on `background` — a
/// translucent background composited over `backdrop`, as drawn — else
/// near-black or white, whichever contrasts more.
///
/// Integrator, Phase C: the choice used to compare HSL lightness, which
/// ignores the alpha of the translucent match fill and the luminance of
/// yellow; it drew white on the pale light-theme fill and on the dark theme's
/// yellow current match (screenshot review). It is WCAG contrast now.
pub fn legible_on(background: Hsla, foreground: Hsla, backdrop: Hsla) -> Hsla {
    let ratio = |color: Hsla| contrast_ratio(color, background, backdrop);
    if ratio(foreground) >= MIN_CONTRAST {
        return foreground;
    }
    let (near_black, white) = (hsla(0., 0., 0.05, 1.), hsla(0., 0., 1., 1.));
    if ratio(near_black) >= ratio(white) {
        near_black
    } else {
        white
    }
}

/// The roles of the active theme (kit theme + core palette of the same kind),
/// every text role adjusted to a contrast of at least [`MIN_CONTRAST`] on the
/// backgrounds it is drawn on (X-039). Cached per theme.
pub fn roles(cx: &App) -> Roles {
    let theme = cx.theme();
    let key = [
        theme.is_dark() as u32,
        theme.background.l.to_bits(),
        theme.foreground.l.to_bits(),
        theme.muted_foreground.l.to_bits(),
        theme.sidebar.l.to_bits(),
        theme.list_active.l.to_bits(),
        theme.list_hover.l.to_bits(),
        theme.muted.l.to_bits(),
        theme.link.l.to_bits(),
        theme.warning.l.to_bits(),
        theme.warning_foreground.l.to_bits(),
        theme.danger.l.to_bits(),
        theme.success.l.to_bits(),
        theme.info.l.to_bits(),
        theme.selection.l.to_bits(),
        theme.background.h.to_bits(),
        theme.sidebar.h.to_bits(),
    ];
    ROLES_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some((cached_key, roles)) = cache.as_ref()
            && *cached_key == key
        {
            return *roles;
        }
        let roles = compute_roles(cx);
        *cache = Some((key, roles));
        roles
    })
}

thread_local! {
    static ROLES_CACHE: std::cell::RefCell<Option<([u32; 17], Roles)>> =
        const { std::cell::RefCell::new(None) };
}

fn compute_roles(cx: &App) -> Roles {
    let mut roles = raw_roles(cx);
    adjust_roles(&mut roles);
    roles
}

fn raw_roles(cx: &App) -> Roles {
    let theme = cx.theme();
    let kind = if theme.is_dark() {
        ThemeKind::Dark
    } else {
        ThemeKind::Light
    };
    let palette = core_theme::theme_for(kind);
    let accent = color(palette.accent);
    Roles {
        kind,
        background: theme.background,
        foreground: theme.foreground,
        muted_foreground: theme.muted_foreground,
        border: theme.border,
        sidebar: theme.sidebar,
        selection: theme.selection,
        list_active: theme.list_active,
        list_hover: theme.list_hover,
        code_background: theme.muted,
        link: theme.link,
        warning: theme.warning,
        warning_foreground: theme.warning_foreground,
        danger: theme.danger,
        success: theme.success,
        info: theme.info,
        text_primary: theme.foreground,
        text_secondary: theme.muted_foreground,
        text_muted: theme.muted_foreground.opacity(0.8),
        accent,
        accent_dim: color(palette.accent_dim),
        thinking: color(palette.thinking_text),
        tool_text: color(palette.tool_text),
        heading: color(palette.heading),
        code: color(palette.code_color),
        diff_add: color(palette.diff_add),
        diff_remove: color(palette.diff_remove),
        quote: theme.muted_foreground,
        dimmed: theme.muted_foreground,
        focus_mark: accent.opacity(0.6),
        focus_tint: theme.list_active,
        separator: theme.border,
        match_background: accent.opacity(0.25),
        match_foreground: theme.foreground,
        current_match_background: theme.warning,
        current_match_foreground: theme.warning_foreground,
        custom_title: color(palette.custom_title),
        custom_title_highlight: color(palette.custom_title_highlight),
        summary: color(palette.summary),
        summary_highlight: color(palette.summary_highlight),
        model: color(palette.model_color),
        duration: color(palette.duration_color),
        msg_count: color(palette.msg_count),
        preview: color(palette.preview),
        context_base: color(palette.context_base),
        context_highlight: color(palette.context_highlight),
        dot_separator: color(palette.dot_separator),
        header_summary: color(palette.header_summary),
        source_label: theme.muted_foreground,
        recency_now: color(palette.timestamp_now),
        recency_minutes: color(palette.timestamp_minutes),
        recency_hours: color(palette.timestamp_hours),
        recency_days: color(palette.timestamp_days),
        recency_old: theme.muted_foreground,
    }
}

/// The user's theme preference for this session (`[gui] theme`, View → Theme).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThemePreferenceState(pub ThemePreference);

impl Global for ThemePreferenceState {}

/// The current theme preference.
pub fn preference(cx: &App) -> ThemePreference {
    cx.try_global::<ThemePreferenceState>()
        .map(|state| state.0)
        .unwrap_or_default()
}

fn mode_for(kind: ThemeKind) -> ThemeMode {
    match kind {
        ThemeKind::Dark => ThemeMode::Dark,
        ThemeKind::Light => ThemeMode::Light,
    }
}

fn appearance_is_dark(appearance: WindowAppearance) -> bool {
    matches!(
        appearance,
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

/// Apply the configured theme before the first window opens (§7.3 step 9).
pub fn apply_startup_theme(cx: &mut App) {
    let kind = apply_startup_theme_to_app(cx);
    core_theme::set_theme(kind);
}

/// The app-local part of [`apply_startup_theme`]: the `[gui] theme`
/// preference and gpui-kit's theme mode, without core's process-wide palette
/// (`app::init_for_test` uses it; parallel tests share the process). Returns
/// the resolved kind.
pub fn apply_startup_theme_to_app(cx: &mut App) -> ThemeKind {
    let pref = cx
        .try_global::<crate::globals::AppState>()
        .map(|state| state.config.gui.theme())
        .unwrap_or_default();
    cx.set_global(ThemePreferenceState(pref));
    let kind = pref.resolve(appearance_is_dark(cx.window_appearance()));
    Theme::change(mode_for(kind), None, cx);
    kind
}

/// Follow the OS appearance when the preference is System (window roots call
/// this from `observe_window_appearance`; idempotent).
pub fn follow_system(window: &mut Window, cx: &mut App) {
    follow_system_with(window.appearance(), window, cx);
}

/// [`follow_system`] for an explicit OS `appearance` (tests simulate an OS
/// appearance change with it; the platform test window cannot).
pub fn follow_system_with(appearance: WindowAppearance, window: &mut Window, cx: &mut App) {
    if preference(cx) != ThemePreference::System {
        return;
    }
    let kind = ThemePreference::System.resolve(appearance_is_dark(appearance));
    if cx.theme().is_dark() != kind.is_dark() {
        Theme::change(mode_for(kind), Some(window), cx);
        core_theme::set_theme(kind);
        crate::menus::rebuild(cx);
        refresh_viewers(cx);
        cx.refresh_windows();
    }
}

/// Tell the viewer of every open window (main and single-file) the theme
/// changed (§11.19: drop theme-keyed caches, re-render anchored). Deferred:
/// the window that triggered the switch is mid-update and cannot be read now.
pub fn refresh_viewers(cx: &mut App) {
    cx.defer(|cx| {
        let mut viewers = Vec::new();
        for handle in cx.windows() {
            let Some(root) = handle.downcast::<gpui_kit::base::Root>() else {
                continue;
            };
            let Ok(view) = root.read(cx).map(|root| root.view().clone()) else {
                continue;
            };
            if let Ok(main) = view.clone().downcast::<crate::workspace::Workspace>() {
                viewers.push(main.read(cx).viewer().clone());
            } else if let Ok(single) = view.downcast::<crate::single_file::SingleFileWindow>() {
                viewers.push(single.read(cx).viewer().clone());
            }
        }
        for viewer in viewers {
            viewer.update(cx, |viewer, cx| viewer.apply_theme(cx));
        }
    });
}

/// Switch to `pref` for this session: kit theme, core palette, menus.
pub fn apply_theme(pref: ThemePreference, window: &mut Window, cx: &mut App) {
    apply_theme_with(pref, window.appearance(), window, cx);
}

/// [`apply_theme`] for an explicit OS `appearance` (what System resolves to).
pub fn apply_theme_with(
    pref: ThemePreference,
    appearance: WindowAppearance,
    window: &mut Window,
    cx: &mut App,
) {
    cx.set_global(ThemePreferenceState(pref));
    let kind = pref.resolve(appearance_is_dark(appearance));
    Theme::change(mode_for(kind), Some(window), cx);
    core_theme::set_theme(kind);
    crate::menus::rebuild(cx);
    refresh_viewers(cx);
    cx.refresh_windows();
}

/// Minimum contrast ratio (WCAG AA for text) of every text role on the
/// backgrounds it is drawn on, in both themes (X-039).
pub const MIN_CONTRAST: f32 = 4.5;

/// Contrast the adjustment aims for: a little above [`MIN_CONTRAST`] so that
/// rounding in a later colour conversion never drops below it.
const TARGET_CONTRAST: f32 = 4.6;

fn linear(channel: f32) -> f32 {
    if channel <= 0.040_45 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG relative luminance of an opaque colour.
pub fn relative_luminance(color: Hsla) -> f32 {
    let rgb = color.to_rgb();
    0.2126 * linear(rgb.r) + 0.7152 * linear(rgb.g) + 0.0722 * linear(rgb.b)
}

/// `fg` drawn over `bg` (alpha composited; `bg` is treated as opaque).
pub fn flatten(bg: Hsla, fg: Hsla) -> Hsla {
    let bg = Hsla { a: 1.0, ..bg };
    bg.blend(fg)
}

/// WCAG contrast ratio of `fg` on `bg` (1.0 … 21.0). Translucent colours are
/// composited over `backdrop` first (`bg` over `backdrop`, then `fg` over that).
pub fn contrast_ratio(fg: Hsla, bg: Hsla, backdrop: Hsla) -> f32 {
    let bg = flatten(backdrop, bg);
    let fg = flatten(bg, fg);
    let (a, b) = (relative_luminance(fg), relative_luminance(bg));
    let (light, dark) = if a > b { (a, b) } else { (b, a) };
    (light + 0.05) / (dark + 0.05)
}

/// A text colour on the background it is drawn on.
#[derive(Clone, Copy, Debug)]
pub struct ContrastPair {
    pub name: &'static str,
    pub fg: Hsla,
    pub bg: Hsla,
    /// What translucent colours of the pair are composited over.
    pub backdrop: Hsla,
}

impl ContrastPair {
    pub fn ratio(&self) -> f32 {
        contrast_ratio(self.fg, self.bg, self.backdrop)
    }
}

/// The lowest-contrast adjustment of `fg` that reaches [`TARGET_CONTRAST`] on
/// every background of `bgs`: lightness moves away from the backgrounds'
/// luminance (darker on light themes, lighter on dark ones) keeping hue and
/// saturation. Colours that already pass are returned unchanged.
fn ensure_contrast(fg: Hsla, bgs: &[Hsla], backdrop: Hsla) -> Hsla {
    let worst = |color: Hsla| {
        bgs.iter()
            .map(|bg| contrast_ratio(color, *bg, backdrop))
            .fold(f32::INFINITY, f32::min)
    };
    if worst(fg) >= TARGET_CONTRAST {
        return fg;
    }
    let mean_luminance = bgs
        .iter()
        .map(|bg| relative_luminance(flatten(backdrop, *bg)))
        .sum::<f32>()
        / bgs.len().max(1) as f32;
    let darken = mean_luminance > 0.18;
    let mut color = fg;
    for _ in 0..100 {
        color.l = if darken {
            (color.l - 0.01).max(0.0)
        } else {
            (color.l + 0.01).min(1.0)
        };
        if worst(color) >= TARGET_CONTRAST || color.l <= 0.0 || color.l >= 1.0 {
            break;
        }
    }
    if worst(color) < TARGET_CONTRAST {
        // Saturated colours can top out below the target at the extreme
        // lightness; drop the saturation too.
        color.s = 0.0;
    }
    color
}

/// Declares which backgrounds each text role is drawn on: the single source of
/// [`adjust_roles`] (the roles are adjusted to pass) and [`Roles::contrast_pairs`]
/// (the test checks them).
macro_rules! text_roles {
    ($($field:ident => [$($bg:ident),+ $(,)?]),+ $(,)?) => {
        fn adjust_roles(roles: &mut Roles) {
            let backdrop = roles.background;
            $(
                roles.$field = ensure_contrast(roles.$field, &[$(roles.$bg),+], backdrop);
            )+
        }

        impl Roles {
            /// Every text role with each background it is drawn on (X-039).
            pub fn contrast_pairs(&self) -> Vec<ContrastPair> {
                let mut pairs = Vec::new();
                $(
                    for bg in [$(self.$bg),+] {
                        pairs.push(ContrastPair {
                            name: stringify!($field),
                            fg: self.$field,
                            bg,
                            backdrop: self.background,
                        });
                    }
                )+
                pairs
            }
        }
    };
}

// Backgrounds: `background` (viewer transcript, dialogs), `sidebar` (left pane),
// `list_active` (selected row, focused message), `list_hover`, `code_background`,
// `match_background` / `current_match_background` (search highlights).
// Not text, so not listed: `border`, `separator`, `selection`, `focus_mark`,
// `focus_tint`, `dot_separator` (a decorative glyph between meta fields).
text_roles! {
    text_primary => [background, sidebar, list_active, list_hover],
    text_secondary => [background, sidebar, list_active, list_hover],
    text_muted => [background, sidebar, list_active],
    muted_foreground => [background, sidebar, list_active, list_hover],
    accent => [background, sidebar, list_active, match_background],
    accent_dim => [background, sidebar, list_active],
    thinking => [background, list_active],
    tool_text => [background, list_active],
    heading => [background, list_active],
    code => [background, code_background],
    diff_add => [background, list_active],
    diff_remove => [background, list_active],
    quote => [background, list_active],
    dimmed => [background, list_active],
    link => [background, sidebar, code_background],
    warning => [background, sidebar],
    danger => [background, sidebar],
    success => [background, sidebar],
    info => [background, sidebar],
    match_foreground => [match_background],
    custom_title => [sidebar, list_active],
    custom_title_highlight => [sidebar, list_active],
    summary => [sidebar, list_active],
    summary_highlight => [sidebar, list_active],
    model => [sidebar, list_active],
    duration => [sidebar, list_active],
    msg_count => [sidebar, list_active],
    preview => [sidebar, list_active],
    context_base => [sidebar, list_active],
    context_highlight => [sidebar, list_active],
    header_summary => [background],
    source_label => [sidebar, list_active],
    recency_now => [sidebar, list_active],
    recency_minutes => [sidebar, list_active],
    recency_hours => [sidebar, list_active],
    recency_days => [sidebar, list_active],
    recency_old => [sidebar, list_active],
    warning_foreground => [current_match_background],
    current_match_foreground => [current_match_background],
}

/// View → Theme / Settings / palette: apply `pref` and, with `save`, store it
/// in gui.toml `[gui] theme` through the change-set save (K12: only that key
/// changes, comments and concurrent edits survive). While gui.toml has errors
/// the theme applies to this session only and a warning toast says
/// `Theme not saved: gui.toml has errors` (C-059, §11.19).
pub fn set_theme_preference(pref: ThemePreference, save: bool, window: &mut Window, cx: &mut App) {
    apply_theme(pref, window, cx);
    if save {
        save_theme_preference(pref, window, cx);
    }
}

/// The warning shown when the theme cannot be stored.
pub const THEME_NOT_SAVED_GUI_ERRORS: &str = "Theme not saved: gui.toml has errors";

fn save_theme_preference(pref: ThemePreference, window: &mut Window, cx: &mut App) {
    let (env, base, blocked) = {
        let state = crate::globals::app_state(cx);
        (
            state.env_with_workspace(),
            state.config.gui_file.clone(),
            state.config_errors.gui.is_some(),
        )
    };
    if blocked {
        crate::notify::Toast::warning(THEME_NOT_SAVED_GUI_ERRORS)
            .replacing::<ThemeSaveToast>("theme")
            .show(window, cx);
        return;
    }
    if base.gui_settings().theme() == pref {
        return;
    }
    let mut edited = base.clone();
    let mut gui = edited.gui_settings();
    gui.theme = Some(pref);
    edited.gui = Some(gui);
    let handle = window.window_handle();
    let task = cx.background_executor().spawn(async move {
        claude_history_core::config::save_gui_config_changes(&env, &base, &edited)
            .map_err(|error| crate::settings::error_text(&error))
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| match result {
            Ok(_) => {
                crate::settings::reload_config(cx);
            }
            Err(error) => {
                handle
                    .update(cx, |_, window, cx| {
                        crate::notify::Toast::warning(format!("Theme not saved: {error}"))
                            .replacing::<ThemeSaveToast>("theme")
                            .show(window, cx);
                    })
                    .ok();
            }
        });
    })
    .detach();
}

struct ThemeSaveToast;

#[cfg(test)]
#[path = "theme_tests.rs"]
mod theme_tests;
