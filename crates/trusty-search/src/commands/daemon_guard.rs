//! Auto-start the daemon when a CLI command needs it.
//!
//! Why: most CLI subcommands (query, index, status, etc.) silently fail or
//! emit a confusing connection error when the daemon isn't running. This
//! guard probes `/health`; if the daemon is down, it spawns
//! `trusty-search start` in the background and polls `/health` until the
//! daemon is ready (or a 60s budget is exhausted). Users get a single
//! informational line ("Starting trusty-search daemon…") and the command
//! they typed Just Works.
//!
//! What: thin shim over `trusty_common::daemon_guard` (issue #985).
//! `ensure_daemon_running_with_device` delegates the spinner/probe/timeout
//! loop to the shared implementation; only the trusty-search–specific knobs
//! (PID-file check, device flag propagation, READY_TIMEOUT=60s, indexing
//! device resolution) live here.
//!
//! Test: `probe_health_returns_false_on_connection_refused`,
//! `probe_health_returns_false_on_bad_url`,
//! `probe_health_respects_short_timeout`, and the indexing-device tests
//! cover the shim layer; `trusty_common::daemon_guard` tests cover the
//! shared spin loop.
//!
//! Note: only call this from commands that *require* the daemon. Commands
//! like `start`, `stop`, `serve`, `service`, `init`, and `completions`
//! deliberately do not call this guard.

use anyhow::{anyhow, Result};
use colored::Colorize;
use std::time::Duration;
use trusty_common::daemon_guard::{probe_once, spin_until_ready, DaemonGuardConfig};

/// Total wall-clock budget for the daemon to become ready after we spawn it.
///
/// Why 60s: with the v0.3.12 deferred-embedder-init fix the HTTP port binds
/// in ~1s, so the readiness probe normally returns near-instantly. However,
/// ONNX/CoreML model loading on first run can take 15–30s, and we'd rather
/// wait than fail spuriously.
const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// Probe `GET {base}/health`. Returns `true` on any 2xx response.
///
/// Why: delegates to `trusty_common::daemon_guard::probe_once` so the probe
/// logic is shared. Preserved as a public function so call sites keep their
/// current `probe_health(base)` call shape.
/// What: calls `probe_once("{base}/health")`.
/// Test: `probe_health_returns_false_on_connection_refused` below.
async fn probe_health(base: &str) -> bool {
    probe_once(&format!("{base}/health")).await
}

/// Spawn `trusty-search start --foreground` as a detached background process.
///
/// Why: we want the daemon to outlive this CLI invocation. We use the
/// currently-running executable so a `cargo run` session boots its own debug
/// daemon and a production install boots the production binary. The
/// `--foreground` flag prevents recursive self-spawning.
/// What: delegates to `trusty_common::daemon_guard::spawn_current_exe`.
#[allow(dead_code)]
pub(crate) fn spawn_daemon() -> Result<u32> {
    spawn_daemon_with_device(None)
}

/// Spawn `trusty-search start --foreground` as a detached background process,
/// optionally forcing a specific execution-provider device.
///
/// Why (issue #24): on Apple Silicon, CoreML EP session-init alone allocates
/// from the unified memory pool and inflates virtual RSS to ~72 GB before any
/// inference runs. Auto-spawning the daemon with `--device cpu` sidesteps
/// CoreML init entirely for the indexing path.
/// What: invokes `<exe> start --foreground` and, when `device` is `Some`,
/// appends `--device <device>`. Delegates to `spawn_current_exe`.
/// Test: `cargo check -p trusty-search` plus manual live testing.
pub(crate) fn spawn_daemon_with_device(device: Option<&str>) -> Result<u32> {
    let mut args = vec!["start", "--foreground"];
    let device_str;
    if let Some(dev) = device {
        args.push("--device");
        device_str = dev.to_string();
        args.push(&device_str);
    }
    trusty_common::daemon_guard::spawn_current_exe(&args)
        .map_err(|e| anyhow!("trusty-search daemon spawn failed: {e}"))
}

/// Ensure the daemon at `base` is running and ready. Spawns `trusty-search
/// start` (only when no daemon process is already running) and polls
/// `/health` for up to `READY_TIMEOUT` if not.
///
/// Why (v0.3.12): previously this unconditionally spawned a daemon when the
/// initial `/health` probe returned false. If a daemon process existed but
/// hadn't bound its HTTP port yet, the spawn would print "already running"
/// and the poll would still wait. Now we check the PID lockfile first: if a
/// daemon is already running, skip the spawn and just wait for `/health`.
/// What: fast-path, then PID-file check, then spawn (if needed), then
/// delegates to `spin_until_ready` with a 60s budget.
/// Test: covered indirectly by the live CLI path.
pub async fn ensure_daemon_running(base: &str) -> Result<()> {
    ensure_daemon_running_with_device(base, None).await
}

/// Like `ensure_daemon_running` but passes `--device <device>` to the
/// spawned daemon when an auto-spawn is performed.
///
/// Why (issue #24): the `index --force` flow on Apple Silicon was killed by
/// macOS jetsam before any indexing happened, because CoreML EP init inflated
/// virtual RSS to ~72 GB. Forcing CPU for the spawned daemon avoids the spike.
/// What: when the daemon is auto-spawned, propagates `device` to
/// `spawn_daemon_with_device`. Already-running daemons are left untouched.
/// Test: covered indirectly by `cargo check -p trusty-search` and manual
/// `index --force` on M-series.
pub async fn ensure_daemon_running_with_device(base: &str, device: Option<&str>) -> Result<()> {
    // Fast path: already up.
    if probe_health(base).await {
        return Ok(());
    }

    // Detect existing daemon process before spawning a duplicate.
    let already_running = crate::service::running_daemon_pid().is_some();

    if already_running {
        eprintln!(
            "{} trusty-search daemon already running, waiting for it to become ready…",
            "◉".cyan()
        );
    } else {
        match device {
            Some(dev) => eprintln!(
                "{} Starting trusty-search daemon (--device {dev})…",
                "◉".cyan()
            ),
            None => eprintln!("{} Starting trusty-search daemon…", "◉".cyan()),
        }
        spawn_daemon_with_device(device)?;
    }

    let cfg = DaemonGuardConfig {
        health_url: format!("{base}/health"),
        service_name: "trusty-search".to_string(),
        startup_timeout: READY_TIMEOUT,
        poll_interval: Duration::from_millis(500),
        timeout_hint: "try `trusty-search start` manually to see the error".to_string(),
    };
    spin_until_ready(&cfg).await
}

/// Convenience wrapper: returns a contextualized error on failure.
///
/// Why: every caller of `ensure_daemon_running` would otherwise duplicate the
/// error-handling boilerplate. Returns `Result` (not `process::exit`) so
/// command handlers stay testable.
/// What: delegates to `ensure_daemon_running`.
/// Test: covered by callers.
pub async fn ensure_daemon_running_or_exit(base: &str) -> Result<()> {
    ensure_daemon_running(base).await
}

/// Variant of `ensure_daemon_running_or_exit` that prefers CPU EP for an
/// auto-spawned daemon during the indexing flow.
///
/// Why (issue #24): the indexing path is the load-bearing OOM site on Apple
/// Silicon — CoreML EP init allocates ~72 GB of virtual RSS.
/// What: resolves the desired device from `TRUSTY_INDEX_DEVICE` (override) or
/// defaults to `"auto"`. Passes the resolved device to
/// `ensure_daemon_running_with_device`.
/// Test: `resolve_indexing_device_defaults_to_auto` and
/// `resolve_indexing_device_honours_env_override`.
pub async fn ensure_daemon_running_for_indexing(base: &str) -> Result<()> {
    let device = resolve_indexing_device();
    let device_opt = if device.eq_ignore_ascii_case("auto") {
        None
    } else {
        Some(device.as_str())
    };
    ensure_daemon_running_with_device(base, device_opt).await
}

/// Resolve the auto-spawn device for the indexing flow.
///
/// Why: keep the env-var contract in one place so tests and docs match
/// behaviour. Reads `TRUSTY_INDEX_DEVICE` (`cpu` | `gpu` | `auto`); defaults
/// to `auto` as of trusty-search 0.3.55.
/// What: returns a lowercased owned `String`.
/// Test: `resolve_indexing_device_defaults_to_auto`,
/// `resolve_indexing_device_honours_env_override`.
fn resolve_indexing_device() -> String {
    match std::env::var("TRUSTY_INDEX_DEVICE") {
        Ok(v) if !v.is_empty() => v.to_ascii_lowercase(),
        _ => "auto".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// `probe_health` against an unbound localhost port returns `false`
    /// (connect refused) within a reasonable deadline.
    #[tokio::test]
    async fn probe_health_returns_false_on_connection_refused() {
        let base = "http://127.0.0.1:65535";
        let started = Instant::now();
        let ok = probe_health(base).await;
        assert!(!ok, "probe should fail against an unbound port");
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "probe took too long: {:?}",
            started.elapsed()
        );
    }

    /// `probe_health` against a malformed URL returns `false` cleanly (no panic).
    #[tokio::test]
    async fn probe_health_returns_false_on_bad_url() {
        let ok = probe_health("not-a-valid-url").await;
        assert!(!ok);
    }

    /// `probe_health` returns false for a locally unreachable port within a
    /// generous wall-clock bound.
    #[tokio::test]
    async fn probe_health_respects_short_timeout() {
        let started = Instant::now();
        let _ = probe_health("http://127.0.0.1:1").await;
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "probe took too long: {:?}",
            started.elapsed()
        );
    }

    use std::sync::Mutex;
    static INDEX_DEVICE_ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Why: as of trusty-search 0.3.55 the indexing flow defaults to `auto`
    /// because the embedder now registers CoreML with
    /// `MLComputeUnits=CPUAndNeuralEngine`, eliminating the GPU unified-memory
    /// allocation that caused the original 72 GB virtual-RSS spike (issue #24).
    /// What: clears `TRUSTY_INDEX_DEVICE`, calls `resolve_indexing_device`,
    /// asserts it returns `"auto"`.
    /// Test: this test.
    #[test]
    fn resolve_indexing_device_defaults_to_auto() {
        let _guard = INDEX_DEVICE_ENV_LOCK.lock().unwrap();
        let prev = std::env::var("TRUSTY_INDEX_DEVICE").ok();
        // SAFETY: single-threaded under ENV_LOCK.
        unsafe { std::env::remove_var("TRUSTY_INDEX_DEVICE") };
        assert_eq!(resolve_indexing_device(), "auto");
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TRUSTY_INDEX_DEVICE", v),
                None => std::env::remove_var("TRUSTY_INDEX_DEVICE"),
            }
        }
    }

    /// Why: operators with enough headroom may want GPU during indexing.
    /// `TRUSTY_INDEX_DEVICE` is the documented escape hatch.
    /// What: sets `TRUSTY_INDEX_DEVICE=gpu` then `auto` and asserts the
    /// lowercased value is echoed.
    /// Test: this test.
    #[test]
    fn resolve_indexing_device_honours_env_override() {
        let _guard = INDEX_DEVICE_ENV_LOCK.lock().unwrap();
        let prev = std::env::var("TRUSTY_INDEX_DEVICE").ok();
        // SAFETY: single-threaded under ENV_LOCK.
        unsafe { std::env::set_var("TRUSTY_INDEX_DEVICE", "GPU") };
        assert_eq!(resolve_indexing_device(), "gpu");
        unsafe { std::env::set_var("TRUSTY_INDEX_DEVICE", "auto") };
        assert_eq!(resolve_indexing_device(), "auto");
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TRUSTY_INDEX_DEVICE", v),
                None => std::env::remove_var("TRUSTY_INDEX_DEVICE"),
            }
        }
    }
}
