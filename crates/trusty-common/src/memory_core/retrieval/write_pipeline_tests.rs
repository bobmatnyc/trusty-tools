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

    /// A budget large enough to reach the commit but far too small to outlast a
    /// commit-order guard the test is deliberately holding.
    ///
    /// Why: the point of these tests is a GENUINE `tokio::time::timeout` expiry
    /// inside `run_pipeline`. A zero budget never gets there — `remember_within`
    /// rejects it up front, before the pipeline or the timeout is ever polled —
    /// so every assertion built on one describes the fast path, not
    /// cancellation.
    const TINY: Duration = Duration::from_millis(150);

    /// The slot every Tier C test in this module contends for.
    const SLOT: &str = "pr:6366/state";

    /// Write through the real pipeline, optionally claiming a Tier C slot.
    fn slot_opts(fact_key: Option<&str>) -> RememberOptions {
        RememberOptions {
            fact_key: fact_key.map(str::to_string),
            ..opts()
        }
    }

    /// How many drawers in `rows` still carry `SLOT` as a live claim.
    fn claimants(rows: &[crate::memory_core::palace::Drawer]) -> usize {
        rows.iter()
            .filter(|d| d.fact_key.as_deref() == Some(SLOT))
            .count()
    }

    /// Why (issue #6366, round 2): every over-budget test in this file used
    /// `Duration::ZERO`, which `remember_within` rejects BEFORE `run_pipeline`
    /// starts or `tokio::time::timeout` is polled. The cancellation contract
    /// the module documents was therefore asserted by no test at all — a
    /// refactor could have moved or broken the timeout boundary and stayed
    /// green. This is the first test that lets the pipeline start and then cuts
    /// it off for real.
    /// What: holds the palace's commit-order guard so the pipeline blocks on it
    /// mid-flight, runs a write with a small NONZERO budget, and asserts the
    /// timeout fired (not the zero-budget fast path), the error names the
    /// ceiling, and the write mutex came back.
    /// Test: itself.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_mid_flight_timeout_releases_the_mutex_and_names_the_budget() {
        let (_dir, handle) = palace().await;
        // Held for the whole write: the pipeline reaches the commit-order guard
        // and waits there until its budget runs out.
        let blocker = handle.commit_mutex.clone().lock_owned().await;

        let started = Instant::now();
        let err = handle
            .remember_with_options_within(
                "a write cut off after the pipeline started".to_string(),
                RoomType::General,
                vec![],
                0.5,
                opts(),
                TINY,
            )
            .await
            .expect_err("#6366: a write blocked past its budget must fail");

        assert!(
            started.elapsed() >= TINY,
            "#6366: this must be a real timeout expiry, not the zero-budget \
             fast path — the call returned after {:?}, inside the {TINY:?} \
             budget",
            started.elapsed()
        );
        let msg = format!("{err:#}");
        assert!(
            msg.contains("write pipeline exceeded"),
            "error must name the pipeline ceiling: {msg}"
        );
        assert!(
            handle.write_mutex_for_test().try_lock().is_ok(),
            "#6366: a mid-flight cancellation must still release the write mutex"
        );

        drop(blocker);
    }

    /// Why (issue #6366, round 2): the queue property has to hold for a REAL
    /// cancellation, not just for a budget rejected before the pipeline ran.
    /// What: blocks the commit-order guard, lets one writer time out against it
    /// mid-flight, releases the guard, and asserts a second writer with an ample
    /// budget lands normally.
    /// Test: itself.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_writer_queued_behind_a_mid_flight_cancellation_still_lands() {
        let (_dir, handle) = palace().await;
        let blocker = handle.commit_mutex.clone().lock_owned().await;

        let doomed = handle
            .remember_with_options_within(
                "the write cut off mid-flight".to_string(),
                RoomType::General,
                vec![],
                0.5,
                opts(),
                TINY,
            )
            .await;
        assert!(doomed.is_err(), "the mid-flight write must give up");

        drop(blocker);

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
            .expect("#6366: a queued writer must not inherit the cancellation");

        let contents: Vec<String> = handle
            .drawers
            .read()
            .iter()
            .map(|d| d.content().to_string())
            .collect();
        assert!(
            contents.contains(&"the write that must still land".to_string()),
            "the surviving write must be in the drawer table: {contents:?}"
        );
    }

    /// Why (issue #6366, round 2 — the CRITICAL): this encodes the exact
    /// interleaving that made the bound unsound. Writer A's durable commit is
    /// dispatched and its caller has already gone away — the state a mid-commit
    /// timeout produces, because dropping the pipeline future cancels neither
    /// the `spawn_blocking` redb transaction in `upsert_drawers_atomic` nor an
    /// op already queued to the KG writer actor. Writer B then runs its own
    /// Tier C read-decide-write. Before the commit-order guard, B read the
    /// moved `DRAWERS_BY_FACT_KEY` index, failed to find A in the in-memory
    /// table (A's mirror never ran), took `persist_with_retirement`'s "absent
    /// from the in-memory table" fallback, and wrote its newcomer WITHOUT
    /// retiring A — leaving A's row and B's row both carrying a live `SLOT`.
    /// What: writes an incumbent, dispatches A's commit through
    /// `commit_and_mirror` with the guard already held (caller abandoned), runs
    /// B through the real public pipeline, then asserts on the DURABLE rows:
    /// exactly one drawer claims the slot, the index agrees with it, and A —
    /// the abandoned write — is present and retired rather than absent and
    /// still claiming.
    /// Test: itself.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_abandoned_commit_still_leaves_one_claimant_for_the_slot() {
        use crate::memory_core::palace::Drawer;
        use crate::memory_core::retrieval::tier_c;

        let (_dir, handle) = palace().await;

        // The incumbent A is about to displace.
        handle
            .remember_with_options_within(
                "the incumbent holding the slot".to_string(),
                RoomType::General,
                vec![],
                0.5,
                slot_opts(Some(SLOT)),
                AMPLE,
            )
            .await
            .expect("incumbent write");
        let incumbent_id = handle.drawers.read()[0].id;

        // Writer A: a commit that has been dispatched while its caller is gone.
        let room_id = handle.drawers.read()[0].room_id;
        let mut a = Drawer::new(room_id, "the abandoned newcomer");
        a.fact_key = Some(SLOT.to_string());
        a.expires_at = Some(chrono::Utc::now() + chrono::Duration::hours(24));
        let a_id = a.id;
        let guard = handle.commit_mutex.clone().lock_owned().await;
        let tail_a = tokio::spawn(tier_c::commit_and_mirror(handle.commit_ctx(), a, guard));

        // Writer B goes through the real pipeline while A's commit is in
        // flight. Its own commit must queue behind A's on the order guard.
        handle
            .remember_with_options_within(
                "the second writer for the same slot".to_string(),
                RoomType::General,
                vec![],
                0.5,
                slot_opts(Some(SLOT)),
                AMPLE,
            )
            .await
            .expect("the second writer must land");
        tail_a
            .await
            .expect("abandoned commit task")
            .expect("the abandoned commit must still complete");

        // Durable state is the one that matters: the in-memory table is
        // rebuilt from these rows on the next open.
        let rows = handle.kg.load_drawers().expect("load durable drawers");
        assert_eq!(
            claimants(&rows),
            1,
            "#6366: exactly one drawer may hold {SLOT} durably; found {:?}",
            rows.iter()
                .filter(|d| d.fact_key.is_some())
                .map(|d| (d.id, d.content().to_string()))
                .collect::<Vec<_>>()
        );

        let indexed = handle
            .kg
            .drawer_id_for_fact_key(SLOT)
            .expect("index lookup")
            .expect("the slot must still be occupied");
        let live = rows
            .iter()
            .find(|d| d.fact_key.as_deref() == Some(SLOT))
            .expect("one live claimant");
        assert_eq!(
            indexed, live.id,
            "#6366: the index and the drawer rows must name the same claimant"
        );

        // The decisive assertion: A was abandoned by its caller, but its commit
        // still mirrored, so B could see it and retire it. Without the order
        // guard A is absent from the in-memory table when B reads, B takes the
        // no-incumbent fallback, and A's row is still claiming the slot here.
        let a_row = rows
            .iter()
            .find(|d| d.id == a_id)
            .expect("the abandoned commit must be durable");
        assert_eq!(
            a_row.fact_key, None,
            "#6366: the abandoned write must have been retired by the writer \
             behind it, not left as a second claimant"
        );
        let incumbent_row = rows
            .iter()
            .find(|d| d.id == incumbent_id)
            .expect("the incumbent row survives retirement (ADR-0028 D6)");
        assert_eq!(
            incumbent_row.fact_key, None,
            "the original incumbent must have been retired too"
        );

        assert_eq!(
            claimants(&handle.drawers.read()),
            1,
            "#6366: the in-memory mirror must agree with the durable rows"
        );
    }

    /// Why (issue #6366, round 2): the ordering guarantee is what makes the
    /// invariant above hold, so assert the mechanism directly rather than only
    /// its consequence. A second writer's commit must not begin until an
    /// abandoned commit has finished mirroring into the drawer table.
    /// What: holds the order guard, starts a second writer, and asserts it has
    /// NOT committed while the guard is held — then that it does once released.
    /// Test: itself.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_second_writer_waits_for_an_abandoned_commit_to_mirror() {
        let (_dir, handle) = palace().await;
        let guard = handle.commit_mutex.clone().lock_owned().await;

        let queued = {
            let handle = handle.clone();
            tokio::spawn(async move {
                handle
                    .remember_with_options_within(
                        "queued behind the in-flight commit".to_string(),
                        RoomType::General,
                        vec![],
                        0.5,
                        opts(),
                        AMPLE,
                    )
                    .await
            })
        };

        // Give the queued writer every chance to run its pre-commit legs and
        // reach the guard. It must still not have committed.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            handle.drawers.read().is_empty(),
            "#6366: a writer must not commit while the order guard is held"
        );

        drop(guard);
        queued
            .await
            .expect("queued writer task")
            .expect("the queued write must land once the guard is released");
        assert_eq!(handle.drawers.read().len(), 1);
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
