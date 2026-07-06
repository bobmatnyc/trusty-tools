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
//! valid, empty transcript, not an error.
//! Test: `session::registry_tests::get_transcript_*`.

use serde::{Deserialize, Serialize};

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
/// Test: `session::registry_tests::get_transcript_returns_stored_record`,
/// `session::registry_tests::get_transcript_on_never_run_session_is_empty`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptRecord {
    pub session_id: String,
    pub turns: Vec<TurnRecord>,
    pub usage: TokenUsage,
    pub cost_usd: Option<f64>,
}
