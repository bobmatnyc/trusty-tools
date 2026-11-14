//! The bounded palace open of a single-palace recall (#11255).
//!
//! Why: `memory_recall` and `memory_recall_deep` opened their palace inline on
//! a tokio worker. A cold open is synchronous redb and HNSW work, and callers
//! of the same palace queue on its open lock for up to the open-queue bound
//! (60 s), so each waiting call parked a worker and slowed every other request.
//! What: [`open_for_recall`] returns a resident palace inline, as before.
//! Otherwise it runs the open on the blocking pool and waits at most
//! [`crate::service::op_limits::RecallLimits::open_wait`]. Past that it answers
//! [`PalaceOpening`], a retryable error, and leaves the open running, so the
//! palace becomes resident and a retry finds it.
//! Test: `a_held_palace_open_answers_palace_opening_and_the_open_continues`,
//! `palace_opening_maps_to_its_own_rpc_code`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::AppState;
use crate::engine::palace::PalaceId;
use crate::engine::retrieval::PalaceHandle;

use super::helpers::open_palace_handle;

/// The JSON-RPC error code of [`PalaceOpening`] on the MCP and daemon wires.
///
/// Why: a client must tell "retry shortly" apart from a daemon fault
/// (`-32603`) without parsing the message.
pub const CODE_PALACE_OPENING: i32 = -32007;

/// A recall gave up waiting for its palace to open; the open goes on.
///
/// Why (#11255): the caller needs a distinct, retryable answer instead of a
/// hang or a generic failure.
/// What: names the palace and the wait that elapsed. [`Self::rpc_data`] is
/// the machine-readable `error.data` both wires send.
/// Test: `a_held_palace_open_answers_palace_opening_and_the_open_continues`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "palace '{palace}' is still opening after {waited:?}; the open continues, retry shortly (#11255)"
)]
pub struct PalaceOpening {
    /// The palace the recall named.
    pub palace: String,
    /// How long the recall waited before answering.
    pub waited: Duration,
}

impl PalaceOpening {
    /// The `error.data` object: `kind`, `retryable`, `palace`, `waited_ms`.
    pub fn rpc_data(&self) -> Value {
        json!({
            "kind": "palace_opening",
            "retryable": true,
            "palace": self.palace,
            "waited_ms": u64::try_from(self.waited.as_millis()).unwrap_or(u64::MAX),
        })
    }

    /// The `PalaceOpening` an error chain carries, if any.
    pub fn find(e: &anyhow::Error) -> Option<&Self> {
        e.downcast_ref::<Self>()
    }
}

/// Open `palace` for a recall without blocking a runtime worker (#11255).
///
/// Why: see the module doc.
/// What: a palace the registry holds under `palace` is returned inline, with
/// the same LRU promotion `open_palace_handle` gives it. Otherwise
/// `open_palace_handle` runs on the blocking pool; its own result (including
/// a not-found or open failure) is returned unchanged when it finishes within
/// `state.recall_limits.open_wait`. Past that wait the call fails with
/// [`PalaceOpening`] and the blocking open is left running, never cancelled.
/// A panicked open task is an error, never an empty result.
/// Test: `a_held_palace_open_answers_palace_opening_and_the_open_continues`.
pub(crate) async fn open_for_recall(state: &AppState, palace: &str) -> Result<Arc<PalaceHandle>> {
    if let Some(handle) = state.registry.get(&PalaceId::new(palace)) {
        return Ok(handle);
    }
    let wait = state.recall_limits.open_wait;
    let (owned, id) = (state.clone(), palace.to_string());
    let open = tokio::task::spawn_blocking(move || open_palace_handle(&owned, &id));
    match tokio::time::timeout(wait, open).await {
        Ok(Ok(opened)) => opened,
        Ok(Err(join)) => Err(anyhow!("open palace {palace}: open task failed: {join}")),
        // #11255: dropping the JoinHandle detaches the open; it still registers.
        Err(_elapsed) => Err(PalaceOpening {
            palace: palace.to_string(),
            waited: wait,
        }
        .into()),
    }
}
