//! Turning an accepted `finish_task` call into a structured, evidence-backed
//! report (#8204, #8289).
//!
//! Why: two gaps met at the same seam. `build_finish_output` flattened the
//! model's schema-validated completion into prose before anything left the
//! loop, so a client had to re-parse what the schema already proved (#8204);
//! and nothing read the test command's real output, so a completion could
//! claim a passing suite over a red one (#8289). Both are answered by the
//! same value — [`crate::finish_report::FinishReport`] — assembled here, at
//! the one point that holds BOTH the validated arguments and the transcript
//! the command's output landed in.
//! What: [`report`] builds that value (typed fields plus
//! `verify_gate::evidence`'s capture of what the suite printed);
//! [`contradicted_by_evidence`] is the refusal half — a `completed` claim
//! over captured output that states a FAILURE is downgraded to the same
//! recoverable tool error `verify_gate`'s own reject arm uses, so the model
//! gets a turn to reconcile rather than the run reporting a pass. This file
//! also owns [`build_finish_output`] and [`append_finish_note`], moved here
//! from `agent_loop/mod.rs` when that file reached the 500-SLOC cap.
//! Test: `tests::*` (in `finish_verify_tests.rs`), plus
//! `agent_loop::tests::finish_gate_*` for the surrounding dispatch path.

use serde_json::Value;

use crate::finish_report::{EvidenceOutcome, FinishChange, FinishReport, TestEvidence};
use crate::perf::PerfCollector;
use crate::tools::{AgentOutput, FinishTaskArgs, ToolResult};
use crate::verify_gate::evidence::collect_test_evidence;

use super::{Transcript, build_output};

/// The recoverable reason a `completed` finish gets when the captured test
/// output contradicts it (#8289).
///
/// Why: built per call so the refusal quotes the FAILING LINE the agent has
/// to reconcile — "your tests failed" alone does not say which run or what
/// it printed.
/// What: names the command, the first failure line captured from it, and the
/// one action that clears the refusal.
/// Test: `tests::a_failed_suite_refuses_a_completed_finish`.
fn contradiction_reason(evidence: &TestEvidence) -> String {
    let line = evidence
        .lines
        .iter()
        .find(|l| !l.is_empty())
        .map(String::as_str)
        .unwrap_or("(no verdict line captured)");
    format!(
        "finish_task rejected: `{}` printed `{}`, so this run's tests did not pass. \
         Reconcile the failures and re-run the suite, or call finish_task with \
         status \"failed\" — do not report a passing run.",
        evidence.command, line
    )
}

/// The note folded into an UNVERIFIED completion's summary (#8289).
///
/// Why: the accepted-but-unbacked arm must leave a trace in the prose a
/// caller reads, not only in the typed `verified: false` flag — a consumer
/// that renders only `summary` (the CLI path) would otherwise show a bare
/// success claim.
/// What: one sentence naming the command whose output proved nothing.
/// Test: `tests::unverified_evidence_notes_the_summary`.
fn unverified_note(command: &str) -> String {
    format!(
        "Note (#8289): `{command}` ran but printed no recognisable test result, \
         so this completion is UNVERIFIED."
    )
}

/// Whether the captured test output contradicts this `finish_task` call
/// (#8289) — `Some(reason)` refuses it.
///
/// Why: the verify gate proves a test command was INVOKED and stops there.
/// This is the other half: what it printed. Without it a run could report
/// `tests_passed: 12` over a suite that printed `FAILED`, which is the defect
/// #8289 was filed on.
/// What: refuses ONLY the combination that lies — a `completed` status over
/// evidence classified [`EvidenceOutcome::Failed`]. A `failed`/`cancelled`
/// status over the same evidence is an honest report and is accepted; a suite
/// that printed nothing recognisable is accepted too, marked unverified by
/// [`report`], because refusing it would wedge a run whose tool output is
/// truncated by something other than the model (the #8206 lesson).
/// Test: `tests::a_failed_suite_refuses_a_completed_finish`,
/// `tests::a_failed_status_over_a_red_suite_is_accepted`,
/// `tests::unverified_evidence_does_not_refuse`,
/// `tests::a_green_suite_does_not_refuse`.
fn contradicted_by_evidence(args: &Value, transcript: &Transcript) -> Option<String> {
    let parsed: FinishTaskArgs = serde_json::from_value(args.clone()).ok()?;
    if !matches!(parsed.status, crate::tools::FinishStatus::Completed) {
        return None;
    }
    let evidence = collect_test_evidence(&transcript.messages())?;
    (evidence.outcome == EvidenceOutcome::Failed).then(|| contradiction_reason(&evidence))
}

/// Downgrade a `finish_task` result the captured test output contradicts
/// (#8289).
///
/// Why: the decision and its log line live here rather than inline in
/// `dispatch_all`, which sits against the crate's 500-SLOC production cap.
/// What: a no-op unless `result` is currently a SUCCESS and
/// [`contradicted_by_evidence`] objects; then `result` becomes the same
/// recoverable `ToolResult::err` the verify gate's own reject arm produces,
/// so the model gets another turn instead of the run reporting a pass.
/// Test: `tests::a_failed_suite_refuses_a_completed_finish` (the decision),
/// `agent_loop::tests::gate_intercept::*` (the surrounding retry path).
pub(super) fn enforce_evidence(args: &Value, transcript: &Transcript, result: &mut ToolResult) {
    if result.is_error() {
        return;
    }
    if let Some(reason) = contradicted_by_evidence(args, transcript) {
        tracing::warn!(
            reason = %reason,
            "agent_loop: captured test output contradicts finish_task (#8289)"
        );
        *result = ToolResult::err(reason);
    }
}

/// Build the structured completion report for an accepted `finish_task`
/// (#8204, #8289).
///
/// Why: the typed payload `Event::TaskFinished` carries, so a client renders
/// dedicated slots instead of re-parsing `render_finish_summary`'s prose.
/// What: copies the model's own validated fields; drops a `changes` entry
/// carrying no path (a placeholder would read as a real file); attaches
/// `verify_gate::evidence`'s capture; sets `verified` from the EVIDENCE
/// alone, never from the model's claim, and appends [`unverified_note`] to
/// the summary when a command ran but proved nothing. `None` when the
/// arguments do not deserialise — unreachable past schema validation, and a
/// caller that gets `None` simply emits no report rather than inventing one.
/// Test: `tests::report_carries_the_typed_fields`,
/// `tests::report_drops_a_change_with_no_path`,
/// `tests::report_is_verified_only_by_captured_output`,
/// `tests::unverified_evidence_notes_the_summary`.
pub(super) fn report(args: &Value, transcript: &Transcript) -> Option<FinishReport> {
    let parsed: FinishTaskArgs = serde_json::from_value(args.clone()).ok()?;
    let evidence = collect_test_evidence(&transcript.messages());
    let verified = evidence
        .as_ref()
        .is_some_and(|e| e.outcome == EvidenceOutcome::Passed);

    let mut summary = parsed.summary.clone();
    if let Some(e) = &evidence
        && e.outcome == EvidenceOutcome::Unverified
    {
        summary.push_str("\n\n");
        summary.push_str(&unverified_note(&e.command));
    }

    Some(FinishReport {
        status: parsed.status.to_string(),
        summary,
        changes: parsed
            .changes
            .iter()
            .filter_map(|c| {
                c.file.as_ref().map(|file| FinishChange {
                    file: file.clone(),
                    lines_added: c.lines_added,
                    lines_removed: c.lines_removed,
                })
            })
            .collect(),
        tests_run: parsed.tests_run,
        tests_passed: parsed.tests_passed,
        evidence,
        verified,
    })
}

/// Render the captured test output as a transcript block (#8289).
///
/// Why: #8289's closure condition is about the TRANSCRIPT — the prose a
/// caller reads back — not only the typed event. This is what puts the real
/// `test result:` lines there, beside the model's claim.
/// What: an `Evidence (<command>):` header, one indented line per captured
/// line, and an explicit truncation marker when the cap dropped any. Empty
/// string when nothing was captured, so a no-evidence run adds no noise.
/// Test: `tests::evidence_block_carries_the_real_result_lines`,
/// `tests::evidence_block_is_empty_without_captured_lines`.
fn render_evidence(evidence: &TestEvidence) -> String {
    if evidence.lines.is_empty() {
        return String::new();
    }
    let mut out = format!("\nEvidence ({}):", evidence.command);
    if evidence.truncated {
        out.push_str("\n  … earlier lines omitted");
    }
    for line in &evidence.lines {
        out.push_str("\n  ");
        out.push_str(line);
    }
    out
}

/// Assemble the final `AgentOutput` from an explicit `finish_task` call
/// (#2072, #8289).
///
/// Why: an explicit `finish_task` call carries a deterministic, structured
/// completion report the model built on purpose — reusing that report as the
/// loop's final output (rather than whatever prose the transcript happens to
/// contain) is §5.8's "deterministic, no prose interpretation" claim. #8289
/// adds the half the model cannot fake: the real output of the test command
/// that ran.
/// What: starts from [`build_output`]'s usage/content baseline, then — when
/// `finish_args` deserialises — overwrites `content` with
/// `render_finish_summary` PLUS [`render_evidence`], sets `summary` to the
/// report's own summary (carrying the unverified note when there is one), and
/// records `status` in `finish_status`. On the should-be-unreachable
/// deserialisation failure, falls back to the transcript-derived content.
/// Test: `agent_loop::tests::explicit_finish_task_terminates_loop_with_structured_summary`,
/// `tests::evidence_block_carries_the_real_result_lines`.
pub(super) fn build_finish_output(
    transcript: &Transcript,
    perf: &PerfCollector,
    finish_args: &Value,
) -> AgentOutput {
    let mut output = build_output(transcript, perf);
    if let Ok(parsed) = serde_json::from_value::<FinishTaskArgs>(finish_args.clone()) {
        let rendered = crate::tools::render_finish_summary(&parsed);
        match report(finish_args, transcript) {
            Some(report) => {
                output.summary = Some(report.summary.clone());
                output.content = match &report.evidence {
                    Some(evidence) => format!("{rendered}{}", render_evidence(evidence)),
                    None => rendered,
                };
            }
            None => {
                output.summary = Some(parsed.summary.clone());
                output.content = rendered;
            }
        }
        output.finish_status = Some(parsed.status);
    }
    output
}

/// Fold a `verify_gate::FinishGateOutcome::AcceptWithNote` note into a
/// `finish_task` call's validated arguments (#8206).
///
/// Why: the accepted-but-unverified arm must leave a trace in what the caller
/// reads back, not only in the log. Appending to the model's own `summary` is
/// the one edit that reaches BOTH halves of the recorded report.
/// What: appends `note` to `args["summary"]`, separated by a blank line. A
/// non-string or absent `summary` (unreachable past schema validation, which
/// marks it required) is left untouched rather than replaced.
/// Test: `agent_loop::tests::finish_gate_note_reaches_the_recorded_report`.
pub(super) fn append_finish_note(args: &mut Value, note: &str) {
    if let Some(summary) = args.get("summary").and_then(Value::as_str) {
        let merged = format!("{summary}\n\n{note}");
        args["summary"] = Value::String(merged);
    }
}

#[cfg(test)]
#[path = "finish_verify_tests.rs"]
mod tests;
