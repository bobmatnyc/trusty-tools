//! Auto-start the daemon when a CLI command needs it.
//!
//! Why: most CLI subcommands (query, index, status, etc.) silently fail or
//! emit a confusing connection error when the daemon isn't running. This
//! guard probes the daemon; if it is down, it spawns `trusty-search start` in
//! the background and waits until the daemon answers (or a 60s budget is
//! exhausted). Users get a single informational line ("Starting trusty-search
//! daemon…") and the command they typed Just Works.
//!
//! What: #9214 — the probe is `search.health` on the daemon's Unix socket
//! through `DaemonClient`; this module never dials TCP. The PID-file check,
//! the device flag for the spawned daemon and the 60s budget live here.
//! Subcommands still on HTTP reach it through `commands::daemon_http`, which
//! resolves their HTTP base after this guard returns.
//!
//! Test: `ensure_daemon_up_names_the_socket_when_it_never_answers` and the
//! indexing-device tests.
//!
//! Note: only call this from commands that *require* the daemon. Commands
//! like `start`, `stop`, `serve`, `service`, `init`, and `completions`
//! deliberately do not call this guard.

use anyhow::{anyhow, Result};
use colored::Colorize;
use std::io::Write;
use std::time::Duration;
use trusty_search::service::daemon_client::DaemonClient;

/// Total wall-clock budget for the daemon to become ready after we spawn it.
///
/// Why 60s: with the v0.3.12 deferred-embedder-init fix the daemon binds
/// in ~1s, so the readiness probe normally returns near-instantly. However,
/// ONNX/CoreML model loading on first run can take 15–30s, and we'd rather
/// wait than fail spuriously.
const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// Spawn `trusty-search start --foreground` as a detached background process.
///
/// Why: we want the daemon to outlive this CLI invocation. We use the
/// currently-running executable so a `cargo run` session boots its own debug
/// daemon and a production install boots the production binary. The
/// `--foreground` flag prevents recursive self-spawning.
/// What: delegates to `spawn_daemon_with_device(None)`.
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
/// appends `--device <device>`. Delegates to
/// `spawn_current_exe_forwarding_parent_link`.
/// Test: `cli_auto_started_daemon_exits_when_its_test_binary_is_killed`.
pub(crate) fn spawn_daemon_with_device(device: Option<&str>) -> Result<u32> {
    let mut args = vec!["start", "--foreground"];
    let device_str;
    if let Some(dev) = device {
        args.push("--device");
        device_str = dev.to_string();
        args.push(&device_str);
    }
    // #8900: forward a parent-death stamp, so a daemon a stamped CLI (a test)
    // auto-starts dies with that test rather than outliving the run. No stamp
    // in the environment — every production invocation — forwards nothing.
    trusty_common::daemon_guard::spawn_current_exe_forwarding_parent_link(&args)
        .map_err(|e| anyhow!("trusty-search daemon spawn failed: {e}"))
}

/// Ensure the daemon answers `search.health` on `client`'s socket, starting it
/// when no daemon process is running (#6285).
///
/// Why: a subcommand on the socket must also wait on the socket — a daemon can
/// bind TCP before its socket, and the HTTP listener is being retired.
/// What: [`ensure_daemon_up_with_device`] with no device override.
///
/// # Errors
///
/// When the spawn fails, or the socket still does not answer at the deadline —
/// the error names the socket path.
///
/// Test: `ensure_daemon_up_names_the_socket_when_it_never_answers`.
pub async fn ensure_daemon_up(client: &DaemonClient) -> Result<()> {
    ensure_daemon_up_with_device(client, None).await
}

/// [`ensure_daemon_up`], passing `--device <device>` to the daemon it spawns.
///
/// Why (issue #24): on Apple Silicon, CoreML EP session-init inflates virtual
/// RSS to ~72 GB, so the indexing flow may start its daemon on CPU. An
/// already-running daemon is left untouched.
/// What: fast path on one probe; otherwise spawn `trusty-search start` unless a
/// daemon process already holds the lockfile, then probe every 500 ms for up to
/// [`READY_TIMEOUT`] behind a spinner. #9214: the same lines the HTTP guard
/// printed; it never dials TCP.
///
/// # Errors
///
/// The same set as [`ensure_daemon_up`].
///
/// Test: `ensure_daemon_up_names_the_socket_when_it_never_answers`.
pub async fn ensure_daemon_up_with_device(
    client: &DaemonClient,
    device: Option<&str>,
) -> Result<()> {
    if client.is_up().await {
        return Ok(());
    }
    if crate::service::running_daemon_pid().is_some() {
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
    wait_for_socket(client, READY_TIMEOUT).await
}

/// Spinner frames, the set `trusty_common::daemon_guard` draws.
const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Probe `client`'s socket until it answers or `budget` elapses.
///
/// What: redraws a one-line stderr spinner each poll, then prints
/// "✓ trusty-search ready (Ns)" — the lines the HTTP guard printed (#9214).
async fn wait_for_socket(client: &DaemonClient, budget: Duration) -> Result<()> {
    let start = tokio::time::Instant::now();
    let deadline = start + budget;
    let mut frame = 0usize;
    loop {
        eprint!(
            "\r{} Waiting for trusty-search to become ready… ({}s) ",
            SPINNER[frame % SPINNER.len()].cyan(),
            start.elapsed().as_secs()
        );
        let _ = std::io::stderr().flush();
        frame = frame.wrapping_add(1);
        if client.is_up().await {
            eprint!("\r\x1b[2K");
            eprintln!(
                "{} trusty-search ready ({}s)",
                "✓".green(),
                start.elapsed().as_secs()
            );
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            eprint!("\r\x1b[2K");
            let _ = std::io::stderr().flush();
            return Err(anyhow!(
                "trusty-search did not become ready within {}s on socket {} — \
                 try `trusty-search start` manually to see the error",
                budget.as_secs(),
                client.socket().display()
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// The device the indexing flow starts its daemon on, or `None` for `auto`.
///
/// Why (issue #24): the indexing path is the load-bearing OOM site on Apple
/// Silicon — CoreML EP init allocates ~72 GB of virtual RSS.
/// What: [`resolve_indexing_device`], with `auto` meaning "no override".
/// Test: `resolve_indexing_device_defaults_to_auto`,
/// `resolve_indexing_device_honours_env_override`.
pub(crate) fn indexing_device() -> Option<String> {
    let device = resolve_indexing_device();
    (!device.eq_ignore_ascii_case("auto")).then_some(device)
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

    /// #6285: the socket wait fails closed on a scratch socket nothing serves,
    /// names that socket, and never falls back to TCP.
    #[tokio::test]
    async fn ensure_daemon_up_names_the_socket_when_it_never_answers() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let socket = dir.path().join("absent.sock");
        let client = DaemonClient::at(&socket);
        let err = wait_for_socket(&client, Duration::from_millis(600))
            .await
            .expect_err("nothing answers the scratch socket");
        let text = err.to_string();
        assert!(text.contains(&socket.display().to_string()), "{text}");
        assert!(!text.contains("http://"), "{text}");
    }

    /// Why: as of trusty-search 0.3.55 the indexing flow defaults to `auto`
    /// because the embedder now registers CoreML with
    /// `MLComputeUnits=CPUAndNeuralEngine`, eliminating the GPU unified-memory
    /// allocation that caused the original 72 GB virtual-RSS spike (issue #24).
    /// What: clears `TRUSTY_INDEX_DEVICE`, calls `resolve_indexing_device`,
    /// asserts it returns `"auto"`.
    /// Test: this test.
    #[test]
    #[serial_test::serial] // #5937: the crate's one env group
    fn resolve_indexing_device_defaults_to_auto() {
        let prev = std::env::var("TRUSTY_INDEX_DEVICE").ok();
        // SAFETY: serialised by `#[serial]`.
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
    #[serial_test::serial] // #5937: the crate's one env group
    fn resolve_indexing_device_honours_env_override() {
        let prev = std::env::var("TRUSTY_INDEX_DEVICE").ok();
        // SAFETY: serialised by `#[serial]`.
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
