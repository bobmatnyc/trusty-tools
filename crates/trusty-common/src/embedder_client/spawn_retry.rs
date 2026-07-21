//! Bounded ETXTBSY retry wrapper for spawning the `trusty-embedderd` sidecar.
//!
//! Why: isolated in a sibling file (rather than inline in `supervisor.rs`) to
//! keep `supervisor.rs` under its 500-SLOC production-file cap while still
//! sharing this crate's usual per-module split (`error.rs`, `stdio.rs`, …)
//! rather than a test-only `#[path]` trick.
//!
//! Test: exercised indirectly via every test in
//! `supervisor::tests::shutdown_tests` (all of which call
//! `EmbedderSupervisor::spawn_stdio` -> `spawn_child` -> `spawn_embedderd`
//! before exercising shutdown), especially
//! `supervisor_dropped_handle_does_not_busy_spin` (#3570).

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};

use super::supervisor::SupervisorConfig;

/// Build the `trusty-embedderd --stdio` command and spawn it with a bounded
/// ETXTBSY retry.
///
/// Why: extracted out of `supervisor::spawn_child` so command construction
/// and the retry live together — `Command` is not `Clone`, so retrying a
/// spawn means rebuilding it from scratch on every attempt anyway.
/// What: `Command::new(binary_path).arg("--stdio")` with piped stdin/stdout,
/// inherited stderr, `kill_on_drop(true)`, and (when
/// `config.sidecar_batch_size` is `Some(n)`) `TRUSTY_EMBED_BATCH_SIZE=n`
/// (issue #747 Fix C) — then delegates to `spawn_with_etxtbsy_retry`.
/// Test: `supervisor_dropped_handle_does_not_busy_spin`,
/// `supervisor_shutdown_kills_child`.
pub(super) async fn spawn_embedderd(
    binary_path: &Path,
    config: &SupervisorConfig,
) -> std::io::Result<Child> {
    let build = || {
        let mut cmd = Command::new(binary_path);
        cmd.arg("--stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        if let Some(bs) = config.sidecar_batch_size {
            cmd.env("TRUSTY_EMBED_BATCH_SIZE", bs.to_string());
        }
        cmd
    };
    spawn_with_etxtbsy_retry(build).await
}

/// Spawn a freshly-built `Command`, retrying on ETXTBSY (os error 26).
///
/// Why: `execve` can fail with `ETXTBSY` ("Text file busy") when the kernel
/// still considers the target inode open-for-write — e.g. when a just-written,
/// just-chmod'd mock `trusty-embedderd` script is exec'd before the page-cache
/// flush settles. This is the same race class already diagnosed and fixed in
/// `trusty-agents` under #866, #1528, and #1634 (see
/// `spawn_with_etxtbsy_retry` in `claude_code_runner/mod.rs`); this transplants
/// that exact bounded-retry shape rather than inventing a new one, so the
/// regression test `supervisor_dropped_handle_does_not_busy_spin` (#3570) — and
/// the real sidecar spawn path — stop hitting the transient kernel state.
/// What: calls `build()` to obtain a fresh `Command` on each attempt (required
/// because `Command` is not `Clone`) and `.spawn()`s it. On `ETXTBSY` it backs
/// off with a short exponential delay (≤3 attempts, 5 ms base) and retries; any
/// other error, or the success path, returns immediately. The bound keeps real
/// failures (missing binary, permission denied) from being masked or delayed.
/// Test: `supervisor_dropped_handle_does_not_busy_spin`,
/// `supervisor_shutdown_kills_child`.
async fn spawn_with_etxtbsy_retry<F>(mut build: F) -> std::io::Result<Child>
where
    F: FnMut() -> Command,
{
    const MAX_ATTEMPTS: u32 = 3;
    const BACKOFF_MS: u64 = 5;

    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..MAX_ATTEMPTS {
        match build().spawn() {
            Ok(child) => return Ok(child),
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                last_err = Some(e);
                // Exponential backoff: 5ms, 10ms, 20ms for MAX_ATTEMPTS=3.
                // `attempt` is bounded by MAX_ATTEMPTS, so the shift is far
                // below u64's 64-bit width. Use an explicit `1u64` shift and a
                // saturating multiply so this stays overflow-safe for any
                // reasonable MAX_ATTEMPTS value if it is ever raised.
                let backoff_ms = BACKOFF_MS.saturating_mul(1u64 << attempt);
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
            }
            Err(e) => return Err(e),
        }
    }
    // Unreachable in practice: the loop only falls through after recording an
    // ETXTBSY error, so `last_err` is always `Some`. Construct a defensive
    // fallback rather than panicking.
    Err(last_err.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::ExecutableFileBusy,
            "spawn retries exhausted",
        )
    }))
}
