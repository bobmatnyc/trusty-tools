//! The one door trusty-mpm's memory-socket clients call the daemon through (#9538).
//!
//! Why: #9288 made every first-party memory client verify the daemon's wire
//! protocol before its first call, so a daemon from another release is refused
//! by name instead of having its replies misparsed. This crate has a dozen call
//! sites; routing them through here means a new one cannot forget the check.
//! What: [`call`] and [`call_with_timeout`] run
//! `trusty_common::memory_rpc::ensure_memory_protocol_at` (cached per socket for
//! a few seconds; a pre-handshake daemon stays callable) and then
//! `call_memory_tool_at_with_timeout`. A refusal is an `anyhow::Error` wrapping
//! `MemoryProtocolError`: `{e:#}` shows the named cause, [`protocol_mismatch`]
//! recovers it.
//! Test: `core::memory_protocol_gate_tests`.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;
use trusty_common::memory_rpc::{
    DEFAULT_TIMEOUT, MemoryProtocolError, call_memory_tool_at_with_timeout,
    ensure_memory_protocol_at,
};

/// [`call_with_timeout`] with the shared client's default budget.
pub async fn call(socket: &Path, method: &str, params: Value) -> Result<Value> {
    call_with_timeout(socket, method, params, DEFAULT_TIMEOUT).await
}

/// Check the daemon's protocol, then call `method` on it.
///
/// Why: see the module doc. `timeout` bounds the protocol query and the call
/// separately; the query is skipped while a callable verdict is cached. A
/// read's first exchange can stall on a cold palace open (#10648).
/// What: one attempt; a read listed by `memory_first_call::is_retryable_read`
/// whose exchange timed out is sent once more. A write is never re-sent.
/// Test: `call_with_timeout_retries_a_stalled_first_recall`,
/// `a_read_stalled_twice_is_an_error`, `a_stalled_write_is_not_resent`.
///
/// # Errors
///
/// `MemoryProtocolError` (wrapped) when the daemon speaks an unsupported
/// protocol or the check fails; otherwise as
/// `trusty_common::memory_rpc::call_memory_tool_at_with_timeout`.
pub async fn call_with_timeout(
    socket: &Path,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value> {
    use super::memory_first_call::{is_exchange_timeout, is_retryable_read};

    if !is_retryable_read(method) {
        return call_once(socket, method, params, timeout).await;
    }
    match call_once(socket, method, params.clone(), timeout).await {
        // #10648: a timed-out read is safe to send again; a second timeout surfaces.
        Err(e) if is_exchange_timeout(&e) => {
            tracing::debug!(method, "#10648: memory read timed out; retrying once");
            call_once(socket, method, params, timeout).await
        }
        other => other,
    }
}

/// One protocol-checked attempt at `method`.
async fn call_once(socket: &Path, method: &str, params: Value, timeout: Duration) -> Result<Value> {
    ensure_memory_protocol_at(socket, timeout).await?;
    call_memory_tool_at_with_timeout(socket, method, params, timeout).await
}

/// The protocol MISMATCH inside `e`, when that is what failed.
///
/// Why: a probe reports a daemon from another release as degraded with its
/// reason, which it can only do after telling it from "nothing answered". A
/// `HandshakeFailed` check is a dead or broken transport, not a mismatch, so it
/// is not returned and the caller's existing unreachable handling applies.
/// What: `Some` for `Unsupported` and `MalformedHandshake` only.
/// Test: `core::memory_protocol_gate_tests`, `unreachable_socket_answers_false`.
pub fn protocol_mismatch(e: &anyhow::Error) -> Option<&MemoryProtocolError> {
    e.downcast_ref::<MemoryProtocolError>().filter(|p| {
        matches!(
            p,
            MemoryProtocolError::Unsupported { .. }
                | MemoryProtocolError::MalformedHandshake { .. }
        )
    })
}
