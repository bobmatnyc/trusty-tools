//! The combined-PR proof for #6277: one live socket, both consumers agree.
//!
//! Why: the design review made the daemon's transport swap and its two
//! consumers ONE pull request, because a gap between them on `main` is the
//! #4246 false-`down` class. `tctl` reads a healthy trusty-review as down,
//! `verify_tail::needs_kickstart` turns that into `launchctl kickstart -k`, and
//! every `tctl install` hard-restarts a daemon that was working. Three crates
//! each testing their own half cannot catch that: the gap IS the disagreement
//! between them.
//!
//! So this test binds trusty-review's real router on a real socket and asks the
//! two real consumers — `trusty_console`'s `ReviewConnector` and
//! `trusty_installer`'s health probe — what they see. It is the highest level
//! the crate structure allows: the two consumers arrive as path-only
//! dev-dependencies, and nothing here spawns a binary from another crate.
//!
//! Path resolution runs through `TRUSTY_DATA_DIR_OVERRIDE`, deliberately: the
//! point is that all three sides derive the SAME path from the SAME entry
//! point. Pointing each at a socket by hand would prove the wire format and
//! silently skip the thing most likely to drift.
//!
//! Test: `both_consumers_see_a_live_uds_daemon_as_healthy`,
//! `both_consumers_see_an_absent_daemon_as_not_running`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use trusty_console::connector::{ServiceConnector as _, ServiceStatus};
use trusty_console::detect::ReviewConnector;
use trusty_installer::commands::probe_http::{ProbeOutcome, probe_daemon_http};
use trusty_review::service::{AppState, rpc};

mod fakes;

// Both tests mutate `TRUSTY_DATA_DIR_OVERRIDE` and `PATH` — process-global
// state a parallel sibling in this binary would see — so both carry
// `#[serial]`. A `std::sync::Mutex` guard would be held across an `.await`,
// which clippy refuses and which can deadlock a current-thread runtime.

/// Restores `PATH` and clears `TRUSTY_DATA_DIR_OVERRIDE` when dropped.
///
/// Why: a `#[serial]` sibling runs in the SAME process, so an env var one test
/// leaves behind is the next test's input. Restoring at the end of the test body
/// only runs when every assertion passed — exactly the case where cleanup
/// matters least. A panicking test would strand `TRUSTY_DATA_DIR_OVERRIDE`
/// pointing at a deleted `TempDir`, and the sibling would then resolve a socket
/// path under a directory that no longer exists and fail for a reason with
/// nothing to do with what it tests. `Drop` runs during unwind, so the guard
/// cleans up on both paths.
///
/// Test: used by both tests in this file.
struct EnvGuard {
    original_path: std::ffi::OsString,
}

impl EnvGuard {
    /// Point the data-dir resolver at `root` and put a stub `trusty-review` on
    /// `PATH`.
    ///
    /// Why: `ReviewConnector::detect` short-circuits to `Absent` when the binary
    /// is not installed, which on a CI runner it is not. The stub is never
    /// executed — only `which` looks at it — so an inert script is enough.
    fn point_at(root: &Path) -> Self {
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).expect("create stub bin dir");
        let stub = bin.join("trusty-review");
        std::fs::write(&stub, "#!/bin/sh\nexit 0\n").expect("write stub");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
                .expect("chmod stub");
        }

        let original_path = std::env::var_os("PATH").unwrap_or_default();
        // SAFETY: process-global, and every caller is `#[serial]`.
        unsafe {
            std::env::set_var(
                "PATH",
                format!("{}:{}", bin.display(), original_path.to_string_lossy()),
            );
            std::env::set_var("TRUSTY_DATA_DIR_OVERRIDE", root);
        }
        Self { original_path }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: process-global, and every caller is `#[serial]`.
        unsafe {
            std::env::set_var("PATH", &self.original_path);
            std::env::remove_var("TRUSTY_DATA_DIR_OVERRIDE");
        }
    }
}

/// Unlinks a socket path when dropped, so a panicking assertion does not leave
/// the file behind for a later run to trip over.
struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Poll until something is serving `socket`, or panic after a bounded wait.
///
/// Why: the bind happens before the accept loop is spawned, so a connect already
/// succeeds and the consumers below are not racing TODAY. This exists so they
/// cannot START racing: a future refactor that moves the bind inside the spawned
/// task — which is exactly the shape `rpc_unlinks_its_socket_on_shutdown` uses —
/// would otherwise turn both tests intermittently red for a reason that has
/// nothing to do with what they assert.
/// What: `socket_is_serving` on a 10 ms poll, bounded at two seconds.
async fn wait_until_serving(socket: &Path) {
    for _ in 0..200 {
        if trusty_common::uds::socket_is_serving(socket, Duration::from_millis(200)).await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("nothing came up on {}", socket.display());
}

/// REGRESSION (#6277, #4246): with trusty-review serving its socket, BOTH the
/// console connector and the `tctl` probe must report it healthy.
///
/// Why: this is the assertion the combined PR exists to make. If either
/// consumer is left on the retired TCP port, it reads `Refused` — and `Refused`
/// is one of the two variants `ProbeOutcome::is_confirmed_down` accepts, which
/// is what authorises `launchctl kickstart -k` against a running daemon.
/// What: binds the real `service::rpc` router on the derived path, then calls
/// each consumer's own entry point and asserts the verdict AND the version each
/// read off the same envelope.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn both_consumers_see_a_live_uds_daemon_as_healthy() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let _env = EnvGuard::point_at(tmp.path());

    // The daemon resolves its socket exactly as `serve` does, and both
    // consumers resolve it independently through the same shared entry point.
    let socket = rpc::socket_path().expect("resolve socket path");
    let _socket_guard = SocketGuard(socket.clone());
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");

    let state: AppState = fakes::healthy_state();
    let router = Arc::new(rpc::build_router(state));
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let served = tokio::spawn(async move {
        trusty_common::uds::server::serve_until(
            &listener,
            router,
            trusty_common::uds::server::RpcServeOptions::default(),
            async {
                let _ = shutdown_rx.await;
            },
        )
        .await;
    });

    wait_until_serving(&socket).await;

    // ── Consumer 1: trusty-console's dashboard connector ─────────────────────
    let info = tokio::task::spawn_blocking(|| ReviewConnector::new().detect())
        .await
        .expect("detect");
    assert_eq!(
        info.status,
        ServiceStatus::Running,
        "the console must see a live UDS daemon as Running, not Available — \
         Available is what a stale TCP probe would report"
    );
    assert_eq!(
        info.version.as_deref(),
        Some(env!("CARGO_PKG_VERSION")),
        "the console renders this on the service card, so it must come off the \
         live envelope rather than a placeholder"
    );

    // ── Consumer 2: tctl's health probe ──────────────────────────────────────
    let outcome = probe_daemon_http("trusty-review", "trusty-review").await;
    match &outcome {
        ProbeOutcome::Serving { status, version } => {
            assert_eq!(status, "ok");
            assert_eq!(version.as_deref(), Some(env!("CARGO_PKG_VERSION")));
        }
        other => panic!("tctl must see a live UDS daemon as Serving, got {other:?}"),
    }
    assert_eq!(outcome.health_string(), "healthy");
    assert!(
        !outcome.is_confirmed_down(),
        "a live daemon must never satisfy the gate that authorises \
         `launchctl kickstart -k` (#4246)"
    );

    let _ = shutdown_tx.send(());
    served.await.expect("join");
    // `SocketGuard` and `EnvGuard` clean up on the way out, panic or not.
}

/// Why: the mirror of the case above, and the one that keeps the first honest.
/// A probe that answered `Serving` unconditionally would pass that test; this
/// one fails it. It also pins the asymmetry `tctl` depends on — an absent
/// socket IS confirmed-down, because nothing accepted the connection, which is
/// the only observation that may authorise a repair.
/// What: with no daemon bound, the console reports `Available` (installed, not
/// running) and the probe reports `Refused`.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn both_consumers_see_an_absent_daemon_as_not_running() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let _env = EnvGuard::point_at(tmp.path());

    let info = tokio::task::spawn_blocking(|| ReviewConnector::new().detect())
        .await
        .expect("detect");
    assert_eq!(
        info.status,
        ServiceStatus::Available,
        "the binary is on PATH but nothing is serving, so it is Available"
    );

    let outcome = probe_daemon_http("trusty-review", "trusty-review").await;
    assert_eq!(outcome, ProbeOutcome::Refused, "got {outcome:?}");
    assert!(outcome.is_confirmed_down());
    // `EnvGuard` restores the environment on the way out, panic or not.
}
