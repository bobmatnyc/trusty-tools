//! Unit + dispatch tests for the trusty-memory MCP tool surface.
//!
//! Why: exercises the gate helpers, per-tool handlers, the dispatcher, and the
//! BM25 enqueue path. Split out of the former monolithic `tools.rs` (issue
//! #607) into a sibling test file (1500-SLOC test cap applies).
//! What: the `#[cfg(test)] mod tests` body moved verbatim; `super::*` now
//! resolves against `tools::mod`, which re-exports every item these tests use.
//! Test: this IS the test module.

use super::*;
use crate::AppState;
use serde_json::json;
use trusty_common::memory_core::palace::PalaceId;
use uuid::Uuid;

/// Why: Issue #234 — previously we `mem::forget`ed the `TempDir` so tests
/// could keep using `AppState` without juggling the directory handle, but
/// that leaked one temp directory per test (262+ accumulated each run).
/// What: Returns the `TempDir` alongside the `AppState` so the caller can
/// bind it (`let (state, _tmp) = ...;`) and let drop semantics clean up
/// when the test scope ends.
/// Test: Every test in this module that constructs state.
///
/// Why (issue #88): sets `TRUSTY_SKIP_PALACE_ENFORCEMENT=1` so that
/// existing tests that call `palace_create` with arbitrary names continue
/// to work. The enforcement gate in `handle_palace_create` bypasses the
/// project-slug check when this env var is set, which is the correct
/// behaviour for test helpers that point at isolated tempdirs. Production
/// processes never set this variable.
fn test_state() -> (AppState, tempfile::TempDir) {
    // SAFETY: tests in this module run in-process; setting the bypass var
    // here races with any test that reads env before or after, but since
    // the value is "set to the same constant forever" once any test runs,
    // the race is benign — all tests should see "1" within the first
    // iteration. Tests that need stricter serialisation already use
    // `env_test_lock()`.
    unsafe {
        std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let state = AppState::new(root);
    // Pre-existing tests exercise functional paths — flip to Ready so the
    // issue #911 warming preflight does not reject them.
    state.set_ready();
    (state, tmp)
}

/// Why: warming-state tests need a fresh state that explicitly stays in
/// Warming. The `test_state()` helper flips to Ready by default; this
/// variant skips that step so the preflight guard can be tested.
/// Test: `remember_returns_warming_error_while_state_is_warming`,
///       `recall_returns_warming_error_while_state_is_warming`,
///       `note_returns_warming_error_while_state_is_warming`.
fn test_state_warming() -> (crate::AppState, tempfile::TempDir) {
    // Use OnceLock so the env var is written exactly once across all
    // parallel test threads — avoids the unsynchronised set_var race while
    // remaining consistent with the idempotent-write approach in test_state().
    static SKIP_ENFORCEMENT_SET: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    SKIP_ENFORCEMENT_SET.get_or_init(|| unsafe {
        std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
    });
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let state = crate::AppState::new(root);
    // Deliberately do NOT call set_ready() — state stays Warming.
    (state, tmp)
}

/// Why: Issue #26 — when the server is started with `--palace`, the
/// `tools/list` schema must drop `palace` from the `required` array for
/// every tool that accepts it, so MCP clients know it's optional.
/// Test: Build the schema both ways and check the required arrays.
#[test]
fn tool_definitions_drops_palace_required_when_default_set() {
    let with_default = tool_definitions_with(true);
    let without_default = tool_definitions_with(false);
    for (name, palace_required_when_no_default) in [
        ("memory_remember", true),
        ("memory_recall", true),
        ("memory_recall_deep", true),
        ("memory_list", true),
        ("memory_forget", true),
        ("palace_info", true),
        ("palace_compact", true),
        ("kg_assert", true),
        ("kg_query", true),
        // Issue #664: add_alias and discover_aliases now include `palace`
        // in their schema and follow the same conditional-required pattern.
        ("add_alias", true),
        ("discover_aliases", true),
    ] {
        for (defs, has_default) in [(&with_default, true), (&without_default, false)] {
            let tools = defs["tools"].as_array().unwrap();
            let tool = tools.iter().find(|t| t["name"] == name).unwrap();
            let required: Vec<&str> = tool["inputSchema"]["required"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|v| v.as_str())
                .collect();
            let palace_required = required.contains(&"palace");
            let expected = palace_required_when_no_default && !has_default;
            assert_eq!(
                palace_required, expected,
                "tool={name} has_default={has_default} required={required:?}"
            );
        }
    }
}

#[test]
fn tool_definitions_lists_all_tools() {
    let defs = tool_definitions();
    let tools = defs
        .get("tools")
        .and_then(|t| t.as_array())
        .expect("tools array");
    // 34 original + 3 task tools (task_add, task_list, task_complete, issue #1722)
    assert_eq!(tools.len(), 37);
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
        .collect();
    for expected in [
        "memory_remember",
        "memory_note",
        "memory_recall",
        "memory_recall_deep",
        "memory_list",
        "memory_forget",
        "palace_create",
        "palace_delete",
        "palace_update",
        "palace_list",
        "palace_info",
        "palace_compact",
        "kg_assert",
        "kg_query",
        "memory_recall_all",
        "kg_gaps",
        "add_alias",
        "list_prompt_facts",
        "remove_prompt_fact",
        "get_prompt_context",
        "discover_aliases",
        "kg_bootstrap",
        "memory_send_message",
        "upgrade",
        "console_metrics",
        "chat_session_create",
        "chat_session_add_turn",
        "chat_session_get",
        "chat_session_recall",
        "chat_session_list",
        "chat_session_delete",
        "chat_turn_append",
        "dream_consolidate_room",
        "palace_dream",
        // spec-001 Phase 4 (issue #1722):
        "task_add",
        "task_list",
        "task_complete",
    ] {
        assert!(names.contains(&expected), "missing tool: {expected}");
    }
}

/// Why: Confirm `palace_create` actually persists a palace under the
/// configured data root and `palace_list` then sees it.
#[tokio::test]
async fn dispatch_palace_create_persists() {
    let (state, _tmp) = test_state();
    let created = dispatch_tool(&state, "palace_create", json!({"name": "alpha"}))
        .await
        .expect("palace_create");
    assert_eq!(created["palace_id"], "alpha");

    let listed = dispatch_tool(&state, "palace_list", json!({}))
        .await
        .expect("palace_list");
    let ids = listed["palaces"].as_array().expect("palaces array");
    assert!(ids.iter().any(|v| v.as_str() == Some("alpha")));
}

/// Why (issue #1714): `force=true` bypasses slug validation with no
/// authorization check by default (single-tenant mode, unchanged
/// behaviour) — confirm that default mode still lets `force=true` through
/// end-to-end via the MCP `palace_create` tool.
#[tokio::test]
async fn dispatch_palace_create_force_allowed_in_single_tenant_default() {
    let (state, _tmp) = test_state();
    let created = dispatch_tool(
        &state,
        "palace_create",
        json!({"name": "forced-slug", "force": true}),
    )
    .await
    .expect("palace_create with force must succeed in default single-tenant mode");
    assert_eq!(created["palace_id"], "forced-slug");
}

/// Why (issue #1714): in multi-tenant mode there is no capability model yet
/// to decide whether the caller may bypass slug validation, so
/// `authz::authorize_force_palace_create` fails closed and `palace_create
/// force=true` must be refused end-to-end through the MCP dispatcher.
/// `force=false` (or omitted) is unaffected by the multi-tenant flag.
#[tokio::test]
async fn dispatch_palace_create_force_denied_in_multi_tenant_mode() {
    let (mut state, _tmp) = test_state();
    state.multi_tenant_mode = true;

    let err = dispatch_tool(
        &state,
        "palace_create",
        json!({"name": "forced-slug", "force": true}),
    )
    .await
    .expect_err("force=true must be refused in multi-tenant mode");
    assert!(format!("{err:#}").contains("authorization signal"));
}

/// Why: End-to-end confirmation that a remembered drawer is recallable
/// through the MCP tool surface using the real embedder + retrieval path.
#[tokio::test]
async fn dispatch_remember_then_recall() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "beta"}))
        .await
        .expect("palace_create");

    let remembered = dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": "beta",
            "text": "Quokkas are the happiest marsupials in Australia by general consensus",
            "room": "General",
            "tags": ["wildlife"],
        }),
    )
    .await
    .expect("memory_remember");
    assert!(remembered["drawer_id"].as_str().is_some());

    let recalled = dispatch_tool(
        &state,
        "memory_recall",
        json!({"palace": "beta", "query": "Quokkas marsupials Australia", "top_k": 5}),
    )
    .await
    .expect("memory_recall");
    let results = recalled["results"].as_array().expect("results");
    assert!(
        results
            .iter()
            .any(|r| r["content"].as_str().unwrap_or("").contains("Quokkas")),
        "expected to recall the Quokkas drawer; got {results:?}"
    );
}

/// Why: Issue #97 — `memory_remember` should auto-populate the KG so
/// every drawer leaves a graph trail. Confirm a freshly remembered
/// drawer leaves `has-tag`/`in-room`/`mentions` triples (using the
/// tag-as-subject encoding) in the palace KG.
/// What: Create a palace, write one drawer with known tags + room +
/// recognisable pattern content, then read all active triples and
/// assert the expected auto-extracted shapes show up.
/// Test: This test.
#[tokio::test]
async fn auto_kg_extraction_hooks_into_memory_remember() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "kgauto"}))
        .await
        .expect("palace_create");

    let _ = dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": "kgauto",
            "text": "Rustc is a compiler for the Rust language; tracks #performance",
            "room": "Backend",
            "tags": ["compiler", "language"],
        }),
    )
    .await
    .expect("memory_remember");

    let handle = open_palace_handle(&state, "kgauto").expect("open palace");
    let triples = handle.kg.list_active(1000, 0).await.expect("list_active");
    let auto: Vec<_> = triples
        .iter()
        .filter(|t| t.provenance.as_deref() == Some(crate::kg_extract::AUTO_PROVENANCE))
        .collect();
    assert!(
        !auto.is_empty(),
        "expected at least one auto-extracted triple after memory_remember; got: {triples:?}"
    );
    // Tag/room/topic encoding: each metadata category becomes its own
    // subject so multiple tags coexist under the KG's "one active
    // triple per (s, p)" invariant. Confirm both tags survive.
    assert!(
        auto.iter()
            .any(|t| t.subject == "tag:compiler" && t.predicate == "tags"),
        "expected tag:compiler edge in auto subset: {auto:?}"
    );
    assert!(
        auto.iter()
            .any(|t| t.subject == "tag:language" && t.predicate == "tags"),
        "expected tag:language edge in auto subset: {auto:?}"
    );
    assert!(
        auto.iter()
            .any(|t| t.subject == "room:Backend" && t.predicate == "contains"),
        "expected room:Backend edge in auto subset: {auto:?}"
    );
    assert!(
        auto.iter().any(|t| t.predicate == "mentioned-in"),
        "expected at least one #hashtag mention triple in auto subset: {auto:?}"
    );
}

/// Why: Issue #97 — failures inside the auto-extraction pass must
/// never fail the parent write. We can't easily inject a failure into
/// the live `KnowledgeGraph::assert`, so this test exercises the
/// documented contract by verifying the parent `memory_remember`
/// succeeds even when the content produces zero auto-extracted triples
/// (the closest natural no-op to "extraction failed").
/// What: Remember a drawer with empty tags + minimal patternless
/// content; confirm `memory_remember` returns a drawer id and no
/// auto-extracted triples are emitted (the only built-in auto triples
/// would have come from tags/room/hashtags/patterns).
/// Test: This test.
#[tokio::test]
async fn auto_kg_extraction_no_op_does_not_fail_remember() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "kgnoop"}))
        .await
        .expect("palace_create");

    let res = dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": "kgnoop",
            // 8+ tokens to clear MCP_MIN_TOKENS; no tags, no room, no
            // hashtags, no pattern triggers.
            "text": "The quick brown fox jumped over the lazy dog repeatedly",
        }),
    )
    .await
    .expect("memory_remember should succeed even when extraction yields nothing");
    assert!(res["drawer_id"].as_str().is_some());
}

/// Why: Confirm `kg_assert` writes a triple and `kg_query` returns it
/// through the MCP tool surface.
#[tokio::test]
async fn dispatch_kg_assert_then_query() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "gamma"}))
        .await
        .expect("palace_create");

    let _ = dispatch_tool(
        &state,
        "kg_assert",
        json!({
            "palace": "gamma",
            "subject": "alice",
            "predicate": "works_at",
            "object": "Acme",
            "confidence": 0.9,
            "provenance": "test",
        }),
    )
    .await
    .expect("kg_assert");

    let queried = dispatch_tool(
        &state,
        "kg_query",
        json!({"palace": "gamma", "subject": "alice"}),
    )
    .await
    .expect("kg_query");
    let triples = queried["triples"].as_array().expect("triples array");
    assert_eq!(triples.len(), 1);
    assert_eq!(triples[0]["object"], "Acme");
    assert_eq!(triples[0]["predicate"], "works_at");
}

/// Why: Issue #53 — verify the MCP `kg_gaps` tool returns whatever was
/// last cached on the registry. Two cases: empty cache returns an empty
/// array, and a seeded cache returns the cached entries verbatim.
/// What: Creates a palace, dispatches `kg_gaps` (expects empty), then
/// directly seeds the registry cache via `set_gaps` and dispatches again
/// to confirm the entry round-trips through serialization.
/// Test: This test itself.
#[tokio::test]
async fn dispatch_kg_gaps_returns_cached() {
    use trusty_common::memory_core::community::KnowledgeGap;

    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "delta"}))
        .await
        .expect("palace_create");

    // Empty cache → empty gaps list (not an error).
    let initial = dispatch_tool(&state, "kg_gaps", json!({"palace": "delta"}))
        .await
        .expect("kg_gaps empty");
    let gaps = initial["gaps"].as_array().expect("gaps array");
    assert_eq!(gaps.len(), 0);

    // Seed the cache and re-dispatch.
    state.registry.set_gaps(
        PalaceId::new("delta"),
        vec![KnowledgeGap {
            entities: vec!["x".to_string(), "y".to_string()],
            internal_density: 0.05,
            external_bridges: 0,
            suggested_exploration: "Explore connections between x and y".to_string(),
        }],
    );
    let seeded = dispatch_tool(&state, "kg_gaps", json!({"palace": "delta"}))
        .await
        .expect("kg_gaps seeded");
    let gaps = seeded["gaps"].as_array().expect("gaps array");
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0]["entities"][0], "x");
    assert_eq!(gaps[0]["external_bridges"], 0);
    assert!(gaps[0]["suggested_exploration"]
        .as_str()
        .unwrap()
        .contains("x"));
}

/// Why: Issue #42 — `add_alias` must (a) assert the triple in the KG,
/// (b) cause `list_prompt_facts` to surface it, (c) refresh the prompt
/// cache so `prompts/get` returns it, and (d) be reversible via
/// `remove_prompt_fact`.
#[tokio::test]
async fn add_alias_round_trip_through_prompt_cache() {
    // Issue #234: bind `_tmp` so the directory is cleaned up on drop at
    // end of scope (previously we leaked via `std::mem::forget`).
    let _tmp = tempfile::tempdir().expect("tempdir");
    let root = _tmp.path().to_path_buf();
    let state = AppState::new(root).with_default_palace(Some("ctx".to_string()));

    // Pre-create the default palace.
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "ctx"}))
        .await
        .expect("palace_create");

    // (a) add_alias asserts the triple.
    let added = dispatch_tool(
        &state,
        "add_alias",
        json!({"short": "tga", "full": "trusty-git-analytics"}),
    )
    .await
    .expect("add_alias");
    assert_eq!(added["asserted"], true);
    assert_eq!(added["short"], "tga");

    // (b) list_prompt_facts surfaces it.
    let listed = dispatch_tool(&state, "list_prompt_facts", json!({}))
        .await
        .expect("list_prompt_facts");
    let facts = listed["facts"].as_array().expect("facts array");
    assert!(
        facts.iter().any(|f| f["subject"] == "tga"
            && f["predicate"] == "is_alias_for"
            && f["object"] == "trusty-git-analytics"),
        "expected tga alias in facts; got {facts:?}"
    );

    // (c) prompt cache has been refreshed with the formatted block.
    {
        let guard = state.prompt_context_cache.read().await;
        assert!(
            guard.formatted.contains("tga → trusty-git-analytics"),
            "prompt cache should contain alias; got: {}",
            guard.formatted
        );
    }

    // add_alias with `extra` appends parenthetical context.
    let _ = dispatch_tool(
        &state,
        "add_alias",
        json!({"short": "tm", "full": "trusty-memory", "extra": "the MCP frontend"}),
    )
    .await
    .expect("add_alias with extra");
    {
        let guard = state.prompt_context_cache.read().await;
        assert!(
            guard
                .formatted
                .contains("tm → trusty-memory (the MCP frontend)"),
            "alias with extra not formatted; got: {}",
            guard.formatted
        );
    }

    // (d) remove_prompt_fact retracts and refreshes.
    let removed = dispatch_tool(
        &state,
        "remove_prompt_fact",
        json!({"subject": "tga", "predicate": "is_alias_for"}),
    )
    .await
    .expect("remove_prompt_fact");
    assert_eq!(removed["removed"], true);
    {
        let guard = state.prompt_context_cache.read().await;
        assert!(
            !guard.formatted.contains("tga → trusty-git-analytics"),
            "retracted alias still in cache: {}",
            guard.formatted
        );
        assert!(
            guard.formatted.contains("tm → trusty-memory"),
            "non-retracted alias missing from cache: {}",
            guard.formatted
        );
    }

    // Removing a non-existent fact reports not found.
    let missing = dispatch_tool(
        &state,
        "remove_prompt_fact",
        json!({"subject": "nope", "predicate": "is_alias_for"}),
    )
    .await
    .expect("remove_prompt_fact missing");
    assert_eq!(missing["removed"], false);
}

/// Why (issue #664): `add_alias` must accept an explicit `palace` arg when
/// the server has no `--palace` default, and reject with a clear error when
/// both are absent.
/// What: (a) explicit palace succeeds and refreshes the cache; (b) no
/// palace + no default returns an error mentioning both `palace` and
/// `add_alias`.
/// Test: this function.
#[tokio::test]
async fn add_alias_palace_arg_required_without_server_default() {
    // (a) explicit palace succeeds — use test_state() so palace-name
    // enforcement is bypassed (sets TRUSTY_SKIP_PALACE_ENFORCEMENT=1).
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "p"}))
        .await
        .expect("palace_create");
    let added = dispatch_tool(
        &state,
        "add_alias",
        json!({"palace": "p", "short": "tga", "full": "trusty-git-analytics"}),
    )
    .await
    .expect("add_alias with explicit palace");
    assert_eq!(added["asserted"], true);
    let guard = state.prompt_context_cache.read().await;
    assert!(guard.formatted.contains("tga → trusty-git-analytics"));

    // (b) no palace + no default → clear error (state2 has no default_palace).
    drop(guard);
    let (state2, _tmp2) = test_state();
    let err = dispatch_tool(&state2, "add_alias", json!({"short": "x", "full": "y"}))
        .await
        .expect_err("should fail without palace");
    let msg = format!("{err:#}");
    assert!(msg.contains("palace"), "error must mention 'palace': {msg}");
    assert!(msg.contains("add_alias"), "error must name tool: {msg}");
}

/// Why (issue #42): `get_prompt_context` is the per-message replacement
/// for the deprecated `prompts/get` flow. It must (a) return a hint when
/// the cache is empty, (b) return the formatted block when populated,
/// and (c) filter by `query` against subject/object case-insensitively.
#[tokio::test]
async fn get_prompt_context_serves_cache_and_filters() {
    let (state, _tmp) = test_state();

    // (a) empty cache -> "No prompt facts stored yet."
    let resp = dispatch_tool(&state, "get_prompt_context", json!({}))
        .await
        .expect("get_prompt_context empty");
    assert_eq!(resp.as_str().unwrap(), "No prompt facts stored yet.");

    // Populate the cache by hand with a known triple set.
    {
        let mut guard = state.prompt_context_cache.write().await;
        let triples = vec![
            (
                "tga".to_string(),
                "is_alias_for".to_string(),
                "trusty-git-analytics".to_string(),
            ),
            (
                "tm".to_string(),
                "is_alias_for".to_string(),
                "trusty-memory".to_string(),
            ),
            (
                "fact-1".to_string(),
                "is_fact".to_string(),
                "MSRV is 1.88".to_string(),
            ),
        ];
        let formatted = crate::prompt_facts::build_prompt_context(&triples);
        *guard = crate::prompt_facts::PromptFactsCache { triples, formatted };
    }

    // (b) unfiltered -> serves the full formatted block.
    let resp = dispatch_tool(&state, "get_prompt_context", json!({}))
        .await
        .expect("get_prompt_context populated");
    let text = resp.as_str().expect("string body");
    assert!(text.contains("tga → trusty-git-analytics"));
    assert!(text.contains("tm → trusty-memory"));
    assert!(text.contains("MSRV is 1.88"));

    // (c) filtered to "tga" -> only the matching alias.
    let resp = dispatch_tool(&state, "get_prompt_context", json!({"query": "tga"}))
        .await
        .expect("get_prompt_context filtered");
    let text = resp.as_str().expect("string body");
    assert!(text.contains("tga → trusty-git-analytics"));
    assert!(!text.contains("tm → trusty-memory"));
    assert!(!text.contains("MSRV is 1.88"));

    // Case-insensitive match on the object side.
    let resp = dispatch_tool(&state, "get_prompt_context", json!({"query": "MEMORY"}))
        .await
        .expect("get_prompt_context case-insensitive");
    let text = resp.as_str().expect("string body");
    assert!(text.contains("tm → trusty-memory"));
    assert!(!text.contains("tga → trusty-git-analytics"));

    // No match -> "No project context found matching your query."
    let resp = dispatch_tool(
        &state,
        "get_prompt_context",
        json!({"query": "zzz-nonexistent"}),
    )
    .await
    .expect("get_prompt_context no-match");
    assert_eq!(
        resp.as_str().unwrap(),
        "No project context found matching your query."
    );

    // Empty/whitespace `query` is treated as no filter.
    let resp = dispatch_tool(&state, "get_prompt_context", json!({"query": "   "}))
        .await
        .expect("get_prompt_context whitespace");
    let text = resp.as_str().expect("string body");
    assert!(text.contains("tga → trusty-git-analytics"));
    assert!(text.contains("tm → trusty-memory"));
}

/// Why (issue #42): `discover_aliases` must (a) auto-discover the
/// canonical workspace shorthand (`tga → trusty-git-analytics`),
/// (b) assert each discovery as an `is_alias_for` triple, (c) refresh
/// the prompt cache, and (d) dedupe on a second invocation — the second
/// call should report zero new and N already_known.
/// Test: this test itself.
#[tokio::test]
async fn dispatch_discover_aliases_inserts_new_and_dedupes() {
    // Issue #234: bind `_tmp` so the directory is cleaned up on drop at
    // end of scope (previously we leaked via `std::mem::forget`).
    let _tmp = tempfile::tempdir().expect("tempdir");
    let root = _tmp.path().to_path_buf();
    let state = AppState::new(root).with_default_palace(Some("disc".to_string()));
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "disc"}))
        .await
        .expect("palace_create");

    // Use the live workspace root so the discovery actually finds
    // something. CARGO_MANIFEST_DIR points at the crate dir; walk up
    // twice to the workspace root.
    let workspace_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf();

    let first = dispatch_tool(
        &state,
        "discover_aliases",
        json!({"project_root": workspace_root.to_string_lossy()}),
    )
    .await
    .expect("discover_aliases first");

    let new_count = first["new"].as_u64().expect("new is u64");
    assert!(new_count > 0, "expected new discoveries on first call");
    let discovered = first["discovered"].as_array().expect("discovered array");
    assert!(
        discovered
            .iter()
            .any(|d| d["short"] == "tga" && d["full"] == "trusty-git-analytics"),
        "expected tga alias in discoveries; got {discovered:?}"
    );

    // The prompt cache must contain the new alias after discovery.
    {
        let guard = state.prompt_context_cache.read().await;
        assert!(
            guard.formatted.contains("tga → trusty-git-analytics"),
            "prompt cache missing tga alias after discover_aliases; got: {}",
            guard.formatted
        );
    }

    // Second invocation should report zero new and at least `new_count`
    // already_known — the same discoveries are now in the KG.
    let second = dispatch_tool(
        &state,
        "discover_aliases",
        json!({"project_root": workspace_root.to_string_lossy()}),
    )
    .await
    .expect("discover_aliases second");
    assert_eq!(second["new"].as_u64(), Some(0), "expected 0 new on rerun");
    let already_known = second["already_known"].as_u64().expect("already_known");
    assert!(
        already_known >= new_count,
        "expected already_known >= {new_count}, got {already_known}"
    );
}

/// Why (issue #60): `palace_create` must auto-seed temporal metadata so
/// every new palace has at least `created_at` + `bootstrapped_at`
/// triples — without auto-bootstrap, brand-new palaces had a zero-triple
/// KG and no signal to users that they were supposed to seed it.
/// Test: create a palace, then query the seeded subject (the palace id)
/// and confirm the temporal triples are present.
#[tokio::test]
async fn palace_create_auto_seeds_temporal_metadata() {
    let (state, _tmp) = test_state();
    let created = dispatch_tool(&state, "palace_create", json!({"name": "auto"}))
        .await
        .expect("palace_create");
    assert_eq!(created["palace_id"], "auto");
    // bootstrap summary is present on success
    let summary = &created["bootstrap"];
    assert!(summary.is_object(), "expected bootstrap summary object");
    assert!(summary["triples_asserted"].as_u64().unwrap_or(0) >= 2);

    let queried = dispatch_tool(
        &state,
        "kg_query",
        json!({"palace": "auto", "subject": "auto"}),
    )
    .await
    .expect("kg_query");
    let triples = queried["triples"].as_array().expect("triples");
    let predicates: Vec<&str> = triples
        .iter()
        .filter_map(|t| t["predicate"].as_str())
        .collect();
    assert!(
        predicates.contains(&"created_at"),
        "expected created_at after palace_create; got {predicates:?}",
    );
    assert!(
        predicates.contains(&"bootstrapped_at"),
        "expected bootstrapped_at after palace_create; got {predicates:?}",
    );
    // Hint must NOT appear when triples are present.
    assert!(
        queried.get("hint").is_none(),
        "hint should be absent when triples exist"
    );
}

/// Why (issue #60): `kg_query` against a subject with no triples must
/// surface a `hint` field pointing the user at `kg_bootstrap` /
/// `kg_assert`. Without the hint, brand-new palaces returned empty
/// arrays with no breadcrumb back to the seeding tools.
#[tokio::test]
async fn kg_query_emits_hint_when_palace_empty() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "hinted"}))
        .await
        .expect("palace_create");
    // Query a subject that auto-bootstrap did NOT seed.
    let queried = dispatch_tool(
        &state,
        "kg_query",
        json!({"palace": "hinted", "subject": "unrelated-subject"}),
    )
    .await
    .expect("kg_query");
    assert_eq!(queried["triples"].as_array().unwrap().len(), 0);
    let hint = queried["hint"].as_str().expect("hint field present");
    assert!(hint.contains("kg_bootstrap"));
    assert!(hint.contains("kg_assert"));
}

/// Why (issue #60): `kg_bootstrap` against the live workspace root must
/// extract Cargo facts (language, version, rust-version) and the git
/// origin URL, then make them queryable through `kg_query`.
#[tokio::test]
async fn kg_bootstrap_seeds_workspace_facts() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "ws"}))
        .await
        .expect("palace_create");

    let workspace_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf();

    let result = dispatch_tool(
        &state,
        "kg_bootstrap",
        json!({"palace": "ws", "project_path": workspace_root.to_string_lossy()}),
    )
    .await
    .expect("kg_bootstrap");
    assert!(result["triples_asserted"].as_u64().unwrap() > 0);
    let subject = result["project_subject"]
        .as_str()
        .expect("project_subject")
        .to_string();

    // Verify the workspace facts are queryable.
    let queried = dispatch_tool(
        &state,
        "kg_query",
        json!({"palace": "ws", "subject": subject}),
    )
    .await
    .expect("kg_query");
    let triples = queried["triples"].as_array().expect("triples");
    let predicates: Vec<&str> = triples
        .iter()
        .filter_map(|t| t["predicate"].as_str())
        .collect();
    // Either Rust language (single-crate manifest) or workspace member
    // triples must appear; the trusty-tools root manifest is a workspace
    // so we expect has_workspace_member.
    assert!(
        predicates.contains(&"has_workspace_member") || predicates.contains(&"has_language"),
        "expected workspace/language fact; got {predicates:?}",
    );
    // source_repo from .git/config.
    assert!(
        predicates.contains(&"source_repo"),
        "expected source_repo from .git/config; got {predicates:?}",
    );
    // Temporal metadata always.
    assert!(predicates.contains(&"bootstrapped_at"));
}

// -----------------------------------------------------------------
// Issue #215 — content gate for short prompts
// -----------------------------------------------------------------

/// Why: short single-word content with no `context` must be skipped so
/// the palace doesn't accumulate orphan "yes"/"ok" fragments.
/// What: passes "yes" through the gate and asserts `None`.
/// Test: itself.
#[test]
fn content_gate_blocks_short_no_context() {
    assert_eq!(content_gate("yes", None), None);
    assert_eq!(content_gate("ok", None), None);
    assert_eq!(
        content_gate("  no thanks  ", None),
        None,
        "2 words still < 4"
    );
    assert_eq!(
        content_gate("one two three", None),
        None,
        "3 words still < 4"
    );
}

/// Why: when the caller wraps a short answer with `context`, the gate
/// must keep the content but prepend the context with a `---` separator
/// so the stored memory has standalone value.
/// What: passes "yes" + context, asserts the combined shape.
/// Test: itself.
#[test]
fn content_gate_wraps_short_with_context() {
    let combined = content_gate(
        "yes",
        Some("Do you want to enable auto-bootstrap on new palaces?"),
    )
    .expect("context should unlock the gate");
    assert_eq!(
        combined,
        "Do you want to enable auto-bootstrap on new palaces?\n\n---\n\nyes",
    );
    // Even content that would otherwise pass the threshold is wrapped
    // when context is supplied — the caller is explicit.
    let combined = content_gate(
        "the quick brown fox jumps over the lazy dog",
        Some("Famous typing pangram"),
    )
    .expect("long content + context still combines");
    assert!(combined.starts_with("Famous typing pangram"));
    assert!(combined.contains("\n\n---\n\n"));
    assert!(combined.ends_with("the quick brown fox jumps over the lazy dog"));
}

/// Why: content that meets the threshold should pass through untouched
/// when no context is supplied — the gate must not rewrite or reformat
/// passing content.
/// What: passes a 5-word string through and asserts the output equals
/// the input verbatim.
/// Test: itself.
#[test]
fn content_gate_keeps_long() {
    let body = "User prefers snake_case for python";
    let kept = content_gate(body, None).expect(">= 4 words passes");
    assert_eq!(kept, body, "passing content must round-trip verbatim");
    // Exactly four words is the boundary — it must pass.
    let boundary = "one two three four";
    assert_eq!(content_gate(boundary, None).as_deref(), Some(boundary));
}

/// Why: an empty or whitespace-only `context` argument must be treated
/// the same as `None` so callers can't accidentally smuggle short
/// content through by passing `""`.
/// What: passes blank context with short content and asserts the gate
/// still skips the write.
/// Test: itself.
#[test]
fn content_gate_blank_context_treated_as_none() {
    assert_eq!(content_gate("yes", Some("")), None);
    assert_eq!(content_gate("yes", Some("   ")), None);
    assert_eq!(content_gate("yes", Some("\n\t")), None);
}

/// Why: the dispatch path must return a structured "skipped" envelope
/// without writing to the store when the gate fires on `memory_remember`.
/// What: dispatch with single-word `text` and no `context`; assert the
/// response carries `status = "skipped"` and that no drawer landed.
/// Test: itself.
#[tokio::test]
async fn dispatch_remember_skips_short_no_context() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "gate"}))
        .await
        .expect("palace_create");

    let res = dispatch_tool(
        &state,
        "memory_remember",
        json!({"palace": "gate", "text": "yes"}),
    )
    .await
    .expect("memory_remember (short)");
    assert_eq!(res["status"], "skipped");
    assert!(res["reason"]
        .as_str()
        .unwrap_or("")
        .contains("content gate"));
    // No drawer was written.
    let listed = dispatch_tool(
        &state,
        "memory_list",
        json!({"palace": "gate", "limit": 10}),
    )
    .await
    .expect("memory_list");
    let drawers = listed["drawers"].as_array().expect("drawers array");
    assert!(
        drawers.is_empty(),
        "no drawer should be written; got {drawers:?}"
    );
}

/// Why: confirm the `context` argument unlocks a short content write —
/// the resulting drawer must carry the combined `context + content`
/// body so downstream recall sees the wrapping.
/// What: dispatch with one-word text plus a context arg, then list and
/// assert the stored content begins with the context and ends with the
/// original short body.
/// Test: itself.
#[tokio::test]
async fn dispatch_remember_with_context_writes_combined() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "ctxgate"}))
        .await
        .expect("palace_create");

    let res = dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": "ctxgate",
            "text": "yes",
            "context": "Do you want to enable auto-bootstrap on new palaces?",
            "force": true,
        }),
    )
    .await
    .expect("memory_remember (with context)");
    assert_eq!(res["status"], "stored");

    let listed = dispatch_tool(
        &state,
        "memory_list",
        json!({"palace": "ctxgate", "limit": 10}),
    )
    .await
    .expect("memory_list");
    let drawers = listed["drawers"].as_array().expect("drawers array");
    assert_eq!(drawers.len(), 1);
    let body = drawers[0]["content"].as_str().expect("content");
    assert!(body.starts_with("Do you want to enable auto-bootstrap"));
    assert!(body.contains("\n\n---\n\n"));
    assert!(body.ends_with("yes"));
}

/// Why: `memory_note` must respect the same content gate as
/// `memory_remember` so the short-prompt protection is uniform across
/// the write surface.
/// What: dispatch `memory_note` with a one-word content and no context;
/// assert it returns a skipped envelope and no drawer is written.
/// Test: itself.
#[tokio::test]
async fn dispatch_note_skips_short_no_context() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "noteg"}))
        .await
        .expect("palace_create");

    let res = dispatch_tool(
        &state,
        "memory_note",
        json!({"palace": "noteg", "content": "ok"}),
    )
    .await
    .expect("memory_note (short)");
    assert_eq!(res["status"], "skipped");
    let listed = dispatch_tool(
        &state,
        "memory_list",
        json!({"palace": "noteg", "limit": 10}),
    )
    .await
    .expect("memory_list");
    assert!(listed["drawers"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn dispatch_unknown_tool_errors() {
    let (state, _tmp) = test_state();
    let err = dispatch_tool(&state, "does_not_exist", json!({}))
        .await
        .expect_err("should error");
    assert!(err.to_string().contains("unknown tool"));
}

// -----------------------------------------------------------------
// Issue #220 — blocklist pattern + rolling dedup window
// -----------------------------------------------------------------

/// Why: the blocklist gate must reject Claude Code tool-use captures
/// (`Tool use: Bash`, `Tool use: Edit File: …`) because those entries
/// have no standalone semantic value.
/// What: passes the literal prefix and a realistic example through
/// the gate and asserts a match is returned (blocked).
/// Test: itself.
#[test]
fn blocklist_gate_blocks_tool_use() {
    assert!(blocklist_gate("Tool use: Bash").is_some());
    assert!(blocklist_gate("Tool use: Edit File: /Users/me/Projects/foo/bar.rs").is_some());
    // Leading whitespace should not let it through.
    assert!(blocklist_gate("   Tool use: Read").is_some());
}

/// Why: session-lifecycle events are auto-emitted by Claude Code and
/// should not pollute the palace.
/// What: passes the prefix through the gate and asserts a match.
/// Test: itself.
#[test]
fn blocklist_gate_blocks_session_ended() {
    assert!(
        blocklist_gate("Claude Code session ended: 1d2c3b4a-0000-0000-0000-000000000000").is_some()
    );
    assert!(blocklist_gate("Claude Code session started").is_some());
}

/// Why: normal user content (with no blocklist substring) must pass
/// the gate untouched so the regular content gate (issue #215) gets
/// to make the next decision.
/// What: passes normal prose / facts through and asserts no match.
/// Test: itself.
#[test]
fn blocklist_gate_passes_normal_content() {
    assert!(blocklist_gate("User prefers snake_case for python").is_none());
    assert!(blocklist_gate("Quokkas are the happiest marsupials in Australia").is_none());
    assert!(blocklist_gate("Note: refactor the dispatcher next sprint").is_none());
    // Substring-only — a tool-use mention inside legitimate prose is
    // still blocked. This is intentional: the prefix is rare enough
    // outside the auto-capture path that the false-positive rate is
    // acceptable, and a future regex upgrade can tighten it.
    assert!(blocklist_gate("I used Tool use: Bash here").is_some());
}

/// Why (issue #1481): a blocked write must name *which* pattern tripped the
/// gate so the caller can identify and remove it, replacing the previous
/// opaque "blocked pattern" envelope.
/// What: asserts the gate returns the exact matched pattern string for a
/// tool-use capture and `None` for clean prose.
/// Test: itself.
#[test]
fn blocklist_gate_names_matched_pattern() {
    assert_eq!(blocklist_gate("Tool use: Bash"), Some("Tool use: "));
    assert_eq!(
        blocklist_gate("Claude Code session ended: abc"),
        Some("Claude Code session")
    );
    assert_eq!(blocklist_gate("an ordinary engineering note"), None);
}

/// Why: the dedup gate must reject a fresh write whose content is a
/// near-duplicate (Jaro-Winkler > 0.92) of a drawer landed inside the
/// rolling window. Without this gate, bursty auto-captures inflate
/// the palace with no recall benefit (issue #220).
/// What: creates a palace, writes one drawer through the MCP path,
/// then runs the gate directly against a string that differs by one
/// trailing word — Jaro-Winkler should score that above 0.92 and the
/// gate should return `true`.
/// Test: itself.
#[tokio::test]
async fn dedup_skips_near_duplicate() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "dedup1"}))
        .await
        .expect("palace_create");

    // Land the seed drawer through the real write path so its
    // `created_at` is `Utc::now()` and falls inside the dedup window.
    let _ = dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": "dedup1",
            "text": "The quick brown fox jumped over the lazy dog repeatedly today",
        }),
    )
    .await
    .expect("memory_remember seed");

    let handle = open_palace_handle(&state, "dedup1").expect("open handle");
    // Near-duplicate: same prefix, trailing word replaced. Jaro-Winkler
    // weights the shared prefix heavily so this should clear the 0.92
    // bar comfortably.
    assert!(
        dedup_gate(
            &handle,
            "The quick brown fox jumped over the lazy dog repeatedly yesterday"
        ),
        "near-duplicate should be detected"
    );
    // Exact match also blocks.
    assert!(
        dedup_gate(
            &handle,
            "The quick brown fox jumped over the lazy dog repeatedly today"
        ),
        "exact match should be detected"
    );
}

/// Why: a write whose content is genuinely different from every drawer
/// in the window must pass the dedup gate so the palace can grow.
/// What: writes one seed drawer, then runs the gate against an
/// unrelated string. Asserts `false`.
/// Test: itself.
#[tokio::test]
async fn dedup_allows_different_content() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "dedup2"}))
        .await
        .expect("palace_create");

    let _ = dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": "dedup2",
            "text": "Quokkas are the happiest marsupials in Australia by general consensus",
        }),
    )
    .await
    .expect("memory_remember seed");

    let handle = open_palace_handle(&state, "dedup2").expect("open handle");
    // Completely different content — far below 0.92.
    assert!(
        !dedup_gate(
            &handle,
            "Rust is a systems programming language focused on safety and concurrency"
        ),
        "unrelated content should pass the dedup gate"
    );
    // Empty/whitespace content is also a pass — the content gate
    // handles the empty case upstream.
    assert!(!dedup_gate(&handle, "   "));
}

/// Why (issue #230): the dedup gate previously had a TOCTOU race —
/// two concurrent `memory_remember` calls with identical content
/// both saw the empty pre-write snapshot, both passed the gate, and
/// both wrote duplicate drawers. The per-palace write mutex on
/// `AppState` now serialises the gate-then-write sequence so the
/// second writer observes the first writer's drawer in
/// `list_drawers` and bails. This test would have failed before the
/// fix and passes after.
/// What: spawns two `tokio` tasks that race to write the same long
/// content into a fresh palace, joins both, then asserts that
/// `memory_list` returns exactly one drawer (the loser's envelope
/// carries `status = "skipped"` with a `duplicate within window`
/// reason).
/// Test: itself — fail-then-pass on this commit.
#[tokio::test]
async fn dedup_gate_blocks_concurrent_duplicate_writes() {
    let (state, _tmp) = test_state();
    let state = std::sync::Arc::new(state);
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "dedup_race"}))
        .await
        .expect("palace_create");

    // Long enough to clear the 8-token MCP filter; identical content
    // in both racers so the dedup gate is the only thing keeping
    // them from both landing.
    let text = "Concurrent identical writes must collapse to a single drawer under the dedup gate";

    let s1 = state.clone();
    let t1 = tokio::spawn(async move {
        dispatch_tool(
            &s1,
            "memory_remember",
            json!({"palace": "dedup_race", "text": text}),
        )
        .await
    });
    let s2 = state.clone();
    let t2 = tokio::spawn(async move {
        dispatch_tool(
            &s2,
            "memory_remember",
            json!({"palace": "dedup_race", "text": text}),
        )
        .await
    });
    let r1 = t1.await.expect("join t1").expect("dispatch t1");
    let r2 = t2.await.expect("join t2").expect("dispatch t2");

    // Exactly one of the two should be `stored`; the other should be
    // `skipped` with the documented duplicate-window reason.
    let statuses = [
        r1["status"].as_str().unwrap_or(""),
        r2["status"].as_str().unwrap_or(""),
    ];
    let stored = statuses.iter().filter(|s| **s == "stored").count();
    let skipped = statuses.iter().filter(|s| **s == "skipped").count();
    assert_eq!(
        stored, 1,
        "exactly one concurrent write should be stored; got responses {r1:?} {r2:?}"
    );
    assert_eq!(
        skipped, 1,
        "exactly one concurrent write should be skipped; got responses {r1:?} {r2:?}"
    );
    let skipped_reason = if r1["status"] == "skipped" {
        r1["reason"].as_str().unwrap_or("")
    } else {
        r2["reason"].as_str().unwrap_or("")
    };
    assert!(
        skipped_reason.contains("duplicate within window"),
        "skipped envelope should cite dedup reason; got {skipped_reason:?}"
    );

    // Belt-and-braces: confirm the palace contains exactly one drawer.
    let listed = dispatch_tool(
        &state,
        "memory_list",
        json!({"palace": "dedup_race", "limit": 10}),
    )
    .await
    .expect("memory_list");
    let drawers = listed["drawers"].as_array().expect("drawers array");
    assert_eq!(
        drawers.len(),
        1,
        "only one drawer should be persisted after concurrent identical writes; got {drawers:?}"
    );
}

/// Why: end-to-end confirmation that the blocklist short-circuits the
/// MCP `memory_remember` dispatch — no drawer is written, the
/// response envelope carries the documented `status = "skipped"` and
/// reason. Mirrors the issue-215 short-prompt test.
/// What: dispatch a `Tool use:` payload through `memory_remember`,
/// then `memory_list` and assert no drawer landed.
/// Test: itself.
#[tokio::test]
async fn dispatch_remember_blocks_blocklist_pattern() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "blk"}))
        .await
        .expect("palace_create");

    let res = dispatch_tool(
        &state,
        "memory_remember",
        json!({"palace": "blk", "text": "Tool use: Bash"}),
    )
    .await
    .expect("memory_remember (blocked)");
    assert_eq!(res["status"], "skipped");
    assert!(
        res["reason"]
            .as_str()
            .unwrap_or("")
            .contains("blocked pattern"),
        "reason should mention blocked pattern; got {res:?}"
    );

    let listed = dispatch_tool(&state, "memory_list", json!({"palace": "blk", "limit": 10}))
        .await
        .expect("memory_list");
    let drawers = listed["drawers"].as_array().expect("drawers array");
    assert!(drawers.is_empty(), "no drawer should be written");
}

/// Why (issue #1481): a legitimate engineering memory that references git
/// commit SHAs must be STORED, not silently dropped. This is the exact repro
/// from the bug report driven end-to-end through the MCP dispatch surface.
/// What: `memory_remember` a prose string containing two short SHAs and a PR
/// number, then `memory_list` and assert the drawer landed with its content
/// intact (no `status: skipped`).
/// Test: itself.
#[tokio::test]
async fn dispatch_remember_stores_git_sha_prose() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "shas"}))
        .await
        .expect("palace_create");

    let res = dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": "shas",
            "text": "Shipped via PR #1466 squash 0fda534e -> merge 4c536992, CI green.",
        }),
    )
    .await
    .expect("memory_remember (git sha prose)");
    assert_eq!(
        res["status"], "stored",
        "git-SHA prose must be stored, not skipped; got {res:?}"
    );
    assert!(res["drawer_id"].as_str().is_some());

    let listed = dispatch_tool(
        &state,
        "memory_list",
        json!({"palace": "shas", "limit": 10}),
    )
    .await
    .expect("memory_list");
    let drawers = listed["drawers"].as_array().expect("drawers array");
    assert_eq!(drawers.len(), 1, "exactly one drawer should land");
    assert!(
        drawers[0]["content"]
            .as_str()
            .unwrap_or("")
            .contains("4c536992"),
        "stored content must preserve the SHA; got {drawers:?}"
    );
}

/// Why (issue #1481): genuine credentials must still be blocked even after the
/// git-SHA allowlist lands, and the skip/error must name the trigger so the
/// caller can remediate.
/// What: `memory_remember` a prose string carrying a high-entropy mixed-case
/// credential token and assert the call errors with a message that names the
/// (redacted) secret token. No drawer should land.
/// Test: itself.
#[tokio::test]
async fn dispatch_remember_blocks_real_secret() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "sec"}))
        .await
        .expect("palace_create");

    let err = dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": "sec",
            "text": "deploy uses token AbCd1234EfGh5678IjKl9012 for the prod webhook auth", // pragma: allowlist secret
        }),
    )
    .await
    .expect_err("a real secret must be rejected");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("secret") && msg.contains("AbCd"),
        "rejection must name the redacted secret token; got: {msg}"
    );

    let listed = dispatch_tool(&state, "memory_list", json!({"palace": "sec", "limit": 10}))
        .await
        .expect("memory_list");
    let drawers = listed["drawers"].as_array().expect("drawers array");
    assert!(
        drawers.is_empty(),
        "no drawer should be written for a secret"
    );
}

/// Why (issue #231): the bounded BM25 indexer channel must drop excess
/// requests with a logged `warn!` rather than block the writer or grow
/// unbounded behind a slow daemon. Verifying this directly at the
/// `bm25_index_enqueue` boundary protects the back-pressure contract
/// without needing a real BM25 daemon in the test loop.
/// What: builds an `AppState` whose worker can't drain (we replace
/// `bm25_index_tx` with a fresh, deliberately-unattended channel), then
/// hammers `bm25_index_enqueue` past the bound and asserts the channel
/// reports `Full` for the overflow. We assert behaviour by inspecting
/// the channel state after the burst — the function is `void` so
/// observable evidence is "the sender stayed open and the writer never
/// blocked even when we shoved >capacity items at it."
/// Test: this test.
#[tokio::test]
async fn bm25_index_queue_drops_when_full() {
    // Build a normal AppState, then swap in a fresh bounded channel
    // *without* spawning a drain worker so we can deterministically
    // observe overflow at `try_send`.
    let (mut state, _tmp) = test_state();
    let (tx, _rx_held) = tokio::sync::mpsc::channel::<Bm25IndexRequest>(BM25_INDEX_QUEUE_CAPACITY);
    state.bm25_index_tx = tx;

    // Push CAPACITY items — these must all succeed.
    for i in 0..BM25_INDEX_QUEUE_CAPACITY {
        bm25_index_enqueue(
            &state,
            "default",
            Uuid::new_v4(),
            &format!("filler content {i}"),
        );
    }
    // Sender capacity reports 0 once filled.
    assert_eq!(
        state.bm25_index_tx.capacity(),
        0,
        "after filling, sender capacity must be 0"
    );

    // Now push another batch — these must be dropped (logged warn) and
    // must not panic, block, or close the channel.
    for i in 0..16 {
        bm25_index_enqueue(
            &state,
            "default",
            Uuid::new_v4(),
            &format!("overflow content {i}"),
        );
    }

    // The sender must still be live — the channel is not closed by a
    // full-queue drop. A subsequent send-attempt to the live receiver
    // must still return `TrySendError::Full`, not `Closed`.
    let probe_req = Bm25IndexRequest {
        palace: "default".to_string(),
        drawer_id: Uuid::new_v4().to_string(),
        content: "probe".to_string(),
        data_dir: state.data_root.join("default").join("bm25"),
    };
    let probe = state.bm25_index_tx.try_send(probe_req);
    match probe {
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
        other => panic!("expected Full overflow, got {other:?}"),
    }
}

// -------------------------------------------------------------------------
// Issue #1970 — graceful degradation while the embedder is Warming
// (supersedes the former issues #910/#911/#914 hard-error preflight, which
// blocked writes AND reads outright until the embedder finished cold-init;
// trusty-memory now mirrors trusty-search's staged-pipeline degradation:
// BM25/KG/text paths never wait on the embedder).
// -------------------------------------------------------------------------

/// Why (issue #1970): `memory_remember` must succeed immediately while the
/// daemon is still `Warming` — the KG/redb write never depends on the
/// embedder, and vector embedding is deferred to a background task rather
/// than blocking the caller behind a 30-120s cold compile.
/// What: dispatch `memory_remember` against a state that stays `Warming`,
/// assert the call succeeds (`status: "stored"`), assert the drawer is
/// immediately visible via `memory_list` (proves the synchronous KG/redb
/// portion completed), then poll (bounded, condition-based — no blind
/// sleep) until the real shared embedder backfills the vector so
/// `handle.vector_store` returns a hit for the drawer id. This proves the
/// deferred embed job is not just fired but actually completes.
///
/// Deliberately does NOT seed a mock embedder: this unit-test binary also
/// runs `dispatch_remember_then_recall` against the real, process-wide
/// `shared_embedder()` singleton, and seeding a mock here would race with
/// (and potentially poison) that test depending on execution order.
/// Test: this test.
#[tokio::test]
async fn remember_succeeds_and_defers_embedding_while_state_is_warming() {
    use trusty_common::memory_core::store::VectorStore;

    let (state, _tmp) = test_state_warming();
    let _ = dispatch_tool(
        &state,
        "palace_create",
        serde_json::json!({"name": "warmtest"}),
    )
    .await
    .expect("palace_create");

    let content = "Quokkas are famously photogenic marsupials found in Western Australia";
    let remembered = dispatch_tool(
        &state,
        "memory_remember",
        serde_json::json!({
            "palace": "warmtest",
            "text": content,
        }),
    )
    .await
    .expect("memory_remember must succeed while Warming (issue #1970)");
    assert_eq!(remembered["status"], "stored");
    let drawer_id_str = remembered["drawer_id"]
        .as_str()
        .expect("drawer_id present")
        .to_string();
    let drawer_id = Uuid::parse_str(&drawer_id_str).expect("valid uuid");

    // The text/KG portion is synchronous — no need to wait for it.
    let listed = dispatch_tool(
        &state,
        "memory_list",
        serde_json::json!({"palace": "warmtest"}),
    )
    .await
    .expect("memory_list");
    let drawers = listed["drawers"].as_array().expect("drawers array");
    assert!(
        drawers.iter().any(|d| d["drawer_id"] == drawer_id_str),
        "drawer must be listed immediately even though the embedder is warming"
    );

    // Poll (bounded) until the background embed task backfills the vector.
    let handle = open_palace_handle(&state, "warmtest").expect("open palace");
    let embedder = trusty_common::memory_core::retrieval::shared_embedder()
        .await
        .expect("shared embedder must initialise");
    // Generous bound: a cold ONNX/CoreML compile can take up to ~120s per
    // CLAUDE.md; a warm model cache (the common case once any other test in
    // this binary has touched the embedder) resolves in well under a second.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let mut backfilled = false;
    while std::time::Instant::now() < deadline {
        let vecs = embedder
            .embed_batch(&[content.to_string()])
            .await
            .expect("embed query");
        let hits = handle
            .vector_store
            .search(&vecs[0], 5)
            .await
            .expect("vector search");
        if hits
            .iter()
            .any(|h| h.drawer_id.as_bytes()[..8] == drawer_id.as_bytes()[..8])
        {
            backfilled = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    assert!(
        backfilled,
        "background embed task must backfill the vector index once the embedder is ready"
    );
}

/// Why (issue #1970): `memory_note` shares `write_drawer`'s deferred-embed
/// posture; confirm it also succeeds (rather than erroring) while Warming.
/// Test: this test.
#[tokio::test]
async fn note_succeeds_while_state_is_warming() {
    let (state, _tmp) = test_state_warming();
    let _ = dispatch_tool(
        &state,
        "palace_create",
        serde_json::json!({"name": "warmtest-note"}),
    )
    .await
    .expect("palace_create");

    let result = dispatch_tool(
        &state,
        "memory_note",
        serde_json::json!({
            "palace": "warmtest-note",
            "content": "short note content here"
        }),
    )
    .await
    .expect("memory_note must succeed while Warming (issue #1970)");
    assert_eq!(result["status"], "stored");
}

/// Why (issue #1970): `memory_recall` must never block/error on embedder
/// state — it should degrade to the BM25/L0/L1 fallback and return
/// normally while the daemon is `Warming`.
/// What: dispatch `memory_recall` against a Warming state and assert the
/// call succeeds with a well-formed (possibly empty, since BM25 isn't
/// wired up in this unit test and L1 isn't live-refreshed mid-process)
/// results array — the key assertion is the absence of a "warming up"
/// error, not the result content (see
/// `bm25_hits_hydrate_from_handle_during_warmup` for content-level
/// coverage of the fallback's BM25 hydration path).
/// Test: this test.
#[tokio::test]
async fn recall_does_not_error_while_state_is_warming() {
    let (state, _tmp) = test_state_warming();
    let _ = dispatch_tool(
        &state,
        "palace_create",
        serde_json::json!({"name": "warmtest-recall"}),
    )
    .await
    .expect("palace_create");

    let result = dispatch_tool(
        &state,
        "memory_recall",
        serde_json::json!({
            "palace": "warmtest-recall",
            "query": "test query"
        }),
    )
    .await
    .expect("memory_recall must not error while Warming (issue #1970)");
    assert!(result["results"].is_array());
}

/// Why (issue #1970): `memory_recall_deep` mirrors `memory_recall`'s
/// warming-fallback posture.
/// Test: this test.
#[tokio::test]
async fn recall_deep_does_not_error_while_state_is_warming() {
    let (state, _tmp) = test_state_warming();
    let _ = dispatch_tool(
        &state,
        "palace_create",
        serde_json::json!({"name": "warmtest-recall-deep"}),
    )
    .await
    .expect("palace_create");

    let result = dispatch_tool(
        &state,
        "memory_recall_deep",
        serde_json::json!({
            "palace": "warmtest-recall-deep",
            "query": "test query"
        }),
    )
    .await
    .expect("memory_recall_deep must not error while Warming (issue #1970)");
    assert!(result["results"].is_array());
}

/// Why (issue #1970, was #914 Part A): `memory_recall_all` must not error
/// while `Warming` either — it fans the same BM25/L0/L1 fallback out across
/// every palace.
/// Test: this test (regression guard for the gap originally fixed in #914
/// Part A, now re-targeted at graceful degradation instead of a hard error).
#[tokio::test]
async fn recall_all_does_not_error_while_state_is_warming() {
    let (state, _tmp) = test_state_warming();

    let result = dispatch_tool(
        &state,
        "memory_recall_all",
        serde_json::json!({
            "q": "test query issued while warming up"
        }),
    )
    .await
    .expect("memory_recall_all must not error while Warming (issue #1970)");
    assert!(result["results"].is_array());
}

/// Why (issue #1970): `bm25_hits_to_recall_results` is the piece that makes
/// the warming-fallback recall path actually useful — without it, BM25
/// hits would be silently dropped whenever there's no vector lane to boost
/// (exactly the situation while the embedder is cold). This test exercises
/// it directly against a handle's in-memory drawer table, with no BM25
/// daemon or embedder involved.
/// What: adds two drawers to a fresh handle, fabricates `BM25Hit`s
/// referencing one real drawer id and one unknown id, calls
/// `bm25_hits_to_recall_results`, and asserts the known drawer is hydrated
/// with the BM25 score and `layer: 4` while the unknown hit is skipped.
/// Test: this test.
#[test]
fn bm25_hits_hydrate_from_handle_during_warmup() {
    use trusty_common::bm25_client::BM25Hit;
    use trusty_common::memory_core::palace::Drawer;
    use trusty_common::memory_core::store::kg::KnowledgeGraph;
    use trusty_common::memory_core::store::vector::UsearchStore;

    let dir = tempfile::tempdir().expect("tempdir");
    let vs = UsearchStore::new(dir.path().join("idx.usearch"), 384).expect("vector store");
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).expect("kg");
    let handle = trusty_common::memory_core::retrieval::PalaceHandle::new(
        PalaceId::new("bm25hydrate"),
        String::new(),
        vs,
        kg,
    );

    let drawer = Drawer::new(Uuid::new_v4(), "Rustc is a compiler for the Rust language");
    let known_id = drawer.id;
    handle.add_drawer(drawer);

    let hits = vec![
        BM25Hit {
            doc_id: known_id.to_string(),
            score: 4.2,
        },
        BM25Hit {
            doc_id: Uuid::new_v4().to_string(),
            score: 1.0,
        },
    ];

    let results = bm25_hits_to_recall_results(&handle, &hits);
    assert_eq!(
        results.len(),
        1,
        "unknown drawer id must be skipped, not fabricated"
    );
    assert_eq!(results[0].drawer.id, known_id);
    assert_eq!(results[0].score, 4.2);
    assert_eq!(results[0].layer, 4, "BM25-hydrated hits use layer 4");
}
