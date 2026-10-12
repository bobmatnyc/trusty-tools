//! Write-tool input properties spliced into `tool_definitions` (#9144, #9390).
//!
//! Why: the main `json!` literal in `definitions.rs` is at the macro recursion
//! limit and that file is at the 500-SLOC cap, so properties added to the
//! write tools are built here and inserted after the literal.
//! What: [`splice`] adds `type` to `memory_remember`, `verify` to both
//! `memory_remember` and `memory_note`, and each write tool's body alias.
//! Test: `memory_remember_schema_advertises_the_type_enum`,
//! `memory_remember_schema_advertises_the_verify_argument`,
//! `write_schemas_advertise_the_text_content_alias`.

use serde_json::{json, Value};

/// Insert the spliced write-tool properties into `tools`.
///
/// Test: `memory_remember_schema_advertises_the_type_enum`,
/// `memory_remember_schema_advertises_the_verify_argument`,
/// `write_schemas_advertise_the_text_content_alias`.
pub(super) fn splice(tools: &mut [Value]) {
    for tool in tools.iter_mut() {
        let name = tool["name"].as_str().unwrap_or_default().to_string();
        let properties = &mut tool["inputSchema"]["properties"];
        if name == "memory_remember" {
            properties["type"] = remember_type_property();
            properties["content"] = alias_property("text");
        }
        if name == "memory_note" {
            properties["text"] = alias_property("content");
        }
        // #9390: both write tools accept a slot's re-verify command.
        if name == "memory_remember" || name == "memory_note" {
            properties["verify"] = verify_property();
        }
    }
}

/// The `type` property of `memory_remember`'s input schema (#9144).
///
/// Why: its own `json!` call, because the enum array in the main literal trips
/// the macro recursion limit (the same reason `palace_rename_definition`
/// splits).
/// What: an optional string, enum of the five writable drawer types, matched
/// case-insensitively by `helpers::resolve_type_arg`.
/// Test: `memory_remember_schema_advertises_the_type_enum`.
fn remember_type_property() -> Value {
    json!({
        "type": "string",
        "enum": ["Ruling", "Decision", "Status", "Turn", "Reference"],
        "description": "#9144: optional drawer type, matched case-insensitively. Ruling = a standing rule that holds until revoked; Decision = a choice made for a piece of work; Status = a point-in-time state report a newer one replaces; Turn = a record of one conversational turn; Reference = a pointer to an external source of truth. Recall treats Ruling and Decision as rulings, and weights Status by 0.8 and Turn by 0.7 unless the drawer holds a live `fact_key` slot. Omit it for the heuristic classifier (today's behaviour). Any other value, including UserFact, is an error and nothing is written."
    })
}

/// The `verify` property of the write tools' input schemas (#9390).
///
/// What: an optional string, stored per drawer as the KG triple
/// `drawer:<id> verify_with <command>` and reported by `memory_list`.
/// Test: `memory_remember_schema_advertises_the_verify_argument`.
fn verify_property() -> Value {
    json!({
        "type": "string",
        "description": "#9390: optional command that re-reads this slot's fact, e.g. `gh pr view 123 --json headRefOid`. Requires `fact_key`: a command on a write with no `fact_key`, or an empty one, is an error and nothing is written. Stored only when the slot is admitted (`tier: \"C\"`); `memory_list` reports it as `verify`. A later write to the same slot retires this drawer and closes its command; the new drawer carries only its own `verify`."
    })
}

/// An optional string property that is an alias of the documented field
/// `name` (#9746).
///
/// Why: `required` keeps naming only `name`, because several MCP clients
/// reject a top-level `anyOf` in a tool's input schema; the alias is listed
/// here so a client can still learn it.
/// What: the handler resolves both names with `helpers::resolve_aliased_str_arg`.
/// Test: `write_schemas_advertise_the_text_content_alias`.
fn alias_property(name: &str) -> Value {
    json!({
        "type": "string",
        "description": format!("#9746: alias of `{name}`. Send either name; `{name}` is the documented one. Both with the same value is accepted; both with different values is an error and nothing is written.")
    })
}
