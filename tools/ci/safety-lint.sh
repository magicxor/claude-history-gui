#!/usr/bin/env bash
# The CI safety lint (DESIGN section 16.2, FEATURES X-042): forbidden calls may appear in
# crates/gui/src only in their allow-listed files.
#
#   tools/ci/safety-lint.sh              lint the working tree
#   tools/ci/safety-lint.sh --self-test  plant a violation in a temp copy and prove the lint fails,
#                                        then prove it passes on a clean tree
#   tools/ci/safety-lint.sh ROOT         lint another checkout (used by the self-test)
#
# The same rules run as the cargo test crates/gui/src/skeleton_tests/safety_lint.rs (and
# qa/lint.rs for the extra WP7 rules), so `cargo test` enforces them before CI.
# Patterns are assembled from two halves so that this script never matches itself.
set -u

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

lint() {
    local root="$1" src violations=0
    src="$root/crates/gui/src"
    [ -d "$src" ] || { echo "safety-lint: $src not found" >&2; return 2; }

    # check <fixed pattern> <allowed file>...   (files relative to crates/gui/src)
    check() {
        local pattern="$1"; shift
        local hit file rel allowed ok
        while IFS= read -r hit; do
            file="${hit%%:*}"
            rel="${file#"$src"/}"
            ok=0
            for allowed in "$@"; do
                [ "$rel" = "$allowed" ] && ok=1
            done
            if [ "$ok" = 0 ]; then
                echo "safety-lint: forbidden '$pattern' in ${hit}" >&2
                violations=$((violations + 1))
            fi
        done < <(grep -rnF --include='*.rs' -e "$pattern" "$src" || true)
    }

    # check_regex <extended regex> <label> <allowed file>...
    check_regex() {
        local regex="$1" label="$2"; shift 2
        local hit file rel allowed ok
        while IFS= read -r hit; do
            file="${hit%%:*}"
            rel="${file#"$src"/}"
            ok=0
            for allowed in "$@"; do
                [ "$rel" = "$allowed" ] && ok=1
            done
            if [ "$ok" = 0 ]; then
                echo "safety-lint: forbidden $label in ${hit}" >&2
                violations=$((violations + 1))
            fi
        done < <(grep -rnE --include='*.rs' -e "$regex" "$src" || true)
    }

    # Process state: the launch environment is read once, in main.rs.
    check "from_process""_env" main.rs skeleton_tests.rs
    check "home::home""_dir" main.rs qa/gate.rs services.rs skeleton_tests.rs
    check "dirs::home""_dir" main.rs qa/gate.rs services.rs
    check "env::""var(" main.rs qa/gate.rs services.rs skeleton_tests.rs
    check "env::""var_os(" main.rs qa/gate.rs services.rs skeleton_tests.rs
    check "current""_dir(" main.rs qa/gate.rs services.rs
    # Child processes, terminals, the network.
    check "process::""Command" services.rs ops/console.rs
    check "terminal::spawn""_terminal" services.rs skeleton_tests.rs
    check "spawn_terminal""_checked" services.rs skeleton_tests.rs
    check "check_latest""_release" services.rs skeleton_tests.rs
    check "resume::""launch(" services.rs
    check "execute""_copy" ops/resume.rs
    # Desktop side effects only through PlatformServices.
    check "write_to""_clipboard" services.rs
    check "read_from""_clipboard" services.rs skeleton_tests/contracts.rs
    check "prompt_for""_new_path" services.rs
    check "prompt_for""_paths" services.rs
    check "cx.reveal""_path(" services.rs
    check "cx.open""_url(" services.rs
    check "cx.open_with""_system(" services.rs

    # C-048 / C-049: no PAGER, NO_COLOR or CLICOLOR reads.
    check_regex 'var(_os)?\(\s*"(PAGER|NO_COLOR|CLICOLOR)' "a PAGER/NO_COLOR/CLICOLOR read"
    # X-018: no OSC 52 clipboard sequence (ESC ] 52).
    check_regex '(\\x1b|\\u\{1b\}|\\033|\\e)\]52' "an OSC 52 sequence"
    # X-027: the QA runner and everything else never inject OS input.
    # (`mouse_event` only as a whole identifier: gpui's `window.on_mouse_event` listener is not input injection.)
    check_regex 'SendInput|SendKeys|keybd_event|(^|[^A-Za-z0-9_])mouse_event|SetCursorPos|CGEventPost|XTestFake|enigo|UIAutomation' \
        "an OS input injection API"

    # X-031: nothing installs or writes the companion skill (.claude/skills).
    local skills=".claude""/skills"
    local hit
    while IFS= read -r hit; do
        echo "safety-lint: forbidden '$skills' in ${hit}" >&2
        violations=$((violations + 1))
    done < <(grep -rnF --include='*.rs' --include='*.sh' --include='*.ps1' --include='*.yml' -e "$skills" \
        "$src" "$root/tools" "$root/packaging" "$root/.github" 2>/dev/null \
        | grep -v "safety-lint.sh" || true)

    # X-026: no "..." in user-visible string literals (use the ellipsis character). Comment lines
    # and the allow-listed files (parsers/tests that handle the three-dot form) are skipped.
    while IFS= read -r hit; do
        file="${hit%%:*}"
        rel="${file#"$src"/}"
        text="${hit#*:*:}"
        case "$rel" in
            # notify.rs maps core's three-dot texts to the ellipsis (gui_text).
            notify.rs|skeleton_tests.rs|skeleton_tests/*|*/tests.rs|test_support/*|qa/lint.rs) continue ;;
        esac
        trimmed="${text#"${text%%[![:space:]]*}"}"
        case "$trimmed" in //*) continue ;; esac
        echo "safety-lint: '...' in a string literal: ${hit}" >&2
        violations=$((violations + 1))
    done < <(grep -rnE --include='*.rs' '"[^"]*\.\.\.[^"]*"' "$src" || true)

    if [ "$violations" -gt 0 ]; then
        echo "safety-lint: $violations violation(s)" >&2
        return 1
    fi
    echo "safety-lint: ok"
    return 0
}

if [ "${1:-}" = "--self-test" ]; then
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    mkdir -p "$tmp/crates/gui/src/viewer" "$tmp/tools" "$tmp/packaging"
    printf 'pub fn fine() {}\n' > "$tmp/crates/gui/src/viewer/mod.rs"
    printf 'fn main() { let _ = HistoryEnv::%s(); }\n' "from_process""_env" > "$tmp/crates/gui/src/main.rs"
    lint "$tmp" >/dev/null 2>&1 || { echo "self-test: a clean tree must pass" >&2; exit 1; }
    printf 'pub fn bad() { let _ = std::process::%s::new("x"); }\n' "Command" > "$tmp/crates/gui/src/viewer/mod.rs"
    if lint "$tmp" >/dev/null 2>&1; then
        echo "self-test: a planted violation must fail the lint" >&2
        exit 1
    fi
    printf 'pub fn bad() { let _ = std::env::%s("PAGER"); }\n' "var" > "$tmp/crates/gui/src/viewer/mod.rs"
    if lint "$tmp" >/dev/null 2>&1; then
        echo "self-test: a PAGER read must fail the lint" >&2
        exit 1
    fi
    printf 'pub fn bad() { let _ = "Loading..."; }\n' > "$tmp/crates/gui/src/viewer/mod.rs"
    if lint "$tmp" >/dev/null 2>&1; then
        echo "self-test: a three-dot literal must fail the lint" >&2
        exit 1
    fi
    echo "safety-lint self-test: ok"
    exit 0
fi

root="${1:-$here/../..}"
lint "$(cd "$root" && pwd)"
