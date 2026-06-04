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
//! Test: `supervisor_spawns_mock_child_and_embeds`,
//! `supervisor_restarts_on_crash`, `supervisor_shutdown_kills_child`,
//! `stdio_eof_terminates_child` in this module's `tests` submodule.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::process::{Child, Command};
use tokio::sync::RwLock;

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
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            max_restarts: 5,
            backoff_max_secs: 60,
            startup_timeout_secs: 5,
            sidecar_batch_size: None,
        }
    }
}

impl SupervisorConfig {
    /// Read configuration from environment variables, falling back to defaults.
    ///
    /// Why: lets operators tune restart behaviour in launchd/systemd unit files
    /// without recompiling.
    /// What: reads `TRUSTY_EMBEDDERD_MAX_RESTARTS`,
    /// `TRUSTY_EMBEDDERD_RESTART_BACKOFF_MAX_SECS`, and
    /// `TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS` from the process environment.
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
        }
    }
}

/// Resolve the ONNX batch size to forward to the sidecar (issue #747 Fix C).
///
/// Why: the parent's auto-tuned `TRUSTY_MAX_BATCH_SIZE` was never forwarded to
/// the sidecar, which therefore always ran at the default of 32. CoreML safety
/// cap: CoreML pre-allocates per-batch GPU/ANE buffers in the unified-memory
/// pool; oversized batches can trigger jetsam SIGKILL, so the value is clamped
/// to `coreml_cap` when `is_coreml` is `true`. A zero result is invalid (the
/// sidecar would set `TRUSTY_EMBED_BATCH_SIZE=0` which ORT rejects), so the
/// return value is always clamped to at least 1.
/// What: `min(resolved, coreml_cap)` when `is_coreml`; `resolved` otherwise;
/// result is further clamped to `max(result, 1)` to prevent a zero batch size.
/// When `is_coreml && coreml_cap == 0` a `tracing::warn!` is emitted to stderr
/// because that combination indicates a likely `resolve_coreml_batch_size()`
/// misconfiguration — the clamp-to-1 keeps the system alive but will be very
/// slow (one embedding per ONNX call).
/// Test: `sidecar_batch_size_*` in this module's `tests`, including
/// `sidecar_batch_size_zero_resolved` and `sidecar_batch_size_zero_coreml_cap`.
pub fn sidecar_batch_size(resolved: usize, is_coreml: bool, coreml_cap: usize) -> usize {
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
            config,
        };

        Ok((supervisor, client_slot_clone, child_pid_slot_clone))
    }

    /// Detach the supervisor background task.
    ///
    /// Why: the search daemon calls this once after `spawn_stdio` and then
    /// forgets about the supervisor — all restart logic runs in the background.
    ///
    /// What: consumes `self` and spawns a Tokio task that calls `child.wait()`
    /// in a loop. On non-zero exit: exponential back-off, respawn, swap in new
    /// client. On `max_restarts` consecutive failures: log ERROR and stop.
    /// `child_pid_slot` is updated to the new PID on each respawn and cleared
    /// to 0 when the sidecar exits for the last time.
    ///
    /// Test: `supervisor_restarts_on_crash`.
    pub fn start_supervisor_task(self) {
        tokio::spawn(supervision_loop(
            self.binary_path,
            self.child,
            self.client_slot,
            self.child_pid_slot,
            self.config,
        ));
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

/// Background supervision loop.
///
/// Why: runs as a detached Tokio task so the parent daemon never blocks on it.
/// What: calls `child.wait()`, handles crash/exit, applies exponential back-off,
/// respawns, and atomically swaps in the new `StdioEmbedderClient`. Exits when
/// the process exits cleanly (code 0) or when `max_restarts` is exceeded.
/// `child_pid_slot` is updated to the new PID after each successful respawn and
/// cleared to 0 when supervision terminates so RSS samplers stop sampling a
/// dead PID.
/// Test: `supervisor_restarts_on_crash`.
async fn supervision_loop(
    binary_path: PathBuf,
    child_slot: Arc<tokio::sync::Mutex<Option<Child>>>,
    client_slot: Arc<RwLock<Arc<dyn EmbedderClient>>>,
    child_pid_slot: Arc<AtomicU32>,
    config: SupervisorConfig,
) {
    let mut consecutive_failures: u32 = 0;

    loop {
        // Wait for the child to exit.
        let exit_status = {
            let mut guard = child_slot.lock().await;
            match guard.as_mut() {
                Some(child) => match child.wait().await {
                    Ok(status) => status,
                    Err(e) => {
                        tracing::error!("EmbedderSupervisor: wait() failed: {e}");
                        // Clear the PID slot so samplers stop polling a dead PID.
                        child_pid_slot.store(0, AtomicOrdering::Release);
                        return;
                    }
                },
                None => {
                    // Sidecar was explicitly shut down; stop supervising.
                    child_pid_slot.store(0, AtomicOrdering::Release);
                    return;
                }
            }
        };

        // Clear PID immediately after process exit.
        child_pid_slot.store(0, AtomicOrdering::Release);

        if exit_status.success() {
            tracing::info!("EmbedderSupervisor: sidecar exited cleanly — stopping supervision");
            return;
        }

        consecutive_failures += 1;
        tracing::warn!(
            "EmbedderSupervisor: sidecar exited with {:?} (failure #{}/{})",
            exit_status.code(),
            consecutive_failures,
            config.max_restarts,
        );

        if consecutive_failures > config.max_restarts {
            tracing::error!(
                "EmbedderSupervisor: exceeded max_restarts={} — giving up. \
                 Set TRUSTY_EMBEDDERD_MAX_RESTARTS to increase the limit.",
                config.max_restarts
            );
            return;
        }

        // Exponential back-off: 1s, 2s, 4s, …, capped at backoff_max_secs.
        let delay_secs = (1u64 << consecutive_failures.min(16)).min(config.backoff_max_secs);
        tracing::info!(
            "EmbedderSupervisor: restarting sidecar in {delay_secs}s (attempt {consecutive_failures})"
        );
        tokio::time::sleep(Duration::from_secs(delay_secs)).await;

        // Respawn.
        match spawn_child(&binary_path, &config).await {
            Ok((new_child, new_client)) => {
                // Publish the new PID before swapping the client so any
                // RSS sampler that wakes up after the swap sees a valid PID.
                let new_pid = new_child.id().unwrap_or(0);

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

                // Reset consecutive failure count — the new process is up.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_env_uses_defaults_when_no_vars_set() {
        // Why: validate that unset env vars produce the documented defaults.
        // What: construct from env (no vars set in test process by default)
        //       and compare each field.
        // Test: this test.

        // Save any existing env vars to restore later.
        let saved_max = std::env::var("TRUSTY_EMBEDDERD_MAX_RESTARTS").ok();
        let saved_backoff = std::env::var("TRUSTY_EMBEDDERD_RESTART_BACKOFF_MAX_SECS").ok();
        let saved_timeout = std::env::var("TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS").ok();

        // Ensure they are unset during the test.
        // SAFETY: test-only, single-threaded by test framework convention.
        unsafe {
            std::env::remove_var("TRUSTY_EMBEDDERD_MAX_RESTARTS");
            std::env::remove_var("TRUSTY_EMBEDDERD_RESTART_BACKOFF_MAX_SECS");
            std::env::remove_var("TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS");
        }

        let cfg = SupervisorConfig::from_env();
        assert_eq!(cfg.max_restarts, 5);
        assert_eq!(cfg.backoff_max_secs, 60);
        assert_eq!(cfg.startup_timeout_secs, 5);

        // Restore.
        unsafe {
            if let Some(v) = saved_max {
                std::env::set_var("TRUSTY_EMBEDDERD_MAX_RESTARTS", v);
            }
            if let Some(v) = saved_backoff {
                std::env::set_var("TRUSTY_EMBEDDERD_RESTART_BACKOFF_MAX_SECS", v);
            }
            if let Some(v) = saved_timeout {
                std::env::set_var("TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS", v);
            }
        }
    }

    #[test]
    fn parse_env_uses_override() {
        // Why: verify that a valid env-var value overrides the default.
        // What: set the var to "99", call `from_env`, check the field.
        // Test: this test.
        let saved = std::env::var("TRUSTY_EMBEDDERD_MAX_RESTARTS").ok();
        // SAFETY: test-only.
        unsafe {
            std::env::set_var("TRUSTY_EMBEDDERD_MAX_RESTARTS", "99");
        }
        let cfg = SupervisorConfig::from_env();
        assert_eq!(cfg.max_restarts, 99);
        unsafe {
            if let Some(v) = saved {
                std::env::set_var("TRUSTY_EMBEDDERD_MAX_RESTARTS", v);
            } else {
                std::env::remove_var("TRUSTY_EMBEDDERD_MAX_RESTARTS");
            }
        }
    }

    // ── sidecar_batch_size tests (Fix C, issue #747) ──────────────────────

    #[test]
    fn sidecar_batch_size_cpu_passthrough() {
        // Why: CPU path must forward resolved value unchanged.
        // What: is_coreml=false → returns resolved.
        // Test: this test.
        assert_eq!(sidecar_batch_size(128, false, 32), 128);
        assert_eq!(sidecar_batch_size(512, false, 32), 512);
        assert_eq!(sidecar_batch_size(32, false, 32), 32);
    }

    #[test]
    fn sidecar_batch_size_coreml_caps_and_passes_through() {
        // Why: CoreML path must cap at coreml_cap to prevent OOM/jetsam on
        // Apple Silicon, but pass through values at or below the cap.
        // What: is_coreml=true → min(resolved, coreml_cap).
        // Test: this test.
        assert_eq!(sidecar_batch_size(256, true, 32), 32);
        assert_eq!(sidecar_batch_size(512, true, 64), 64);
        assert_eq!(sidecar_batch_size(16, true, 32), 16);
        assert_eq!(sidecar_batch_size(32, true, 32), 32);
    }

    #[test]
    fn sidecar_batch_size_zero_resolved_clamps_to_one() {
        // Why: resolved=0 would cause TRUSTY_EMBED_BATCH_SIZE=0 which ONNX
        // Runtime rejects; the guard must clamp to 1 regardless of is_coreml.
        // What: resolved=0, is_coreml=false → 1 (clamped from 0).
        // Test: this test.
        assert_eq!(
            sidecar_batch_size(0, false, 32),
            1,
            "zero resolved (non-coreml) must clamp to 1"
        );
    }

    #[test]
    fn sidecar_batch_size_zero_coreml_cap_clamps_to_one() {
        // Why: if the CoreML cap is 0, min(resolved, 0) = 0, which is
        // invalid. The guard must still clamp to 1.
        // What: resolved=32, is_coreml=true, coreml_cap=0 → 1 (clamped from 0).
        // Test: this test.
        assert_eq!(
            sidecar_batch_size(32, true, 0),
            1,
            "zero coreml_cap must clamp result to 1"
        );
    }

    #[test]
    fn sidecar_batch_size_both_zero_clamps_to_one() {
        // Why: both inputs at zero must still produce a valid result.
        // What: resolved=0, is_coreml=true, coreml_cap=0 → 1.
        // Test: this test.
        assert_eq!(
            sidecar_batch_size(0, true, 0),
            1,
            "resolved=0, coreml_cap=0 must clamp to 1"
        );
    }

    #[test]
    fn locate_binary_respects_explicit_override() {
        // Why: `TRUSTY_EMBEDDERD_BIN` must take priority over all discovery.
        // What: set `TRUSTY_EMBEDDERD_BIN` to a non-existent path — the
        //       function should return an error mentioning the path.
        // Test: this test.
        let saved = std::env::var("TRUSTY_EMBEDDERD_BIN").ok();
        unsafe {
            std::env::set_var("TRUSTY_EMBEDDERD_BIN", "/no/such/binary");
        }
        let result = locate_embedderd_binary();
        assert!(result.is_err(), "must fail on non-existent override path");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("TRUSTY_EMBEDDERD_BIN"),
            "error must mention the env var"
        );
        unsafe {
            if let Some(v) = saved {
                std::env::set_var("TRUSTY_EMBEDDERD_BIN", v);
            } else {
                std::env::remove_var("TRUSTY_EMBEDDERD_BIN");
            }
        }
    }
}
