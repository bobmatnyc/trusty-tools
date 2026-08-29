//! The standing guard on #4743 — a test process reaches no trusty-search daemon
//! on any destructive path — and the sweep's fail-safe reads (#6285).
//!
//! Why a real, live, accepting daemon rather than a dead socket: a dead socket
//! makes the guarded and the unguarded code look identical — both end with "no
//! index was deleted", one because the request was refused at the source and one
//! because nothing was there to answer it. The daemon here WOULD have accepted
//! the delete and answered success, so the call counter staying at zero is
//! evidence about trusty-mpm's behaviour rather than about the machine.
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
//! Why the sweep's READ path is pinned in the same file: #6285 moved these calls
//! onto the socket, and the sweep's decision to delete rests on what those reads
//! return. A read that fails must not resolve to a value that licenses a delete.
//!
//! Test: the four functions below.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

use super::record::{ManagedSessionId, ManagedSessionState};
use super::tests::make_manager;
use crate::daemon::search_rpc;
use crate::test_support::isolated_daemon_home;
use crate::uds_mock::{self, MockFuture, MockUdsDaemon, RpcError};

/// Generous next to the client's own 5 s budget — this only has to outlast a
/// loopback round-trip on a loaded machine.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// A stand-in for the operator's trusty-search daemon that answers every method
/// with a successful delete body and counts what it was asked.
///
/// Why it binds where production resolution looks, rather than taking
/// `TRUSTY_SEARCH_SOCKET`: the hazard #4743 describes is the daemon a process
/// finds on its own. `daemon_socket_path` under an overridden data directory is
/// that lookup, run against a socket that is not the operator's.
struct CountingDaemon {
    calls: Arc<AtomicUsize>,
    daemon: MockUdsDaemon,
}

impl CountingDaemon {
    /// Bind at the path `search_rpc::search_socket` derives under the active
    /// [`isolated_daemon_home`] override.
    async fn start() -> Self {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let socket = search_rpc::search_socket().expect("derive the trusty-search socket");
        let daemon = uds_mock::spawn_at(socket, move |_method, _params| {
            counter.fetch_add(1, Ordering::SeqCst);
            Box::pin(
                async move { Ok(json!({ "ok": true, "removed": true, "data_deleted": true })) },
            )
        })
        .await;
        Self { calls, daemon }
    }

    /// Confirm the fixture's own premise: this daemon really does accept and
    /// answer. Without it, a zero count could mean the daemon was broken.
    async fn self_check(&self) {
        search_rpc::call_at(
            self.daemon.socket(),
            search_rpc::METHOD_HEALTH,
            json!({}),
            PROBE_TIMEOUT,
        )
        .await
        .expect("the stand-in daemon must answer — the fixture's premise");
        // The self-check's own call does not count against the assertion.
        self.calls.store(0, Ordering::SeqCst);
    }

    fn seen(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

/// The regression test for #4743: a full decommission from a test process must
/// put nothing on the wire, even though a daemon is discoverable and willing.
///
/// Why serial: the daemon-home override is process-global.
/// What: reproduces the issue's own fixture — an SM-owned workspace whose
/// basename is `full`, so `disposable_workspace_index_id` yields the index id
/// `full` and `decommission_with_root` reaches its effect-4 index delete. Points
/// daemon resolution at a daemon that would accept the delete, decommissions for
/// real, and asserts it was called zero times. Reverting
/// `DestructiveIndexDelete::acquire`'s `running_under_test_harness` branch makes
/// this fail with a count of 1.
/// Test: this function IS the test.
#[serial_test::serial]
#[tokio::test]
async fn decommission_issues_no_request_to_a_live_daemon_under_test() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;

    let managed_root = crate::test_support::hermetic_temp_dir();
    // The issue's own fixture name. A bare `file_name()` becomes the index id.
    let workspace_path = managed_root.path().join("owner").join("repo").join("full");
    std::fs::create_dir_all(&workspace_path).expect("create the fixture workspace");

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

    let (_home, _override) = isolated_daemon_home(false);
    let daemon = CountingDaemon::start().await;
    daemon.self_check().await;

    let outcome = mgr
        .decommission_with_root(&record.id, managed_root.path(), None)
        .await;

    let (tombstone, _removed) = outcome.expect("full decommission");
    assert_eq!(
        tombstone.state,
        ManagedSessionState::Decommissioned,
        "the guard must not change what decommission does to the record"
    );
    assert_eq!(
        daemon.seen(),
        0,
        "a test process must reach the trusty-search daemon zero times — this delete \
         carries the delete_data opt-in and destroys a real index named `full` (#4743)"
    );
}

/// The same guarantee for the second destructive site, which before #4743 had
/// no guard of its own at all.
///
/// Why separate: `sweep_orphaned_search_indexes` built its own destructive
/// request in a loop rather than calling the decommission-time helper, so a
/// guard placed only on that helper would have left this path fully exposed.
/// Asserting on the sweep directly is what keeps the two from diverging again.
/// What: with a willing daemon resolvable, a non-dry-run sweep must return empty
/// having made no call — not even the index listing, which the capability is
/// acquired ahead of.
/// Test: this function IS the test.
#[serial_test::serial]
#[tokio::test]
async fn sweep_makes_no_request_under_a_test_harness() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;

    let (_home, _override) = isolated_daemon_home(false);
    let daemon = CountingDaemon::start().await;
    daemon.self_check().await;

    let swept = mgr.sweep_orphaned_search_indexes(false).await;

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

/// No daemon is a no-op, not a failure and not a sweep of nothing.
///
/// Why: the orphan-GC loop calls this every 60 s on every machine, including
/// ones with no trusty-search installed. Before #6285 the absent discovery file
/// answered that question; now the resolver returns a path whether or not
/// anything is bound to it, so the no-op has to come from the unanswered call.
/// What: dry-run, so no capability is needed and the read path really runs, with
/// resolution pointed at a data directory holding no socket.
/// Test: this function IS the test.
#[serial_test::serial]
#[tokio::test]
async fn sweep_orphaned_search_indexes_noop_when_daemon_unreachable() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;

    let (_home, _override) = isolated_daemon_home(false);
    let swept = mgr
        .sweep_orphaned_search_indexes(true)
        .await
        .expect("an unreachable daemon is a no-op, never an error");

    assert!(
        swept.is_empty(),
        "nothing is collectable when nothing answered: {swept:?}"
    );
}

/// The index id both halves of the status-probe test use.
const ORPHAN_ID: &str = "gone-session";

/// A daemon that lists exactly one `.worktrees`-rooted index and answers
/// `status` for it — or refuses the status call when `status` is `None`.
fn one_index_daemon(
    root: PathBuf,
    status: Option<Value>,
) -> impl Fn(&str, Value) -> MockFuture + Send + Sync {
    move |method, _params| {
        let answer = if method == search_rpc::METHOD_INDEXES_LIST {
            Ok(json!({
                "indexes": [{ "id": ORPHAN_ID, "root_path": root.to_string_lossy() }]
            }))
        } else if method == search_rpc::METHOD_INDEX_STATUS {
            match &status {
                Some(body) => Ok(body.clone()),
                None => Err(RpcError::new(
                    crate::daemon::error::CODE_UNAVAILABLE,
                    "index status unavailable",
                )),
            }
        } else {
            Err(RpcError::new(
                -32601,
                format!("unexpected method: {method}"),
            ))
        };
        Box::pin(async move { answer })
    }
}

/// Run one dry-run sweep against a daemon that lists `ORPHAN_ID` rooted at
/// `root` and answers `status` for it.
///
/// Why a fresh [`isolated_daemon_home`] per call: the socket path is derived
/// from the override, and a bound socket file outlives the daemon that bound it
/// — two daemons in one home would collide on the second bind.
async fn dry_sweep_against(
    mgr: &super::manager::SessionManager,
    root: PathBuf,
    status: Option<Value>,
) -> Vec<String> {
    let (_home, _override) = isolated_daemon_home(false);
    let socket = search_rpc::search_socket().expect("derive the trusty-search socket");
    let _daemon = uds_mock::spawn_at(socket, one_index_daemon(root, status)).await;
    mgr.sweep_orphaned_search_indexes(true)
        .await
        .expect("a dry-run sweep against an answering daemon must not error")
}

/// A status probe that failed must not resolve to a collectable index (#6285).
///
/// Why: `probe_chunk_count` used to answer `0` for a failed probe, and `0` is
/// precisely the reading that makes an unclaimed `.worktrees` index collectable.
/// A wedged daemon, a timed-out call, or a one-off refusal therefore looked
/// exactly like "this index is empty" — the sweep deleted on evidence it never
/// gathered. An unreachable daemon must never read as an absent index.
/// What: one fixture, driven twice against the SAME candidate — a `.worktrees`
/// root that no longer exists on disk, which `is_orphan_index` reclaims at any
/// chunk count. With the status probe refused the sweep must report nothing;
/// with it answered the sweep must report the id. The second half is what proves
/// the first is the probe failing rather than the fixture never qualifying.
/// Dry-run throughout: the decision is what is under test, and a test process
/// could not acquire the delete capability anyway (#4743).
/// Test: this function IS the test.
#[serial_test::serial]
#[tokio::test]
async fn sweep_skips_a_candidate_whose_status_probe_failed() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;

    // Never existed on disk: an unclaimed worktree index whose root is gone is
    // an orphan at any chunk count, so only the probe's outcome can change the
    // verdict between the two halves below.
    let vanished_root = PathBuf::from("/nonexistent/owner/repo/.worktrees/gone-session");

    let skipped = dry_sweep_against(&mgr, vanished_root.clone(), None).await;
    assert!(
        skipped.is_empty(),
        "an index whose chunk count could not be read must not be collectable: {skipped:?}"
    );

    let collected = dry_sweep_against(&mgr, vanished_root, Some(json!({ "chunk_count": 0 }))).await;
    assert_eq!(
        collected,
        vec![ORPHAN_ID.to_string()],
        "fixture premise: the same candidate IS collectable once the daemon answers"
    );
}
