//! Tool descriptors for the index-LIFECYCLE tools: `create_index`,
//! `add_root`, `delete_index`, `reindex`.
//!
//! Why: `descriptors.rs` had reached 467 SLOC against the 500-SLOC cap, and
//! #7434 adds a whole tool to it. The lifecycle group is the natural seam —
//! those four are the only descriptors that describe MUTATIONS of an index's
//! identity (its id, its root table, its existence) rather than a query over
//! one, and they change together whenever the daemon's index API does.
//! What: exports [`lifecycle_tool_descriptors`], a `Vec<Value>` of descriptor
//! objects appended to the base array by
//! [`super::descriptors::tool_descriptors`]. The schemas are verbatim what
//! lived in `descriptors.rs` before the split, plus `add_root` and
//! `create_index`'s `roots`.
//! Test: `every_lifecycle_tool_is_advertised_exactly_once` asserts every one of
//! these names survives the split exactly once;
//! `add_root_is_advertised_with_a_required_roots_array` and
//! `create_index_advertises_roots` in `tests_7434_add_root.rs` cover the #7434
//! additions.

use serde_json::Value;

/// The four index-lifecycle descriptors, in the order they are advertised.
///
/// Why: kept as a function rather than a `const` because each entry is a
/// `serde_json::Value` built by the `json!` macro, and because
/// [`super::descriptors::tool_descriptors`] mutates the assembled array
/// afterwards (#6317's directory annotation, #1373's pin annotation) — it
/// needs owned values, not a shared static.
/// What: returns `create_index`, `add_root`, `delete_index`, `reindex` as
/// descriptor objects carrying `name`, `description` and `inputSchema`.
/// Every one of them mutates index state, so `openrpc::scopes_for_tool`
/// classifies all four as `search.write`.
/// Test: `test_tools_list_complete`, `every_tool_has_scopes` (openrpc),
/// `every_lifecycle_tool_is_advertised_exactly_once`.
pub(super) fn lifecycle_tool_descriptors() -> Vec<Value> {
    vec![
        serde_json::json!({
            "name": "create_index",
            "description": "Register a new (empty) index. The reindex that populates it refuses any tree over the daemon's file-count / total-byte budget (TRUSTY_MAX_INDEX_FILES, default 50000; TRUSTY_MAX_INDEX_BYTES, default 2 GiB) rather than silently truncating it — narrow a large tree with exclude_globs. Pass `roots` to have the index span several directory trees from the outset (#7434).",
            "inputSchema": {
                "type": "object",
                "required": ["id", "root_path"],
                "properties": {
                    "id":        { "type": "string" },
                    "root_path": { "type": "string" },
                    // #7434: create-time additional roots. Same gate as `add_root`.
                    "roots": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Additional absolute directory trees this index also covers, beyond root_path. Each runs the same gate as `add_root`: absolute and existing, canonicalised, and refused with 409 when another index already covers it. `root_path` stays the primary root — it alone derives the index id and its colocated storage location — and files under an additional root are stored as `@root<n>/<relative path>` (#7434)."
                    },
                    "follow_links": {
                        "type": "boolean",
                        "description": "Dereference symlinks during the index walk. Default false (do not follow) — the safe choice for roots containing symlinks that escape the tree. Set true to index vendored / monorepo-aliased subtrees reached via a symlink."
                    },
                    "exclude_globs": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Glob patterns to exclude from the walk, on top of the built-in ignores (.gitignore, node_modules, .git, target, dist, build, ...). Use this to bring an oversized tree under the index budget, e.g. [\"**/fixtures/**\", \"**/*.generated.ts\"]."
                    }
                }
            }
        }),
        // #7434: the MCP door onto `POST /indexes/:id/roots`.
        serde_json::json!({
            "name": "add_root",
            "description": "Add one or more directory trees to an EXISTING index, so a single index spans several checkouts (a monorepo plus its vendored sibling, a repo plus its docs repo). Use this instead of registering a second index when the trees are one project to the reader: searches then return hits from every root in one ranked list. Each path must be absolute and exist; a tree the index already covers is an idempotent no-op, duplicates within one call collapse, and a tree ANOTHER index already covers — as its primary root or one of its additional ones — is refused (409) rather than indexed twice. On success the new tree starts being watched immediately and a background reindex is queued, so hits appear once that walk finishes rather than on return. The response carries the index's whole root table (`roots`, primary first) and just what this call appended (`added`); ordinals in that table are what `@root<n>/…` result paths refer to. There is deliberately no way to reorder or remove a root — those ordinals are baked into stored chunk paths.",
            "inputSchema": {
                "type": "object",
                "required": ["index_id", "roots"],
                "properties": {
                    "index_id": { "type": "string", "description": "Target index id (from `list_indexes`)" },
                    "roots": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Absolute directory paths to add to this index's root table."
                    }
                },
                "examples": [
                    { "index_id": "trusty-tools", "roots": ["/Users/me/src/vendored-lib"] }
                ]
            }
        }),
        serde_json::json!({
            "name": "delete_index",
            // #6422: the destructive default, and the opt-out beside it.
            "description": "Delete a registered index and all its on-disk data. \
                            Pass delete_data: false to deregister the index only \
                            and leave its corpus on disk for a later \
                            re-registration.",
            "inputSchema": {
                "type": "object",
                "required": ["index_id"],
                "properties": {
                    "index_id":    { "type": "string" },
                    "delete_data": { "type": "boolean", "default": true }
                }
            }
        }),
        serde_json::json!({
            "name": "reindex",
            "description": "Trigger a full reindex of a collection (async, returns immediately)",
            "inputSchema": {
                "type": "object",
                "required": ["index_id"],
                "properties": {
                    "index_id":  { "type": "string" },
                    "root_path": { "type": "string" }
                }
            }
        }),
    ]
}
