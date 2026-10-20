//! The `palace_migrate` tool schema (#10094).
//!
//! Why: its own `json!` call, like `backup_definitions.rs`: one more tool in
//! the main literal in `definitions.rs` trips the macro recursion limit.
//! What: [`palace_migrate_definition`]; spliced after `palace_backup` by
//! `tool_definitions_with`.
//! Test: `tool_definitions_lists_all_tools`, `mcp_schema_matches_snapshot`.

use serde_json::{Value, json};

/// The `palace_migrate` schema (#10094).
///
/// Why: ADR-0066 makes the tool additive, so every parameter is optional.
/// Test: `tool_definitions_lists_all_tools`, `mcp_schema_matches_snapshot`.
pub fn palace_migrate_definition() -> Value {
    json!({
        "name": "palace_migrate",
        "description": "#10094: migrate a palace to this binary's format through the daemon's own migration chain, as `trusty-memory palace migrate` does. mode=migrate (default) takes a verified backup under <data root>/backups/format-migration/<palace>/, runs each format step, then checks that the format is current, the kg.redb marker and the palace.json mirror agree, no format_migration.json remains and the drawer and triple counts are unchanged; it deletes nothing, resumes an interrupted migration, and leaves a current palace alone. mode=dry_run reports the found and target format, the planned backup, its size and the free space, and writes nothing. mode=verify runs the checks only and reports each marker as present, missing or mismatched, and whether a backup exists; it writes nothing. all=true covers every palace; an all=true migration that would change more than one palace is refused unless yes=true. The report's `ok` is false when a check failed.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "palace": {"type": "string", "description": "Palace id. Required unless all=true or the server was started with --palace."},
                "all":    {"type": "boolean", "description": "Every palace under the data root. Default false."},
                "mode":   {"type": "string", "enum": ["migrate", "dry_run", "verify"], "description": "migrate (default), dry_run or verify."},
                "yes":    {"type": "boolean", "description": "Confirms an all=true migration that changes more than one palace. Default false."}
            },
            "required": []
        }
    })
}
