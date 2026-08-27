//! Coverage-visibility tests for the MCP embed-audit surface (#5000, #4786).
//!
//! Why: `palace_reembed` answers about a whole palace and `console_metrics`
//! answers about at most twenty of them, from cache. These pin the two questions
//! that actually get asked — "are MY ids findable?" and "what does the whole
//! estate look like?" — and, in particular, that the sweep enumerates from disk,
//! which is the property that makes a never-opened palace visible at all.
//! What: drives the real handlers through `dispatch_tool`.
//! Test: this IS the test module.

use super::*;

// ---------------------------------------------------------------------------
// #5000 / #4786 — is this findable?
// ---------------------------------------------------------------------------

/// Put a drawer in the palace with no vector, the way a dropped deferred embed
/// leaves one: durable in redb, permanently invisible to vector recall.
fn add_vectorless_drawer(
    handle: &trusty_common::memory_core::PalaceHandle,
    content: &str,
) -> uuid::Uuid {
    let drawer = trusty_common::memory_core::palace::Drawer::new(uuid::Uuid::new_v4(), content);
    let id = drawer.id;
    handle.add_drawer(drawer);
    id
}

/// Why (#5000 gap 2): a migration or deletion workflow holds its own drawer ids
/// and needs a yes or no about exactly those. Before this the only way to ask
/// was to pull `palace_reembed`'s full missing-set dump and diff it caller-side,
/// and `memory_recall` is not a substitute — it can hit lexically and pass on a
/// drawer no vector search will ever return, which is how #4834 deleted 72
/// source files after a content-recall check.
/// What: one embedded drawer and one vectorless one; asks about both; asserts
/// each lands in the right list and that `verified` — the single boolean a
/// deletion gates on — is false.
/// Test: itself. Removing the dispatch arm makes this an unknown-tool error.
#[tokio::test]
async fn verify_embedded_names_the_unembedded_id() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "verify-test"}))
        .await
        .expect("palace_create");
    let embedded = dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": "verify-test",
            "text": "Harbour measures the ledger on bay 1, an embedded fact about caching.",
        }),
    )
    .await
    .expect("memory_remember");
    let embedded_id = embedded["drawer_id"]
        .as_str()
        .unwrap_or_else(|| panic!("memory_remember returned no drawer_id: {embedded}"))
        .to_string();

    let handle =
        crate::tools::helpers::open_palace_handle(&state, "verify-test").expect("open palace");
    let missing_id = add_vectorless_drawer(&handle, "a fact with no vector").to_string();

    let out = dispatch_tool(
        &state,
        "palace_verify_embedded",
        json!({
            "palace": "verify-test",
            "drawer_ids": [embedded_id.clone(), missing_id.clone()],
        }),
    )
    .await
    .expect("palace_verify_embedded must be dispatchable");

    assert_eq!(
        out["missing"].as_array().map(Vec::len),
        Some(1),
        "the vectorless drawer must be named: {out}"
    );
    assert_eq!(out["missing"][0], missing_id, "{out}");
    assert_eq!(out["embedded"][0], embedded_id, "{out}");
    assert_eq!(
        out["verified"], false,
        "a palace with an unembedded drawer must not pass the deletion gate: {out}"
    );
}

/// Why (#5000): "already gone" and "here but unfindable" call for opposite
/// actions in a deletion workflow, so an id that is not a drawer at all must not
/// be reported as merely unembedded.
/// What: asks about an id nothing ever wrote; asserts it lands in `unknown`, not
/// `missing`, and still fails `verified`.
/// Test: itself.
#[tokio::test]
async fn verify_embedded_separates_an_unknown_id_from_an_unembedded_one() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "verify-unknown"}))
        .await
        .expect("palace_create");
    let stranger = uuid::Uuid::new_v4().to_string();

    let out = dispatch_tool(
        &state,
        "palace_verify_embedded",
        json!({"palace": "verify-unknown", "drawer_ids": [stranger.clone()]}),
    )
    .await
    .expect("palace_verify_embedded");

    assert_eq!(out["unknown"][0], stranger, "{out}");
    assert!(
        out["missing"].as_array().is_some_and(|a| a.is_empty()),
        "an id that is not a drawer is not an unembedded drawer: {out}"
    );
    assert_eq!(out["verified"], false, "{out}");
}

/// Why (#5000): a caller about to delete source files must not have one of its
/// ids silently dropped from the answer it gates on — a skipped id reads as a
/// clean verify.
/// What: passes a malformed id; asserts the call errors rather than answering.
/// Test: itself.
#[tokio::test]
async fn verify_embedded_refuses_a_malformed_id() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "verify-malformed"}))
        .await
        .expect("palace_create");

    let err = dispatch_tool(
        &state,
        "palace_verify_embedded",
        json!({"palace": "verify-malformed", "drawer_ids": ["not-a-uuid"]}),
    )
    .await
    .expect_err("a malformed id must be refused, not skipped");
    assert!(
        format!("{err:#}").contains("not a UUID"),
        "the error must name the problem: {err:#}"
    );
}

/// Why (#5000 gap 1, #4786): `console_metrics` reports from the handle cache, so
/// a palace this process has never opened reports 0/0 — which reads as healthy.
/// That is the blind spot that let palace `localLLM` sit at 15 drawers and zero
/// vectors unnoticed, and that left #4786's three palaces at `drawer_count: 0`
/// unexplained: nothing enumerated the estate and said what it found.
/// What: seeds a palace, then drives the sweep from a FRESH `AppState` whose
/// handle cache is asserted empty. A cache-only enumeration returns nothing
/// here; the sweep must still find the palace and read its real drawer count.
/// Test: itself.
#[tokio::test]
async fn embed_sweep_sees_a_palace_the_cache_never_opened() {
    let (state, tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "sweep-test"}))
        .await
        .expect("palace_create");
    dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": "sweep-test",
            "text": "Harbour measures the ledger on bay 1, a durable fact worth keeping.",
        }),
    )
    .await
    .expect("memory_remember");
    drop(state);

    // A state that has opened nothing: its handle cache is empty, so anything
    // enumerating from the cache reports an empty estate.
    let cold = AppState::new(tmp.path().to_path_buf());
    cold.set_ready();
    assert!(
        cold.registry.list().is_empty(),
        "the fixture must actually start cold, or this proves nothing"
    );

    let out = dispatch_tool(&cold, "palace_embed_sweep", json!({}))
        .await
        .expect("palace_embed_sweep must be dispatchable");

    assert_eq!(
        out["palace_count"], 1,
        "the sweep must enumerate from disk, not from the handle cache: {out}"
    );
    assert_eq!(out["palaces"][0]["palace"], "sweep-test", "{out}");
    assert_eq!(
        out["palaces"][0]["drawer_count"], 1,
        "a palace the cache never opened must report its real count, not 0: {out}"
    );
    assert_eq!(
        out["unhealthy"], 0,
        "nothing is missing a vector here: {out}"
    );
}

/// Why (#5000): the sweep has to say a palace is unhealthy, or it is an
/// inventory rather than a check. The signal is the missing-vector set plus the
/// alias audit — never `drawer_count` against `vector_count`, which #5005
/// disproved in both directions.
/// What: adds a drawer with no vector and asserts the sweep counts the palace as
/// unhealthy and names the shortfall.
/// Test: itself.
#[tokio::test]
async fn embed_sweep_reports_a_palace_with_an_unembedded_drawer() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "sweep-unhealthy"}))
        .await
        .expect("palace_create");
    let handle =
        crate::tools::helpers::open_palace_handle(&state, "sweep-unhealthy").expect("open palace");
    add_vectorless_drawer(&handle, "a fact with no vector");

    let out = dispatch_tool(&state, "palace_embed_sweep", json!({}))
        .await
        .expect("palace_embed_sweep");

    assert_eq!(
        out["unhealthy"], 1,
        "a palace with an unembedded drawer is not healthy: {out}"
    );
    assert_eq!(out["palaces"][0]["missing"], 1, "{out}");
    assert_eq!(out["palaces"][0]["healthy"], false, "{out}");
}
