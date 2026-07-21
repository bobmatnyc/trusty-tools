//! Configuration and batch-size tuning for `EmbedderSupervisor`.
//!
//! Why: split out of `supervisor.rs` (issue #3524 slice 5 CI follow-up) to
//! keep that file under the 500-SLOC production cap. This module is a
//! self-contained seam: `SupervisorConfig` and the CUDA/CoreML batch-size
//! resolution helpers have no dependency on the child-process/watch-channel
//! machinery in `supervisor.rs` — they are pure config plumbing that
//! `spawn_child` (in `supervisor.rs`) reads from, not the other way around.
//!
//! What: `SupervisorConfig` (the tunable-knobs struct read via
//! `SupervisorConfig::from_env()`), the CUDA sidecar batch cap
//! (`DEFAULT_CUDA_SIDECAR_BATCH_CAP` / `cuda_sidecar_batch_cap()`), the
//! `sidecar_batch_size` resolution function, and the shared `parse_env`
//! helper. Re-exported from `supervisor.rs` via `pub use` so the
//! `embedder_client` module's public API (`SupervisorConfig`,
//! `cuda_sidecar_batch_cap`, `sidecar_batch_size`) is unchanged.
//!
//! Test: `supervisor_tests.rs` (`from_env_uses_defaults`, `sidecar_batch_size_*`)
//! exercises this module via `use super::*;` in `supervisor.rs`'s test
//! submodule.

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
