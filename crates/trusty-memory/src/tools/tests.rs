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
/// Why (issue #88): calls [`skip_palace_enforcement`] so that existing tests
/// that call `palace_create` with arbitrary names continue to work.
/// Set `TRUSTY_SKIP_PALACE_ENFORCEMENT=1` for this test process (#88, #4413).
///
/// Why: `handle_palace_create` rejects a palace name that does not match the
/// project slug derived from cwd unless this var is set, so every test that
/// creates a palace with an arbitrary name (`"ctx"`, `"disc"`, …) needs it.
/// Production processes never set it.
///
/// Why it exists as a named helper rather than a line inside [`test_state`]
/// (#4413): two tests here build their `AppState` INLINE — they need
/// `with_default_palace`, which [`test_state`] does not offer — and so never
/// ran [`test_state`]'s `set_var`. They passed anyway under `cargo test`, purely
/// because some *other* test in the same process had already set the var: a
/// hidden ordering dependency, and one that made them pass for the wrong reason
/// (in isolation they verify nothing, because they never get past
/// `palace_create`). Per-test process isolation removes the donor and exposes
/// it — `cargo nextest run -p trusty-memory` failed both, which is how #4413
/// was found. Making the requirement callable, and calling it from each test
/// that needs it, is what makes each test self-sufficient.
///
/// What: writes the var at most once per process via a `OnceLock`, so N callers
/// perform ONE write rather than N racing ones (the pre-existing `test_state`
/// body re-wrote it on every call). Idempotent and safe to call from any test.
/// Test: [`add_alias_round_trip_through_prompt_cache`] and
/// [`dispatch_discover_aliases_inserts_new_and_dedupes`] are the two callers
/// that #4413 was filed for; every `test_state`/`test_state_warming` user gets
/// it transitively.
fn skip_palace_enforcement() {
    static SET: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    // SAFETY: a single write of a constant value, performed at most once per
    // process by the OnceLock. Matches this crate's established test-env
    // convention; tests needing stricter serialisation use `env_test_lock()`.
    SET.get_or_init(|| unsafe {
        std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
    });
}

fn test_state() -> (AppState, tempfile::TempDir) {
    skip_palace_enforcement();
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
    skip_palace_enforcement();
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
        ("palace_reembed", true),
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
    // 34 original + 3 task tools (task_add, task_list, task_complete, issue
    // #1722) + 3 room tools (room_list, room_create, room_rename, ADR-0027 T6)
    // + 3 wing tools (wing_list, wing_create, wing_rename, ADR-0027 T9 / #4809)
    // + 1 repair tool (palace_reembed, #4906)
    assert_eq!(tools.len(), 44);
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
        "palace_reembed",
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
        // ADR-0027 T6 (#4805):
        "room_list",
        "room_create",
        "room_rename",
        // ADR-0027 T9 (#4809) — the wing surface ships with the wing entity:
        "wing_list",
        "wing_create",
        "wing_rename",
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

/// Why (#4906): the repair path has to be reachable from the daemon, because
/// the daemon holds the palace's writer lock — a CLI would only ever get a
/// read-only snapshot. This confirms the tool is wired end-to-end through
/// `dispatch_tool` and that a dry run reports rather than mutates.
/// What: creates a palace, calls `palace_reembed` with no arguments, and
/// asserts the response carries the coverage counts and defaults to a dry run.
/// Test: itself.
#[tokio::test]
async fn dispatch_palace_reembed_dry_run_reports_counts() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "reembed-test"}))
        .await
        .expect("palace_create");
    let out = dispatch_tool(&state, "palace_reembed", json!({"palace": "reembed-test"}))
        .await
        .expect("palace_reembed");
    assert_eq!(out["dry_run"], true, "must default to a dry run: {out}");
    assert_eq!(out["missing"], 0);
    assert_eq!(out["attempted"], 0);
    assert_eq!(out["repaired"], 0);
    assert!(out["drawer_count"].is_number());
    assert!(out["vector_count"].is_number());
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
    // #4413: this test builds `AppState` inline (it needs `with_default_palace`,
    // which `test_state()` does not offer), so it must set the palace-enforcement
    // bypass ITSELF rather than inherit one another test happened to leak — see
    // `skip_palace_enforcement`. Without this the `palace_create` below fails
    // whenever no sibling test ran first (e.g. under `cargo nextest run`).
    skip_palace_enforcement();
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
    // #4413: this test builds `AppState` inline (it needs `with_default_palace`,
    // which `test_state()` does not offer), so it must set the palace-enforcement
    // bypass ITSELF rather than inherit one another test happened to leak — see
    // `skip_palace_enforcement`. Without this the `palace_create` below fails
    // whenever no sibling test ran first (e.g. under `cargo nextest run`).
    skip_palace_enforcement();
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
    assert_eq!(content_gate("yes", None, false), None);
    assert_eq!(content_gate("ok", None, false), None);
    assert_eq!(
        content_gate("  no thanks  ", None, false),
        None,
        "2 words still < 4"
    );
    assert_eq!(
        content_gate("one two three", None, false),
        None,
        "3 words still < 4"
    );
}

/// Why (issue #2442): `force = true` is an explicit operator override and
/// must bypass the short-content rejection the same way it bypasses the
/// blocklist and dedup gates — app-managed writers (e.g. tcode's turn
/// recorder, #2424) need deterministic storage even for short content.
/// What: passes single-word content with `force = true` and no context;
/// asserts the content passes through unchanged instead of being rejected.
/// Test: itself.
#[test]
fn content_gate_force_bypasses_short_content() {
    assert_eq!(
        content_gate("yes", None, true),
        Some("yes".to_string()),
        "force=true must bypass the short-content gate"
    );
    assert_eq!(content_gate("ok", None, true), Some("ok".to_string()));
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
        false,
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
        false,
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
    let kept = content_gate(body, None, false).expect(">= 4 words passes");
    assert_eq!(kept, body, "passing content must round-trip verbatim");
    // Exactly four words is the boundary — it must pass.
    let boundary = "one two three four";
    assert_eq!(
        content_gate(boundary, None, false).as_deref(),
        Some(boundary)
    );
}

/// Why: an empty or whitespace-only `context` argument must be treated
/// the same as `None` so callers can't accidentally smuggle short
/// content through by passing `""`.
/// What: passes blank context with short content and asserts the gate
/// still skips the write.
/// Test: itself.
#[test]
fn content_gate_blank_context_treated_as_none() {
    assert_eq!(content_gate("yes", Some(""), false), None);
    assert_eq!(content_gate("yes", Some("   "), false), None);
    assert_eq!(content_gate("yes", Some("\n\t"), false), None);
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
    // Issue #2442: the gate is now anchored to the START of the content
    // (`starts_with`, not `contains`) — a coding agent's turn text that
    // merely QUOTES an auto-capture phrase mid-prose (recapping tool
    // output it just ran) is legitimate content and must NOT be dropped.
    // This was the "sharper problem" the issue reported: the old
    // substring-anywhere match silently thinned the recall surface for
    // exactly this realistic case.
    assert!(blocklist_gate("I used Tool use: Bash here").is_none());
    assert!(
        blocklist_gate("The transcript quoted \"Claude Code session\" lifecycle events twice")
            .is_none()
    );
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

/// Why (issue #2442): `force = true` is documented as bypassing ALL
/// content-quality gates, including the blocklist. Before this fix,
/// `force` was parsed AFTER `blocklist_gate` ran, so a `force = true`
/// write of blocklisted content was still silently skipped.
/// What: dispatch a `"Tool use: Bash"` payload with `force: true` and
/// assert the write is `stored` (not `skipped`), and the drawer lands.
/// Test: itself.
#[tokio::test]
async fn dispatch_remember_force_bypasses_blocklist_gate() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "blk-force"}))
        .await
        .expect("palace_create");

    let res = dispatch_tool(
        &state,
        "memory_remember",
        json!({"palace": "blk-force", "text": "Tool use: Bash", "force": true}),
    )
    .await
    .expect("memory_remember (forced)");
    assert_eq!(
        res["status"], "stored",
        "force=true must bypass the blocklist gate; got {res:?}"
    );

    let listed = dispatch_tool(
        &state,
        "memory_list",
        json!({"palace": "blk-force", "limit": 10}),
    )
    .await
    .expect("memory_list");
    let drawers = listed["drawers"].as_array().expect("drawers array");
    assert_eq!(drawers.len(), 1, "the forced write must land");
}

/// Why (issue #2442): `force = true` must also bypass the short-content
/// (issue #215) gate — before this fix `force` was parsed after that gate
/// ran too.
/// What: dispatch a single-word payload with `force: true` and no
/// `context`; assert the write is `stored`.
/// Test: itself.
#[tokio::test]
async fn dispatch_remember_force_bypasses_short_content_gate() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "short-force"}))
        .await
        .expect("palace_create");

    let res = dispatch_tool(
        &state,
        "memory_remember",
        json!({"palace": "short-force", "text": "yes", "force": true}),
    )
    .await
    .expect("memory_remember (forced short content)");
    assert_eq!(
        res["status"], "stored",
        "force=true must bypass the short-content gate; got {res:?}"
    );
}

/// Why (issue #2520, two-tier `force` MAJOR fix): `force = true` bypasses
/// the QUALITY gates (blocklist, short-content, noise) but must NEVER
/// silently bypass secret detection — an automated writer that always sets
/// `force: true` (e.g. trusty-code's per-turn memory sink) would otherwise
/// persist raw credentials with zero screening.
/// What: dispatch a `force: true` write of secret-shaped content over the
/// MCP surface and assert the call still errors naming the secret; no
/// drawer lands.
/// Test: itself.
#[tokio::test]
async fn dispatch_remember_force_still_blocks_secret() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "sec-force"}))
        .await
        .expect("palace_create");

    let err = dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": "sec-force",
            "text": "deploy uses token AbCd1234EfGh5678IjKl9012 for the prod webhook auth", // pragma: allowlist secret
            "force": true,
        }),
    )
    .await
    .expect_err("force=true must still reject secret-shaped content");
    let msg = format!("{err:#}");
    assert!(
        msg.to_lowercase().contains("secret"),
        "expected a secret-gate rejection even under force; got: {msg}"
    );

    let listed = dispatch_tool(
        &state,
        "memory_list",
        json!({"palace": "sec-force", "limit": 10}),
    )
    .await
    .expect("memory_list");
    let drawers = listed["drawers"].as_array().expect("drawers array");
    assert!(
        drawers.is_empty(),
        "no drawer should be written for a secret, even under force"
    );
}

/// Why (issue #2520, two-tier `force` MAJOR fix): the separate
/// `allow_secret_like` MCP arg is the only way to bypass the secret gate —
/// asserts it works end-to-end through the dispatch surface (arg parsing in
/// `handle_memory_remember` through to `RememberOptions::allow_secret_like`).
/// What: dispatch a write with both `force: true` and `allow_secret_like:
/// true` set and assert the secret-shaped content is `stored`.
/// Test: itself.
#[tokio::test]
async fn dispatch_remember_allow_secret_like_bypasses_secret_gate() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "sec-allow"}))
        .await
        .expect("palace_create");

    let res = dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": "sec-allow",
            "text": "deploy uses token AbCd1234EfGh5678IjKl9012 for the prod webhook auth", // pragma: allowlist secret
            "force": true,
            "allow_secret_like": true,
        }),
    )
    .await
    .expect("force + allow_secret_like must bypass the secret gate too");
    assert_eq!(
        res["status"], "stored",
        "allow_secret_like=true must let secret-shaped content through; got {res:?}"
    );
}

/// Why (issue #2442, live false positives): two real-world memories were
/// rejected by the secret heuristic during trusty-mpm orchestration despite
/// containing no actual credential — a Rust source-location reference
/// (`client/http_client/error.rs::response_or_body_error`) and a compact
/// issue/PR/SHA ledger reference (`#2486→PR#2491(e993c18a)`). Both had to be
/// reworded to store. This test drives the exact strings end-to-end through
/// the MCP dispatch surface (no `force` needed) to lock in the fix.
/// What: `memory_remember` prose containing each token and assert `stored`.
/// Test: itself.
#[tokio::test]
async fn dispatch_remember_accepts_live_false_positive_tokens() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "fp2442"}))
        .await
        .expect("palace_create");

    let res = dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": "fp2442",
            "text": "Fixed the retry loop in client/http_client/error.rs::response_or_body_error \
                      so transient errors no longer abort the batch",
        }),
    )
    .await
    .expect("memory_remember (path::fn reference)");
    assert_eq!(
        res["status"], "stored",
        "Rust path::fn reference must be stored, not rejected as a secret; got {res:?}"
    );

    let res = dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": "fp2442",
            "text": "Milestone: shipped #2486→PR#2491(e993c18a) today, closing the retry-loop regression report finally",
        }),
    )
    .await
    .expect("memory_remember (ledger reference)");
    assert_eq!(
        res["status"], "stored",
        "issue/PR/SHA ledger reference must be stored, not rejected as a secret; got {res:?}"
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

    // #5048 review: dropping is only defensible if something repairs the drop.
    // Removing the `mark_dirty` call from `bm25_index_enqueue`'s `Full` arm
    // leaves this list empty and the coverage gap unrepaired until restart.
    assert_eq!(
        crate::bm25_repair::dirty_palaces(&state),
        vec!["default".to_string()],
        "a dropped index op must queue its palace for coverage repair"
    );
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

// Issue #1970's degraded-recall tests for memory_recall / memory_recall_deep /
// memory_recall_all moved to tests/recall_degraded_lane.rs (#4836): they gate on
// a process-wide embedder cell that `dispatch_remember_then_recall` initialises
// in this binary, so here they were order-dependent no-ops.

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

/// Why (MEDIUM 1, DOC-53 §3.1): a hand-written claim drawer already carries
/// `ws:<name>` in its own caller-supplied tags by convention — dispatched
/// through `attach_mcp_attribution` with a matching `args["workstream"]`
/// (the same value a real claim-drawer write would supply), the auto-stamp
/// must not duplicate that tag.
/// What: seeds `tags` with a `ws-claim`-shaped tag list (including
/// `ws:feat-x` already present, exactly as DOC-53 §3.1 instructs a PM to
/// write), calls `attach_mcp_attribution` with `args = {"workstream":
/// "feat-x"}`, and asserts `ws:feat-x` appears exactly once while the
/// non-overlapping `creator:*` tags still land.
/// Test: itself.
#[test]
fn attach_mcp_attribution_dedupes_hand_written_ws_claim_tag() {
    let mut tags = vec![
        "ws-claim".to_string(),
        "ws:feat-x".to_string(),
        "area:health-endpoint".to_string(),
    ];
    let args = json!({"workstream": "feat-x"});
    helpers::attach_mcp_attribution(&mut tags, &args);
    assert_eq!(
        tags.iter().filter(|t| *t == "ws:feat-x").count(),
        1,
        "ws:feat-x must not be duplicated by the auto-stamp; got {tags:?}"
    );
    assert!(
        tags.contains(&"creator:workstream=feat-x".to_string()),
        "creator:workstream= must still be stamped; got {tags:?}"
    );
    assert!(
        tags.contains(&"creator:client=trusty-memory-mcp".to_string()),
        "non-overlapping creator tags must still be appended; got {tags:?}"
    );
}

/// Why (ADR-0027 T3 / D4.1): the MCP surface used to carry its own
/// exact-case, alias-free room parser while HTTP used `RoomType::parse`, so
/// `room="backend"` named `Custom("backend")` over MCP and `Backend` over
/// HTTP — two rooms, two ids, each invisible to the other's filter. There is
/// now one parser, and this test is what stops the fork coming back.
/// What: writes over MCP with the lowercase spelling, lists with the
/// canonical spelling, and asserts the drawer comes back. Also pins the
/// documented alias behaviour the old MCP parser lacked (`docs` ->
/// `Documentation`).
/// Test: itself.
#[tokio::test]
async fn mcp_room_parse_matches_http() {
    use trusty_common::memory_core::palace::RoomType;

    // The parser contract itself, shared by both transports.
    assert_eq!(RoomType::parse("backend"), RoomType::Backend);
    assert_eq!(RoomType::parse("Backend"), RoomType::Backend);
    assert_eq!(RoomType::parse("docs"), RoomType::Documentation);

    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "roomparity"}))
        .await
        .expect("palace_create");

    let _ = dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": "roomparity",
            "text": "The scheduler retries a failed job with exponential backoff",
            "room": "backend",
            "tags": ["scheduler"],
        }),
    )
    .await
    .expect("memory_remember");

    for spelling in ["Backend", "backend"] {
        let listed = dispatch_tool(
            &state,
            "memory_list",
            json!({"palace": "roomparity", "room": spelling, "limit": 10}),
        )
        .await
        .expect("memory_list");
        let drawers = listed["drawers"].as_array().expect("drawers");
        assert!(
            drawers
                .iter()
                .any(|d| d["content"].as_str().unwrap_or("").contains("scheduler")),
            "room={spelling} must find the drawer written as room=backend; got {drawers:?}"
        );
    }
}

// ── ADR-0027 room surface (T5 #4804, T6 #4805, T7 #4806) ─────────────────

/// Create a palace with one drawer per named room, and return its id.
///
/// Uses `memory_remember` (the real write path) so the drawers carry whatever
/// `room_id` production would stamp on them — the point of every assertion
/// below is that the room surface agrees with the write path. `force` is set
/// because these are app-managed fixture writes that must land deterministically
/// regardless of the length and dedup heuristics; it bypasses content-QUALITY
/// gates only, exactly the case issue #2520 carved it out for.
async fn palace_with_rooms(state: &AppState, palace: &str, rooms: &[(&str, &str)]) {
    let _ = dispatch_tool(state, "palace_create", json!({"name": palace}))
        .await
        .expect("palace_create");
    for (room, text) in rooms {
        let res = dispatch_tool(
            state,
            "memory_remember",
            json!({"palace": palace, "text": text, "room": room, "force": true}),
        )
        .await
        .expect("memory_remember");
        assert_eq!(res["status"], "stored", "fixture write was gated: {res}");
    }
}

/// Why (ADR-0027 T5 / #4804): `memory_note` was hard-pinned to `General`, so a
/// curated fact could not be filed anywhere else. The pin is lifted.
/// What: a note written with `room` lands in that room; one written without it
/// still lands in `General`.
#[tokio::test]
async fn dispatch_note_accepts_an_explicit_room() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "noteroom"}))
        .await
        .expect("palace_create");

    for (room, content) in [
        (
            Some("decisions"),
            "Deploy target is prod-east for this service",
        ),
        (None, "User prefers snake_case in generated identifiers"),
    ] {
        let mut args = json!({"palace": "noteroom", "content": content});
        if let Some(r) = room {
            args["room"] = json!(r);
        }
        let res = dispatch_tool(&state, "memory_note", args)
            .await
            .expect("memory_note");
        assert_eq!(res["status"], "stored", "{res}");
    }

    let scoped = dispatch_tool(
        &state,
        "memory_list",
        json!({"palace": "noteroom", "room": "decisions", "limit": 10}),
    )
    .await
    .expect("memory_list");
    let rows = scoped["drawers"].as_array().expect("drawers");
    assert_eq!(rows.len(), 1, "exactly the note filed into `decisions`");
    assert!(rows[0]["content"].as_str().unwrap().contains("prod-east"));

    // The default is unchanged for a caller that passes nothing.
    let general = dispatch_tool(
        &state,
        "memory_list",
        json!({"palace": "noteroom", "room": "General", "limit": 10}),
    )
    .await
    .expect("memory_list");
    let rows = general["drawers"].as_array().expect("drawers");
    assert_eq!(rows.len(), 1);
    assert!(rows[0]["content"].as_str().unwrap().contains("snake_case"));
}

/// Why (ADR-0027 T6 / #4805): `room_list` is the discovery primitive that did
/// not exist — a caller could not find out a palace had a `decisions` room
/// without already knowing the word.
#[tokio::test]
async fn dispatch_room_list_reports_rooms_with_drawer_counts() {
    let (state, _tmp) = test_state();
    palace_with_rooms(
        &state,
        "roomlist",
        &[
            (
                "decisions",
                "We chose redb over a JSON sidecar for room storage",
            ),
            (
                "decisions",
                "Room ids are read from the table, never recomputed",
            ),
            (
                "Planning",
                "The room surface ships before the wing entity does",
            ),
        ],
    )
    .await;

    let res = dispatch_tool(&state, "room_list", json!({"palace": "roomlist"}))
        .await
        .expect("room_list");
    let rooms = res["rooms"].as_array().expect("rooms array");
    let by_label: std::collections::HashMap<&str, &serde_json::Value> = rooms
        .iter()
        .map(|r| (r["label"].as_str().unwrap(), r))
        .collect();

    assert_eq!(by_label["decisions"]["drawer_count"], 2);
    assert_eq!(by_label["decisions"]["room_type"], "Custom");
    assert_eq!(by_label["Planning"]["drawer_count"], 1);
    assert_eq!(by_label["Planning"]["room_type"], "Planning");
    for room in rooms {
        assert_eq!(room["resolved"], true, "a live write is never unresolved");
        assert!(room["room_id"].as_str().is_some());
        assert!(room["wing_id"].as_str().is_some());
    }
}

/// Why: wings are gated on ADR-0027 T9. Accepting a wing we cannot honour —
/// by ignoring it, or by returning an empty list — would be an invisible
/// failure, the exact class ADR-0027 exists to remove.
#[tokio::test]
async fn dispatch_room_list_rejects_an_unknown_wing() {
    let (state, _tmp) = test_state();
    palace_with_rooms(&state, "wingcheck", &[("Planning", "A drawer in planning")]).await;

    let err = dispatch_tool(
        &state,
        "room_list",
        json!({"palace": "wingcheck", "wing": Uuid::from_u128(7).to_string()}),
    )
    .await
    .expect_err("an unknown wing must be rejected");
    assert!(
        format!("{err:#}").contains("not implemented yet"),
        "{err:#}"
    );

    // The palace's own default wing is accepted.
    let ok = dispatch_tool(
        &state,
        "room_list",
        json!({
            "palace": "wingcheck",
            "wing": trusty_common::memory_core::room_identity::DEFAULT_WING_ID.to_string(),
        }),
    )
    .await
    .expect("default wing accepted");
    assert!(!ok["rooms"].as_array().unwrap().is_empty());
}

/// Why: `room_create` is documented idempotent, including across case — two
/// spellings of one intent must not become two rooms (ADR-0027 C3.2's defect).
#[tokio::test]
async fn dispatch_room_create_is_idempotent() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "roomcreate"}))
        .await
        .expect("palace_create");

    let first = dispatch_tool(
        &state,
        "room_create",
        json!({"palace": "roomcreate", "label": "Decisions", "description": "why we chose things"}),
    )
    .await
    .expect("room_create");
    assert_eq!(first["created"], true);
    assert_eq!(
        first["label"], "Decisions",
        "the caller's spelling survives"
    );

    let second = dispatch_tool(
        &state,
        "room_create",
        json!({"palace": "roomcreate", "label": "decisions"}),
    )
    .await
    .expect("room_create again");
    assert_eq!(second["created"], false);
    assert_eq!(second["room_id"], first["room_id"]);

    let listed = dispatch_tool(&state, "room_list", json!({"palace": "roomcreate"}))
        .await
        .expect("room_list");
    let rooms = listed["rooms"].as_array().expect("rooms");
    assert_eq!(rooms.len(), 1, "one room, not two: {rooms:?}");
    assert_eq!(
        rooms[0]["drawer_count"], 0,
        "creating a room moves no drawer"
    );
    assert_eq!(rooms[0]["description"], "why we chose things");
}

/// Why (ADR-0027 D6): `room_rename` is the repair path, and its defining
/// property is that it renames a room without touching a drawer. Proven here
/// by reading every drawer back and checking the set is identical.
#[tokio::test]
async fn dispatch_room_rename_leaves_drawers_in_place() {
    let (state, _tmp) = test_state();
    palace_with_rooms(
        &state,
        "roomrename",
        &[
            ("checkpoint", "Session checkpoint after the registry landed"),
            ("General", "Unrelated general memory that must not move"),
        ],
    )
    .await;

    let before = dispatch_tool(
        &state,
        "memory_list",
        json!({"palace": "roomrename", "limit": 100}),
    )
    .await
    .expect("memory_list before");

    let renamed = dispatch_tool(
        &state,
        "room_rename",
        json!({"palace": "roomrename", "room": "checkpoint", "new_label": "Session Checkpoints"}),
    )
    .await
    .expect("room_rename");
    assert_eq!(renamed["label"], "Session Checkpoints");

    let after = dispatch_tool(
        &state,
        "memory_list",
        json!({"palace": "roomrename", "limit": 100}),
    )
    .await
    .expect("memory_list after");
    assert_eq!(before, after, "a rename must not change any drawer");

    // The drawer is reachable under the NEW name and not the old one.
    let by_new = dispatch_tool(
        &state,
        "memory_list",
        json!({"palace": "roomrename", "room": "Session Checkpoints", "limit": 10}),
    )
    .await
    .expect("memory_list new name");
    assert_eq!(by_new["drawers"].as_array().unwrap().len(), 1);

    let listed = dispatch_tool(&state, "room_list", json!({"palace": "roomrename"}))
        .await
        .expect("room_list");
    let labels: Vec<&str> = listed["rooms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["label"].as_str().unwrap())
        .collect();
    assert!(labels.contains(&"Session Checkpoints"), "{labels:?}");
    assert!(!labels.contains(&"checkpoint"), "{labels:?}");
}

/// Why: merging two rooms is ADR-0027 D5 and deliberately deferred; folding
/// one into the other silently would change which drawers a filter returns.
#[tokio::test]
async fn dispatch_room_rename_rejects_a_taken_name() {
    let (state, _tmp) = test_state();
    palace_with_rooms(
        &state,
        "roomclash",
        &[
            ("alpha", "A memory that belongs to the alpha room here"),
            ("beta", "A memory that belongs to the beta room instead"),
        ],
    )
    .await;

    let err = dispatch_tool(
        &state,
        "room_rename",
        json!({"palace": "roomclash", "room": "alpha", "new_label": "beta"}),
    )
    .await
    .expect_err("must refuse to merge");
    assert!(
        format!("{err:#}").contains("already belongs to another room"),
        "{err:#}"
    );
}

/// Why (ADR-0027 T7 / #4806): room-scoped recall was reachable only through
/// `memory_list` and the HTTP route — the MCP recall schema had no `room`.
/// What: both recall tools, scoped to one room, must not return the other
/// room's drawer at the search layer (L0/L1 are always-on grounding and stay).
#[tokio::test]
async fn dispatch_recall_room_filter_scopes_results() {
    let (state, _tmp) = test_state();
    palace_with_rooms(
        &state,
        "recallroom",
        &[
            (
                "Backend",
                "Rust is a systems programming language with ownership",
            ),
            (
                "Frontend",
                "Rust is a systems programming toolkit for the browser",
            ),
        ],
    )
    .await;

    for tool in ["memory_recall", "memory_recall_deep"] {
        let res = dispatch_tool(
            &state,
            tool,
            json!({
                "palace": "recallroom",
                "query": "systems programming Rust",
                "room": "Backend",
                "top_k": 10,
            }),
        )
        .await
        .unwrap_or_else(|e| panic!("{tool}: {e:#}"));
        let leaked = res["results"]
            .as_array()
            .expect("results array")
            .iter()
            .any(|r| {
                r["layer"].as_u64().unwrap_or(0) >= 2
                    && r["content"].as_str().unwrap_or("").contains("browser")
            });
        assert!(
            !leaked,
            "{tool} returned a Frontend drawer under a Backend room filter: {res}"
        );
    }
}

/// Why (#4807): `palace_info` never reported how many rooms a palace has.
#[tokio::test]
async fn dispatch_palace_info_reports_room_count() {
    let (state, _tmp) = test_state();
    palace_with_rooms(
        &state,
        "infocount",
        &[
            (
                "Planning",
                "The room registry lands before the wing entity does",
            ),
            ("General", "A second memory that sits in the default room"),
        ],
    )
    .await;

    let info = dispatch_tool(&state, "palace_info", json!({"palace": "infocount"}))
        .await
        .expect("palace_info");
    assert_eq!(info["room_count"], 2, "{info}");
    assert_eq!(info["drawer_count"], 2);
}

// ── ADR-0028 Tier C MCP surface (#4886) ─────────────────────────────────────

/// A well-formed slot is admitted and reported as Tier C, and a second write to
/// the same slot retires the first — the "one slot, one live fact" contract
/// (ADR-0028 D5) as an MCP client sees it.
#[tokio::test]
async fn dispatch_remember_admits_a_tier_c_slot() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "tierc"}))
        .await
        .expect("palace_create");

    let first = dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": "tierc",
            "text": "PR 4818 is in flight at head d39638482bfe8de462c02c4f40e02b56b16897ff",
            "fact_key": "pr:4818/state",
        }),
    )
    .await
    .expect("first tier C write");
    assert_eq!(first["tier"], "C", "{first}");
    assert!(first.get("tier_c_refused").is_none(), "{first}");

    let second = dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": "tierc",
            "text": "PR 4818 merged as squash 4c412ae1 at head 59ae50d8 on main",
            "fact_key": "pr:4818/state",
            "force": true,
        }),
    )
    .await
    .expect("second tier C write");
    assert_eq!(second["tier"], "C", "{second}");

    let handle = open_palace_handle(&state, "tierc").expect("open");
    let winner: uuid::Uuid = second["drawer_id"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        handle.kg.drawer_id_for_fact_key("pr:4818/state").unwrap(),
        Some(winner),
        "the newer write must hold the slot"
    );
    assert_eq!(
        handle.kg.load_drawers().unwrap().len(),
        2,
        "the superseded fact is demoted, never deleted (D6)"
    );
}

/// Fail-closed admission is OBSERVABLE: the write still succeeds, but as an
/// ordinary Tier E drawer, and the envelope names the reason.
#[tokio::test]
async fn dispatch_remember_reports_a_refused_slot_as_tier_e() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "refused"}))
        .await
        .expect("palace_create");

    let out = dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": "refused",
            "text": "A bare unnamespaced slot name would collide across every workstream",
            "fact_key": "state",
        }),
    )
    .await
    .expect("the write degrades, it does not fail");
    assert_eq!(out["tier"], "E", "{out}");
    assert!(
        out["tier_c_refused"]
            .as_str()
            .is_some_and(|s| s.contains("state")),
        "{out}"
    );

    let handle = open_palace_handle(&state, "refused").expect("open");
    assert_eq!(handle.kg.drawer_id_for_fact_key("state").unwrap(), None);
    assert!(
        handle
            .kg
            .load_drawers()
            .unwrap()
            .iter()
            .all(|d| d.fact_key.is_none() && d.expires_at.is_none()),
        "a refused write must not pick up a slot or the Tier C default TTL"
    );
}

/// `memory_note` carries the same slot surface — it is the `importance = 1.0`
/// path, so a stale note there is the precise failure ADR-0028 exists to stop.
#[tokio::test]
async fn dispatch_note_admits_a_tier_c_slot() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "tiercnote"}))
        .await
        .expect("palace_create");

    let out = dispatch_tool(
        &state,
        "memory_note",
        json!({
            "palace": "tiercnote",
            "content": "origin/main is at 2b83d19e right now",
            "fact_key": "repo:trusty-tools/main-head",
        }),
    )
    .await
    .expect("memory_note");
    assert_eq!(out["tier"], "C", "{out}");

    let handle = open_palace_handle(&state, "tiercnote").expect("open");
    let id: uuid::Uuid = out["drawer_id"].as_str().unwrap().parse().unwrap();
    let stored = handle
        .kg
        .load_drawers()
        .unwrap()
        .into_iter()
        .find(|d| d.id == id)
        .expect("stored");
    assert_eq!(
        stored.fact_key.as_deref(),
        Some("repo:trusty-tools/main-head")
    );
    assert!(
        stored.expires_at.is_some(),
        "an admitted Tier C fact always carries a retirement condition (D4)"
    );
}

/// A malformed `expires_at` STRING is a caller typo, not a degradation —
/// silently substituting the 24-hour default would hide it.
#[tokio::test]
async fn dispatch_remember_rejects_an_unparseable_expires_at() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "badttl"}))
        .await
        .expect("palace_create");

    let err = dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": "badttl",
            "text": "A memory whose retirement timestamp cannot be parsed at all",
            "fact_key": "pr:1/state",
            "expires_at": "tomorrow",
        }),
    )
    .await
    .expect_err("an unparseable timestamp must be an error");
    assert!(format!("{err:#}").contains("RFC 3339"), "{err:#}");
}

/// The schema advertises the two new arguments, on both write tools.
#[test]
fn tool_definitions_expose_the_tier_c_arguments() {
    let defs = super::definitions::tool_definitions();
    for tool in ["memory_remember", "memory_note"] {
        let props = defs["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == tool)
            .unwrap_or_else(|| panic!("{tool} missing"))["inputSchema"]["properties"]
            .clone();
        assert!(props.get("fact_key").is_some(), "{tool} lacks fact_key");
        assert!(props.get("expires_at").is_some(), "{tool} lacks expires_at");
    }
}

// ---------------------------------------------------------------------------
// #4888 — Tier S admission control (ADR-0028 D2 / D8)
//
// Every test here drives the real MCP handler through `dispatch_tool`. The
// gate is only worth anything at the surface a caller actually reaches, so
// none of these call `check_tier_s_admission` directly.
// ---------------------------------------------------------------------------

/// Assert `count` distinct hot-predicate facts into `palace`, expecting each
/// to be admitted. Panics with context on the first refusal.
async fn fill_tier_s(state: &AppState, palace: &str, count: usize) {
    for i in 0..count {
        dispatch_tool(
            state,
            "kg_assert",
            json!({
                "palace": palace,
                "subject": format!("rule-{i}"),
                "predicate": "has_convention",
                "object": format!("standing rule number {i}"),
            }),
        )
        .await
        .unwrap_or_else(|e| panic!("fact {i} should be admitted below the cap: {e:#}"));
    }
}

/// Why (#4888): the cap is 20, so exactly 20 facts must be admitted. A gate
/// that refused the 20th would be off-by-one in the direction that silently
/// costs the user a slot.
#[tokio::test]
async fn dispatch_kg_assert_accepts_twenty_facts() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "cap"}))
        .await
        .expect("palace_create");

    fill_tier_s(&state, "cap", crate::prompt_facts::TIER_S_MAX_FACTS).await;

    let listed = dispatch_tool(&state, "list_prompt_facts", json!({}))
        .await
        .expect("list_prompt_facts");
    assert_eq!(
        listed["facts"].as_array().expect("facts array").len(),
        crate::prompt_facts::TIER_S_MAX_FACTS,
        "all 20 facts should be active on the surface",
    );
}

/// Why (#4888): the 21st write must FAIL, not be silently dropped or
/// truncated at read time. Fail-closed means the fact is absent from storage
/// afterwards, not merely absent from the rendered block.
#[tokio::test]
async fn dispatch_kg_assert_rejects_twenty_first_fact() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "cap"}))
        .await
        .expect("palace_create");
    fill_tier_s(&state, "cap", crate::prompt_facts::TIER_S_MAX_FACTS).await;

    let err = dispatch_tool(
        &state,
        "kg_assert",
        json!({
            "palace": "cap",
            "subject": "one-too-many",
            "predicate": "has_convention",
            "object": "this rule arrives when the surface is already full",
        }),
    )
    .await
    .expect_err("the 21st fact must be rejected");

    let msg = format!("{err:#}");
    assert!(msg.contains("Tier S is full"), "{msg}");

    // Fail-closed: the write did not happen.
    let listed = dispatch_tool(&state, "list_prompt_facts", json!({}))
        .await
        .expect("list_prompt_facts");
    let facts = listed["facts"].as_array().expect("facts array");
    assert_eq!(facts.len(), crate::prompt_facts::TIER_S_MAX_FACTS);
    assert!(
        !facts.iter().any(|f| f["subject"] == "one-too-many"),
        "rejected fact must not be in storage: {listed}",
    );
}

/// Why (#4888): "cap exceeded" alone is a dead end — the caller cannot act on
/// it. The rejection must name the facts occupying the surface and name the
/// tool that retires one, or the actionability requirement is unmet.
#[tokio::test]
async fn dispatch_kg_assert_rejection_names_existing_facts() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "cap"}))
        .await
        .expect("palace_create");
    fill_tier_s(&state, "cap", crate::prompt_facts::TIER_S_MAX_FACTS).await;

    let err = dispatch_tool(
        &state,
        "kg_assert",
        json!({
            "palace": "cap",
            "subject": "blocked",
            "predicate": "is_fact",
            "object": "rejected",
        }),
    )
    .await
    .expect_err("must be rejected at the cap");
    let msg = format!("{err:#}");

    // The retirement path is named explicitly.
    assert!(msg.contains("remove_prompt_fact"), "{msg}");
    // Every occupying fact is named, so the caller can choose one to retire.
    for i in 0..crate::prompt_facts::TIER_S_MAX_FACTS {
        assert!(
            msg.contains(&format!("rule-{i} has_convention")),
            "rejection must name existing fact rule-{i}: {msg}",
        );
        assert!(
            msg.contains(&format!("standing rule number {i}")),
            "rejection must show the object of rule-{i}: {msg}",
        );
    }
}

/// Why (#4888): an author who filled the surface must still be able to
/// correct an existing rule. Re-asserting an active `(subject, predicate)`
/// supersedes rather than adds, so occupancy is unchanged and the write is
/// admitted even at the cap.
#[tokio::test]
async fn dispatch_kg_assert_allows_replacing_existing_fact_at_cap() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "cap"}))
        .await
        .expect("palace_create");
    fill_tier_s(&state, "cap", crate::prompt_facts::TIER_S_MAX_FACTS).await;

    dispatch_tool(
        &state,
        "kg_assert",
        json!({
            "palace": "cap",
            "subject": "rule-3",
            "predicate": "has_convention",
            "object": "corrected wording for rule three",
        }),
    )
    .await
    .expect("replacing an existing fact at the cap must be admitted");

    let listed = dispatch_tool(&state, "list_prompt_facts", json!({}))
        .await
        .expect("list_prompt_facts");
    let facts = listed["facts"].as_array().expect("facts array");
    assert_eq!(
        facts.len(),
        crate::prompt_facts::TIER_S_MAX_FACTS,
        "a replacement must not grow the surface",
    );
    assert!(
        facts
            .iter()
            .any(|f| f["object"] == "corrected wording for rule three"),
        "replacement object should be live: {listed}",
    );
}

/// Why (#4888): the cap is on ACTIVE facts. Retraction closes the interval
/// (`valid_to` set) but leaves the row in storage, so a counting bug that
/// walked raw rows instead of active ones would wedge the surface at 20
/// forever with no way to recover.
#[tokio::test]
async fn dispatch_kg_assert_retracted_fact_frees_a_slot() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "cap"}))
        .await
        .expect("palace_create");
    fill_tier_s(&state, "cap", crate::prompt_facts::TIER_S_MAX_FACTS).await;

    // Confirm we really are wedged before retiring anything.
    dispatch_tool(
        &state,
        "kg_assert",
        json!({
            "palace": "cap",
            "subject": "successor",
            "predicate": "has_convention",
            "object": "waiting for a free slot",
        }),
    )
    .await
    .expect_err("cap must be enforced before retirement");

    let removed = dispatch_tool(
        &state,
        "remove_prompt_fact",
        json!({"subject": "rule-7", "predicate": "has_convention"}),
    )
    .await
    .expect("remove_prompt_fact");
    assert_eq!(removed["removed"], true, "{removed}");

    // The retracted row is still on disk but must not consume a slot.
    dispatch_tool(
        &state,
        "kg_assert",
        json!({
            "palace": "cap",
            "subject": "successor",
            "predicate": "has_convention",
            "object": "admitted into the slot the retraction freed",
        }),
    )
    .await
    .expect("a retracted fact must free its slot");

    let listed = dispatch_tool(&state, "list_prompt_facts", json!({}))
        .await
        .expect("list_prompt_facts");
    let facts = listed["facts"].as_array().expect("facts array");
    assert_eq!(facts.len(), crate::prompt_facts::TIER_S_MAX_FACTS);
    assert!(
        facts.iter().any(|f| f["subject"] == "successor"),
        "{listed}"
    );
    assert!(
        !facts.iter().any(|f| f["subject"] == "rule-7"),
        "retracted fact must not be active: {listed}",
    );
}

// ---------------------------------------------------------------------------
// #4890 — Tier S re-affirmation (ADR-0028 D8 point 4)
// ---------------------------------------------------------------------------

/// Why (#4890): this is the ticket's central semantic decision and the only
/// place it can be proven. `affirmed_at` is derived from the active row's
/// `valid_from` rather than stored, and the decision that re-asserting a rule
/// **verbatim** counts as re-affirmation depends entirely on `assert`
/// rewriting `valid_from` even when the object is byte-identical. Nothing in
/// this crate owns that behaviour — it is `KgStoreRedb::assert`'s — so a change
/// there (an "identical write is a no-op" optimisation, say) would silently
/// turn the doctor check into a nag that no amount of re-affirmation could
/// clear. This test is the tripwire for that.
/// What: asserts a fact, records its `affirmed_at`, sleeps past the storage
/// layer's millisecond resolution, re-asserts the SAME subject/predicate/object,
/// and asserts the surface still holds one fact whose `affirmed_at` moved
/// strictly forward.
#[tokio::test]
async fn reasserting_an_identical_fact_refreshes_affirmed_at() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "affirm"}))
        .await
        .expect("palace_create");

    let write = json!({
        "palace": "affirm",
        "subject": "conv-1",
        "predicate": "has_convention",
        "object": "Write plainly",
    });

    dispatch_tool(&state, "kg_assert", write.clone())
        .await
        .expect("first assert");
    let first = affirmed_at_of(&state, "conv-1").await;

    // Timestamps persist at millisecond resolution, so two asserts inside the
    // same millisecond would be indistinguishable and the comparison below
    // would be measuring the clock rather than the behaviour.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    dispatch_tool(&state, "kg_assert", write)
        .await
        .expect("re-asserting the identical fact must be admitted");
    let second = affirmed_at_of(&state, "conv-1").await;

    assert!(
        second > first,
        "re-asserting a verbatim-identical rule must refresh affirmed_at \
         (first={first}, second={second})",
    );

    let listed = dispatch_tool(&state, "list_prompt_facts", json!({}))
        .await
        .expect("list_prompt_facts");
    assert_eq!(
        listed["facts"].as_array().expect("facts array").len(),
        1,
        "a re-affirmation supersedes rather than adds: {listed}",
    );
}

/// Read the `affirmed_at` of the single Tier S fact with the given subject.
///
/// Parses rather than string-compares: `to_rfc3339` emits 0, 3, 6, or 9
/// fractional digits depending on the value, so two RFC 3339 strings do not
/// order lexicographically (a whole-second timestamp emits no fraction at all,
/// and `+` sorts before `.`).
async fn affirmed_at_of(state: &AppState, subject: &str) -> chrono::DateTime<chrono::Utc> {
    let listed = dispatch_tool(state, "list_prompt_facts", json!({}))
        .await
        .expect("list_prompt_facts");
    let facts = listed["facts"].as_array().expect("facts array").clone();
    let row = facts
        .iter()
        .find(|f| f["subject"] == subject)
        .unwrap_or_else(|| panic!("no Tier S fact for {subject}: {listed}"))
        .clone();
    let raw = row["affirmed_at"]
        .as_str()
        .unwrap_or_else(|| panic!("affirmed_at missing or not a string: {row}"));
    chrono::DateTime::parse_from_rfc3339(raw)
        .unwrap_or_else(|e| panic!("affirmed_at {raw:?} is not RFC 3339: {e}"))
        .with_timezone(&chrono::Utc)
}

/// Why (#4888): 80 characters is the boundary and must be inclusive — a rule
/// of exactly 80 chars is legal (ADR-0028 D2).
#[tokio::test]
async fn dispatch_kg_assert_accepts_object_at_char_limit() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "form"}))
        .await
        .expect("palace_create");

    let exactly_80 = "x".repeat(crate::prompt_facts::TIER_S_MAX_OBJECT_CHARS);
    assert_eq!(exactly_80.chars().count(), 80);

    dispatch_tool(
        &state,
        "kg_assert",
        json!({
            "palace": "form",
            "subject": "boundary",
            "predicate": "has_convention",
            "object": exactly_80,
        }),
    )
    .await
    .expect("an 80-character object is within the form constraint");
}

/// Why (#4888): an 81-character object must be rejected, and the error must
/// state the actual length and the limit so the author knows how much to cut.
#[tokio::test]
async fn dispatch_kg_assert_rejects_object_over_char_limit() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "form"}))
        .await
        .expect("palace_create");

    let too_long = "x".repeat(crate::prompt_facts::TIER_S_MAX_OBJECT_CHARS + 1);
    let err = dispatch_tool(
        &state,
        "kg_assert",
        json!({
            "palace": "form",
            "subject": "overlong",
            "predicate": "has_convention",
            "object": too_long,
        }),
    )
    .await
    .expect_err("an 81-character object must be rejected");

    let msg = format!("{err:#}");
    assert!(
        msg.contains("81 characters"),
        "actual length missing: {msg}"
    );
    assert!(msg.contains("limit is 80"), "limit missing: {msg}");

    // Fail-closed: nothing was written.
    let listed = dispatch_tool(&state, "list_prompt_facts", json!({}))
        .await
        .expect("list_prompt_facts");
    assert!(
        listed["facts"].as_array().expect("facts array").is_empty(),
        "rejected over-long fact must not be stored: {listed}",
    );
}

/// Why (#4888): `add_alias` writes `is_alias_for`, a hot predicate, so it
/// consumes a Tier S slot and must be gated identically to `kg_assert`.
/// Gating only the generic tool would leave a named bypass.
#[tokio::test]
async fn dispatch_add_alias_enforces_tier_s_cap() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "cap"}))
        .await
        .expect("palace_create");
    fill_tier_s(&state, "cap", crate::prompt_facts::TIER_S_MAX_FACTS).await;

    let err = dispatch_tool(
        &state,
        "add_alias",
        json!({"palace": "cap", "short": "tga", "full": "trusty-git-analytics"}),
    )
    .await
    .expect_err("add_alias must respect the Tier S cap");
    assert!(format!("{err:#}").contains("Tier S is full"), "{err:#}");
}

/// Why (#4888): `add_alias` must also enforce the form constraint — the
/// object it writes is composed (`full (extra)`), so the length that matters
/// is the composed one, not the raw `full` argument.
#[tokio::test]
async fn dispatch_add_alias_enforces_form_constraint_on_composed_object() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "form"}))
        .await
        .expect("palace_create");

    let err = dispatch_tool(
        &state,
        "add_alias",
        json!({
            "palace": "form",
            "short": "x",
            "full": "y".repeat(60),
            "extra": "z".repeat(60),
        }),
    )
    .await
    .expect_err("composed object over 80 chars must be rejected");
    assert!(format!("{err:#}").contains("limit is 80"), "{err:#}");
}

/// Why (#4888): `discover_aliases` is the only path that can add many hot
/// facts in one call, so it is where an ungated cap would leak worst. It must
/// stop exactly at the cap, report what it refused, and leave the refused
/// aliases unwritten — never overrun the budget, and never abort a bulk call
/// so late that earlier aliases are stranded as partial state.
#[tokio::test]
async fn dispatch_discover_aliases_stops_at_tier_s_cap() {
    skip_palace_enforcement();
    let _tmp = tempfile::tempdir().expect("tempdir");
    let root = _tmp.path().to_path_buf();
    let state = AppState::new(root).with_default_palace(Some("disccap".to_string()));
    dispatch_tool(&state, "palace_create", json!({"name": "disccap"}))
        .await
        .expect("palace_create");

    // Leave exactly one free slot, then discover against the live workspace,
    // which yields far more than one alias.
    fill_tier_s(&state, "disccap", crate::prompt_facts::TIER_S_MAX_FACTS - 1).await;

    let workspace_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf();

    let res = dispatch_tool(
        &state,
        "discover_aliases",
        json!({"project_root": workspace_root.to_string_lossy()}),
    )
    .await
    .expect("discover_aliases must not abort when the budget runs out");

    assert_eq!(
        res["new"].as_u64(),
        Some(1),
        "exactly the one free slot should be filled: {res}",
    );
    let rejected = res["rejected"].as_array().expect("rejected array");
    assert!(
        !rejected.is_empty(),
        "refused aliases must be reported, not silently dropped: {res}",
    );
    let reason = res["rejected_reason"].as_str().expect("rejected_reason");
    assert!(reason.contains("Tier S is full"), "{reason}");
    assert!(reason.contains("remove_prompt_fact"), "{reason}");

    // The cap held: the surface is at exactly 20, never above.
    let facts = crate::prompt_facts::gather_hot_triples(&state)
        .await
        .expect("gather");
    assert_eq!(
        facts.len(),
        crate::prompt_facts::TIER_S_MAX_FACTS,
        "auto-discovery must never push the surface past the cap",
    );
    // A refused alias is genuinely absent from storage.
    let refused = rejected[0]["short"].as_str().expect("short");
    assert!(
        !facts.iter().any(|(s, _, _)| s == refused),
        "refused alias {refused} must not be written: {facts:?}",
    );
}

/// Why (#4888): the cap governs the always-injected surface only. A cold
/// predicate never reaches that surface, so a full Tier S must not block
/// ordinary knowledge-graph writes — over-broad enforcement would break every
/// non-prompt KG user.
#[tokio::test]
async fn dispatch_kg_assert_cap_does_not_apply_to_cold_predicates() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "cap"}))
        .await
        .expect("palace_create");
    fill_tier_s(&state, "cap", crate::prompt_facts::TIER_S_MAX_FACTS).await;

    dispatch_tool(
        &state,
        "kg_assert",
        json!({
            "palace": "cap",
            "subject": "alice",
            "predicate": "works_at",
            "object": "a description far longer than eighty characters, which is fine \
                       because this predicate never reaches the always-injected surface",
        }),
    )
    .await
    .expect("cold predicates are unaffected by the Tier S budget");
}

/// Why (#4888): the cap is only a cap if it cannot be raced past. Counting
/// active facts and then writing is two steps, and nothing else serializes
/// them — the KG's single-writer actor orders writes only within one palace,
/// while the count spans all of them. Before the admission lock, N callers
/// that all observed 19 would all pass and the surface would land above 20.
/// Every other Tier S test is sequential and cannot catch this.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn tier_s_cap_holds_under_concurrent_writes() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "race"}))
        .await
        .expect("palace_create");

    // One free slot, then many writers contend for it at once.
    fill_tier_s(&state, "race", crate::prompt_facts::TIER_S_MAX_FACTS - 1).await;

    let state = std::sync::Arc::new(state);
    let mut handles = Vec::new();
    for i in 0..16 {
        let state = state.clone();
        handles.push(tokio::spawn(async move {
            dispatch_tool(
                &state,
                "kg_assert",
                json!({
                    "palace": "race",
                    "subject": format!("racer-{i}"),
                    "predicate": "has_convention",
                    "object": format!("contender {i}"),
                }),
            )
            .await
            .is_ok()
        }));
    }

    let mut admitted = 0usize;
    for h in handles {
        if h.await.expect("task joined") {
            admitted += 1;
        }
    }

    assert_eq!(admitted, 1, "exactly one writer should win the last slot");
    let facts = crate::prompt_facts::gather_hot_triples(&state)
        .await
        .expect("gather");
    assert_eq!(
        facts.len(),
        crate::prompt_facts::TIER_S_MAX_FACTS,
        "concurrent writers must never push the surface past the cap",
    );
}

/// Why (#4888): the chat `kg_assert` tool takes `predicate` and `object`
/// straight from the model's tool call and writes them directly, so an
/// ordinary chat turn could push the always-injected surface past 20 or land
/// an unbounded fact. This is the routinely-hit surface, not an admin one.
#[tokio::test]
async fn chat_kg_assert_tool_enforces_tier_s_cap() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "chatcap"}))
        .await
        .expect("palace_create");
    fill_tier_s(&state, "chatcap", crate::prompt_facts::TIER_S_MAX_FACTS).await;

    let args = json!({
        "palace_id": "chatcap",
        "subject": "chat-rule",
        "predicate": "has_convention",
        "object": "a rule the assistant tried to add when the surface was full",
    })
    .to_string();
    let res = crate::chat::tools::execute_tool("kg_assert", &args, &state).await;

    let err = res["error"].as_str().unwrap_or_default();
    assert!(err.contains("Tier S is full"), "{res}");
    assert!(err.contains("remove_prompt_fact"), "{res}");
    assert!(
        res.get("status").is_none(),
        "must not report success: {res}"
    );

    // Fail-closed: nothing landed.
    let facts = crate::prompt_facts::gather_hot_triples(&state)
        .await
        .expect("gather");
    assert_eq!(facts.len(), crate::prompt_facts::TIER_S_MAX_FACTS);
    assert!(!facts.iter().any(|(s, _, _)| s == "chat-rule"), "{facts:?}");
}

/// Why (#4888): the chat tool must enforce the form constraint too — an
/// unbounded object is injected verbatim into every turn.
#[tokio::test]
async fn chat_kg_assert_tool_enforces_form_constraint() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "chatform"}))
        .await
        .expect("palace_create");

    let args = json!({
        "palace_id": "chatform",
        "subject": "verbose",
        "predicate": "is_fact",
        "object": "q".repeat(crate::prompt_facts::TIER_S_MAX_OBJECT_CHARS + 1),
    })
    .to_string();
    let res = crate::chat::tools::execute_tool("kg_assert", &args, &state).await;

    let err = res["error"].as_str().unwrap_or_default();
    assert!(err.contains("81 characters"), "{res}");
    assert!(err.contains("limit is 80"), "{res}");

    let facts = crate::prompt_facts::gather_hot_triples(&state)
        .await
        .expect("gather");
    assert!(facts.is_empty(), "rejected write must not land: {facts:?}");
}

/// Why (#4888): the chat tool must stay usable for cold predicates — the gate
/// is scoped to the injected surface, not to the knowledge graph.
#[tokio::test]
async fn chat_kg_assert_tool_allows_cold_predicates_at_cap() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "chatcold"}))
        .await
        .expect("palace_create");
    fill_tier_s(&state, "chatcold", crate::prompt_facts::TIER_S_MAX_FACTS).await;

    let args = json!({
        "palace_id": "chatcold",
        "subject": "alice",
        "predicate": "works_at",
        "object": "z".repeat(200),
    })
    .to_string();
    let res = crate::chat::tools::execute_tool("kg_assert", &args, &state).await;
    assert_eq!(res["status"], "asserted", "{res}");
}

/// Why (#4888): `kuzu-migrate` imports `relation_type` verbatim from a legacy
/// file, so a legacy vocabulary colliding with a hot predicate would land on
/// the always-injected surface. A bulk legacy import is not a deliberate act
/// of authoring a standing rule (ADR-0028 D8 point 3), so that path refuses
/// hot predicates outright rather than counting free slots — which leaves no
/// cap arithmetic to get wrong, and no enumeration that could fail open.
/// Cold relation types, which is every one in practice, still import
/// regardless of length.
#[test]
fn kuzu_migrate_refuses_hot_predicates_and_passes_cold_ones() {
    use crate::prompt_facts::is_hot_predicate;

    // The importer's guard is exactly `is_hot_predicate`, so every Tier S
    // predicate is refused.
    for p in crate::prompt_facts::HOT_PREDICATES {
        assert!(is_hot_predicate(p), "{p} must be refused by kuzu-migrate");
    }

    // Relation types a legacy kuzu-memory store actually carries are cold, so
    // ordinary imports are unaffected. `alias_of` is the near-miss worth
    // pinning: it reads like an alias but is not the hot `is_alias_for`.
    for p in [
        "relates_to",
        "mentions",
        "derived_from",
        "part_of",
        "alias_of",
    ] {
        assert!(
            !is_hot_predicate(p),
            "{p} is an ordinary relation type and must still import",
        );
    }
}

/// Why (#5048 re-review): the enqueue drop was tested but the worker's own two
/// loss paths were not, and they lose a write just as completely — a daemon
/// that will not spawn and an index call that fails both leave the drawer out
/// of the BM25 corpus with nothing queued to repair it.
/// What: drives `spawn_bm25_index_worker` directly with a client pointed at a
/// dead socket, so `client.index` fails, and asserts the palace is queued.
/// Test: this test itself. Delete the `dirty.insert` from the index-failure arm
/// and the queue stays empty.
#[tokio::test]
async fn a_failed_index_call_queues_the_palace_for_repair() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dirty: crate::bm25_repair::DirtyPalaces = std::sync::Arc::new(dashmap::DashSet::new());
    let (tx, rx) = tokio::sync::mpsc::channel::<Bm25IndexRequest>(8);

    // No listener at this path, so every `index` call fails at connect.
    let client = std::sync::Arc::new(trusty_common::bm25_client::Bm25Client::new(
        tmp.path().join("dead.sock"),
    ));
    // No supervisor: this isolates the index-failure arm from the spawn arm.
    spawn_bm25_index_worker(rx, Some(client), None, std::sync::Arc::clone(&dirty));

    tx.send(Bm25IndexRequest {
        palace: "lossy".to_string(),
        drawer_id: Uuid::new_v4().to_string(),
        content: "content that will never reach the daemon".to_string(),
        data_dir: tmp.path().join("bm25"),
    })
    .await
    .expect("send to worker");

    for _ in 0..200 {
        if dirty.contains("lossy") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        dirty.contains("lossy"),
        "an index call that failed lost the write and must queue the palace"
    );
}

/// Why: the other worker loss path. A supervisor that cannot start a daemon
/// makes the worker skip the request entirely, which is the same lost write.
/// What: points the daemon locator at a path that does not exist so
/// `ensure_running` fails, and asserts the palace is queued.
/// Test: this test itself. Delete the `dirty.insert` from the spawn-failure arm
/// and the queue stays empty.
#[tokio::test]
async fn a_daemon_that_will_not_spawn_queues_the_palace_for_repair() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let prev = std::env::var("TRUSTY_BM25_DAEMON_BIN").ok();
    let prev_ext = std::env::var("TRUSTY_BM25_EXTERNAL").ok();
    // SAFETY: test-only env mutation, restored below.
    unsafe {
        std::env::set_var("TRUSTY_BM25_DAEMON_BIN", tmp.path().join("no-such-binary"));
        std::env::remove_var("TRUSTY_BM25_EXTERNAL");
    }

    let dirty: crate::bm25_repair::DirtyPalaces = std::sync::Arc::new(dashmap::DashSet::new());
    let (tx, rx) = tokio::sync::mpsc::channel::<Bm25IndexRequest>(8);
    let client = std::sync::Arc::new(trusty_common::bm25_client::Bm25Client::new(
        tmp.path().join("unused.sock"),
    ));
    let supervisor = std::sync::Arc::new(crate::bm25_supervisor::Bm25Supervisor::new());
    spawn_bm25_index_worker(
        rx,
        Some(client),
        Some(supervisor),
        std::sync::Arc::clone(&dirty),
    );

    // A palace name short enough that the socket path stays inside `sun_path`.
    let palace = format!("nz{:x}", std::process::id() & 0xfff);
    tx.send(Bm25IndexRequest {
        palace: palace.clone(),
        drawer_id: Uuid::new_v4().to_string(),
        content: "content that will never reach a daemon".to_string(),
        data_dir: tmp.path().join("bm25"),
    })
    .await
    .expect("send to worker");

    for _ in 0..400 {
        if dirty.contains(&palace) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let queued = dirty.contains(&palace);

    // SAFETY: restoring the captured prior values.
    unsafe {
        match prev {
            Some(v) => std::env::set_var("TRUSTY_BM25_DAEMON_BIN", v),
            None => std::env::remove_var("TRUSTY_BM25_DAEMON_BIN"),
        }
        if let Some(v) = prev_ext {
            std::env::set_var("TRUSTY_BM25_EXTERNAL", v);
        }
    }

    assert!(
        queued,
        "a daemon that will not spawn lost the write and must queue the palace"
    );
}

/// Why (#5048 re-review): `Full` marked the palace dirty and `Closed`, three
/// lines below it, logged at `debug!` and returned. Both lose the write
/// identically — the drawer never reaches the index and nothing remembers that
/// it did not. Fixing one arm and leaving its sibling is the shape #4683
/// shipped with.
/// What: drops the receiver so `try_send` returns `Closed`, then enqueues.
/// Test: this test itself. Remove the `mark_dirty` from the `Closed` arm and
/// the queue stays empty.
#[tokio::test]
async fn a_closed_index_queue_queues_the_palace_for_repair() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut state = AppState::new(tmp.path().to_path_buf());

    // Swap in a sender whose receiver is dropped. Waiting for the real worker
    // to exit would race its spawn; this closes the channel deterministically.
    let (tx, rx) = tokio::sync::mpsc::channel::<Bm25IndexRequest>(8);
    drop(rx);
    state.bm25_index_tx = tx;

    assert!(
        matches!(
            state.bm25_index_tx.try_send(Bm25IndexRequest {
                palace: "default".to_string(),
                drawer_id: Uuid::new_v4().to_string(),
                content: "probe".to_string(),
                data_dir: state.data_root.join("default"),
            }),
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_))
        ),
        "precondition: the queue must actually be closed, not merely full"
    );

    bm25_index_enqueue(&state, "default", Uuid::new_v4(), "content that is lost");

    assert_eq!(
        crate::bm25_repair::dirty_palaces(&state),
        vec!["default".to_string()],
        "a closed queue loses the write as completely as a full one and must queue repair"
    );
}
