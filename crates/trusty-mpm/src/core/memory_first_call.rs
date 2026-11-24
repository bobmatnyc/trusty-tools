//! A palace verb's first call after the daemon idled the palace out (#10648).
//!
//! Why: trusty-memory drops a palace's handle after `TRUSTY_MEMORY_IDLE_EVICT_SECS`
//! (default 300 s) idle, and the next call re-opens it inside the request: drawer
//! load, HNSW replay, stranded scan. On a loaded host that open outran the
//! client's 5 s exchange budget. The stalling side is the daemon's palace
//! re-open, not accept or connect: each `uds rpc connection failed ... Broken
//! pipe` in the daemon log (2026-10-29 18:09Z, 10-30 09:12Z, 09:17Z, 21:14Z,
//! 21:15Z, 21:17Z) follows within 50 ms the open-time warning `vector index has
//! orphaned entries palace=trusty-tools`, which only `PalaceHandle::open` emits.
//! The open finished, the call ran, and its answer met a client that had hung
//! up; the retry found the palace warm.
//! What: [`is_retryable_read`] names the read methods that
//! `memory_checked::call_with_timeout` re-sends once on an exchange timeout;
//! every other method, every write among them, is sent once. A write is never
//! re-sent, because the daemon finishes a write whose client left (the log
//! above shows it did) and a second send could store it twice. Instead
//! [`call_verb`] warms a write's palace with a read (`palace_info`, itself
//! retried once), so the write's own exchange meets an open palace. A warm-up
//! that times out twice fails the verb before the write is sent.
//! Test: `core::memory_first_call_tests`.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};
use trusty_common::uds::rpc::UdsRpcError;

use super::memory_checked;

/// The read a write sends first to make the daemon open its palace.
pub const WARM_METHOD: &str = "palace_info";

/// Methods that only read, so a timed-out exchange may be sent again.
///
/// An allowlist: a method not named here, including any new write, is never
/// re-sent.
const RETRYABLE_READS: &[&str] = &["memory_recall", "memory_list", WARM_METHOD];

/// May `method` be re-sent after its exchange timed out?
pub(crate) fn is_retryable_read(method: &str) -> bool {
    RETRYABLE_READS.contains(&method)
}

/// Is `e` an exchange that did not complete inside its budget?
///
/// Why: only a timeout is the cold-open stall; a dial failure, a refusal or a
/// protocol mismatch is a real failure that a retry must not hide.
/// What: walks the `anyhow` chain for [`UdsRpcError::Timeout`].
/// Test: `a_refusal_is_not_retried`.
pub(crate) fn is_exchange_timeout(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<UdsRpcError>(),
            Some(UdsRpcError::Timeout { .. })
        )
    })
}

/// Call a palace verb's `method`, surviving a cold palace open.
///
/// Why: see the module doc (#10648).
/// What: a read (`is_write == false`) goes straight to
/// `memory_checked::call_with_timeout`, which retries a listed read once on
/// exchange timeout. A write naming a `palace` first sends [`WARM_METHOD`] for
/// it, with the same retry, then sends the write exactly once. A warm-up that
/// times out twice returns that error and the write is never sent; any other
/// warm-up error is ignored, because the write reports its own failure (a new
/// palace, for one, has no `palace_info` yet).
/// Test: `a_read_retries_once_after_a_stalled_first_exchange`,
/// `a_write_warms_its_palace_then_sends_once`,
/// `a_write_is_not_sent_when_the_warm_up_times_out_twice`,
/// `a_stalled_write_is_not_resent`, `a_refusal_is_not_retried`.
pub async fn call_verb(
    socket: &Path,
    method: &str,
    params: Value,
    is_write: bool,
    timeout: Duration,
) -> Result<Value> {
    if is_write && let Some(palace) = params.get("palace").and_then(Value::as_str) {
        let warm = json!({ "palace": palace });
        if let Err(e) = memory_checked::call_with_timeout(socket, WARM_METHOD, warm, timeout).await
        {
            // #10648: still stalled after a retry; the write stays unsent.
            if is_exchange_timeout(&e) {
                return Err(e);
            }
        }
    }
    memory_checked::call_with_timeout(socket, method, params, timeout).await
}
