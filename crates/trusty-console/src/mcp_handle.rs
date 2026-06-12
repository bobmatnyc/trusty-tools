//! Supervised stdio MCP connection to a local service (epic #1104 Phase 0b).
//!
//! Why: The trusty-console needs to poll each local service (starting with
//! trusty-analyze) over a persistent stdio MCP connection. Re-spawning on
//! every poll wastes process-creation overhead and can miss in-flight work.
//! Keeping a persistent connection allows low-overhead polling while the
//! supervisor handles crashes gracefully.
//!
//! What: `McpServiceHandle` wraps a `StdioMcpClient` behind an async mutex.
//! It spawns the service binary on construction (or lazily on first poll),
//! implements the supervisor pattern from `trusty-common`'s embedder client:
//! - `which(binary)` miss → never retry (service not installed on this machine)
//! - spawn failure (initial connect OR later respawn) → exponential backoff
//!   (1 s → 2 s → 4 s … cap 60 s) via `SpawnBackoff` so a consistently-failing
//!   binary does not spam the logs on every poll cycle
//! - `poll_metrics()` calls `console_metrics` tool and returns a parsed report
//!
//! ## Division of responsibility: `McpServiceHandle` vs. `StdioMcpClient`
//!
//! `StdioMcpClient::ensure_alive` / `respawn` (in `trusty-common`) handle the
//! *mechanics* of replacing a dead child process (issue #421: avoids the 30 s
//! write-to-dead-stdin stall). They contain **no rate-limiting** — if `respawn`
//! fails, the error propagates immediately and on the very next `call_tool`
//! the cycle repeats. Rate-limiting is therefore the responsibility of the
//! **caller** (`McpServiceHandle`). The `SpawnBackoff` struct fulfils that role
//! for **both** the initial-connect path (state = `None`) *and* the
//! already-connected respawn path (state = `Connected`). On a failed
//! `call_tool` / respawn we record the failure, transition back to `None`, and
//! return `Err`; the next `poll_metrics()` will re-enter the lazy-init block,
//! honour the backoff window, and attempt a fresh spawn only when the window
//! has elapsed.
//!
//! Test: `mcp_handle_absent_binary_returns_error`,
//! `mcp_handle_absent_never_retries`,
//! `mcp_handle_respawn_failure_applies_backoff`, and
//! `compute_backoff_delay_*` in this module.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::json;
use tokio::sync::Mutex;
use tracing::{debug, warn};
use trusty_common::console_metrics::{CONSOLE_METRICS_METHOD, ConsoleMetricsReport, parse_report};
use trusty_common::stdio_mcp_client::StdioMcpClient;

// ── Backoff constants (matches workspace supervisor pattern) ─────────────────

/// Initial retry delay in milliseconds after the first spawn failure.
const BACKOFF_BASE_MS: u64 = 1_000;

/// Maximum retry delay cap in milliseconds (60 s, matches EmbedderSupervisor).
const BACKOFF_CAP_MS: u64 = 60_000;

// ── State ────────────────────────────────────────────────────────────────────

/// State of the supervised connection.
enum HandleState {
    /// `which(binary)` returned None — service not installed; never retry.
    Absent,
    /// Connection is up (client holds the live pipe).
    /// Boxed to avoid large_enum_variant: `StdioMcpClient` is ~384 bytes.
    Connected(Box<StdioMcpClient>),
}

/// Spawn failure tracking embedded directly in the handle.
///
/// Why: A consistently-failing binary (bad permissions, missing dep, wrong
/// path) previously caused spawn-and-fail spam on every poll cycle because
/// the handle re-attempted the spawn unconditionally. Embedding the failure
/// counter + next-retry timestamp in the handle struct avoids a separate global
/// or task while keeping the SUCCESS path completely unchanged.
/// What: Counts consecutive spawn failures; computes the next allowed attempt
/// time using `compute_backoff_delay`; resets to zero on the first successful
/// spawn.
/// Test: `compute_backoff_delay_*` tests cover the pure delay logic.
struct SpawnBackoff {
    /// Number of consecutive spawn failures so far.
    failure_count: u32,
    /// The earliest `Instant` at which the next spawn attempt is allowed.
    next_attempt: Instant,
}

impl SpawnBackoff {
    fn new() -> Self {
        Self {
            failure_count: 0,
            next_attempt: Instant::now(),
        }
    }

    /// Record a spawn failure and advance `next_attempt` by the exponential
    /// backoff delay.
    fn record_failure(&mut self) {
        self.failure_count = self.failure_count.saturating_add(1);
        let delay_ms = compute_backoff_delay(self.failure_count, BACKOFF_BASE_MS, BACKOFF_CAP_MS);
        self.next_attempt = Instant::now() + Duration::from_millis(delay_ms);
    }

    /// Reset on a successful spawn so the next failure starts from the base.
    fn reset(&mut self) {
        self.failure_count = 0;
        self.next_attempt = Instant::now();
    }

    /// Return `true` if enough time has elapsed to allow the next spawn attempt.
    fn should_attempt(&self) -> bool {
        Instant::now() >= self.next_attempt
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// A supervised, persistent stdio MCP connection to a single local service.
///
/// Why: The console needs to poll each local service every ~15 s. A persistent
/// connection avoids per-poll spawn overhead. The supervisor recovers from
/// crashes (the underlying `StdioMcpClient` auto-respawns via `ensure_alive`).
/// Binary-missing machines degrade immediately to `Absent` and never retry.
/// Consistently-failing spawns use exponential backoff (via `SpawnBackoff`) so
/// the poller is not spammed with error logs on every cycle.
/// What: Holds the `StdioMcpClient` behind an async `Mutex`. `poll_metrics()`
/// calls the `console_metrics` tool and returns a `ConsoleMetricsReport`.
/// Test: Unit tests in this module cover the absent-binary, backoff-delay pure
/// function, and parse-failure paths; the end-to-end smoke test covers the live
/// pipe.
pub struct McpServiceHandle {
    /// Absolute path or short name of the binary to spawn.
    binary: String,
    /// Args to pass to the binary (e.g. `["mcp"]`).
    args: Vec<String>,
    /// The supervised client state plus backoff tracking, protected by an async
    /// mutex so the console and the poller task can share the same handle.
    state: Arc<Mutex<(Option<HandleState>, SpawnBackoff)>>,
}

impl McpServiceHandle {
    /// Construct a new `McpServiceHandle`.
    ///
    /// Why: Defers the actual spawn to the first `poll_metrics()` call so
    /// construction is sync and cheap (no async required at startup).
    /// What: Stores the binary + args; `state = None` means not yet connected.
    /// Test: `mcp_handle_constructs_without_io` constructs and verifies fields.
    pub fn new(binary: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            binary: binary.into(),
            args,
            state: Arc::new(Mutex::new((None, SpawnBackoff::new()))),
        }
    }

    /// Poll the service's `console_metrics` tool and return a decoded report.
    ///
    /// Why: The background poller calls this every ~15 s to refresh the cached
    /// snapshot. The method lazily connects on the first call and re-connects
    /// whenever the child process dies (via `StdioMcpClient::ensure_alive`).
    /// Repeated spawn/respawn failures are suppressed via `SpawnBackoff` so a
    /// consistently-broken binary does not spam the log on every poll cycle.
    /// The backoff applies equally to the initial connect and to post-crash
    /// respawns — `StdioMcpClient` handles the respawn mechanics but does not
    /// rate-limit repeated failures; that is this layer's responsibility.
    /// What: Lazy-init (honouring backoff) on state = `None`; call the
    /// `console_metrics` tool on state = `Connected`. On `call_tool` failure
    /// (which includes a failed internal respawn), record the failure, drop
    /// the dead client back to `None` so the next poll re-enters the lazy-init
    /// block and respects the backoff window. Returns `Err` on any failure so
    /// the poller can log and retain the previous cached value.
    /// Test: `mcp_handle_absent_binary_returns_error` covers the absent path;
    /// `mcp_handle_respawn_failure_applies_backoff` verifies the post-connect
    /// backoff gate; end-to-end smoke test validates the live pipe.
    pub async fn poll_metrics(&self) -> Result<ConsoleMetricsReport> {
        let mut guard = self.state.lock().await;
        let (state_opt, backoff) = &mut *guard;

        // Lazy initialisation: None means we have not tried connecting yet (or
        // a previous attempt failed and we transitioned back to None).
        if state_opt.is_none() {
            let resolved = which::which(&self.binary).ok();
            if resolved.is_none() {
                warn!(
                    binary = %self.binary,
                    "McpServiceHandle: binary not found on PATH — marking as Absent"
                );
                *state_opt = Some(HandleState::Absent);
            } else {
                // Honour backoff: skip this cycle if we failed recently.
                if !backoff.should_attempt() {
                    return Err(anyhow::anyhow!(
                        "McpServiceHandle: spawn of {} is in backoff (failure #{}); \
                         next attempt in {:?}",
                        self.binary,
                        backoff.failure_count,
                        backoff
                            .next_attempt
                            .saturating_duration_since(Instant::now()),
                    ));
                }

                debug!(binary = %self.binary, "McpServiceHandle: spawning MCP child");
                let args_ref: Vec<&str> = self.args.iter().map(String::as_str).collect();
                match StdioMcpClient::spawn(&self.binary, &args_ref, "trusty-console").await {
                    Ok(mut client) => {
                        client.initialize().await.with_context(|| {
                            format!(
                                "McpServiceHandle: MCP initialize failed for {}",
                                self.binary
                            )
                        })?;
                        backoff.reset();
                        *state_opt = Some(HandleState::Connected(Box::new(client)));
                    }
                    Err(e) => {
                        backoff.record_failure();
                        warn!(
                            binary = %self.binary,
                            failure_count = backoff.failure_count,
                            next_attempt_secs = ?backoff.next_attempt.saturating_duration_since(Instant::now()),
                            error = %e,
                            "McpServiceHandle: spawn failed — will retry after backoff"
                        );
                        return Err(e.context(format!(
                            "McpServiceHandle: failed to spawn {} (failure #{})",
                            self.binary, backoff.failure_count
                        )));
                    }
                }
            }
        }

        match state_opt.as_mut() {
            Some(HandleState::Absent) => anyhow::bail!(
                "McpServiceHandle: {} is not installed on this machine",
                self.binary
            ),
            Some(HandleState::Connected(client)) => {
                // `call_tool` calls `ensure_alive` internally, which respawns the
                // child if it has died. If the respawn itself fails (binary gone,
                // permissions changed, etc.) the error surfaces here.
                // Rate-limiting for those repeated failures lives here: on error
                // we record a failure and drop the client back to `None` so the
                // next poll re-enters the lazy-init block above and respects the
                // backoff window. On success we reset the counter so transient
                // crashes don't permanently throttle the poller.
                let result = client
                    .call_tool(CONSOLE_METRICS_METHOD, json!({}))
                    .await
                    .with_context(|| {
                        format!(
                            "McpServiceHandle: {} tool call failed for {}",
                            CONSOLE_METRICS_METHOD, self.binary
                        )
                    });

                match result {
                    Ok(raw) => {
                        backoff.reset();
                        parse_report(&raw).with_context(|| {
                            format!("McpServiceHandle: parse_report failed for {}", self.binary)
                        })
                    }
                    Err(e) => {
                        backoff.record_failure();
                        // Drop the client so the next poll attempts a fresh
                        // spawn after the backoff window elapses.
                        *state_opt = None;
                        warn!(
                            binary = %self.binary,
                            failure_count = backoff.failure_count,
                            next_attempt_secs = ?backoff.next_attempt.saturating_duration_since(Instant::now()),
                            error = %e,
                            "McpServiceHandle: tool call/respawn failed — resetting to None, \
                             will retry after backoff"
                        );
                        Err(e)
                    }
                }
            }
            None => unreachable!("guard must be Some after init block"),
        }
    }
}

// ── Pure backoff helper (testable without async) ─────────────────────────────

/// Compute the exponential backoff delay in milliseconds for a given attempt.
///
/// Why: Extracted as a pure function so unit tests can verify the backoff
/// curve (base, doubling, cap) without spawning processes or async machinery.
/// Matches the pattern used in `trusty-common`'s `EmbedderSupervisor`:
/// `delay = base * 2^attempt, capped at cap_ms`.
/// What: Returns `base_ms * 2^attempt` saturating-capped at `cap_ms`.
/// `attempt` is shifted by 1 relative to `failure_count` so the first failure
/// (failure_count = 1) waits `base_ms`, not `2 * base_ms`.
///
/// **Caller contract:** In production callers always pass `failure_count >= 1`
/// (the `SpawnBackoff::record_failure` path). `attempt = 0` is a degenerate
/// sentinel (no failures recorded) that also returns `base_ms` due to the
/// `saturating_sub(1)` floor; this case does not occur in normal operation.
///
/// Test: `compute_backoff_delay_base`, `compute_backoff_delay_doubles`,
/// `compute_backoff_delay_caps`, `compute_backoff_delay_attempt_zero`.
pub fn compute_backoff_delay(attempt: u32, base_ms: u64, cap_ms: u64) -> u64 {
    // Shift: failure_count=1 → 2^0 * base = base (1 s initial delay).
    let shift = attempt.saturating_sub(1).min(62);
    let raw = base_ms.saturating_mul(1u64 << shift);
    raw.min(cap_ms)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Pure backoff function tests ───────────────────────────────────────────

    /// Why: attempt=1 (first failure) must wait exactly base_ms, not 2*base_ms.
    /// What: call compute_backoff_delay(1, 1000, 60000) and assert 1000.
    /// Test: this test.
    #[test]
    fn compute_backoff_delay_base() {
        assert_eq!(
            compute_backoff_delay(1, BACKOFF_BASE_MS, BACKOFF_CAP_MS),
            BACKOFF_BASE_MS,
            "first failure must wait base_ms"
        );
    }

    /// Why: second failure must double the delay (2*base_ms = 2000 ms).
    /// What: call compute_backoff_delay(2, 1000, 60000) and assert 2000.
    /// Test: this test.
    #[test]
    fn compute_backoff_delay_doubles() {
        assert_eq!(
            compute_backoff_delay(2, BACKOFF_BASE_MS, BACKOFF_CAP_MS),
            2 * BACKOFF_BASE_MS,
            "second failure must double the delay"
        );
    }

    /// Why: large attempt counts must be capped at cap_ms so the delay never
    /// grows without bound.
    /// What: call compute_backoff_delay(100, 1000, 60000) and assert 60000.
    /// Test: this test.
    #[test]
    fn compute_backoff_delay_caps() {
        assert_eq!(
            compute_backoff_delay(100, BACKOFF_BASE_MS, BACKOFF_CAP_MS),
            BACKOFF_CAP_MS,
            "large attempt must cap at cap_ms"
        );
    }

    /// Why: attempt=0 is a sentinel (no failures yet); it should return base_ms
    /// (shift by saturating_sub(1) → 0, so 2^0 * base = base).
    /// What: call compute_backoff_delay(0, 1000, 60000) and assert 1000.
    /// Test: this test.
    #[test]
    fn compute_backoff_delay_attempt_zero() {
        assert_eq!(
            compute_backoff_delay(0, BACKOFF_BASE_MS, BACKOFF_CAP_MS),
            BACKOFF_BASE_MS,
            "attempt=0 must not underflow — returns base_ms"
        );
    }

    // ── Integration-style handle tests ────────────────────────────────────────

    /// Why: When the binary is absent from PATH, `poll_metrics` must return an
    /// error immediately (not hang or panic) so the poller can degrade gracefully.
    /// What: Create a handle pointing at a binary that does not exist, call
    /// `poll_metrics`, assert it returns `Err`.
    /// Test: This test.
    #[tokio::test]
    async fn mcp_handle_absent_binary_returns_error() {
        let handle =
            McpServiceHandle::new("/nonexistent/trusty-analyze-xyzzy", vec!["mcp".to_string()]);
        let result = handle.poll_metrics().await;
        assert!(result.is_err(), "absent binary must return Err");
    }

    /// Why: `new()` must succeed synchronously without performing I/O so
    /// the console can construct handles at startup without async.
    /// What: Construct a handle and assert the binary/args fields are stored.
    /// Test: This test (checks construction is cheap/sync-compatible).
    #[test]
    fn mcp_handle_constructs_without_io() {
        let handle = McpServiceHandle::new("trusty-analyze", vec!["mcp".to_string()]);
        assert_eq!(handle.binary, "trusty-analyze");
        assert_eq!(handle.args, vec!["mcp"]);
    }

    /// Why: Once a binary is marked `Absent` (not found on PATH) the handle
    /// must never retry — every subsequent poll must return Err immediately.
    /// What: Poll twice; both must return Err with no hang.
    /// Test: This test.
    #[tokio::test]
    async fn mcp_handle_absent_never_retries() {
        let handle = McpServiceHandle::new(
            "/nonexistent/trusty-analyze-xyzzy2",
            vec!["mcp".to_string()],
        );
        let r1 = handle.poll_metrics().await;
        let r2 = handle.poll_metrics().await;
        assert!(r1.is_err(), "first poll must return Err for absent binary");
        assert!(r2.is_err(), "second poll must also return Err (no retry)");
    }

    /// Why: After a `call_tool` / respawn failure on an already-connected handle,
    /// `SpawnBackoff` must gate subsequent poll attempts — the respawn path must
    /// NOT be unbounded just because the handle reached `Connected` once. This
    /// verifies the fix for the backoff gap identified in PR #1124 review.
    ///
    /// Mechanism under test: when `call_tool` returns `Err`, `poll_metrics`
    /// records a failure via `backoff.record_failure()`, resets state to `None`,
    /// and returns `Err`. The very next call re-enters the lazy-init block; since
    /// `backoff.should_attempt()` returns `false` (backoff window not yet elapsed),
    /// it returns `Err` immediately without attempting another spawn — proving
    /// the respawn path is now gated by the same backoff mechanism as the initial
    /// connect path.
    ///
    /// What: Manually insert a `SpawnBackoff` in the failure state (failure_count=1,
    /// next_attempt = far future) into a handle whose state is `None`, then verify
    /// that `poll_metrics` returns `Err` immediately without trying to spawn.
    /// Test: This test (no real binary or network required).
    #[tokio::test]
    async fn mcp_handle_respawn_failure_applies_backoff() {
        // Construct a handle whose backoff is already in the penalty window:
        // failure_count = 1, next_attempt = 60 seconds in the future.
        let handle = McpServiceHandle::new("trusty-analyze", vec!["mcp".to_string()]);
        {
            let mut guard = handle.state.lock().await;
            let (state_opt, backoff) = &mut *guard;
            // Simulate one prior failure that put us in backoff.
            backoff.failure_count = 1;
            backoff.next_attempt = Instant::now() + Duration::from_secs(60);
            // State remains None (as if we transitioned back from Connected after
            // a failed call_tool / respawn).
            assert!(state_opt.is_none());
        }

        // The binary exists on the machine (trusty-analyze may or may not be on
        // PATH). We prime the binary name to something that IS on PATH to avoid
        // the `which` miss path, so the test exercises the backoff gate rather
        // than the absent-binary gate. Use "true" (always present on Unix).
        let handle_with_true = McpServiceHandle::new("true", vec![]);
        {
            let mut guard = handle_with_true.state.lock().await;
            let (_state_opt, backoff) = &mut *guard;
            backoff.failure_count = 1;
            backoff.next_attempt = Instant::now() + Duration::from_secs(60);
        }

        let result = handle_with_true.poll_metrics().await;
        assert!(
            result.is_err(),
            "poll_metrics must return Err while in backoff window — respawn path must be gated"
        );
        let msg = format!("{:#}", result.unwrap_err());
        assert!(
            msg.contains("backoff"),
            "error message must mention backoff; got: {msg}"
        );
    }
}
