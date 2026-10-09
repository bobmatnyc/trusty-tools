//! #9487 AC4c: a remember whose vector step exceeds the HNSW op budget fails
//! closed.
//!
//! Why: the vector upsert runs before the drawer and KG writes. If its budget
//! error were logged and swallowed, the remember would report success for a
//! drawer that recall can never find by vector — the Fail-Open shape AC4 rules
//! out.
//! What: one real on-disk palace with the mock embedder. A first remember
//! succeeds; then the HNSW insert gate is parked from the test and a second
//! remember runs under a watchdog. It must return `BudgetExceeded` and leave
//! the in-memory drawer table, the persisted drawer table and the KG exactly
//! as they were.
//! Test: this file is the test.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::memory_core::palace::{Palace, PalaceId, RoomType};
    use crate::memory_core::retrieval::{PalaceHandle, RememberOptions};
    use crate::memory_core::store::concurrent_open::OpenIntent;
    use crate::memory_core::store::hnsw_store::op_budget::{OpBudgetError, op_budget_error};

    /// Pipeline ceiling well above the op budget, so the op budget fires first.
    const PIPELINE: Duration = Duration::from_secs(10);
    const SMALL: Duration = Duration::from_millis(300);
    const WATCHDOG: Duration = Duration::from_secs(20);

    fn opts() -> RememberOptions {
        RememberOptions {
            force: true,
            defer_embedding: false,
            ..RememberOptions::default()
        }
    }

    /// Drawers in memory, drawers in redb, active KG triples.
    fn counts(handle: &PalaceHandle) -> (usize, usize, usize) {
        (
            handle.drawers.read().len(),
            handle.kg.load_drawer_ids().expect("load drawer ids").len(),
            handle.kg.count_active_triples().expect("count triples"),
        )
    }

    /// Why (#9487 AC4c): the error arm of the remember path is the one that
    /// must not advance any table. See the module doc.
    /// What: see the module doc. The insert gate is parked inside the
    /// watchdog's future, so a hung remember releases it before the panic.
    /// Test: this test.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_remember_whose_vector_step_exceeds_its_budget_commits_nothing() {
        crate::memory_core::retrieval::seed_shared_embedder_with_mock();
        let dir = tempfile::tempdir().expect("tempdir");
        let palace = Palace {
            id: PalaceId::new("op-budget-test"),
            name: "Op budget".into(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: dir.path().join("op-budget-test"),
        };
        std::fs::create_dir_all(&palace.data_dir).expect("create palace data dir");
        let handle = PalaceHandle::open_with_intent(&palace, OpenIntent::Writer)
            .expect("open palace handle for writing");

        handle
            .remember_with_options_within(
                "the first fact lands normally".into(),
                RoomType::General,
                vec![],
                0.5,
                opts(),
                PIPELINE,
            )
            .await
            .expect("an unobstructed remember succeeds");
        let before = counts(&handle);

        let store = handle.vector_store.clone();
        store.op_breaker().set_budget_for_test(SMALL);
        let attempt = async {
            let _parked = store.hnsw_for_test().park_insert_gate_for_test();
            handle
                .remember_with_options_within(
                    "a second fact whose vector cannot be inserted".into(),
                    RoomType::General,
                    vec![],
                    0.5,
                    opts(),
                    PIPELINE,
                )
                .await
        };
        let outcome = tokio::time::timeout(WATCHDOG, attempt)
            .await
            .expect("the remember must give up at the op budget, not hang");

        let err = outcome.expect_err("a remember whose vector step failed must not succeed");
        assert!(
            matches!(
                op_budget_error(&err),
                Some(OpBudgetError::BudgetExceeded { op: "upsert", .. })
            ),
            "expected the op budget error, got {err:#}"
        );
        assert_eq!(
            counts(&handle),
            before,
            "a failed remember must not advance the drawer table or the KG"
        );
        assert!(store.op_breaker().is_tripped());
    }
}
