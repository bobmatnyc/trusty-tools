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
//! `MemoryProtocolError`: `{e:#}` shows the named cause, [`protocol_error`]
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
/// separately; the query is skipped while a callable verdict is cached.
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
    ensure_memory_protocol_at(socket, timeout).await?;
    call_memory_tool_at_with_timeout(socket, method, params, timeout).await
}

/// The protocol refusal inside `e`, when that is what failed.
///
/// Why: a probe reports a mismatch as degraded with its reason, which it can
/// only do after telling it from "nothing answered".
pub fn protocol_error(e: &anyhow::Error) -> Option<&MemoryProtocolError> {
    e.downcast_ref::<MemoryProtocolError>()
}
