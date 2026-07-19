//! `session.create`'s workstream-binding step (DOC-48 §4.1/§4.2, issue
//! #3298), split out of `protocol.rs` purely to keep that production file
//! under the crate's 500-SLOC cap — a child module of `protocol` (declared
//! via `#[path = ...] mod protocol_workstream;`), so it shares full access
//! to `protocol`'s private items exactly as if this function were still
//! defined there.
//!
//! Why: `create`'s own body would otherwise inline the
//! resolve-then-bind-then-snapshot sequence; factoring it into one function
//! keeps `create` itself short and makes the sequence a single named unit
//! `task::protocol::task_run`'s mint path could, in principle, share too
//! (it does not today only because its own resolve/bind call sites differ
//! slightly around the reuse-vs-mint branch — see that module's docs).
//!
//! What: [`bind_and_snapshot`] resolves the effective workstream target via
//! `crate::workstreams::protocol::resolve_and_bind_session`, stamps it onto
//! the registry via `SessionRegistry::bind_workstream` when one was
//! resolved, and returns the POST-bind `Session` snapshot as `Value` — so a
//! caller's response always reflects the binding it just made, never a
//! stale pre-bind snapshot.

use serde_json::{Value, json};

use crate::jsonrpc::RpcError;
use crate::workstreams::SharedWorkstreamStore;

use super::SessionRegistry;

/// Resolve + persist `session_id`'s workstream binding, then return its
/// current snapshot.
pub(super) async fn bind_and_snapshot(
    registry: &SessionRegistry,
    workstreams: &SharedWorkstreamStore,
    session_id: &str,
    workstream_id: Option<&str>,
    method: &str,
) -> Result<Value, RpcError> {
    let target = crate::workstreams::protocol::resolve_and_bind_session(
        workstreams,
        workstream_id,
        session_id,
        method,
    )
    .await?;
    if let Some(ws_id) = target {
        registry.bind_workstream(session_id, ws_id)?;
    }
    Ok(json!(registry.status(session_id)?))
}
