//! Tests of `theme_tokens.rs` (DESIGN §17.5): contrast of every role pair in
//! both themes (X-039), no colour literal outside this module (X-019), theme
//! switching (C-035), saving through the change-set save (C-059, C-060).

use std::path::{Path, PathBuf};

use claude_history_core::theme::{self as core_theme, ThemeKind, ThemePreference};
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{AppContext as _, TestAppContext, WindowAppearance};

use super::*;
use crate::actions::{SetThemeDark, SetThemeLight, SetThemeSystem};
use crate::globals::{app_state, last_toast};
use crate::test_support::{TestWorld, global_lock, open_main};

fn roles_in(mode: ThemeMode, cx: &mut TestAppContext) -> Roles {
    cx.update(|cx| {
        if cx
            .try_global::<crate::test_support::KitInitialized>()
            .is_none()
        {
            gpui_kit::init(cx);
            cx.set_global(crate::test_support::KitInitialized);
        }
        Theme::change(mode, None, cx);
        roles(cx)
    })
}

fn check_theme(mode: ThemeMode, cx: &mut TestAppContext) {
    let roles = roles_in(mode, cx);
    let mut failures = Vec::new();
    for pair in roles.contrast_pairs() {
        let ratio = pair.ratio();
        if ratio < MIN_CONTRAST {
            failures.push(format!("{} on a background: {ratio:.2}", pair.name));
        }
    }
    assert!(
        failures.is_empty(),
        "{mode:?}: roles below {MIN_CONTRAST}:1\n{}",
        failures.join("\n")
    );
    assert!(roles.contrast_pairs().len() > 50);
}

/// X-039: every foreground/background pair of `theme_tokens::roles()` has a
/// contrast of at least 4.5:1, in the light and the dark theme.
#[gpui_kit::test]
fn every_role_pair_has_enough_contrast_in_both_themes(cx: &mut TestAppContext) {
    check_theme(ThemeMode::Light, cx);
    check_theme(ThemeMode::Dark, cx);
}

/// V-075 / X-039: the search-match text (other matches and the current one)
/// reaches 4.5:1 on its fill as drawn — the translucent fill composited over
/// the transcript background — in both themes (integrator, Phase C: white
/// text was chosen on the pale light-theme fill and on dark-theme yellow).
#[gpui_kit::test]
fn search_match_text_is_legible_in_both_themes(cx: &mut TestAppContext) {
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        let roles = roles_in(mode, cx);
        let styles = crate::viewer::render::MatchStyles::new(&roles);
        for (name, style, fill) in [
            ("other match", styles.other, roles.match_background),
            (
                "current match",
                styles.current,
                roles.current_match_background,
            ),
        ] {
            let text = style.color.expect("a match sets its text colour");
            let ratio = contrast_ratio(text, fill, roles.background);
            assert!(
                ratio >= MIN_CONTRAST,
                "{mode:?} {name}: {ratio:.2}:1 (text {text:?} on {fill:?})"
            );
        }
    }
    // The guard itself: a foreground that passes stays, one that fails is
    // replaced by the better of near-black and white.
    let white = gpui_kit::hsla(0.0, 0.0, 1.0, 1.0);
    let yellow = gpui_kit::hsla(0.14, 0.95, 0.5, 1.0);
    let dark = gpui_kit::hsla(0.6, 0.5, 0.15, 1.0);
    assert_eq!(legible_on(yellow, dark, white), dark);
    assert!(legible_on(yellow, white, white).l < 0.1);
}

/// The contrast math: black on white is 21:1, equal colours 1:1.
#[test]
fn contrast_ratio_matches_wcag() {
    let white = gpui_kit::hsla(0.0, 0.0, 1.0, 1.0);
    let black = gpui_kit::hsla(0.0, 0.0, 0.0, 1.0);
    assert!((contrast_ratio(black, white, white) - 21.0).abs() < 0.01);
    assert!((contrast_ratio(white, white, white) - 1.0).abs() < 0.001);
    // A translucent foreground is composited over the background first.
    let faint = gpui_kit::hsla(0.0, 0.0, 0.0, 0.0);
    assert!((contrast_ratio(faint, white, white) - 1.0).abs() < 0.001);
}

/// A failing colour is moved (keeping its hue) until it passes; a passing one
/// is returned unchanged.
#[test]
fn ensure_contrast_adjusts_only_failing_colours() {
    let white = gpui_kit::hsla(0.0, 0.0, 1.0, 1.0);
    let pale_yellow = gpui_kit::hsla(0.16, 0.9, 0.6, 1.0);
    assert!(contrast_ratio(pale_yellow, white, white) < MIN_CONTRAST);
    let fixed = ensure_contrast(pale_yellow, &[white], white);
    assert!(contrast_ratio(fixed, white, white) >= MIN_CONTRAST);
    assert_eq!(fixed.h, pale_yellow.h);
    assert!(fixed.l < pale_yellow.l);
    let dark = gpui_kit::hsla(0.6, 0.5, 0.15, 1.0);
    assert_eq!(ensure_contrast(dark, &[white], white).l, dark.l);
    // On a dark background lightness goes up instead.
    let night = gpui_kit::hsla(0.6, 0.2, 0.1, 1.0);
    let dim = gpui_kit::hsla(0.6, 0.3, 0.3, 1.0);
    let lifted = ensure_contrast(dim, &[night], night);
    assert!(lifted.l > dim.l);
    assert!(contrast_ratio(lifted, night, night) >= MIN_CONTRAST);
}

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

/// Whether the type name at byte `at` of `code` is a function's return type
/// followed by its body (`-> gpui_kit::Hsla {`), not a struct literal.
fn is_return_type(code: &str, at: usize) -> bool {
    code[..at]
        .trim_end_matches(|c: char| c.is_alphanumeric() || c == '_' || c == ':')
        .trim_end()
        .ends_with("->")
}

/// Test modules build arbitrary colours as test inputs; X-019 is about what
/// the app paints (integrator, Phase A: same exemptions as qa/lint.rs).
fn is_test_source(rel: &str) -> bool {
    rel.starts_with("skeleton_tests")
        || rel.starts_with("test_support/")
        || rel.starts_with("integration_tests/")
        || rel.contains("/tests/")
        || rel.ends_with("/tests.rs")
        || rel.ends_with("_tests.rs")
}

/// Whether `line` constructs a colour from a literal.
fn has_color_literal(line: &str) -> bool {
    let code = line.split("//").next().unwrap_or("");
    let constructors = [
        concat!("rg", "b("),
        concat!("rg", "ba("),
        concat!("hs", "la("),
        concat!("hs", "l("),
        concat!("Rg", "ba {"),
        concat!("Hs", "la {"),
        concat!("opaque_", "grey("),
        concat!("opaque_", "gray("),
        concat!("gpui_kit::bl", "ack("),
        concat!("gpui_kit::wh", "ite("),
        concat!("Hsla::bl", "ack("),
        concat!("Hsla::wh", "ite("),
        concat!("Hsla::re", "d("),
        concat!("Hsla::gre", "en("),
        concat!("Hsla::blu", "e("),
    ];
    let literal_at = |pattern: &str| {
        code.match_indices(pattern)
            .any(|(at, _)| !is_return_type(code, at))
    };
    if constructors.iter().any(|pattern| literal_at(pattern)) {
        return true;
    }
    // `0xRRGGBB` / `0xRRGGBBAA` hex colours (6 or 8 hex digits).
    let bytes = code.as_bytes();
    let mut i = 0;
    while i + 2 < bytes.len() {
        if bytes[i] == b'0' && (bytes[i + 1] == b'x' || bytes[i + 1] == b'X') {
            let run: Vec<u8> = bytes[i + 2..]
                .iter()
                .copied()
                .take_while(|byte| byte.is_ascii_hexdigit() || *byte == b'_')
                .collect();
            let digits = run.iter().filter(|byte| **byte != b'_').count();
            // A type suffix (`0xdead_beef_u32`) is a number, not a colour.
            let suffixed = bytes
                .get(i + 2 + run.len())
                .is_some_and(|byte| byte.is_ascii_alphanumeric());
            if (digits == 6 || digits == 8) && !suffixed {
                return true;
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    false
}

/// X-019: colours are converted only in `theme_tokens.rs`; call sites use its
/// roles or the kit's theme tokens.
#[test]
fn no_color_literal_outside_theme_tokens() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let mut violations = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        // The roles module and its own tests are the colour authority.
        if rel == "theme_tokens.rs" || rel == "theme_tests.rs" || is_test_source(&rel) {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        for (number, line) in text.lines().enumerate() {
            if has_color_literal(line) {
                violations.push(format!("{rel}:{}: {}", number + 1, line.trim()));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "colour literals outside theme_tokens.rs (DESIGN X-019):\n{}",
        violations.join("\n")
    );
}

#[test]
fn the_colour_literal_detector_finds_literals() {
    assert!(has_color_literal(concat!("let c = rg", "b(0x112233);")));
    assert!(has_color_literal("let c = 0xFF00AA;"));
    assert!(has_color_literal("let c = 0xff00aa80;"));
    assert!(!has_color_literal("let n = 0x1F; // no colour"));
    assert!(!has_color_literal("let id = 0xdead_beef_u32;"));
    assert!(!has_color_literal("// rgb(0x112233) in a comment"));
    // Struct literals count; a return type before a function body does not.
    assert!(has_color_literal(concat!(
        "let c = Hs",
        "la { h: 0.1, s: 0.2, l: 0.3, a: 1.0 };"
    )));
    assert!(has_color_literal(concat!(
        "Some(gpui_kit::Hs",
        "la { h, s, l, a })"
    )));
    assert!(!has_color_literal(concat!(
        "fn lane(role: Role) -> Hs",
        "la {"
    )));
    assert!(!has_color_literal(concat!(
        "fn lane(r: &Roles) -> gpui_kit::Hs",
        "la {"
    )));
    assert!(is_test_source("viewer/tests.rs"));
    assert!(is_test_source("list/tests/rows.rs"));
    assert!(!is_test_source("viewer/render.rs"));
}

fn dispatch_theme(cx: &mut TestAppContext, handle: gpui_kit::AnyWindowHandle, pref: &str) {
    match pref {
        "dark" => cx.dispatch_action(handle, SetThemeDark),
        "light" => cx.dispatch_action(handle, SetThemeLight),
        _ => cx.dispatch_action(handle, SetThemeSystem),
    }
    cx.run_until_parked();
}

/// C-035: SetThemeDark → core kind Dark and the kit theme dark; SetThemeSystem
/// follows the OS appearance, also when it changes later.
#[gpui_kit::test]
fn theme_switch_sets_core_kind_and_kit_mode_and_follows_the_os(cx: &mut TestAppContext) {
    let _guard = global_lock();
    let world = TestWorld::new();
    let (handle, _ws) = open_main(cx, &world, &[]);
    dispatch_theme(cx, handle, "dark");
    assert_eq!(core_theme::kind(), ThemeKind::Dark);
    assert!(cx.read(|cx| cx.theme().is_dark()));
    assert_eq!(cx.read(preference), ThemePreference::Dark);

    dispatch_theme(cx, handle, "light");
    assert_eq!(core_theme::kind(), ThemeKind::Light);
    assert!(!cx.read(|cx| cx.theme().is_dark()));

    // System follows the OS (the test platform reports light), and a later
    // OS appearance change flips it.
    dispatch_theme(cx, handle, "system");
    assert_eq!(cx.read(preference), ThemePreference::System);
    assert_eq!(core_theme::kind(), ThemeKind::Light);
    cx.update_window(handle, |_, window, cx| {
        follow_system_with(WindowAppearance::Dark, window, cx)
    })
    .unwrap();
    assert_eq!(core_theme::kind(), ThemeKind::Dark);
    assert!(cx.read(|cx| cx.theme().is_dark()));
    cx.update_window(handle, |_, window, cx| {
        follow_system_with(WindowAppearance::Light, window, cx)
    })
    .unwrap();
    assert_eq!(core_theme::kind(), ThemeKind::Light);

    // An explicit choice ignores OS changes.
    dispatch_theme(cx, handle, "dark");
    cx.update_window(handle, |_, window, cx| {
        follow_system_with(WindowAppearance::Light, window, cx)
    })
    .unwrap();
    assert_eq!(core_theme::kind(), ThemeKind::Dark);
    dispatch_theme(cx, handle, "light");
}

fn gui_toml(world: &TestWorld) -> PathBuf {
    world.env.gui_config_file().expect("gui.toml path")
}

/// C-033 (theme part): a View-menu theme change is saved to gui.toml as
/// `[gui] theme`, config.toml stays byte-identical and a comment of gui.toml
/// survives.
#[gpui_kit::test]
fn the_view_menu_saves_the_theme_and_keeps_comments(cx: &mut TestAppContext) {
    let _guard = global_lock();
    let world = TestWorld::new();
    world.write_config(
        Some("[display]\nlast = false\n"),
        Some("# my gui settings\n[gui]\nterminal = \"wt\"\n"),
    );
    let config_before = std::fs::read(world.env.config_file().unwrap()).unwrap();
    let (handle, _ws) = open_main(cx, &world, &[]);
    dispatch_theme(cx, handle, "dark");
    let text = std::fs::read_to_string(gui_toml(&world)).unwrap();
    assert!(text.contains("theme = \"dark\""), "{text}");
    assert!(text.contains("# my gui settings"), "{text}");
    assert!(text.contains("terminal = \"wt\""), "{text}");
    assert_eq!(
        std::fs::read(world.env.config_file().unwrap()).unwrap(),
        config_before
    );
    // The reloaded configuration knows the new theme.
    assert_eq!(
        cx.read(|cx| app_state(cx).config.gui.theme()),
        ThemePreference::Dark
    );
    dispatch_theme(cx, handle, "light");
}

/// C-059: a malformed gui.toml blocks the write: the theme applies to the
/// session, the warning toast names the reason, the bytes are unchanged.
#[gpui_kit::test]
fn a_broken_gui_toml_blocks_theme_saving(cx: &mut TestAppContext) {
    let _guard = global_lock();
    let world = TestWorld::new();
    let broken = "[gui\ntheme = ";
    world.write_config(None, Some(broken));
    let (handle, _ws) = open_main(cx, &world, &[]);
    assert!(cx.read(|cx| app_state(cx).config_errors.gui.is_some()));
    dispatch_theme(cx, handle, "dark");
    assert_eq!(core_theme::kind(), ThemeKind::Dark);
    assert!(cx.read(|cx| cx.theme().is_dark()));
    assert_eq!(
        cx.read(last_toast).as_ref(),
        "Theme not saved: gui.toml has errors"
    );
    assert_eq!(std::fs::read_to_string(gui_toml(&world)).unwrap(), broken);
    dispatch_theme(cx, handle, "light");
}

/// §11.19: a theme switch reaches the viewer of every open window (the
/// viewer's `apply_theme` notifies it).
#[gpui_kit::test]
fn theme_switch_notifies_the_viewers(cx: &mut TestAppContext) {
    let _guard = global_lock();
    let world = TestWorld::new();
    let (handle, ws) = open_main(cx, &world, &[]);
    let viewer = cx.read(|cx| ws.read(cx).viewer().clone());
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = hits.clone();
    let _sub = cx.update(|cx| {
        cx.observe(&viewer, move |_, _| {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })
    });
    dispatch_theme(cx, handle, "dark");
    assert!(hits.load(std::sync::atomic::Ordering::SeqCst) >= 1);
}
