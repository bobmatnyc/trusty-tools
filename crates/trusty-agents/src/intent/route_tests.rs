//! Tests for `intent::route` — pure-Tcode, pure-Tm, the required ambiguous
//! cases, and a determinism/no-panic property sweep (epic #3052, PR B).

use super::*;

// =====================================================================
// Pure-Tcode cases
// =====================================================================

#[test]
fn hard_verb_alone_routes_tcode() {
    assert_eq!(route_task("please refactor the parser"), BridgeRoute::Tcode);
}

#[test]
fn repo_file_extension_routes_tcode() {
    assert_eq!(
        route_task("something is wrong in main.rs"),
        BridgeRoute::Tcode
    );
}

#[test]
fn src_path_token_routes_tcode() {
    assert_eq!(
        route_task("look under src/ for the bug"),
        BridgeRoute::Tcode
    );
}

#[test]
fn count_based_tcode_signals_without_hard_verb_or_file_token() {
    // No hard verb (fix/debug/implement/refactor), no repo-file token, no Tm
    // signal — falls through to rule 4, where "patch"/"compile"/"rename"/
    // "bug" (4 Tcode signals, 0 Tm signals) tip it to Tcode.
    assert_eq!(
        route_task("patch the compile error and rename the bug"),
        BridgeRoute::Tcode
    );
}

#[test]
fn stack_trace_and_error_marker_route_tcode() {
    assert_eq!(
        route_task("here is a stack trace, error: null pointer"),
        BridgeRoute::Tcode
    );
}

#[test]
fn unit_test_phrase_routes_tcode() {
    assert_eq!(
        route_task("the unit test for this module keeps failing"),
        BridgeRoute::Tcode
    );
}

// =====================================================================
// Pure-Tm cases
// =====================================================================

#[test]
fn session_and_backlog_route_tm() {
    assert_eq!(
        route_task("spawn a new session and check the backlog"),
        BridgeRoute::Tm
    );
}

#[test]
fn delegate_and_agents_route_tm() {
    assert_eq!(
        route_task("delegate this across the agents roster"),
        BridgeRoute::Tm
    );
}

#[test]
fn pull_request_phrase_routes_tm() {
    assert_eq!(
        route_task("what's the status of the pull request"),
        BridgeRoute::Tm
    );
}

#[test]
fn multi_agent_hyphenated_token_routes_tm() {
    assert_eq!(
        route_task("coordinate the multi-agent rollout"),
        BridgeRoute::Tm
    );
}

#[test]
fn across_repos_phrase_routes_tm() {
    assert_eq!(
        route_task("prioritize the roadmap across repos"),
        BridgeRoute::Tm
    );
}

// =====================================================================
// Required ambiguous cases (spec-mandated)
// =====================================================================

/// "fix" is a hard Tcode verb — rule 1 fires before the Tm words
/// ("sprint"/"backlog") are ever counted. This pins the documented
/// precedence (hard verb beats Tm vocabulary), not a naive "ambiguous -> Tm"
/// shortcut.
#[test]
fn ambiguous_fix_the_sprint_backlog_routes_tcode_via_hard_verb_precedence() {
    assert_eq!(
        route_task("fix the sprint backlog"),
        BridgeRoute::Tcode,
        "hard Tcode verb 'fix' must win over Tm words 'sprint'/'backlog' (rule 1 precedes rule 2)"
    );
}

/// "write" is only a GENERIC code verb (rule 3), not a hard verb (rule 1) —
/// so the Tm signals "project"/"roadmap" are checked FIRST (rule 2) and win.
#[test]
fn ambiguous_write_up_the_project_roadmap_routes_tm_via_signal_precedence() {
    assert_eq!(
        route_task("write up the project roadmap"),
        BridgeRoute::Tm,
        "Tm words 'project'/'roadmap' must win over the generic verb 'write' (rule 2 precedes rule 3)"
    );
}

#[test]
fn empty_string_routes_tm_default() {
    assert_eq!(route_task(""), BridgeRoute::Tm);
}

#[test]
fn whitespace_only_routes_tm_default() {
    assert_eq!(route_task("   "), BridgeRoute::Tm);
}

/// No coding verb, no repo-file token, no Tm vocabulary at all — a genuine
/// zero-signal tie, must fall back to the locked Tm default.
#[test]
fn no_signal_input_routes_tm_default() {
    assert_eq!(route_task("do the thing"), BridgeRoute::Tm);
}

// =====================================================================
// Determinism + no-panic property sweep (mirrors
// intent::classifier_property_tests)
// =====================================================================

#[test]
fn never_panics_on_adversarial_inputs() {
    let adversarial: Vec<String> = vec![
        "".into(),
        " ".into(),
        "\0".into(),
        "\x01\x02\x03".into(),
        "a".repeat(10_000),
        "/".into(),
        "src/".repeat(500),
        "fix ".repeat(200),
        "\n\n\n".into(),
        "\u{1F525}\u{1F680}\u{1F480}".into(),
        "caf\u{00E9} r\u{00E9}sum\u{00E9} na\u{00EF}ve".into(),
        "\u{4E2D}\u{6587}\u{8F93}\u{5165}".into(),
        "error:".into(),
        "main.rs".into(),
        "tm-tm-tm".into(),
    ];
    for input in &adversarial {
        let _ = route_task(input);
    }
}

#[test]
fn deterministic_across_calls() {
    let inputs = [
        "",
        "fix the sprint backlog",
        "write up the project roadmap",
        "spawn a new session and check the backlog",
        "patch the compile error and rename the bug",
        "the quick brown fox jumps over the lazy dog",
    ];
    for input in &inputs {
        let first = route_task(input);
        let second = route_task(input);
        assert_eq!(first, second, "non-deterministic for '{input}'");
    }
}

#[test]
fn whitespace_padding_invariance() {
    let inputs = [
        "fix the parser",
        "spawn a session",
        "do the thing",
        "write up the roadmap",
    ];
    for input in &inputs {
        let plain = route_task(input);
        let padded = route_task(&format!("  {input}  "));
        assert_eq!(plain, padded, "whitespace changed the route for '{input}'");
    }
}
