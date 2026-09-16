//! #7979 regression tests: a failed migration must be visible in the payload
//! the daemon serves, not only in a WARN log line.
//!
//! Why: a corrupt `chunks.json` correctly fails its migration and leaves the
//! schema stamp where it was, but the index then serves 0 chunks while
//! `GET /indexes/:id/status` reported nothing at all — so the endpoint an
//! operator consults rendered a broken index as an ordinary empty one.
//! What: drives a genuinely failing chain through `run_migrations` and asserts
//! the `migration_error` object in the status body, plus the clear-on-success
//! arm that keeps the record from outliving the condition.
//! Test: `failed_schema_chain_is_reported_as_migration_error_in_status`,
//! `a_succeeding_chain_clears_an_earlier_recorded_fault`.

use super::*;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexId, IndexRegistry};
use crate::service::server::{index_status_report, SearchAppState};
use tokio::sync::RwLock;

const FAILURE_TEXT: &str = "simulated corrupt snapshot (#7979)";

/// A migration whose `apply` always fails — the shape a corrupt snapshot takes.
struct FailingMigration;

#[async_trait]
impl Migration for FailingMigration {
    fn source_version(&self) -> u32 {
        0
    }
    fn target_version(&self) -> u32 {
        1
    }
    fn description(&self) -> &'static str {
        "always fails (#7979 test)"
    }
    async fn apply(&self, _index: &IndexHandle) -> Result<(), anyhow::Error> {
        Err(anyhow::anyhow!(FAILURE_TEXT))
    }
}

/// A migration that succeeds without touching the corpus.
struct NoopMigration;

#[async_trait]
impl Migration for NoopMigration {
    fn source_version(&self) -> u32 {
        0
    }
    fn target_version(&self) -> u32 {
        0
    }
    fn description(&self) -> &'static str {
        "no-op (#7979 test)"
    }
    async fn apply(&self, _index: &IndexHandle) -> Result<(), anyhow::Error> {
        Ok(())
    }
}

/// Register one bare (no-corpus) index and return its state plus its handle.
fn state_with_index(id: &str) -> (Arc<SearchAppState>, Arc<IndexHandle>) {
    let registry = IndexRegistry::new();
    let handle = registry.register(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(RwLock::new(CodeIndexer::new(id, "/tmp/migration-7979"))),
        std::path::PathBuf::from("/tmp/migration-7979"),
    ));
    (Arc::new(SearchAppState::new(registry)), handle)
}

/// #7979 verbatim: the status body must name the failed migration.
#[tokio::test]
async fn failed_schema_chain_is_reported_as_migration_error_in_status() {
    let (state, handle) = state_with_index("migration-error-7979");

    let before = index_status_report(&state, "migration-error-7979")
        .await
        .expect("status 200");
    assert!(
        before["migration_error"].is_null(),
        "a healthy index must report no migration error, got: {}",
        before["migration_error"]
    );

    let registry = MigrationRegistry {
        migrations: vec![Arc::new(FailingMigration)],
    };
    let err = run_migrations(&handle, &registry)
        .await
        .expect_err("#7979: the failing chain must surface as an error");
    assert!(
        matches!(err, MigrationError::Apply { .. }),
        "expected the apply arm, got: {err:?}"
    );

    let after = index_status_report(&state, "migration-error-7979")
        .await
        .expect("status 200");
    assert_eq!(
        after["migration_error"]["stage"],
        crate::core::indexer::MIGRATION_STAGE_SCHEMA_CHAIN,
        "#7979: the status body must name which stage failed"
    );
    let detail = after["migration_error"]["detail"]
        .as_str()
        .expect("#7979: migration_error must carry the failure text");
    assert!(
        detail.contains(FAILURE_TEXT),
        "the detail must carry the underlying cause, got: {detail}"
    );
    assert!(
        after["migration_error"]["at"].is_string(),
        "the record must say when it was taken"
    );
}

/// The record must not outlive the condition that produced it.
#[tokio::test]
async fn a_succeeding_chain_clears_an_earlier_recorded_fault() {
    let (state, handle) = state_with_index("migration-clear-7979");

    let failing = MigrationRegistry {
        migrations: vec![Arc::new(FailingMigration)],
    };
    run_migrations(&handle, &failing)
        .await
        .expect_err("must fail");
    assert!(
        !index_status_report(&state, "migration-clear-7979")
            .await
            .expect("status 200")["migration_error"]
            .is_null(),
        "precondition: the fault must be recorded first"
    );

    let healthy = MigrationRegistry {
        migrations: vec![Arc::new(NoopMigration)],
    };
    run_migrations(&handle, &healthy)
        .await
        .expect("a no-op chain must succeed");

    let after = index_status_report(&state, "migration-clear-7979")
        .await
        .expect("status 200");
    assert!(
        after["migration_error"].is_null(),
        "a successful chain must clear the record, got: {}",
        after["migration_error"]
    );
}
