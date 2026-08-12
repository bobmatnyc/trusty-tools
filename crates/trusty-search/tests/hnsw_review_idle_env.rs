//! Every test that MUTATES `TRUSTY_HNSW_REVIEW_IDLE`, isolated into its own
//! test binary (issue #3769).
//!
//! Why: `TRUSTY_HNSW_REVIEW_IDLE` is process-global. Two tests used to write it
//! from inside the `trusty-search` LIB test binary — one in
//! `core::indexer::tests_idle_evict`, one in `core::store_config::tests` —
//! while a third test in that same binary,
//! `hnsw_idle_demotion_reviews_clean_promoted_store`, READ it through
//! `demote_vector_store_if_idle`'s
//! [`trusty_search::core::store_config::hnsw_review_idle_enabled`] gate.
//! `cargo test` runs a binary's tests on a thread pool in ONE process, so a
//! writer holding the var at `"0"` when the reader reached that gate made the
//! gate return `false`, demotion was skipped, and the reader panicked with
//! "expected the idle sweep to demote the clean, promoted store" — issue
//! #3769's exact failure, which reproduced only under `cargo test --workspace`
//! load and passed on an immediate rerun of the same commit.
//!
//! `#[serial]` alone would NOT have fixed it: it serialises the writers against
//! each other, but `setenv`/`unsetenv` reallocate the C `environ` array, so a
//! concurrent NON-serial `getenv` in any other test can still tear. That is the
//! same conclusion `service::server::list_repo_identity_tests` reached for
//! issue #2717 — which names THIS file's
//! `hnsw_idle_demotion_skips_when_disabled_via_env` as the mutator that broke
//! it — and the remedy there was to stop touching global env, not to serialise
//! around it.
//!
//! What: an integration test is its own binary, hence its own PROCESS. Moving
//! both mutators here leaves the 1500+-test lib binary with ZERO writers of
//! this variable, so no current or future lib test can observe it changing —
//! including `service::server::tickers`' idle sweep, which reads the same gate.
//! Inside THIS binary the two mutators are `#[serial]` against each other, and
//! they are the only tests present, so the serialisation is airtight rather
//! than best-effort. No assertion is weakened and no coverage is dropped: both
//! tests keep their original bodies, including the end-to-end
//! "set the env var, observe demotion skipped" path.
//! Test: this file IS the tests.

use std::sync::Arc;
use std::time::Duration;

use trusty_common::embedder::MockEmbedder;
use trusty_search::core::chunker::{ChunkType, RawChunk};
use trusty_search::core::embed::Embedder;
use trusty_search::core::indexer::CodeIndexer;
use trusty_search::core::store::{UsearchStore, VectorStore};
use trusty_search::core::store_config::{hnsw_review_idle_enabled, HNSW_REVIEW_IDLE_ENV};

/// Minimal in-memory `RawChunk` builder (mirrors `indexer::tests_idle_evict::raw`).
fn raw(id: &str, file: &str, content: &str) -> RawChunk {
    RawChunk {
        id: id.to_string(),
        file: file.to_string(),
        start_line: 1,
        end_line: 1 + content.lines().count(),
        content: content.to_string(),
        function_name: None,
        language: Some("rust".to_string()),
        chunk_type: ChunkType::Code,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    }
}

/// The `TRUSTY_HNSW_REVIEW_IDLE=0` escape hatch disables demotion while
/// leaving the store otherwise untouched (issue #2164).
///
/// #3769: moved verbatim out of `core::indexer::tests_idle_evict` — its
/// `set_var` was what raced `hnsw_idle_demotion_reviews_clean_promoted_store`,
/// which stays behind in the lib binary and no longer has a writer to lose to.
#[tokio::test]
#[serial_test::serial]
async fn hnsw_idle_demotion_skips_when_disabled_via_env() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");
    let dim = 32;

    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    let usearch = Arc::new(UsearchStore::new(dim).expect("usearch new"));
    let store: Arc<dyn VectorStore> = usearch.clone();
    let idx = CodeIndexer::new(
        "hnsw-demote-disabled-test",
        "/tmp/hnsw-demote-disabled-test",
    )
    .with_components(embedder, store);

    idx.add_chunk(raw("a", "src/a.rs", "fn a() {}"))
        .await
        .expect("add chunk a");
    idx.save_vector_store(&path).await.expect("save hnsw");
    tokio::time::sleep(Duration::from_millis(5)).await;

    let prior = std::env::var(HNSW_REVIEW_IDLE_ENV).ok();
    // SAFETY (#3769): this binary contains only the two `#[serial]` tests that
    // touch this var, so no concurrent reader or writer exists in this process.
    unsafe { std::env::set_var(HNSW_REVIEW_IDLE_ENV, "0") };

    assert!(
        !idx.demote_vector_store_if_idle(Duration::from_nanos(1))
            .await,
        "TRUSTY_HNSW_REVIEW_IDLE=0 must disable demotion"
    );
    assert!(
        !usearch.in_view_mode(),
        "store must remain mutable while the gate is disabled"
    );

    // SAFETY: see above.
    unsafe {
        match prior {
            Some(v) => std::env::set_var(HNSW_REVIEW_IDLE_ENV, v),
            None => std::env::remove_var(HNSW_REVIEW_IDLE_ENV),
        }
    }

    // Re-enabled (default): demotion now proceeds.
    assert!(
        idx.demote_vector_store_if_idle(Duration::from_nanos(1))
            .await,
        "demotion must proceed once the gate is re-enabled"
    );
    assert!(usearch.in_view_mode());
}

/// `hnsw_review_idle_enabled` honours unset (default on), explicit on/off
/// spellings, and falls back to enabled on garbage (issue #2164).
///
/// #3769: moved verbatim out of `core::store_config::tests` — the second
/// writer of this var inside the lib binary.
#[test]
#[serial_test::serial]
fn hnsw_review_idle_enabled_default_and_env_override() {
    let prior = std::env::var(HNSW_REVIEW_IDLE_ENV).ok();

    // SAFETY (#3769): see `hnsw_idle_demotion_skips_when_disabled_via_env`.
    unsafe { std::env::remove_var(HNSW_REVIEW_IDLE_ENV) };
    assert!(hnsw_review_idle_enabled());

    unsafe { std::env::set_var(HNSW_REVIEW_IDLE_ENV, "0") };
    assert!(!hnsw_review_idle_enabled());

    unsafe { std::env::set_var(HNSW_REVIEW_IDLE_ENV, "false") };
    assert!(!hnsw_review_idle_enabled());

    unsafe { std::env::set_var(HNSW_REVIEW_IDLE_ENV, "1") };
    assert!(hnsw_review_idle_enabled());

    unsafe { std::env::set_var(HNSW_REVIEW_IDLE_ENV, "banana") };
    assert!(hnsw_review_idle_enabled());

    unsafe {
        match prior {
            Some(v) => std::env::set_var(HNSW_REVIEW_IDLE_ENV, v),
            None => std::env::remove_var(HNSW_REVIEW_IDLE_ENV),
        }
    }
}
