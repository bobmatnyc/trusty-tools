//! MCP `tools/list` schema for the `console_metrics` tool.
//!
//! Why (#9269): `tool_definitions_with` is compiled under `mcp-schema`, which
//! leaves out the `console_metrics` handler module; the descriptor has to live
//! beside the other schema modules for that build.
//! What: [`descriptor`], moved verbatim from `console_metrics` and re-exported
//! there as `console_metrics::descriptor`.
//! Test: `tool_definitions_lists_all_tools` counts it; `tests/generated_docs.rs`
//! pins its README row.

use serde_json::{json, Value};

/// JSON schema descriptor for the `console_metrics` MCP tool.
///
/// Why: Required by `tool_definitions_with()` so MCP clients can discover
/// the tool in `tools/list` responses and by the dispatcher so it can route
/// `tools/call` requests.
/// What: Returns a `serde_json::Value` matching the MCP tool schema shape
/// used by all other trusty-memory tools.
/// Test: Included in `tool_definitions_lists_all_tools` assertion count.
pub fn descriptor() -> Value {
    json!({
        "name": "console_metrics",
        "description": "Return a ConsoleMetricsReport with palace aggregate statistics \
            (palace_count, counted_palace_count, cached_palace_count, total_drawers, \
            total_vectors, total_rooms, total_kg_triples) and per-palace detail \
            (first 20). Counts come from the open-handle LRU cache for a resident \
            palace and from a read-only pass over the palace's redb files otherwise, \
            so a palace that is merely closed still reports real numbers; no palace is \
            opened to be counted. Each entry carries `cached` (was it resident) and \
            `stats_source` (`cache`, `disk`, or `unavailable`); an unavailable entry \
            carries `stats_error` and null counts. Each entry also carries \
            `last_used_unix` — when a recall, remember or note last touched that \
            palace, or null when none has since the stamp shipped, and `disk_bytes` \
            — that palace directory's allocated size. Schema 5 (#6928) adds this \
            daemon's own usage: `ram_bytes` (physical footprint, null where the OS \
            has no such counter), `disk_bytes` (allocated size of the whole palace \
            store, the figure `du -s` reports), `data_root`, and — present only where \
            the OS supplies them — the `ram_heap_bytes` / `ram_file_backed_bytes` / \
            `ram_compressed_bytes` split. Used by the trusty-console dashboard \
            metrics poller.",
        "inputSchema": {
            "type": "object",
            "properties": {},
            "required": []
        }
    })
}
