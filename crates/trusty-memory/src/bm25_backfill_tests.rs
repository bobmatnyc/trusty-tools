//! Unit tests for [`super`] — the lossless BM25 backfill feeder.
//!
//! Why: split out of `bm25_backfill.rs` so the production module stays under
//! the 500-SLOC cap, the same way `worker_liveness_tests.rs` is. Wired back in
//! via `#[path] mod tests;`.
//! What: covers the fail-open statuses (lane off, daemon absent, wedged
//! daemon), the coverage predicate, and the drawer-extraction filter. Every
//! test here calls the production function it names — a test that restates the
//! logic inline passes against a deleted implementation, which is how three of
//! these came to prove nothing. The lossless-under-saturation property needs a
//! real daemon and lives in `tests/bm25_backfill_e2e.rs`.
//! Test: this *is* the test file.

use super::*;
use trusty_common::memory_core::palace::Drawer;
use uuid::Uuid;

/// Why: with the lane off, a backfill must be a reported no-op — not an error
/// that a caller has to catch, and not a silent success that makes an
/// unindexed palace look covered.
/// What: calls `backfill_state_palace` against an `AppState` with no BM25
/// client. The lane check precedes every use of the handle, so a bare in-memory
/// handle is enough to reach it — the point is that the REAL function is what
/// produces the status, not a report built by hand.
/// Test: this test itself.
#[tokio::test]
async fn backfill_state_palace_is_disabled_without_a_client() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = AppState::new(tmp.path().to_path_buf());
    assert!(
        state.bm25_client.is_none(),
        "the lane must still be off by default — this PR does not flip it"
    );

    let handle = test_handle(tmp.path(), &["some content"]);
    let report = backfill_state_palace(&state, &handle, "anything", false).await;

    assert_eq!(report.status, BackfillStatus::Disabled);
    assert_eq!(
        report.missing_after, None,
        "a run that never happened has verified nothing"
    );
    assert!(!report.fully_indexed());
}

/// Build an in-memory `PalaceHandle` holding one drawer per content string.
///
/// Why: the disabled and blank-filter paths need a real handle so the tests can
/// call the production functions rather than restate them. Nothing here touches
/// disk — `PalaceHandle::new` takes an in-memory vector store and KG.
/// What: an empty palace with the given drawer contents pushed in.
/// Test: used by the tests above and below.
fn test_handle(
    dir: &std::path::Path,
    contents: &[&str],
) -> trusty_common::memory_core::retrieval::PalaceHandle {
    use trusty_common::memory_core::palace::PalaceId;
    use trusty_common::memory_core::store::kg::KnowledgeGraph;
    use trusty_common::memory_core::store::vector::UsearchStore;

    let vs = UsearchStore::new(dir.join("idx.usearch"), 384).expect("vector store");
    let kg = KnowledgeGraph::open(&dir.join("kg.db")).expect("kg");
    let handle = trusty_common::memory_core::retrieval::PalaceHandle::new(
        PalaceId::new("unit-test"),
        String::new(),
        vs,
        kg,
    );
    {
        let mut drawers = handle.drawers.write();
        for c in contents {
            drawers.push(Drawer::new(Uuid::new_v4(), *c));
        }
    }
    handle
}

/// Why (fail-open, daemon absent): pointing the feeder at a socket nothing is
/// listening on must produce `DaemonUnavailable` promptly. The two failures
/// this rules out are a propagated error (which would fail a caller's request)
/// and a hang (which would hold a startup task open).
/// What: a tempdir path with no listener; asserts the status and that the call
/// returned well inside the per-op timeout rather than waiting it out.
/// Test: this test itself.
#[tokio::test]
async fn backfill_reports_daemon_unavailable_when_socket_is_dead() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("nothing-here.sock");
    let docs = PalaceDocs::from_pairs(vec![("d1".to_string(), "alpha beta".to_string())]);

    let started = std::time::Instant::now();
    let report = backfill_palace(&socket, "ghost", docs, false).await;

    assert_eq!(report.status, BackfillStatus::DaemonUnavailable);
    assert_eq!(report.indexed, 0);
    assert_eq!(report.drawers_total, 1);
    assert_eq!(report.missing_after, None);
    assert!(!report.fully_indexed());
    assert!(
        started.elapsed() < OP_TIMEOUT,
        "a refused connection must fail fast, not wait out the {OP_TIMEOUT:?} deadline"
    );
}

/// Why (fail-open, wedged daemon): a socket that accepts but never answers is
/// the case a plain `read_line` would hang on forever. The per-op timeout is
/// the only thing standing between that and a stalled sweep, so it needs a
/// test that actually produces the condition.
/// What: binds a listener that accepts connections and then does nothing, and
/// asserts the pre-flight coverage probe gives up and reports the daemon
/// unavailable.
/// Test: this test itself.
#[tokio::test]
async fn backfill_gives_up_on_a_socket_that_never_answers() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("silent.sock");
    let listener = tokio::net::UnixListener::bind(&socket).expect("bind silent listener");
    // Accept and hold, never reply.
    let accepted = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });

    let docs = PalaceDocs::from_pairs(vec![("d1".to_string(), "alpha".to_string())]);
    let report = backfill_palace(&socket, "silent", docs, false).await;
    assert_eq!(
        report.status,
        BackfillStatus::DaemonUnavailable,
        "a wedged daemon must be reported, not waited on forever"
    );
    assert!(!report.fully_indexed());

    accepted.abort();
}

/// Why (fail-open, empty palace): a palace with nothing indexable is fully
/// indexed by definition — there is no id that could be missing. It is the one
/// case where coverage is established without asking the daemon, and it must
/// not require a daemon to be reachable.
/// Test: this test itself.
#[tokio::test]
async fn empty_palace_is_already_indexed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("unused.sock");
    let report = backfill_palace(&socket, "empty", PalaceDocs::default(), false).await;
    assert_eq!(report.status, BackfillStatus::AlreadyIndexed);
    assert!(report.fully_indexed());
    assert_eq!(report.missing_after, Some(0));
    assert_eq!(report.drawers_total, 0);
}

/// Why: a palace of nothing but blank drawers has nothing to index, so it is
/// covered — but its `drawers_total` must still report the drawers it holds,
/// otherwise the report lies about the palace's size.
/// Test: this test itself.
#[tokio::test]
async fn a_palace_of_only_blank_drawers_is_covered_and_counted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("unused.sock");
    let docs = PalaceDocs {
        docs: Vec::new(),
        skipped_empty: 3,
    };
    let report = backfill_palace(&socket, "blanks", docs, false).await;
    assert!(report.fully_indexed());
    assert_eq!(report.drawers_total, 3, "the palace holds three drawers");
    assert_eq!(report.skipped_empty, 3);
}

/// Why: this predicate is the alarm the whole design rests on. It previously
/// answered from `stats.doc_count >= drawers_total`, a COUNT comparison — and
/// because trusty-memory issues no BM25 `delete`, stale documents accumulate
/// until that comparison is satisfied by a corpus sharing no ids with the
/// palace. It must now be answerable only by a verified, empty missing set.
/// What: walks every shape of report, including the one that broke it — a
/// daemon holding MORE documents than the palace has drawers while still
/// missing one of them.
/// Test: this test itself.
#[test]
fn fully_indexed_requires_a_verified_empty_missing_set() {
    let base = BackfillReport {
        palace: "p".into(),
        status: BackfillStatus::Completed,
        drawers_total: 10,
        skipped_empty: 0,
        indexed: 10,
        failed: 0,
        missing_after: Some(0),
        final_doc_count: Some(10),
        elapsed_ms: 1,
    };
    assert!(base.fully_indexed());

    // The regression: a corpus bloated with stale documents satisfies every
    // count comparison while still missing a live drawer.
    let stale_bloat = BackfillReport {
        missing_after: Some(1),
        final_doc_count: Some(400),
        ..base.clone()
    };
    assert!(
        !stale_bloat.fully_indexed(),
        "400 documents cannot cover 10 drawers when one of the 10 is absent"
    );
    assert_eq!(stale_bloat.stale_doc_estimate(), Some(390));

    let no_verification = BackfillReport {
        missing_after: None,
        ..base.clone()
    };
    assert!(
        !no_verification.fully_indexed(),
        "an unverifiable run must not claim coverage"
    );

    // Status alone must not be able to satisfy the predicate — this is the
    // exact fail-open the `AlreadyIndexed` arm used to have.
    for status in [
        BackfillStatus::AlreadyIndexed,
        BackfillStatus::Completed,
        BackfillStatus::Partial,
        BackfillStatus::DaemonUnavailable,
        BackfillStatus::Disabled,
    ] {
        let unverified = BackfillReport {
            status,
            missing_after: None,
            ..base.clone()
        };
        assert!(
            !unverified.fully_indexed(),
            "{status:?} must not claim coverage without a verified missing set"
        );
    }
}

/// Why: a short-circuit report is produced by paths that did no work and
/// verified nothing. If its constructor defaulted `missing_after` to `Some(0)`
/// the whole predicate would invert.
/// Test: this test itself.
#[test]
fn short_circuit_reports_are_never_covered() {
    for status in [
        BackfillStatus::Disabled,
        BackfillStatus::DaemonUnavailable,
        BackfillStatus::Partial,
    ] {
        let r = BackfillReport::short_circuit("p", status, 7);
        assert_eq!(r.missing_after, None);
        assert!(!r.fully_indexed(), "{status:?}");
        assert_eq!(r.drawers_total, 7);
    }
}

/// Why: stale-document drift is what broke the old predicate. It must be
/// visible to an operator and must never feed back into a coverage decision.
/// Test: this test itself.
#[test]
fn stale_doc_estimate_is_reported_not_acted_on() {
    let mut r = BackfillReport::short_circuit("p", BackfillStatus::Completed, 10);
    r.skipped_empty = 2;
    r.missing_after = Some(0);
    r.final_doc_count = Some(50);
    assert_eq!(
        r.stale_doc_estimate(),
        Some(42),
        "8 indexable drawers, 50 documents held"
    );
    assert!(r.fully_indexed(), "stale documents do not remove coverage");

    r.final_doc_count = Some(3);
    assert_eq!(r.stale_doc_estimate(), Some(0), "never negative");
    r.final_doc_count = None;
    assert_eq!(r.stale_doc_estimate(), None);
}

/// Why: a blank drawer indexes zero tokens and can never produce a hit, but it
/// WOULD add a document to the daemon's corpus. It must be dropped from the
/// submission set and still counted in the palace's size, because a report that
/// silently forgets blank drawers understates what the palace holds.
/// What: calls the production splitter with real `Drawer` values — the earlier
/// version of this test re-implemented the filter inline and passed whether or
/// not `docs_from_drawers` existed.
/// Test: this test itself.
#[test]
fn docs_from_drawers_splits_blank_from_indexable() {
    let room = Uuid::new_v4();
    let drawers: Vec<Drawer> = ["real content", "", "   ", "\n\t", "also real"]
        .iter()
        .map(|c| Drawer::new(room, *c))
        .collect();

    let split = docs_from_drawers(&drawers);

    let texts: Vec<&str> = split.docs.iter().map(|(_, t)| t.as_str()).collect();
    assert_eq!(texts, vec!["real content", "also real"]);
    assert_eq!(split.skipped_empty, 3);
    assert_eq!(split.drawers_total(), 5);
    for (id, _) in &split.docs {
        assert!(
            drawers.iter().any(|d| d.id.to_string() == *id),
            "doc ids must be the drawer ids the coverage probe will ask about"
        );
    }
}

/// Why: `palace_docs` is the lock-holding wrapper the production path uses; a
/// test that only exercises the inner splitter would not catch a wrapper that
/// read the wrong field.
/// Test: this test itself.
#[test]
fn palace_docs_reads_the_drawer_table() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let handle = test_handle(tmp.path(), &["alpha", "  ", "beta"]);
    let split = palace_docs(&handle);
    assert_eq!(split.docs.len(), 2);
    assert_eq!(split.skipped_empty, 1);
    assert_eq!(split.drawers_total(), 3);
}

/// Why: the opt-out exists so an operator can keep the lane on while deferring
/// the sweep. If it were wired to the lane check instead, saying "not now"
/// would also say "not ever".
/// What: calls the production guard with each value set in the environment —
/// the earlier version compared a locally-built `Ok(value).as_deref()` against
/// `Ok("1")`, which is the guard's source restated, not the guard.
/// Test: this test itself.
#[test]
fn startup_backfill_respects_the_opt_out() {
    assert_eq!(ENV_NO_BACKFILL, "TRUSTY_BM25_NO_BACKFILL");
    let prev = std::env::var(ENV_NO_BACKFILL).ok();

    for (value, expected) in [("1", true), ("0", false), ("true", false), ("", false)] {
        // SAFETY: test-only env mutation, restored below. No other test in
        // this module reads this variable.
        unsafe { std::env::set_var(ENV_NO_BACKFILL, value) };
        assert_eq!(startup_backfill_opted_out(), expected, "value {value:?}");
    }

    // SAFETY: same invariant — restoring the captured prior value.
    unsafe { std::env::remove_var(ENV_NO_BACKFILL) };
    assert!(
        !startup_backfill_opted_out(),
        "an unset variable must leave the sweep enabled"
    );
    if let Some(v) = prev {
        // SAFETY: restoring the caller's environment.
        unsafe { std::env::set_var(ENV_NO_BACKFILL, v) };
    }
}

/// Spawn a stub daemon that answers every frame with `reply`, counting frames.
///
/// Why: the coverage probe's three outcomes and its chunking both need a
/// controllable peer. A real daemon cannot be made to answer `-32601`, and a
/// test that constructs the `Coverage` value by hand would restate the
/// classification instead of exercising it.
/// What: binds `socket`, and for every newline-delimited request writes
/// `reply(seen)` back. Returns the shared request counter and the task handle.
/// Test: used by the two tests below.
fn stub_daemon(
    socket: std::path::PathBuf,
    reply: fn(usize) -> String,
) -> (
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = std::sync::Arc::clone(&seen);
    let listener = tokio::net::UnixListener::bind(&socket).expect("bind stub daemon");
    let handle = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let counter = std::sync::Arc::clone(&counter);
            tokio::spawn(async move {
                let (read_half, mut write_half) = stream.into_split();
                let mut reader = BufReader::new(read_half);
                let mut line = String::new();
                while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                    let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let out = format!("{}\n", reply(n));
                    if write_half.write_all(out.as_bytes()).await.is_err() {
                        break;
                    }
                    line.clear();
                }
            });
        }
    });
    (seen, handle)
}

/// Why: the three ways a coverage probe can end are three different operator
/// situations, and only one of them may skip work. Collapsing "the daemon is
/// too old to answer" into "the daemon is gone" sends an operator hunting a
/// dead socket that is in fact serving; collapsing either into "covered" is
/// the fail-open this whole module exists to remove.
/// What: drives the real `probe_coverage` against stub daemons answering a
/// clean result, a `-32601`, and an internal error.
/// Test: this test itself.
#[tokio::test]
async fn coverage_probe_classifies_the_three_failure_modes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ids = vec!["a".to_string(), "b".to_string()];

    let ok = tmp.path().join("ok.sock");
    let (_n, h) = stub_daemon(ok.clone(), |_| {
        r#"{"jsonrpc":"2.0","result":{"missing":["b"],"checked":2},"id":1}"#.to_string()
    });
    let client = Bm25Client::new(ok);
    assert_eq!(
        probe_coverage(&client, "p", &ids).await,
        Coverage::Missing(1)
    );
    h.abort();

    let old = tmp.path().join("old.sock");
    let (_n, h) = stub_daemon(old.clone(), |_| {
        r#"{"jsonrpc":"2.0","error":{"code":-32601,"message":"unknown method: missing_docs"},"id":1}"#
            .to_string()
    });
    let client = Bm25Client::new(old);
    assert_eq!(
        probe_coverage(&client, "p", &ids).await,
        Coverage::Unsupported,
        "a daemon that predates the op must be reported as such, never as covered"
    );
    h.abort();

    let broken = tmp.path().join("broken.sock");
    let (_n, h) = stub_daemon(broken.clone(), |_| {
        r#"{"jsonrpc":"2.0","error":{"code":-32603,"message":"index poisoned"},"id":1}"#.to_string()
    });
    let client = Bm25Client::new(broken);
    assert_eq!(
        probe_coverage(&client, "p", &ids).await,
        Coverage::Unreachable
    );
    h.abort();
}

/// Why: the wire framing is one JSON line per request, so an unchunked probe
/// over the largest palace on this host would build a single ~50 KB frame.
/// Chunking must not change the answer — the missing counts have to sum.
/// What: 600 ids against a chunk size of 256 must produce exactly three
/// requests, and the per-chunk missing sets must add up.
/// Test: this test itself.
#[tokio::test]
async fn coverage_probe_chunks_large_id_sets() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("chunked.sock");
    // Each chunk reports exactly one missing doc, so three chunks sum to three.
    let (seen, h) = stub_daemon(socket.clone(), |_| {
        r#"{"jsonrpc":"2.0","result":{"missing":["x"],"checked":1},"id":1}"#.to_string()
    });
    let ids: Vec<String> = (0..600).map(|i| format!("d{i}")).collect();
    let client = Bm25Client::new(socket);

    assert_eq!(
        probe_coverage(&client, "p", &ids).await,
        Coverage::Missing(3),
        "the missing sets of every chunk must sum"
    );
    assert_eq!(
        seen.load(std::sync::atomic::Ordering::SeqCst),
        3,
        "600 ids at a chunk size of {COVERAGE_CHUNK} must be three requests"
    );
    h.abort();
}

/// Create `count` palaces on disk under the state's data root, each holding one
/// indexable drawer.
///
/// Why: the enumeration bug is only visible when a palace exists on disk and is
/// absent from the open-handle LRU, so the fixture has to go through the
/// registry's own create path.
/// What: returns the ids created.
/// Test: used by the sweep tests below.
fn create_palaces_on_disk(state: &AppState, count: usize) -> Vec<String> {
    use trusty_common::memory_core::palace::{Palace, PalaceId};
    (0..count)
        .map(|i| {
            let id = format!("sweep-{i}");
            let handle = state
                .registry
                .create_palace(
                    &state.data_root,
                    Palace {
                        id: PalaceId::new(&id),
                        name: id.clone(),
                        description: None,
                        created_at: chrono::Utc::now(),
                        data_dir: state.data_root.join(&id),
                    },
                )
                .expect("create palace on disk");
            // Persist through redb — the drawer table is rebuilt from there on
            // open, so an in-memory push alone would leave the reopened palace
            // empty and the test would prove nothing about the sweep.
            let drawer = Drawer::new(Uuid::new_v4(), "content worth indexing");
            handle
                .kg
                .upsert_drawer_sync(&drawer)
                .expect("persist drawer");
            handle.drawers.write().push(drawer);
            id
        })
        .collect()
}

/// Why (#5048 re-review): the sweep enumerated `registry.list()` — the LRU key
/// set of currently-OPEN handles, capped at 64 — while this host holds ~99
/// palaces. At least 35 were never probed, never marked dirty, and the sweep
/// then logged `all coverage verified`. That is the coverage fail-open moved
/// one layer out: the predicate stopped lying about palaces it examined, and
/// the sweep started lying about palaces it never examines.
/// What: builds three palaces on disk, then runs the sweep from a COLD
/// `AppState` whose registry holds nothing — the worst case of the eviction the
/// cap causes. All three must be enumerated, and with the lane off all three
/// must land in the repair queue rather than being silently skipped.
/// Test: this test itself. Revert the enumeration to `state.registry.list()`
/// and `enumerated` reads 0, the queue is empty, and `all_verified()` is true.
#[tokio::test]
async fn startup_sweep_enumerates_every_palace_on_disk() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let ids = create_palaces_on_disk(&AppState::new(root.clone()), 3);

    let cold = AppState::new(root);
    assert!(
        cold.registry.list().is_empty(),
        "precondition: the registry must be cold, so an LRU-based enumeration finds nothing"
    );

    let out = run_startup_sweep(&cold).await;

    assert_eq!(
        out.enumerated,
        Some(3),
        "every palace on disk must be enumerated, not just the open ones"
    );
    assert_eq!(
        out.swept, 3,
        "each has a drawer, so each must be backfilled"
    );
    assert_eq!(
        out.incomplete, 3,
        "the lane is off, so none can be verified"
    );
    assert!(
        !out.all_verified(),
        "a sweep that verified nothing must never read as complete"
    );

    let mut queued = crate::bm25_repair::dirty_palaces(&cold);
    queued.sort();
    assert_eq!(
        queued, ids,
        "every unverified palace must be queued for repair"
    );
}

/// Depth-first search for a file by name under `root`.
fn find_file(root: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|f| f == name) {
            return Some(path);
        }
    }
    None
}

/// Why: a palace the sweep cannot open is a palace it cannot verify, and the
/// pre-existing code `continue`d past it into the "all verified" tally. Same
/// shape as the enumeration gap, one case narrower.
/// What: replaces the palace's KG redb file with a DIRECTORY, so `list_palaces`
/// still decodes the row from `palace.json` but `open_palace` cannot open the
/// store. Asserts the sweep counts it, queues it, and does not report a clean
/// sweep. Corrupting `palace.json` instead would not exercise this branch —
/// `list_palaces` skips rows it cannot decode, so the palace would never be
/// enumerated in the first place.
/// Test: this test itself. Replace the `unopenable` arm with a bare `continue`
/// and `all_verified()` reads true over a palace that was never examined.
#[tokio::test]
async fn startup_sweep_marks_unopenable_palaces_instead_of_skipping_them() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    create_palaces_on_disk(&AppState::new(root.clone()), 1);

    // Locate the store rather than assuming the layout — `create_palace`
    // normalises `data_dir`, so the path is the registry's to decide.
    let kg_path = find_file(&root, "kg.redb").expect("the palace must have a KG store on disk");
    std::fs::remove_file(&kg_path).expect("remove kg.db");
    std::fs::create_dir(&kg_path).expect("a directory cannot be opened as a redb file");

    let cold = AppState::new(root);
    let out = run_startup_sweep(&cold).await;

    assert_eq!(
        out.enumerated,
        Some(1),
        "precondition: the row is still decodable, so the palace IS enumerated"
    );
    assert_eq!(out.unopenable, 1, "an unopenable palace must be counted");
    assert!(
        !out.all_verified(),
        "a palace that could not be examined must not read as verified"
    );
    assert_eq!(
        crate::bm25_repair::dirty_palaces(&cold),
        vec!["sweep-0".to_string()],
        "and queued for repair rather than skipped"
    );
}

/// Why: a failed enumeration is the outermost fail-open — the sweep would run
/// its loop zero times and report a clean result over the entire corpus. It
/// must verify nothing and say so.
/// What: points the state at a data root that cannot be listed.
/// Test: this test itself.
#[tokio::test]
async fn a_sweep_that_cannot_enumerate_verifies_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // A regular file where the data root should be — `read_dir` cannot walk it.
    let root = tmp.path().join("not-a-directory");
    std::fs::write(&root, b"x").expect("write file");

    let state = AppState::new(root);
    let out = run_startup_sweep(&state).await;

    assert_eq!(
        out.enumerated, None,
        "the enumeration failed — say so in the type"
    );
    assert_eq!(out.swept, 0);
    assert!(
        !out.all_verified(),
        "a sweep that examined nothing has zero incomplete palaces only because \
         it looked at none — that must never read as complete"
    );
    assert!(
        crate::bm25_repair::dirty_palaces(&state).is_empty(),
        "nothing was examined, so nothing can be queued — the log line is the alarm"
    );
}
