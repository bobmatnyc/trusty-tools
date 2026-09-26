//! The structured completion report a `finish_task` call produces (#8204,
//! #8289) — the ONE wire shape both the daemon's event stream and the TUI's
//! dedicated result slots read.
//!
//! Why: `tools::finish_task` already validates a structured completion
//! (`status`, `summary`, `changes`, `tests_run`, `tests_passed`), but
//! `render_finish_summary` flattened it to prose before anything left the
//! daemon, so every consumer had to re-parse what the schema had already
//! proved (#8204). And nothing carried the test command's REAL output, so a
//! completion could claim a passing suite the transcript never showed
//! (#8289). This module is the typed payload that closes both: the model's
//! self-report ([`FinishReport`]'s own fields) and the machine's counter-
//! evidence ([`TestEvidence`]) travelling together, so a reader can see the
//! claim and what backs it side by side and never has to believe one on its
//! own.
//! What: plain serialisable data with no behaviour. `verify_gate::evidence`
//! extracts [`TestEvidence`] from a transcript; `agent_loop::finish_verify`
//! assembles [`FinishReport`] and decides [`FinishReport::verified`];
//! `events::Event::TaskFinished` carries it to an attached client, which
//! `tui_client::session_events` turns into the TUI's
//! `trusty_code_tui::ReplEvent::TaskResult`.
//! Test: `tests::*`, `events_tests::task_finished_round_trips_through_json`.
//!
//! [`FinishReport`]: crate::finish_report::FinishReport
//! [`FinishReport::verified`]: crate::finish_report::FinishReport::verified
//! [`TestEvidence`]: crate::finish_report::TestEvidence

use serde::{Deserialize, Serialize};

/// How many captured test-output lines a report carries at most (#8289).
///
/// Why: a test suite's stdout is unbounded and this payload rides the event
/// stream on every completion. Twelve lines holds cargo's `test result:` line
/// per target for a multi-target run, plus a handful of failure lines, which
/// is what a reader needs to disbelieve a wrong claim; the rest is scrollback
/// they can get from the `bash` tool card that already rendered it.
/// Test: `verify_gate::evidence::tests::evidence_keeps_the_last_lines_under_the_cap`.
pub const EVIDENCE_LINE_CAP: usize = 12;

/// How many characters one captured line carries at most (#8289).
///
/// Why: one pathological line (a minified assertion dump) must not cost more
/// than the whole cap above.
/// Test: `verify_gate::evidence::tests::a_long_line_is_truncated_to_the_char_cap`.
pub const EVIDENCE_LINE_CHARS: usize = 240;

/// What the captured test output itself says about the run (#8289).
///
/// Why: the difference between "the suite printed a pass", "the suite printed
/// a failure", and "nothing recognisable was captured" is the whole point of
/// the evidence — collapsing the third case into either of the other two is
/// exactly the fail-open #8289 was filed against.
/// What: serialises snake_case. `Unverified` is the honest default for output
/// that exists but carries no recognisable verdict line, and for a command
/// whose output never reached the transcript at all.
/// Test: `tests::outcome_round_trips_through_json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceOutcome {
    /// The captured output carries a pass verdict and no failure marker.
    Passed,
    /// The captured output carries a failure marker.
    Failed,
    /// Nothing in the captured output states a verdict either way.
    Unverified,
}

/// The real output of the test command that ran, as captured from the
/// transcript (#8289).
///
/// Why: `tests_run`/`tests_passed` on [`FinishReport`] are integers the model
/// typed. This is what the machine printed.
/// What: `command` is the `bash` invocation's own command string; `lines` are
/// the verdict lines extracted from that call's tool result, capped by
/// [`EVIDENCE_LINE_CAP`] / [`EVIDENCE_LINE_CHARS`]; `truncated` says whether
/// the cap dropped anything, so a reader is never silently shown a subset;
/// `outcome` is [`EvidenceOutcome`]'s classification of `lines`.
/// Test: `verify_gate::evidence::tests::*`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestEvidence {
    /// The shell command whose output this is.
    pub command: String,
    /// The extracted verdict lines, in the order they were printed.
    pub lines: Vec<String>,
    /// Whether [`EVIDENCE_LINE_CAP`] dropped lines from `lines`.
    pub truncated: bool,
    /// What `lines` says about the run.
    pub outcome: EvidenceOutcome,
}

/// One changed file, as the model reported it (#8204).
///
/// Why: the TUI's changed-files slot renders a list of paths; it must never
/// have to find them inside a rendered `Changes:` block.
/// What: `file` is non-empty by construction — `agent_loop::finish_verify`
/// drops a `changes` entry with no path rather than emitting an
/// `<unknown file>` placeholder a reader would mistake for a real one.
/// Test: `agent_loop::finish_verify::tests::report_drops_a_change_with_no_path`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishChange {
    /// Path of the changed file.
    pub file: String,
    /// Lines added, when the model reported a count.
    pub lines_added: Option<i64>,
    /// Lines removed, when the model reported a count.
    pub lines_removed: Option<i64>,
}

/// One completed task's structured report (#8204, #8289).
///
/// Why: the payload `Event::TaskFinished` carries, so a client renders the
/// completion from typed fields rather than re-parsing
/// `tools::render_finish_summary`'s prose. #8182's subagent panel reads the
/// same value, keyed by `agent_id` on the carrying event.
/// What: `status` is `finish_task`'s own enum as its wire string
/// (`completed`/`failed`/`cancelled`). `summary`, `changes`, `tests_run`,
/// `tests_passed` are the model's self-report. `evidence` is the machine's
/// (`None` when no test command ran in this session at all). `verified` is
/// the one derived field: true ONLY when captured output states a pass, so a
/// reader that shows nothing else still cannot present an unbacked claim as
/// proven.
/// Test: `agent_loop::finish_verify::tests::*`,
/// `events_tests::task_finished_round_trips_through_json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishReport {
    /// `completed` / `failed` / `cancelled`.
    pub status: String,
    /// The model's free-text summary.
    pub summary: String,
    /// The files the model reported changing.
    pub changes: Vec<FinishChange>,
    /// The model's own count of tests run.
    pub tests_run: Option<i64>,
    /// The model's own count of tests passed.
    pub tests_passed: Option<i64>,
    /// The captured output of the test command that actually ran.
    pub evidence: Option<TestEvidence>,
    /// Whether captured output — not the model — states that tests passed.
    pub verified: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `EvidenceOutcome` keeps its snake_case wire spelling: the TUI
    /// slot picks its label from this string.
    #[test]
    fn outcome_round_trips_through_json() {
        for (outcome, wire) in [
            (EvidenceOutcome::Passed, "passed"),
            (EvidenceOutcome::Failed, "failed"),
            (EvidenceOutcome::Unverified, "unverified"),
        ] {
            let json = serde_json::to_value(outcome).expect("serialise");
            assert_eq!(json, serde_json::json!(wire));
            let back: EvidenceOutcome = serde_json::from_value(json).expect("deserialise");
            assert_eq!(back, outcome);
        }
    }

    /// A full report round-trips with every field intact, including the
    /// derived `verified` flag the TUI reads.
    #[test]
    fn report_round_trips_through_json() {
        let report = FinishReport {
            status: "completed".to_string(),
            summary: "did the thing".to_string(),
            changes: vec![FinishChange {
                file: "a.rs".to_string(),
                lines_added: Some(3),
                lines_removed: None,
            }],
            tests_run: Some(2),
            tests_passed: Some(2),
            evidence: Some(TestEvidence {
                command: "cargo test".to_string(),
                lines: vec!["test result: ok. 2 passed; 0 failed".to_string()],
                truncated: false,
                outcome: EvidenceOutcome::Passed,
            }),
            verified: true,
        };

        let json = serde_json::to_value(&report).expect("serialise");
        assert_eq!(json["verified"], serde_json::json!(true));
        assert_eq!(json["evidence"]["outcome"], serde_json::json!("passed"));
        let back: FinishReport = serde_json::from_value(json).expect("deserialise");
        assert_eq!(back, report);
    }
}
