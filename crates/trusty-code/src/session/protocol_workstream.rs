//! `session.create`'s validate-then-mint-then-bind step (DOC-48 §4.1/§4.2,
//! issue #3298; atomicity fix per PR #3354 code-critic HIGH finding 1),
//! split out of `protocol.rs` purely to keep that production file under the
//! crate's 500-SLOC cap — a child module of `protocol` (declared via
//! `#[path = ...] mod protocol_workstream;`), so it shares full access to
//! `protocol`'s private items exactly as if this function were still
//! defined there.
//!
//! Why: `SessionRegistry` has no delete/rollback, so `create` must NOT mint
//! a session until the workstream target (if any) has been fully validated
//! — the first cut minted first and could then fail the bind, stranding an
//! orphaned, caller-invisible session in the registry on every rejected
//! bind (code-critic HIGH 1, PR #3354). This helper drives
//! `crate::workstreams::protocol::resolve_validate_bind`, which validates
//! every caller-triggerable failure mode (malformed/unknown/closed
//! `workstream_id`) BEFORE invoking the mint closure, all under one store
//! lock (TOCTOU-free — see that function's docs).
//!
//! What: [`mint_bound_session`] resolves+validates the effective workstream
//! target, mints the session via the caller's closure only once validation
//! passes, persists the bind, stamps it onto the registry via
//! `SessionRegistry::bind_workstream` (publishing `Event::SessionAdded`),
//! and returns the POST-bind `Session` snapshot as `Value` — so the
//! response always reflects the binding it just made, never a stale
//! pre-bind snapshot.
//!
//! Test: `super::workstream_binding_tests::*`, in particular
//! `create_rejected_bind_leaves_no_phantom_session` (the regression the
//! critic required).

use serde_json::{Value, json};

use crate::jsonrpc::RpcError;
use crate::workstreams::SharedWorkstreamStore;

use super::SessionRegistry;

/// Validate the workstream target, mint the session (only after validation
/// passes), persist + stamp its binding, and return its snapshot.
pub(super) async fn mint_bound_session<F>(
    registry: &SessionRegistry,
    workstreams: &SharedWorkstreamStore,
    workstream_id: Option<&str>,
    method: &str,
    mint: F,
) -> Result<Value, RpcError>
where
    F: FnOnce() -> String,
{
    let (session_id, target) = crate::workstreams::protocol::resolve_validate_bind(
        workstreams,
        workstream_id,
        method,
        mint,
    )
    .await?;
    if let Some(ws_id) = target {
        registry.bind_workstream(&session_id, ws_id)?;
    }
    Ok(json!(registry.status(&session_id)?))
}
