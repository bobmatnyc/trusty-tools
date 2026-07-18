//! Lifecycle supervisor for the `trusty-embedderd` sidecar process.
//!
//! Why: the `trusty-search` daemon owns the `trusty-embedderd` process when
//! running in the default auto-spawn mode. A sidecar that crashes must be
//! restarted automatically so searches degrade only momentarily rather than
//! permanently. This module encapsulates all spawn, restart, and shutdown
//! logic in one place so the rest of the daemon treats the embedder as a
//! stable `Arc<dyn EmbedderClient>` regardless of whether the underlying
//! sidecar has been restarted.
//!
//! What: `EmbedderSupervisor` spawns `trusty-embedderd --stdio`, passes the
//! resulting pipe handles to `StdioEmbedderClient`, and stores the resulting
//! client behind an `Arc<tokio::sync::RwLock<Arc<dyn EmbedderClient>>>`. A
//! detached background task (`start_supervisor_task`) watches the child via
//! `child.wait()`, and on any non-zero exit applies exponential back-off
//! before respawning up to `max_restarts` times.
//!
//! On respawn the supervisor atomically swaps in a fresh `StdioEmbedderClient`
//! so all subsequent embed calls automatically use the new process without any
//! restart logic at the call site.
//!
//! Wedged-but-alive detection (issues #1448/#1450): `child.wait()` alone only
//! detects a sidecar that has actually exited. A sidecar whose reader task
//! died unexpectedly, or whose process is alive but stuck (e.g. the CUDA
//! BFCArena stall from #1428, or a CoreML/ANE scheduling stall), never
//! satisfies `child.wait()`. The supervision loop additionally races
//! `StdioEmbedderClient::unhealthy_signal()` alongside `child.wait()`; when it
//! fires, the loop force-kills the (possibly still-running) child and treats
//! it exactly like a crash — same back-off, same respawn path.
//!
//! Cooperative shutdown (issue #2979): `start_supervisor_task` used to consume
//! `self` and return nothing, so `EmbedderSupervisor::shutdown()` — which also
//! consumes `self` — became permanently unreachable the moment a caller
//! detached the background task (the only way callers actually use this
//! type). trusty-search's idle-shutdown watchdog worked around this by
//! sending a raw SIGTERM/SIGKILL to the child's OS PID directly, bypassing the
//! supervisor entirely — but the supervision loop's `child.wait()` has no way
//! to tell that deliberate kill apart from a real crash, so it respawned the
//! sidecar the watchdog had just stopped. `start_supervisor_task` now returns
//! a [`SupervisorHandle`] whose `shutdown()` flips a `watch::Receiver<bool>`
//! the loop selects on (both while waiting on the child and during the
//! respawn back-off sleep); the loop observes the flag, kills and reaps the
//! child itself, clears the PID slot, and returns *without* ever entering the
//! crash-restart path — the intentional stop can never be misclassified.
//!
//! Test: `supervisor_shutdown_kills_child`,
//! `supervisor_shutdown_handle_is_reachable_and_stops_child`,
//! `supervisor_intentional_shutdown_does_not_respawn`,
//! `stdio_eof_terminates_child` in this module's `tests` submodule.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::process::{Child, Command};
use tokio::sync::{RwLock, watch};

use super::{EmbedderClient, StdioEmbedderClient};

// ── Config ──────────────────────────────────────────────────────────────────

/// Configuration for `EmbedderSupervisor`.
///
/// Why: groups all tunable knobs so they can be read from env-vars in one
/// place and passed through cleanly without threading individual vars.
///
/// What: max restart count, backoff cap, startup timeout, and an optional
/// resolved ONNX batch size to forward to the sidecar process. All fields have
/// sensible defaults readable via `SupervisorConfig::from_env()`.
///
/// Test: `from_env_uses_defaults` verifies the default values.
#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    /// How many consecutive crashes are tolerated before the supervisor gives
    /// up and returns an error from `start_supervisor_task`.
    ///
    /// Env: `TRUSTY_EMBEDDERD_MAX_RESTARTS` (default 5).
    pub max_restarts: u32,

    /// Maximum sleep between restarts under exponential back-off (seconds).
    ///
    /// Env: `TRUSTY_EMBEDDERD_RESTART_BACKOFF_MAX_SECS` (default 60).
    pub backoff_max_secs: u64,

    /// How long to wait for the child to respond to the first request before
    /// treating startup as failed (seconds).
    ///
    /// Env: `TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS` (default 5).
    pub startup_timeout_secs: u64,

    /// Resolved ONNX batch size to forward as `TRUSTY_EMBED_BATCH_SIZE` to the
    /// sidecar child process (issue #747 Fix C).
    ///
    /// Why: the parent computes an auto-tuned value the sidecar never received,
    /// so the sidecar always defaulted to 32. `None` = do not forward.
    /// What: when `Some(n)`, `spawn_child` sets `.env("TRUSTY_EMBED_BATCH_SIZE", n)`.
    /// Test: `sidecar_batch_size_*` tests in this module.
    pub sidecar_batch_size: Option<usize>,

    /// Seconds of sustained health (no further wedge-triggered restart)
    /// required before the supervisor resets its wedge-restart escalation
    /// counter back to zero (#1450 HIGH follow-up — restart-storm fix).
    ///
    /// Why: a workload-deterministic wedge (the sidecar reliably wedges again
    /// shortly after every respawn, because the *workload* — not the process
    /// — is what triggers the stall) would otherwise never escalate: the
    /// ordinary `consecutive_failures` counter resets on every successful
    /// respawn, since the respawn *probe* itself succeeds even though the
    /// real workload re-wedges it moments later. `EmbedderSupervisor` tracks
    /// wedge-triggered restarts in a separate counter that is reset ONLY
    /// after this many seconds have elapsed since the last one — not by an
    /// ordinary respawn-probe success — so a genuine storm eventually trips
    /// `max_restarts` instead of cycling forever.
    ///
    /// Env: `TRUSTY_EMBEDDERD_WEDGE_RESET_SECS` (default 300 = 5 minutes).
    pub wedge_reset_secs: u64,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            max_restarts: 5,
            backoff_max_secs: 60,
            startup_timeout_secs: 5,
            sidecar_batch_size: None,
            wedge_reset_secs: 300,
        }
    }
}

impl SupervisorConfig {
    /// Read configuration from environment variables, falling back to defaults.
    ///
    /// Why: lets operators tune restart behaviour in launchd/systemd unit files
    /// without recompiling.
    /// What: reads `TRUSTY_EMBEDDERD_MAX_RESTARTS`,
    /// `TRUSTY_EMBEDDERD_RESTART_BACKOFF_MAX_SECS`,
    /// `TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS`, and
    /// `TRUSTY_EMBEDDERD_WEDGE_RESET_SECS` from the process environment.
    /// `sidecar_batch_size` defaults to `None`; callers set it via the struct.
    /// Test: `from_env_uses_defaults` (no env vars set → defaults).
    pub fn from_env() -> Self {
        let def = Self::default();
        Self {
            max_restarts: parse_env("TRUSTY_EMBEDDERD_MAX_RESTARTS", def.max_restarts),
            backoff_max_secs: parse_env(
                "TRUSTY_EMBEDDERD_RESTART_BACKOFF_MAX_SECS",
                def.backoff_max_secs,
            ),
            startup_timeout_secs: parse_env(
                "TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS",
                def.startup_timeout_secs,
            ),
            sidecar_batch_size: None,
            wedge_reset_secs: parse_env("TRUSTY_EMBEDDERD_WEDGE_RESET_SECS", def.wedge_reset_secs),
        }
    }
}

/// Default CUDA sidecar batch cap (issue #763 Fix 2).
///
/// Why: `tune_batch_size_for_provider` sets `TRUSTY_MAX_BATCH_SIZE=512` on
/// CUDA builds for pipeline-wave efficiency, but forwarding 512 directly to the
/// sidecar causes two concurrent 512-chunk ORT sessions to saturate the T4
/// BFCArena — the same OOM scenario fixed by issue #600, re-triggered by the
/// multi-flight wave size. A conservative sidecar cap decouples the parent's
/// wave size from the sidecar's per-call ORT batch size.
///
/// Overridable via `TRUSTY_CUDA_SIDECAR_BATCH_CAP` at runtime.
pub const DEFAULT_CUDA_SIDECAR_BATCH_CAP: usize = 64;

/// Read the CUDA sidecar batch cap from `TRUSTY_CUDA_SIDECAR_BATCH_CAP`; fall
/// back to `DEFAULT_CUDA_SIDECAR_BATCH_CAP` (64).
///
/// Why: allows operators to tune the cap without recompiling (e.g. smaller
/// values on VRAM-constrained GPUs, larger on multi-GPU hosts with more VRAM).
/// What: reads the env var once, parses as `usize`, clamps to `[1, 512]`.
/// Cache note: the `OnceLock` is process-scoped and initialised on first call.
/// Any change to `TRUSTY_CUDA_SIDECAR_BATCH_CAP` after the first call (including
/// changes made via `std::env::set_var` in tests) will NOT be reflected. Test
/// code that needs a different cap value must arrange for the test to execute
/// before any other code has called this function in the same process, or must
/// use a fresh process (e.g. `cargo test -- --test-threads=1`).
/// Test: `sidecar_batch_size_cuda_*` tests in this module.
pub fn cuda_sidecar_batch_cap() -> usize {
    static CACHED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("TRUSTY_CUDA_SIDECAR_BATCH_CAP")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(DEFAULT_CUDA_SIDECAR_BATCH_CAP)
            .clamp(1, 512)
    })
}

/// Resolve the ONNX batch size to forward to the sidecar (issue #747 Fix C,
/// extended by issue #763 Fix 2).
///
/// Why: the parent's auto-tuned `TRUSTY_MAX_BATCH_SIZE` was never forwarded to
/// the sidecar, which therefore always ran at the default of 32. CoreML safety
/// cap: CoreML pre-allocates per-batch GPU/ANE buffers in the unified-memory
/// pool; oversized batches can trigger jetsam SIGKILL, so the value is clamped
/// to `coreml_cap` when `is_coreml` is `true`. CUDA safety cap (#763): with
/// `INFLIGHT=2` the parent sends two concurrent 512-chunk waves; forwarding 512
/// to the sidecar causes two ORT sessions to saturate the BFCArena on a T4,
/// re-triggering the #600 OOM. `cuda_cap` (default 64, overridable via
/// `TRUSTY_CUDA_SIDECAR_BATCH_CAP`) bounds the per-ORT-call batch size
/// independently of the parent's wave size. A zero result is invalid (the sidecar
/// would set `TRUSTY_EMBED_BATCH_SIZE=0` which ORT rejects), so the return value
/// is always clamped to at least 1.
/// What: `min(resolved, coreml_cap)` when `is_coreml`; `min(resolved, cuda_cap)`
/// when `is_cuda`; `resolved` otherwise. Result further clamped to
/// `max(result, 1)` to prevent a zero batch size.
/// When `is_coreml && coreml_cap == 0` or `is_cuda && cuda_cap == 0` a
/// `tracing::warn!` is emitted to stderr because those combinations indicate a
/// likely misconfiguration — the clamp-to-1 keeps the system alive but will be
/// very slow (one embedding per ONNX call).
/// Test: `sidecar_batch_size_*` in this module's `tests`.
pub fn sidecar_batch_size(
    resolved: usize,
    is_coreml: bool,
    coreml_cap: usize,
    is_cuda: bool,
    cuda_cap: usize,
) -> usize {
    let raw = if is_coreml {
        if coreml_cap == 0 {
            tracing::warn!(
                resolved,
                "sidecar_batch_size: CoreML batch cap resolved to 0 — likely a \
                 resolve_coreml_batch_size() misconfiguration. Clamping to 1, \
                 which will be very slow (one embedding per ONNX call). \
                 Check TRUSTY_COREML_TRIPWIRE_MB and available system RAM."
            );
        }
        resolved.min(coreml_cap)
    } else if is_cuda {
        // CUDA cap: keep the sidecar's per-ORT-call batch independent of the
        // parent's wave size. The parent may send 512-chunk waves; we cap the
        // sidecar at `cuda_cap` (default 64) so two concurrent INFLIGHT=2
        // sessions stay within the BFCArena budget.
        if cuda_cap == 0 {
            tracing::warn!(
                resolved,
                "sidecar_batch_size: CUDA batch cap resolved to 0 — likely a \
                 misconfiguration. Clamping to 1. \
                 Check TRUSTY_CUDA_SIDECAR_BATCH_CAP."
            );
        }
        resolved.min(cuda_cap)
    } else {
        resolved
    };
    // Guard: a zero batch size is invalid — the sidecar would receive
    // TRUSTY_EMBED_BATCH_SIZE=0 which ONNX Runtime rejects. Clamp to 1.
    raw.max(1)
}

fn parse_env<T: std::str::FromStr + Copy>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

// ── Supervisor ───────────────────────────────────────────────────────────────

/// Supervisor for the `trusty-embedderd` sidecar process.
///
/// Why: the search daemon's embed calls go through
/// `Arc<RwLock<Arc<dyn EmbedderClient>>>`. The supervisor atomically swaps in
/// a fresh `StdioEmbedderClient` whenever the sidecar crashes and respawns,
/// so callers see uninterrupted service after a short respawn delay.
///
/// What: spawns the child with `--stdio` on construction (`spawn_stdio`).
/// `start_supervisor_task` detaches a background task that calls `child.wait()`
/// in a loop. On non-zero exit it applies exponential back-off, respawns, and
/// swaps the live client pointer. On `max_restarts` consecutive failures it
/// logs an error and stops trying.
///
/// `child_pid_slot` is an `Arc<AtomicU32>` shared with callers so they can
/// read the current OS PID of the sidecar without holding any lock. The slot
/// is updated to 0 whenever the sidecar exits and to the new PID whenever a
/// fresh process is spawned.
///
/// Test: `supervisor_spawns_mock_child_and_embeds`,
/// `supervisor_restarts_on_crash` (integration; requires the binary built).
pub struct EmbedderSupervisor {
    binary_path: PathBuf,
    /// The child handle — kept so `shutdown` can send SIGTERM.
    child: Arc<tokio::sync::Mutex<Option<Child>>>,
    /// Pointer shared with callers; swapped on each respawn.
    client_slot: Arc<RwLock<Arc<dyn EmbedderClient>>>,
    /// Current OS PID of the sidecar process. 0 = no live process.
    /// Shared with callers so they can read the PID without acquiring
    /// the child mutex (e.g. for RSS sampling in the reindex poller).
    child_pid_slot: Arc<AtomicU32>,
    /// Health receiver for the *currently live* client's reader task
    /// (issues #1448/#1450). `supervision_loop` races this alongside
    /// `child.wait()` and re-fetches a fresh receiver from each respawned
    /// client (see `StdioEmbedderClient::unhealthy_signal`).
    unhealthy_signal: watch::Receiver<bool>,
    config: SupervisorConfig,
}

impl EmbedderSupervisor {
    /// Spawn `trusty-embedderd --stdio` and return a `(supervisor, client_slot,
    /// child_pid_slot)` triple.
    ///
    /// Why: the caller keeps the `client_slot` behind an `Arc<RwLock<…>>` so
    /// `embed_batch` always reads the current live client. The supervisor keeps
    /// a clone of the same slot and swaps it on each respawn.
    /// `child_pid_slot` lets the caller sample the sidecar's OS PID for RSS
    /// monitoring (issue #282) without holding any mutex; the supervisor updates
    /// it automatically on spawn and exit.
    ///
    /// What: spawns the child with `Stdio::piped()` for both stdin and stdout,
    /// `Stdio::inherit()` for stderr (so the child's logs flow to the parent's
    /// stderr), and `kill_on_drop(true)` as a safety net.  Extracts the pipe
    /// handles, constructs `StdioEmbedderClient`, stores it in `client_slot`.
    /// Sets `child_pid_slot` to the fresh process's OS PID.
    ///
    /// Test: `supervisor_spawns_mock_child_and_embeds`.
    pub async fn spawn_stdio(
        binary_path: impl Into<PathBuf>,
        config: SupervisorConfig,
    ) -> Result<(Self, Arc<RwLock<Arc<dyn EmbedderClient>>>, Arc<AtomicU32>)> {
        let binary_path = binary_path.into();
        let (child, client) = spawn_child(&binary_path, &config).await?;

        // Capture the health signal (#1448/#1450) before erasing `client`
        // into the `Arc<dyn EmbedderClient>` trait object below — the trait
        // itself has no notion of reader health, only `StdioEmbedderClient`
        // does.
        let unhealthy_signal = client.unhealthy_signal();

        // Capture the initial PID before moving `child` into the Arc<Mutex>.
        let initial_pid: u32 = child.id().unwrap_or(0);
        let child_pid_slot = Arc::new(AtomicU32::new(initial_pid));

        let client_slot: Arc<RwLock<Arc<dyn EmbedderClient>>> =
            Arc::new(RwLock::new(Arc::new(client)));
        let client_slot_clone = Arc::clone(&client_slot);
        let child_pid_slot_clone = Arc::clone(&child_pid_slot);

        let supervisor = Self {
            binary_path,
            child: Arc::new(tokio::sync::Mutex::new(Some(child))),
            client_slot,
            child_pid_slot,
            unhealthy_signal,
            config,
        };

        Ok((supervisor, client_slot_clone, child_pid_slot_clone))
    }

    /// Detach the supervisor background task.
    ///
    /// Why: the search daemon calls this once after `spawn_stdio` and then
    /// forgets about the supervisor — all restart logic runs in the background.
    /// Returning a [`SupervisorHandle`] (rather than nothing, as before issue
    /// #2979) is what makes cooperative shutdown reachable after detaching:
    /// the caller keeps the handle instead of losing all access to the
    /// (now-moved) supervisor.
    ///
    /// What: consumes `self` and spawns a Tokio task that races `child.wait()`
    /// against the live client's `unhealthy_signal` (#1448/#1450) and a
    /// shutdown flag (#2979) in a loop. On non-zero exit, or on an unhealthy
    /// signal (which force-kills the still-running child first): exponential
    /// back-off, respawn, swap in new client. On `max_restarts` consecutive
    /// failures: log ERROR and stop. On the returned handle's `shutdown()`:
    /// kill and reap the child, then stop — never respawn. `child_pid_slot` is
    /// updated to the new PID on each respawn and cleared to 0 when the
    /// sidecar exits for the last time.
    ///
    /// Test: `supervisor_restarts_on_crash`,
    /// `supervisor_shutdown_handle_is_reachable_and_stops_child`,
    /// `supervisor_intentional_shutdown_does_not_respawn`.
    pub fn start_supervisor_task(self) -> SupervisorHandle {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let join = tokio::spawn(supervision_loop(
            self.binary_path,
            self.child,
            self.client_slot,
            self.child_pid_slot,
            self.unhealthy_signal,
            self.config,
            shutdown_rx,
        ));
        SupervisorHandle { shutdown_tx, join }
    }

    /// Terminate the sidecar and stop supervising.
    ///
    /// Why: clean shutdown path — sends SIGTERM to the child so it can flush
    /// any in-flight work and exit cleanly (the daemon's stdin-EOF handling
    /// provides a secondary signal).
    ///
    /// What: takes the `Child` out of the mutex, calls `child.kill()` (async
    /// SIGKILL on Unix if SIGTERM already happened, or sends SIGTERM on first
    /// call), then waits for it to exit.
    ///
    /// Test: `supervisor_shutdown_kills_child`.
    pub async fn shutdown(self) {
        let mut guard = self.child.lock().await;
        if let Some(mut child) = guard.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
            tracing::info!("EmbedderSupervisor: sidecar terminated on shutdown");
        }
    }
}

/// Cooperative shutdown handle for a detached supervision task (issue #2979).
///
/// Why: `start_supervisor_task` consumes `self`, so once a caller detaches
/// the background task it no longer holds an `EmbedderSupervisor` to call
/// `shutdown()` on — that method is unreachable from that point on. This
/// handle is the reachable replacement: it is returned BY
/// `start_supervisor_task`, so the caller always has a way to stop the
/// sidecar cooperatively, no matter how long ago it detached.
///
/// What: wraps the `watch::Sender<bool>` half of the shutdown flag the
/// supervision loop selects on, plus the loop's `JoinHandle` so `shutdown()`
/// can await confirmation that the child was actually killed and reaped
/// before returning — the same "confirmed dead" guarantee the old
/// self-consuming `shutdown()` gave callers who never detached.
///
/// Test: `supervisor_shutdown_handle_is_reachable_and_stops_child`,
/// `supervisor_intentional_shutdown_does_not_respawn`.
#[must_use = "dropping the handle without calling shutdown() leaves the \
              sidecar running in the background with no way to stop it \
              cooperatively later"]
pub struct SupervisorHandle {
    shutdown_tx: watch::Sender<bool>,
    join: tokio::task::JoinHandle<()>,
}

impl SupervisorHandle {
    /// Cooperatively stop the supervised sidecar and its supervision loop.
    ///
    /// Why: replaces the out-of-band PID kill trusty-search's idle watchdog
    /// used before this fix (issue #2979) — that raw SIGTERM/SIGKILL raced
    /// `child.wait()` in the supervision loop, which had no way to
    /// distinguish an intentional stop from a crash and respawned the sidecar
    /// the caller had just stopped.
    ///
    /// What: flips the shared shutdown flag to `true` (a send on a channel
    /// with no live receiver — which only happens if the loop already exited
    /// on its own, e.g. a clean-exit or a crash-storm give-up — is harmless
    /// and ignored), then awaits the loop's `JoinHandle`. The loop observes
    /// the flag (see `supervision_loop`'s shutdown branches), kills the
    /// child, waits for it to exit, clears the PID slot, and returns without
    /// respawning. A join error (the task panicked) is logged, not
    /// propagated, since by definition the task — and therefore the sidecar
    /// it supervised — is gone either way.
    ///
    /// Test: `supervisor_shutdown_handle_is_reachable_and_stops_child`.
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(true);
        if let Err(e) = self.join.await {
            tracing::warn!("EmbedderSupervisor: supervision task join error on shutdown: {e}");
        }
    }
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Spawn the sidecar and return the `(Child, StdioEmbedderClient)` pair.
///
/// Why: extracted so both the initial spawn and the respawn path call the same
/// code.
/// What: `Command::new(binary_path).arg("--stdio")` with piped stdin/stdout
/// and inherited stderr. When `config.sidecar_batch_size` is `Some(n)`, sets
/// `TRUSTY_EMBED_BATCH_SIZE=n` (issue #747 Fix C).
/// Test: called by `spawn_stdio` and the supervision loop.
async fn spawn_child(
    binary_path: &Path,
    config: &SupervisorConfig,
) -> Result<(Child, StdioEmbedderClient)> {
    use std::process::Stdio;

    let mut cmd = Command::new(binary_path);
    cmd.arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);

    // Forward resolved ONNX batch size (issue #747 Fix C).
    if let Some(bs) = config.sidecar_batch_size {
        cmd.env("TRUSTY_EMBED_BATCH_SIZE", bs.to_string());
        tracing::debug!(
            bs,
            "EmbedderSupervisor: forwarding TRUSTY_EMBED_BATCH_SIZE={bs}"
        );
    }

    let mut child = cmd.spawn().with_context(|| {
        format!(
            "spawn trusty-embedderd --stdio from {}",
            binary_path.display()
        )
    })?;

    let stdin = child
        .stdin
        .take()
        .context("child stdin handle missing (expected Stdio::piped)")?;
    let stdout = child
        .stdout
        .take()
        .context("child stdout handle missing (expected Stdio::piped)")?;

    let client = StdioEmbedderClient::new(stdin, stdout);

    // Startup probe: send an empty embed request and wait up to
    // `startup_timeout_secs` for a response. An empty batch short-circuits
    // in `StdioEmbedderClient::embed_batch` before sending anything — we
    // need a real (non-empty) probe to verify the process is alive and
    // responding.
    //
    // We probe with a single known-innocuous text rather than a heavyweight
    // model call: the daemon's `BatchQueue` will embed it once and discard the
    // result. The cost is ~1 ONNX call on startup, which is negligible
    // compared to model-load time.
    let probe_result = tokio::time::timeout(
        Duration::from_secs(config.startup_timeout_secs),
        client.embed_batch(vec!["trusty-embedderd startup probe".to_string()]),
    )
    .await;

    match probe_result {
        Ok(Ok(_)) => {
            tracing::info!(
                binary = %binary_path.display(),
                "EmbedderSupervisor: sidecar started and responding"
            );
        }
        Ok(Err(e)) => {
            anyhow::bail!("sidecar startup probe failed: {e}");
        }
        Err(_elapsed) => {
            anyhow::bail!(
                "sidecar did not respond within {}s (TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS={})",
                config.startup_timeout_secs,
                config.startup_timeout_secs
            );
        }
    }

    Ok((child, client))
}

/// Why a process-exit wait and an unhealthy-signal wait produce different
/// restart handling: a clean exit (code 0) stops supervision entirely, but an
/// unhealthy signal never carries an exit code (the process may still be
/// running) and always means "force a restart".
enum RestartTrigger {
    /// `child.wait()` resolved — the process actually exited.
    ProcessExit(std::process::ExitStatus),
    /// `unhealthy_signal` fired (#1448/#1450) — reader died or the sidecar
    /// was judged wedged. The child may still be alive and must be killed.
    Unhealthy,
}

// ── Wedge-restart-storm prevention (#1450 HIGH follow-up) ──────────────────
//
// A workload-deterministic wedge (the sidecar reliably wedges again shortly
// after every respawn because the WORKLOAD — not the process — triggers the
// stall) previously never escalated: `consecutive_failures` reset to 0 on
// every successful respawn (the respawn *probe* itself succeeds), so the
// detect(90s)→kill→backoff→reset→re-wedge cycle repeated forever without ever
// tripping `max_restarts`. `consecutive_wedge_restarts` is a second counter,
// tracked alongside `consecutive_failures` in `supervision_loop`, that is
// reset ONLY after sustained real-world health — never by an ordinary
// respawn-probe success. The two pure functions below implement its
// reset/give-up decisions so that logic is unit-testable without driving the
// full async supervision loop.

/// Decide whether the wedge-restart escalation counter should reset.
///
/// Why: extracted as a pure function (no I/O, no locking, no `Instant::now()`
/// call) so the reset decision is directly unit-testable.
/// What: `true` iff a prior wedge-restart happened (`elapsed_since_last_wedge`
/// is `Some`) and at least `wedge_reset_secs` have elapsed since then. `None`
/// (no prior wedge this supervision run) never resets — there is nothing to
/// reset.
/// Test: `wedge_counter_should_reset_*` in `supervisor_tests.rs`.
fn wedge_counter_should_reset(
    elapsed_since_last_wedge: Option<Duration>,
    wedge_reset_secs: u64,
) -> bool {
    match elapsed_since_last_wedge {
        Some(elapsed) => elapsed >= Duration::from_secs(wedge_reset_secs),
        None => false,
    }
}

/// Decide whether the supervisor should give up (stop respawning).
///
/// Why: extracted as a pure function so the "storm" ceiling check — which now
/// considers TWO independent counters instead of one — is directly
/// unit-testable. Either counter alone exceeding `max_restarts` is
/// sufficient: an ordinary crash storm (`consecutive_failures`) or a wedge
/// storm that keeps recurring despite each individual respawn's startup
/// probe succeeding (`consecutive_wedge_restarts`).
/// What: `consecutive_failures > max_restarts || consecutive_wedge_restarts >
/// max_restarts`.
/// Test: `should_give_up_*` in `supervisor_tests.rs`.
fn should_give_up(
    consecutive_failures: u32,
    consecutive_wedge_restarts: u32,
    max_restarts: u32,
) -> bool {
    consecutive_failures > max_restarts || consecutive_wedge_restarts > max_restarts
}

/// Background supervision loop.
///
/// Why: runs as a detached Tokio task so the parent daemon never blocks on it.
/// What: races `child.wait()` against `unhealthy_signal.changed()`
/// (#1448/#1450) and `shutdown_rx.changed()` (#2979) each iteration. A
/// process exit is handled as before (success → stop supervising; failure →
/// back-off + respawn). An unhealthy signal force-kills the still-running
/// child first, then falls into the same back-off + respawn path as a crash.
/// A shutdown signal kills and reaps the child itself and returns
/// immediately — it never enters the crash-restart path, so it can never be
/// misclassified as a failure and respawned. The respawn back-off sleep also
/// races the shutdown signal so a shutdown requested mid-backoff stops
/// promptly instead of waiting out the delay and respawning first. Exits when
/// the process exits cleanly (code 0), shutdown is requested, or when either
/// restart ceiling (`should_give_up`) is exceeded.
/// `child_pid_slot` is updated to the new PID after each successful respawn
/// and cleared to 0 when supervision terminates so RSS samplers stop sampling
/// a dead PID.
///
/// Restart-storm prevention (#1450 HIGH follow-up): `consecutive_wedge_restarts`
/// tracks wedge-triggered restarts specifically and — unlike
/// `consecutive_failures` — is NOT reset by an ordinary respawn-probe
/// success; it only resets once `wedge_counter_should_reset` observes
/// `config.wedge_reset_secs` of sustained health since the last wedge. Both
/// counters feed `should_give_up` and both drive the exponential back-off, so
/// a workload-deterministic wedge that recurs after every respawn eventually
/// trips `max_restarts` instead of cycling forever at a 1s backoff.
/// Test: `supervisor_restarts_on_crash`,
/// `supervisor_intentional_shutdown_does_not_respawn`.
/// Test-only iteration counter (issue #3023 regression test): bumped once per
/// `supervision_loop` pass so `supervisor_tests.rs` can assert the loop
/// blocks normally — rather than busy-spinning — once `shutdown_rx`'s sender
/// closes without a shutdown ever being requested. Compiled out entirely in
/// non-test builds.
#[cfg(test)]
pub(crate) static SUPERVISION_LOOP_ITERATIONS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

async fn supervision_loop(
    binary_path: PathBuf,
    child_slot: Arc<tokio::sync::Mutex<Option<Child>>>,
    client_slot: Arc<RwLock<Arc<dyn EmbedderClient>>>,
    child_pid_slot: Arc<AtomicU32>,
    mut unhealthy_signal: watch::Receiver<bool>,
    config: SupervisorConfig,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut consecutive_failures: u32 = 0;
    let mut consecutive_wedge_restarts: u32 = 0;
    let mut last_wedge_restart_at: Option<tokio::time::Instant> = None;
    // Set once `shutdown_rx` observes its sender closed (issue #3023 HIGH):
    // `watch::Receiver::changed()` resolves `Ready(Err(_))` on every
    // subsequent poll once the sender drops without ever calling
    // `.shutdown()`, so leaving that `select!` branch active would busy-spin
    // this loop for the remaining lifetime of the child. Once set, the
    // shutdown branches below are permanently excluded from the `select!` —
    // supervision continues via `child.wait()` / `unhealthy_signal` only.
    let mut shutdown_closed = false;

    loop {
        #[cfg(test)]
        SUPERVISION_LOOP_ITERATIONS.fetch_add(1, AtomicOrdering::Relaxed);

        // Reset the wedge-restart escalation counter once sustained health
        // has been observed since the last wedge — see `wedge_counter_should_reset`.
        if wedge_counter_should_reset(
            last_wedge_restart_at.map(|t| t.elapsed()),
            config.wedge_reset_secs,
        ) {
            tracing::info!(
                "EmbedderSupervisor: {}s without a further wedge — resetting wedge-restart \
                 escalation counter (was {consecutive_wedge_restarts})",
                config.wedge_reset_secs,
            );
            consecutive_wedge_restarts = 0;
            last_wedge_restart_at = None;
        }

        // Race the child actually exiting against the live client reporting
        // itself unhealthy (#1448/#1450) — whichever happens first drives
        // this iteration.
        let trigger = {
            let mut guard = child_slot.lock().await;
            match guard.as_mut() {
                Some(child) => {
                    tokio::select! {
                        wait_result = child.wait() => match wait_result {
                            Ok(status) => RestartTrigger::ProcessExit(status),
                            Err(e) => {
                                tracing::error!("EmbedderSupervisor: wait() failed: {e}");
                                child_pid_slot.store(0, AtomicOrdering::Release);
                                return;
                            }
                        },
                        changed = unhealthy_signal.changed() => {
                            if changed.is_err() {
                                // The sender side closed without ever firing —
                                // treat conservatively as "keep waiting on
                                // child.wait() only" by looping back around.
                                tracing::debug!(
                                    "EmbedderSupervisor: unhealthy_signal channel closed \
                                     without firing — continuing to watch child.wait()"
                                );
                                continue;
                            }
                            RestartTrigger::Unhealthy
                        }
                        changed_result = shutdown_rx.changed(), if !shutdown_closed => {
                            // Sender (SupervisorHandle) dropped without ever
                            // calling `.shutdown()` — permanently disable this
                            // branch (see `shutdown_closed` above) instead of
                            // looping back into an immediately-ready `Err`
                            // forever (issue #3023 HIGH).
                            let Ok(()) = changed_result else {
                                shutdown_closed = true;
                                continue;
                            };
                            if !*shutdown_rx.borrow() {
                                // Spurious wake with the flag still false —
                                // nothing to do, keep watching the child.
                                continue;
                            }
                            // Cooperative shutdown (issue #2979): kill and reap
                            // the child ourselves and return directly — this
                            // path never touches `RestartTrigger`, so it can
                            // never be misclassified as a crash and respawned,
                            // unlike the old out-of-band PID kill it replaces.
                            tracing::info!(
                                "EmbedderSupervisor: shutdown requested — stopping the \
                                 sidecar and supervision loop (no respawn)"
                            );
                            if let Err(e) = child.start_kill() {
                                tracing::warn!(
                                    "EmbedderSupervisor: start_kill on shutdown failed \
                                     (may have already exited): {e}"
                                );
                            }
                            let _ = child.wait().await;
                            child_pid_slot.store(0, AtomicOrdering::Release);
                            return;
                        }
                    }
                }
                None => {
                    // Sidecar was explicitly shut down; stop supervising.
                    child_pid_slot.store(0, AtomicOrdering::Release);
                    return;
                }
            }
        };

        let is_wedge_restart = matches!(trigger, RestartTrigger::Unhealthy);

        match trigger {
            RestartTrigger::ProcessExit(exit_status) => {
                child_pid_slot.store(0, AtomicOrdering::Release);
                if exit_status.success() {
                    tracing::info!(
                        "EmbedderSupervisor: sidecar exited cleanly — stopping supervision"
                    );
                    return;
                }
                tracing::warn!(
                    "EmbedderSupervisor: sidecar exited with {:?} (failure #{}/{})",
                    exit_status.code(),
                    consecutive_failures + 1,
                    config.max_restarts,
                );
            }
            RestartTrigger::Unhealthy => {
                consecutive_wedge_restarts += 1;
                last_wedge_restart_at = Some(tokio::time::Instant::now());
                tracing::warn!(
                    "EmbedderSupervisor: sidecar client reported unhealthy (reader task \
                     died, or accumulating call timeouts indicate a wedged process) — \
                     forcing restart (failure #{}/{}, wedge-restart #{}/{})",
                    consecutive_failures + 1,
                    config.max_restarts,
                    consecutive_wedge_restarts,
                    config.max_restarts,
                );
                // The process may still be running (that's the whole point of
                // this signal) — force it down before respawning.
                let mut guard = child_slot.lock().await;
                if let Some(mut child) = guard.take() {
                    if let Err(e) = child.start_kill() {
                        tracing::warn!(
                            "EmbedderSupervisor: start_kill on wedged sidecar failed \
                             (may have already exited): {e}"
                        );
                    }
                    let _ = child.wait().await;
                }
                drop(guard);
                child_pid_slot.store(0, AtomicOrdering::Release);
            }
        }

        consecutive_failures += 1;

        if should_give_up(
            consecutive_failures,
            consecutive_wedge_restarts,
            config.max_restarts,
        ) {
            tracing::error!(
                "EmbedderSupervisor: exceeded max_restarts={} ({}) — giving up. \
                 Set TRUSTY_EMBEDDERD_MAX_RESTARTS to increase the limit, or \
                 TRUSTY_EMBEDDERD_WEDGE_RESET_SECS if wedges are recurring faster \
                 than the sustained-health reset window.",
                config.max_restarts,
                if consecutive_wedge_restarts > config.max_restarts {
                    "wedge-restart storm — recurring despite successful respawns"
                } else {
                    "process-exit crash storm"
                },
            );
            return;
        }

        // Exponential back-off: 1s, 2s, 4s, …, capped at backoff_max_secs.
        // A wedge-triggered restart backs off against its own (non-resetting)
        // counter so a recurring wedge escalates delay across cycles instead
        // of returning to 1s every time the respawn probe itself succeeds.
        let backoff_attempt = if is_wedge_restart {
            consecutive_wedge_restarts
        } else {
            consecutive_failures
        };
        let delay_secs = (1u64 << backoff_attempt.min(16)).min(config.backoff_max_secs);
        tracing::info!(
            "EmbedderSupervisor: restarting sidecar in {delay_secs}s (attempt \
             {consecutive_failures}{})",
            if is_wedge_restart {
                format!(", wedge-triggered, wedge-restart #{consecutive_wedge_restarts}")
            } else {
                String::new()
            },
        );
        // Race the back-off delay against shutdown (issue #2979) so a
        // shutdown requested while a respawn is pending stops immediately
        // instead of waiting out the delay and respawning anyway.
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(delay_secs)) => {}
            // Closed (`Err`) means the sender dropped without a shutdown
            // request ever firing — `changed()` resolves `Ready(Err(_))`
            // immediately, so left unguarded this branch would win the race
            // every time and skip the intended back-off delay, respawning
            // immediately on every subsequent pass (issue #3023 MEDIUM).
            // Disable the branch and honor the full delay here instead;
            // `Ok(())` with the flag still false is a spurious wake — do
            // nothing and fall through to the respawn below either way.
            changed_result = shutdown_rx.changed(), if !shutdown_closed => {
                match changed_result {
                    Err(_) => {
                        shutdown_closed = true;
                        tokio::time::sleep(Duration::from_secs(delay_secs)).await;
                    }
                    Ok(()) if *shutdown_rx.borrow() => {
                        tracing::info!(
                            "EmbedderSupervisor: shutdown requested during respawn \
                             back-off — stopping without respawn"
                        );
                        child_pid_slot.store(0, AtomicOrdering::Release);
                        return;
                    }
                    Ok(()) => {}
                }
            }
        }

        // Respawn.
        match spawn_child(&binary_path, &config).await {
            Ok((new_child, new_client)) => {
                // Publish the new PID before swapping the client so any
                // RSS sampler that wakes up after the swap sees a valid PID.
                let new_pid = new_child.id().unwrap_or(0);

                // Capture the new client's health signal (#1448/#1450) before
                // erasing it into the trait object below, and start watching
                // it on the next loop iteration.
                unhealthy_signal = new_client.unhealthy_signal();

                // Swap the live client so subsequent embed calls use the new
                // sidecar. Callers hold `Arc<RwLock<Arc<dyn EmbedderClient>>>`;
                // they `.read().clone()` to get a current handle per call, so
                // this write is seen by the very next embed call.
                {
                    let mut client_guard = client_slot.write().await;
                    *client_guard = Arc::new(new_client);
                }

                // Store the new child in the slot.
                {
                    let mut child_guard = child_slot.lock().await;
                    *child_guard = Some(new_child);
                }

                // Publish the PID after the child handle is in place.
                child_pid_slot.store(new_pid, AtomicOrdering::Release);

                // Reset the ordinary failure count — the new process is up.
                // NOTE: `consecutive_wedge_restarts` is deliberately NOT reset
                // here (see its doc comment and `wedge_counter_should_reset`)
                // — only sustained real-world health resets it.
                consecutive_failures = 0;
                tracing::info!(
                    "EmbedderSupervisor: sidecar restarted successfully (pid={new_pid})"
                );
            }
            Err(e) => {
                tracing::error!("EmbedderSupervisor: respawn failed: {e:#}");
                // Count the failed spawn itself as another failure.
            }
        }
    }
}

/// Locate the `trusty-embedderd` binary for the current build profile.
///
/// Why: the default auto-spawn path needs to find the binary without requiring
/// the operator to set `TRUSTY_EMBEDDERD_BIN`. The search order is:
///   1. `TRUSTY_EMBEDDERD_BIN` env var (explicit override)
///   2. Sibling of `current_exe()` (release build: both binaries are in
///      `target/release/`)
///   3. `trusty-embedderd` on `PATH`
///
/// What: returns the first path at which the file exists and is executable.
/// Returns `Err` if none is found.
///
/// Test: unit test `locate_embedderd_binary_prefers_sibling` (mocked via the
/// explicit override path).
pub fn locate_embedderd_binary() -> Result<PathBuf> {
    // Env override takes precedence over all discovery.
    if let Ok(explicit) = std::env::var("TRUSTY_EMBEDDERD_BIN") {
        let p = PathBuf::from(&explicit);
        if p.is_file() {
            return Ok(p);
        }
        anyhow::bail!("TRUSTY_EMBEDDERD_BIN={explicit:?} does not point to an existing file");
    }

    // Sibling of current_exe (works for both `cargo run` and installed binaries).
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join("trusty-embedderd");
        if sibling.is_file() {
            return Ok(sibling);
        }
        // Windows
        let sibling_exe = dir.join("trusty-embedderd.exe");
        if sibling_exe.is_file() {
            return Ok(sibling_exe);
        }
    }

    // PATH lookup.
    if let Ok(path) = which_embedderd() {
        return Ok(path);
    }

    anyhow::bail!(
        "could not locate trusty-embedderd binary. \
         Set TRUSTY_EMBEDDERD_BIN=/path/to/trusty-embedderd or ensure it is on PATH."
    )
}

/// Minimal `which`-style PATH search for `trusty-embedderd`.
///
/// Why: avoids adding a `which` crate dependency just for this one lookup.
/// What: splits `PATH` by OS separator, appends `trusty-embedderd` (+ `.exe`
/// on Windows), returns the first path that is_file().
/// Test: tested implicitly when the sibling-path lookup fails.
fn which_embedderd() -> Result<PathBuf> {
    let path_var = std::env::var("PATH").unwrap_or_default();
    let sep = if cfg!(windows) { ';' } else { ':' };
    for dir in path_var.split(sep) {
        let candidate = PathBuf::from(dir).join("trusty-embedderd");
        if candidate.is_file() {
            return Ok(candidate);
        }
        #[cfg(windows)]
        {
            let candidate_exe = PathBuf::from(dir).join("trusty-embedderd.exe");
            if candidate_exe.is_file() {
                return Ok(candidate_exe);
            }
        }
    }
    anyhow::bail!("trusty-embedderd not found on PATH")
}

// Tests are in a sibling file to keep this file under its allowlist budget.
// The submodule can access private items via `super::` (Rust child-module rule).
#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod tests;
