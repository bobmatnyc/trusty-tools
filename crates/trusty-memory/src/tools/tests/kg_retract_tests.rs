//! Dispatch tests for the `kg_retract_triple` MCP tool.
//!
//! Why: `tools/tests.rs` sits against the 3000-SLOC test cap, so a new tool's
//! tests ship as a child module rather than pushing that file over it.
//! What: `use super::*` inherits the parent test module's imports and its
//! `test_state` helper, so each test reads exactly as it would in-place.
//! Test: this IS the test module.

use super::*;

/// Why: `kg_retract_triple` exists to take back ONE wrong object, so the
/// contract that matters is that the named object goes and its siblings at the
/// same `(subject, predicate)` pair stay — the exact over-deletion that made
/// pair-level `retract` unusable as an undo.
/// What: asserts two objects under one pair, retracts one over the dispatcher,
/// and reads the survivors back through `kg_query`.
/// Test: this test.
#[tokio::test]
async fn dispatch_kg_retract_triple_closes_one_object_and_keeps_siblings() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "retract"}))
        .await
        .expect("palace_create");
    for object in ["Acme", "Globex"] {
        let _ = dispatch_tool(
            &state,
            "kg_assert",
            json!({
                "palace": "retract",
                "subject": "alice",
                "predicate": "works_at",
                "object": object,
            }),
        )
        .await
        .expect("kg_assert");
    }

    let retracted = dispatch_tool(
        &state,
        "kg_retract_triple",
        json!({
            "palace": "retract",
            "subject": "alice",
            "predicate": "works_at",
            "object": "Acme",
        }),
    )
    .await
    .expect("kg_retract_triple");
    assert_eq!(retracted["closed"], 1);
    assert_eq!(retracted["retracted"], true);
    assert!(
        retracted.get("reason").is_none(),
        "a successful retraction carries no no-op reason: {retracted}"
    );

    let queried = dispatch_tool(
        &state,
        "kg_query",
        json!({"palace": "retract", "subject": "alice"}),
    )
    .await
    .expect("kg_query");
    let triples = queried["triples"].as_array().expect("triples array");
    assert_eq!(triples.len(), 1, "sibling object survived: {queried}");
    assert_eq!(triples[0]["object"], "Globex");
}

/// Why: a retraction that matches nothing must be legible rather than silent —
/// a caller cannot otherwise tell "removed it" from "there was nothing there",
/// which is what makes the second call of an idempotent retry safe to read.
/// What: retracts a triple that was never asserted, then retracts a real one
/// twice, and checks `closed` reports 0 with a `reason` both times.
/// Test: this test.
#[tokio::test]
async fn dispatch_kg_retract_triple_missing_triple_is_a_legible_noop() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "noop"}))
        .await
        .expect("palace_create");

    let never_asserted = dispatch_tool(
        &state,
        "kg_retract_triple",
        json!({
            "palace": "noop",
            "subject": "ghost",
            "predicate": "works_at",
            "object": "Nowhere",
        }),
    )
    .await
    .expect("kg_retract_triple on an absent triple is not an error");
    assert_eq!(never_asserted["closed"], 0);
    assert_eq!(never_asserted["retracted"], false);
    assert!(
        never_asserted["reason"].as_str().is_some(),
        "the no-op says why: {never_asserted}"
    );

    let _ = dispatch_tool(
        &state,
        "kg_assert",
        json!({
            "palace": "noop",
            "subject": "alice",
            "predicate": "works_at",
            "object": "Acme",
        }),
    )
    .await
    .expect("kg_assert");
    let args = json!({
        "palace": "noop",
        "subject": "alice",
        "predicate": "works_at",
        "object": "Acme",
    });
    let first = dispatch_tool(&state, "kg_retract_triple", args.clone())
        .await
        .expect("first retract");
    let second = dispatch_tool(&state, "kg_retract_triple", args)
        .await
        .expect("second retract");
    assert_eq!(first["closed"], 1);
    assert_eq!(second["closed"], 0, "retraction is idempotent: {second}");
}

/// Why: omitting `object` must fail loudly. The neighbouring pair-level
/// `KnowledgeGraph::retract` would close every object at the pair, so a
/// silently-defaulted object is the one mistake that turns an undo into data
/// loss.
/// What: dispatches without `object` and checks the error names the argument.
/// Test: this test.
#[tokio::test]
async fn dispatch_kg_retract_triple_requires_object() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "strict"}))
        .await
        .expect("palace_create");
    let err = dispatch_tool(
        &state,
        "kg_retract_triple",
        json!({"palace": "strict", "subject": "alice", "predicate": "works_at"}),
    )
    .await
    .expect_err("missing object must error");
    assert!(
        err.to_string().contains("missing 'object'"),
        "unexpected error: {err}"
    );
}

/// Why: every other test in this module retracts under `works_at`, which
/// `is_hot_predicate` rejects — so the prompt-cache-rebuild branch in
/// `handle_kg_retract_triple` (`kg_ops.rs:128-134`) never actually runs in
/// this suite. The branch is correct as written, but nothing stops a later
/// edit from inverting the condition or dropping the call while every
/// existing test stays green, and the hazard is exactly the one the
/// handler's own doc names: a retracted Tier S fact kept being injected
/// into every session's prompt.
/// What: asserts a fact under the hot predicate `is_fact`, confirms
/// `get_prompt_context` surfaces it, retracts it, and confirms a second
/// `get_prompt_context` call no longer contains the retracted object —
/// which only happens if the retraction path rebuilds the cache.
/// Test: this test.
#[tokio::test]
async fn dispatch_kg_retract_triple_rebuilds_prompt_cache_for_hot_predicate() {
    let (state, _tmp) = test_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "hot"}))
        .await
        .expect("palace_create");

    let _ = dispatch_tool(
        &state,
        "kg_assert",
        json!({
            "palace": "hot",
            "subject": "msrv-rule",
            "predicate": "is_fact",
            "object": "MSRV is 1.94",
        }),
    )
    .await
    .expect("kg_assert");

    let before = dispatch_tool(&state, "get_prompt_context", json!({}))
        .await
        .expect("get_prompt_context after assert");
    let before_text = before.as_str().expect("string body");
    assert!(
        before_text.contains("MSRV is 1.94"),
        "hot fact should be in the prompt cache before retraction: {before_text}"
    );

    let retracted = dispatch_tool(
        &state,
        "kg_retract_triple",
        json!({
            "palace": "hot",
            "subject": "msrv-rule",
            "predicate": "is_fact",
            "object": "MSRV is 1.94",
        }),
    )
    .await
    .expect("kg_retract_triple");
    assert_eq!(retracted["closed"], 1);

    let after = dispatch_tool(&state, "get_prompt_context", json!({}))
        .await
        .expect("get_prompt_context after retract");
    let after_text = after.as_str().expect("string body");
    assert!(
        !after_text.contains("MSRV is 1.94"),
        "retracted hot fact must not still be injected into the prompt: {after_text}"
    );
}
