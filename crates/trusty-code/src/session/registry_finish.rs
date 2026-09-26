//! `SessionRegistry::record_task_finished` — the structured completion
//! report's publish path (#8204, #8289). Split out of `registry.rs` for the
//! same 500-SLOC-cap reason as `registry_events.rs`; a child module of
//! `registry` (declared via `#[path = ...] mod finish_ops;`), so it shares
//! access to `SessionRegistry`'s private `ensure_exists`/`record` helpers.
//!
//! Why: `record_tool_finished` already publishes the `finish_task` call, but
//! carries only its rendered prose. A client that wants the changed-file
//! list, the test counts, or the captured test output would have to re-parse
//! that text. This publishes the typed value instead.
//! What: [`SessionRegistry::record_task_finished`] records
//! `Event::TaskFinished` and nothing else — unlike `set_agent_todos` there is
//! no roster state to keep in step, because a completion is an occurrence,
//! not a value a later poll can read back.
//! Test: `tests::*`.

use super::*;
use crate::finish_report::FinishReport;

impl SessionRegistry {
    /// Publish `agent_id`'s accepted `finish_task` report on session `id`
    /// (#8204).
    ///
    /// Why: the one publish path for a completion report, so the event and
    /// the loop's own output are built from the SAME value
    /// (`agent_loop::finish_verify::report`).
    /// What: `Err(session_not_found)` if `id` is unknown; otherwise records
    /// the event verbatim. `agent`/`agent_id` attribute the completion, so a
    /// delegated sub-agent's report is distinguishable from the PM's.
    /// Test: `tests::record_task_finished_publishes_the_typed_report`,
    /// `tests::record_task_finished_unknown_session_errors`.
    pub fn record_task_finished(
        &self,
        id: &str,
        agent: &str,
        agent_id: &str,
        report: FinishReport,
    ) -> Result<(), RpcError> {
        self.ensure_exists(id)?;
        self.record(
            id,
            Event::TaskFinished {
                session_id: id.to_string(),
                agent: agent.to_string(),
                agent_id: agent_id.to_string(),
                report,
            },
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finish_report::{EvidenceOutcome, FinishChange, TestEvidence};

    fn report() -> FinishReport {
        FinishReport {
            status: "completed".to_string(),
            summary: "added the flag".to_string(),
            changes: vec![FinishChange {
                file: "crates/a/src/lib.rs".to_string(),
                lines_added: Some(10),
                lines_removed: Some(2),
            }],
            tests_run: Some(12),
            tests_passed: Some(12),
            evidence: Some(TestEvidence {
                command: "cargo test -p a".to_string(),
                lines: vec!["test result: ok. 12 passed; 0 failed".to_string()],
                truncated: false,
                outcome: EvidenceOutcome::Passed,
            }),
            verified: true,
        }
    }

    /// The report reaches an attached client AS DATA — every field readable
    /// without touching a rendered string.
    #[test]
    fn record_task_finished_publishes_the_typed_report() {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        registry
            .record_task_finished(&session.id, "engineer", "eng-1", report())
            .expect("record");

        let replay = registry.replay(&session.id).expect("replay");
        let streamed = replay
            .iter()
            .find_map(|e| match &e.event {
                Event::TaskFinished {
                    agent,
                    agent_id,
                    report,
                    ..
                } => Some((agent.clone(), agent_id.clone(), report.clone())),
                _ => None,
            })
            .expect("a task_finished event must be recorded");

        assert_eq!(streamed.0, "engineer");
        assert_eq!(streamed.1, "eng-1");
        assert_eq!(streamed.2.changes[0].file, "crates/a/src/lib.rs");
        assert_eq!(streamed.2.tests_passed, Some(12));
        assert!(streamed.2.verified);
        assert_eq!(
            streamed.2.evidence.expect("evidence").lines[0],
            "test result: ok. 12 passed; 0 failed"
        );
    }

    /// An unknown session maps to `-32007 session_not_found` rather than
    /// silently dropping the report.
    #[test]
    fn record_task_finished_unknown_session_errors() {
        let registry = SessionRegistry::new();

        let err = registry
            .record_task_finished("nope", "engineer", "eng-1", report())
            .expect_err("unknown session");

        assert_eq!(err.code, -32007);
    }
}
