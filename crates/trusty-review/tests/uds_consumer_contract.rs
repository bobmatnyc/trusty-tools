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

use trusty_console::connector::{ServiceConnector as _, ServiceStatus};
use trusty_console::detect::ReviewConnector;
use trusty_installer::commands::probe_http::{ProbeOutcome, probe_daemon_http};
use trusty_review::service::{AppState, rpc};

mod fakes;

// Both tests mutate `TRUSTY_DATA_DIR_OVERRIDE` and `PATH` — process-global
// state a parallel sibling in this binary would see — so both carry
// `#[serial]`. A `std::sync::Mutex` guard would be held across an `.await`,
// which clippy refuses and which can deadlock a current-thread runtime.

/// Point the data-dir resolver at `root` and put a stub `trusty-review` on
/// `PATH`.
///
/// Why: `ReviewConnector::detect` short-circuits to `Absent` when the binary is
/// not installed, which on a CI runner it is not. The stub is never executed —
/// only `which` looks at it — so it can be an empty executable file.
///
/// # Safety
///
/// Both writes are process-global. Every caller is `#[serial]`.
unsafe fn point_environment_at(root: &Path) -> PathBuf {
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

    let path = std::env::var("PATH").unwrap_or_default();
    unsafe {
        std::env::set_var("PATH", format!("{}:{path}", bin.display()));
        std::env::set_var("TRUSTY_DATA_DIR_OVERRIDE", root);
    }
    path.into()
}

/// # Safety
///
/// Process-global; every caller is `#[serial]`.
unsafe fn restore_environment(original_path: &Path) {
    unsafe {
        std::env::set_var("PATH", original_path);
        std::env::remove_var("TRUSTY_DATA_DIR_OVERRIDE");
    }
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
    // SAFETY: this test is `#[serial]`.
    let original_path = unsafe { point_environment_at(tmp.path()) };

    // The daemon resolves its socket exactly as `serve` does, and both
    // consumers resolve it independently through the same shared entry point.
    let socket = rpc::socket_path().expect("resolve socket path");
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
    let _ = std::fs::remove_file(&socket);
    // SAFETY: this test is `#[serial]`.
    unsafe { restore_environment(&original_path) };
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
    // SAFETY: this test is `#[serial]`.
    let original_path = unsafe { point_environment_at(tmp.path()) };

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

    // SAFETY: this test is `#[serial]`.
    unsafe { restore_environment(&original_path) };
}
