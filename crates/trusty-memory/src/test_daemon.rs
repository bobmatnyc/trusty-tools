//! A real daemon on a temp socket, for this crate's own tests (#6286).
//!
//! Why: several in-crate tests need to prove a caller and the daemon agree —
//! the hook emit, the `note` CLI, the palace listing — and each one's unit test
//! can pass while the two disagree about a method name or a params shape. Each
//! rig standing up its own bind-and-poll loop is the duplication the
//! workspace's common-entry-point rule exists to prevent, and it is easy to get
//! subtly wrong: a fixed sleep instead of a readiness poll is either flaky or
//! slow.
//!
//! What: [`TestDaemon::start`] seeds the mock embedder, builds an `AppState`
//! over a leaked tempdir, serves it on a temp socket through the real
//! [`crate::transport::uds::serve_with_shutdown`], and polls until the socket
//! answers. Dropping the handle stops the accept loop.
//!
//! This never touches the operator's data directory or the live daemon: both
//! the data root and the socket live under `TempDir`s this module creates.
//!
//! This is a `#[cfg(test)]` module, so it never ships.
//!
//! Test: every caller — `crate::hook_emit::tests`,
//! `crate::transport::methods::palaces` coverage in `transport::uds::tests`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::sync::oneshot;

use crate::AppState;

/// A running daemon on a temp socket, and the handle that stops it.
pub struct TestDaemon {
    socket: PathBuf,
    state: AppState,
    stop: Option<oneshot::Sender<()>>,
}

impl TestDaemon {
    /// Bind a temp socket, serve a fresh state on it, and wait until it
    /// answers.
    ///
    /// # Panics
    ///
    /// When a tempdir cannot be created — a test-only failure with no recovery.
    pub async fn start() -> Self {
        Self::start_with(new_test_state()).await
    }

    /// [`TestDaemon::start`] over a caller-built state.
    ///
    /// Why: a test that pre-seeds palaces needs the state before it is served,
    /// and the same handle afterwards to flush or assert against it.
    ///
    /// # Panics
    ///
    /// As [`TestDaemon::start`].
    pub async fn start_with(state: AppState) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir for the test socket");
        let socket = tmp.path().join("sockets").join("trusty-memory.sock");
        // Leaked so the directory outlives this handle without it owning the
        // `TempDir`; the process reaps it.
        std::mem::forget(tmp);

        let (stop, shutdown) = oneshot::channel::<()>();
        let serve_socket = socket.clone();
        let serve_state = state.clone();
        tokio::spawn(async move {
            let _ = crate::transport::uds::serve_with_shutdown(serve_state, &serve_socket, async {
                let _ = shutdown.await;
            })
            .await;
        });

        // Poll rather than sleep a fixed interval: the bind is fast but a
        // loaded machine is not, and a fixed wait is either flaky or slow.
        for _ in 0..200 {
            if trusty_common::uds::socket_is_serving(&socket, Duration::from_millis(200)).await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        Self {
            socket,
            state,
            stop: Some(stop),
        }
    }

    /// The path a client under test should dial.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The state this daemon serves, for flushing and direct assertions.
    pub fn state(&self) -> &AppState {
        &self.state
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

/// A ready `AppState` over a leaked tempdir.
///
/// Seeds the process-wide `retrieval::shared_embedder()` cell with the mock:
/// under `cargo nextest run` each test gets a virgin cell and would otherwise
/// reach for the real ONNX model and fail on the HuggingFace download (the
/// #4413 defect class). `seed_shared_embedder_with_mock` is idempotent, so
/// calling it from the one fixture every test uses is free and
/// order-independent.
///
/// # Panics
///
/// When a tempdir cannot be created.
pub fn new_test_state() -> AppState {
    trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();
    let tmp = tempfile::tempdir().expect("tempdir for the test data root");
    let root = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    // #88: bypass the project-slug enforcement gate so a test can create a
    // palace without a real project root on disk.
    // SAFETY: every test in this process wants the same idempotent "1".
    unsafe {
        std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
    }
    let state = AppState::new(root);
    // #911: flip past the warming preflight so handlers run.
    state.set_ready();
    state
}
