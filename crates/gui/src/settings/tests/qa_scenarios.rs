//! The WP6 QA scenarios (`qa/scenarios/*.qa`, DESIGN §15.3) are checked here
//! for syntax against the DSL (the runner itself is WP7's): known steps,
//! parseable keystrokes, real action names, well-formed `expect` lines.

use std::path::Path;

use gpui_kit::{Keystroke, TestAppContext};

const SCENARIOS: [&str; 3] = ["settings-keys.qa", "theme.qa", "palette.qa"];

const STEPS: [&str; 17] = [
    "wait-loaded",
    "type",
    "key",
    "action",
    "click",
    "dblclick",
    "rclick",
    "answer-save",
    "answer-open",
    "set-stdout",
    "wait",
    "wait-for",
    "expect",
    "timing",
    "shot",
    "set-theme",
    "quit",
];

fn read(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("qa")
        .join("scenarios")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn steps(text: &str) -> Vec<(usize, String)> {
    text.lines()
        .enumerate()
        .map(|(ix, line)| (ix + 1, line.trim().to_string()))
        .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

#[test]
fn scenarios_use_only_dsl_steps_with_wellformed_arguments() {
    for name in SCENARIOS {
        let text = read(name);
        let list = steps(&text);
        assert!(list.len() > 8, "{name} has steps");
        assert!(
            list.first()
                .is_some_and(|(_, line)| line.starts_with("wait-loaded")),
            "{name} starts by waiting for the list"
        );
        assert!(
            list.last().is_some_and(|(_, line)| line == "quit 0"),
            "{name} ends with quit 0"
        );
        for (number, line) in &list {
            let word = line.split_whitespace().next().unwrap();
            assert!(
                STEPS.contains(&word),
                "{name}:{number}: unknown step `{word}`"
            );
            let rest = line[word.len()..].trim();
            match word {
                "type" => assert!(
                    rest.starts_with('"') && rest.ends_with('"') && rest.len() >= 2,
                    "{name}:{number}: type takes a quoted string"
                ),
                "expect" => {
                    let operators = [" == ", " != ", " contains ", " > ", " < "];
                    assert!(
                        operators.iter().any(|op| rest.contains(op)),
                        "{name}:{number}: expect needs an operator: {rest}"
                    );
                }
                "wait" => assert!(
                    rest.ends_with("ms") || rest.ends_with('s'),
                    "{name}:{number}: wait takes a duration"
                ),
                "shot" => assert!(
                    rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') && !rest.is_empty(),
                    "{name}:{number}: shot takes a plain name"
                ),
                "click" => assert!(
                    !rest.is_empty() && !rest.contains(' '),
                    "{name}:{number}: click takes one element id (no spaces)"
                ),
                "set-theme" => assert!(
                    ["dark", "light", "system"].contains(&rest),
                    "{name}:{number}: {rest}"
                ),
                _ => {}
            }
        }
    }
}

#[test]
fn scenario_keystrokes_parse() {
    for name in SCENARIOS {
        for (number, line) in steps(&read(name)) {
            if let Some(keys) = line.strip_prefix("key ") {
                for key in keys.split_whitespace() {
                    Keystroke::parse(key)
                        .unwrap_or_else(|e| panic!("{name}:{number}: key `{key}`: {e}"));
                }
            }
        }
    }
}

/// Every `action history::<Name>` step names an action that exists.
#[gpui_kit::test]
fn scenario_actions_exist(cx: &mut TestAppContext) {
    let world = crate::test_support::TestWorld::new();
    let (_handle, _ws) = crate::test_support::open_main(cx, &world, &[]);
    cx.update(|cx| {
        let mut checked = 0;
        for name in SCENARIOS {
            for (number, line) in steps(&read(name)) {
                if let Some(action) = line.strip_prefix("action ") {
                    cx.build_action(action.trim(), None)
                        .unwrap_or_else(|e| panic!("{name}:{number}: action `{action}`: {e}"));
                    checked += 1;
                }
            }
        }
        assert!(checked >= 3, "the scenarios dispatch actions by name");
    });
}
