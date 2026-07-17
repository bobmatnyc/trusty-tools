//! Tests for the `search_code` trusty-search discovery tool.
//!
//! Why: pins the pure index-resolution/parsing helpers and the fail-open
//! contract (a missing daemon must yield a SUCCESSFUL "use grep/glob" result,
//! never an error) so a regression in either can't silently derail the agent
//! loop.
//! What: covers `pick_index`, `mcp_text`, `is_mcp_error`, the tool schema, and
//! `execute`'s daemon-absent / malformed-args paths. The live search lanes are
//! not exercised here (they require a running trusty-search daemon + index);
//! their routing is covered by `build_call`'s construction being unit-visible.
//! Test: this module.

use serde_json::json;

use super::*;

/// An index detail entry as returned by `list_indexes?details=true`.
fn index_entry(id: &str, root_path: &str) -> Value {
    json!({ "id": id, "root_path": root_path, "size_bytes": 0 })
}

// ── pick_index ──────────────────────────────────────────────────────────────

/// Why: the common case — the project root is exactly a registered index root.
#[test]
fn pick_index_prefers_exact_match() {
    let tmp = std::env::temp_dir();
    let root = tmp.to_str().expect("utf8 temp dir");
    let indexes = vec![
        index_entry("other", "/nonexistent/elsewhere"),
        index_entry("mine", root),
    ];
    assert_eq!(pick_index(&indexes, &tmp).as_deref(), Some("mine"));
}

/// Why: a subdirectory/worktree of a registered repo must resolve to that
/// repo's index (longest ancestor wins over a shallower one).
#[test]
fn pick_index_falls_back_to_longest_ancestor() {
    let base = std::env::temp_dir();
    let nested = base.join("tcode-pick-index-nested");
    std::fs::create_dir_all(&nested).expect("mkdir nested");

    let shallow = base.to_str().expect("utf8");
    let deep = nested.to_str().expect("utf8");
    // Register both an ancestor (temp dir) and the exact nested dir; the exact
    // match must win. Then drop the exact entry and the ancestor must be used.
    let with_exact = vec![index_entry("ancestor", shallow), index_entry("exact", deep)];
    assert_eq!(pick_index(&with_exact, &nested).as_deref(), Some("exact"));

    let ancestor_only = vec![index_entry("ancestor", shallow)];
    assert_eq!(
        pick_index(&ancestor_only, &nested).as_deref(),
        Some("ancestor")
    );

    let _ = std::fs::remove_dir_all(&nested);
}

/// Why: no registered index covers the project → `None`, so the search lanes
/// fail open rather than guess a wrong index.
#[test]
fn pick_index_returns_none_when_uncovered() {
    let indexes = vec![index_entry("elsewhere", "/nonexistent/elsewhere")];
    assert_eq!(
        pick_index(&indexes, Path::new("/nonexistent/project")),
        None
    );
}

// ── mcp_text / is_mcp_error ─────────────────────────────────────────────────

/// Why: every trusty-search payload arrives as `content[0].text`; the extractor
/// must return that inner string.
#[test]
fn mcp_text_extracts_content() {
    let result = json!({ "content": [{ "type": "text", "text": "{\"indexes\":[]}" }] });
    assert_eq!(mcp_text(&result).as_deref(), Some("{\"indexes\":[]}"));

    let empty = json!({ "isError": false });
    assert_eq!(mcp_text(&empty), None);
}

/// Why: `STAGE_NOT_READY` and friends arrive as in-band `isError: true`
/// content (not a JSON-RPC error), so the flag must be detected to fail open.
#[test]
fn is_mcp_error_detects_error_flag() {
    assert!(is_mcp_error(&json!({ "isError": true, "content": [] })));
    assert!(!is_mcp_error(&json!({ "isError": false })));
    assert!(!is_mcp_error(&json!({ "content": [] })));
}

// ── stage-not-ready → lexical fallback (issue #2783) ────────────────────────

/// Why: the recoverable warm-up window is signalled ONLY by the structured
/// `_meta.error_code` (issue #138) — the human-readable text never carries the
/// literal code — so detection must key off `_meta` and nothing else.
#[test]
fn is_stage_not_ready_detects_meta_code() {
    let meta = json!({
        "isError": true,
        "content": [{ "type": "text", "text": "requires Stage 2 (embeddings), not yet ready" }],
        "_meta": { "error_code": "STAGE_NOT_READY" }
    });
    assert!(is_stage_not_ready(&meta));

    // A different structured error code must NOT be treated as stage-not-ready.
    let other_code = json!({
        "isError": true,
        "_meta": { "error_code": "INDEX_NOT_FOUND" }
    });
    assert!(!is_stage_not_ready(&other_code));

    // No `_meta` at all → not a stage-not-ready condition (fail open normally).
    let no_meta = json!({
        "isError": true,
        "content": [{ "type": "text", "text": "no index covers this project" }]
    });
    assert!(!is_stage_not_ready(&no_meta));
}

/// Why: only the semantic/symbol lanes gate on a warm-up stage; a `grep` error
/// is already lexical and must not re-route (would loop on the same lane).
#[test]
fn should_lexical_fallback_only_for_stage_not_ready() {
    let not_ready = json!({ "isError": true, "_meta": { "error_code": "STAGE_NOT_READY" } });
    assert!(should_lexical_fallback("semantic", &not_ready));
    assert!(should_lexical_fallback("symbol", &not_ready));
    // grep is itself the lexical lane — never re-route it.
    assert!(!should_lexical_fallback("grep", &not_ready));

    // A non-stage error on the semantic lane is NOT a lexical-retry candidate.
    let other = json!({ "isError": true, "content": [{ "type": "text", "text": "boom" }] });
    assert!(!should_lexical_fallback("semantic", &other));
}

/// Why: the retry must hit trusty-search's `search_lexical` lane with the exact
/// argument shape it advertises (`index_id`, `query`, `top_k`).
#[test]
fn lexical_params_matches_search_lexical_schema() {
    let params = lexical_params("trusty-tools", "apply_archive_downrank", 8);
    assert_eq!(params["index_id"], "trusty-tools");
    assert_eq!(params["query"], "apply_archive_downrank");
    assert_eq!(params["top_k"], 8);
}

// ── build_call ──────────────────────────────────────────────────────────────

/// Why: each mode must route to trusty-search's real lane with the right
/// argument shape; a wrong tool name or missing arg would silently 400.
#[test]
fn build_call_routes_each_mode() {
    let (tool, params) = build_call("semantic", "how does auth work", 5, Some("idx")).expect("ok");
    assert_eq!(tool, "search_semantic");
    assert_eq!(params["index_id"], "idx");
    assert_eq!(params["query"], "how does auth work");
    assert_eq!(params["top_k"], 5);

    let (tool, params) = build_call("symbol", "validate_token", 5, Some("idx")).expect("ok");
    assert_eq!(tool, "search_kg");
    assert_eq!(params["query"], "validate_token");

    // grep works without an index (the daemon fans out across all indexes).
    let (tool, params) = build_call("grep", "TODO", 7, None).expect("ok");
    assert_eq!(tool, "grep");
    assert_eq!(params["pattern"], "TODO");
    assert_eq!(params["max_results"], 7);
    assert!(params.get("index_id").is_none());
}

/// Why: semantic/symbol need an index; without one the call is a fail-open
/// message steering the model back to the local tools.
#[test]
fn build_call_semantic_without_index_is_fallback() {
    let err = build_call("semantic", "q", 5, None).expect_err("needs index");
    assert!(err.contains("grep"), "must steer to local tools: {err}");
}

// ── schema ──────────────────────────────────────────────────────────────────

/// Why: the model must see `query` as required and `mode` as an enum defaulting
/// to semantic.
#[test]
fn schema_advertises_mode_and_query() {
    let tool = TrustySearchTool::new(std::env::temp_dir());
    let schema = tool.schema();
    assert_eq!(schema["function"]["name"], SEARCH_CODE_TOOL_NAME);
    let params = &schema["function"]["parameters"];
    let required: Vec<&str> = params["required"]
        .as_array()
        .expect("required array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(required, vec!["query"]);
    assert_eq!(params["properties"]["mode"]["default"], "semantic");
    assert_eq!(params["additionalProperties"], json!(false));
}

// ── execute (fail-open) ─────────────────────────────────────────────────────

/// Why: the daemon-absent path is the whole point of fail-open — a missing
/// binary must yield a SUCCESSFUL "use grep/glob" result, never an error.
#[tokio::test]
async fn execute_fail_open_when_binary_absent() {
    let tool = TrustySearchTool::new(std::env::temp_dir())
        .with_binary("trusty-search-definitely-not-installed-xyzzy");
    let result = tool.execute(json!({ "query": "how does auth work" })).await;
    assert!(!result.is_error(), "must not surface as a tool error");
    assert!(
        result.content().contains("grep"),
        "must steer to local tools: {}",
        result.content()
    );
}

/// Why: malformed args (missing `query`) must be a recoverable, non-fatal error
/// the model can correct — mirrors `recall_session`'s contract.
#[tokio::test]
async fn execute_rejects_malformed_args_recoverably() {
    let tool = TrustySearchTool::new(std::env::temp_dir())
        .with_binary("trusty-search-definitely-not-installed-xyzzy");
    let result = tool.execute(json!({ "mode": "semantic" })).await; // no query
    assert!(result.is_error());
    assert!(!result.is_fatal());
}

// ── UI Phase 1: lane resolution + hit counting + telemetry ──────────────────

/// Why: the requested `mode` must map to the SAME lane `build_call` routes to
/// — if these two ever disagree, the telemetry reports a lane that never ran.
#[test]
fn lane_from_mode_matches_build_call_routing() {
    // The MCP tool name `build_call` picks IS the lane, so asserting the pair
    // agree pins them together.
    for (mode, expect_lane, expect_tool) in [
        ("grep", SearchLane::Grep, "grep"),
        ("symbol", SearchLane::Symbol, "search_kg"),
        ("semantic", SearchLane::Semantic, "search_semantic"),
        // An unknown mode defaults to semantic in BOTH.
        ("nonsense", SearchLane::Semantic, "search_semantic"),
    ] {
        assert_eq!(SearchLane::from_mode(mode), expect_lane, "lane for {mode}");
        let (tool, _params) = build_call(mode, "q", 5, Some("idx")).expect("routes");
        assert_eq!(tool, expect_tool, "build_call tool for {mode}");
    }
}

/// Why: the lane labels are wire values on `Event::SearchPerformed.lane`; a UI
/// switches on them, so they must not drift.
#[test]
fn lane_labels_are_stable() {
    assert_eq!(SearchLane::Semantic.label(), "semantic");
    assert_eq!(SearchLane::Symbol.label(), "symbol");
    assert_eq!(SearchLane::Grep.label(), "grep");
    assert_eq!(SearchLane::Lexical.label(), "lexical");
}

/// Why: THE requirement — the event must carry the lane that ACTUALLY served
/// the query, not the one the model asked for. When a still-building index
/// forces the lexical retry (#2783), a `semantic` request is really answered
/// by the lexical lane, and reporting "semantic" would tell the UI a
/// comfortable lie about how the code was discovered.
#[test]
fn resolved_lane_reports_lexical_when_the_retry_served_it() {
    // Retry fired: whatever was asked, LEXICAL answered.
    assert_eq!(
        SearchLane::resolved("semantic", true),
        SearchLane::Lexical,
        "a semantic query served by the lexical retry is a LEXICAL search"
    );
    assert_eq!(SearchLane::resolved("symbol", true), SearchLane::Lexical);

    // No retry: the requested lane answered.
    assert_eq!(
        SearchLane::resolved("semantic", false),
        SearchLane::Semantic
    );
    assert_eq!(SearchLane::resolved("symbol", false), SearchLane::Symbol);
    assert_eq!(SearchLane::resolved("grep", false), SearchLane::Grep);
}

/// Why: the UI renders a hit count; each lane wraps its payload differently,
/// so the counter must read all the observed shapes. Each item's `path`
/// (the `CodeChunk` shape) must also be collected (DOC-39 Slice B).
#[test]
fn parse_search_hits_reads_each_lane_shape() {
    assert_eq!(
        parse_search_hits(
            r#"{"results":[{"path":"a.rs","score":0.9},{"path":"b.rs","score":0.4}]}"#
        ),
        (
            Some(2),
            vec![
                SearchHit {
                    path: "a.rs".into(),
                    score: 0.9
                },
                SearchHit {
                    path: "b.rs".into(),
                    score: 0.4
                },
            ]
        )
    );
    assert_eq!(
        parse_search_hits(r#"{"hits":[{"path":"c.rs","score":1.0}]}"#),
        (
            Some(1),
            vec![SearchHit {
                path: "c.rs".into(),
                score: 1.0
            }]
        )
    );
    assert_eq!(parse_search_hits(r#"{"matches":[]}"#), (Some(0), vec![]));
}

/// Why: an uncountable payload must report `None`, never `0` — "we could not
/// count" and "there were no hits" are different facts, and a UI showing a
/// confident `0 hits` for the former would be wrong. `hits` must stay empty
/// in lockstep.
#[test]
fn parse_search_hits_is_none_for_unrecognised_payloads() {
    assert_eq!(parse_search_hits("not json at all"), (None, vec![]));
    assert_eq!(
        parse_search_hits(r#"{"unexpected":"shape"}"#),
        (None, vec![])
    );
    assert_eq!(
        parse_search_hits(r#"{"results":"not-an-array"}"#),
        (None, vec![]),
        "a non-array under a known key is uncountable, not empty"
    );
}

/// Why: `grep`'s `GrepMatch` shape carries `file`, never `path` or `score` —
/// the path must fall back to `file` and the score must default to `0.0`
/// rather than dropping the hit entirely (DOC-39 Slice B).
#[test]
fn parse_search_hits_falls_back_to_file_and_defaults_missing_score() {
    let (count, hits) = parse_search_hits(
        r#"{"matches":[{"file":"src/main.rs","line":10},{"file":"src/lib.rs","line":2}]}"#,
    );
    assert_eq!(count, Some(2));
    assert_eq!(
        hits,
        vec![
            SearchHit {
                path: "src/main.rs".into(),
                score: 0.0
            },
            SearchHit {
                path: "src/lib.rs".into(),
                score: 0.0
            },
        ]
    );
}

/// Why: a hit with neither `path` nor `file` cannot be pointed at by the UI —
/// it must be skipped rather than fabricating an empty path (DOC-39 Slice B).
#[test]
fn parse_search_hits_skips_items_with_no_path_or_file() {
    let (count, hits) =
        parse_search_hits(r#"{"results":[{"score":0.5},{"path":"src/ok.rs","score":0.1}]}"#);
    // `count` mirrors the raw array length (matches pre-existing `hit_count`
    // semantics), while `hits` only carries the pointable ones.
    assert_eq!(count, Some(2));
    assert_eq!(
        hits,
        vec![SearchHit {
            path: "src/ok.rs".into(),
            score: 0.1
        }]
    );
}

/// Why: the fail-open paths never reached a lane, so there is no search to
/// report — a `SearchPerformed` event for them would invent a search that
/// never happened.
#[tokio::test]
async fn fail_open_paths_report_no_telemetry() {
    let tool = TrustySearchTool::new(std::env::temp_dir())
        .with_binary("trusty-search-definitely-not-installed-xyzzy");
    let result = tool.execute(json!({ "query": "how does auth work" })).await;
    assert!(
        result.telemetry().is_none(),
        "an absent daemon is not a search with zero hits"
    );
}

// ── execute: lane reached but reply carries no extractable text ────────────

/// Spawn a minimal fake MCP stdio server as a throwaway executable shell
/// script.
///
/// Why: exercising `execute()`'s "lane reached, but the reply had no
/// extractable text" arm end-to-end requires a process that speaks just
/// enough MCP to complete the handshake plus two `tools/call` round-trips
/// (`list_indexes`, then the lane call itself) — `TrustySearchTool` always
/// drives a real `StdioMcpClient` subprocess, so there is no lighter-weight
/// seam to fake this at.
/// What: writes an executable POSIX shell script that replies to the Nth
/// request line it sees (matched only by the presence of an `"id":` field,
/// so the handshake's `notifications/initialized` notification — which has
/// no id — is silently skipped) with the Nth canned response, verbatim.
/// Test: `execute_attaches_telemetry_when_lane_reply_has_no_text`.
#[cfg(unix)]
fn write_fake_mcp_server(responses: &[Value; 3]) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let script = format!(
        "#!/bin/sh\ni=0\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"id\":'*)\n      i=$((i + 1))\n      case \"$i\" in\n        1) printf '%s\\n' '{}' ;;\n        2) printf '%s\\n' '{}' ;;\n        3) printf '%s\\n' '{}' ;;\n      esac\n      ;;\n  esac\ndone\n",
        responses[0], responses[1], responses[2],
    );

    let path = std::env::temp_dir().join(format!(
        "tcode-fake-mcp-{}-{}.sh",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::write(&path, script).expect("write fake mcp script");
    let mut perms = std::fs::metadata(&path)
        .expect("stat fake mcp script")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod fake mcp script");
    path
}

/// Why: `execute`'s doc comment promises every path that actually reaches a
/// lane attaches a `SearchTelemetry`. The `mcp_text(&result) == None` arm — a
/// lane reply that is otherwise successful (no `isError`) but carries a
/// `content` item with no `text` field — previously attached none, silently
/// breaking that invariant even though a real search genuinely happened.
/// What: drives `execute()` against a fake MCP server that completes the
/// handshake, answers `list_indexes` with an empty list (so `grep` mode needs
/// no index), then answers the `grep` call with a textless `content` item.
/// Asserts `execute()` still fails open (a successful, non-error `ToolResult`)
/// AND now attaches telemetry for this arm.
/// Test: this test.
#[tokio::test]
#[cfg(unix)]
async fn execute_attaches_telemetry_when_lane_reply_has_no_text() {
    let init_resp = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "serverInfo": { "name": "fake-trusty-search", "version": "0.0.0" },
            "protocolVersion": "2024-11-05"
        }
    });
    let list_indexes_resp = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "result": { "content": [{ "type": "text", "text": "{\"indexes\":[]}" }] }
    });
    let grep_resp = json!({
        "jsonrpc": "2.0",
        "id": 3,
        // No "text" field on the content item — this is what makes
        // `mcp_text` return `None` despite a genuinely successful,
        // non-error lane reply.
        "result": { "content": [{ "type": "text" }] }
    });

    let script_path = write_fake_mcp_server(&[init_resp, list_indexes_resp, grep_resp]);
    let tool = TrustySearchTool::new(std::env::temp_dir()).with_binary(
        script_path
            .to_str()
            .expect("utf8 fake mcp script path")
            .to_string(),
    );

    let result = tool
        .execute(json!({ "query": "TODO", "mode": "grep" }))
        .await;

    let _ = std::fs::remove_file(&script_path);

    assert!(
        !result.is_error(),
        "a malformed-but-successful lane reply is still fail-open success: {}",
        result.content()
    );
    assert!(
        result.telemetry().is_some(),
        "execute()'s doc comment promises every lane-reached path attaches \
         telemetry, but the no-extractable-text arm attached none"
    );
}
