//! Search-as-a-service daemon (#374, on a Unix socket since #6433).
//!
//! Why: The semantic code index is expensive to keep warm — loading the
//! HNSW into RAM, opening the redb store, and running the FastEmbedder
//! are all one-time costs that a short-lived REPL or sub-agent process
//! pays repeatedly. Running the index as a long-lived daemon shared by
//! every trusty-agents process in a project amortizes that cost so a tool
//! call that searches the index pays only the socket round-trip plus the
//! query itself. The daemon also owns the redb write lock exclusively,
//! which avoids the lock-contention failures we used to hit when a
//! REPL, an --api server, and a sub-agent all tried to open the same
//! `.trusty-agents/state/code/` directory.
//! What: [`run_search_service`] is the daemon entry point. It opens
//! the on-disk store, warms the HNSW into RAM, spawns a [`FileWatcher`]
//! to keep the index in sync with the working tree, and serves five
//! JSON-RPC methods on [`search_socket_path`] until SIGTERM or SIGINT.
//! Test: See `tests` module — socket-path convention, liveness probing,
//! and every method driven over a real socket.
//!
//! Module layout (split for the 500-line cap, #365):
//! - `mod.rs` — daemon lifecycle: socket-path resolution, liveness
//!   probing, [`SearchState`], and [`run_search_service`].
//! - [`rpc`] — the method table, the shared wire types, and the serve loop.
//! - [`handlers`] — what the five methods actually do.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::Mutex;

use crate::memory::{CodeStore, FastEmbedder};
use crate::search::indexer::CodeIndexer;
use crate::search::watcher::FileWatcher;

mod handlers;
pub mod rpc;
#[cfg(test)]
pub(crate) mod tests;

pub use rpc::{METHOD_HEALTH, METHODS, build_router};

/// Embedding dimension for FastEmbedder. Mirrors `build_file_watcher` in
/// `src/main.rs` so the daemon and the in-process watcher see identical
/// vectors on the wire.
const EMBED_DIM: usize = 384;

/// How long [`is_daemon_running`] waits for `search.health` to answer.
///
/// Why 500ms: the probe runs on the PM's startup path, before the background
/// file watcher decides whether it may open the code store itself. A local
/// socket round-trip is sub-millisecond, so this is generous for a live daemon
/// and short enough that a wedged one does not stall startup.
const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Default extensions the embedded watcher tracks. Mirrors
/// `default_extensions` in `src/main.rs`.
pub(crate) fn default_extensions() -> Vec<String> {
    ["rs", "py", "ts", "tsx", "js", "jsx", "go", "md"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Resolve the canonical Unix-socket path for the daemon.
///
/// Why: mirrors `ctrl_socket_path` in `src/ctrl/socket.rs`, so both of this
/// crate's sockets live in one directory under one naming convention. The path
/// is per-project because the index is: two checkouts of the same repository
/// each run their own daemon over their own store.
///
/// #6433: this is now the daemon's whole address. There is no discovery file —
/// caller and daemon derive the same path from the same project root, so there
/// is nothing for a stale file to disagree with.
/// What: returns `~/.trusty-agents/sockets/<project_id>.search.sock`. Falls
/// back to `<project_root>/.trusty-agents/state/search.sock` when no home
/// directory is detectable.
/// Test: `search_socket_path_uses_project_id`.
pub fn search_socket_path(project_root: &Path) -> PathBuf {
    let project_id = crate::ctrl::socket::project_id_from_path(project_root);
    if let Some(home) = dirs::home_dir() {
        home.join(".trusty-agents")
            .join("sockets")
            .join(format!("{project_id}.search.sock"))
    } else {
        project_root
            .join(".trusty-agents")
            .join("state")
            .join("search.sock")
    }
}

/// Delete the discovery file the TCP daemon used to publish (#6433).
///
/// Why: `.trusty-agents/state/search.pid` recorded `{pid, port}` so a client
/// could find the auto-assigned port. Nothing reads it now, and leaving it on
/// disk invites a future reader to trust a port that belongs to whatever else
/// has since bound it — the shape of the #2566 collision. Removing it at every
/// start makes the retirement observable rather than assumed.
/// What: best-effort unlink of the retired path. A missing file is the expected
/// case after the first start.
/// Test: `daemon_removes_the_retired_discovery_file`.
pub(crate) fn remove_retired_discovery_file(project_root: &Path) {
    let _ = std::fs::remove_file(
        project_root
            .join(".trusty-agents")
            .join("state")
            .join("search.pid"),
    );
}

/// Returns true iff a daemon is observably answering for `project_root`.
///
/// Why: the PM's background file watcher must not open the code store when a
/// daemon already holds the redb lock, and a socket file's existence does not
/// prove anyone is serving it. Only an answered `search.health` does.
/// What: dials [`search_socket_path`] with a [`HEALTH_PROBE_TIMEOUT`] budget
/// and reports whether the daemon answered with a `result`. A daemon that
/// answers a refusal counts as not running: a health method that refuses is not
/// a healthy daemon, and treating it as up would let the watcher race the
/// store lock.
/// Test: `is_daemon_running_is_false_with_no_socket`,
/// `is_daemon_running_is_true_against_a_live_daemon`.
pub async fn is_daemon_running(project_root: &Path) -> bool {
    health_ok(&search_socket_path(project_root)).await
}

/// Call `search.health` on `socket` and report whether it answered.
///
/// Split from [`is_daemon_running`] so a test can point it at a temporary
/// socket without owning the home directory the real path resolves under.
async fn health_ok(socket: &Path) -> bool {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": METHOD_HEALTH,
        "params": {},
    });
    matches!(
        trusty_common::uds::send_framed_request::<
            _,
            trusty_common::uds::server::RpcResponse,
        >(socket, &request, HEALTH_PROBE_TIMEOUT)
        .await,
        Ok(response) if response.result.is_some()
    )
}

/// Shared state injected into every handler.
///
/// Why: each handler needs the same indexer plus bookkeeping; one struct keeps
/// the five registrations in [`rpc::build_router`] uniform.
/// What: holds an `Arc<CodeIndexer>` (the warm index), the project root (so
/// handlers can canonicalise relative paths), and a small `Mutex` guard around
/// an in-flight reindex flag so duplicate background reindex requests don't
/// pile up.
/// Test: exercised by every `rpc_*` test.
#[derive(Clone)]
pub struct SearchState {
    pub indexer: Arc<CodeIndexer>,
    pub project_root: PathBuf,
    pub reindex_in_flight: Arc<Mutex<bool>>,
}

/// Run the search-as-a-service daemon to completion.
///
/// Why: long-running entry point invoked from `main.rs` early dispatch
/// (`--search-service`). Owns the redb lock for the project's code store for
/// the lifetime of the process — every other trusty-agents process in the same
/// project must talk to the daemon over the socket rather than opening the
/// store itself.
/// What:
///   1. Refuses to start if a healthy daemon is already answering.
///   2. Takes a `flock` on `.trusty-agents/state/search.lock` so two daemons
///      started in the same instant cannot both pass step 1.
///   3. Opens `CodeStore` + `FastEmbedder` and builds an `Arc<CodeIndexer>`
///      with `Duration::MAX` cool-down (never evict — that's the whole point
///      of having a daemon).
///   4. `warm_up()` — load HNSW into RAM.
///   5. Spawns a `FileWatcher` task so on-disk edits update the index.
///   6. Serves five JSON-RPC methods on [`search_socket_path`] until SIGTERM
///      or SIGINT, then unlinks the socket.
///
/// # Errors
///
/// When the code store cannot be opened, the embedder cannot be built, or the
/// socket cannot be bound — the last including the case where another daemon is
/// provably live on the path.
///
/// Test: `rpc_health_answers_over_a_real_socket` drives the serve loop with
/// mocked stores; the store-opening prologue needs a real embedder model and is
/// covered manually by `trusty-agents --search-service`.
pub async fn run_search_service(project_root: PathBuf) -> Result<()> {
    if is_daemon_running(&project_root).await {
        println!("search daemon already running; nothing to do");
        return Ok(());
    }

    // TOCTOU guard (#376 A3): two daemons started concurrently can both
    // pass the `is_daemon_running` check above. `bind_singleton_hardened`
    // narrows the window but does not close it — both would probe an unserved
    // path and both would then bind. Acquire an exclusive non-blocking flock on
    // a sibling lock file so only one process proceeds. The lock is released
    // automatically when `_lock_file` is dropped (i.e., on daemon exit).
    let state_dir = project_root.join(".trusty-agents").join("state");
    std::fs::create_dir_all(&state_dir)
        .with_context(|| format!("creating state dir {}", state_dir.display()))?;
    let lock_path = state_dir.join("search.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("opening startup lock {}", lock_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        // SAFETY: `lock_file` owns the fd for the duration of this call;
        // we never close it manually. flock with LOCK_NB returns 0 on
        // success and -1 with errno=EWOULDBLOCK if another process holds
        // the lock — the exact contention case we want to detect.
        let rc = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            anyhow::bail!(
                "search daemon already starting (lock held at {})",
                lock_path.display()
            );
        }
    }
    // Keep the lock alive for the lifetime of the daemon by binding it
    // to a name; dropping at end-of-fn releases the OS lock.
    let _lock_file = lock_file;

    // #6433: one-time migration cleanup for the TCP daemon's port file.
    remove_retired_discovery_file(&project_root);

    let code_dir = project_root
        .join(".trusty-agents")
        .join("state")
        .join("code");
    std::fs::create_dir_all(&code_dir)
        .with_context(|| format!("creating code dir {}", code_dir.display()))?;

    tracing::info!("opening code store at {}", code_dir.display());
    let store = CodeStore::open(&code_dir, EMBED_DIM).context("failed to open CodeStore")?;
    let embedder = FastEmbedder::new().context("failed to construct FastEmbedder")?;
    // Cap concurrent indexing jobs at ~half available parallelism so handler
    // tasks always have threads to run on. Without this cap a burst of
    // fastembed ONNX inference jobs (one per chunk) saturates the tokio
    // blocking pool and `search.query` times out under active re-indexing
    // (#399).
    let indexing_concurrency = std::thread::available_parallelism()
        .map(|n| (n.get() / 2).max(1))
        .unwrap_or(1);
    let indexing_permits = Arc::new(tokio::sync::Semaphore::new(indexing_concurrency));
    tracing::info!(
        permits = indexing_concurrency,
        "search daemon: capping concurrent indexing jobs"
    );
    // `Duration::MAX` disables cool-down — the daemon is exactly the place
    // where we want to keep the HNSW pinned in RAM forever.
    let indexer = Arc::new(
        CodeIndexer::new(Arc::new(store), Arc::new(embedder))
            .with_cool_after(Duration::MAX)
            .with_indexing_semaphore(Arc::clone(&indexing_permits)),
    );

    tracing::info!("warming code index...");
    if let Err(e) = indexer.warm_up().await {
        tracing::warn!(error = %e, "warm_up failed; will lazy-load on first query");
    }

    // Background file watcher so edits propagate without a manual reindex.
    let watcher_indexer = Arc::clone(&indexer);
    let watcher_root = project_root.clone();
    tokio::spawn(async move {
        let watcher = FileWatcher::new(watcher_indexer, watcher_root, default_extensions());
        if let Err(e) = watcher.watch().await {
            tracing::warn!(error = %e, "file watcher exited");
        }
    });

    let socket = search_socket_path(&project_root);
    println!(
        "[trusty-agents] search daemon: {} (pid {})",
        socket.display(),
        std::process::id()
    );

    let state = SearchState {
        indexer: Arc::clone(&indexer),
        project_root: project_root.clone(),
        reindex_in_flight: Arc::new(Mutex::new(false)),
    };
    rpc::serve(state, &socket).await
}
