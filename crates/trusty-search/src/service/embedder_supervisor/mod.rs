//! Supervisor façade for the `trusty-embedderd` subprocess.
//!
//! Why: trusty-embedderd is a core subprocess that owns ONNX model loading
//! and serves embedding RPC. We supervise it from trusty-search so the user
//! experiences a single daemon (`trusty-search start`) without manual
//! lifecycle management. This aligns with industry-standard ML serving
//! topology (Triton, vLLM, TEI, ollama) and reduces trusty-search daemon
//! RSS substantially by moving the ONNX arena out of the search process.
//!
//! What: re-exports the supervisor types from `trusty_common::embedder_client`
//! so callers inside trusty-search can import from a single stable path. Also
//! provides `SupervisorConfig::from_env()` with trusty-search–specific
//! defaults, the `default_socket_path()` helper for per-instance UDS sockets,
//! the `locate_embedderd_binary()` wrapper that adds the actionable error
//! message format preferred by trusty-search's startup logs, and the new
//! `LazyEmbedderHandle` that defers spawn until the first embedding request
//! arrives (issue #315).
//!
//! Test: unit tests in the `tests` submodule cover config parsing, socket
//! path construction, binary discovery, and the lazy-spawn contract.
//! Integration tests in `tests/embedder_supervisor_e2e.rs` cover the full
//! process lifecycle (marked `#[ignore]` since they spawn a real ONNX binary).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::{Mutex, RwLock};
use trusty_common::embedder_client::EmbedderClient;

// Re-export the core supervisor type from trusty-common.
pub use trusty_common::embedder_client::EmbedderSupervisor;

// ── Configuration ────────────────────────────────────────────────────────────

/// Supervisor tuning knobs, all settable via environment variables.
///
/// Why: hard-coded constants make the supervisor untunable in production.
/// Env vars let operators increase `startup_timeout_secs` on slow machines or
/// `max_restarts` on flaky networks without recompiling.
/// What: wraps the field names used by `trusty_common::embedder_client::SupervisorConfig`
/// and provides a `from_env()` constructor that reads the `TRUSTY_EMBEDDERD_*`
/// environment variables with trusty-search's preferred defaults.
/// The `into_common_for_tests()` method converts to the type expected by
/// `EmbedderSupervisor::spawn_stdio` for lifecycle-only test spawns.
/// Test: `config_from_env_defaults` and `config_from_env_overrides` in the
/// `tests` module below.
#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    /// How long to wait for the startup readiness probe (seconds).
    /// Env: `TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS` (default 30).
    pub startup_timeout_secs: u64,

    /// Maximum exponential back-off ceiling between crash restarts (seconds).
    /// Env: `TRUSTY_EMBEDDERD_RESTART_BACKOFF_MAX_SECS` (default 60).
    pub backoff_max_secs: u64,

    /// Maximum number of crashes before the supervisor gives up.
    /// Env: `TRUSTY_EMBEDDERD_MAX_RESTARTS` (default 5).
    pub max_restarts: u32,

    /// Idle-shutdown timeout in seconds (issue #315).
    ///
    /// When non-zero, the lazy handle kills the embedderd subprocess after this
    /// many seconds with no embedding request and resets the spawn gate so the
    /// next request triggers a fresh spawn. This is the primary memory-savings
    /// lever for `lexical_only` deployments: an embedderd that was briefly
    /// needed for one reindex session will be reclaimed once it goes quiet.
    ///
    /// `0` (the default) disables idle-shutdown entirely.
    ///
    /// Env: `TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS` (default 0 = disabled).
    pub idle_shutdown_secs: u64,
}

impl SupervisorConfig {
    /// Read configuration from environment variables, falling back to defaults.
    ///
    /// Why: makes the supervisor tunable in CI / production without source changes.
    /// What: reads the four `TRUSTY_EMBEDDERD_*` vars; ignores malformed
    /// values and falls through to defaults.
    /// Test: `config_from_env_defaults` and `config_from_env_overrides`.
    pub fn from_env() -> Self {
        Self {
            startup_timeout_secs: parse_env_u64("TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS", 30),
            backoff_max_secs: parse_env_u64("TRUSTY_EMBEDDERD_RESTART_BACKOFF_MAX_SECS", 60),
            max_restarts: parse_env_u32("TRUSTY_EMBEDDERD_MAX_RESTARTS", 5),
            idle_shutdown_secs: parse_env_u64("TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS", 0),
        }
    }

    /// Convert to the `trusty_common` supervisor config type without a
    /// sidecar batch size — **for test/lifecycle spawns only**.
    ///
    /// Why: `EmbedderSupervisor::spawn_stdio` expects
    /// `trusty_common::embedder_client::SupervisorConfig`; this conversion
    /// avoids duplicating field names at the call site. It is used by the
    /// integration tests (`tests/embedder_supervisor_e2e.rs`) that test
    /// process lifecycle (spawn, crash-restart, shutdown) and do not need
    /// batch forwarding. The production code path (`do_spawn`) does NOT use
    /// this method — it builds the common config directly so it can populate
    /// `sidecar_batch_size: Some(forwarded_batch)` after resolving the
    /// execution provider (see Fix C, issue #747). The two paths are therefore
    /// intentionally divergent: `into_common_for_tests` is for lifecycle/test
    /// spawns where batch forwarding is irrelevant. **Do not use this method
    /// in production spawn paths** — the `None` batch size means the sidecar
    /// will use its own default (32), silently losing batch forwarding.
    ///
    /// What: maps the three spawn-relevant fields 1:1; `idle_shutdown_secs` is
    /// trusty-search–specific and has no counterpart in the common type.
    /// `sidecar_batch_size` is always `None`. Use `do_spawn` for production
    /// paths where batch forwarding is required.
    ///
    /// Test: `into_common_for_tests_maps_fields`.
    pub fn into_common_for_tests(self) -> trusty_common::embedder_client::SupervisorConfig {
        trusty_common::embedder_client::SupervisorConfig {
            startup_timeout_secs: self.startup_timeout_secs,
            backoff_max_secs: self.backoff_max_secs,
            max_restarts: self.max_restarts,
            // sidecar_batch_size intentionally None: this method is used by
            // integration tests that exercise lifecycle, not batch forwarding.
            // Production spawns go through do_spawn, which sets Some(batch).
            sidecar_batch_size: None,
        }
    }
}

impl Default for SupervisorConfig {
    /// Default configuration — matches `from_env()` when no env vars are set.
    ///
    /// Why: unit tests need a cheap config without touching env vars.
    /// What: `startup_timeout_secs=30`, `backoff_max_secs=60`,
    /// `max_restarts=5`, `idle_shutdown_secs=0` (disabled).
    /// Test: used directly in unit tests.
    fn default() -> Self {
        Self {
            startup_timeout_secs: 30,
            backoff_max_secs: 60,
            max_restarts: 5,
            idle_shutdown_secs: 0,
        }
    }
}

// ── Binary discovery ─────────────────────────────────────────────────────────

/// Locate the `trusty-embedderd` binary.
///
/// Why: operators may install the binary in a non-standard location or point
/// to a development build; both cases are handled without modifying source.
/// What: delegates to `trusty_common::embedder_client::locate_embedderd_binary`.
/// Search order:
///
///   1. `TRUSTY_EMBEDDERD_BIN` env var — must exist if set.
///   2. Sibling of `current_exe()` — works for both `cargo run` and installs.
///   3. `trusty-embedderd` on `PATH`.
///   4. Otherwise returns `Err` with an actionable install hint.
///
/// Test: `locate_binary_bad_explicit_path_errors` and `locate_binary_via_explicit_env`.
pub fn locate_embedderd_binary() -> anyhow::Result<PathBuf> {
    trusty_common::embedder_client::locate_embedderd_binary()
}

// ── Socket path resolution ───────────────────────────────────────────────────

/// Compute a per-instance UDS socket path that avoids collisions between
/// concurrent trusty-search daemons on the same machine.
///
/// Why: if two daemons share a single socket path, the second spawn would
/// fail with "address already in use". Using the parent PID disambiguates.
/// What:
///   - macOS/Linux: `$TMPDIR/trusty-embedderd-<PID>.sock`
///   - Falls back to `/tmp/trusty-embedderd-<PID>.sock` when `TMPDIR` is
///     empty (common on headless Linux).
///
/// Note: this path is used for the UDS transport
/// (`TRUSTY_EMBEDDER=unix:/path`). The default auto-spawn path uses the
/// stdio transport via `EmbedderSupervisor::spawn_stdio`.
/// Test: `default_socket_path_is_pid_specific`.
pub fn default_socket_path() -> PathBuf {
    let pid = std::process::id();
    let filename = format!("trusty-embedderd-{pid}.sock");

    let dir = std::env::var("TMPDIR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));

    dir.join(filename)
}

// ── Lazy spawn handle (issue #315) ───────────────────────────────────────────

/// Shared inner state for `LazyEmbedderHandle` that can be re-created
/// after an idle-shutdown cycle.
///
/// Why: `OnceCell` cannot be reset after initialisation. Wrapping it in a
/// `Mutex<Option<SpawnedState>>` lets the idle-shutdown watchdog atomically
/// clear the live state so the next embed request triggers a fresh spawn.
///
/// What: holds the live `client_slot` (used for crash-restart transparent
/// embed calls) and the background `shutdown_tx` channel that the watchdog
/// uses to signal the supervisor to stop.
///
/// Test: covered by `lazy_handle_*` unit tests in the `tests` module.
struct SpawnedState {
    /// The embed-client slot — the supervisor swaps this on crash-restart.
    client_slot: Arc<RwLock<Arc<dyn EmbedderClient>>>,
    /// Kept alive so that dropping `SpawnedState` (on idle-shutdown or daemon
    /// exit) automatically signals the watchdog task to stop. The receiver
    /// end is held by the watchdog; when this Sender is dropped, the
    /// `shutdown_rx` in the watchdog fires.
    // Why field is "unused" according to rustc: we store it for its drop
    // behaviour (implicit oneshot cancellation), not for any explicit send.
    #[allow(dead_code)]
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
    /// Kept alive to ensure the supervisor's `child_pid_slot` Arc remains
    /// valid as long as the state is live. The forwarder task in `do_spawn`
    /// clones the same Arc; this field prevents it from becoming a dangling
    /// clone if the caller drops their reference.
    #[allow(dead_code)]
    pid_slot: Arc<AtomicU32>,
}

/// Deferred-spawn handle for the `trusty-embedderd` sidecar (issue #315).
///
/// Why: `trusty-search start` previously spawned `trusty-embedderd` at boot
/// unconditionally — even for `lexical_only` deployments that never issue an
/// embed request. Idle embedderd processes hold ~123 MB RSS doing nothing.
/// `LazyEmbedderHandle` defers the spawn until the first `embed_batch` call
/// arrives, saving that RSS for deployments with zero or infrequent semantic
/// workloads.
///
/// What: wraps the binary path, config, and a `OnceCell`-behind-a-`Mutex`
/// so that concurrent first callers race to acquire the lock; only one
/// spawns the child while the others wait. After the first spawn all
/// subsequent calls proceed without the lock. On idle-shutdown (when
/// `TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS > 0`), the watchdog sends a
/// shutdown signal, clears the state, and resets the spawn gate so the
/// next request triggers a fresh spawn. The same `Arc<AtomicU32>` PID slot
/// that the search daemon's `/health` handler reads is updated automatically.
///
/// Test: `lazy_handle_defers_spawn`, `lazy_handle_single_flight_concurrent`,
/// and `lazy_handle_idle_shutdown` in this module's `tests` submodule.
pub struct LazyEmbedderHandle {
    binary_path: PathBuf,
    config: SupervisorConfig,
    /// Guards a lazily-initialised `SpawnedState`. The `Option` is `Some`
    /// while the sidecar is live and `None` when idle-shutdown has cleared it
    /// (or before the first spawn). The `Mutex` provides single-flight
    /// semantics: concurrent first callers serialise on it and only the
    /// winner spawns the child.
    state: Arc<Mutex<Option<SpawnedState>>>,
    /// The search daemon's AppState PID slot — written once by
    /// `child_pid_slot()` after construction so the health handler always
    /// reads the current child PID.
    app_pid_slot: Arc<AtomicU32>,
    /// Last time any embed request completed successfully (monotonic clock).
    /// Used by the idle-shutdown watchdog.
    last_use: Arc<Mutex<Option<Instant>>>,
    /// Abort handle for the pid-slot forwarder task (issue #829 — task leak).
    /// Why: old slots never reset to 0 — abort before each re-spawn.
    pid_forwarder_handle: Mutex<Option<tokio::task::AbortHandle>>,
}

impl LazyEmbedderHandle {
    /// Construct a new handle.
    ///
    /// Why: separates construction (cheap, synchronous) from spawn (async,
    /// slow). Called from `build_embedder` at daemon startup so the handle
    /// is ready to accept the first request without blocking the HTTP listener.
    ///
    /// What: stores `binary_path` and `config` for use at first-spawn time.
    /// No child process is started. Logs "embedderd supervisor armed, deferred
    /// spawn enabled" so operators see the new behaviour in startup logs.
    ///
    /// Test: `lazy_handle_defers_spawn` — asserts the child PID is 0 after
    /// construction.
    pub fn new(binary_path: PathBuf, config: SupervisorConfig) -> Self {
        tracing::info!(
            "embedderd supervisor armed, deferred spawn enabled \
             (idle_shutdown_secs={})",
            config.idle_shutdown_secs,
        );
        Self {
            binary_path,
            config,
            state: Arc::new(Mutex::new(None)),
            app_pid_slot: Arc::new(AtomicU32::new(0)),
            last_use: Arc::new(Mutex::new(None)),
            pid_forwarder_handle: Mutex::new(None),
        }
    }

    /// Return the `Arc<AtomicU32>` PID slot shared with the search daemon's
    /// AppState (for `/health` embedderd RSS reporting).
    ///
    /// Why: the AppState calls `install_embedderd_pid_slot` with this Arc so
    /// `/health` always reads the current child PID without any mutex.
    /// What: clones and returns `self.app_pid_slot`.
    /// Test: `lazy_handle_defers_spawn` — asserts the slot reads 0 before
    /// spawn and non-zero after the first embed call.
    pub fn app_pid_slot(&self) -> Arc<AtomicU32> {
        Arc::clone(&self.app_pid_slot)
    }

    /// Get (or lazily spawn) the live embed-client, then execute `op`.
    ///
    /// Why: inlining the single-flight logic into every embed path would
    /// scatter the deferred-spawn contract across call sites. This method is
    /// the single choke-point: acquire the lock, check if already spawned,
    /// spawn if not, then call `op` with the live client.
    ///
    /// What:
    ///   1. Lock `self.state`.
    ///   2. If `state` is `None`, call `do_spawn` to start the child and
    ///      store a `SpawnedState`.
    ///   3. Clone the `client_slot` Arc, drop the lock.
    ///   4. Read-lock the client slot, clone the `Arc<dyn EmbedderClient>`,
    ///      release that lock.
    ///   5. Call `op(client)` — the actual embed request.
    ///   6. Update `last_use` on success so the idle watchdog can fire.
    ///
    /// Test: `lazy_handle_defers_spawn`, `lazy_handle_single_flight_concurrent`.
    pub async fn embed_via<F, Fut, T>(
        &self,
        op: F,
    ) -> Result<T, trusty_common::embedder_client::EmbedderError>
    where
        F: FnOnce(Arc<dyn EmbedderClient>) -> Fut,
        Fut: std::future::Future<Output = Result<T, trusty_common::embedder_client::EmbedderError>>,
    {
        // Acquire the state lock for single-flight spawn.
        let client_slot = {
            let mut guard = self.state.lock().await;
            if guard.is_none() {
                // First caller wins the race to spawn. All others are
                // serialised on the lock and will find `state = Some` when
                // they acquire it after this block completes.
                let spawned = do_spawn(
                    &self.binary_path,
                    &self.config,
                    Arc::clone(&self.app_pid_slot),
                    Arc::clone(&self.state),
                    Arc::clone(&self.last_use),
                    &self.pid_forwarder_handle,
                )
                .await
                .map_err(|e| {
                    trusty_common::embedder_client::EmbedderError::ModelError(format!(
                        "lazy embedderd spawn failed: {e:#}"
                    ))
                })?;
                *guard = Some(spawned);
            }
            // Safety: we just set it to Some if it was None.
            let spawned = guard.as_ref().expect("state is Some after spawn");
            Arc::clone(&spawned.client_slot)
        };

        // Read the live client from the slot (the supervisor may swap it on
        // crash-restart). Drop the read lock before calling `op`.
        let client = client_slot.read().await.clone();

        let result = op(client).await;

        // Record last-use time on success so the idle watchdog doesn't evict
        // a process that is actively serving requests.
        if result.is_ok() {
            let mut last_use = self.last_use.lock().await;
            *last_use = Some(Instant::now());
        }

        result
    }
}

/// Spawn the sidecar, wire the supervisor, arm the idle-shutdown watchdog, and
/// return `SpawnedState`. Aborts any previous pid-slot forwarder (issue #829).
///
/// Why: extracted from `LazyEmbedderHandle::embed_via` so spawn logic can be
/// tested in isolation. Also forwards ONNX batch size (issue #747 Fix C).
/// What: calls `EmbedderSupervisor::spawn_stdio`, updates `app_pid_slot`.
/// Test: `lazy_handle_defers_spawn` — spawn triggered inside `embed_via`.
async fn do_spawn(
    binary_path: &Path,
    config: &SupervisorConfig,
    app_pid_slot: Arc<AtomicU32>,
    state_cell: Arc<Mutex<Option<SpawnedState>>>,
    last_use: Arc<Mutex<Option<Instant>>>,
    pid_forwarder_handle: &Mutex<Option<tokio::task::AbortHandle>>,
) -> Result<SpawnedState> {
    tracing::info!(
        binary = %binary_path.display(),
        "LazyEmbedderHandle: first embed request — spawning trusty-embedderd",
    );

    // Fix C (issue #747): forward the auto-tuned batch size to the sidecar.
    // CoreML: cap at coreml_cap to avoid jetsam SIGKILL.
    // CUDA (issue #763 Fix 2): cap at cuda_sidecar_batch_cap() to stay within
    // VRAM budget (forwarding 512 with INFLIGHT=2 re-triggers #600).
    // Re-resolve on each (re)spawn so config changes take effect without a
    // full daemon restart.
    let predicted_provider = trusty_common::embedder::resolve_expected_provider();
    let is_coreml = matches!(
        predicted_provider,
        trusty_common::embedder::ExecutionProvider::CoreML
            | trusty_common::embedder::ExecutionProvider::CoreMLAne
    );
    let is_cuda = matches!(
        predicted_provider,
        trusty_common::embedder::ExecutionProvider::Cuda
    );
    let resolved_batch = crate::core::indexer::embed_batch_size();
    let coreml_cap = crate::core::resolve_coreml_batch_size();
    let cuda_cap = trusty_common::embedder_client::cuda_sidecar_batch_cap();
    let forwarded_batch = trusty_common::embedder_client::sidecar_batch_size(
        resolved_batch,
        is_coreml,
        coreml_cap,
        is_cuda,
        cuda_cap,
    );
    tracing::info!(
        resolved_batch,
        forwarded_batch,
        is_coreml,
        is_cuda,
        coreml_cap,
        cuda_cap,
        "LazyEmbedderHandle: TRUSTY_EMBED_BATCH_SIZE={forwarded_batch} \
         (resolved={resolved_batch}, is_cuda={is_cuda}, cuda_cap={cuda_cap})"
    );

    let common_config = trusty_common::embedder_client::SupervisorConfig {
        startup_timeout_secs: config.startup_timeout_secs,
        backoff_max_secs: config.backoff_max_secs,
        max_restarts: config.max_restarts,
        sidecar_batch_size: Some(forwarded_batch),
    };

    let (supervisor, client_slot, child_pid_slot) =
        EmbedderSupervisor::spawn_stdio(binary_path.to_path_buf(), common_config).await?;

    // Copy the initial PID into the AppState's slot so `/health` reports it
    // immediately.
    let initial_pid = child_pid_slot.load(AtomicOrdering::Acquire);
    app_pid_slot.store(initial_pid, AtomicOrdering::Release);

    // Issue #829: abort the previous forwarder before spawning a new one.
    // On idle-shutdown cycles the old child_pid_slot never resets to 0, so
    // the old forwarder loops forever without this cancellation.
    {
        let src = Arc::clone(&child_pid_slot);
        let dst = Arc::clone(&app_pid_slot);
        let join = tokio::spawn(async move {
            loop {
                let pid = src.load(AtomicOrdering::Acquire);
                dst.store(pid, AtomicOrdering::Release);
                if pid == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        });
        // Swap in the new handle, abort the old one.
        let mut handle_guard = pid_forwarder_handle.lock().await;
        if let Some(old) = handle_guard.take() {
            old.abort();
        }
        *handle_guard = Some(join.abort_handle());
    }

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    // Detach the crash-restart loop. Note: we need to arm the idle watchdog
    // AFTER calling start_supervisor_task (which consumes the supervisor).
    // We wrap the receiver in the supervision task by passing it as a
    // graceful-stop signal via a separate mechanism — since the existing
    // `start_supervisor_task` API doesn't accept a shutdown channel, we
    // use `kill_on_drop(true)` (already set in `spawn_child`) + the watchdog
    // directly killing the process via the PID slot.
    supervisor.start_supervisor_task();

    // Arm the idle-shutdown watchdog when requested.
    let idle_secs = config.idle_shutdown_secs;
    if idle_secs > 0 {
        let state_cell_clone = Arc::clone(&state_cell);
        let app_pid_slot_clone = Arc::clone(&app_pid_slot);
        let last_use_clone = Arc::clone(&last_use);
        // shutdown_rx: fires when the watchdog wants to stop itself cleanly
        // (e.g. the process was already shut down by other means). We pass
        // it through to the watchdog task to avoid a dangling task.
        tokio::spawn(idle_watchdog(
            idle_secs,
            state_cell_clone,
            app_pid_slot_clone,
            last_use_clone,
            shutdown_rx,
        ));
    }

    Ok(SpawnedState {
        client_slot,
        shutdown_tx,
        pid_slot: child_pid_slot,
    })
}

/// Idle-shutdown watchdog task (issue #315).
///
/// Why: an embedderd that was briefly needed (e.g. one reindex cycle on a
/// `lexical_only` deployment) should not hold ~123 MB RSS indefinitely. The
/// watchdog polls the `last_use` timestamp and kills the child when the idle
/// window expires, then resets the spawn gate so the next request triggers a
/// fresh spawn.
///
/// What: ticks every 10 seconds. On each tick:
///   1. Reads `last_use` to compute idle duration.
///   2. If `idle_duration >= idle_secs`, kills the child via its OS PID,
///      clears `state_cell` (resets the spawn gate), and exits.
///   3. If `shutdown_rx` fires, exits cleanly (the handle was dropped or the
///      daemon is shutting down).
///
/// Killing the child triggers the supervisor loop's `child.wait()` to
/// return, which clears `child_pid_slot` to 0 and then stops supervising
/// (exit code will be non-zero from SIGKILL; after `max_restarts` is
/// exceeded the loop exits — but we clear `state_cell` first so the
/// next request triggers `do_spawn` freshly rather than waiting for the
/// old supervisor to exhaust its retry budget).
///
/// Test: `lazy_handle_idle_shutdown` — creates a handle, waits for the
/// watchdog to fire, and asserts the PID slot returns to 0.
async fn idle_watchdog(
    idle_secs: u64,
    state_cell: Arc<Mutex<Option<SpawnedState>>>,
    app_pid_slot: Arc<AtomicU32>,
    last_use: Arc<Mutex<Option<Instant>>>,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) {
    let poll_interval = Duration::from_secs(10).min(Duration::from_secs(idle_secs));
    let idle_threshold = Duration::from_secs(idle_secs);

    loop {
        tokio::select! {
            _ = tokio::time::sleep(poll_interval) => {}
            _ = &mut shutdown_rx => {
                tracing::debug!("idle_watchdog: shutdown signal received, exiting");
                return;
            }
        }

        // Check how long since the last successful embed call.
        let idle_duration = {
            let guard = last_use.lock().await;
            match *guard {
                Some(t) => t.elapsed(),
                // Never used yet — treat as zero idle (don't evict something
                // that hasn't been used at all; it may still be coming up).
                None => Duration::ZERO,
            }
        };

        if idle_duration < idle_threshold {
            continue;
        }

        // Idle threshold exceeded. Kill the child and reset the spawn gate.
        tracing::info!(
            idle_secs = idle_secs,
            "LazyEmbedderHandle: idle threshold exceeded — shutting down embedderd"
        );

        // Lock the state to prevent concurrent embed calls from observing a
        // partially-torn-down state.
        let mut guard = state_cell.lock().await;
        if guard.is_some() {
            // Kill the child process via its OS PID.
            let pid = app_pid_slot.load(AtomicOrdering::Acquire);
            if pid != 0 {
                #[cfg(unix)]
                {
                    use nix::sys::signal::{kill, Signal};
                    use nix::unistd::Pid;
                    let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
                    // Brief grace period before SIGKILL.
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
                }
                #[cfg(not(unix))]
                {
                    // On non-Unix platforms (Windows), we have no direct kill
                    // mechanism from PID alone without a Child handle. The
                    // watchdog logs and resets the state; the supervisor will
                    // eventually notice the child is gone.
                    tracing::warn!(
                        "idle_watchdog: idle kill not supported on this platform; \
                         clearing state only"
                    );
                }
            }

            // Reset the spawn gate so the next embed call triggers a fresh
            // spawn. Clear the app PID slot so `/health` reports no sidecar.
            *guard = None;
            app_pid_slot.store(0, AtomicOrdering::Release);

            tracing::info!(
                "LazyEmbedderHandle: embedderd idle-shutdown complete; spawn gate reset"
            );
        }

        // Exit the watchdog — the next spawn will start a new watchdog task.
        return;
    }
}

// ── Private utilities ─────────────────────────────────────────────────────────

fn parse_env_u64(var: &str, default: u64) -> u64 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn parse_env_u32(var: &str, default: u32) -> u32 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests;
