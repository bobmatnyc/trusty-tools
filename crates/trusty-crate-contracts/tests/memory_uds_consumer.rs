//! tctl's own probe against a live trusty-memory daemon (#6555).
//!
//! Why: every other arm of `crates/trusty-memory/tests/uds_consumer_contract.rs`
//! hands the daemon `json!({})` by hand, so none of them builds the frame `tctl`
//! actually sends. `probe_socket` sent no `params` at all, which decodes to
//! `Value::Null`, which `memory.health`'s `HealthQuery` refuses with `-32602`.
//! `tctl status` then rendered a healthy daemon as `down`, exited 2, and failed
//! `tctl install`'s verify tail.
//!
//! What: binds the real trusty-memory router on the DERIVED socket path — not a
//! temp one, because `tctl` takes no socket argument and resolves the path
//! itself — and calls `tctl`'s public entry point, which builds its own request
//! end to end.
//!
//! #8341: this arm used to live in trusty-memory, over a `trusty-installer`
//! `[dev-dependencies]` edge that added thirteen crates to every
//! `cargo test -p trusty-memory` and every
//! `cargo clippy -p trusty-memory --all-targets`. Here both crates are NORMAL
//! dependencies. The rest of that file stays where it is: it proves
//! trusty-memory against `trusty-common`, which trusty-memory already depends
//! on, so it welds nothing.
//!
//! Test: `tctl_probe_sees_a_live_uds_daemon_as_serving`.

use std::time::Duration;

use serde_json::json;
use tokio::sync::oneshot;
use trusty_common::memory_rpc::call_memory_tool_at_with_timeout;
use trusty_installer::commands::probe_http::{probe_daemon_http, ProbeOutcome};
use trusty_memory::AppState;

/// Block until `socket` ANSWERS a real request, not merely until it accepts one.
///
/// Why (#6667): a bare connect-and-close gate proves the listener exists; it
/// does not prove a client that dials can get a frame through. Two known
/// windows sit behind that weaker gate: `bind_hardened` binds BEFORE it chmods,
/// so a connect landing in between sees a socket a hardened dial would still
/// refuse (#6315), and a connect the server has not yet accepted has no peer to
/// write to.
/// What: polls `memory.health` through the shared client until one call
/// SUCCEEDS, which cannot be satisfied by a half-open socket. Panics at the
/// budget rather than letting the first assertion fail somewhere less legible.
/// Test: `tctl_probe_sees_a_live_uds_daemon_as_serving`.
async fn wait_until_answering(socket: &std::path::Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let mut last: Option<String> = None;
    while std::time::Instant::now() < deadline {
        match call_memory_tool_at_with_timeout(
            socket,
            "memory.health",
            json!({}),
            Duration::from_secs(1),
        )
        .await
        {
            Ok(_) => return,
            Err(e) => last = Some(format!("{e:#}")),
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "nothing answered memory.health on {} within the budget: {}",
        socket.display(),
        last.unwrap_or_else(|| "no error recorded".to_string())
    );
}

/// Points `resolve_data_dir` at a temp root, and clears the override on `Drop`.
///
/// Why (#6555): `probe_daemon_http` takes no socket path — it derives one
/// through `trusty_common::daemon_socket_path`, the entry point the daemon
/// binds through. So this test binds where `tctl` actually looks. `Drop` rather
/// than cleanup at the end of the body, because a panicking assertion would
/// otherwise strand the override pointing at a deleted directory.
/// What: sets and removes `TRUSTY_DATA_DIR_OVERRIDE`.
/// Test: `tctl_probe_sees_a_live_uds_daemon_as_serving`.
struct DataDirGuard;

impl DataDirGuard {
    fn point_at(root: &std::path::Path) -> Self {
        // SAFETY: process-global, and the only caller is `#[serial]`.
        unsafe { std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, root) };
        Self
    }
}

impl Drop for DataDirGuard {
    fn drop(&mut self) {
        // SAFETY: process-global, and the only caller is `#[serial]`.
        unsafe { std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV) };
    }
}

/// REGRESSION (#6555): `tctl`'s own probe must read a live daemon as `Serving`.
///
/// Why: see this file's module doc — the defect was a frame no other test
/// builds, and it rendered a healthy daemon as `down`.
/// What: binds the real router on the derived path and calls `tctl`'s public
/// entry point, which builds its own request end to end.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn tctl_probe_sees_a_live_uds_daemon_as_serving() {
    trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();

    let data = tempfile::tempdir().expect("tempdir");
    let root = data.path().to_path_buf();
    std::mem::forget(data);
    let _data_dir = DataDirGuard::point_at(&root);
    // #88: bypass the project-slug gate so a test can create a palace with no
    // real project root on disk.
    // SAFETY: every test in this process wants the same idempotent "1".
    unsafe {
        std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
    }

    let state = AppState::new(root);
    // #911: flip past the warming preflight so the health handler runs.
    state.set_ready();

    // The daemon binds, and `tctl` resolves, the SAME path from the SAME entry
    // point — which is what makes this a consumer contract rather than a wire
    // format test.
    let socket = trusty_common::daemon_socket_path("trusty-memory").expect("derive socket path");

    let (stop, shutdown) = oneshot::channel::<()>();
    let serve_socket = socket.clone();
    let serving = tokio::spawn(async move {
        trusty_memory::transport::uds::serve_with_shutdown(state, &serve_socket, async {
            let _ = shutdown.await;
        })
        .await
    });

    wait_until_answering(&socket).await;

    let outcome = probe_daemon_http("trusty-memory", "trusty-memory").await;
    assert!(
        matches!(outcome, ProbeOutcome::Serving { .. }),
        "got {outcome:?}"
    );

    let _ = stop.send(());
    let _ = serving.await;
}
