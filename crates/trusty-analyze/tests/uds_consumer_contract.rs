//! The combined-PR proof for #6287: one live socket, four consumers agree.
//!
//! Why: the design review made this daemon's transport swap and every crate
//! that dialled its port ONE pull request, because a gap between them on `main`
//! is the #4246 false-`down` class — `tctl` reads a healthy daemon as down,
//! `verify_tail::needs_kickstart` turns that into `launchctl kickstart -k`, and
//! every `tctl install` hard-restarts a daemon that was working. Four crates
//! each testing their own half cannot catch that: the gap IS the disagreement
//! between them.
//!
//! Four consumers dial `analyze.health` by literal, none with a Cargo edge on
//! this crate — `trusty-console`'s `AnalyzeConnector`, `tctl`'s health probe,
//! `tga`'s audit guard, and `trusty-audit`'s grounding guard. Each names the
//! method in a private constant whose doc comment points here. So this test
//! binds the real `service::rpc` router on the derived path and asks each
//! consumer's own public entry point what it sees. A literal that drifted from
//! [`trusty_analyze::service::METHOD_HEALTH`] answers `method_not_found`, which
//! every one of them reads as "not running" — which is what these assertions
//! catch.
//!
//! Path resolution runs through `TRUSTY_DATA_DIR_OVERRIDE`, deliberately: the
//! point is that all five sides derive the SAME path from the SAME entry point.
//! Pointing each at a socket by hand would prove the wire format and silently
//! skip the thing most likely to drift.
//!
//! Test: `every_consumer_sees_a_live_uds_daemon_as_healthy`,
//! `every_consumer_sees_an_absent_daemon_as_not_running`,
//! `the_deadline_code_trusty_review_copies_is_the_one_this_daemon_sends`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use trusty_analyze::core::{FactStore, ScipOverlayStore, TrustySearchClient};
use trusty_analyze::service::events::AnalyzerAppState;
use trusty_analyze::service::rpc;
use trusty_console::connector::{ServiceConnector as _, ServiceStatus};
use trusty_console::detect::AnalyzeConnector;
use trusty_installer::commands::probe_http::{probe_daemon_http, ProbeOutcome};

// Every test here mutates `TRUSTY_DATA_DIR_OVERRIDE` and `PATH` — process-global
// state a parallel sibling in this binary would see — so each carries
// `#[serial]`. A `std::sync::Mutex` guard would be held across an `.await`,
// which clippy refuses and which can deadlock a current-thread runtime.

/// The socket overrides each consumer reads before falling back to the derived
/// path.
///
/// Why they are CLEARED rather than set: a developer with either exported
/// points that consumer at their own running daemon, and the test would then
/// assert against a socket it never bound. Clearing them is what makes every
/// side fall through to `TRUSTY_DATA_DIR_OVERRIDE`, which is the agreement
/// under test.
const SOCKET_OVERRIDES: [&str; 2] = [
    // tga's `audit::AnalyzeGuard`.
    "PR_INTELLIGENCE_ANALYZER_SOCKET",
    // trusty-audit's `grounding::daemons`.
    "TRUSTY_ANALYZE_SOCKET",
];

/// Restores `PATH` and clears the data-dir and socket overrides when dropped.
///
/// Why `Drop` rather than cleanup at the end of the test body: the body only
/// reaches its end when every assertion passed — exactly the case where cleanup
/// matters least. A panicking test would strand `TRUSTY_DATA_DIR_OVERRIDE`
/// pointing at a deleted `TempDir`, and the `#[serial]` sibling would resolve a
/// socket under a directory that no longer exists and fail for a reason with
/// nothing to do with what it tests. `Drop` runs during unwind, so this cleans
/// up on both paths.
struct EnvGuard {
    original_path: std::ffi::OsString,
}

impl EnvGuard {
    /// Point the data-dir resolver at `root` and put a stub `trusty-analyze` on
    /// `PATH`.
    ///
    /// Why the stub: `AnalyzeConnector::detect` short-circuits to `Absent` when
    /// the binary is not installed, which on a CI runner it is not. Nothing
    /// executes it — only `which` looks at it on the healthy path — so an inert
    /// script is enough. `tga` and `trusty-audit` would spawn it if the daemon
    /// were absent, which is why the absent-daemon test below does not call
    /// them.
    fn point_at(root: &Path) -> Self {
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).expect("create stub bin dir");
        let stub = bin.join("trusty-analyze");
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
            for var in SOCKET_OVERRIDES {
                std::env::remove_var(var);
            }
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

/// A loopback stub answering `GET /health` with 200.
///
/// Why: `analyze.health` reports `status: "ok"` only when its own trusty-search
/// dependency is reachable, and both `tga` and `trusty-audit` refuse anything
/// short of `"ok"` — a degraded daemon serves an empty hotspot list, which
/// reads as "nothing complex" rather than as an outage. So the contract cannot
/// be asserted at all without a reachable search daemon.
/// What: serves `GET /health` from axum on `127.0.0.1:0`. axum rather than a
/// hand-written response because `TrustySearchClient` dials with
/// `http2_prior_knowledge()` — a raw HTTP/1.1 reply is never read, and the
/// daemon reports `degraded` with nothing to say why. trusty-search is still an
/// HTTP daemon; #6287 moved trusty-analyze's OWN transport, not the one it
/// consumes.
async fn spawn_search_stub() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind search stub");
    let addr = listener.local_addr().expect("stub addr");
    let stub = axum::Router::new().route(
        "/health",
        axum::routing::get(|| async {
            axum::response::Json(serde_json::json!({ "status": "ok" }))
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, stub).await.ok();
    });
    format!("http://{addr}")
}

/// Build the daemon's real state over stores in `dir`, pointed at `search_base`.
fn state_over(dir: &Path, search_base: &str) -> AnalyzerAppState {
    let facts = FactStore::open(&dir.join("facts.redb")).expect("open the facts store");
    let overlays =
        ScipOverlayStore::open(&dir.join("scip_overlays.redb")).expect("open the overlay store");
    AnalyzerAppState::new(TrustySearchClient::new(search_base), facts, overlays)
}

/// Poll until something is serving `socket`, or panic after a bounded wait.
async fn wait_until_serving(socket: &Path) {
    for _ in 0..200 {
        if trusty_common::uds::socket_is_serving(socket, Duration::from_millis(200)).await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("nothing came up on {}", socket.display());
}

/// REGRESSION (#6287, #4246): with trusty-analyze serving its socket, all four
/// consumers must report it healthy.
///
/// Why: this is the assertion the combined PR exists to make. A consumer left
/// on the retired TCP port 7879 reads `Refused` — one of the two variants
/// `ProbeOutcome::is_confirmed_down` accepts, which is what authorises
/// `launchctl kickstart -k` against a running daemon — and a consumer whose
/// method literal drifted reads an error frame, which each of them treats as
/// "not healthy" for its own reason.
/// What: binds the real `service::rpc` router on the derived path, then calls
/// each consumer's own entry point.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn every_consumer_sees_a_live_uds_daemon_as_healthy() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let _env = EnvGuard::point_at(tmp.path());
    let search_base = spawn_search_stub().await;

    // The daemon resolves its socket exactly as `serve` does, and every consumer
    // resolves it independently through the same shared entry point.
    let socket = rpc::socket_path().expect("resolve socket path");
    let _socket_guard = SocketGuard(socket.clone());

    let state = state_over(tmp.path(), &search_base);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let serving = {
        let socket = socket.clone();
        tokio::spawn(async move {
            rpc::serve_with_shutdown(state, &socket, async {
                let _ = shutdown_rx.await;
            })
            .await
        })
    };

    wait_until_serving(&socket).await;

    // ── Consumer 1: trusty-console's dashboard connector ─────────────────────
    let info = tokio::task::spawn_blocking(|| AnalyzeConnector::new().detect())
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
    assert!(info.url.is_none(), "a UDS daemon has no URL to render");

    // ── Consumer 2: tctl's health probe ──────────────────────────────────────
    let outcome = probe_daemon_http("trusty-analyze", "trusty-analyze").await;
    match &outcome {
        ProbeOutcome::Serving { status, version } => {
            assert_eq!(status, "ok");
            assert_eq!(version.as_deref(), Some(env!("CARGO_PKG_VERSION")));
        }
        other => panic!("tctl must see a live UDS daemon as Serving, got {other:?}"),
    }
    assert!(
        !outcome.is_confirmed_down(),
        "a live daemon must never satisfy the gate that authorises \
         `launchctl kickstart -k` (#4246)"
    );

    // ── Consumer 3: tga's audit guard ────────────────────────────────────────
    // A healthy probe returns before the guard spawns anything, which is the
    // whole assertion: a drifted literal would send it spawning a second daemon
    // on top of the one already serving.
    let guard = tga::audit::AnalyzeGuard::from_env().expect("resolve the guard");
    assert_eq!(guard.socket, socket, "tga must derive the same path");
    tga::audit::ensure_analyze_daemon_with(&guard)
        .await
        .expect("tga must accept a live, search-reachable daemon");

    // ── Consumer 4: trusty-audit's grounding guard ───────────────────────────
    let mut tools = trusty_audit::grounding::Tools::pinned(
        PathBuf::from("trusty-search"),
        PathBuf::from("trusty-analyze"),
    );
    tools.search_url = search_base.clone();
    assert_eq!(
        tools.analyze_socket, socket,
        "trusty-audit must derive the same path"
    );
    trusty_audit::grounding::daemons::ensure_analyze(&tools)
        .await
        .expect("trusty-audit must accept a live, search-reachable daemon");

    let _ = shutdown_tx.send(());
    serving.await.expect("join").expect("serve cleanly");
    // `SocketGuard` and `EnvGuard` clean up on the way out, panic or not.
}

/// Why: the mirror of the case above, and the one that keeps the first honest.
/// A probe that answered `Serving` unconditionally would pass that test; this
/// one fails it. It also pins the asymmetry `tctl` depends on — an absent
/// socket IS confirmed-down, because nothing accepted the connection, which is
/// the only observation that may authorise a repair.
///
/// `tga` and `trusty-audit` are deliberately not called here: both answer an
/// absent daemon by SPAWNING one, and the binary on `PATH` in this test is an
/// inert stub. Their absent-daemon arms are covered in their own crates, by
/// `tga::audit::tests::an_analyze_daemon_that_never_comes_up_refuses_the_audit`
/// and `trusty_audit::grounding::grounding_tests::
/// an_analyze_daemon_that_will_not_start_is_a_named_gap`.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn every_consumer_sees_an_absent_daemon_as_not_running() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let _env = EnvGuard::point_at(tmp.path());

    let info = tokio::task::spawn_blocking(|| AnalyzeConnector::new().detect())
        .await
        .expect("detect");
    assert_eq!(
        info.status,
        ServiceStatus::Available,
        "the binary is on PATH but nothing is serving, so it is Available"
    );
    assert!(info.version.is_none(), "there is no envelope to read");

    let outcome = probe_daemon_http("trusty-analyze", "trusty-analyze").await;
    assert_eq!(outcome, ProbeOutcome::Refused, "got {outcome:?}");
    assert!(outcome.is_confirmed_down());
    // `EnvGuard` restores the environment on the way out, panic or not.
}

/// Why (#6287): `trusty-review`'s `report::analyze_adapter` copies this code as
/// a literal — it has no Cargo edge on an analysis daemon, and adding one to
/// share an `i64` would pull a tree-sitter engine into every
/// `trusty-review report` build. Its `classify_failure` matches on the code to
/// choose `EndpointFailure::TimedOut` over `Rejected`, which decides which
/// sentence the report's Gaps & Caveats section prints. Changing the code here
/// changes that sentence silently.
/// What: pins the value the copy asserts. `trusty-review`'s
/// `a_daemon_side_deadline_is_a_timeout_not_a_rejection` holds the other end,
/// and `rpc_diagnostics_reports_deadline_exceeded_distinctly` proves this
/// daemon actually sends it.
/// Test: this is the test.
#[test]
fn the_deadline_code_trusty_review_copies_is_the_one_this_daemon_sends() {
    assert_eq!(
        trusty_analyze::service::events::CODE_DEADLINE_EXCEEDED,
        -32005
    );
    assert_eq!(trusty_analyze::service::events::CODE_NOT_FOUND, -32004);
}
