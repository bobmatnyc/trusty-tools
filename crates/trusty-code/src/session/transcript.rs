//! `session.get_transcript`'s response shape (#2058).
//!
//! Why: `SessionRegistry::set_run_outcome` (#2056) already persists a
//! finished execution's turn-by-turn transcript, aggregate token usage, and
//! priced cost onto the `SessionEntry` — this is the M1 cut-line's "inspect
//! transcript" verb, exposing that stored record over the wire without
//! recomputing anything. Kept as its own small file (rather than growing
//! `registry.rs`) so the wire shape has one obvious home and stays decoupled
//! from the registry's storage/locking internals.
//! What: [`TranscriptRecord`] is a plain, `Serialize`+`Deserialize` DTO
//! (#2060: the `Deserialize` half lets `tcode`'s CLI thin client — see
//! `crate::cli_client` — parse `session.get_transcript`'s JSON-RPC result
//! straight back into this type rather than picking fields off a raw
//! `serde_json::Value`): `session_id`,
//! the ordered `turns` (each already `role`/`model`/`text`/`tool_calls`/
//! `usage` — see `crate::run_task::TurnRecord`), the aggregate `usage`, and
//! `cost_usd` exactly as stored (`None` either because pricing was
//! unavailable for the run, or because no run has ever completed on this
//! session — the two cases are indistinguishable here by design, matching
//! `RunReport::cost_usd`'s existing convention). A session that has never
//! run a task returns `turns: []`, `usage` all-zero, `cost_usd: null` — a
//! valid, empty transcript, not an error. `mode` (#2059) is the resolved
//! `HarnessMode` `task.run` set via `SessionRegistry::set_mode` — `None` for
//! the same "never run" case. `compaction_events` (#2349, epic #2343) is the
//! session's `pm_transcript`'s cumulative count of #2308 threshold-compactor
//! fires — `0` for a never-run session, and the field epic #2343's success
//! metric ("500+ turn session with `compaction_events == 0`") reads.
//! Test: `session::registry_tests::get_transcript_*`.

use serde::{Deserialize, Serialize};

use crate::mode::HarnessMode;
use crate::perf::TokenUsage;
use crate::run_task::TurnRecord;

/// The full stored run record for one session, as `session.get_transcript`
/// returns it.
///
/// Why: see module docs — this is the read-only DTO wrapping whatever
/// `SessionRegistry::set_run_outcome` last stored (or the all-empty default
/// for a session that has never run a task).
/// What: field-for-field passthrough of `SessionEntry`'s transcript/usage/
/// cost, plus the `session_id` for a self-describing response (the wire
/// method already takes `session_id` as a param, but echoing it back keeps
/// the result self-contained for a caller inspecting the JSON alone).
/// `mode` (#2059) mirrors `Session.mode` — the same field, read at
/// `get_transcript` time. `compaction_events` (#2349) mirrors
/// `agent_loop::Transcript::compaction_events()` on the session's
/// `pm_transcript` — `0` when that transcript has never fired the #2308
/// threshold compactor, or the session has never run.
/// Test: `session::registry_tests::get_transcript_returns_stored_record`,
/// `session::registry_tests::get_transcript_on_never_run_session_is_empty`,
/// `session::registry_tests::get_transcript_reports_compaction_events`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptRecord {
    pub session_id: String,
    pub turns: Vec<TurnRecord>,
    pub usage: TokenUsage,
    pub cost_usd: Option<f64>,
    pub mode: Option<HarnessMode>,
    pub compaction_events: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (#2349) `compaction_events` must round-trip through JSON exactly like
    /// every other field — this is the wire contract every
    /// `session.get_transcript` caller (the CLI thin client, external RPC
    /// clients) relies on.
    #[test]
    fn compaction_events_round_trips_through_json() {
        let record = TranscriptRecord {
            session_id: "s-1".to_string(),
            turns: vec![],
            usage: TokenUsage::default(),
            cost_usd: None,
            mode: None,
            compaction_events: 7,
        };

        let json = serde_json::to_value(&record).expect("serialize");
        assert_eq!(json["compaction_events"], 7);

        let round_tripped: TranscriptRecord = serde_json::from_value(json).expect("deserialize");
        assert_eq!(round_tripped.compaction_events, 7);
    }
}
