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
fn run_and_build_route_tcode_with_a_genuine_tcode_signal() {
    // Owner-approved #4319 follow-up (2026-07-29): "run"/"build" added to
    // GENERIC_CODE_VERBS, gated on a genuine `has_tcode_lexical_signal` hit
    // (repo-file token / error-marker / TCODE_PHRASES / TCODE_WORDS)
    // co-occurring in the same input.
    // "patch" is a `TCODE_WORDS` entry (not a repo-file token, not a hard
    // verb) so this exercises rule 3's run/build gate specifically.
    assert_eq!(
        route_task("run the build after you patch the code"),
        BridgeRoute::Tcode
    );
    // "failing test" is a `TCODE_PHRASES` entry.
    assert_eq!(
        route_task("build the release, there's a failing test"),
        BridgeRoute::Tcode
    );
}

#[test]
fn run_and_build_alone_no_longer_route_tcode_without_that_signal() {
    // #4319 code-critic CRITICAL third follow-up (2026-07-29): the shared
    // "tests"/"release" exception that let "run the tests"/"build the
    // release" route Tcode without any OTHER Tcode signal was deleted (see
    // `route_task`'s rule-3 doc comment) — it mirrored a
    // `intent::classify_intent` exception that was itself proven to reopen
    // the #4319 crash class one level down. These now fall through to the
    // OWNER-LOCKED Tm default, same as any other signal-sparse task text.
    assert_eq!(route_task("run the tests"), BridgeRoute::Tm);
    assert_eq!(route_task("build the release"), BridgeRoute::Tm);
}

#[test]
fn run_and_build_require_a_real_technical_signal_not_just_the_bare_verb() {
    // Code-critic MEDIUM (2026-07-29): proven regression — `route_task` and
    // `classify_intent` used to disagree on plainly non-coding text.
    // "run"/"build" alone are common non-coding verbs ("run to the store",
    // "I'll run by the store after work", "let's build rapport with the
    // client", "run point on this deal") and must NOT route Tcode just
    // because the bare word matched `GENERIC_CODE_VERBS` — they now require
    // a genuine `has_tcode_lexical_signal` hit in the SAME input.
    assert_ne!(route_task("run to the store"), BridgeRoute::Tcode);
    assert_eq!(route_task("run to the store"), BridgeRoute::Tm);
    assert_ne!(
        route_task("I'll run by the store after work"),
        BridgeRoute::Tcode
    );
    assert_ne!(
        route_task("let's build rapport with the client"),
        BridgeRoute::Tcode
    );
    assert_ne!(route_task("run point on this deal"), BridgeRoute::Tcode);
}

#[test]
fn run_generic_verb_still_loses_to_tm_signal() {
    // Precedence rule 2 (Tm signal) still beats rule 3 (generic verb) —
    // "run" joining GENERIC_CODE_VERBS must not out-rank real Tm vocabulary,
    // same guarantee `ambiguous_write_up_the_project_roadmap_routes_tm_via_signal_precedence`
    // already pins for "write".
    assert_eq!(
        route_task("run the project roadmap session"),
        BridgeRoute::Tm
    );
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

// =====================================================================
// #4319 (code-critic HIGH-1): `has_tcode_lexical_signal` — reused by
// `intent::classify_intent`'s bug-report `Research` fallback.
// =====================================================================

#[test]
fn has_tcode_lexical_signal_true_for_repo_file_extension() {
    assert!(has_tcode_lexical_signal("something is wrong in main.rs"));
}

#[test]
fn has_tcode_lexical_signal_true_for_src_path_token() {
    assert!(has_tcode_lexical_signal("look under src/ for the bug"));
}

#[test]
fn has_tcode_lexical_signal_true_for_raw_error_marker() {
    assert!(has_tcode_lexical_signal(
        "here is a stack trace, error: null pointer"
    ));
}

#[test]
fn has_tcode_lexical_signal_true_for_tcode_phrase() {
    assert!(has_tcode_lexical_signal(
        "the unit test for this module keeps failing"
    ));
}

#[test]
fn has_tcode_lexical_signal_true_for_tcode_word() {
    assert!(has_tcode_lexical_signal("please patch the bug"));
}

#[test]
fn has_tcode_lexical_signal_false_for_signal_free_input() {
    assert!(!has_tcode_lexical_signal("do the thing"));
    assert!(!has_tcode_lexical_signal(
        "the situation with the auth middleware on staging seems related \
         to the recent token refresh changes from last week"
    ));
}
