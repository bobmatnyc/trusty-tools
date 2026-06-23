//! Tests for `control::activity` — regex/NLP parse layer (§8.2, WI-3 #1594).
//!
//! Why: keeping tests in a sibling file lets activity.rs stay within the
//! 500-SLOC production cap while retaining full coverage.
//! What: covers every `ActivityKind` variant, false-positive guards for the
//! git-op regex, and pending-decision extraction.
//! Test: run with `cargo test -p trusty-mpm control::activity_tests`.

use super::*;

/// Table-driven: (input, expected_kind, summary_contains, pending_decision,
/// proposed_default)
struct Case {
    input: &'static str,
    expected_kind: ActivityKind,
    summary_prefix: &'static str,
    pending_decision: Option<&'static str>,
    proposed_default: Option<&'static str>,
}

#[test]
fn activity_parser_awaiting_input() {
    let result = ActivityParser::parse_output("> ");
    assert!(result.is_some(), "should match awaiting input");
    let r = result.unwrap();
    assert_eq!(r.kind, ActivityKind::AwaitingInput);
    assert!(r.summary.contains("awaiting"));
}

#[test]
fn activity_parser_tool_use() {
    let result = ActivityParser::parse_output("Tool use: Bash");
    assert!(result.is_some());
    let r = result.unwrap();
    assert_eq!(
        r.kind,
        ActivityKind::ToolUse {
            name: "Bash".into()
        }
    );
    assert!(r.summary.contains("Bash"));
}

#[test]
fn activity_parser_tool_use_using_tool() {
    let result = ActivityParser::parse_output("Using tool: Edit");
    assert!(result.is_some());
    let r = result.unwrap();
    assert_eq!(
        r.kind,
        ActivityKind::ToolUse {
            name: "Edit".into()
        }
    );
}

#[test]
fn activity_parser_error() {
    let result = ActivityParser::parse_output("Error: file not found");
    assert!(result.is_some());
    let r = result.unwrap();
    assert!(matches!(r.kind, ActivityKind::Error { .. }));
    assert!(r.summary.contains("error"));
}

#[test]
fn activity_parser_test_result_cargo() {
    let result = ActivityParser::parse_output("test result: ok. 42 passed; 0 failed; 0 ignored");
    assert!(result.is_some());
    let r = result.unwrap();
    assert_eq!(
        r.kind,
        ActivityKind::TestResult {
            passed: 42,
            failed: 0
        }
    );
    assert!(r.summary.contains("42 passed"));
}

#[test]
fn activity_parser_test_result_cargo_failed() {
    let result =
        ActivityParser::parse_output("test result: FAILED. 10 passed; 3 failed; 0 ignored");
    assert!(result.is_some());
    let r = result.unwrap();
    assert_eq!(
        r.kind,
        ActivityKind::TestResult {
            passed: 10,
            failed: 3
        }
    );
    assert!(r.summary.contains("3 failed"));
}

#[test]
fn activity_parser_test_result_pytest() {
    let result = ActivityParser::parse_output("5 passed, 2 failed");
    assert!(result.is_some());
    let r = result.unwrap();
    assert_eq!(
        r.kind,
        ActivityKind::TestResult {
            passed: 5,
            failed: 2
        }
    );
}

/// Verify git-op detection with word-boundary guards (review bug #4).
///
/// Why: the old regex over-matched prose phrases like "fetching dependencies".
/// What: asserts that real git output matches and prose phrases do NOT.
/// Test: this test plus `activity_parser_git_op_no_false_positives`.
#[test]
fn activity_parser_git_op() {
    let cases = [
        ("[main abc1234] Add feature", true),
        ("git push origin main", true),
        ("git pull --rebase", true),
        ("git merge feature-branch", true),
        ("git fetch upstream", true),
        // false-positive phrases must NOT match (review bug #4)
        ("fetching dependencies from npm", false),
        ("merging results from the database", false),
        ("pulling data from the API endpoint", false),
    ];
    for (input, should_match) in &cases {
        let result = ActivityParser::parse_output(input);
        let is_git_op = result
            .as_ref()
            .is_some_and(|r| matches!(r.kind, ActivityKind::GitOp { .. }));
        if *should_match {
            assert!(
                is_git_op,
                "expected GitOp match for: {input:?}; got: {result:?}"
            );
        } else {
            assert!(
                !is_git_op,
                "false positive: {input:?} must NOT classify as GitOp; \
                 got: {result:?}"
            );
        }
    }
}

/// Explicit false-positive guard for git-op regex (review bug #4).
///
/// Why: prose phrases containing git verbs must not be misclassified.
/// What: verifies word-boundary / `git ` prefix requirement prevents matches.
/// Test: this test.
#[test]
fn activity_parser_git_op_no_false_positives() {
    let false_positives = [
        "fetching dependencies from npm",
        "merging results from the database",
        "pulling data from the API",
        "pushing the limits of what's possible",
    ];
    for input in &false_positives {
        let result = ActivityParser::parse_output(input);
        let is_git_op = result
            .as_ref()
            .is_some_and(|r| matches!(r.kind, ActivityKind::GitOp { .. }));
        assert!(
            !is_git_op,
            "false positive: {input:?} must NOT classify as GitOp; got: {result:?}"
        );
    }
}

#[test]
fn activity_parser_auth_prompt() {
    let cases = [
        "Please log in to claude.ai",
        "Run claude auth login",
        "Authentication required",
    ];
    for input in &cases {
        let result = ActivityParser::parse_output(input);
        assert!(result.is_some(), "expected match for: {input}");
        let r = result.unwrap();
        assert_eq!(r.kind, ActivityKind::AuthPrompt, "input={input}");
    }
}

#[test]
fn activity_parser_session_complete() {
    let result = ActivityParser::parse_output("Session complete");
    assert!(result.is_some());
    let r = result.unwrap();
    assert_eq!(r.kind, ActivityKind::SessionComplete);
}

#[test]
fn activity_parser_rate_limited() {
    let cases = ["rate limit exceeded", "429 Too Many Requests"];
    for input in &cases {
        let result = ActivityParser::parse_output(input);
        assert!(result.is_some(), "expected match for: {input}");
        let r = result.unwrap();
        assert_eq!(r.kind, ActivityKind::RateLimited, "input={input}");
    }
}

#[test]
fn activity_parser_pending_decision_yes_default() {
    let result =
        ActivityParser::parse_output("Do you want to proceed with the file deletion? [Y/n]:");
    assert!(result.is_some());
    let r = result.unwrap();
    assert_eq!(r.kind, ActivityKind::AwaitingInput);
    assert!(r.pending_decision.is_some());
    assert_eq!(r.proposed_default.as_deref(), Some("yes"));
}

#[test]
fn activity_parser_pending_decision_no_default() {
    let result = ActivityParser::parse_output("Continue with destructive operation? [y/N]:");
    assert!(result.is_some());
    let r = result.unwrap();
    assert_eq!(r.kind, ActivityKind::AwaitingInput);
    assert!(r.pending_decision.is_some());
    assert_eq!(r.proposed_default.as_deref(), Some("no"));
}

#[test]
fn activity_parser_returns_none_for_unknown_output() {
    let result =
        ActivityParser::parse_output("Some totally normal progress message without any keywords");
    assert!(
        result.is_none(),
        "should not match unrecognised output: got {result:?}"
    );
}

/// Table-driven sweep of all ActivityKind variants.
#[test]
fn activity_parser_table_driven() {
    let cases: Vec<Case> = vec![
        Case {
            input: "> ",
            expected_kind: ActivityKind::AwaitingInput,
            summary_prefix: "awaiting",
            pending_decision: None,
            proposed_default: None,
        },
        Case {
            input: "Tool use: Bash",
            expected_kind: ActivityKind::ToolUse {
                name: "Bash".into(),
            },
            summary_prefix: "tool use",
            pending_decision: None,
            proposed_default: None,
        },
        Case {
            input: "Error: cannot read file",
            expected_kind: ActivityKind::Error {
                excerpt: "cannot read file".into(),
            },
            summary_prefix: "error",
            pending_decision: None,
            proposed_default: None,
        },
        Case {
            input: "test result: ok. 7 passed; 0 failed; 0 ignored",
            expected_kind: ActivityKind::TestResult {
                passed: 7,
                failed: 0,
            },
            summary_prefix: "tests:",
            pending_decision: None,
            proposed_default: None,
        },
        Case {
            input: "Session complete",
            expected_kind: ActivityKind::SessionComplete,
            summary_prefix: "session complete",
            pending_decision: None,
            proposed_default: None,
        },
        Case {
            input: "Please log in to use Claude",
            expected_kind: ActivityKind::AuthPrompt,
            summary_prefix: "authentication",
            pending_decision: Some("Please log in to use Claude"),
            proposed_default: None,
        },
        Case {
            input: "rate limit exceeded: try again later",
            expected_kind: ActivityKind::RateLimited,
            summary_prefix: "rate-limit",
            pending_decision: None,
            proposed_default: None,
        },
    ];

    for c in &cases {
        let result = ActivityParser::parse_output(c.input);
        assert!(result.is_some(), "expected Some for input={:?}", c.input);
        let r = result.unwrap();
        let kind_matches = match (&r.kind, &c.expected_kind) {
            (ActivityKind::ToolUse { name: a }, ActivityKind::ToolUse { name: b }) => a == b,
            (ActivityKind::Error { .. }, ActivityKind::Error { .. }) => true,
            (a, b) => a == b,
        };
        assert!(
            kind_matches,
            "kind mismatch for input={:?}: got {:?}, want {:?}",
            c.input, r.kind, c.expected_kind
        );
        assert!(
            r.summary.contains(c.summary_prefix),
            "summary {:?} missing prefix {:?} for input={:?}",
            r.summary,
            c.summary_prefix,
            c.input
        );
        assert_eq!(
            r.pending_decision.as_deref(),
            c.pending_decision,
            "pending_decision mismatch for input={:?}",
            c.input
        );
        assert_eq!(
            r.proposed_default.as_deref(),
            c.proposed_default,
            "proposed_default mismatch for input={:?}",
            c.input
        );
    }
}
