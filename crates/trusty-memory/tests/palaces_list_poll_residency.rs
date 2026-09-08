//! Issue #7125: a periodic poll must not open the whole estate to count it.
//!
//! Why: `memory.palaces_list` defaults `counts` to `true`, and counting opens
//! every palace on disk. The monitor dashboard polled it every 2 seconds and
//! trusty-mpm's health TUI every 5, both sending `{}` — so on the ~94-palace
//! estate that prompted the issue each tick hydrated the whole estate and left
//! the registry's 64-slot LRU at its ceiling, a multi-GB floor for as long as
//! the poller ran. Nothing in either client's own tests could see that: the
//! rows they project are identical either way. The registry's open-handle count
//! is the observable that separates the two.
//!
//! What: seeds an estate of closed palaces, serves a real daemon on a temp
//! socket, and drives it through the SHARED monitor client — the production
//! caller, not a hand-built params object — asserting the registry is still
//! empty afterwards. The second test pins the daemon-side half directly, so a
//! future caller that sends `counts: true` is diagnosed as the caller's fault
//! rather than the method's.
//! Test: this IS the test module.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::sync::oneshot;
use trusty_common::memory_core::palace::{Palace, PalaceId};
use trusty_common::memory_core::PalaceRegistry;
use trusty_common::memory_rpc::call_memory_tool_at_with_timeout;
use trusty_common::monitor::memory_client::MemoryClient;
use trusty_memory::AppState;

/// Palaces to seed. Larger than one poll's worth of rows and small enough that
/// the whole estate is created in well under a second.
const ESTATE: usize = 6;

/// Generous enough for a loaded machine; a local socket answers in microseconds.
const CALL_TIMEOUT: Duration = Duration::from_secs(20);

/// Create `ESTATE` palaces on disk and close every handle again.
///
/// Why: the poll's residue is only observable against a known baseline, so the
/// seeding registry must not still hold the handles it created.
/// What: creates each palace through a throwaway [`PalaceRegistry`], then drops
/// it — every handle goes with it and the redb flocks release.
fn seed_estate(data_root: &std::path::Path) {
    let registry = PalaceRegistry::new();
    for i in 0..ESTATE {
        let id = PalaceId::new(format!("poll-{i:02}"));
        let palace = Palace {
            id: id.clone(),
            name: id.as_str().to_string(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: data_root.join(id.as_str()),
        };
        registry
            .create_palace(data_root, palace)
            .unwrap_or_else(|e| panic!("create_palace({id}) failed: {e:#}"));
    }
    drop(registry);
}

/// A daemon on a temp socket, plus the registry the test watches.
struct Daemon {
    socket: PathBuf,
    registry: Arc<PalaceRegistry>,
    stop: Option<oneshot::Sender<()>>,
}

impl Daemon {
    /// Seed an estate, bind a temp socket, serve on it, and wait until it
    /// answers a real call.
    async fn start() -> Self {
        // #4413: seed the process-wide embedder cell so nothing reaches for the
        // real ONNX model.
        trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();

        let data = tempfile::tempdir().expect("tempdir");
        let root = data.path().to_path_buf();
        std::mem::forget(data);
        seed_estate(&root);

        let state = AppState::new(root);
        // #911: flip past the warming preflight so handlers run.
        state.set_ready();
        let registry = Arc::clone(&state.registry);
        assert_eq!(
            registry.len(),
            0,
            "a fresh AppState must start with no open handles, or the \
             assertions below measure the fixture instead of the poll"
        );

        let sockets = tempfile::tempdir().expect("tempdir");
        let socket = sockets.path().join("trusty-memory.sock");
        std::mem::forget(sockets);

        let (stop, shutdown) = oneshot::channel::<()>();
        let serve_socket = socket.clone();
        tokio::spawn(async move {
            let _ =
                trusty_memory::transport::uds::serve_with_shutdown(state, &serve_socket, async {
                    let _ = shutdown.await;
                })
                .await;
        });

        let daemon = Self {
            socket,
            registry,
            stop: Some(stop),
        };
        daemon.wait_until_answering().await;
        daemon
    }

    /// Block until the socket ANSWERS, not merely until it accepts (#6667).
    async fn wait_until_answering(&self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut last: Option<String> = None;
        while std::time::Instant::now() < deadline {
            match call_memory_tool_at_with_timeout(
                &self.socket,
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
            self.socket.display(),
            last.unwrap_or_else(|| "no error recorded".to_string())
        );
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

/// One monitor poll leaves every palace it listed closed.
///
/// Why (#7125): against `origin/main` this reads 6, not 0 — `MemoryClient`
/// sent `{}`, `counts` defaulted to `true`, and the daemon opened the whole
/// estate to count it, on every tick of a 2-second timer. The rows the client
/// projects are the same shape either way, so only the registry's open-handle
/// count can tell the two implementations apart.
/// What: seeds six closed palaces, runs the production `fetch_all` (the poll
/// the dashboard's event loop runs), and asserts the registry is still empty
/// and every row came back flagged unknown rather than dropped.
/// Test: this test.
#[tokio::test]
async fn monitor_client_poll_leaves_closed_palaces_closed() {
    let daemon = Daemon::start().await;

    let data = MemoryClient::new(daemon.socket.clone())
        .fetch_all()
        .await
        .expect("the monitor's poll answers");

    assert_eq!(
        daemon.registry.len(),
        0,
        "a poll must not open a palace to count it; {} of {ESTATE} were left \
         resident",
        daemon.registry.len()
    );
    assert_eq!(
        data.palaces.len(),
        ESTATE,
        "every palace is still LISTED — the poll trades counts for opens, not \
         rows: {:?}",
        data.palaces.iter().map(|p| &p.id).collect::<Vec<_>>()
    );
    assert!(
        data.palaces.iter().all(|p| p.counts_unknown),
        "an unopened palace's zeros must read as unknown, never as empty"
    );
}

/// The daemon half, pinned independently of the client.
///
/// Why (#7125): the assertion above can go green for the wrong reason if the
/// daemon ever starts opening palaces under `counts: false` — the client would
/// still be sending the right params and the test would still fail, with
/// nothing saying which side moved. This one call names the daemon.
/// What: calls `memory.palaces_list` with `counts: false` directly and asserts
/// the estate is still closed afterwards.
/// Test: this test.
#[tokio::test]
async fn palaces_list_without_counts_opens_nothing_on_the_daemon() {
    let daemon = Daemon::start().await;

    let listed = call_memory_tool_at_with_timeout(
        &daemon.socket,
        "memory.palaces_list",
        json!({ "counts": false }),
        CALL_TIMEOUT,
    )
    .await
    .expect("palaces_list answers");

    let rows = listed["palaces"]
        .as_array()
        .expect("palaces_list answers a palaces array");
    assert_eq!(rows.len(), ESTATE, "every palace is listed: {listed}");
    assert_eq!(
        daemon.registry.len(),
        0,
        "counts: false must answer from the registry walk alone"
    );
}
