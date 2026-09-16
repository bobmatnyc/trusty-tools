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
//! `a_succeeding_chain_clears_an_earlier_recorded_fault`,
//! `a_no_op_schema_chain_does_not_clear_a_json_to_redb_fault`,
//! `both_stages_are_reported_when_both_are_outstanding`.

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
    let faults = after["migration_error"]
        .as_array()
        .expect("#7979: migration_error is the SET of outstanding faults");
    assert_eq!(faults.len(), 1, "one stage failed, got: {faults:?}");
    assert_eq!(
        faults[0]["stage"],
        crate::core::indexer::MIGRATION_STAGE_SCHEMA_CHAIN,
        "#7979: the status body must name which stage failed"
    );
    let detail = faults[0]["detail"]
        .as_str()
        .expect("#7979: migration_error must carry the failure text");
    assert!(
        detail.contains(FAILURE_TEXT),
        "the detail must carry the underlying cause, got: {detail}"
    );
    assert!(
        faults[0]["at"].is_string(),
        "the record must say when it was taken"
    );
}

/// #7979 round 2: one runner's success must not erase the other's live fault.
///
/// Why: the boot order is `restore_indexes` — which records a `json_to_redb`
/// fault — then `spawn_index_migrations` for every index. A schema chain with
/// nothing to do returns `Ok` at its `current >= target` no-op, and an
/// un-stage-keyed record let that success wipe a JSON fault that was still
/// true. The index then served 0 chunks with `migration_error: null` again,
/// which is the exact silence this issue is about.
/// What: records the JSON fault the way `run_migrations_for_entry` does, runs a
/// chain that has nothing to apply, and asserts the JSON fault survives in the
/// status body.
/// Test: this IS the test.
#[tokio::test]
async fn a_no_op_schema_chain_does_not_clear_a_json_to_redb_fault() {
    let (state, handle) = state_with_index("migration-keyed-7979");
    handle.indexer.read().await.record_migration_failure(
        crate::core::indexer::MIGRATION_STAGE_JSON_TO_REDB,
        "corrupt legacy snapshot (#7979)",
    );

    // `current_version() == 0` for this registry and a no-corpus handle reads
    // schema 0, so the chain takes its `current >= target` no-op return.
    let no_op = MigrationRegistry {
        migrations: vec![Arc::new(NoopMigration)],
    };
    run_migrations(&handle, &no_op)
        .await
        .expect("a no-op chain must succeed");

    let after = index_status_report(&state, "migration-keyed-7979")
        .await
        .expect("status 200");
    let faults = after["migration_error"]
        .as_array()
        .expect("#7979: the JSON fault must survive an unrelated chain's success");
    assert_eq!(
        faults.len(),
        1,
        "exactly the JSON fault must remain, got: {faults:?}"
    );
    assert_eq!(
        faults[0]["stage"],
        crate::core::indexer::MIGRATION_STAGE_JSON_TO_REDB,
        "#7979: a schema chain with nothing to do must not clear another stage"
    );
}

/// Both stages outstanding at once are both reported.
#[tokio::test]
async fn both_stages_are_reported_when_both_are_outstanding() {
    let (state, handle) = state_with_index("migration-both-7979");
    handle
        .indexer
        .read()
        .await
        .record_migration_failure(crate::core::indexer::MIGRATION_STAGE_JSON_TO_REDB, "json");

    let failing = MigrationRegistry {
        migrations: vec![Arc::new(FailingMigration)],
    };
    run_migrations(&handle, &failing)
        .await
        .expect_err("must fail");

    let after = index_status_report(&state, "migration-both-7979")
        .await
        .expect("status 200");
    let stages: Vec<&str> = after["migration_error"]
        .as_array()
        .expect("array")
        .iter()
        .map(|f| f["stage"].as_str().expect("stage"))
        .collect();
    assert_eq!(
        stages,
        vec![
            crate::core::indexer::MIGRATION_STAGE_JSON_TO_REDB,
            crate::core::indexer::MIGRATION_STAGE_SCHEMA_CHAIN,
        ],
        "both outstanding faults must be reported, stage-ordered"
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
