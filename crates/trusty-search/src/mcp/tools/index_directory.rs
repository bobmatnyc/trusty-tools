//! The answer a read tool gives when no `index_id` resolves (issue #6317).
//!
//! Why: an unpinned session that omits `index_id` used to get
//! [`super::types::MISSING_INDEX_ID`] — an error naming `list_indexes`. The
//! caller then had to make a second call to learn what it could have been told
//! in the first. Worse, a model that will not spend the round-trip guesses an
//! id instead, which is the wrong-index failure #1373 exists to prevent. The
//! owner ruling (2026-08-27) is that a read tool with nothing to resolve
//! answers with an index of what exists, as a successful result.
//! What: [`index_directory`] builds that answer from `GET /indexes?details=true`
//! — the same rows `list_indexes` serves — plus a `hint` naming how to retry.
//! [`annotate_directory_tools`] makes the advertised schema agree: `index_id`
//! stops being `required` on those tools and both descriptions say what an
//! omitted id does.
//! Test: `tests_index_directory.rs`.

use serde_json::Value;

use super::{types::DispatchError, McpServer};

/// The read tools that answer an unresolvable index with a directory (#6317).
///
/// Why: the issue names six — `search`, `search_lexical`, `search_semantic`,
/// `typeahead`, `grep`, `index_status`. `grep` is absent here because it
/// already satisfies the ruling: an unpinned `grep` with no id fans out over
/// `POST /grep` rather than erroring (#3805). `search_kg` is present because it
/// shares [`super::search::run_lane_search`] with the two named lanes, and a
/// read lane that errors where its siblings answer would be the drift this
/// module exists to prevent.
/// What: the single list both the dispatcher and the descriptor annotation read,
/// so behaviour and advertised schema cannot disagree. Every entry is
/// `search.read` per [`crate::mcp::openrpc::scopes_for_tool`]; no mutating tool
/// appears, and none may be added.
/// Test: `directory_tools_are_all_read_scoped`,
/// `unresolved_read_tools_return_the_index_directory`.
pub(super) const DIRECTORY_TOOLS: &[&str] = &[
    "search",
    "search_lexical",
    "search_semantic",
    "search_kg",
    "typeahead",
    "index_status",
];

/// The `status` field marking a directory answer, so a caller can branch on it
/// without matching prose.
///
/// Why: the payload is a SUCCESS, which means it reaches the model in the same
/// shape as a hit list. Without a discriminator an orchestrator cannot tell
/// "here are your options" from "here are your results" except by absence of a
/// field, which is exactly the ambiguity #4715 rejected for `INDEX_NOT_READY`.
/// What: a free constant, emitted as `status` on every directory payload.
/// Test: `unresolved_read_tools_return_the_index_directory`.
pub(super) const NO_INDEX_RESOLVED: &str = "no_index_resolved";

/// Build the successful directory payload for a tool that resolved no index.
///
/// Why: see the module doc — the owner ruling requires the discovery answer
/// inline rather than an error pointing at discovery.
/// What: GETs `/indexes?details=true` (the `list_indexes` body: `id`,
/// `root_path`, `size_bytes`, `repo_identity`, `last_used_unix`) and returns
/// `{status, tool, hint, indexes}`. Chunk counts are deliberately absent — they
/// live behind a per-index `GET /indexes/:id/status`, so including them would
/// turn one call into N. A daemon that cannot answer the listing still yields a
/// `Transport` error; this path invents nothing.
/// Test: `unresolved_read_tools_return_the_index_directory`,
/// `empty_daemon_directory_points_at_create_index`.
pub(super) async fn index_directory(
    server: &McpServer,
    tool: &str,
) -> Result<Value, DispatchError> {
    let listed = server.get("/indexes?details=true").await?;
    let indexes = listed
        .get("indexes")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let empty = indexes.as_array().is_none_or(Vec::is_empty);
    let hint = if empty {
        format!(
            "`{tool}` resolved no index: this session has no pinned index, the call \
             passed no `index_id`, and this daemon has no registered indexes. Register \
             one with `create_index` (or `trusty-search index add <root>`), then retry."
        )
    } else {
        format!(
            "`{tool}` resolved no index: this session has no pinned index and the call \
             passed no `index_id`, so here is what exists instead of an error. Retry \
             with `index_id` set to one of `indexes[].id` below, or start the server \
             pinned with `trusty-search serve --index <id>`."
        )
    };
    Ok(serde_json::json!({
        "status": NO_INDEX_RESOLVED,
        "tool": tool,
        "hint": hint,
        "indexes": indexes,
    }))
}

/// The sentence appended to each directory tool's own description.
const TOOL_NOTE: &str = "Omitting `index_id` on an unpinned session is not an error: the call \
                         returns the list of available indexes plus a hint to retry with one \
                         of them (issue #6317).";

/// The sentence appended to each directory tool's `index_id` property.
const PROP_NOTE: &str = "Omitted, it resolves to this session's pinned index when one is set \
                         (#1373); with no pin the call returns the available indexes and a \
                         retry hint rather than failing (#6317).";

/// Make the advertised schema agree with the dispatcher for [`DIRECTORY_TOOLS`].
///
/// Why: leaving `index_id` in `required` would keep a schema-obeying client from
/// ever issuing the call the ruling makes valid — the feature would be
/// unreachable from the surface that advertises it. Applying the edit from the
/// same const the dispatcher reads is what keeps the two from drifting as tools
/// are added.
/// What: for every tool named in [`DIRECTORY_TOOLS`], drops `index_id` from
/// `required` and appends [`PROP_NOTE`] / [`TOOL_NOTE`] to the `index_id`
/// property and tool descriptions. Tools outside the list are untouched, so
/// every mutating tool keeps `index_id` required.
/// Test: `directory_tools_drop_required_index_id`,
/// `mutating_tools_keep_required_index_id`.
pub(super) fn annotate_directory_tools(defs: &mut Value) {
    let Some(tools) = defs.as_array_mut() else {
        return;
    };
    for tool in tools.iter_mut() {
        let named = tool
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|n| DIRECTORY_TOOLS.contains(&n));
        if !named {
            continue;
        }
        if let Some(desc) = tool.get("description").and_then(Value::as_str) {
            // Normalise the join to exactly one period + space, whether or not
            // the base already ends in one — the same shape
            // `tool_descriptors_pinned` uses for its note.
            let base = desc.trim_end().trim_end_matches('.');
            tool["description"] = Value::String(format!("{base}. {TOOL_NOTE}"));
        }
        let Some(schema) = tool.get_mut("inputSchema").and_then(Value::as_object_mut) else {
            continue;
        };
        if let Some(required) = schema.get_mut("required").and_then(Value::as_array_mut) {
            required.retain(|v| v.as_str() != Some("index_id"));
        }
        if let Some(prop) = schema
            .get_mut("properties")
            .and_then(Value::as_object_mut)
            .and_then(|p| p.get_mut("index_id"))
            .and_then(Value::as_object_mut)
        {
            let base = prop
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("Target index id (from `list_indexes`)");
            let base = base.trim_end().trim_end_matches('.');
            prop.insert(
                "description".into(),
                Value::String(format!("{base}. {PROP_NOTE}")),
            );
        }
    }
}
