//! A migration chain waiting on its index permit is visible (#8659).
//!
//! Why: the wait used to be silent in the log and absent from status, so a
//! queued M005 read the same as a dead one.
//! What: holds the per-index permit as a reindex would, starts the chain, and
//! reads the wait back through `GET /indexes/:id/status`'s report; then
//! releases the permit and asserts the wait record clears.
//! Test: this module.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;

use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::migration::{run_migrations_exclusive, MigrationRegistry};
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::persistence::PersistedIndex;
use crate::service::persistence_loader::build_indexer_from_entry;
use crate::service::server::{index_status_report, SearchAppState};

/// #8659: with the permit held by a reindex, the chain reports itself waiting
/// with its pending migrations and the holder, and stops reporting once it
/// gets the permit. On pre-fix code `migration_waiting` is absent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::parallel]
async fn a_waiting_migration_is_logged_and_reported_with_its_holder() {
    let dir = tempfile::tempdir().expect("tempdir");
    let id = "wait-8659";
    let mut entry = PersistedIndex::new(id.to_string(), dir.path().to_path_buf());
    entry.colocated = true;
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(8));
    let indexer = build_indexer_from_entry(&entry, &embedder)
        .await
        .expect("build indexer");
    let registry = IndexRegistry::new();
    registry.register(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(RwLock::new(indexer)),
        dir.path().to_path_buf(),
    ));
    let state = Arc::new(SearchAppState::new(registry));
    let handle = state.registry.get(&IndexId::new(id)).expect("registered");

    let permit = crate::service::reindex::index_semaphore(&handle.id)
        .acquire_owned()
        .await
        .expect("permit");
    let holder = crate::service::reindex::mark_index_permit_holder(&handle.id, "reindex");
    let chain = {
        let handle = Arc::clone(&handle);
        tokio::spawn(
            async move { run_migrations_exclusive(&handle, &MigrationRegistry::new()).await },
        )
    };

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let waiting = loop {
        let report = index_status_report(&state, id).await.expect("status");
        if !report["migration_waiting"].is_null() {
            break report["migration_waiting"].clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "never reported waiting"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(waiting["holder"], "reindex", "{waiting}");
    let pending = waiting["pending"].as_array().expect("pending list");
    assert!(
        !pending.is_empty(),
        "an unstamped corpus has migrations pending"
    );
    assert!(
        waiting["waiting_since_unix_ms"].as_u64().is_some(),
        "{waiting}"
    );

    drop(holder);
    drop(permit);
    chain
        .await
        .expect("chain task")
        .expect("the chain runs once the permit is free");
    let report = index_status_report(&state, id).await.expect("status");
    assert!(
        report["migration_waiting"].is_null(),
        "the wait clears once the permit is acquired: {}",
        report["migration_waiting"]
    );
}
