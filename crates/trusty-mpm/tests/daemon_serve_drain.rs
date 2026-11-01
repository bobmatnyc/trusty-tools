//! The daemon serves its RPC socket, and one shutdown drains it and leaves
//! every managed session running (#6288 slice 1; S3a acceptance A6; #10597).
//!
//! Why the `env_serial` target: the proof reassigns `$HOME` to a scratch
//! directory so `core::host_state_gate` classifies this process as a scratch
//! environment and the startup tmux/host-process adoption is skipped — without
//! that, booting a daemon inside the lib test binary would adopt the operator's
//! live Claude Code panes into a temp registry. `$HOME` is process-global, so it
//! cannot be reassigned while other tests run concurrently; `env_serial` runs
//! one test at a time, and [`EnvRestore`] hands the next test the environment
//! this one found (#8345). Same rationale as `scratch_home_tmux_gate.rs`.
//!
//! Why #10597 rewrote it: owner ruling D82 deleted the #1455 session stop, so
//! the drain now asserts the stop is gone and the shutdown report still runs.
//!
//! What: binds the socket, runs `serve_with_shutdown` with a oneshot in place
//! of SIGTERM on a current-thread runtime under a capturing `tracing`
//! subscriber, proves the socket answers, fires the shutdown, and asserts the
//! serve returned `Ok`, the socket file is gone, both left-running reports
//! logged, and the deleted `SessionManager::shutdown` pass did not.
//! Test: this file IS the test; run with
//! `cargo test -p trusty-mpm --test env_serial daemon_serve_drain::`.

#![cfg(feature = "daemon")]

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tempfile::TempDir;
use trusty_mpm::core::paths::FrameworkPaths;
use trusty_mpm::daemon::state::DaemonState;

/// Headroom for a drain on a loaded machine, not a latency budget.
const DRAIN_BUDGET: Duration = Duration::from_secs(30);

/// Sets environment variables and restores their prior values on drop (#8345).
///
/// Why: this test shares the `env_serial` process with other tests, so a
/// `$HOME` left pointing at a deleted scratch dir would change how the next
/// test's `host_state_gate` classifies the host.
struct EnvRestore(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl EnvRestore {
    fn set(pairs: &[(&'static str, &std::ffi::OsStr)]) -> Self {
        let prev = pairs
            .iter()
            .map(|(key, value)| {
                let prev = std::env::var_os(key);
                // SAFETY: `env_serial` runs one test at a time, so no other
                // thread reads or writes the environment while this runs.
                unsafe { std::env::set_var(key, value) };
                (*key, prev)
            })
            .collect();
        Self(prev)
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        for (key, prev) in self.0.drain(..) {
            // SAFETY: as in `set`.
            match prev {
                Some(v) => unsafe { std::env::set_var(key, v) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }
}

/// A `tracing` writer that appends every event to a shared buffer.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("capture lock")).into_owned()
    }
}

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("capture lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Send one JSON-RPC frame over `socket` and read the answer back.
async fn call_over_socket(
    socket: &std::path::Path,
    method: &str,
) -> Result<serde_json::Value, trusty_common::uds::UdsRpcError> {
    let request = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": method, "params": {},
    });
    trusty_common::uds::send_framed_request(socket, &request, Duration::from_secs(10)).await
}

/// A6: one shutdown drains the socket, unlinks it, and stops no session.
///
/// Why a current-thread runtime: `tracing::subscriber::with_default` is
/// thread-local, so every task `serve_with_shutdown` spawns must run on this
/// thread for its events to be captured. #10597: restoring the session stop
/// in `serve_with_shutdown` makes the log assertions fail.
/// Test: this function IS the test.
#[test]
fn serve_drains_the_socket_and_leaves_sessions_running_on_shutdown() {
    let scratch_home = TempDir::new().expect("scratch home");
    // `$HOME` puts `host_state_gate` into its scratch arm; the two flags switch
    // off the sweeps that would otherwise touch real repositories and
    // managed-session state on a developer machine. Declared after
    // `scratch_home`, so it restores `$HOME` before that directory is deleted.
    let _env = EnvRestore::set(&[
        ("HOME", scratch_home.path().as_os_str()),
        ("TRUSTY_MPM_ORPHAN_GC", "0".as_ref()),
        ("TRUSTY_MPM_INPROJECT_HYGIENE", "0".as_ref()),
    ]);

    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || writer.clone())
        .finish();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    let tmp = TempDir::new().expect("tempdir");
    let socket = tmp.path().join("sockets").join("trusty-mpm.sock");
    tracing::subscriber::with_default(subscriber, || {
        runtime.block_on(drive(tmp.path(), &socket));
    });

    assert!(
        !socket.exists(),
        "the socket file must be unlinked on the drain: {}",
        socket.display()
    );
    let logs = captured.text();
    // #10597: the stop-every-session pass (#1455) must stay deleted.
    assert!(
        !logs.contains("shutdown: no owned live managed sessions to stop"),
        "SessionManager::shutdown ran on the drain (#10597):\n{logs}"
    );
    assert!(
        logs.contains("graceful shutdown: managed sessions left running (#10597)"),
        "the drain must report the managed sessions it leaves running:\n{logs}"
    );
    assert!(
        logs.contains("graceful shutdown: left 0 legacy session(s) running"),
        "the legacy-registry pass must run on the drain (#9101):\n{logs}"
    );
}

/// Serve, prove the socket answers, then fire the one shutdown.
async fn drive(root: &std::path::Path, socket: &std::path::Path) {
    let paths = FrameworkPaths::under(root);
    std::fs::create_dir_all(&paths.hooks).expect("hooks dir");
    std::fs::create_dir_all(&paths.instructions).expect("instructions dir");
    std::fs::create_dir_all(&paths.agents).expect("agents dir");
    let state = Arc::new(DaemonState::with_paths(&paths));

    let rpc = trusty_mpm::daemon::socket::bind(socket)
        .await
        .expect("bind the rpc socket");
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let served = tokio::spawn(trusty_mpm::daemon::serve_with_shutdown(
        state,
        rpc,
        async move {
            let _ = stop_rx.await;
        },
    ));

    let answer = call_over_socket(socket, "mpm.health")
        .await
        .expect("the rpc listener must answer");
    assert_eq!(
        answer["result"]["status"],
        serde_json::json!("ok"),
        "mpm.health answers over the socket: {answer}"
    );

    stop_tx
        .send(())
        .expect("the serve task must still be running");
    tokio::time::timeout(DRAIN_BUDGET, served)
        .await
        .expect("the socket must drain on the one shutdown")
        .expect("the serve task must not panic")
        .expect("a requested drain is not an error");
}
