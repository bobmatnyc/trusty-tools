//! The standing guard on #4743: a test process reaches no trusty-search daemon
//! on any destructive path.
//!
//! Why a real, live, accepting listener rather than a dead port: a dead port
//! makes the guarded and the unguarded code look identical — both end with "no
//! index was deleted", one because the request was refused at the source and one
//! because nothing was there to answer it. The listener here is a daemon that
//! WOULD have accepted the DELETE and answered 200, so the connection counter
//! staying at zero is evidence about trusty-mpm's behaviour rather than about
//! the machine's port allocation. This mirrors the shape PR #4864 used for the
//! registry-write guard, and the reason is the same.
//!
//! Why these fixtures reproduce the reported hazard exactly: the issue names
//! `decommission_full_still_terminates_the_runtime`, whose workspace basename is
//! `full`. `disposable_workspace_index_id` derives the index id from a bare
//! `file_name()`, so that test asks a daemon to destroy an index called `full`.
//! `sess`, `live` and `proj` appear the same way elsewhere in this suite, and
//! the operator's daemon carried a real index named `proj` when this was
//! written. Nothing about the fixture names is fixed here — the point of the fix
//! is that the id no longer matters, because the request is never issued.
//!
//! Test: the two functions below.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tempfile::TempDir;

use super::record::{ManagedSessionId, ManagedSessionState};
use super::tests::make_manager;

/// A loopback stand-in for the operator's trusty-search daemon that accepts
/// every connection, answers a success body, and counts what it saw.
struct CountingDaemon {
    connections: Arc<AtomicUsize>,
    /// Data dir whose `trusty-search/http_addr` points at this listener, for
    /// `TRUSTY_DATA_DIR_OVERRIDE`.
    data_dir: TempDir,
}

impl CountingDaemon {
    /// Bind, start accepting, and write the discovery file the production
    /// resolver reads.
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connections = Arc::new(AtomicUsize::new(0));

        let counter = Arc::clone(&connections);
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            while let Ok((mut sock, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf).await;
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                          Content-Length: 36\r\n\r\n{\"removed\":true,\"data_deleted\":true}",
                    )
                    .await;
                let _ = sock.shutdown().await;
            }
        });

        let data_dir = crate::test_support::hermetic_temp_dir();
        std::fs::create_dir_all(data_dir.path().join("trusty-search")).unwrap();
        std::fs::write(
            data_dir.path().join("trusty-search").join("http_addr"),
            addr.to_string(),
        )
        .unwrap();

        Self {
            connections,
            data_dir,
        }
    }

    /// Confirm the fixture's own premise: this listener really does accept and
    /// answer. Without it, a zero count could mean the listener was broken.
    async fn self_check(&self) {
        let body = reqwest::Client::new()
            .get(format!(
                "http://{}/health",
                std::fs::read_to_string(
                    self.data_dir.path().join("trusty-search").join("http_addr")
                )
                .unwrap()
            ))
            .send()
            .await
            .expect("the stand-in daemon must answer — the fixture's premise")
            .text()
            .await
            .unwrap();
        assert!(body.contains("removed"), "unexpected body: {body}");
        // The self-check's own connection does not count against the assertion.
        self.connections.store(0, Ordering::SeqCst);
    }

    fn seen(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

/// The regression test for #4743: a full decommission from a test process must
/// put nothing on the wire, even though a daemon is discoverable and willing.
///
/// Why serial: `TRUSTY_DATA_DIR_OVERRIDE` is process-global.
/// What: reproduces the issue's own fixture — an SM-owned workspace whose
/// basename is `full`, so `disposable_workspace_index_id` yields the index id
/// `full` and `decommission_with_root` reaches its effect-4 index delete. Points
/// daemon discovery at a listener that would accept the DELETE, decommissions
/// for real, and asserts the listener saw zero connections. Reverting
/// `DestructiveIndexDelete::acquire`'s `running_under_test_harness` branch makes
/// this fail with a count of 1.
/// Test: this function IS the test.
#[serial_test::serial]
#[tokio::test]
async fn decommission_issues_no_request_to_a_live_daemon_under_test() {
    let daemon = CountingDaemon::start().await;
    daemon.self_check().await;

    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;

    let managed_root = crate::test_support::hermetic_temp_dir();
    // The issue's own fixture name. A bare `file_name()` becomes the index id.
    let workspace_path = managed_root.path().join("owner").join("repo").join("full");
    std::fs::create_dir_all(&workspace_path).unwrap();

    let record = mgr
        .create_with_id(
            ManagedSessionId::new(),
            "task".into(),
            Some(workspace_path.clone()),
            None,
            Some(workspace_path.clone()),
            None,
            None,
            crate::runtime::RuntimeKind::default(),
            false,
            true, // owned — reaches the index-delete effect
        )
        .await
        .expect("create");

    // SAFETY: `#[serial]` — no other test thread races this set/restore.
    unsafe {
        std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, daemon.data_dir.path());
    }
    let outcome = mgr
        .decommission_with_root(&record.id, managed_root.path(), None)
        .await;
    unsafe {
        std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
    }

    let (tombstone, _removed) = outcome.expect("full decommission");
    assert_eq!(
        tombstone.state,
        ManagedSessionState::Decommissioned,
        "the guard must not change what decommission does to the record"
    );
    assert_eq!(
        daemon.seen(),
        0,
        "a test process must reach the trusty-search daemon zero times — a DELETE here \
         carries ?delete_data=true and destroys a real index named `full` (#4743)"
    );
}

/// The same guarantee for the second destructive site, which before #4743 had
/// no guard of its own at all.
///
/// Why separate: `sweep_orphaned_search_indexes` formatted its own
/// `?delete_data=true` URL in a loop rather than calling the decommission-time
/// helper, so a guard placed only on that helper would have left this path
/// fully exposed. Asserting on the sweep directly is what keeps the two from
/// diverging again.
/// What: with a willing daemon discoverable, a non-dry-run sweep must return
/// empty having made no request — not even the index listing, which the
/// capability is acquired ahead of.
/// Test: this function IS the test.
#[serial_test::serial]
#[tokio::test]
async fn sweep_makes_no_request_under_a_test_harness() {
    let daemon = CountingDaemon::start().await;
    daemon.self_check().await;

    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;

    // SAFETY: `#[serial]` — no other test thread races this set/restore.
    unsafe {
        std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, daemon.data_dir.path());
    }
    let swept = mgr.sweep_orphaned_search_indexes(false).await;
    unsafe {
        std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
    }

    assert_eq!(
        swept.expect("sweep must not error").len(),
        0,
        "a refused sweep reports nothing removed"
    );
    assert_eq!(
        daemon.seen(),
        0,
        "the orphan sweep must not even LIST indexes from a test process (#4743)"
    );
}
