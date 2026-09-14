//! Compact field mode for MCP search hits (#7676).
//!
//! Why: a hit envelope is 48-72% metadata. `content` and `compact_snippet`
//! carry the same source text twice for a small chunk, and ten more fields
//! ride along that an identifier lookup never reads — measured at 130-318
//! tokens returned per hit for 67-88 tokens of ground truth, and one
//! conceptual `search` call spilled 77k characters. The caller cannot trim
//! what the tool already sent.
//!
//! What: [`wants_compact`] reads the tool-level `compact` flag, and
//! [`apply`] removes [`DROPPED_FIELDS`] from every object in the response's
//! `results` array. The keys are absent, not null. Everything outside
//! `results` — `meta`, `intent`, `latency_ms`, the fan-out counters — is
//! untouched.
//!
//! This is an MCP-surface transform, not a daemon one. `SearchQuery.compact`
//! already defaults to `true` over HTTP, so making field-dropping follow that
//! flag would change the shape every HTTP integrator reads today; the crate's
//! CLAUDE.md documents that surface as an integrator contract. The MCP layer
//! instead pins `compact: true` in the daemon body (so `compact_snippet` is
//! guaranteed present before `content` is dropped) and trims the response it
//! returns.
//!
//! Test: `tests_compact.rs`.

use serde_json::Value;

use super::types::DispatchError;

/// The hit fields compact mode removes.
///
/// `content` is redundant with `compact_snippet`; `id` and `file` are
/// derivable from `path` plus the line range; the rest are KG / ranking
/// metadata an identifier or outline lookup does not read. `path`,
/// `start_line`, `end_line`, `compact_snippet`, `score`, and `match_reason`
/// survive — plus `index_id`, which is the only way to tell which project a
/// fan-out hit came from.
pub(super) const DROPPED_FIELDS: [&str; 10] = [
    "calls",
    "chunk_depth",
    "chunk_type",
    "content",
    "file",
    "function_name",
    "id",
    "inherits_from",
    "language",
    "on_branch",
];

/// `true` when the caller asked for compact hits. Absent or `null` is
/// `false`, so an existing caller's response shape does not change.
///
/// A present `compact` that is not a boolean is rejected rather than read as
/// `false`: `compact: "true"` used to return full hits byte-identical to a
/// call that never asked for compaction, with nothing in the response to say
/// the flag was dropped. Mirrors the `full` / `max_bytes` rejection in
/// [`super::byte_cap`].
///
/// Test: `a_non_boolean_compact_is_rejected`.
// #7676: a silently ignored `compact` is indistinguishable from no `compact`.
pub(super) fn wants_compact(args: &Value) -> Result<bool, DispatchError> {
    match args.get("compact") {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(other) => Err(DispatchError::InvalidParams(format!(
            "compact must be a boolean (true drops `content` and the KG/ranking \
             metadata from every hit); got {other}"
        ))),
    }
}

/// Drop [`DROPPED_FIELDS`] from every hit in `resp["results"]`.
///
/// A response with no `results` array (an error body, an index directory) is
/// left alone, so this is safe to call unconditionally on any tool result.
pub(super) fn apply(resp: &mut Value) {
    let Some(results) = resp.get_mut("results").and_then(Value::as_array_mut) else {
        return;
    };
    for hit in results {
        let Some(map) = hit.as_object_mut() else {
            continue;
        };
        for field in DROPPED_FIELDS {
            map.remove(field);
        }
    }
}
