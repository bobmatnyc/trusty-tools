//! #9275: `palace_verify_embedded` while the daemon's embedder is warming.
//!
//! Why: the chunk-count check needs an initialised embedder, so a fully
//! embedded drawer answers `verified: false` during warm-up. A caller has to
//! be able to tell that "not yet" apart from "unfindable", and the answer has
//! to name its cause.
//! What: its own test binary, so the process-wide shared embedder is never
//! seeded — the state a daemon is in before its warm-up finishes.
//! Test: this file is the test.

use serde_json::json;
use trusty_memory::AppState;
use trusty_memory::engine::palace::{Drawer, PalaceId};
use trusty_memory::engine::retrieval::shared_embedder_initialized;
use trusty_memory::engine::store::vector::VectorStore;
use trusty_memory::tools::dispatch_tool;

/// Why (#9275): see the module doc.
/// What: a short drawer with its vector row, asked about before any embedder
/// has initialised. `verified` stays false (fail closed) and the answer says
/// the chunk check did not run, why, and that the embedder is `warming`; once
/// the startup warm-up has failed, it says `failed` and names that error.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verify_embedded_names_the_unrun_chunk_check_while_warming() {
    // #88: bypass the project-slug gate — this palace has no project root.
    // SAFETY: the only test in this process, and it wants the idempotent "1".
    unsafe {
        std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = AppState::new(tmp.path().to_path_buf());
    dispatch_tool(&state, "palace_create", json!({"name": "warming"}))
        .await
        .expect("palace_create");
    let handle = state
        .registry
        .open_palace(&state.data_root, &PalaceId::new("warming"))
        .expect("open palace");
    let drawer = Drawer::new(uuid::Uuid::new_v4(), "A short fact that has its vector.");
    let id = drawer.id;
    handle.add_drawer(drawer);
    let dim = handle.vector_store.dim();
    handle
        .vector_store
        .upsert(id, vec![0.5_f32; dim])
        .await
        .expect("vector row");

    let out = dispatch_tool(
        &state,
        "palace_verify_embedded",
        json!({"palace": "warming", "drawer_ids": [id.to_string()]}),
    )
    .await
    .expect("palace_verify_embedded");

    assert!(
        !shared_embedder_initialized(),
        "precondition: no embedder has initialised in this process"
    );
    assert_eq!(out["embedded"], json!([id.to_string()]), "{out}");
    assert_eq!(out["verified"], false, "{out}");
    assert_eq!(out["chunk_check"], "unavailable", "{out}");
    assert_eq!(out["embedder_ready"], false, "{out}");
    let cause = out["chunk_check_error"].as_str().unwrap_or_default();
    assert!(cause.contains("no embedder has initialised"), "{out}");
    assert_eq!(out["embedder_state"], "warming", "{out}");
    assert_eq!(out["embedder_error"], json!(null), "{out}");

    // The startup warm-up fails: the same question now answers "failed" and
    // names the warm-up's own error, so a caller does not wait on it.
    let _ = state
        .embedder_warm_failure
        .set("FastEmbedder cold init timed out after 180s".to_string());
    let out = dispatch_tool(
        &state,
        "palace_verify_embedded",
        json!({"palace": "warming", "drawer_ids": [id.to_string()]}),
    )
    .await
    .expect("palace_verify_embedded after a failed warm-up");
    assert_eq!(out["verified"], false, "{out}");
    assert_eq!(out["embedder_state"], "failed", "{out}");
    assert_eq!(
        out["embedder_error"], "FastEmbedder cold init timed out after 180s",
        "{out}"
    );
}
