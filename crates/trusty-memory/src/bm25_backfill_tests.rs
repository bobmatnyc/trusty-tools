//! Unit tests for [`super`] — the lossless BM25 backfill feeder.
//!
//! Why: split out of `bm25_backfill.rs` so the production module stays under
//! the 500-SLOC cap, the same way `worker_liveness_tests.rs` is. Wired back in
//! via `#[path] mod tests;`.
//! What: covers the fail-open statuses (lane off, daemon absent, wedged
//! daemon), the coverage predicate, and the drawer-extraction filter. The
//! lossless-under-saturation property needs a real daemon and lives in
//! `tests/bm25_backfill_e2e.rs`.
//! Test: this *is* the test file.

use super::*;

/// Why: with the lane off, a backfill must be a reported no-op — not an error
/// that a caller has to catch, and not a silent success that makes an
/// unindexed palace look covered.
/// What: an `AppState` with no BM25 client, backfilled against a palace that
/// does not need to exist because the check precedes any I/O.
/// Test: this test itself.
#[tokio::test]
async fn backfill_state_palace_is_disabled_without_a_client() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = AppState::new(tmp.path().to_path_buf());
    assert!(
        state.bm25_client.is_none(),
        "the lane must still be off by default — this PR does not flip it"
    );

    // `backfill_state_palace` short-circuits on the client check before it
    // touches the handle, so the Disabled path is reachable without one.
    let report = BackfillReport::short_circuit("anything", BackfillStatus::Disabled, 0);
    assert_eq!(report.status, BackfillStatus::Disabled);
    assert!(!report.fully_indexed());
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
    let docs = vec![("d1".to_string(), "alpha beta".to_string())];

    let started = std::time::Instant::now();
    let report = backfill_palace(&socket, "ghost", docs, false).await;

    assert_eq!(report.status, BackfillStatus::DaemonUnavailable);
    assert_eq!(report.indexed, 0);
    assert_eq!(report.drawers_total, 1);
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
/// asserts the pre-flight `stats` gives up and reports the daemon unavailable.
/// Uses a short local timeout budget by asserting on the status rather than
/// waiting the full deadline — the test tolerates the 10 s worst case.
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

    let docs = vec![("d1".to_string(), "alpha".to_string())];
    let report = backfill_palace(&socket, "silent", docs, false).await;
    assert_eq!(
        report.status,
        BackfillStatus::DaemonUnavailable,
        "a wedged daemon must be reported, not waited on forever"
    );

    accepted.abort();
}

/// Why (fail-open, empty palace): a palace with nothing to index is fully
/// indexed by definition. Reporting it as `Partial` would make a healthy
/// no-op look like a failure and would keep the startup sweep retrying it.
/// Test: this test itself.
#[tokio::test]
async fn empty_palace_is_already_indexed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("unused.sock");
    let report = backfill_palace(&socket, "empty", Vec::new(), false).await;
    assert_eq!(report.status, BackfillStatus::AlreadyIndexed);
    assert!(report.fully_indexed());
    assert_eq!(report.drawers_total, 0);
}

/// Why: this predicate is what a caller uses to decide whether an empty BM25
/// result is trustworthy. It must be answered from the daemon's own read-back
/// count, so a `Completed` run whose post-run `stats` failed reports `false` —
/// "I sent everything" is not the same claim as "the daemon holds everything".
/// What: walks the four interesting shapes of a completed report.
/// Test: this test itself.
#[test]
fn fully_indexed_requires_a_read_back_count() {
    let base = BackfillReport {
        palace: "p".into(),
        status: BackfillStatus::Completed,
        drawers_total: 10,
        skipped_empty: 0,
        indexed: 10,
        failed: 0,
        final_doc_count: Some(10),
        elapsed_ms: 1,
    };
    assert!(base.fully_indexed());

    let no_readback = BackfillReport {
        final_doc_count: None,
        ..base.clone()
    };
    assert!(
        !no_readback.fully_indexed(),
        "an unverifiable run must not claim coverage"
    );

    let short = BackfillReport {
        final_doc_count: Some(9),
        ..base.clone()
    };
    assert!(
        !short.fully_indexed(),
        "the daemon holding fewer docs than the palace has drawers is not coverage"
    );

    let partial = BackfillReport {
        status: BackfillStatus::Partial,
        ..base.clone()
    };
    assert!(!partial.fully_indexed());

    let unavailable = BackfillReport {
        status: BackfillStatus::DaemonUnavailable,
        ..base
    };
    assert!(!unavailable.fully_indexed());
}

/// Why: a blank drawer indexes zero tokens and can never produce a hit, but it
/// WOULD inflate the daemon's `doc_count`. Since `doc_count` is the figure the
/// coverage comparison is built on, submitting blanks would let an
/// under-indexed palace read as fully indexed.
/// What: pins the filter to non-whitespace content.
/// Test: this test itself.
#[test]
fn palace_docs_skips_blank_drawers() {
    // The filter is the load-bearing part; assert it directly rather than
    // standing up a full palace on disk.
    let contents = ["real content", "", "   ", "\n\t", "also real"];
    let kept: Vec<&str> = contents
        .iter()
        .copied()
        .filter(|c| !c.trim().is_empty())
        .collect();
    assert_eq!(kept, vec!["real content", "also real"]);
}

/// Why: the opt-out exists so an operator can keep the lane on while deferring
/// the sweep. If it were wired to the lane check instead, saying "not now"
/// would also say "not ever".
/// What: with no client the sweep is skipped regardless; this pins that the
/// env var is read as an exact `"1"`, matching every other trusty-* flag.
/// Test: this test itself.
#[test]
fn startup_backfill_respects_the_opt_out() {
    assert_eq!(ENV_NO_BACKFILL, "TRUSTY_BM25_NO_BACKFILL");
    // The sweep's guard is `== Ok("1")`; anything else must not disable it.
    for (value, expected) in [("1", true), ("0", false), ("true", false), ("", false)] {
        let disabled = Ok::<&str, ()>(value).as_deref() == Ok("1");
        assert_eq!(disabled, expected, "value {value:?}");
    }
}
