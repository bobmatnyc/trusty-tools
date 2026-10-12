//! The `palace_backup` tool schema (#9282).
//!
//! Why: its own `json!` call, because one more tool in the main literal in
//! `definitions.rs` trips the macro recursion limit, and that file sits near
//! the 500-SLOC cap.
//! What: [`palace_backup_definition`]; spliced after `palace_rename` by
//! `tool_definitions_with`.
//! Test: `tool_definitions_lists_all_tools`, `mcp_schema_matches_snapshot`.

use serde_json::{json, Value};

/// The `palace_backup` schema (#9282).
///
/// Why: ADR-0066 makes the tool additive, so it takes optional parameters
/// only; a missing `palace` with no server default is an error at call time,
/// as for the other palace tools.
/// Test: `tool_definitions_lists_all_tools`, `mcp_schema_matches_snapshot`.
pub fn palace_backup_definition() -> Value {
    json!({
        "name": "palace_backup",
        "description": "#9282: write a consistent backup of one palace while the daemon serves it; writers are not paused. kg.redb and chat_sessions.redb are each copied from one read transaction; palace.json, identity.txt, the deletion journals and this palace's drawer_counts.jsonl lines are copied too; manifest.json (row counts and a SHA-256 per file) is written last. The vector index is NOT in a backup: after `trusty-memory palace restore`, run palace_reembed with dry_run=false before recall finds the drawers. Returns the backup directory, the manifest counts and the file list. Backups are never pruned.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "palace": {"type": "string", "description": "Palace id. Required unless the server was started with --palace."},
                "dest":   {"type": "string", "description": "Absolute path of the backup directory to create; it must not exist and must not sit directly in the palace registry directory (<data root>, or <data root>/palaces when that exists). Default: <data root>/.trusty-memory/backups/manual/<palace>/<UTC timestamp>/, or <data root>/backups/manual/<palace>/<UTC timestamp>/ when palaces live under <data root>/palaces."}
            },
            "required": []
        }
    })
}
