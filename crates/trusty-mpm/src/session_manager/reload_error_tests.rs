//! Tests for `SessionManager::get`'s tolerance of a transient store-reload error.
//!
//! Why: `session_manager/tests.rs` is at the 1500-SLOC test cap; this single
//! test (extracted verbatim, #2453 review finding 1 round 2 — the `pane_id`
//! field addition pushed `tests.rs` 5 SLOC over budget) lives here, mirroring
//! `reactivate_tests.rs` / `restart_tests.rs`'s established extraction pattern.
//! What: `manager_get_returns_last_known_on_reload_error`.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::path::PathBuf;

use tempfile::TempDir;

use super::manager::ManagedError;
use super::record::ManagedSessionId;
use super::tests::{corrupt_store_file, make_manager};

/// Why: #1219 follow-up — a transient reload error on a single-session lookup
/// must NOT surface as a false `SessionNotFound`; that would make a still-present
/// session look gone. `get()` must fall back to the last-known in-memory record.
/// What: creates a session, corrupts `sessions.json` so the next `get()` reload
/// fails, and asserts `get()` still returns the previously-loaded record instead
/// of erroring.
/// Test: this test.
#[tokio::test]
async fn manager_get_returns_last_known_on_reload_error() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let record = mgr
        .create(
            "single-session task".into(),
            Some(PathBuf::from("/tmp/wt-getlastknown")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    let id = record.id;

    // Inject a transient reload failure by corrupting the backing file.
    corrupt_store_file(&mgr);

    // get() must fall back to the last-known record, not a false not-found.
    let got = mgr
        .get(&id)
        .await
        .expect("get must return last-known record on reload error");
    assert_eq!(got.id, id, "get() returned the last-known record");

    // A genuinely-absent id must still be a not-found, even under reload error.
    let missing = ManagedSessionId::new();
    assert!(
        matches!(
            mgr.get(&missing).await,
            Err(ManagedError::SessionNotFound(_))
        ),
        "an unknown id must still yield SessionNotFound"
    );
}
