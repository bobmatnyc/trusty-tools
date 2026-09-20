//! Unit tests for the structured, evidence-backed completion report (#8204,
//! #8289).
//!
//! Why: the refusal arm is a safety property — accepting a `completed` finish
//! over a red suite is the exact defect #8289 was filed on — so each arm of
//! the decision gets its own test against a hand-built transcript, with no
//! LLM round trip.
//! What: the typed-report shape (#8204) and all four evidence arms: failed,
//! unverified, passed, and absent (#8206's untouched path).
//! Test: this module is itself the test surface.

use serde_json::json;

use super::*;
use crate::llm::{FunctionCall, ToolCall};
use crate::tools::BASH_TOOL_NAME;

fn bash_call(id: &str, command: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        kind: "function".into(),
        function: FunctionCall {
            name: BASH_TOOL_NAME.into(),
            arguments: json!({ "command": command }).to_string(),
        },
    }
}

/// A transcript in which `cargo test` ran and printed `output`.
fn transcript_running(command: &str, output: &str) -> Transcript {
    let mut transcript = Transcript::seed("run cargo test before finishing", "do the thing");
    transcript.push_assistant(None, &[bash_call("c1", command)]);
    transcript.push_tool_result("c1", BASH_TOOL_NAME, output);
    transcript
}

/// A transcript where nothing ran at all.
fn bare_transcript() -> Transcript {
    Transcript::seed("no suite here", "do the thing")
}

fn completed_args(summary: &str) -> Value {
    json!({ "status": "completed", "summary": summary })
}

const GREEN: &str = "test result: ok. 68 passed; 0 failed; 0 ignored";
const RED: &str = "test result: FAILED. 66 passed; 2 failed; 0 ignored";

// ── #8204: the typed report ───────────────────────────────────────────────

/// Every structured field crosses as itself — this is what the TUI's
/// dedicated slots read instead of re-parsing `render_finish_summary`.
#[test]
fn report_carries_the_typed_fields() {
    let args = json!({
        "status": "completed",
        "summary": "added the flag",
        "changes": [
            {"file": "crates/a/src/lib.rs", "lines_added": 10, "lines_removed": 2},
            {"file": "crates/a/src/b.rs"},
        ],
        "tests_run": 12,
        "tests_passed": 12,
    });

    let report = report(&args, &transcript_running("cargo test", GREEN)).expect("a report");

    assert_eq!(report.status, "completed");
    assert_eq!(report.summary, "added the flag");
    assert_eq!(report.changes.len(), 2);
    assert_eq!(report.changes[0].file, "crates/a/src/lib.rs");
    assert_eq!(report.changes[0].lines_added, Some(10));
    assert_eq!(report.changes[1].lines_added, None);
    assert_eq!(report.tests_run, Some(12));
    assert_eq!(report.tests_passed, Some(12));
}

/// A `changes` entry with no path is dropped rather than rendered as a
/// placeholder a reader would mistake for a real file.
#[test]
fn report_drops_a_change_with_no_path() {
    let args = json!({
        "status": "completed",
        "summary": "s",
        "changes": [{"lines_added": 4}, {"file": "real.rs"}],
    });

    let report = report(&args, &bare_transcript()).expect("a report");

    assert_eq!(report.changes.len(), 1);
    assert_eq!(report.changes[0].file, "real.rs");
}

// ── #8289: verified comes from the machine, never the model ───────────────

/// The model claiming a full pass does NOT make the report verified — only
/// captured output does.
#[test]
fn report_is_verified_only_by_captured_output() {
    let claim = json!({
        "status": "completed",
        "summary": "all green",
        "tests_run": 12,
        "tests_passed": 12,
    });

    let backed = report(&claim, &transcript_running("cargo test", GREEN)).expect("a report");
    assert!(backed.verified, "a captured pass line verifies the run");

    let unbacked = report(&claim, &bare_transcript()).expect("a report");
    assert!(
        !unbacked.verified,
        "a claim with no captured output must never read as verified"
    );
    assert!(unbacked.evidence.is_none());
}

/// A command that ran and printed nothing recognisable is marked unverified
/// in the prose too, not only in the typed flag.
#[test]
fn unverified_evidence_notes_the_summary() {
    let report = report(
        &completed_args("done"),
        &transcript_running("cargo test", "warming up\nfinished\n"),
    )
    .expect("a report");

    assert!(!report.verified);
    assert!(
        report.summary.contains("UNVERIFIED"),
        "summary must say so: {}",
        report.summary
    );
    assert_eq!(
        report.evidence.expect("evidence").outcome,
        EvidenceOutcome::Unverified
    );
}

// ── #8289: the refusal arms ───────────────────────────────────────────────

/// THE fail-open arm: the model claims `completed` while the suite printed
/// `FAILED`. The finish is refused, and the refusal quotes the failing line.
#[test]
fn a_failed_suite_refuses_a_completed_finish() {
    let args = json!({
        "status": "completed",
        "summary": "all tests pass",
        "tests_run": 12,
        "tests_passed": 12,
    });

    let reason = contradicted_by_evidence(&args, &transcript_running("cargo test -p x", RED))
        .expect("a completed claim over a red suite must be refused");

    assert!(reason.contains("cargo test -p x"), "names the command");
    assert!(reason.contains("FAILED"), "quotes the failing line");
}

/// An honest `failed` status over the same red suite is accepted — refusing
/// it would leave the model no way to report a real failure.
#[test]
fn a_failed_status_over_a_red_suite_is_accepted() {
    let args = json!({ "status": "failed", "summary": "two tests fail" });

    assert!(contradicted_by_evidence(&args, &transcript_running("cargo test", RED)).is_none());
}

/// A suite that printed nothing recognisable is NOT refused (that would wedge
/// a run whose tool output was truncated by something other than the model) —
/// it is accepted and marked unverified instead.
#[test]
fn unverified_evidence_does_not_refuse() {
    let transcript = transcript_running("cargo test", "no verdict here\n");

    assert!(contradicted_by_evidence(&completed_args("done"), &transcript).is_none());
    assert!(
        !report(&completed_args("done"), &transcript)
            .expect("report")
            .verified
    );
}

/// A green suite is not refused.
#[test]
fn a_green_suite_does_not_refuse() {
    assert!(
        contradicted_by_evidence(
            &completed_args("done"),
            &transcript_running("cargo test", GREEN)
        )
        .is_none()
    );
}

/// #8206 preserved: a run with no test command at all is neither refused nor
/// claimed as verified.
#[test]
fn no_test_command_neither_refuses_nor_verifies() {
    assert!(contradicted_by_evidence(&completed_args("done"), &bare_transcript()).is_none());
    let report = report(&completed_args("done"), &bare_transcript()).expect("a report");
    assert!(!report.verified);
    assert!(report.evidence.is_none());
    assert_eq!(report.summary, "done", "no note when nothing ran");
}

// ── the transcript block #8289's closure condition names ──────────────────

/// The final output carries the REAL `test result:` line, so a completed
/// run's transcript shows what the suite printed and not only what the model
/// said about it.
#[test]
fn evidence_block_carries_the_real_result_lines() {
    let transcript = transcript_running("cargo test -p trusty-code", GREEN);
    let perf = PerfCollector::new(0, "agent_loop", "t");

    let output = build_finish_output(&transcript, &perf, &completed_args("added the flag"));

    assert!(
        output.content.contains(GREEN),
        "the real result line must reach the transcript: {}",
        output.content
    );
    assert!(
        output
            .content
            .contains("Evidence (cargo test -p trusty-code)"),
        "the block names its command: {}",
        output.content
    );
}

/// A completion with no captured lines adds no evidence block — no empty
/// header in the transcript.
#[test]
fn evidence_block_is_empty_without_captured_lines() {
    let perf = PerfCollector::new(0, "agent_loop", "t");

    let output = build_finish_output(&bare_transcript(), &perf, &completed_args("done"));

    assert!(!output.content.contains("Evidence ("), "{}", output.content);
}
