//! The bounded palace open every per-palace tool passes before it runs (#11613).
//!
//! Why: a call whose palace is not resident (a first open, a re-open after an
//! idle or LRU eviction) re-opened the palace inside the request. A large
//! palace re-opens slower than the client's 5 s exchange, so the client broke
//! its pipe while the daemon finished the call. #11255 bounded the open for
//! recall only, with a 15 s wait, which is still past that exchange.
//! What: [`gate`] runs before the handler of each tool in [`GATED_TOOLS`]. It
//! opens the palace through the shared, off-worker open of
//! [`super::recall_open::open_bounded`], waiting at most [`gate_wait`]. Past
//! that it answers [`super::recall_open::PalaceOpening`] (`-32007`) and the
//! open goes on in the background. The handler has not run, so a write that
//! receives `-32007` was not applied and is safe to re-send. Any other open
//! outcome lets the handler run; it opens inline and reports its own error, so
//! a failed background open surfaces as an error on the next ask.
//! Test: `tools::tests::palace_gate_tests`.

use std::time::Duration;

use anyhow::Result;
use serde_json::Value;

use crate::AppState;

use super::helpers::resolve_palace;
use super::recall_open::{PalaceOpening, open_bounded};

/// The longest the gate waits on an open, under the client's 5 s exchange.
///
/// Why: `trusty_common::memory_rpc::DEFAULT_TIMEOUT` is 5 s per exchange; 3 s
/// leaves room for the handler itself and the protocol round trip.
pub(crate) const EXCHANGE_SAFE_WAIT: Duration = Duration::from_secs(3);

/// The tools whose handler opens the palace named by `palace` (or the default).
pub(crate) const GATED_TOOLS: &[&str] = &[
    "kg_assert",
    "kg_list_subjects",
    "kg_query",
    "kg_retract_triple",
    "memory_forget",
    "memory_list",
    "memory_note",
    "memory_recall",
    "memory_recall_deep",
    "memory_remember",
    "task_add",
    "task_complete",
    "task_list",
];

/// The gate's wait: the recall open wait, capped at [`EXCHANGE_SAFE_WAIT`].
pub(crate) fn gate_wait(state: &AppState) -> Duration {
    state.recall_limits.open_wait.min(EXCHANGE_SAFE_WAIT)
}

/// Refuse `tool` with `PalaceOpening` when its palace cannot open in time.
///
/// Why: see the module doc.
/// What: `Ok(())` for an ungated tool, an unresolvable palace, a resident or
/// freshly opened palace, and an open that failed (the handler reports it).
/// `Err(PalaceOpening)` only when the wait elapsed; nothing else is done.
/// Test: `a_write_to_a_palace_opening_past_the_budget_is_refused_then_applied_once`,
/// `first_call_after_eviction_answers_within_the_exchange_budget`,
/// `a_failed_background_open_is_an_error_on_the_next_ask`.
pub(crate) async fn gate(state: &AppState, tool: &str, args: &Value) -> Result<()> {
    if !GATED_TOOLS.contains(&tool) {
        return Ok(());
    }
    let Ok(palace) = resolve_palace(state, args, tool) else {
        return Ok(());
    };
    match open_bounded(state, &palace, gate_wait(state)).await {
        Err(e) if PalaceOpening::find(&e).is_some() => Err(e),
        _ => Ok(()),
    }
}
