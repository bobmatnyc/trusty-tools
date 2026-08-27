//! Tool schemas for the embed-audit surface (#5000, #4786).
//!
//! Why a sibling module rather than two more entries in `definitions.rs`: that
//! file's single `json!` array already sits at the macro recursion limit, and
//! adding to it fails the build with `recursion limit reached while expanding
//! json_internal`. Splicing is the pattern the task, chat, room and wing groups
//! already use, for the 500-SLOC cap; this group joins it for a second reason.
//! What: returns `[palace_verify_embedded, palace_embed_sweep]`, conditioned on
//! `has_default` the same way every other group is.
//! Test: spliced into `tool_definitions_with`; covered by
//! `tool_definitions_lists_all_tools` in `tools::tests`.

use serde_json::{json, Value};

/// Build the two embed-audit tool schemas.
///
/// Why the descriptions carry as much as they do: both tools exist because a
/// caller reached for a cheaper answer and got a wrong one — `memory_recall` as
/// a proxy for "is it findable", `console_metrics` as a health sweep. The schema
/// is where a model reads that, so it says which cheaper answer is wrong and
/// what to use instead.
/// Test: `tool_definitions_lists_all_tools`.
pub(super) fn embed_audit_tool_definitions(has_default: bool) -> Vec<Value> {
    let palace_required: Vec<&str> = if has_default {
        vec!["drawer_ids"]
    } else {
        vec!["palace", "drawer_ids"]
    };
    vec![
        json!({
            "name": "palace_verify_embedded",
            "description": "#5000: answer whether YOUR OWN drawer ids are vector-findable. Gate a migration or deletion on the single `verified` boolean — it is true only when every id you asked about is embedded AND the alias audit is clean, so a drawer lost to an id collision (which has a vector key and is still unreachable) cannot pass it. The three lists say why: `missing` exists with no vector, `unknown` is not a drawer in this palace at all. `memory_recall` is NOT a substitute — it can hit lexically and pass on a drawer no vector search will ever return. Reach for `palace_reembed` instead when you want the palace's whole missing set rather than an answer about specific ids.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "palace": {"type": "string"},
                    "drawer_ids": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Drawer UUIDs to verify. A malformed entry is refused, never skipped."
                    }
                },
                "required": palace_required,
            }
        }),
        json!({
            "name": "palace_embed_sweep",
            "description": "#5000 / #4786: vector coverage for EVERY palace on disk, uncapped. Use this rather than `console_metrics` for a health sweep — that report caps at 20 palaces and shows an uncached palace as 0/0, which reads as healthy, so a fully-unembedded palace was invisible to it. Act on `unhealthy` (palaces with missing vectors or a dirty alias audit) and `unreadable` (palaces nothing is known about); both are blocks for a deletion workflow. Never compare `drawer_count` to `vector_count` — #5005 disproved that gap in both directions.",
            "inputSchema": {"type": "object", "properties": {}}
        }),
    ]
}
