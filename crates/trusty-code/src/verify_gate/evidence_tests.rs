//! Unit tests for transcript test-output capture (#8289).
//!
//! Why: the classification is the whole safety property — a wrong `Passed`
//! here is a completion that reports success over a red suite — so it is
//! tested against hand-written output rather than only through the loop.
//! What: extraction (which lines survive, the two caps, which `bash` call
//! wins) and classification (pass/fail/unverified, and the asymmetry between
//! them).
//! Test: this module is itself the test surface.

use super::*;
use crate::finish_report::EVIDENCE_LINE_CAP;
use crate::llm::{FunctionCall, ToolCall};
use crate::tools::BASH_TOOL_NAME;

fn bash_call(id: &str, command: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        kind: "function".into(),
        function: FunctionCall {
            name: BASH_TOOL_NAME.into(),
            arguments: serde_json::json!({ "command": command }).to_string(),
        },
    }
}

fn assistant_calling(calls: Vec<ToolCall>) -> ChatMessage {
    ChatMessage {
        role: "assistant".into(),
        content: None,
        images: vec![],
        tool_calls: Some(calls),
        tool_call_id: None,
        name: None,
        cache_control: None,
    }
}

/// A transcript in which `command` ran under `call_id` and printed `output`.
fn ran(call_id: &str, command: &str, output: &str) -> Vec<ChatMessage> {
    vec![
        ChatMessage::system("run cargo test before finishing"),
        ChatMessage::user("do the thing"),
        assistant_calling(vec![bash_call(call_id, command)]),
        ChatMessage::tool_result(call_id, BASH_TOOL_NAME, output),
    ]
}

/// The command and its printed verdict arrive together: the whole point is
/// that a reader sees WHICH command produced WHICH lines.
#[test]
fn evidence_pairs_the_command_with_its_output() {
    let messages = ran(
        "c1",
        "cargo test -p trusty-code",
        "   Compiling trusty-code\nrunning 68 tests\n....\ntest result: ok. 68 passed; 0 failed; 0 ignored\n",
    );

    let evidence = collect_test_evidence(&messages).expect("a test command ran");

    assert_eq!(evidence.command, "cargo test -p trusty-code");
    assert_eq!(evidence.outcome, EvidenceOutcome::Passed);
    assert_eq!(
        evidence.lines,
        vec!["test result: ok. 68 passed; 0 failed; 0 ignored".to_string()]
    );
    assert!(!evidence.truncated);
}

/// Compiler noise and progress rows never reach the payload — only the lines
/// that state something.
#[test]
fn verdict_lines_are_extracted_from_noise() {
    let messages = ran(
        "c1",
        "cargo test",
        "   Compiling serde v1.0\n    Finished test profile\n     Running unittests src/lib.rs\n\nrunning 3 tests\ntest a::b ... ok\ntest result: FAILED. 2 passed; 1 failed; 0 ignored\n",
    );

    let evidence = collect_test_evidence(&messages).expect("a test command ran");

    assert!(
        !evidence.lines.iter().any(|l| l.contains("Compiling")),
        "progress noise must not reach the payload: {:?}",
        evidence.lines
    );
    assert!(
        evidence
            .lines
            .iter()
            .any(|l| l.contains("test result: FAILED")),
        "the verdict line must survive: {:?}",
        evidence.lines
    );
}

/// A suite that printed nothing recognisable is UNVERIFIED, never a pass —
/// the fail-closed arm #8289 exists for.
#[test]
fn unrecognised_output_is_unverified() {
    let messages = ran("c1", "cargo test", "warming the cache\nall done\n");

    let evidence = collect_test_evidence(&messages).expect("a test command ran");

    assert!(evidence.lines.is_empty());
    assert_eq!(evidence.outcome, EvidenceOutcome::Unverified);
}

/// A matching `bash` call whose output never reached the transcript still
/// yields evidence — an UNVERIFIED one. Returning `None` here would make it
/// indistinguishable from "no test command ran", which the caller treats
/// differently.
#[test]
fn a_test_command_with_no_captured_output_is_unverified() {
    let messages = vec![
        ChatMessage::system("run cargo test before finishing"),
        assistant_calling(vec![bash_call("c1", "cargo test")]),
    ];

    let evidence = collect_test_evidence(&messages).expect("the command ran");

    assert_eq!(evidence.command, "cargo test");
    assert!(evidence.lines.is_empty());
    assert_eq!(evidence.outcome, EvidenceOutcome::Unverified);
}

/// A transcript with no test command at all yields `None`, so the #8206
/// "nothing to run" path stays distinguishable from a captured-nothing run.
#[test]
fn no_test_command_yields_no_evidence() {
    let messages = vec![
        ChatMessage::user("do the thing"),
        assistant_calling(vec![bash_call("c1", "ls -la")]),
        ChatMessage::tool_result("c1", BASH_TOOL_NAME, "a.rs\nb.rs\n"),
    ];

    assert!(collect_test_evidence(&messages).is_none());
}

/// The LAST matching command is the one the model is finishing on — an
/// earlier red run followed by a green re-run must report the re-run.
#[test]
fn the_last_test_command_wins() {
    let messages = vec![
        assistant_calling(vec![bash_call("c1", "cargo test --lib")]),
        ChatMessage::tool_result(
            "c1",
            BASH_TOOL_NAME,
            "test result: FAILED. 0 passed; 1 failed",
        ),
        assistant_calling(vec![bash_call("c2", "cargo test --all")]),
        ChatMessage::tool_result("c2", BASH_TOOL_NAME, "test result: ok. 1 passed; 0 failed"),
    ];

    let evidence = collect_test_evidence(&messages).expect("a test command ran");

    assert_eq!(evidence.command, "cargo test --all");
    assert_eq!(evidence.outcome, EvidenceOutcome::Passed);
}

/// Over the cap, the LAST lines are kept (cargo prints its summary last) and
/// the payload says it was cut.
#[test]
fn evidence_keeps_the_last_lines_under_the_cap() {
    let mut output = String::new();
    for i in 0..(EVIDENCE_LINE_CAP + 5) {
        output.push_str(&format!("test result: ok. {i} passed; 0 failed\n"));
    }
    let messages = ran("c1", "cargo test", &output);

    let evidence = collect_test_evidence(&messages).expect("a test command ran");

    assert_eq!(evidence.lines.len(), EVIDENCE_LINE_CAP);
    assert!(evidence.truncated);
    assert!(
        evidence.lines[EVIDENCE_LINE_CAP - 1]
            .contains(&format!("{} passed", EVIDENCE_LINE_CAP + 4)),
        "the tail must be kept, not the head: {:?}",
        evidence.lines.last()
    );
}

/// One pathological line cannot blow the payload past the per-line cap.
#[test]
fn a_long_line_is_truncated_to_the_char_cap() {
    let long = format!("test result: FAILED. {}", "x".repeat(1_000));
    let messages = ran("c1", "cargo test", &long);

    let evidence = collect_test_evidence(&messages).expect("a test command ran");

    assert_eq!(evidence.lines[0].chars().count(), EVIDENCE_LINE_CHARS);
}

/// Cargo's passing summary reads as a pass.
#[test]
fn classify_reads_a_cargo_pass_line() {
    let lines = vec!["test result: ok. 68 passed; 0 failed; 0 ignored".to_string()];
    assert_eq!(classify(&lines), EvidenceOutcome::Passed);
}

/// `0 failed` appears on EVERY passing cargo run — reading it as a failure
/// would make the gate refuse every green suite.
///
/// Why this exact shape: a case-INSENSITIVE `FAILED` alternative matches the
/// `failed` inside `0 failed`, which classified every green cargo run as a
/// failure. Caught by this test during #8289's first gate run.
#[test]
fn zero_failed_is_not_a_failure() {
    let lines = vec!["12 passed; 0 failed".to_string()];
    assert_eq!(classify(&lines), EvidenceOutcome::Passed);
    assert_eq!(
        classify(&["test result: ok. 68 passed; 0 failed; 0 ignored".to_string()]),
        EvidenceOutcome::Passed,
        "cargo's own green summary must not read as a failure"
    );
}

/// A suite that ran nothing proves nothing.
#[test]
fn zero_passed_is_not_a_pass() {
    let lines = vec!["0 passed in 0.01s".to_string()];
    assert_eq!(classify(&lines), EvidenceOutcome::Unverified);
}

/// The asymmetry: a run printing a pass line for one target and a failure
/// for another FAILED.
#[test]
fn a_failure_line_beats_a_pass_line() {
    let lines = vec![
        "test result: ok. 12 passed; 0 failed".to_string(),
        "test result: FAILED. 3 passed; 2 failed".to_string(),
    ];
    assert_eq!(classify(&lines), EvidenceOutcome::Failed);
}

/// A compile error is a failure, not an absence of verdict — the suite never
/// got to print one.
#[test]
fn a_compile_error_is_a_failure() {
    let lines = vec!["error[E0308]: mismatched types".to_string()];
    assert_eq!(classify(&lines), EvidenceOutcome::Failed);
}

/// pytest's own summary line classifies without a cargo-shaped verdict.
#[test]
fn classify_reads_a_pytest_summary() {
    assert_eq!(
        classify(&["5 passed in 0.31s".to_string()]),
        EvidenceOutcome::Passed
    );
    assert_eq!(
        classify(&["2 failed, 3 passed in 0.44s".to_string()]),
        EvidenceOutcome::Failed
    );
}

/// No lines at all is unverified, never a default pass.
#[test]
fn empty_evidence_is_unverified() {
    assert_eq!(classify(&[]), EvidenceOutcome::Unverified);
}
