//! Concurrency proofs for the BM25 spawn supervisor's `spawn_gate`.
//!
//! Why: the two defects `spawn_gate` closes are both races, and neither is
//! reachable from a serial `for … .await` loop on a `current_thread` runtime —
//! against such a test, deleting `spawn_gate` entirely leaves everything green.
//! Both tests below fail when it is removed:
//!
//! - `concurrent_callers_for_one_palace_spawn_exactly_one_daemon` asserts
//!   `spawned_count() == 1`. Without the gate, all N callers pass the
//!   already-supervised check against an empty map and each launches a daemon;
//!   the losers die on EADDRINUSE, so the map still holds one entry — which is
//!   exactly why the assertion is on launches, not on map size.
//! - `a_concurrent_fanout_never_exceeds_the_cap` asserts the resident
//!   population lands at the cap. Without the gate every caller evaluates
//!   `enforce_limits` against a map nobody has inserted into yet, so the cap is
//!   satisfied per-caller and violated in aggregate — the same shape as the
//!   unbounded fan-out that 503-stormed trusty-search.
//!
//! What: NOT `#[ignore]`d. `env!("CARGO_BIN_EXE_trusty-bm25-daemon")` makes
//! cargo build the daemon before this test binary runs, so the binary is always
//! on disk in CI — the reason the older e2e files skip themselves does not
//! apply here. Runs on a `multi_thread` runtime with a `JoinSet` so the callers
//! are genuinely simultaneous rather than interleaved by a single executor.
//!
//! Every test is `#[serial]`. The concurrency each one exercises is INTERNAL —
//! simultaneous callers into one supervisor — and none of it needs a sibling
//! test running alongside. What sharing the binary does provide is
//! interference: these tests spawn real daemons, mutate process-global env, and
//! resolve socket paths from one `$TMPDIR`.
//! `a_child_behind_a_stale_socket_file_is_evicted_and_respawned` failed 3 runs
//! in 5 when parallel and 0 in 3 when serial, so the isolation is load-bearing,
//! not decorative. This serialises the file; it removes no coverage.
//!
//! Test: this *is* the test file.

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinSet;
use trusty_memory::bm25_supervisor::{Bm25Supervisor, ENV_EXTERNAL_BM25};

/// Point the supervisor's binary locator at the daemon cargo just built.
///
/// Why: `locate_bm25_daemon_binary` searches PATH and `current_exe()`'s
/// siblings, neither of which reliably holds the daemon when the test binary
/// runs from `target/*/deps/`. `CARGO_BIN_EXE_*` is resolved at compile time
/// and cargo guarantees the binary exists before the test runs.
/// What: sets `TRUSTY_BM25_DAEMON_BIN` and clears external mode. Also pins the
/// spawned daemons' write window at 10 minutes (#5085): the batch worker
/// otherwise flushes its snapshot ~50 ms after a write, which would let
/// `an_evicted_live_child_is_given_a_chance_to_flush` pass on the periodic
/// flush rather than on the shutdown flush it exists to prove. Ten minutes
/// outlasts any run of this binary, so only a graceful SIGTERM can produce the
/// snapshot. The other tests here never assert on flushing, so a long window is
/// inert for them. Process-global and set identically by every test in this
/// binary, so there is nothing to race.
/// Test: used by every test below.
fn arm() {
    // SAFETY: test-only env mutation. Every test in this binary writes the
    // same values, so concurrent execution cannot observe a torn state.
    unsafe {
        std::env::set_var(
            "TRUSTY_BM25_DAEMON_BIN",
            env!("CARGO_BIN_EXE_trusty-bm25-daemon"),
        );
        std::env::set_var("TRUSTY_BM25_WRITE_WINDOW_MS", "600000");
        std::env::remove_var(ENV_EXTERNAL_BM25);
    }
}

/// Short, collision-free palace name.
///
/// Why: the socket path is `$TMPDIR/trusty-bm25-<palace>.sock` and macOS'
/// `$TMPDIR` is already ~50 bytes, so a long palace name blows past the
/// kernel's `sun_path` limit and the bind fails for reasons unrelated to the
/// behaviour under test.
/// What: a two-character prefix plus the low bits of the pid.
fn palace(prefix: &str, n: usize) -> String {
    format!("{prefix}{:x}{n}", std::process::id() & 0xfff)
}

/// Remove any socket left behind by a previous crashed run.
fn clear_socket(name: &str) {
    let _ = std::fs::remove_file(trusty_common::bm25_client::socket_path_for_palace(name));
}

/// Why: the double-spawn race. Two callers hitting `ensure_running` for the
/// same palace at the same moment both find an empty map, and without the
/// spawn gate both launch a daemon — the second's `bind` fails, its process
/// dies, and the map still ends up with one entry, so nothing downstream
/// notices. This asserts on LAUNCHES, which is the only figure that separates
/// the two implementations.
/// What: eight genuinely-simultaneous callers on a multi-thread runtime, all
/// for one palace. Exactly one daemon may be launched and all eight must get
/// the same socket back.
/// Test: this test itself. Remove `let _spawn = self.spawn_gate.lock().await;`
/// from `ensure_running` and this reads 8, not 1.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_callers_for_one_palace_spawn_exactly_one_daemon() {
    arm();
    let name = palace("s", 0);
    clear_socket(&name);
    let tmp = tempfile::tempdir().expect("tempdir");
    let sup = Arc::new(Bm25Supervisor::with_limits(8, None));

    // A barrier makes the eight calls simultaneous rather than merely
    // concurrent — without it the first task can finish its whole spawn
    // before the eighth is scheduled, which is the serial case again.
    let gate = Arc::new(tokio::sync::Barrier::new(8));
    let mut set = JoinSet::new();
    for _ in 0..8 {
        let sup = Arc::clone(&sup);
        let gate = Arc::clone(&gate);
        let dir = tmp.path().to_path_buf();
        let name = name.clone();
        set.spawn(async move {
            gate.wait().await;
            sup.ensure_running(&name, &dir).await
        });
    }

    let mut sockets = Vec::new();
    while let Some(joined) = set.join_next().await {
        sockets.push(
            joined
                .expect("task must not panic")
                .expect("ensure_running"),
        );
    }

    assert_eq!(sockets.len(), 8);
    assert!(
        sockets.windows(2).all(|w| w[0] == w[1]),
        "every caller must be handed the same socket: {sockets:?}"
    );
    assert_eq!(
        sup.spawned_count(),
        1,
        "eight simultaneous callers for one palace must launch exactly one \
         daemon — a higher count means the spawn gate did not serialise them"
    );
    assert_eq!(
        sup.supervised_count().await,
        1,
        "and exactly one child may be owned"
    );

    sup.shutdown().await;
    clear_socket(&name);
}

/// Why: the aggregate-cap violation. A cross-palace fan-out has every caller
/// evaluating the cap against a map nobody has inserted into yet, so each one
/// individually concludes there is room and the population lands at N rather
/// than at the cap. This is the failure the gate exists for, and a serial loop
/// cannot produce it.
/// What: six simultaneous callers for six distinct palaces against a cap of
/// two. The resident population must land at the cap, not at six.
/// Test: this test itself. Remove the spawn gate and this reads 6, not 2.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_concurrent_fanout_never_exceeds_the_cap() {
    arm();
    const FANOUT: usize = 6;
    const CAP: usize = 2;

    let names: Vec<String> = (0..FANOUT).map(|i| palace("f", i)).collect();
    for n in &names {
        clear_socket(n);
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let sup = Arc::new(Bm25Supervisor::with_limits(CAP, None));

    let gate = Arc::new(tokio::sync::Barrier::new(FANOUT));
    let mut set = JoinSet::new();
    for name in &names {
        let sup = Arc::clone(&sup);
        let gate = Arc::clone(&gate);
        let dir = tmp.path().join(name);
        let name = name.clone();
        set.spawn(async move {
            gate.wait().await;
            sup.ensure_running(&name, &dir).await
        });
    }
    while let Some(joined) = set.join_next().await {
        joined
            .expect("task must not panic")
            .expect("ensure_running must succeed for every palace");
    }

    let live = sup.supervised_count().await;
    assert!(
        live <= CAP,
        "a {FANOUT}-way simultaneous fan-out left {live} daemons resident \
         against a cap of {CAP} — the cap was satisfied per-caller and \
         violated in aggregate"
    );
    assert_eq!(
        live, CAP,
        "the cap should be saturated, not undershot: {live}"
    );
    assert_eq!(
        sup.reaped_count(),
        (FANOUT - CAP) as u64,
        "every daemon above the cap must be reaped, and counted"
    );

    sup.shutdown().await;
    for n in &names {
        clear_socket(n);
    }
}

/// Why: `lookup_live` evicts a child that has exited and lets `ensure_running`
/// fall through to a fresh spawn. That restart path had no test at all — the
/// supervisor's doc comment cited one that did not exist.
/// What: spawns a daemon, kills it outside the supervisor's knowledge, removes
/// the orphaned socket file so the adoption path cannot mask the restart, then
/// calls `ensure_running` again and asserts a second launch happened.
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dead_child_is_evicted_and_respawned() {
    arm();
    let name = palace("d", 0);
    clear_socket(&name);
    let tmp = tempfile::tempdir().expect("tempdir");
    let sup = Bm25Supervisor::with_limits(4, None);

    let socket = sup
        .ensure_running(&name, tmp.path())
        .await
        .expect("first spawn");
    assert_eq!(sup.spawned_count(), 1);
    assert_eq!(sup.supervised_count().await, 1);

    // Kill the daemon behind the supervisor's back, the way a crash or an
    // out-of-band `kill` would.
    kill_daemon_for(&socket).await;

    let respawned = sup
        .ensure_running(&name, tmp.path())
        .await
        .expect("dead child must be evicted and a fresh daemon spawned");
    assert_eq!(respawned, socket);
    assert_eq!(
        sup.spawned_count(),
        2,
        "a dead child must be replaced by a NEW process, not silently reused"
    );
    assert_eq!(sup.supervised_count().await, 1);
    assert_eq!(
        sup.reaped_count(),
        0,
        "removing a corpse reclaims nothing and must not count as a reap"
    );

    sup.shutdown().await;
    clear_socket(&name);
}

/// Why (#5085): the deterministic half of `a_dead_child_is_evicted_and_respawned`.
/// That test kills the daemon and then races the kernel — `try_wait()` reports a
/// SIGKILLed child alive until it finishes tearing down, so for a window the
/// supervisor's fast path handed back a socket nothing was listening on. The
/// window is microseconds on an idle host (which is why 62 local runs found
/// nothing) and milliseconds on a contended CI runner. This test pins the SAME
/// `lookup_live` state — child un-reaped, socket unserved — without any timing
/// dependence, by unlinking the socket while the daemon keeps running. The
/// child is then alive by `try_wait()` FOREVER, so a supervisor that trusts
/// `try_wait()` alone fails this on every run rather than one in twelve.
///
/// It is also a real production state in its own right: a `$TMPDIR` reaper that
/// unlinks the socket leaves a daemon that is alive and permanently unreachable.
/// What: spawn, unlink the socket out from under the live daemon, call
/// `ensure_running` again, and require a second launch.
/// Test: this test itself. Revert `lookup_live` to a bare `try_wait()` check and
/// this reads 1, not 2.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_live_child_that_stopped_serving_is_evicted_and_respawned() {
    arm();
    let name = palace("u", 0);
    clear_socket(&name);
    let tmp = tempfile::tempdir().expect("tempdir");
    let sup = Bm25Supervisor::with_limits(4, None);

    let socket = sup
        .ensure_running(&name, tmp.path())
        .await
        .expect("first spawn");
    assert_eq!(sup.spawned_count(), 1);
    assert_eq!(sup.supervised_count().await, 1);

    // The daemon keeps running; only its socket goes away. `try_wait()` will
    // report this child alive indefinitely, so nothing here is a race.
    std::fs::remove_file(&socket).expect("unlink the live daemon's socket");

    let respawned = sup
        .ensure_running(&name, tmp.path())
        .await
        .expect("an unreachable daemon must be evicted and a fresh one spawned");
    assert_eq!(respawned, socket);
    assert_eq!(
        sup.spawned_count(),
        2,
        "a child that is no longer serving its socket must be replaced — \
         try_wait() reporting it un-reaped is not evidence that it is serving"
    );
    assert_eq!(sup.supervised_count().await, 1);
    assert_eq!(
        sup.reaped_count(),
        0,
        "evicting an unreachable child reclaims nothing and must not count as a reap"
    );

    sup.shutdown().await;
    clear_socket(&name);
}

/// Why (#5085): the two tests above unlink the socket, so `lookup_live` sees
/// ENOENT. The race in the field produces the OTHER `NotServing` errno — the
/// crashed daemon leaves its socket file on disk and connects get
/// ECONNREFUSED. Nothing exercised that variant end-to-end, so the eviction
/// path could have keyed on `NotFound` alone and still looked covered.
/// What: same shape as the test above, but the socket path is left occupied by
/// a stale file (bind, then drop the listener) instead of removed.
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_child_behind_a_stale_socket_file_is_evicted_and_respawned() {
    arm();
    let name = palace("r", 0);
    clear_socket(&name);
    let tmp = tempfile::tempdir().expect("tempdir");
    let sup = Bm25Supervisor::with_limits(4, None);

    let socket = sup
        .ensure_running(&name, tmp.path())
        .await
        .expect("first spawn");
    assert_eq!(sup.spawned_count(), 1);

    // Leave a socket file with nothing behind it: connects now get
    // ECONNREFUSED rather than ENOENT, which is what a real crash leaves.
    std::fs::remove_file(&socket).expect("unlink the live daemon's socket");
    let stale = tokio::net::UnixListener::bind(&socket).expect("bind a stale socket file");
    drop(stale);
    assert!(socket.exists(), "the stale socket file must be on disk");

    sup.ensure_running(&name, tmp.path())
        .await
        .expect("a daemon behind a stale socket must be evicted and respawned");
    assert_eq!(
        sup.spawned_count(),
        2,
        "ECONNREFUSED proves nothing is listening just as ENOENT does"
    );
    assert_eq!(sup.supervised_count().await, 1);

    sup.shutdown().await;
    clear_socket(&name);
}

/// Why (#5085 review): the eviction added for this issue is the first one in
/// the module that removes a child which is still ALIVE. Every other eviction
/// removes a corpse, so dropping the handle — and letting `kill_on_drop`
/// SIGKILL it — costs nothing. Here it costs data: BM25 acks a write on the
/// in-memory apply and only writes it out at the end of the batch window, and
/// `trusty-bm25-daemon`'s shutdown flush (which exists for this supervisor)
/// runs only on SIGTERM. A SIGKILLed evictee therefore drops acked documents
/// with nothing left to recover them from — the supervisor cannot reach
/// `mark_dirty`, so no repair sweep ever revisits the palace.
/// What: index a document, then evict the daemon while that write is still
/// inside the open window, and require the document to reach the evictee's own
/// snapshot. `arm()` pins the write window at 10 minutes so the periodic batch
/// flush provably cannot be what wrote it — only the shutdown flush can. The
/// replacement is spawned into a DIFFERENT data dir so it cannot write the
/// snapshot being asserted on.
/// Test: this test itself. Replace `reap_doomed` with a bare drop of the
/// handle and the snapshot never gains the document.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_evicted_live_child_is_given_a_chance_to_flush() {
    arm();
    let name = palace("g", 0);
    clear_socket(&name);
    let evictee_dir = tempfile::tempdir().expect("tempdir");
    let replacement_dir = tempfile::tempdir().expect("tempdir");
    let sup = Bm25Supervisor::with_limits(4, None);

    let socket = sup
        .ensure_running(&name, evictee_dir.path())
        .await
        .expect("first spawn");
    assert_eq!(sup.spawned_count(), 1);

    // Acked by the daemon, but held in the open write window — see `arm()`.
    trusty_common::bm25_client::Bm25Client::new(socket.clone())
        .index("doc-5085", "flush me before you evict me")
        .await
        .expect("index a document into the daemon we are about to evict");

    let snapshot = evictee_dir.path().join("bm25_index.json");
    assert!(
        !snapshot_contains(&snapshot, "doc-5085"),
        "precondition: the write must still be unflushed, or this test proves nothing"
    );

    // Make the daemon unreachable so `ensure_running` evicts it while alive.
    std::fs::remove_file(&socket).expect("unlink the live daemon's socket");
    sup.ensure_running(&name, replacement_dir.path())
        .await
        .expect("respawn");
    assert_eq!(sup.spawned_count(), 2);

    assert!(
        snapshot_contains(&snapshot, "doc-5085"),
        "the evicted daemon was killed without a chance to flush — an acked \
         document was lost. Snapshot at {}: {:?}",
        snapshot.display(),
        std::fs::read_to_string(&snapshot).ok()
    );

    sup.shutdown().await;
    clear_socket(&name);
}

/// Whether the BM25 snapshot on disk records `doc_id`.
fn snapshot_contains(snapshot: &std::path::Path, doc_id: &str) -> bool {
    std::fs::read_to_string(snapshot).is_ok_and(|s| s.contains(doc_id))
}

/// SIGKILL whatever is listening on `socket`, then remove the socket file.
///
/// Why: the supervisor must observe a dead child, not an adoptable socket. If
/// the file survives, `ensure_running`'s socket-adoption branch returns before
/// the eviction path is reached and the test proves nothing.
/// What: finds the pid via `lsof`, kills it, waits for the socket to stop
/// accepting, and unlinks the file.
async fn kill_daemon_for(socket: &std::path::Path) {
    let out = tokio::process::Command::new("lsof")
        .arg("-t")
        .arg(socket)
        .output()
        .await
        .expect("lsof must be available");
    let pids: Vec<i32> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect();
    assert!(!pids.is_empty(), "no process holds {}", socket.display());
    for pid in pids {
        // SAFETY: `kill` with a pid read from lsof; the kernel returns ESRCH
        // rather than misbehaving if the process is already gone.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
    for _ in 0..200 {
        if tokio::net::UnixStream::connect(socket).await.is_err() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let _ = std::fs::remove_file(socket);
}
