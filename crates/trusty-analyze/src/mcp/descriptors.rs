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
            "name": "complexity_distribution",
            "description": "Full A-F cyclomatic-complexity histogram over the whole index corpus, with the counted total. Unlike complexity_hotspots (a descending top-N), this is exhaustive, so its counts are the only honest denominator for a percentage.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "index": { "type": "string" },
                    "index_id": { "type": "string" }
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
            "description": "Group chunks into concept clusters using k-means over hashed bag-of-words embeddings",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "index":    { "type": "string" },
                    "index_id": { "type": "string" },
                    "k":        { "type": "number" },
                    // #5067: 'neural' was removed along with the fastembed embedder.
                    "method":   { "type": "string", "enum": ["bow"], "description": "Embedding method. Only 'bow' is supported; it is the default." }
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
            "name": "scip_status",
            "description": "Report whether a SCIP overlay has been ingested for an index. Distinguishes 'never ingested' (this tool errors) from 'ingested but contributed zero symbols' (this tool returns node/edge counts of 0), which extract_graph's own scip_overlay flag cannot do on its own. Returns node count, edge count, and the ingest timestamp for the overlay.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "index":    { "type": "string" },
                    "index_id": { "type": "string" }
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

/// Return the descriptors for the three `tr_review_*` tools (#630).
///
/// Why: these ship only under `feature = "review"`, but their *metadata* must
/// be readable in every build. The generated MCP-tool section in `README.md` /
/// `CLAUDE.md` has to be true under both configurations, and CI never builds
/// this crate with `--features review` — so a descriptor list compiled only
/// under that feature would leave the documented review rows unverified. The
/// dispatch handler stays gated in `mcp::review`; only the data moved here.
/// What: returns a `Vec<Value>`, one descriptor object per tool, mirroring the
/// trusty-review tool schemas but with the `tr_` name prefix.
/// Test: `review_descriptors_are_tr_prefixed_and_complete` below (always runs);
/// `mod.rs::tools_list_includes_tr_review_tools` asserts they reach
/// `tools/list` under the feature.
pub fn review_tool_descriptors() -> Vec<Value> {
    vec![
        serde_json::json!({
            "name": "tr_review_pr",
            "description": "LLM-backed review of a GitHub pull request via the embedded trusty-review pipeline. Fetches the PR diff, retrieves code context from trusty-search, augments with this analyzer daemon's static-analysis context (loopback), and returns a structured verdict (APPROVE / APPROVE* / REQUEST_CHANGES / BLOCK / UNKNOWN) with actionable findings. Requires GITHUB_TOKEN and AWS Bedrock credentials (or OPENROUTER_API_KEY). Always dry-run — never posts a GitHub comment.",
            "inputSchema": {
                "type": "object",
                "required": ["owner", "repo", "pr"],
                "properties": {
                    "owner": { "type": "string", "description": "GitHub organisation or user that owns the repository" },
                    "repo":  { "type": "string", "description": "GitHub repository name" },
                    "pr":    { "type": "integer", "description": "Pull request number" },
                    "reviewer_model": { "type": "string", "description": "Override the reviewer model slug (e.g. 'bedrock/us.anthropic.claude-sonnet-4-6')" }
                }
            }
        }),
        serde_json::json!({
            "name": "tr_review_diff",
            "description": "LLM-backed review of a raw unified diff string via the embedded trusty-review pipeline. No GitHub credentials required. Useful for reviewing local changes, staged diffs, or patches. Requires AWS Bedrock credentials (or OPENROUTER_API_KEY). trusty-search + this analyzer daemon supply code and static-analysis context.",
            "inputSchema": {
                "type": "object",
                "required": ["diff"],
                "properties": {
                    "diff":    { "type": "string", "description": "Unified diff string (output of `git diff` or similar)" },
                    "context": { "type": "string", "description": "Optional human-readable context (PR title/description, ticket, intent)" },
                    "reviewer_model": { "type": "string", "description": "Override the reviewer model slug (same format as tr_review_pr)" }
                }
            }
        }),
        serde_json::json!({
            "name": "tr_review_health",
            "description": "Probe the embedded trusty-review pipeline's liveness and configuration (dry_run mode, reviewer model, dependency URLs). Safe to call without any credentials.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        // #6669: the technical-DD report verb. Unlike the three above it does
        // NOT forward to trusty-review's own MCP surface — that surface has no
        // report tool — so `mcp::review::handle_tr_report` calls the library
        // entry point directly. The descriptor lives here with its siblings so
        // the generated tool table is true with and without the feature.
        serde_json::json!({
            "name": "tr_report",
            "description": "Generate a technical due-diligence report from a report manifest, via the embedded trusty-review report pipeline. `template` selects the methodology template ('cast' for the CAST health-factor report, 'default' for the vendor-neutral one, or a full bundled/override name). `code_only` renders the sections no repository can answer — peer benchmark, organizational next steps — as stated out-of-scope boundaries instead of dropping them, and marks the code-derived-but-uncorroborated sections as inferred. Writes a markdown + JSON pair and returns their paths. Long-running (minutes) and always calls inference: requires OPENROUTER_API_KEY on the daemon.",
            "inputSchema": {
                "type": "object",
                "required": ["manifest_path"],
                "properties": {
                    "manifest_path": { "type": "string", "description": "Path to the report manifest TOML file" },
                    "template":      { "type": "string", "description": "Template name or alias: 'cast', 'default', or a full bundled/override name" },
                    "code_only":     { "type": "boolean", "description": "Render non-code sections as stated out-of-scope boundaries (default false)", "default": false },
                    "out":           { "type": "string", "description": "Output directory for the report pair (default './reports')" },
                    "instructions":  { "type": "string", "description": "Path to a free-form analyst instructions markdown file" },
                    "analyze":       { "type": "boolean", "description": "Fetch deterministic metrics from this analyzer daemon (default true; fail-open)", "default": true }
                }
            }
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_descriptors_are_tr_prefixed_and_complete() {
        let descs = review_tool_descriptors();
        let names: Vec<&str> = descs
            .iter()
            .filter_map(|d| d.get("name").and_then(Value::as_str))
            .collect();
        assert_eq!(names.len(), 4, "expected 4 tr_ tools, got {names:?}");
        for required in [
            "tr_review_pr",
            "tr_review_diff",
            "tr_review_health",
            // #6669.
            "tr_report",
        ] {
            assert!(names.contains(&required), "missing {required} in {names:?}");
        }
        // Every tr_ tool carries an inputSchema.
        for d in &descs {
            assert!(d.get("inputSchema").is_some(), "missing inputSchema: {d}");
        }
    }

    /// Why (#6669): `tr_report` is the only `tr_` tool that does not forward to
    /// trusty-review's own MCP surface, and the one required input is what a
    /// caller most easily omits. A schema that did not require it would let the
    /// dispatcher take a call it cannot serve.
    /// What: `manifest_path` is required and `code_only` is a boolean.
    /// Test: this test itself.
    #[test]
    fn tr_report_requires_a_manifest_path() {
        let descs = review_tool_descriptors();
        let report = descs
            .iter()
            .find(|d| d.get("name").and_then(Value::as_str) == Some("tr_report"))
            .expect("tr_report descriptor");
        let schema = &report["inputSchema"];
        assert_eq!(schema["required"][0], "manifest_path");
        assert_eq!(schema["properties"]["code_only"]["type"], "boolean");
        assert_eq!(schema["properties"]["template"]["type"], "string");
    }
}
