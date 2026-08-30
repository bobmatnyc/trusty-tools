//! Index management tool arms: `index_file`, `remove_file`, `list_indexes`,
//! `create_index`, `delete_index`, `reindex`, `index_status`, `list_chunks`.
//!
//! Why: index lifecycle operations (register, populate, inspect, delete) form
//! a cohesive group that changes together when the daemon's index API evolves.
//! Keeping them separate from search and admin tools makes code review and
//! feature additions easier.
//! What: exports `dispatch_index_tool`, called from `call_tool` in `mod.rs`,
//! which routes the eight index-management tool names to their daemon endpoints.
//! Test: `tests.rs` — `missing_params_returns_invalid_params` and the
//! `tools/list` completeness tests cover all eight names.

use serde_json::Value;

use super::{
    types::{require_str, DispatchError},
    McpServer,
};

/// Resolve `index_id` for an index-management tool, defaulting to the pinned
/// index (#1373) when the caller omits it.
///
/// Why: a pinned trusty-search session (`serve --index <id>`) should let the
/// LLM run `index_status`, `reindex`, `list_chunks`, etc. without repeating the
/// project's index id, exactly as the search tools do. Centralising the
/// precedence here keeps every index arm consistent with `search`.
/// What: returns the caller's non-empty `index_id` argument, else the session's
/// pinned index, else an `InvalidParams` error naming the missing field AND the
/// tool that lists valid values for it (#5213 — an error that says only "field
/// missing" leaves the caller guessing an id, which is the failure #1373 pinned
/// the session to avoid in the first place).
/// Test: `resolve_index_id_prefers_explicit_then_pinned` pins the precedence
/// and `missing_index_id_error_names_list_indexes` the error text.
fn required_index_id(server: &McpServer, args: &Value) -> Result<String, DispatchError> {
    server
        .resolve_index_id(args)
        .ok_or_else(|| DispatchError::InvalidParams(super::types::MISSING_INDEX_ID.into()))
}

/// Read `key` as a non-empty array whose every entry is a string.
///
/// Why: `create_index` forwards `exclude_globs` verbatim into the HTTP body,
/// and the daemon deserialises it as `Option<Vec<String>>` — a caller that
/// passed `[1, 2]` would get a 422 from the daemon instead of an MCP-level
/// answer. Validating here keeps the wire body well-typed.
/// What: all-or-nothing. Returns `None` when the key is absent, is not an
/// array, is empty, or holds ANY non-string entry. The last case used to drop
/// just the offending entries, so `["a", 1, "b"]` registered an index filtered
/// by two globs when the caller wrote three — a quieter outcome for the same
/// typo that `[1, 2]` already rejected whole.
/// Test: `create_index_forwards_exclude_globs`,
/// `create_index_omits_malformed_exclude_globs` in `tests.rs`.
fn string_array(args: &Value, key: &str) -> Option<Vec<Value>> {
    let items = args.get(key)?.as_array()?;
    (!items.is_empty() && items.iter().all(Value::is_string)).then(|| items.clone())
}

/// Read `delete_index`'s `delete_data` opt-out.
///
/// Why (#6422): the owner ruling made purging the on-disk data the default on
/// every delete-index surface, so an absent argument means DELETE THE DATA and
/// `false` is the explicit deregister-only opt-out. This is a deliberate
/// behaviour change for remote callers: an argument-free `delete_index` used to
/// reach `?delete_data=true` too, but the schema now advertises the choice, so
/// the value a caller sends has to be read rather than assumed.
///
/// What: absent or `null` ⇒ `true`. A boolean is honoured. Anything else is an
/// `InvalidParams` error rather than a coerced value — #4123 established that a
/// destructive toggle is never guessed at, and coercing `"false"` to the
/// default would destroy the corpus a caller asked to keep.
/// Test: `delete_index_purges_data_by_default`,
/// `delete_index_honours_the_deregister_only_opt_out`,
/// `delete_index_rejects_a_non_boolean_delete_data`.
fn delete_data_arg(args: &Value) -> Result<bool, DispatchError> {
    match args.get("delete_data") {
        None | Some(Value::Null) => Ok(true),
        Some(Value::Bool(b)) => Ok(*b),
        Some(other) => Err(DispatchError::InvalidParams(format!(
            "delete_data must be a boolean (true deletes the on-disk data, \
             false deregisters only); got {other}"
        ))),
    }
}

/// Route one of the eight index-management tool names to the correct daemon
/// call.
///
/// Why: grouping index management separately from search and admin lets each
/// file stay focused and under the 500-line cap.
/// What: returns `None` when `tool` is not an index-management tool (so
/// `call_tool` can try the next group), `Some(Ok(value))` on success, or
/// `Some(Err(DispatchError))` on failure.
/// Test: `tools_list_returns_all_tools` and `missing_params_returns_invalid_params`
/// in `tests.rs` exercise all returned tool names and error paths.
pub(super) async fn dispatch_index_tool(
    server: &McpServer,
    tool: &str,
    args: &Value,
) -> Option<Result<Value, DispatchError>> {
    match tool {
        "index_file" => {
            let index_id = match required_index_id(server, args) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let path = match require_str(args, "path") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let content = match require_str(args, "content") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(
                server
                    .post(
                        &format!("/indexes/{index_id}/index-file"),
                        &serde_json::json!({ "path": path, "content": content }),
                    )
                    .await,
            )
        }
        "remove_file" => {
            let index_id = match required_index_id(server, args) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let path = match require_str(args, "path") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(
                server
                    .post(
                        &format!("/indexes/{index_id}/remove-file"),
                        &serde_json::json!({ "path": path }),
                    )
                    .await,
            )
        }
        // Issue #312: request details=true so the response includes
        // per-index size_bytes in addition to the id list.
        "list_indexes" => Some(server.get("/indexes?details=true").await),
        "create_index" => {
            let id = match require_str(args, "id") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let root_path = match require_str(args, "root_path") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut body = serde_json::json!({ "id": id, "root_path": root_path });
            // Optional symlink policy. Omitted ⇒ the daemon default (`false`,
            // do NOT follow symlinks — the safe default for new indexes). Pass
            // `follow_links: true` to index vendored / monorepo-aliased subtrees
            // reached through a symlink.
            if let Some(follow) = args.get("follow_links").and_then(Value::as_bool) {
                body["follow_links"] = Value::Bool(follow);
            }
            // #4356: `POST /indexes` has accepted `exclude_globs` since the
            // repo-config work, but this tool never forwarded it, so an MCP
            // caller could only register a whole tree and hope the built-in
            // `SKIP_DIRS` + `.gitignore` filters were enough.
            if let Some(globs) = string_array(args, "exclude_globs") {
                body["exclude_globs"] = Value::Array(globs);
            }
            Some(server.post("/indexes", &body).await)
        }
        "delete_index" => {
            let index_id = match required_index_id(server, args) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            // #6422: the on-disk data goes by default, and `delete_data: false`
            // is the explicit deregister-only opt-out. The daemon's own default
            // is still the opposite (#4123), so the flag is always sent.
            let delete_data = match delete_data_arg(args) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(
                server
                    .delete(&format!("/indexes/{index_id}?delete_data={delete_data}"))
                    .await,
            )
        }
        "reindex" => {
            let index_id = match required_index_id(server, args) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            // Accept optional root_path override (mirrors the HTTP body).
            let mut body = serde_json::json!({});
            if let Some(rp) = args.get("root_path").and_then(Value::as_str) {
                body["root_path"] = Value::String(rp.to_string());
            }
            Some(
                server
                    .post(&format!("/indexes/{index_id}/reindex"), &body)
                    .await,
            )
        }
        "index_status" => {
            let index_id = match required_index_id(server, args) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            // #4715: `index_status` on a never-indexed pin 404'd the same way
            // `search` did; it gets the same honest not-ready answer.
            Some(
                server
                    .get_scoped(&format!("/indexes/{index_id}/status"), Some(&index_id))
                    .await,
            )
        }
        "list_chunks" => {
            // Issue #54 — paginated enumeration of an index's corpus.
            // Mirrors `GET /indexes/:id/chunks?offset=&limit=&after=`.
            // Issue #1325: an optional `after` cursor switches to an indexed
            // redb seek (O(page) at any depth) instead of the O(offset) scan;
            // the daemon echoes `next_cursor` for the next call.
            let index_id = match required_index_id(server, args) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(100);
            let mut query: Vec<(&str, String)> =
                vec![("offset", offset.to_string()), ("limit", limit.to_string())];
            if let Some(after) = args.get("after").and_then(Value::as_str) {
                // The cursor is a chunk id (`path:start:end`) — reqwest's
                // `.query()` percent-encodes it so reserved chars don't break
                // the query string.
                query.push(("after", after.to_string()));
            }
            // #4715: index-scoped like its `index_status` neighbour — a
            // never-indexed pin gets the same not-ready answer here.
            Some(
                server
                    .get_query_scoped(
                        &format!("/indexes/{index_id}/chunks"),
                        &query,
                        Some(&index_id),
                    )
                    .await,
            )
        }
        _ => None,
    }
}
