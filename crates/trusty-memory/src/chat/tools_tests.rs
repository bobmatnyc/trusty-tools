//! `all_tools` and `execute_tool`: the chat assistant's tool surface.
//!
//! Why this file exists (#6286): these tests lived in `web::tests::chat_tests`
//! beside three endpoint tests that drove an axum router. Nothing here ever
//! did — they call `crate::chat::all_tools` and `execute_tool` directly — so
//! the router's removal moved them rather than costing them. The three
//! endpoint tests they sat with are covered on the socket instead, by
//! `rpc_chat_providers_answers_both_upstreams` and
//! `rpc_messages_send_list_and_mark_read_round_trip` in
//! `crate::transport::uds::tests`; chat-session CRUD is a dispatcher method
//! (`chat_session_*`) rather than a folded one, so it goes through the
//! fallback and needs no separate wire test.

use serde_json::json;
use trusty_common::memory_core::palace::PalaceId;

/// Build a fresh `AppState` rooted in an ephemeral tempdir.
fn test_state() -> crate::AppState {
    trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    // #88: bypass the project-slug enforcement gate.
    // SAFETY: every test in this process wants the same idempotent "1".
    unsafe {
        std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
    }
    let state = crate::AppState::new(root);
    state.set_ready();
    state
}

/// Why: The chat assistant's tool surface is part of the public API — any
/// drift in tool names or required-argument lists is a breaking change for
/// the UI and any external automation. Pin the shape here so a refactor
/// has to acknowledge it.
/// What: Snapshots the names + every tool's `required` array.
/// Test: This test itself.
#[test]
fn all_tools_returns_expected_set() {
    let tools = super::tools::all_tools();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "list_palaces",
            "get_palace",
            "recall_memories",
            "list_drawers",
            "kg_query",
            "get_config",
            "get_status",
            "get_dream_status",
            "get_palace_dream_status",
            "create_memory",
            "kg_assert",
            "memory_recall_all",
        ]
    );
    // Every tool's `parameters` must be a JSON Schema object with a
    // `required` array (possibly empty).
    for t in &tools {
        assert_eq!(
            t.parameters["type"], "object",
            "tool {} schema type",
            t.name
        );
        assert!(
            t.parameters["required"].is_array(),
            "tool {} required not array",
            t.name
        );
    }
}

/// Why: `execute_tool` is the bridge between the model's tool_call
/// arguments and the live Rust core. We exercise the happy path
/// (`list_palaces` on an empty registry returns `[]`) and the unknown-
/// tool path (returns `{"error": "..."}`) to lock down both branches.
/// What: Calls execute_tool against a fresh `AppState`.
/// Test: This test itself.
#[tokio::test]
async fn execute_tool_dispatches_known_tools() {
    let state = test_state();
    let result = super::tools::execute_tool("list_palaces", "{}", &state).await;
    assert!(
        result.is_array(),
        "list_palaces should be array, got {result}"
    );
    assert_eq!(result.as_array().unwrap().len(), 0);

    let unknown = super::tools::execute_tool("not_a_tool", "{}", &state).await;
    assert!(
        unknown["error"]
            .as_str()
            .unwrap_or("")
            .contains("unknown tool"),
        "expected unknown-tool error, got {unknown}"
    );

    let missing = super::tools::execute_tool("get_palace", "{}", &state).await;
    assert!(
        missing["error"]
            .as_str()
            .unwrap_or("")
            .contains("palace_id"),
        "expected missing-arg error, got {missing}"
    );
}

/// Create a palace on `state` and return its name, for the chat KG tests.
fn seed_palace(state: &crate::AppState, name: &str) {
    let palace = trusty_common::memory_core::Palace {
        id: PalaceId::new(name),
        name: name.to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join(name),
    };
    state
        .registry
        .create_palace(&state.data_root, palace)
        .expect("create palace");
}

/// Regression for [#4905](https://github.com/bobmatnyc/trusty-tools/issues/4905):
/// a standing rule the assistant is told to remember in chat must reach the
/// prompt context, not just storage.
///
/// Before the fix `execute_kg_assert` returned `{"status":"asserted"}` without
/// rebuilding the cache, so the user got a success report and a rule that
/// reached no later turn — ADR-0028's Tier S guarantee inverted. This assertion
/// on `formatted` found an empty string against the pre-fix commit.
#[tokio::test]
async fn chat_kg_assert_refreshes_prompt_cache() {
    let state = test_state();
    seed_palace(&state, "chatassert");
    let args = json!({
        "palace_id": "chatassert",
        "subject": "masa",
        "predicate": "has_convention",
        "object": "always branch off a freshly fetched origin/main",
    })
    .to_string();

    let result = super::tools::execute_tool("kg_assert", &args, &state).await;
    assert_eq!(result["status"], "asserted", "got {result}");

    let guard = state.prompt_context_cache.read().await;
    assert!(
        guard
            .formatted
            .contains("always branch off a freshly fetched origin/main"),
        "chat kg_assert did not reach the prompt cache; got: {:?}",
        guard.formatted
    );
}

/// Error arm for the chat path: a Tier S refusal is reported to the model as
/// `{"error": …}` and nothing is written.
///
/// The refusal text must survive consolidation intact — it is what tells the
/// model to retire a fact, so an opaque wrapper would break the recovery the
/// gate depends on.
#[tokio::test]
async fn chat_kg_assert_reports_tier_s_refusal_without_writing() {
    let state = test_state();
    seed_palace(&state, "chatreject");
    let over_long = "x".repeat(crate::prompt_facts::TIER_S_MAX_OBJECT_CHARS + 1);
    let args = json!({
        "palace_id": "chatreject",
        "subject": "masa",
        "predicate": "has_convention",
        "object": over_long,
    })
    .to_string();

    let result = super::tools::execute_tool("kg_assert", &args, &state).await;
    let err = result["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("Tier S fact rejected"),
        "expected the Tier S refusal text, got {result}"
    );

    let handle = state
        .registry
        .get(&PalaceId::new("chatreject"))
        .expect("palace handle");
    let stored = handle.kg.query_active("masa").await.expect("query");
    assert!(
        stored.is_empty(),
        "refused write reached storage: {stored:?}"
    );
    assert!(
        state.prompt_context_cache.read().await.triples.is_empty(),
        "refused write reached the prompt cache"
    );
}
