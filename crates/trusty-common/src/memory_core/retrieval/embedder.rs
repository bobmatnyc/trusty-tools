//! Process-wide shared embedder singleton for the memory retrieval layer.
//!
//! Why: Extracted from retrieval/mod.rs to keep each file under the 500-SLOC
//! cap (#607). Owns the `SHARED_EMBEDDER` OnceCell and the helpers that
//! initialise or pre-seed it.
//! What: `SHARED_EMBEDDER` static, `shared_embedder()` async resolver,
//! `seed_shared_embedder_with_mock()` test helper.
//! Test: `shared_embedder_is_singleton` in retrieval::tests.

use crate::memory_core::embed::{Embedder, FastEmbedder};
use crate::memory_core::timeouts;
use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::sync::OnceCell;

/// Process-wide shared embedder (type-erased).
///
/// Why: `FastEmbedder::new()` loads a ~90 MB ONNX session — creating one per
/// call (as the previous `recall_with_default_embedder` / `remember` /
/// dream `dedup_pass` did) blew memory to multiple GB and forked dozens of
/// model instances. Issue #57. Typed as `dyn Embedder` so tests can seed the
/// cell with `MockEmbedder` before any ONNX download occurs (issue #850).
/// What: A `tokio::sync::OnceCell` initialised on first use and shared by every
/// caller that lacks a context-supplied embedder. Concurrent first-use races
/// collapse to a single load. Tests call `seed_shared_embedder_with_mock()`
/// before any `shared_embedder()` call to avoid HuggingFace downloads in CI.
/// Test: `shared_embedder_is_singleton` confirms two calls return the same
/// `Arc` pointer.
pub(super) static SHARED_EMBEDDER: OnceCell<Arc<dyn Embedder + Send + Sync>> =
    OnceCell::const_new();

/// Resolve (or initialise) the process-wide shared embedder.
///
/// Why: Centralising fallback embedder construction guarantees at most one
/// ONNX session per process — critical for the daemon footprint (issue #57).
/// Issue #906: `FastEmbedder::new()` can take 30-120 s on a CoreML cold-compile
/// or a CUDA warm-up — without a timeout the first `memory_remember` or
/// `memory_recall` call blocks indefinitely. Wrapping in
/// `tokio::time::timeout` turns the hang into an explicit error that the
/// caller (and the daemon) can surface to the user.
/// What: Returns a clone of the shared `Arc<dyn Embedder + Send + Sync>`,
/// initialising it on first call via `FastEmbedder::new()`. The init is
/// bounded by `TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS` (default 180 s). In test
/// builds, callers should first call `seed_shared_embedder_with_mock()` so
/// the cell is pre-populated with `MockEmbedder` and no model download is
/// attempted.
/// Test: `shared_embedder_is_singleton`; timeout path covered by
///       `timeout_wrapper_fires_on_embedder_init` in retrieval::tests.
pub async fn shared_embedder() -> Result<Arc<dyn Embedder + Send + Sync>> {
    let timeout = timeouts::embedder_init_timeout();
    SHARED_EMBEDDER
        .get_or_try_init(|| async {
            let e = tokio::time::timeout(timeout, FastEmbedder::new())
                .await
                .map_err(|_| {
                    anyhow::anyhow!(
                        "FastEmbedder cold init timed out after {:?} (issue #906); \
                         increase TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS if the model \
                         needs more time on this host",
                        timeout
                    )
                })?
                .context("init shared FastEmbedder")?;
            Ok::<Arc<dyn Embedder + Send + Sync>, anyhow::Error>(Arc::new(e))
        })
        .await
        .cloned()
}

/// Pre-seed the shared embedder with a `MockEmbedder` for offline tests.
///
/// Why: CI environments cannot download the ~23 MB ONNX model from HuggingFace
/// without hitting HTTP 429 rate limits. Calling this before any `remember` /
/// `recall` / `dream_cycle` operation in tests avoids the download entirely by
/// pre-populating the process-wide `SHARED_EMBEDDER` cell with a deterministic
/// hash-based mock (issue #850 — mirrors the fix applied to open-mpm in #813).
/// What: Attempts `OnceCell::set` with a 384-dim `MockEmbedder`. Idempotent
/// — if the cell was already set (by an earlier test in the same process), the
/// call is a silent no-op; the first caller wins.
/// Test: All memory-core tests that exercise the embedding path call this at
/// the start of their body; `shared_embedder_is_singleton` verifies ptr-eq.
#[cfg(any(test, feature = "embedder-test-support"))]
pub fn seed_shared_embedder_with_mock() {
    use crate::embedder::MockEmbedder;
    let mock: Arc<dyn Embedder + Send + Sync> = Arc::new(MockEmbedder::new(384));
    // `set` returns Err if already initialised — that is the desired no-op.
    let _ = SHARED_EMBEDDER.set(mock);
}
