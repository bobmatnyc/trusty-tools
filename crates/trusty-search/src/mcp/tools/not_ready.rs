//! The `INDEX_NOT_READY` error contract (issue #4715).
//!
//! Why: a session pinned to a worktree that has never been indexed used to get
//! the daemon's `404 unknown index` verbatim. "Unknown" reads as permanent —
//! the caller gives up or silently falls back and the gap stays invisible. The
//! honest answer is "too early": the index will exist, it just does not yet.
//! This module gives that state its own machine-readable error so a caller can
//! branch on it instead of pattern-matching an English string.
//!
//! What: the JSON-RPC error code, the payload builder, the `tools/call`
//! envelope wrapper, and [`McpServer::classify_index_miss`] — the single place
//! that decides whether a daemon 404 is "not indexed yet" or a genuine unknown
//! index. Mirrors the shape of the `STAGE_NOT_READY` contract (issue #138) so
//! the tool surface has one error convention, not two.
//!
//! Test: `mcp/tools/tests_not_ready.rs`.

use serde_json::Value;

use super::{types::DispatchError, McpServer};

/// Application-level JSON-RPC error code for "the target index has not been
/// built yet" (issue #4715).
///
/// Why: sits in the JSON-RPC 2.0 server-reserved range (`-32099` ..= `-32000`),
/// one slot below [`super::STAGE_NOT_READY_CODE`], so an orchestrator can
/// branch on the numeric code alone and never collides with a transport-level
/// code. The semantic distinction is the whole point: `METHOD_NOT_FOUND` /
/// "unknown index" is permanent, this is retryable.
/// What: a free integer constant emitted on bare-method invocations. The
/// `tools/call` form carries the same condition as `_meta.error_code =
/// "INDEX_NOT_READY"`, per MCP's in-band error convention.
/// Test: `bare_method_search_on_unindexed_pin_returns_index_not_ready_code`.
pub const INDEX_NOT_READY_CODE: i32 = -32011;

/// Machine-readable discriminator carried in `_meta.error_code` / `error.data`.
pub const INDEX_NOT_READY: &str = "INDEX_NOT_READY";

/// The single readiness state this contract reports (issue #4715).
///
/// Why: "indexing in progress" and "index exists but is stale" are deliberately
/// NOT states here. An in-progress index already answers — partially, and the
/// per-lane `STAGE_NOT_READY` contract (issue #138) plus the daemon's
/// `503 index_loading` already describe it. A stale index answers too, and the
/// caller's action does not change. Only "never indexed" produces no answer at
/// all, so it is the only state a caller can act on differently.
/// What: the literal written into the `state` field of every payload.
/// Test: `not_ready_payload_carries_state_reason_and_fallback`.
pub const STATE_NOT_INDEXED: &str = "not_indexed";

/// Filesystem tools a caller should reach for while the index is missing.
///
/// Why: the owner's ruling on #4715 — the caller must learn the fallback from
/// the error itself rather than knowing it out of band. These are the CALLER's
/// own tools, not this server's: trusty-search's `grep` tool is index-backed
/// under a pinned session and reports the same not-ready state, so naming it
/// here would send the agent in a circle.
/// What: the value of the payload's `suggested_fallback` array.
/// Test: `not_ready_payload_carries_state_reason_and_fallback`.
const SUGGESTED_FALLBACK: [&str; 2] = ["grep", "find"];

impl McpServer {
    /// Decide whether a daemon `404` on an index-scoped call is "not indexed
    /// yet" rather than a genuine unknown index (issue #4715).
    ///
    /// Why: the daemon cannot tell the two apart — from its side both are "no
    /// such id". The MCP layer can, because it knows which id it ADVERTISED to
    /// the caller as this session's default (`tools/list` annotates the pinned
    /// index, #1373). If the daemon 404s on the id it told the caller to use,
    /// the state is "not built yet"; any other 404 is a caller-supplied id that
    /// really does not exist, and stays a plain transport error.
    /// What: returns `Some(DispatchError::IndexNotReady { .. })` when
    /// `index_id` is exactly the session's pinned index, else `None` so the
    /// caller falls through to its existing error path unchanged. Pure
    /// comparison over data already held — it has no fallible step that could
    /// degrade a real failure into a success or an empty result.
    ///
    /// **Invariant this depends on.** Only route an endpoint through the
    /// `*_scoped` HTTP helpers when its daemon handler returns 404 *only* for
    /// an id absent from the hot registry, the cold store, AND the failed set.
    /// A handler that 404s on a bare hot-registry miss reports a cold-parked
    /// index — one that was built and merely is not resident — as one that was
    /// never built, which is this bug pointed the other way. `service::server::
    /// tests_4715` pins that rule for every currently-routed handler.
    ///
    /// Test: `classify_index_miss_only_fires_for_the_pinned_index`;
    /// `cold_parked_index_status_is_503_not_404` guards the invariant.
    pub(super) fn classify_index_miss(&self, index_id: Option<&str>) -> Option<DispatchError> {
        let id = index_id?;
        if self.pinned_index.as_deref() != Some(id) {
            return None;
        }
        Some(DispatchError::IndexNotReady {
            message: index_not_ready_message(id),
            payload: index_not_ready_payload(id),
        })
    }
}

/// Human-readable text shown to the model alongside the structured payload.
///
/// Why: the model reads `content[]` prose first; it must say "retry later, use
/// grep now" in plain words as well as in the machine-readable block.
/// What: names the index, states the condition is transient, contrasts it with
/// an unknown index, and gives the fallback.
/// Test: `not_ready_message_says_retryable_and_names_the_fallback`.
fn index_not_ready_message(index_id: &str) -> String {
    format!(
        "Index '{index_id}' has not been built yet, so there is nothing to search. \
         This is a transient state, not an unknown index: the daemon advertised \
         this id as the session default and it will exist once indexing runs. \
         Retry later, and meanwhile take one of three ways forward, in order of \
         usefulness: (1) call `list_indexes` and pass an `index_id` that exists — \
         search works normally against any built index; (2) build this one with \
         `create_index` + `reindex`; (3) use your OWN filesystem tools \
         ({fallback}). Do NOT reach for \
         trusty-search's `grep` tool as the fallback — it is index-backed and in a \
         pinned session reports this same state, so it sends you in a circle.",
        fallback = SUGGESTED_FALLBACK.join(" or "),
    )
}

/// Structured payload carried in `_meta` (`tools/call`) or `error.data` (bare).
///
/// Why: the ruling requires the response to carry the state, the reason, and
/// the suggested fallback as data — an agent must not have to pattern-match a
/// message string to know it may retry.
/// What: a flat JSON object with `error_code`, `state`, `index_id`,
/// `retryable`, `reason`, `suggested_fallback`, and `next_steps` (#5213).
///
/// #5213: `suggested_fallback` alone made this a fail-open dressed as an error.
/// It named `grep`, an agent read that as trusty-search's `grep` TOOL, that tool
/// is index-backed under the same pin, and it reported the same failure — the
/// loss of search capability round-tripped into advice that could not work. The
/// prose warned about it; the machine-readable field did not, and the field is
/// what an agent branches on. `next_steps.discover` is the fix the owner ruling
/// asked for: point at `list_indexes` so the caller can reach a real index
/// instead of only being told to give up on this one.
/// Test: `not_ready_payload_carries_state_reason_and_fallback`,
/// `not_ready_payload_points_at_list_indexes_not_only_a_fallback`.
fn index_not_ready_payload(index_id: &str) -> Value {
    serde_json::json!({
        "error_code": INDEX_NOT_READY,
        "state": STATE_NOT_INDEXED,
        "index_id": index_id,
        "retryable": true,
        "reason": "the daemon advertised this index as the session default, but no \
                   index has been built for it yet",
        "suggested_fallback": SUGGESTED_FALLBACK,
        "fallback_scope": "caller's own filesystem tools — NOT trusty-search's \
                           index-backed `grep` tool, which reports this same state",
        "next_steps": {
            "discover": "list_indexes — enumerate the index ids that DO exist on this \
                         daemon, then retry with an explicit index_id",
            "build": "create_index then reindex — build this id",
        },
    })
}

/// Wrap an `INDEX_NOT_READY` failure in MCP's structured tool-error envelope.
///
/// Why: `tools/call` signals failures in band with `isError: true`. Emitting
/// the payload under `_meta` — exactly as `STAGE_NOT_READY` does — means a
/// client that already understands one understands the other, and it can never
/// be mistaken for a successful search that happened to return no results.
/// What: returns `{isError: true, content: [text], _meta: <payload>}`.
/// Test: `tools_call_search_on_unindexed_pin_returns_structured_not_ready`.
pub(super) fn wrap_index_not_ready_error(message: &str, payload: &Value) -> Value {
    serde_json::json!({
        "isError": true,
        "content": [{
            "type": "text",
            "text": message,
        }],
        "_meta": payload,
    })
}
