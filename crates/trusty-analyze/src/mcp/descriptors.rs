//! Base MCP tool descriptors for the trusty-analyze dispatcher.
//!
//! Why: the `tools/list` JSON-Schema payload is large (~200 lines) and was
//! inflating `mcp/mod.rs` past the #610 shrink-only line-cap budget. Extracting
//! it into its own module both keeps `mod.rs` under its frozen budget and gives
//! the descriptor list a single, easy-to-locate home. The optional `review`
//! feature (#630) appends its three `tr_review_*` descriptors on top of these.
//!
//! What: `base_tool_descriptors()` returns the descriptor array for every tool
//! the dispatcher serves unconditionally (i.e. excluding feature-gated ones).
//!
//! Test: `crates/trusty-analyze/src/mcp/mod.rs` `tools_list_contains_full_surface`
//! asserts the names this list produces are all present in the `tools/list`
//! response.

use serde_json::Value;

/// Return the descriptor array for the always-compiled analyzer tools.
///
/// Why: callers (`mod.rs::tool_descriptors`) assemble the final `tools/list`
/// payload from this base set plus any feature-gated additions; keeping the
/// base set here is what lets `mod.rs` stay within its line-cap budget.
/// What: returns a `serde_json::Value` array, one object per tool, each with a
/// `name`, `description`, and JSON-Schema `inputSchema`.
/// Test: name coverage asserted by `mod.rs::tools_list_contains_full_surface`.
pub fn base_tool_descriptors() -> Value {
    serde_json::json!([
        {
            "name": "complexity_hotspots",
            "description": "Top-N chunks ranked by cyclomatic complexity",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "index": { "type": "string" },
                    "index_id": { "type": "string" },
                    "top_n": { "type": "number" }
                }
            }
        },
        {
            "name": "find_smells",
            "description": "Chunks with at least one detected code smell. Results are paginated (default limit 500) and content is omitted by default to keep responses bounded. Use limit/offset to page through large result sets; set omit_content=false to include raw source text.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "index": { "type": "string" },
                    "index_id": { "type": "string" },
                    "limit": { "type": "number", "description": "Max results per page (default 500)" },
                    "offset": { "type": "number", "description": "Zero-based page offset (default 0)" },
                    "omit_content": { "type": "boolean", "description": "Strip raw source text from results (default true). Set false to include full content." }
                }
            }
        },
        {
            "name": "analyze_quality",
            "description": "Aggregate quality stats: avg cyclomatic, %A, smell count",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "index": { "type": "string" },
                    "index_id": { "type": "string" }
                }
            }
        },
        {
            "name": "run_diagnostics",
            "description": "Run available external static-analysis tools (clippy, ruff, biome, staticcheck, pmd, rubocop, phpstan, swiftlint, detekt, clang-tidy, roslyn) across the index corpus on demand. Tools are auto-discovered: only installed binaries run. Returns normalized diagnostics with file, line, severity, rule code, and message. Results are paginated (default limit 500) to keep MCP responses bounded.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "index":    { "type": "string" },
                    "index_id": { "type": "string" },
                    "language": { "type": "string", "description": "Optional: restrict to one language tag (rust, python, typescript, go, java, ruby, php, swift, kotlin, cpp, csharp)" },
                    "tools":    { "type": "string", "description": "Optional: comma-separated list of tool names to run; defaults to all available" },
                    "limit":    { "type": "number", "description": "Max results per page (default 500)" },
                    "offset":   { "type": "number", "description": "Zero-based page offset (default 0)" }
                }
            }
        },
        {
            "name": "list_facts",
            "description": "List canonical facts, optionally filtered by subject/predicate/object",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "subject":   { "type": "string" },
                    "predicate": { "type": "string" },
                    "object":    { "type": "string" }
                }
            }
        },
        {
            "name": "upsert_fact",
            "description": "Insert or update a canonical fact triple",
            "inputSchema": {
                "type": "object",
                "required": ["subject", "predicate", "object", "index_id"],
                "properties": {
                    "subject":    { "type": "string" },
                    "predicate":  { "type": "string" },
                    "object":     { "type": "string" },
                    "index_id":   { "type": "string" },
                    "confidence": { "type": "number" },
                    "provenance": { "type": "array", "items": { "type": "string" } }
                }
            }
        },
        {
            "name": "delete_fact",
            "description": "Delete a fact by its u64 id",
            "inputSchema": {
                "type": "object",
                "required": ["id"],
                "properties": { "id": { "type": "number" } }
            }
        },
        {
            "name": "analyzer_health",
            "description": "Probe analyzer daemon liveness and version",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "list_analyze_indexes",
            "description": "List all indexes known to the trusty-analyze daemon. Used by the trusty-console dashboard so the browser calls the console's /api route instead of the daemon HTTP directly.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "extract_graph",
            "description": "Build the multi-language knowledge graph (nodes + edges) for an index",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "index":    { "type": "string" },
                    "index_id": { "type": "string" },
                    "language": { "type": "string" }
                }
            }
        },
        {
            "name": "cluster_concepts",
            "description": "Group chunks into concept clusters using k-means over embeddings (BOW or neural)",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "index":    { "type": "string" },
                    "index_id": { "type": "string" },
                    "k":        { "type": "number" },
                    "method":   { "type": "string", "description": "Embedding method: 'bow' (default, fast) or 'neural' (semantic, requires fastembed model)" }
                }
            }
        },
        {
            "name": "ingest_scip",
            "description": "Ingest a SCIP (Scalable and Precise Index for Code) protobuf index for a given index_id, enriching the knowledge graph with fully-resolved symbols and cross-file relationships. The SCIP bytes must be base64-encoded.",
            "inputSchema": {
                "type": "object",
                "required": ["scip_base64"],
                "properties": {
                    "index":        { "type": "string" },
                    "index_id":     { "type": "string" },
                    "scip_base64":  { "type": "string", "description": "Base64-encoded SCIP Index protobuf payload" }
                }
            }
        },
        {
            "name": "extract_ner",
            "description": "Extract named entities from doc comments for a code index using NER",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "index":    { "type": "string" },
                    "index_id": { "type": "string", "description": "Index ID" },
                    "top_k":    { "type": "integer", "description": "Max entities to return", "default": 50 }
                }
            }
        },
        {
            "name": "suggest_refactors",
            "description": "Suggest concrete refactoring actions (extract method, reduce nesting, ...) ranked by severity, derived from complexity metrics and code smells",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "index":        { "type": "string" },
                    "index_id":     { "type": "string" },
                    "file":         { "type": "string", "description": "Optional path filter — restrict suggestions to one file" },
                    "min_severity": { "type": "string", "description": "Minimum severity: 'low' (default), 'medium', 'high', 'critical'" },
                    "top_k":        { "type": "number", "description": "Cap on suggestions returned (default 20)" }
                }
            }
        },
        {
            "name": "review_diff",
            "description": "Review a unified git diff and return a structured quality report (per-file complexity, code smells, grade A-F, recommendations). Cross-references the diff against the trusty-search index corpus, so trusty-search must be running. Deterministic and LLM-free — use the deep_analysis tool for LLM-augmented narrative.",
            "inputSchema": {
                "type": "object",
                "required": ["diff", "index_id"],
                "properties": {
                    "diff":     { "type": "string", "description": "Unified git diff text to review" },
                    "index_id": { "type": "string", "description": "Index ID to cross-reference the diff against in trusty-search" }
                }
            }
        },
        {
            "name": "deep_analysis",
            "description": "Run an LLM-augmented deep analysis pass over an index: synthesises a deterministic review report from the indexed corpus, looks up detected frameworks, and asks an OpenRouter model for a prose narrative plus framework-aware recommendations. Requires OPENROUTER_API_KEY configured on the daemon.",
            "inputSchema": {
                "type": "object",
                "required": ["index_id"],
                "properties": {
                    "index_id": { "type": "string", "description": "trusty-search index ID to analyse" },
                    "model":    { "type": "string", "description": "Optional OpenRouter model id (e.g. 'openai/gpt-4o-mini'); falls back to TRUSTY_LLM_MODEL on the daemon" }
                }
            }
        },
        {
            "name": "review_github_pr",
            "description": "Fetch a GitHub pull request's unified diff and run a structured quality review against a trusty-search index. Requires GITHUB_TOKEN set on the daemon. Optionally posts the review back as a PR comment.",
            "inputSchema": {
                "type": "object",
                "required": ["owner", "repo", "pr", "index_id"],
                "properties": {
                    "owner":        { "type": "string", "description": "Repository owner (user or org)" },
                    "repo":         { "type": "string", "description": "Repository name" },
                    "pr":           { "type": "integer", "description": "Pull request number" },
                    "index_id":     { "type": "string", "description": "trusty-search index ID to cross-reference" },
                    "post_comment": { "type": "boolean", "description": "Post the review back as a PR comment (default false)", "default": false }
                }
            }
        },
        {
            "name": "list_entities",
            "description": "List symbol-level entities (functions, classes, ...) for an index",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "index":    { "type": "string" },
                    "index_id": { "type": "string" },
                    "kind":     { "type": "string" },
                    "language": { "type": "string" }
                }
            }
        }
    ])
}
