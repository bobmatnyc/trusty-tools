//! End-to-end proof that the BM25 backfill is lossless, against a real daemon.
//!
//! Why: the whole reason backfill does not reuse `bm25_index_enqueue` is that
//! the live write path holds a 256-slot channel written with `try_send` and
//! drops on full. The largest palace on this host holds 1311 drawers — five
//! times that queue — so a backfill routed through it would land roughly a
//! fifth of the corpus and report nothing wrong. That claim is only worth
//! anything if it is measured, so this test pushes more documents than the
//! queue could hold and asserts the daemon's own `stats` reports every one.
//!
//! It also exercises the bounded process model: the supervisor's daemon cap
//! must reap the least-recently-used child rather than accumulating one
//! subprocess per palace, which is the failure mode #2845/#2846 record.
//!
//! What: `#[ignore]`d because it needs the `trusty-bm25-daemon` binary. Run
//! with `cargo test -p trusty-memory --test bm25_backfill_e2e -- --include-ignored`.
//!
//! Test: this *is* the test file.

use std::path::PathBuf;

use trusty_common::bm25_client::Bm25Client;
use trusty_memory::bm25_backfill::{backfill_palace, BackfillStatus, PalaceDocs};
use trusty_memory::bm25_supervisor::Bm25Supervisor;
use trusty_memory::tools::BM25_INDEX_QUEUE_CAPACITY;

/// Resolve the freshly-built `trusty-bm25-daemon` binary.
///
/// Why: the supervisor discovers the daemon as a sibling of `current_exe()`,
/// which for an integration test is the test binary under `target/*/deps/`.
/// Pointing `TRUSTY_BM25_DAEMON_BIN` at the real build output sidesteps that.
/// What: honours the env var if already set, else walks up from the test
/// binary looking for the daemon next to it or one level up.
/// Test: this is the test bootstrap.
fn discover_daemon_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("TRUSTY_BM25_DAEMON_BIN") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let mut p = exe.as_path();
    while let Some(parent) = p.parent() {
        let candidate = parent.join("trusty-bm25-daemon");
        if candidate.is_file() {
            return Some(candidate);
        }
        let candidate = parent.join("..").join("trusty-bm25-daemon");
        if candidate.is_file() {
            return Some(candidate);
        }
        p = parent;
    }
    None
}

/// Point the supervisor's locator at the built daemon, or skip.
fn arm_daemon_binary() -> bool {
    let Some(binary) = discover_daemon_binary() else {
        eprintln!(
            "skipping: trusty-bm25-daemon binary not found. Build it first \
             (`cargo build -p trusty-memory --bin trusty-bm25-daemon`) or set \
             TRUSTY_BM25_DAEMON_BIN=<path>."
        );
        return false;
    };
    // SAFETY: test-only env mutation; this binary is the sole writer.
    unsafe {
        std::env::set_var("TRUSTY_BM25_DAEMON_BIN", &binary);
        std::env::remove_var("TRUSTY_BM25_EXTERNAL");
    }
    true
}

/// Why: this is the regression the separate feeder exists for. Against a
/// backfill built on `bm25_index_enqueue`, this assertion reads ~256 (the
/// queue capacity), not 1024 — the rest are dropped with a `warn!` and the
/// palace serves a quarter of its corpus while looking healthy.
/// What: spawns a real daemon, backfills 4x the live write path's queue
/// capacity, and asserts the daemon's own `stats` reports every document,
/// that the report says `Completed`, and that `fully_indexed()` agrees. Then
/// re-runs to prove idempotence and the already-indexed short circuit.
/// Test: this test itself.
#[ignore = "requires trusty-bm25-daemon binary on disk; run with --include-ignored"]
#[tokio::test(flavor = "current_thread")]
async fn backfill_indexes_every_drawer_without_drops() {
    if !arm_daemon_binary() {
        return;
    }

    // 4x the live write path's queue. Any drop-on-full feeder fails here.
    let doc_count = BM25_INDEX_QUEUE_CAPACITY * 4;
    let docs: Vec<(String, String)> = (0..doc_count)
        .map(|i| {
            (
                format!("drawer-{i}"),
                format!("token{i} shared corpus text"),
            )
        })
        .collect();

    let palace = format!("bf{:x}", std::process::id() & 0xffff);
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().join(&palace).join("bm25");

    let supervisor = Bm25Supervisor::new();
    let socket = supervisor
        .ensure_running(&palace, &data_dir)
        .await
        .expect("supervisor must spawn the daemon");

    let docs_for_drift = docs.clone();
    let started = std::time::Instant::now();
    let report = backfill_palace(
        &socket,
        &palace,
        PalaceDocs::from_pairs(docs.clone()),
        false,
    )
    .await;
    let elapsed = started.elapsed();

    assert_eq!(
        report.status,
        BackfillStatus::Completed,
        "backfill must complete: {report:?}"
    );
    assert_eq!(report.failed, 0, "no document may fail: {report:?}");
    assert_eq!(
        report.indexed, doc_count,
        "every document must be acked — a drop-on-full feeder lands ~{BM25_INDEX_QUEUE_CAPACITY}"
    );
    assert_eq!(
        report.final_doc_count,
        Some(doc_count),
        "the daemon's own count must match what we sent"
    );
    assert!(report.fully_indexed());
    eprintln!("backfilled {doc_count} docs in {elapsed:?}");

    // The corpus is genuinely searchable, not merely counted.
    let client = Bm25Client::new(socket.clone());
    let hits = client
        .search("token7", 5)
        .await
        .expect("search must succeed");
    assert!(
        hits.iter().any(|h| h.doc_id == "drawer-7"),
        "a backfilled doc must be reachable by a lexical query; got {hits:?}"
    );

    // Idempotent: a second run sees full coverage and does no work.
    let again = backfill_palace(&socket, &palace, PalaceDocs::from_pairs(docs), false).await;
    assert_eq!(again.status, BackfillStatus::AlreadyIndexed);
    assert_eq!(again.indexed, 0, "a covered palace must not be re-fed");
    assert_eq!(
        again.missing_after,
        Some(0),
        "the skip must be a VERIFIED set"
    );
    assert!(again.fully_indexed());

    // The identity guarantee: a corpus inflated with documents the palace does
    // not have must NOT read as coverage. Against the old count-based
    // predicate, deleting one live doc and adding two stale ones leaves
    // `doc_count` above the drawer count and `AlreadyIndexed` claims full
    // coverage over a palace missing a drawer.
    client.delete("drawer-7").await.expect("delete a live doc");
    for i in 0..2 {
        client
            .index(&format!("stale-{i}"), "a drawer the palace no longer has")
            .await
            .expect("seed a stale doc");
    }
    let drifted = backfill_palace(
        &socket,
        &palace,
        PalaceDocs::from_pairs(docs_for_drift),
        false,
    )
    .await;
    assert!(
        drifted.final_doc_count.unwrap() > drifted.drawers_total,
        "precondition: the corpus is larger than the palace, so every count \
         comparison reports coverage"
    );
    assert_eq!(
        drifted.indexed, doc_count,
        "the missing drawer must have triggered a real run, not the skip"
    );
    assert!(drifted.fully_indexed(), "and the run must have repaired it");

    supervisor.shutdown().await;
}

/// Why: an index that holds only part of a palace must be detectable and
/// repairable — that is the "index partial" arm of the fail-open requirement.
/// Before the `stats` op existed there was no way to observe the shortfall at
/// all, so a partially-indexed palace answered from partial content forever.
/// What: indexes half the corpus directly, then backfills the whole thing and
/// asserts the run detected the shortfall (did NOT short-circuit) and that the
/// daemon's read-back count covers the full corpus afterwards.
/// Test: this test itself.
#[ignore = "requires trusty-bm25-daemon binary on disk; run with --include-ignored"]
#[tokio::test(flavor = "current_thread")]
async fn partial_index_is_detected_and_repaired() {
    if !arm_daemon_binary() {
        return;
    }

    let total = 40usize;
    let docs: Vec<(String, String)> = (0..total)
        .map(|i| (format!("d{i}"), format!("word{i} common")))
        .collect();

    let palace = format!("bp{:x}", std::process::id() & 0xffff);
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().join(&palace).join("bm25");

    let supervisor = Bm25Supervisor::new();
    let socket = supervisor
        .ensure_running(&palace, &data_dir)
        .await
        .expect("spawn daemon");
    let client = Bm25Client::new(socket.clone());

    // Seed half the corpus, standing in for a palace whose live write path
    // dropped the rest under queue pressure.
    for (doc_id, text) in docs.iter().take(total / 2) {
        client.index(doc_id, text).await.expect("seed index");
    }
    let mid = client.stats().await.expect("stats");
    assert_eq!(mid.doc_count, total / 2, "seed must be half the corpus");

    let report = backfill_palace(&socket, &palace, PalaceDocs::from_pairs(docs), false).await;
    assert_eq!(
        report.status,
        BackfillStatus::Completed,
        "a shortfall must trigger a real run, not the already-indexed skip: {report:?}"
    );
    assert_eq!(report.indexed, total);
    assert_eq!(report.missing_after, Some(0));
    assert_eq!(report.final_doc_count, Some(total));
    assert!(report.fully_indexed());

    supervisor.shutdown().await;
}

/// Why (#2845/#2846): before the cap, `ensure_running` only ever grew the map
/// — one `memory_recall_all` across ~99 palaces left 99 resident subprocesses
/// for the trusty-memory daemon's lifetime. Against that code this assertion
/// reads 5, not 2.
/// What: drives five palaces through a supervisor capped at 2 with real
/// daemons, then asserts the live population never exceeded the cap, that the
/// reap counter moved, and that the most recently used palace survived.
/// Test: this test itself.
#[ignore = "requires trusty-bm25-daemon binary on disk; run with --include-ignored"]
#[tokio::test(flavor = "current_thread")]
async fn daemon_population_stays_within_the_cap() {
    if !arm_daemon_binary() {
        return;
    }

    let cap = 2usize;
    let supervisor = Bm25Supervisor::with_limits(cap, None);
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = format!("bc{:x}", std::process::id() & 0xffff);

    let mut palaces = Vec::new();
    for i in 0..5 {
        let palace = format!("{base}{i}");
        let data_dir = tmp.path().join(&palace).join("bm25");
        supervisor
            .ensure_running(&palace, &data_dir)
            .await
            .unwrap_or_else(|e| panic!("spawn {palace}: {e:#}"));
        assert!(
            supervisor.supervised_count().await <= cap,
            "population must never exceed the cap of {cap}"
        );
        palaces.push(palace);
    }

    assert_eq!(
        supervisor.supervised_count().await,
        cap,
        "steady state must sit at the cap, not at the number of palaces touched"
    );
    assert_eq!(
        supervisor.reaped_count(),
        3,
        "five palaces through a cap of two must reap three daemons"
    );

    supervisor.shutdown().await;
    assert_eq!(supervisor.supervised_count().await, 0);
}

/// Why: a reaped daemon must be transparently respawnable — otherwise the cap
/// trades an unbounded process count for a permanently degraded palace, which
/// is a worse bug than the one it fixes.
/// What: capped at 1, spawns palace A, spawns palace B (reaping A), then asks
/// for A again and asserts it comes back serving.
/// Test: this test itself.
#[ignore = "requires trusty-bm25-daemon binary on disk; run with --include-ignored"]
#[tokio::test(flavor = "current_thread")]
async fn a_reaped_palace_respawns_on_next_use() {
    if !arm_daemon_binary() {
        return;
    }

    let supervisor = Bm25Supervisor::with_limits(1, None);
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = format!("br{:x}", std::process::id() & 0xffff);
    let (a, b) = (format!("{base}a"), format!("{base}b"));

    let sock_a = supervisor
        .ensure_running(&a, &tmp.path().join(&a).join("bm25"))
        .await
        .expect("spawn a");
    Bm25Client::new(sock_a.clone())
        .index("d1", "alpha beta")
        .await
        .expect("index into a");

    supervisor
        .ensure_running(&b, &tmp.path().join(&b).join("bm25"))
        .await
        .expect("spawn b");
    assert_eq!(supervisor.supervised_count().await, 1, "cap of 1 must hold");
    assert_eq!(supervisor.reaped_count(), 1);

    // A was reaped; asking again must bring it back, snapshot intact.
    let sock_a2 = supervisor
        .ensure_running(&a, &tmp.path().join(&a).join("bm25"))
        .await
        .expect("respawn a");
    let stats = Bm25Client::new(sock_a2)
        .stats()
        .await
        .expect("stats from respawned a");
    assert_eq!(
        stats.doc_count, 1,
        "a respawned daemon must reload its snapshot, not start empty"
    );

    supervisor.shutdown().await;
}
