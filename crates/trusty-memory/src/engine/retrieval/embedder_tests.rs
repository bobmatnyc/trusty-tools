//! Tests that a shared-embedder init failure is `EngineError::Embed` (#9278 T3c-2).
//!
//! Why: the process-wide `SHARED_EMBEDDER` cell is seeded by other tests, so
//! the init-failure arm of `shared_embedder()` is unreachable through it.
//! `init_in` takes the cell and the init, so these tests drive that arm with a
//! local cell and no model download.
//! What: a failing init and a never-finishing init, each asserted to be
//! `EngineError::Embed` with the cause's text.
//! Test: this file.

use super::embedder::init_in;
use crate::engine::EngineError;
use crate::engine::embed::Embedder;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::OnceCell;

type Cell = OnceCell<Arc<dyn Embedder + Send + Sync>>;

/// Why: a `from_internal` mapping at the init site would make this `Other`.
/// What: a local empty cell, an init that fails, a generous timeout.
/// Test: itself.
#[tokio::test]
async fn an_embedder_init_failure_is_engine_error_embed() {
    let cell = Cell::const_new();
    let err = init_in(&cell, Duration::from_secs(30), || async {
        Err(anyhow::anyhow!("no model on this host"))
    })
    .await
    .map(|_| ())
    .expect_err("a failing init must be an Err");
    assert!(matches!(err, EngineError::Embed(_)), "{err:?}");
    assert!(
        format!("{err:#}").contains("no model on this host"),
        "{err:#}"
    );
}

/// Why: the timeout arm is the second `Err` source at the same site.
/// What: an init that never finishes under a 10 ms ceiling.
/// Test: itself.
#[tokio::test]
async fn an_embedder_init_timeout_is_engine_error_embed() {
    let cell = Cell::const_new();
    let err = init_in(&cell, Duration::from_millis(10), || {
        std::future::pending::<anyhow::Result<Arc<dyn Embedder + Send + Sync>>>()
    })
    .await
    .map(|_| ())
    .expect_err("a hung init must time out");
    assert!(matches!(err, EngineError::Embed(_)), "{err:?}");
    assert!(format!("{err:#}").contains("timed out"), "{err:#}");
}
