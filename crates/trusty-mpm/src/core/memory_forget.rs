//! `tm memory forget <drawer-id>` — drawer removal over the daemon socket (#9340).
//!
//! Why: QA and operators write throwaway drawers through `tm memory
//! remember`/`note`, and expiry cannot drop them — a retired slot's
//! `expires_at` clears and a refused slot stays as an ordinary drawer. The only
//! cleanup path was a hand-built `memory_forget` call on the socket.
//! What: the forget-specific halves of [`super::memory_verbs::MemoryVerb::Forget`]
//! — the drawer-id check that runs before any RPC, and [`forget_failure`],
//! which reads the daemon's answer. The call itself goes through
//! [`super::memory_verbs::run_verb`], so palace and socket resolution are the
//! ones `recall`/`remember`/`note` use. There is no `--fact-key` alias: no
//! trusty-memory tool resolves a slot to its drawer id (the slot index is
//! internal to the KG store), and `memory_list` does not report `fact_key`.
//! Test: `memory_verbs_tests.rs`, `tests/memory_verbs_socket.rs`.

use serde_json::Value;

use super::memory_verbs::MemoryVerbError;

/// The trusty-memory method `tm memory forget` calls.
pub const FORGET_METHOD: &str = "memory_forget";

/// The `status` `memory_forget` reports when a drawer was removed.
const DELETED: &str = "deleted";

/// Reject a drawer id that is not a UUID, before anything is sent.
///
/// Why: the daemon rejects it too, but its error would arrive wrapped as
/// "trusty-memory did not answer", which reads as a transport fault.
/// Test: `a_malformed_drawer_id_is_refused_before_any_rpc`.
pub(crate) fn validate_drawer_id(drawer_id: &str) -> Result<(), MemoryVerbError> {
    uuid::Uuid::parse_str(drawer_id.trim())
        .map(|_| ())
        .map_err(|e| MemoryVerbError::DrawerId {
            value: drawer_id.to_string(),
            detail: e.to_string(),
        })
}

/// Why a forget the daemon answered did not remove a drawer, if it did not.
///
/// Why: `memory_forget` answers an unknown id with a SUCCESSFUL body,
/// `{"status": "not_found"}` (#5231, `handle_memory_forget`). Exiting 0 on that
/// would tell a cleanup script it removed a drawer it never touched.
/// What: `None` only when `status` is `"deleted"`. Any other status, or none,
/// returns a message naming the drawer, the palace and the status.
/// Test: `forget_failure_accepts_only_deleted`,
/// `an_unknown_drawer_exits_non_zero_and_says_nothing_was_deleted`.
pub fn forget_failure(drawer_id: &str, palace: Option<&str>, result: &Value) -> Option<String> {
    let status = result.get("status").and_then(Value::as_str);
    if status == Some(DELETED) {
        return None;
    }
    let palace = palace.unwrap_or("(none resolved)");
    Some(match status {
        Some("not_found") => {
            format!("drawer {drawer_id} not found in palace {palace}; nothing was deleted")
        }
        Some(other) => format!(
            "trusty-memory answered status {other:?} for drawer {drawer_id} in palace {palace}; \
             it did not report the drawer deleted"
        ),
        None => format!(
            "trusty-memory answered without a status for drawer {drawer_id} in palace {palace}; \
             it did not report the drawer deleted"
        ),
    })
}
