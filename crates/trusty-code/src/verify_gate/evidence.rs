//! Pulling the REAL output of the test command that ran out of a transcript
//! (#8289).
//!
//! Why: `verify_gate`'s gates answer "was a test command invoked" and stop
//! there — they never read what it printed. That is how a completed run's
//! transcript could carry `tests_passed: 12` beside a suite that printed
//! `FAILED`: the gate saw the invocation, accepted the finish, and the only
//! account of the run that reached the caller was the model's own prose. This
//! module reads the other half: the `bash` call's captured result, the verdict
//! lines in it, and what those lines actually say.
//! What: [`collect_test_evidence`] finds the LAST `bash` call in a
//! transcript whose command [`super::is_test_command`] matches, pairs it with
//! the `tool` message carrying that call's output, and reduces that output to
//! its verdict lines — capped by
//! [`crate::finish_report::EVIDENCE_LINE_CAP`] and
//! [`crate::finish_report::EVIDENCE_LINE_CHARS`].
//! [`classify`] then reads a verdict off those lines: any failure marker wins
//! over any pass marker (a suite that printed both failed), and output with
//! neither is [`EvidenceOutcome::Unverified`] rather than either verdict —
//! the fail-CLOSED choice #8289 exists to force.
//! Test: `tests::*`; the refusal/unverified arms it feeds are pinned in
//! `agent_loop::finish_verify::tests::*`.

use regex::Regex;

use crate::finish_report::{EVIDENCE_LINE_CAP, EVIDENCE_LINE_CHARS, EvidenceOutcome, TestEvidence};
use crate::llm::ChatMessage;

use super::{bash_command_from_call, is_test_command};

/// Lines worth keeping out of a test command's output.
///
/// Why: a suite prints hundreds of lines of progress and a handful that state
/// an outcome. Keeping only the latter is what makes a bounded, per-completion
/// payload possible at all.
/// What: cargo's `test result:` and `error[E…]`/`error:` lines, the
/// `N passed`/`N failed` shape pytest/jest/vitest print, go's `--- FAIL`/
/// `FAIL`/`ok  ` lines, a `failures:` header, and a panic. Compiled per call
/// for the same reason `super::test_command_pattern` is: this runs at most
/// once per `finish_task` dispatch.
/// Test: `tests::verdict_lines_are_extracted_from_noise`.
fn verdict_pattern() -> Regex {
    Regex::new(
        r"(test result:)|(\d+\s+(passed|failed|failing))|(\bFAILED\b)|(^\s*failures:)|(^\s*---\s+FAIL)|(^\s*FAIL\b)|(^\s*ok\s)|(panicked at)|(^error(\[[A-Z0-9]+\])?:)",
    )
    .expect("verdict_pattern: hardcoded regex literal must compile")
}

/// Markers that mean the suite did NOT pass.
///
/// Why: classification must be asymmetric — a run printing both `12 passed`
/// and `3 failed` failed. Kept separate from [`verdict_pattern`] so widening
/// what is CAPTURED can never quietly widen what counts as a pass.
/// What: `FAILED` (cargo's `test result: FAILED.`), a `failures:` header, go's
/// `--- FAIL`/leading `FAIL`, a non-zero `N failed`/`N failing`, a panic, and
/// a compiler `error[E…]:`/`error:`. `0 failed` is deliberately NOT a failure
/// marker — cargo prints it on every passing run, which is also why this
/// pattern is CASE-SENSITIVE: a case-insensitive `FAILED` alternative matches
/// the `failed` inside `0 failed` and classifies every green cargo run as a
/// failure.
/// Test: `tests::zero_failed_is_not_a_failure`,
/// `tests::a_failure_line_beats_a_pass_line`.
fn failure_pattern() -> Regex {
    Regex::new(
        r"(\bFAILED\b)|(^\s*failures:)|(^\s*---\s+FAIL)|(^\s*FAIL\b)|([1-9]\d*\s+(failed|failing))|((?i:panicked at))|(^error(\[[A-Z0-9]+\])?:)",
    )
    .expect("failure_pattern: hardcoded regex literal must compile")
}

/// Markers that mean the suite passed.
///
/// What: cargo's `test result: ok.`, a non-zero `N passed`, and go's leading
/// `ok `. `0 passed` is not a pass — a suite that ran nothing proves nothing.
/// Test: `tests::classify_reads_a_cargo_pass_line`,
/// `tests::zero_passed_is_not_a_pass`.
fn pass_pattern() -> Regex {
    Regex::new(r"(test result:\s*ok)|([1-9]\d*\s+passed)|(^\s*ok\s)")
        .expect("pass_pattern: hardcoded regex literal must compile")
}

/// Read a verdict off already-extracted evidence lines (#8289).
///
/// Why: separated from extraction so the "what does this output SAY"
/// decision is unit-testable against a hand-written `&[String]`, with no
/// transcript to build.
/// What: [`EvidenceOutcome::Failed`] if ANY line matches [`failure_pattern`];
/// otherwise [`EvidenceOutcome::Passed`] if any matches [`pass_pattern`];
/// otherwise [`EvidenceOutcome::Unverified`], which is also what an empty
/// slice gets.
/// Test: `tests::classify_reads_a_cargo_pass_line`,
/// `tests::a_failure_line_beats_a_pass_line`,
/// `tests::unrecognised_output_is_unverified`.
pub fn classify(lines: &[String]) -> EvidenceOutcome {
    let failure = failure_pattern();
    if lines.iter().any(|l| failure.is_match(l)) {
        return EvidenceOutcome::Failed;
    }
    let pass = pass_pattern();
    if lines.iter().any(|l| pass.is_match(l)) {
        return EvidenceOutcome::Passed;
    }
    EvidenceOutcome::Unverified
}

/// Reduce one command's raw output to the capped verdict lines.
///
/// Why: the cap is what makes this payload safe to put on every completion
/// event; see [`EVIDENCE_LINE_CAP`]'s own docs.
/// What: keeps [`verdict_pattern`] lines, trimmed of trailing whitespace,
/// each truncated to [`EVIDENCE_LINE_CHARS`] characters (never bytes — a
/// byte slice can split a UTF-8 sequence and panic). When more than
/// [`EVIDENCE_LINE_CAP`] match, the LAST `EVIDENCE_LINE_CAP` are kept —
/// cargo prints its summary last — and the returned flag says so.
/// Test: `tests::verdict_lines_are_extracted_from_noise`,
/// `tests::evidence_keeps_the_last_lines_under_the_cap`,
/// `tests::a_long_line_is_truncated_to_the_char_cap`.
fn verdict_lines(output: &str) -> (Vec<String>, bool) {
    let pattern = verdict_pattern();
    let all: Vec<String> = output
        .lines()
        .map(str::trim_end)
        .filter(|line| pattern.is_match(line))
        .map(|line| line.chars().take(EVIDENCE_LINE_CHARS).collect())
        .collect();
    let truncated = all.len() > EVIDENCE_LINE_CAP;
    let kept = if truncated {
        all[all.len() - EVIDENCE_LINE_CAP..].to_vec()
    } else {
        all
    };
    (kept, truncated)
}

/// The test command a transcript ran, paired with what it printed (#8289).
///
/// Why: the one entry point `agent_loop::finish_verify` calls at the
/// `finish_task` boundary. Returns `None` when NO test command ran at all,
/// which is a different state from "ran and printed nothing recognisable" —
/// the caller treats the two differently, so this must not collapse them.
/// What: scans assistant turns for `bash` calls whose command
/// [`is_test_command`] matches, takes the LAST one (the run the model is
/// finishing on), and looks up the `tool` message whose `tool_call_id` is
/// that call's id. A matching call with no result message yet, or an empty
/// one, still returns `Some` — with empty `lines` and
/// [`EvidenceOutcome::Unverified`], because a command that ran and captured
/// nothing is exactly the case the caller must refuse to report as passed.
/// Test: `tests::evidence_pairs_the_command_with_its_output`,
/// `tests::no_test_command_yields_no_evidence`,
/// `tests::a_test_command_with_no_captured_output_is_unverified`,
/// `tests::the_last_test_command_wins`.
pub fn collect_test_evidence(messages: &[ChatMessage]) -> Option<TestEvidence> {
    let (call_id, command) = messages
        .iter()
        .filter_map(|m| m.tool_calls.as_ref())
        .flatten()
        .filter_map(|call| {
            bash_command_from_call(call)
                .filter(|cmd| is_test_command(cmd))
                .map(|cmd| (call.id.clone(), cmd))
        })
        .next_back()?;

    let output = messages
        .iter()
        .find(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some(call_id.as_str()))
        .and_then(|m| m.content.as_deref())
        .unwrap_or_default();

    let (lines, truncated) = verdict_lines(output);
    let outcome = classify(&lines);
    Some(TestEvidence {
        command,
        lines,
        truncated,
        outcome,
    })
}

#[cfg(test)]
#[path = "evidence_tests.rs"]
mod tests;
