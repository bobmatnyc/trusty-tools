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
//! palace becomes resident and a retry finds it. Callers that miss the cache
//! together share one open ([`RecallOpens`]), so retries add no open attempts.
//! Test: `a_held_palace_open_answers_palace_opening_and_the_open_continues`,
//! `palace_opening_maps_to_its_own_rpc_code`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::AppState;
use crate::engine::palace::PalaceId;
use crate::engine::palace_not_found::PalaceNotFound;
use crate::engine::registry::OpenQueueTimeout;
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

/// The least time between two joins of one recall's opens (#11255 review, M1).
const REJOIN_FLOOR: Duration = Duration::from_millis(50);

/// The outcome one shared open publishes to every recall waiting on it.
type SharedOpen = Result<Arc<PalaceHandle>, Arc<anyhow::Error>>;

/// The recall opens in flight, one per palace name (#11255 review, F2).
///
/// Why: each recall that missed the cache spawned its own blocking open, and
/// each such task queued on the palace's open lock for up to the 60 s queue
/// bound after its caller had already been told [`PalaceOpening`]. Every
/// retry added a parked blocking thread, and a failed open (not cached,
/// #3992) was repeated by each one.
/// What: [`Self::join`] hands back a receiver for the open of `palace`,
/// starting one blocking task only when none is in flight under that name.
/// The task publishes its outcome to every receiver and leaves the map, so
/// the next miss starts a fresh attempt: a failure never poisons a later one.
/// Test: `concurrent_recalls_of_a_stalled_open_share_one_open_attempt`.
#[derive(Default)]
pub struct RecallOpens {
    inflight: parking_lot::Mutex<HashMap<String, watch::Receiver<Option<SharedOpen>>>>,
    started: AtomicUsize,
}

impl RecallOpens {
    /// How many blocking opens this map has started; the test seam.
    pub fn opens_started(&self) -> usize {
        self.started.load(Ordering::Relaxed)
    }

    /// Join the in-flight open of `palace`, or start it.
    fn join(state: &AppState, palace: &str) -> watch::Receiver<Option<SharedOpen>> {
        let opens = Arc::clone(&state.recall_opens);
        let mut inflight = opens.inflight.lock();
        if let Some(rx) = inflight.get(palace) {
            return rx.clone();
        }
        let (tx, rx) = watch::channel(None);
        inflight.insert(palace.to_string(), rx.clone());
        drop(inflight);
        opens.started.fetch_add(1, Ordering::Relaxed);
        let (owned, id) = (state.clone(), palace.to_string());
        tokio::task::spawn_blocking(move || {
            // Leaves the map even if the open panics (the sender then drops).
            let leave = Leave(&owned.recall_opens, &id);
            let outcome = open_palace_handle(&owned, &id).map_err(Arc::new);
            // #11255 review, N2: leave the map BEFORE publishing, so a caller
            // woken by the outcome, or arriving after it, can only start or
            // join a fresh open, never this finished one.
            drop(leave);
            tx.send_replace(Some(outcome));
        });
        rx
    }
}

/// Removes one palace's entry from [`RecallOpens`] when its open ends.
struct Leave<'a>(&'a RecallOpens, &'a str);

impl Drop for Leave<'_> {
    fn drop(&mut self) {
        self.0.inflight.lock().remove(self.1);
    }
}

/// A shared open's failure, as each waiting recall receives it.
///
/// Why: `anyhow::Error` is not `Clone`, and the waiters need the failure's
/// chain intact so the wire still maps a format refusal (`-32006`).
/// What: displays as `open palace <id> failed`; its source is the original
/// error's own root, so the rest of the chain is unchanged. An absence is
/// re-raised as its own [`PalaceNotFound`] by [`shared_failure`], so `-32004`
/// holds too.
#[derive(Debug)]
struct SharedOpenFailure {
    palace: String,
    cause: Arc<anyhow::Error>,
}

impl std::fmt::Display for SharedOpenFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "open palace {} failed", self.palace)
    }
}

impl std::error::Error for SharedOpenFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&**self.cause)
    }
}

/// The error a waiter on a failed shared open answers with.
fn shared_failure(palace: &str, e: &Arc<anyhow::Error>) -> anyhow::Error {
    // #11255 review: an absence keeps its type, so it still answers -32004.
    if let Some(absent) = e.downcast_ref::<PalaceNotFound>() {
        return absent.clone().into();
    }
    SharedOpenFailure {
        palace: palace.to_string(),
        cause: Arc::clone(e),
    }
    .into()
}

/// Open `palace` for a recall without blocking a runtime worker (#11255).
///
/// Why: see the module doc.
/// What: a palace the registry holds under `palace` is returned inline, with
/// the same LRU promotion `open_palace_handle` gives it. Otherwise the call
/// joins the one in-flight open of `palace` ([`RecallOpens`]), which runs on
/// the blocking pool, and waits at most `state.recall_limits.open_wait` for
/// its outcome: the handle, or its failure (see [`shared_failure`]). A
/// shared open that hit the registry's open-queue bound ([`OpenQueueTimeout`])
/// is not this call's failure: the call joins a fresh open for what is left
/// of its own wait, no sooner than [`REJOIN_FLOOR`] after its last join.
/// Past that wait the call fails with [`PalaceOpening`]; the
/// open goes on, never cancelled. An open task that ends without an outcome
/// (a panic) is an error, never an empty result.
/// Test: `a_held_palace_open_answers_palace_opening_and_the_open_continues`,
/// `concurrent_recalls_of_a_stalled_open_share_one_open_attempt`,
/// `a_failed_shared_open_reaches_its_waiters_and_a_later_open_succeeds`,
/// `a_joiner_past_the_open_queue_bound_answers_palace_opening`,
/// `a_zero_queue_wait_rejoins_no_faster_than_the_floor`,
/// `a_joiner_of_a_failed_open_gets_the_open_failure`,
/// `a_joiner_of_a_missing_palace_open_answers_not_found`.
pub(crate) async fn open_for_recall(state: &AppState, palace: &str) -> Result<Arc<PalaceHandle>> {
    if let Some(handle) = state.registry.get(&PalaceId::new(palace)) {
        return Ok(handle);
    }
    let wait = state.recall_limits.open_wait;
    let deadline = tokio::time::Instant::now() + wait;
    let opening = || -> anyhow::Error {
        PalaceOpening {
            palace: palace.to_string(),
            waited: wait,
        }
        .into()
    };
    loop {
        let joined = tokio::time::Instant::now();
        let mut rx = RecallOpens::join(state, palace);
        let shared = match tokio::time::timeout_at(deadline, rx.wait_for(Option::is_some)).await {
            Ok(Ok(done)) => done.clone(),
            Ok(Err(_closed)) => None,
            // #11255: the open is left running; it still registers the palace.
            Err(_elapsed) => return Err(opening()),
        };
        match shared {
            Some(Ok(handle)) => return Ok(handle),
            // #11255 review, N1: the queue bound means "busy", not "failed";
            // wait out this call's own deadline on a fresh open. A fresh open
            // starts after the last one left the map (N2), so it waits a whole
            // queue bound, and the deadline ends the loop.
            Some(Err(e)) if e.downcast_ref::<OpenQueueTimeout>().is_some() => {
                // #11255 review, M1: a zero queue wait (a zero write-op
                // budget) must not re-join as fast as `spawn_blocking` returns.
                let floor = (joined + REJOIN_FLOOR).min(deadline);
                tokio::time::sleep_until(floor).await;
                if floor >= deadline {
                    return Err(opening());
                }
            }
            Some(Err(e)) => return Err(shared_failure(palace, &e)),
            None => {
                return Err(anyhow!(
                    "open palace {palace}: open task ended without an outcome"
                ));
            }
        }
    }
}
