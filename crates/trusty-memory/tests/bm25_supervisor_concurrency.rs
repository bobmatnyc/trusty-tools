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
/// What: sets `TRUSTY_BM25_DAEMON_BIN` and clears external mode. Process-global
/// and set identically by every test in this binary, so there is nothing to
/// race.
/// Test: used by every test below.
fn arm() {
    // SAFETY: test-only env mutation. Every test in this binary writes the
    // same values, so concurrent execution cannot observe a torn state.
    unsafe {
        std::env::set_var(
            "TRUSTY_BM25_DAEMON_BIN",
            env!("CARGO_BIN_EXE_trusty-bm25-daemon"),
        );
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
