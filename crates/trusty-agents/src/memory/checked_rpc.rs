//! The one door trusty-agents uses to call the trusty-memory daemon (#9538).
//!
//! Why: #9288 made every first-party memory client verify the daemon's wire
//! protocol before its first call, so a daemon from another release is refused
//! by name instead of having its replies misparsed. trusty-agents has a dozen
//! call sites; routing them all through this module means the check cannot be
//! forgotten at a new one.
//! What: [`call`] and [`call_with_timeout`] run
//! `trusty_common::memory_rpc::ensure_memory_protocol_at` (cached per socket
//! for a few seconds; a daemon with no `memory.protocol` is refused) and then
//! `call_memory_tool_at_with_timeout`. A refusal comes back as an
//! `anyhow::Error` wrapping `MemoryProtocolError`, so a caller that formats
//! with `{e:#}` shows the named cause and one that downcasts can match it.
//! Test: `checked_rpc::tests`.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;
use trusty_common::memory_rpc::{
    DEFAULT_TIMEOUT, call_memory_tool_at_with_timeout, ensure_memory_protocol_at,
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

#[cfg(test)]
mod tests;
