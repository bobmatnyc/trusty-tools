//! Static tool descriptors returned by `tools/list`.
//!
//! Why: the JSON schemas for all 18 MCP tools are long but mechanical; keeping
//! them in a dedicated file makes it easy to update a single tool's description
//! or input schema without touching any dispatch logic.
//! What: exports `tool_descriptors()` which returns the full `serde_json::Value`
//! array consumed by the `tools/list` handler in `dispatch`.
//! Test: `tools_list_returns_all_tools`, `test_tools_list_response`,
//! `tools_list_returns_five_search_tools`, and
//! `per_lane_tool_descriptions_carry_when_to_use_hooks` in `tests.rs`.

use serde_json::Value;

/// Static metadata for `tools/list`. Keep in sync with `call_tool` in `mod.rs`.
///
/// Why: listing tools separately from their implementation allows the MCP
/// client to introspect the full tool catalogue without triggering any daemon
/// HTTP calls.
/// What: returns a `Value::Array` containing one descriptor object per
/// registered tool. Each object has `name`, `description`, and `inputSchema`.
/// Test: `test_tools_list_response` asserts every required tool is present and
/// carries an `inputSchema`.
pub fn tool_descriptors() -> Value {
    serde_json::json!([
        // Issue #138 — per-lane MCP tools. Tool descriptions are
        // first-class LLM prompts: each one opens with "when to use",
        // gives concrete fit/don't-fit examples, states the cost, and
        // explains the failure mode (STAGE_NOT_READY). The legacy
        // `search` tool is preserved below as a back-compat alias.
        {
            "name": "search_lexical",
            "description": "Find code by exact symbol name, regex, or literal string. Equivalent to a fast ripgrep on the indexed codebase. Use this FIRST for any query where the user mentions a specific identifier (function name, struct name, file name) or a literal phrase. Best for: `apply_archive_downrank`, `pub fn main`, `\"TODO: refactor\"`, filename globs like `*.toml`. Don't use for: conceptual queries like \"how does authentication work\" — use `search_semantic` instead. Always available on any indexed project. Cheapest tool in this family.",
            "inputSchema": {
                "type": "object",
                "required": ["index_id", "query"],
                "properties": {
                    "index_id":         { "type": "string", "description": "Target index id (from `list_indexes`)" },
                    "query":            { "type": "string", "description": "Exact symbol, regex, or literal phrase" },
                    "top_k":            { "type": "integer", "default": 10 },
                    "mode":             { "type": "string", "enum": ["code", "text", "data"], "default": "code" },
                    "exclude_archived": { "type": "boolean", "default": false },
                    "branch_files":     { "type": "array", "items": { "type": "string" } },
                    "branch_boost":     { "type": "number" },
                    "branch":           { "type": "string" }
                },
                "examples": [
                    { "index_id": "trusty-tools", "query": "apply_archive_downrank" },
                    { "index_id": "trusty-tools", "query": "pub fn main" },
                    { "index_id": "trusty-tools", "query": "TODO: refactor" }
                ]
            }
        },
        {
            "name": "search_semantic",
            "description": "Find code by meaning, not by literal text. Uses embedding-based similarity to retrieve chunks that semantically match the query, even when the query words don't appear in the code. Best for: \"code that handles JWT verification\", \"the place that does community detection\", \"how does the embedder batch requests\". Don't use for: exact symbol lookups (use `search_lexical`) or finding callers of a known function (use `search_kg`). Requires Stage 2 (embeddings) to be ready on the index — returns a STAGE_NOT_READY error with a `suggested_tools` retry hint if not. Medium cost.",
            "inputSchema": {
                "type": "object",
                "required": ["index_id", "query"],
                "properties": {
                    "index_id":         { "type": "string" },
                    "query":            { "type": "string", "description": "Conceptual query — meaning, not literal text" },
                    "top_k":            { "type": "integer", "default": 10 },
                    "mode":             { "type": "string", "enum": ["code", "text", "data"], "default": "code" },
                    "exclude_archived": { "type": "boolean", "default": false }
                },
                "examples": [
                    { "index_id": "trusty-tools", "query": "code that handles JWT verification" },
                    { "index_id": "trusty-tools", "query": "the place that does community detection" }
                ]
            }
        },
        {
            "name": "search_kg",
            "description": "Explore code structure from a known seed — either a chunk_id (from a previous search result) or a symbol name. Returns chunks connected to the seed via `calls`, `called_by`, `contains`, `inherits` edges. Best for: \"what calls `validate_token`\", \"what does `Authenticator` use internally\", impact analysis before a refactor. Don't use for: free-text discovery (use `search_semantic`) or initial entry-point finding (use `search_lexical` first). Requires Stage 3 (symbol graph) to be ready. Returns empty if the seed is not in the index. Cheap once you have a seed. Optional `refine_query`: provide a longer natural-language description to rerank and filter the expanded neighbourhood by semantic relevance — useful when the seed chunk is correct but you want only the most relevant callers/callees (issue #147).",
            "inputSchema": {
                "type": "object",
                "required": ["index_id", "query"],
                "properties": {
                    "index_id":      { "type": "string" },
                    "query":         { "type": "string", "description": "Seed: a symbol name or chunk_id from a previous result" },
                    "top_k":         { "type": "integer", "default": 10 },
                    "mode":          { "type": "string", "enum": ["code", "text", "data"], "default": "code" },
                    "refine_query":  { "type": "string", "description": "Optional: rerank and filter expanded KG neighbours by cosine similarity to this natural-language description. Neighbours below the 0.4 cosine threshold are dropped. Omit to use default KG expansion without filtering." }
                },
                "examples": [
                    { "index_id": "trusty-tools", "query": "validate_token" },
                    { "index_id": "trusty-tools", "query": "Authenticator", "refine_query": "callers that handle token refresh in the auth middleware" }
                ]
            }
        },
        {
            "name": "search_all",
            "description": "When in doubt, use this. Runs the full hybrid pipeline (lexical + semantic + KG expansion) and merges results via RRF. More expensive than the targeted tools but catches edge cases. Use when: your query has both literal symbols AND conceptual phrasing (\"find the `AuthValidator` that handles refresh tokens\"), or when you've tried the targeted tools and they didn't surface what you need. Always available; gracefully degrades to whatever lanes are ready. When called without `index_id`, falls back to legacy cross-project fan-out behaviour (issue #10) — provide `index_id` for the per-index hybrid path.",
            "inputSchema": {
                "type": "object",
                "required": ["query"],
                "properties": {
                    "index_id":         { "type": "string", "description": "Target index (omit for cross-project fan-out)" },
                    "query":            { "type": "string" },
                    "top_k":            { "type": "integer", "default": 10 },
                    "mode":             { "type": "string", "enum": ["code", "text", "data"], "default": "code" },
                    "exclude_archived": { "type": "boolean", "default": false },
                    "full_content":     { "type": "boolean", "default": false, "description": "Legacy fan-out only: include full chunk content in each hit" },
                    "branch_files":     { "type": "array", "items": { "type": "string" } },
                    "branch_boost":     { "type": "number" },
                    "branch":           { "type": "string" }
                },
                "examples": [
                    { "index_id": "trusty-tools", "query": "AuthValidator that handles refresh tokens" },
                    { "query": "global cross-project fan-out without index_id" }
                ]
            }
        },
        {
            "name": "search",
            "description": "Unified hybrid search (BM25+vector+KG+RRF) with mode-aware ranking (issue #77). The `mode` parameter (\"code\" | \"text\" | \"data\", default \"code\") picks the file-type penalty matrix: code prefers source (prose 0.1x, data 0.2x); text prefers prose docs (source 0.5x, data 0.3x); data prefers structured data (source 0.3x, prose 0.3x). Set `exclude_archived: true` to drop archived/deprecated/legacy chunks entirely instead of downranking them (issue #74). Supports branch-aware scoring via branch_files/branch_boost/branch (issue #122). Replaces the legacy `search_code` tool name; callers that omit `mode` get identical pre-#77 behaviour.",
            "inputSchema": {
                "type": "object",
                "required": ["index_id", "query"],
                "properties": {
                    "index_id": { "type": "string" },
                    "query": { "type": "string" },
                    "top_k": { "type": "integer", "default": 10 },
                    "mode": {
                        "type": "string",
                        "enum": ["code", "text", "data"],
                        "default": "code",
                        "description": "Ranking mode: prefer source code, prose docs, or structured data."
                    },
                    "exclude_archived": {
                        "type": "boolean",
                        "default": false,
                        "description": "Drop archived/deprecated/legacy chunks (paths like _archive/, archive/, _deprecated/, old/, .archive/; #[deprecated]; .archived/DEPRECATED markers) instead of downranking them."
                    },
                    "branch_files": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Files modified on current git branch (relative to index root). Boosted in results."
                    },
                    "branch_boost": {
                        "type": "number",
                        "description": "Score multiplier for branch files (default 1.5, range 1.0-3.0)."
                    },
                    "branch": {
                        "type": "string",
                        "description": "Branch name; daemon will compute branch_files via git if branch_files is absent."
                    }
                }
            }
        },
        {
            "name": "index_file",
            "description": "Add or update one file in an index",
            "inputSchema": {
                "type": "object",
                "required": ["index_id", "path", "content"],
                "properties": {
                    "index_id": { "type": "string" },
                    "path":     { "type": "string" },
                    "content":  { "type": "string" }
                }
            }
        },
        {
            "name": "remove_file",
            "description": "Remove a file's chunks from an index",
            "inputSchema": {
                "type": "object",
                "required": ["index_id", "path"],
                "properties": {
                    "index_id": { "type": "string" },
                    "path":     { "type": "string" }
                }
            }
        },
        {
            "name": "list_indexes",
            "description": "List all registered indexes on this daemon",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "create_index",
            "description": "Register a new (empty) index",
            "inputSchema": {
                "type": "object",
                "required": ["id", "root_path"],
                "properties": {
                    "id":        { "type": "string" },
                    "root_path": { "type": "string" }
                }
            }
        },
        {
            "name": "search_similar",
            "description": "Find chunks semantically similar to a given file/function via HNSW (issue #31)",
            "inputSchema": {
                "type": "object",
                "required": ["file"],
                "properties": {
                    "file":     { "type": "string" },
                    "function": { "type": "string" },
                    "top_k":    { "type": "number" },
                    "index":    { "type": "string" }
                }
            }
        },
        {
            "name": "search_health",
            "description": "Probe daemon liveness and version",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "delete_index",
            "description": "Delete a registered index and all its data",
            "inputSchema": {
                "type": "object",
                "required": ["index_id"],
                "properties": {
                    "index_id": { "type": "string" }
                }
            }
        },
        {
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
        },
        {
            "name": "index_status",
            "description": "Get stats for an index (chunk count, root path)",
            "inputSchema": {
                "type": "object",
                "required": ["index_id"],
                "properties": {
                    "index_id": { "type": "string" }
                }
            }
        },
        {
            "name": "list_chunks",
            "description": "Paginated enumeration of every chunk in an index (issue #54). Stable order by (file, start_line).",
            "inputSchema": {
                "type": "object",
                "required": ["index_id"],
                "properties": {
                    "index_id": { "type": "string" },
                    "offset":   { "type": "integer", "default": 0 },
                    "limit":    { "type": "integer", "default": 100 }
                }
            }
        },
        {
            "name": "get_call_chain",
            "description": "Annotated call tree for a function entry point (issue #76). \
                            Returns plain-text prose with the entry function's signature, \
                            Why/What doc lines, its depth-1 callees with full source, and \
                            its depth-1 callers as signatures only. LLMs read this prose \
                            tree more reliably than JSON. Entry point accepts an exact \
                            symbol name, a case-insensitive fuzzy substring, or a \
                            `file:line` reference; the most-connected match wins ties.",
            "inputSchema": {
                "type": "object",
                "required": ["index_id", "entry_point"],
                "properties": {
                    "index_id":       { "type": "string" },
                    "entry_point":    { "type": "string", "description": "Function name, fuzzy substring, or file:line" },
                    "direction":      { "type": "string", "enum": ["both", "outgoing", "callers"], "default": "both" },
                    "max_depth":      { "type": "integer", "minimum": 1, "maximum": 4, "default": 2 },
                    "include_source": { "type": "boolean", "default": true, "description": "Embed full source at depth <= 1" }
                }
            }
        },
        {
            "name": "grep",
            "description": "Search indexed files using regex/literal patterns with ripgrep-compatible options. \
                            Greps the on-disk bytes of files the index already knows about, so no \
                            re-embedding occurs and line numbers are exact. Supports regex or fixed-string \
                            matching, case folding (-i), context windows (-A/-B/-C), include globs, \
                            multiline mode, files-with-matches (-l), invert (-v), and word-regexp (-w). \
                            When `index_id` is omitted the daemon fans out across every registered index.",
            "inputSchema": {
                "type": "object",
                "required": ["pattern"],
                "properties": {
                    "pattern":            { "type": "string", "description": "Regex (default) or literal when fixed_strings=true" },
                    "index_id":           { "type": "string", "description": "Optional index id; omit to fan out across all indexes" },
                    "case_insensitive":   { "type": "boolean", "default": false, "description": "-i / --ignore-case" },
                    "context":            { "type": "integer", "description": "-C: equal before/after context, overrides context_before/context_after" },
                    "context_before":     { "type": "integer", "description": "-B: lines of context before each match" },
                    "context_after":      { "type": "integer", "description": "-A: lines of context after each match" },
                    "glob":                { "type": "string", "description": "--include glob (e.g. '**/*.rs')" },
                    "multiline":          { "type": "boolean", "default": false, "description": "Let `.` span newlines" },
                    "fixed_strings":      { "type": "boolean", "default": false, "description": "-F: treat pattern as literal" },
                    "files_with_matches": { "type": "boolean", "default": false, "description": "-l: return one path per matching file" },
                    "invert_match":       { "type": "boolean", "default": false, "description": "-v: return lines that do NOT match" },
                    "word_regexp":        { "type": "boolean", "default": false, "description": "-w: require word boundaries" },
                    "max_results":        { "type": "integer", "default": 100, "description": "Hard cap on returned matches (alias: max_count)" },
                    "max_count":          { "type": "integer", "description": "Alias for max_results (ripgrep --max-count parity)" }
                }
            }
        },
        {
            "name": "chat",
            "description": "Ask a natural-language question about the indexed codebase. \
                            Automatically searches for the top_k most relevant chunks and \
                            sends them as context to an OpenRouter LLM (default model: \
                            anthropic/claude-haiku-4.5). Returns {answer, sources, model}. \
                            Requires OPENROUTER_API_KEY env var on the daemon, or an \
                            `api_key` field in the request.",
            "inputSchema": {
                "type": "object",
                "required": ["index_id"],
                "properties": {
                    "index_id": { "type": "string" },
                    "message":  { "type": "string", "description": "User question (alias: question)" },
                    "question": { "type": "string", "description": "User question (alias: message)" },
                    "history":  { "type": "array", "items": { "type": "object" } },
                    "model":    { "type": "string", "description": "OpenRouter model id (default: anthropic/claude-haiku-4.5)" },
                    "top_k":    { "type": "integer", "description": "Number of context chunks (default: 5)", "default": 5 },
                    "api_key":  { "type": "string", "description": "Fallback OpenRouter API key when OPENROUTER_API_KEY env is unset" }
                }
            }
        },
        {
            "name": "upgrade",
            "description": "Check for or install a new version of trusty-search (issue #537). \
                            With check=true (or without confirm): report current vs. available version — NEVER installs. \
                            With confirm=true: install via `cargo install trusty-search --locked`, run a binary \
                            health gate, then restart the daemon under launchd (or print a restart hint when \
                            not supervised). The MCP response is returned BEFORE the daemon exits.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "check":   { "type": "boolean", "description": "Report versions only, no install (default when confirm absent)", "default": true },
                    "confirm": { "type": "boolean", "description": "Set to true to install the new version. Must be explicit — never assumed.", "default": false }
                },
                "required": []
            }
        },
        {
            "name": "console_metrics",
            "description": "Return a ConsoleMetricsReport with daemon health and index aggregate \
                            statistics (index_count, warm_boot_degraded, index list with id/root_path/size_bytes). \
                            Used by the trusty-console dashboard metrics poller (epic #1104).",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        }
    ])
}
