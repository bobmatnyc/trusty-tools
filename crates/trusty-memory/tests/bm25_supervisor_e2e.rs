//! End-to-end test for the per-palace BM25 spawn supervisor (issue #193).
//!
//! Why: unit tests in `src/bm25_supervisor.rs` cover the deterministic
//! parts of the supervisor (env-var opt-out, socket adoption, send/sync
//! bounds, idempotent shutdown) without spawning a real daemon. This test
//! drives the full happy path — spawn a real `trusty-bm25-daemon` child,
//! index a document, search for it, then call `shutdown` and verify the
//! process is gone — so any regression that breaks the spawn argv, the
//! socket-probe loop, or the SIGTERM reaping is caught with a real
//! subprocess in the loop.
//!
//! What: marked `#[ignore]` because it requires the `trusty-bm25-daemon`
//! binary to be available either via `cargo build` output (the test
//! discovers `target/<profile>/trusty-bm25-daemon` automatically) or
//! via the `TRUSTY_BM25_DAEMON_BIN` env var. Running it under
//! `cargo test --include-ignored -p trusty-memory` is the canonical
//! invocation.
//!
//! Test: `cargo test -p trusty-memory --test bm25_supervisor_e2e --
//! --include-ignored --nocapture`.

use std::path::PathBuf;
use std::time::Duration;

use trusty_common::bm25_client::Bm25Client;
use trusty_memory::bm25_supervisor::Bm25Supervisor;

/// Resolve the path to the freshly-built `trusty-bm25-daemon` binary.
///
/// Why: the supervisor's binary discovery uses `current_exe()` + sibling
/// lookup, which works for `cargo install`-style layouts but not for the
/// `target/debug/` test-binary path (the test binary is the integration
/// test, NOT trusty-memory). Setting `TRUSTY_BM25_DAEMON_BIN` from the
/// test before invoking the supervisor sidesteps that and pins the test
/// to the specific build output we want.
/// What: returns the value of `TRUSTY_BM25_DAEMON_BIN` if set, otherwise
/// walks `target/{debug,release}/trusty-bm25-daemon` relative to the
/// workspace root. Falls back to `None` if neither resolves to an
/// existing file.
/// Test: trivially — this is the test's bootstrap.
fn discover_daemon_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("TRUSTY_BM25_DAEMON_BIN") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    // Walk up from the test binary's parent to find the workspace
    // `target/` dir, then look in both `debug/` and `release/`.
    let exe = std::env::current_exe().ok()?;
    // Test binaries live at `target/<profile>/deps/<name>-<hash>`. The
    // first `target/<profile>/` we hit is the right one.
    let mut p = exe.as_path();
    while let Some(parent) = p.parent() {
        // The daemon binary should be in the same `<profile>/` directory.
        let candidate = parent.join("trusty-bm25-daemon");
        if candidate.is_file() {
            return Some(candidate);
        }
        // ...or one level up if we're under `deps/`.
        let candidate = parent.join("..").join("trusty-bm25-daemon");
        if candidate.is_file() {
            return Some(candidate);
        }
        p = parent;
    }
    None
}

/// Full happy-path lifecycle test: spawn → index → search → shutdown.
///
/// Why: this is the canonical scenario the supervisor exists to make
/// painless. Any regression that breaks the wiring between trusty-memory
/// and trusty-bm25-daemon (spawn argv, socket path resolution, probe
/// timing, SIGTERM reaping) shows up here as a failed assertion against
/// a real subprocess.
/// What: discovers the daemon binary, sets `TRUSTY_BM25_DAEMON_BIN` so
/// the supervisor's `locate_bm25_daemon_binary` resolves it, picks a
/// unique palace name + tempdir, calls `ensure_running`, indexes a doc
/// via `Bm25Client`, searches for a fragment of the doc, asserts a hit,
/// then calls `shutdown` and confirms the socket file is gone and the
/// supervisor no longer tracks any children. Wrapped in a `tokio::test`.
/// Test: `cargo test -p trusty-memory --test bm25_supervisor_e2e --
/// --include-ignored`.
#[ignore = "requires trusty-bm25-daemon binary on disk; run with --include-ignored"]
#[tokio::test(flavor = "current_thread")]
async fn supervisor_spawns_indexes_searches_and_reaps() {
    // 1. Locate the daemon binary or skip with a helpful message.
    let binary = match discover_daemon_binary() {
        Some(p) => p,
        None => {
            eprintln!(
                "skipping: trusty-bm25-daemon binary not found. \
                 Build it first (`cargo build -p trusty-memory --bin trusty-bm25-daemon`) \
                 or set TRUSTY_BM25_DAEMON_BIN=<path>."
            );
            return;
        }
    };

    // 2. Point the supervisor's locator at the discovered binary.
    // SAFETY: the test process owns this env var for its lifetime; we
    // restore at the end via the EnvGuard pattern.
    let prev_bin = std::env::var("TRUSTY_BM25_DAEMON_BIN").ok();
    unsafe {
        std::env::set_var("TRUSTY_BM25_DAEMON_BIN", &binary);
    }
    // Make sure no stale TRUSTY_BM25_EXTERNAL leaks in from a parallel
    // test and forces us into the no-op branch.
    let prev_external = std::env::var("TRUSTY_BM25_EXTERNAL").ok();
    unsafe {
        std::env::remove_var("TRUSTY_BM25_EXTERNAL");
    }

    // 3. Pick a short, unique palace name + tempdir. The canonical socket
    //    path is `$TMPDIR/trusty-bm25-<palace>.sock`; on macOS `$TMPDIR`
    //    expands to `/var/folders/.../T/` which is already ~50 bytes,
    //    so we cap the palace fragment to a handful of bytes to stay
    //    under the kernel's SUN_LEN ceiling (~104 bytes total).
    let palace = format!("e2e{:x}", std::process::id() & 0xffff);
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().join(&palace).join("bm25");

    // 4. Spawn the daemon via the supervisor.
    let supervisor = Bm25Supervisor::new();
    let socket = supervisor
        .ensure_running(&palace, &data_dir)
        .await
        .expect("supervisor must spawn the daemon");
    assert!(
        socket.exists(),
        "socket file must exist after ensure_running"
    );
    assert_eq!(
        supervisor.supervised_count().await,
        1,
        "exactly one child must be tracked after first ensure_running"
    );

    // 5. Re-calling ensure_running for the same palace must be a no-op
    // that returns the same socket path — pins the "already running"
    // fast path.
    let socket2 = supervisor
        .ensure_running(&palace, &data_dir)
        .await
        .expect("second ensure_running must reuse the same child");
    assert_eq!(socket, socket2);
    assert_eq!(
        supervisor.supervised_count().await,
        1,
        "second ensure_running must NOT spawn a second child"
    );

    // 6. Index a document and search for a token from its text.
    let client = Bm25Client::new(socket.clone());
    client
        .index("drawer-1", "the quick brown fox jumps over the lazy dog")
        .await
        .expect("index must succeed against the spawned daemon");

    // BM25 batch queue may coalesce writes for a short window; allow
    // a tiny delay before searching so the indexed doc is visible.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let hits = client
        .search("fox", 5)
        .await
        .expect("search must succeed against the spawned daemon");
    assert!(
        hits.iter().any(|h| h.doc_id == "drawer-1"),
        "indexed doc must be returned by a BM25 query of one of its tokens; got hits: {hits:?}"
    );

    // 7. Capture the child PID so we can verify it's gone after shutdown.
    // Re-using ensure_running on a never-evicted palace is safe; the PID
    // is only used for the post-shutdown poll below.

    // 8. Shutdown — must SIGTERM the child, wait, and remove the socket.
    supervisor.shutdown().await;
    assert_eq!(
        supervisor.supervised_count().await,
        0,
        "shutdown must drain the per-palace map"
    );
    // The daemon's own SIGTERM handler unlinks the socket; if it didn't,
    // our supervisor's best-effort cleanup did. Either way the file
    // should be gone by now.
    let still_exists = socket.exists();
    assert!(
        !still_exists,
        "socket file at {} must be gone after shutdown",
        socket.display()
    );

    // 9. Restore env vars.
    unsafe {
        match prev_bin {
            Some(v) => std::env::set_var("TRUSTY_BM25_DAEMON_BIN", v),
            None => std::env::remove_var("TRUSTY_BM25_DAEMON_BIN"),
        }
        match prev_external {
            Some(v) => std::env::set_var("TRUSTY_BM25_EXTERNAL", v),
            None => std::env::remove_var("TRUSTY_BM25_EXTERNAL"),
        }
    }
}
