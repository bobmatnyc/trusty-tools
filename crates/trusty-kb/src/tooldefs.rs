//! Static `tools/list` descriptors for the trusty-kb MCP surface.
//!
//! Why: the tool surface is fixed at compile time (no dynamic registry), so a
//! plain literal is the honest representation and keeps the input schemas the
//! single source of truth for what `tools/call` accepts. Kept in its own module
//! so [`crate::mcp`]'s dispatch logic stays readable.
//!
//! What: [`tool_definitions`] returns the eight-tool `{tools:[…]}` payload. Every
//! tree-scoped tool accepts the optional `root` + `agent` parameters that drive
//! per-call root resolution (explicit root → agent-mapped tree → service
//! default); `kb_list_trees` is service-scoped and takes none.
//!
//! Test: `tool_list_names_are_stable` (in `crate::mcp`).

use serde_json::{Value, json};

/// The `root`/`agent` properties every tree-scoped tool shares.
fn root_props() -> Value {
    json!({
        "root": {
            "type": "string",
            "description": "Explicit KB tree root path. Highest precedence; overrides `agent`."
        },
        "agent": {
            "type": "string",
            "description": "Assistant name; resolves to <knowledge_dir>/<slug(agent)>. Used when `root` is absent."
        }
    })
}

/// Merge the shared root props into a tool-specific property set.
fn with_root(mut props: Value) -> Value {
    if let (Value::Object(dst), Value::Object(src)) = (&mut props, root_props()) {
        for (k, v) in src {
            dst.insert(k, v);
        }
    }
    props
}

/// The static `tools/list` result.
pub fn tool_definitions() -> Value {
    json!({
        "tools": [
            {
                "name": "kb_status",
                "description": "Tree overview: collections, entity counts, index.md presence, and files failing frontmatter validation.",
                "inputSchema": {
                    "type": "object",
                    "properties": with_root(json!({})),
                    "additionalProperties": false
                }
            },
            {
                "name": "kb_list_trees",
                "description": "Enumerate the KB trees under the knowledge directory (name, entity count, last-updated). Service-scoped; takes no root.",
                "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
            },
            {
                "name": "kb_put_entity",
                "description": "Create or merge an entity file deterministically (sorted keys, change-detected `updated`), then materialise inverse relationship edges. Merge unions arrays and never drops existing keys.",
                "inputSchema": {
                    "type": "object",
                    "properties": with_root(json!({
                        "collection": { "type": "string", "description": "Collection directory, e.g. people/organizations/projects." },
                        "name": { "type": "string", "description": "Entity display name; slugged for the filename and used as the default title." },
                        "frontmatter": { "type": "object", "description": "Frontmatter keys to set/merge (OKF: only `type` is ever defaulted)." },
                        "body": { "type": "string", "description": "Markdown body. On merge, appended under a dated section idempotently." },
                        "merge": { "type": "boolean", "description": "Deep-merge into the existing entity (default false = overwrite)." }
                    })),
                    "required": ["collection", "name"],
                    "additionalProperties": false
                }
            },
            {
                "name": "kb_get_entity",
                "description": "Read one entity's parsed frontmatter + body.",
                "inputSchema": {
                    "type": "object",
                    "properties": with_root(json!({
                        "collection": { "type": "string" },
                        "name": { "type": "string" }
                    })),
                    "required": ["collection", "name"],
                    "additionalProperties": false
                }
            },
            {
                "name": "kb_list",
                "description": "List entities (optionally scoped to one collection, optionally substring-filtered on title/slug) with a one-line summary each.",
                "inputSchema": {
                    "type": "object",
                    "properties": with_root(json!({
                        "collection": { "type": "string" },
                        "filter": { "type": "string", "description": "Case-insensitive substring on title/slug." }
                    })),
                    "additionalProperties": false
                }
            },
            {
                "name": "kb_ensure_structure",
                "description": "Idempotently create collection directories and (re)generate every index.md README (per-collection inventory + root topic map). READMEs are deterministic generated artifacts.",
                "inputSchema": {
                    "type": "object",
                    "properties": with_root(json!({})),
                    "additionalProperties": false
                }
            },
            {
                "name": "kb_validate",
                "description": "Full-tree lint: frontmatter parse errors, missing required `type`, dangling wiki-links, slug/filename mismatches, and duplicate aliases. Returns structured findings.",
                "inputSchema": {
                    "type": "object",
                    "properties": with_root(json!({})),
                    "additionalProperties": false
                }
            },
            {
                "name": "kb_convert_tree",
                "description": "Convert an arbitrary markdown tree into the collection model: classify files, assign type (fail-open Note), record provenance, and regenerate indexes. Never destroys content. report_only (default) returns the plan without writing; in_place applies it.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "source_root": { "type": "string", "description": "Root of the tree to convert (confined under the knowledge directory)." },
                        "mode": { "type": "string", "enum": ["report_only", "in_place"], "description": "Default report_only." }
                    },
                    "required": ["source_root"],
                    "additionalProperties": false
                }
            }
        ]
    })
}
