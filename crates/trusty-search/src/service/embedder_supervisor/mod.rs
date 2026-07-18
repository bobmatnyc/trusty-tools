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

    /// Idle-shutdown timeout in seconds (issue #315, default flipped in #2315).
    ///
    /// When non-zero, the lazy handle kills the embedderd subprocess after this
    /// many seconds with no embedding request and resets the spawn gate so the
    /// next request triggers a fresh spawn. This is the primary memory-savings
    /// lever for `lexical_only` deployments: an embedderd that was briefly
    /// needed for one reindex session will be reclaimed once it goes quiet.
    ///
    /// Defaults to `300` (5 minutes). An idle `trusty-embedderd` was measured
    /// pinning ~2.9 GB RSS indefinitely (issue #2315) because, while the
    /// watchdog machinery existed since #315, it shipped disabled (`0`). Flipping
    /// the default reclaims that resting RSS after a short idle window; the
    /// cold-respawn cost on the next request is ~2–15 s, which is acceptable for
    /// the memory win. `0` remains "explicitly disabled" for operators who set it.
    ///
    /// Env: `TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS` (default 300; `0` = disabled).
    pub idle_shutdown_secs: u64,

    /// Seconds of sustained health (no further wedge-triggered restart)
    /// required before the supervisor resets its wedge-restart escalation
    /// counter back to zero (#1450 HIGH follow-up — restart-storm fix).
    ///
    /// Why: forwarded 1:1 to `trusty_common::embedder_client::SupervisorConfig`.
    /// Without a trusty-search-specific field here, `TRUSTY_EMBEDDERD_WEDGE_RESET_SECS`
    /// would be dead configuration for the real daemon path — `do_spawn` builds
    /// the common config directly and `into_common_for_tests` previously fell
    /// back to `..SupervisorConfig::default()` (a hardcoded 300s), so the env
    /// var never actually reached either construction site.
    ///
    /// Env: `TRUSTY_EMBEDDERD_WEDGE_RESET_SECS` (default 300 = 5 minutes).
    pub wedge_reset_secs: u64,
}

impl SupervisorConfig {
    /// Read configuration from environment variables, falling back to defaults.
    ///
    /// Why: makes the supervisor tunable in CI / production without source changes.
    /// What: reads the five `TRUSTY_EMBEDDERD_*` vars; ignores malformed
    /// values and falls through to defaults. `idle_shutdown_secs` defaults to
    /// `300` (issue #2315) so an idle sidecar's ~2.9 GB RSS is reclaimed at rest
    /// rather than pinned for the daemon's lifetime; `0` explicitly disables it.
    /// `wedge_reset_secs` defaults to `300` (#1450 HIGH follow-up).
    /// Test: `config_from_env_defaults`, `config_from_env_overrides`,
    /// `config_from_env_idle_shutdown_explicit_zero`, and
    /// `config_from_env_wedge_reset_secs_override`.
    pub fn from_env() -> Self {
        Self {
            startup_timeout_secs: parse_env_u64("TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS", 30),
            backoff_max_secs: parse_env_u64("TRUSTY_EMBEDDERD_RESTART_BACKOFF_MAX_SECS", 60),
            max_restarts: parse_env_u32("TRUSTY_EMBEDDERD_MAX_RESTARTS", 5),
            idle_shutdown_secs: parse_env_u64("TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS", 300),
            wedge_reset_secs: parse_env_u64("TRUSTY_EMBEDDERD_WEDGE_RESET_SECS", 300),
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
    /// What: maps the four spawn-relevant fields 1:1 (including
    /// `wedge_reset_secs`, #1450 HIGH follow-up); `idle_shutdown_secs` is
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
            wedge_reset_secs: self.wedge_reset_secs,
        }
    }
}

impl Default for SupervisorConfig {
    /// Default configuration — matches `from_env()` when no env vars are set.
    ///
    /// Why: unit tests need a cheap config without touching env vars.
    /// What: `startup_timeout_secs=30`, `backoff_max_secs=60`,
    /// `max_restarts=5`, `idle_shutdown_secs=300` (issue #2315),
    /// `wedge_reset_secs=300` (#1450 HIGH follow-up) — matches `from_env()`
    /// when no env vars are set.
    /// Test: used directly in unit tests.
    fn default() -> Self {
        Self {
            startup_timeout_secs: 30,
            backoff_max_secs: 60,
            max_restarts: 5,
            idle_shutdown_secs: 300,
            wedge_reset_secs: 300,
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
    /// Cooperative shutdown handle for the supervisor's detached task (issue
    /// #2979). `idle_watchdog` calls `.shutdown()` on this instead of killing
    /// the sidecar by raw OS PID — the old raw SIGTERM/SIGKILL raced the
    /// supervision loop's `child.wait()`, which had no way to distinguish an
    /// intentional stop from a crash and respawned the sidecar the watchdog
    /// had just stopped. `None` only in hand-seeded test states that never
    /// went through `do_spawn` (a real spawn always populates it).
    supervisor_handle: Option<trusty_common::embedder_client::SupervisorHandle>,
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
    /// Last time any embed request started or completed (monotonic clock).
    /// Used by the idle-shutdown watchdog.
    last_use: Arc<Mutex<Option<Instant>>>,
    /// Count of embed requests currently executing `op(client)` (issue #2315).
    ///
    /// Why: `last_use` is only bumped at request boundaries, so a single long
    /// request that straddles the idle deadline could otherwise be SIGKILLed
    /// mid-flight by the watchdog. This counter is the authoritative "busy"
    /// signal: `embed_via` holds it > 0 for the whole `op` call (via a
    /// drop-guard that decrements on success, error, and panic unwind), and the
    /// watchdog refuses to evict while it is non-zero.
    in_flight: Arc<AtomicU32>,
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
            in_flight: Arc::new(AtomicU32::new(0)),
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
    ///   2. If `state` is `None` (never spawned, or the watchdog just reaped
    ///      a previous sidecar), call `do_spawn` to start the child and store
    ///      a `SpawnedState`.
    ///   3. **While still holding the lock**, construct the `InFlightGuard`
    ///      (increments `in_flight`) and clone the `client_slot` Arc.
    ///   4. Release the lock.
    ///   5. Read-lock the client slot, clone the `Arc<dyn EmbedderClient>`,
    ///      release that lock.
    ///   6. Bump `last_use` at request START (a burst arriving just before the
    ///      idle deadline defers shutdown even before the first request
    ///      returns).
    ///   7. Call `op(client)` — the actual embed request.
    ///   8. Update `last_use` again on success; the `InFlightGuard` drops at
    ///      the end of this function's scope, decrementing `in_flight` on
    ///      success, error return, or panic unwind.
    ///
    /// Step 3 is the load-bearing fix for a kill-mid-request TOCTOU (issue
    /// #2315, reviewer-caught): incrementing `in_flight` *after* releasing
    /// `self.state` left a window where a watchdog tick could observe
    /// `in_flight == 0` under its own `state` lock while this request had
    /// already grabbed a `client_slot` clone and was about to use it —
    /// exactly the scenario the guard exists to prevent. Doing the increment
    /// inside the same critical section the watchdog re-checks under
    /// (`idle_watchdog`'s `state_cell.lock()`) makes the two sides fully
    /// serialised on one mutex: whichever acquires it first is completely
    /// visible to the other before it proceeds. If the watchdog wins the
    /// race and evicts (clearing `state` to `None`), this method's *next*
    /// lock acquisition (on a fresh call) observes `None` and respawns rather
    /// than reusing a stale `client_slot` captured before the kill — there is
    /// no in-progress call that could observe a stale slot, because the
    /// increment and the clone happen in the same locked step.
    ///
    /// Test: `lazy_handle_defers_spawn`, `lazy_handle_single_flight_concurrent`,
    /// `lazy_handle_idle_shutdown_waits_for_inflight_request` (hand-seeded
    /// counter), `embed_via_defers_watchdog_eviction_while_request_in_flight`
    /// (drives the race through this method itself with a blocking `op`).
    pub async fn embed_via<F, Fut, T>(
        &self,
        op: F,
    ) -> Result<T, trusty_common::embedder_client::EmbedderError>
    where
        F: FnOnce(Arc<dyn EmbedderClient>) -> Fut,
        Fut: std::future::Future<Output = Result<T, trusty_common::embedder_client::EmbedderError>>,
    {
        // Acquire the state lock for single-flight spawn AND to serialise the
        // in-flight increment against the watchdog's kill decision. Both the
        // increment below and the watchdog's re-check happen under this same
        // `self.state` mutex — see the doc comment above for why this
        // ordering (not just the atomic increment) is what closes the race.
        let (client_slot, _in_flight_guard) = {
            let mut guard = self.state.lock().await;
            if guard.is_none() {
                // First caller wins the race to spawn — or the watchdog just
                // reaped a previous sidecar and reset the gate. Either way we
                // (re)spawn while holding the lock so nobody downstream ever
                // observes a `client_slot` from an already-killed process.
                let spawned = do_spawn(
                    &self.binary_path,
                    &self.config,
                    Arc::clone(&self.app_pid_slot),
                    Arc::clone(&self.state),
                    Arc::clone(&self.last_use),
                    Arc::clone(&self.in_flight),
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
            // Register this request as in-flight WHILE still holding `state`.
            // This ordering — not the atomic ordering on the counter itself —
            // is what closes the #2315 TOCTOU: see the doc comment above.
            let in_flight_guard = InFlightGuard::new(Arc::clone(&self.in_flight));
            // Safety: we just set it to Some if it was None.
            let spawned = guard.as_ref().expect("state is Some after spawn");
            (Arc::clone(&spawned.client_slot), in_flight_guard)
        };

        // Read the live client from the slot (the supervisor may swap it on
        // crash-restart). Drop the read lock before calling `op`.
        let client = client_slot.read().await.clone();

        // Bump last_use at request START so a burst arriving just before the
        // idle deadline defers shutdown even before the first request returns.
        {
            let mut last_use = self.last_use.lock().await;
            *last_use = Some(Instant::now());
        }

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

/// RAII guard that tracks an in-flight embed request (issue #2315).
///
/// Why: the idle-shutdown watchdog must never SIGKILL the sidecar while a
/// request is executing. A plain increment/decrement pair around `op` would
/// leak the count if `op` returned early with `?` or panicked, permanently
/// wedging the watchdog into "always busy". A drop-guard decrements on every
/// scope exit — success, error, and unwind — so the count is always exact.
///
/// What: increments the shared `AtomicU32` on construction and decrements it
/// in `Drop`. The safety guarantee against the kill-mid-request race does
/// **not** come from the `AcqRel` ordering on the atomic itself — it comes
/// from *where* `embed_via` constructs this guard: inside the same
/// `self.state` mutex critical section that `idle_watchdog` re-checks
/// `in_flight` under (issue #2315, reviewer-caught TOCTOU). `AcqRel` here
/// only ensures the increment/decrement pair is itself atomic and visible
/// across threads once observed; the mutex is what serialises "did the
/// request register before the watchdog decided to kill".
///
/// Test: `in_flight_guard_decrements_on_drop` and
/// `in_flight_guard_decrements_on_panic` in the `tests` submodule;
/// `embed_via_defers_watchdog_eviction_while_request_in_flight` exercises the
/// full race through `embed_via` itself.
struct InFlightGuard {
    counter: Arc<AtomicU32>,
}

impl InFlightGuard {
    fn new(counter: Arc<AtomicU32>) -> Self {
        counter.fetch_add(1, AtomicOrdering::AcqRel);
        Self { counter }
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, AtomicOrdering::AcqRel);
    }
}

/// Decide whether the idle watchdog should evict the sidecar this tick.
///
/// Why: extracting the decision keeps the watchdog loop readable and gives the
/// in-flight race a small, deterministic unit test that needs no real process.
/// What: returns `true` only when the idle window has elapsed AND no embed
/// request is currently in flight. A non-zero `in_flight` defers eviction to a
/// later tick (issue #2315), by which point the request will have refreshed
/// `last_use` on completion.
/// Test: `should_idle_evict_respects_inflight`.
fn should_idle_evict(idle_duration: Duration, idle_threshold: Duration, in_flight: u32) -> bool {
    idle_duration >= idle_threshold && in_flight == 0
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
    in_flight: Arc<AtomicU32>,
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
        // #1450 HIGH follow-up: forwarded from trusty-search's own
        // SupervisorConfig (env TRUSTY_EMBEDDERD_WEDGE_RESET_SECS via
        // from_env), not the trusty-common default — this is the field the
        // running daemon actually respects.
        wedge_reset_secs: config.wedge_reset_secs,
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

    // Detach the crash-restart loop. `start_supervisor_task` returns a
    // `SupervisorHandle` (issue #2979) that `idle_watchdog` uses for
    // cooperative shutdown — flipping its internal shutdown flag makes the
    // supervision loop kill and reap the child itself and return without
    // ever entering the crash-restart path, so an intentional idle-shutdown
    // can no longer be misclassified as a crash and respawned.
    let supervisor_handle = supervisor.start_supervisor_task();

    // Arm the idle-shutdown watchdog when requested.
    let idle_secs = config.idle_shutdown_secs;
    if idle_secs > 0 {
        let state_cell_clone = Arc::clone(&state_cell);
        let app_pid_slot_clone = Arc::clone(&app_pid_slot);
        let last_use_clone = Arc::clone(&last_use);
        let in_flight_clone = Arc::clone(&in_flight);
        // shutdown_rx: fires when the watchdog wants to stop itself cleanly
        // (e.g. the process was already shut down by other means). We pass
        // it through to the watchdog task to avoid a dangling task.
        tokio::spawn(idle_watchdog(
            idle_secs,
            state_cell_clone,
            app_pid_slot_clone,
            last_use_clone,
            in_flight_clone,
            shutdown_rx,
        ));
    }

    Ok(SpawnedState {
        client_slot,
        supervisor_handle: Some(supervisor_handle),
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
///   2. If `idle_duration >= idle_secs` AND no request is in flight, resets
///      the spawn gate (clears `state_cell`) and cooperatively shuts down the
///      sidecar via `SpawnedState::supervisor_handle` (issue #2979), then
///      exits. While `in_flight > 0` it skips the shutdown and re-checks next
///      tick (issue #2315 — never kill a request mid-flight).
///   3. If `shutdown_rx` fires, exits cleanly (the handle was dropped or the
///      daemon is shutting down).
///
/// Before issue #2979 this killed the child directly by raw OS PID
/// (SIGTERM then SIGKILL), racing the supervision loop's `child.wait()` —
/// which had no way to tell that deliberate kill apart from a crash and
/// respawned the sidecar the watchdog had just stopped. Calling
/// `SupervisorHandle::shutdown()` instead flips a flag the supervision loop
/// itself selects on: the loop kills and reaps the child, clears
/// `child_pid_slot`, and returns without ever entering the crash-restart
/// path, so the intentional stop can no longer be misclassified.
///
/// Test: `lazy_handle_idle_shutdown_waits_for_inflight_request` drives this
/// task directly with a synthetic live state (`supervisor_handle: None` → no
/// real process to shut down) and asserts it defers eviction while
/// `in_flight > 0`, then reclaims once it drops to 0. The underlying
/// no-respawn guarantee that `SupervisorHandle::shutdown()` provides is
/// proven against a real (mocked) child process by trusty-common's
/// supervisor_intentional_shutdown_does_not_respawn test.
async fn idle_watchdog(
    idle_secs: u64,
    state_cell: Arc<Mutex<Option<SpawnedState>>>,
    app_pid_slot: Arc<AtomicU32>,
    last_use: Arc<Mutex<Option<Instant>>>,
    in_flight: Arc<AtomicU32>,
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

        // Defer eviction while the idle window has not elapsed OR a request is
        // still executing `op` (issue #2315 in-flight race). Re-check next tick;
        // a completing request refreshes `last_use`, pushing the deadline out.
        if !should_idle_evict(
            idle_duration,
            idle_threshold,
            in_flight.load(AtomicOrdering::Acquire),
        ) {
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
        // Close the TOCTOU window: a request may have incremented `in_flight`
        // between the tick check above and acquiring this lock (embed_via holds
        // the state lock only briefly at spawn, then releases it while `op`
        // runs). If one is now in flight, back off and re-check next tick rather
        // than killing it mid-request (issue #2315).
        if in_flight.load(AtomicOrdering::Acquire) > 0 {
            drop(guard);
            continue;
        }
        if let Some(spawned) = guard.take() {
            // Reset the spawn gate immediately (the cell now holds `None`)
            // and clear the app PID slot so `/health` reports no sidecar
            // right away. Release the lock before awaiting the cooperative
            // shutdown below — the gate is already clear, so a fresh embed
            // request arriving during the shutdown just triggers a new
            // `do_spawn` concurrently; it has no reason to wait on the
            // just-stopped process.
            drop(guard);
            // Note: a concurrent `do_spawn` racing in here could in principle
            // overwrite this with a new PID before we finish; correctness
            // relies on `do_spawn`'s own spawn + client-handshake latency
            // making that reorder practically unreachable, not on any
            // explicit ordering/lock between the two stores.
            app_pid_slot.store(0, AtomicOrdering::Release);

            // Cooperative shutdown (issue #2979): flips the shared shutdown
            // flag the supervision loop selects on. The loop kills and reaps
            // the child itself and returns without ever entering the
            // crash-restart path — unlike the old raw SIGTERM/SIGKILL-by-PID
            // approach this replaces, the intentional stop can never be
            // misclassified as a crash and respawned.
            if let Some(handle) = spawned.supervisor_handle {
                handle.shutdown().await;
            }

            tracing::info!(
                "LazyEmbedderHandle: embedderd idle-shutdown complete; spawn gate reset"
            );
        } else {
            drop(guard);
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
