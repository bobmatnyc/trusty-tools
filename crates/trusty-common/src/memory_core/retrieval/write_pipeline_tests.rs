//! Regression tests for the bounded write pipeline (issue #6366).
//!
//! Why: before this bound, a write that took the per-palace write mutex ran its
//! embed+upsert+persist pipeline with no ceiling of any kind. Three
//! `memory_note` calls were aborted client-side after 1800 s while the daemon
//! stayed healthy and kept serving reads. The property that matters is not just
//! "a slow write errors" — it is that the mutex is RELEASED when it does, so
//! the writers queued behind it proceed instead of inheriting the stall.
//!
//! What: exercises the ceiling on a real on-disk palace through the real
//! pipeline. `defer_embedding` keeps the shared-embedder OnceCell out of these
//! tests entirely — no ONNX download, no cross-test interference with the
//! process-wide cell — so what is measured is the redb/disk critical section
//! the issue is actually about.
//!
//! Test: this file IS the test.

#[cfg(test)]
mod tests {
    use crate::memory_core::palace::{Palace, PalaceId, RoomType};
    use crate::memory_core::retrieval::{PalaceHandle, RememberOptions};
    use crate::memory_core::store::concurrent_open::OpenIntent;
    use crate::memory_core::store::kg::KnowledgeGraph;
    use crate::memory_core::store::vector::UsearchStore;
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    /// A budget no real pipeline can ever exceed, for the happy-path cases.
    const AMPLE: Duration = Duration::from_secs(60);

    /// Open a real on-disk palace handle for one test.
    ///
    /// Why: the defect is about the redb/disk critical section, so an in-memory
    /// handle (no `data_dir`, no L1 snapshot, no KG file) would not exercise
    /// the legs that actually go slow in production.
    /// What: creates a palace under a `TempDir` and opens a writer handle. The
    /// `TempDir` is returned so the caller keeps it alive for the test's life.
    /// Test: used by every test in this module.
    async fn palace() -> (TempDir, Arc<PalaceHandle>) {
        let dir = tempfile::tempdir().expect("tempdir for palace");
        let palace = Palace {
            id: PalaceId::new("write-budget-test"),
            name: "Write budget".into(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: dir.path().join("write-budget-test"),
        };
        std::fs::create_dir_all(&palace.data_dir).expect("create palace data dir");
        let handle = PalaceHandle::open_with_intent(&palace, OpenIntent::Writer)
            .expect("open palace handle for writing");
        (dir, handle)
    }

    /// Options that keep the shared embedder out of the test entirely.
    ///
    /// Why: `shared_embedder()` is a process-wide `OnceCell`; touching it here
    /// would either download an ONNX model or fight whichever other test seeded
    /// it first. `defer_embedding` skips it, and the KG/redb legs — the ones
    /// issue #6366 traced the stall to — still run in full.
    /// What: `RememberOptions` with `force` (no content gate to fight) and
    /// `defer_embedding` set.
    /// Test: used by every test in this module.
    fn opts() -> RememberOptions {
        RememberOptions {
            force: true,
            defer_embedding: true,
            ..RememberOptions::default()
        }
    }

    /// Why (issue #6366): the whole fix is that an over-budget write STOPS. A
    /// pipeline with no ceiling returned `Ok` no matter how long it took, which
    /// is what let one write hold the palace mutex past every client's patience.
    /// What: calls the write with a zero budget and asserts the error names the
    /// budget, the issue, and the override knob an operator would reach for.
    /// Test: itself.
    #[tokio::test]
    async fn an_over_budget_write_fails_with_a_named_reason() {
        let (_dir, handle) = palace().await;
        let err = handle
            .remember_with_options_within(
                "a fact that will not fit inside a zero budget".to_string(),
                RoomType::General,
                vec![],
                0.5,
                opts(),
                Duration::ZERO,
            )
            .await
            .expect_err("a zero-budget write must not be allowed to run");
        let msg = format!("{err:#}");
        assert!(msg.contains("#6366"), "error must cite the issue: {msg}");
        assert!(
            msg.contains("write pipeline exceeded"),
            "error must name the pipeline ceiling: {msg}"
        );
        assert!(
            msg.contains("TRUSTY_WRITE_PIPELINE_TIMEOUT_SECS"),
            "error must name the knob that moves the ceiling: {msg}"
        );
    }

    /// Why (issue #6366): failing the write is only half the fix. If the mutex
    /// stayed held, every queued writer would still be stalled — the reported
    /// symptom — and the error would just move to whoever timed out next.
    /// What: runs an over-budget write, then asserts the palace write mutex is
    /// immediately acquirable, which it can only be if the guard was dropped.
    /// Test: itself.
    #[tokio::test]
    async fn an_over_budget_write_releases_the_palace_write_mutex() {
        let (_dir, handle) = palace().await;
        let outcome = handle
            .remember_with_options_within(
                "another fact that will not fit".to_string(),
                RoomType::General,
                vec![],
                0.5,
                opts(),
                Duration::ZERO,
            )
            .await;
        assert!(
            outcome.is_err(),
            "#6366: the write must have given up — a write that simply \
             succeeded would release the mutex for the wrong reason and this \
             assertion would prove nothing"
        );
        let mutex = handle.write_mutex_for_test();
        assert!(
            mutex.try_lock().is_ok(),
            "#6366: an over-budget write must release the palace write mutex"
        );
    }

    /// Why (issue #6366): the reported symptom is a QUEUE — one slow write, and
    /// every other writer on that palace waits. This is the end-to-end shape:
    /// a writer that gives up must not take its neighbours down with it.
    /// What: races an over-budget writer against a normal one on the same
    /// palace, then asserts the normal write landed and is the only drawer.
    /// Test: itself.
    #[tokio::test]
    async fn a_writer_queued_behind_an_over_budget_write_still_lands() {
        let (_dir, handle) = palace().await;

        let doomed = {
            let handle = handle.clone();
            tokio::spawn(async move {
                handle
                    .remember_with_options_within(
                        "the write that gives up".to_string(),
                        RoomType::General,
                        vec![],
                        0.5,
                        opts(),
                        Duration::ZERO,
                    )
                    .await
            })
        };
        let survivor = {
            let handle = handle.clone();
            tokio::spawn(async move {
                handle
                    .remember_with_options_within(
                        "the write that must still land".to_string(),
                        RoomType::General,
                        vec![],
                        0.5,
                        opts(),
                        AMPLE,
                    )
                    .await
            })
        };

        assert!(
            doomed.await.expect("doomed writer task").is_err(),
            "the zero-budget write must fail"
        );
        survivor
            .await
            .expect("survivor writer task")
            .expect("#6366: a writer sharing the palace must not inherit the stall");

        let drawers = handle.drawers.read();
        assert_eq!(
            drawers.len(),
            1,
            "exactly the surviving write should be durable"
        );
        assert_eq!(drawers[0].content(), "the write that must still land");
    }

    /// Why (issue #6366): a ceiling that fires on ordinary writes would trade a
    /// rare stall for constant failures. The default must be unreachable in
    /// normal operation.
    /// What: performs a plain `remember_with_options` (default ceiling, 240 s)
    /// and asserts it succeeds well inside it.
    /// Test: itself.
    #[tokio::test]
    async fn the_default_ceiling_never_trips_an_ordinary_write() {
        let (_dir, handle) = palace().await;
        let started = Instant::now();
        handle
            .remember_with_options(
                "an ordinary fact written the ordinary way".to_string(),
                RoomType::General,
                vec![],
                0.5,
                opts(),
            )
            .await
            .expect("an ordinary write must not trip the default ceiling");
        assert!(
            started.elapsed() < AMPLE,
            "an ordinary write should be far faster than the ceiling; took {:?}",
            started.elapsed()
        );
    }

    /// Why (issue #6366): a cancelled write must leave the palace usable, not
    /// half-written. The pipeline can only be dropped at an await point, and the
    /// in-memory drawer table is pushed after the durable commit — so an
    /// over-budget write must leave that table untouched.
    /// What: runs an over-budget write, asserts no drawer appeared, then writes
    /// normally and asserts that one does.
    /// Test: itself.
    #[tokio::test]
    async fn an_over_budget_write_leaves_the_palace_writable() {
        let (_dir, handle) = palace().await;
        let _ = handle
            .remember_with_options_within(
                "abandoned".to_string(),
                RoomType::General,
                vec![],
                0.5,
                opts(),
                Duration::ZERO,
            )
            .await;
        assert!(
            handle.drawers.read().is_empty(),
            "#6366: an over-budget write must not push a drawer"
        );
        handle
            .remember_with_options_within(
                "written after the failure".to_string(),
                RoomType::General,
                vec![],
                0.5,
                opts(),
                AMPLE,
            )
            .await
            .expect("the palace must still accept writes after an over-budget one");
        assert_eq!(handle.drawers.read().len(), 1);
    }

    /// Why (issue #6366): the defect is a per-palace queue, so the guard has to
    /// hold under real concurrency, not just for two tasks. Eight writers on one
    /// palace all serialise through the same mutex.
    /// What: spawns eight concurrent writes with an ample ceiling and asserts
    /// every one lands — no lost write, no deadlock, no inherited stall.
    /// Test: itself.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_writers_on_one_palace_all_land() {
        let (_dir, handle) = palace().await;
        let writers: Vec<_> = (0..8)
            .map(|i| {
                let handle = handle.clone();
                tokio::spawn(async move {
                    handle
                        .remember_with_options_within(
                            format!("concurrent fact number {i}"),
                            RoomType::General,
                            vec![],
                            0.5,
                            opts(),
                            AMPLE,
                        )
                        .await
                })
            })
            .collect();
        for writer in writers {
            writer
                .await
                .expect("writer task")
                .expect("every concurrent write must land");
        }
        assert_eq!(
            handle.drawers.read().len(),
            8,
            "#6366: no write may be lost to the per-palace queue"
        );
    }

    /// Why (issue #6366): the size guard reports `kg.redb` beside a slow or
    /// failed write. It must degrade quietly for a handle that has no data
    /// directory rather than failing the write it is describing.
    /// What: builds an in-memory handle and asserts the error still renders,
    /// naming the size as unknown.
    /// Test: itself.
    #[tokio::test]
    async fn the_size_guard_degrades_quietly_without_a_data_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        // `PalaceHandle::new` builds a handle with `data_dir: None`, which is
        // exactly the case the size lookup has to survive.
        let vs = UsearchStore::new(dir.path().join("idx.usearch"), 384).expect("vector store");
        let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).expect("kg store");
        let handle = PalaceHandle::new(PalaceId::new("sizeless"), "in-memory".to_string(), vs, kg);
        let err = handle
            .remember_with_options_within(
                "sizeless".to_string(),
                RoomType::General,
                vec![],
                0.5,
                opts(),
                Duration::ZERO,
            )
            .await
            .expect_err("zero budget still fails");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("unknown"),
            "the size guard should report an unknown size, not fail: {msg}"
        );
    }
}
