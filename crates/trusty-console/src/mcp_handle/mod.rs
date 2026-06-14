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
//! - `call_tool_raw()` calls any named tool and returns the raw JSON Value
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
//! ## Lock discipline
//!
//! The outer `Mutex<(Option<HandleState>, SpawnBackoff)>` is held **only** for
//! state inspection and transition (the spawn / initialize path, backoff reads,
//! and state writes). The long-running `call_tool` I/O is performed **outside**
//! the outer lock: once we have a reference to the inner client lock we drop
//! the outer guard, then acquire the per-client `Mutex<StdioMcpClient>` for
//! the duration of the tool call only. This keeps the background metrics poller
//! from blocking the on-demand route handlers (and vice-versa) across the full
//! duration of a MCP round-trip.
//!
//! Test: `mcp_handle_absent_binary_returns_error`,
//! `mcp_handle_absent_never_retries`,
//! `mcp_handle_respawn_failure_applies_backoff`,
//! `outer_lock_not_held_during_probe_outer_lock_remains_acquirable`, and
//! `compute_backoff_delay_*` in `tests.rs`.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tracing::{debug, warn};
use trusty_common::console_metrics::{CONSOLE_METRICS_METHOD, ConsoleMetricsReport, parse_report};
use trusty_common::stdio_mcp_client::StdioMcpClient;

#[cfg(test)]
mod tests;

// ── Backoff constants (matches workspace supervisor pattern) ─────────────────

/// Initial retry delay in milliseconds after the first spawn failure.
pub(super) const BACKOFF_BASE_MS: u64 = 1_000;

/// Maximum retry delay cap in milliseconds (60 s, matches EmbedderSupervisor).
pub(super) const BACKOFF_CAP_MS: u64 = 60_000;

/// Remediation hint surfaced when the `tools/list` probe succeeds but the
/// expected `console_metrics` tool is absent from the listing.
///
/// Why: A constant prevents the message from drifting between code and the
/// API payload; centralising it here makes it easy to update.
/// What: Human-readable actionable string returned in `McpHandleError::Degraded`
/// and stored in `HandleState::Degraded` for repeated calls.
/// Test: `mcp_handle_degraded_when_console_metrics_missing` asserts this
/// string appears in the `Degraded` error hint.
const DEGRADED_HINT: &str = "reachable but `console_metrics` tool not registered — \
     check `serve --stdio` wiring / restart the daemon";

// ── Typed error ──────────────────────────────────────────────────────────────

/// Structured error returned by `call_tool_raw`, `call_tool_checked`, and `poll_metrics`.
///
/// Why: String-based classification (`msg.contains("not installed")`) is
/// fragile — a message change silently breaks 503 vs 502 routing in the HTTP
/// handlers. Giving callers a typed variant they can `match` on makes the
/// distinction explicit and refactor-safe.
/// What: Five variants cover every outcome: `Absent` (binary not on PATH,
/// never retry), `Backoff` (spawn failure window active, retry later),
/// `Degraded` (handshake OK but expected tool missing — actionable hint
/// included), `ToolUnavailable` (specific requested tool absent from the
/// cached tool set — capability-gate), and `Other` (any other failure).
/// Test: `mcp_handle_absent_binary_returns_error`,
/// `mcp_handle_respawn_failure_applies_backoff`,
/// `mcp_handle_degraded_when_console_metrics_missing`, and
/// `call_tool_checked_returns_tool_unavailable_when_tool_absent` cover the variants.
#[derive(Debug)]
pub enum McpHandleError {
    /// The service binary was not found on PATH; the handle is permanently in
    /// the `Absent` state and will never retry.
    Absent,
    /// A previous spawn failure put the handle into an exponential-backoff
    /// window; the current attempt was skipped to avoid log spam.
    Backoff {
        /// Number of consecutive failures so far.
        failure_count: u32,
        /// How long until the next attempt is allowed.
        next_attempt_in: Duration,
    },
    /// MCP handshake succeeded but the expected `console_metrics` tool was
    /// not listed in `tools/list`. The service is reachable but cannot supply
    /// metrics — the binary may not be running in `serve --stdio` mode or may
    /// need to be restarted.
    Degraded {
        /// Human-readable actionable remediation hint.
        hint: String,
    },
    /// The specific tool requested by `call_tool_checked` is not in the
    /// service's cached tool set (from `tools/list`). The call was NOT made —
    /// returned immediately to prevent a raw JSON-RPC -32601 error reaching
    /// the HTTP handler as a 502.
    ToolUnavailable {
        /// The tool name that was requested but not found in `tools/list`.
        tool: String,
        /// Human-readable actionable remediation hint (e.g. "rebuild/upgrade the daemon").
        hint: String,
    },
    /// Any other failure (spawn error, transport error, tool error, parse
    /// failure). The inner `anyhow::Error` carries the full context chain.
    Other(anyhow::Error),
}

impl std::fmt::Display for McpHandleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Absent => write!(f, "McpServiceHandle: binary not installed on this machine"),
            Self::Backoff {
                failure_count,
                next_attempt_in,
            } => write!(
                f,
                "McpServiceHandle: in backoff after {failure_count} failure(s); \
                 next attempt in {next_attempt_in:.2?}"
            ),
            Self::Degraded { hint } => {
                write!(f, "McpServiceHandle: degraded — {hint}")
            }
            Self::ToolUnavailable { tool, hint } => {
                write!(f, "McpServiceHandle: tool `{tool}` unavailable — {hint}")
            }
            Self::Other(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for McpHandleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Other(e) => e.source(),
            _ => None,
        }
    }
}

// ── State ────────────────────────────────────────────────────────────────────

/// State of the supervised connection.
pub(super) enum HandleState {
    /// `which(binary)` returned None — service not installed; never retry.
    Absent,
    /// Connection is up, with the full tool set and daemon version cached
    /// from the `tools/list` + `initialize` probe.
    ///
    /// The client is wrapped in its own `Arc<Mutex<…>>` so the outer
    /// `state` lock can be released before the long `call_tool` I/O,
    /// preventing the metrics poller from blocking on-demand route handlers.
    /// `tool_names` caches the `tools/list` result so `call_tool_checked`
    /// can gate without a network round-trip. `daemon_version` stores the
    /// `serverInfo.version` from `initialize` for UI display.
    Connected {
        /// The inner stdio MCP client, behind a per-client lock.
        client: Arc<Mutex<Box<StdioMcpClient>>>,
        /// Full set of tool names returned by the last `tools/list` probe.
        ///
        /// **Staleness invariant:** this set is a snapshot captured at
        /// `ensure_connected` probe time and is only refreshed on a full
        /// (re)connect or self-heal reconnect — that is, when the stdio
        /// connection drops and a new MCP client is spawned, triggering a fresh
        /// `initialize` + `tools/list` round-trip.  It is NOT updated on every
        /// `call_tool_checked` call.
        ///
        /// This is intentionally correct for the stale-daemon upgrade use case:
        /// upgrading a daemon restarts its OS process, which closes the stdio
        /// pipe.  The pipe close is detected by the next poll or call, the
        /// connection transitions back to `None`, and `ensure_connected` re-runs
        /// the full probe — so the snapshot is refreshed automatically without
        /// any special handling at the call site.
        tool_names: HashSet<String>,
        /// Daemon version string from `serverInfo.version` in the `initialize` response.
        daemon_version: String,
    },
    /// MCP handshake succeeded but `tools/list` did not include
    /// `console_metrics` — the service is reachable but cannot supply
    /// metrics. Self-heals: `ensure_connected` treats this state like `None`
    /// once the `SpawnBackoff` window elapses, dropping back to `None` and
    /// re-running the full `initialize` + `tools/list` probe. While still
    /// inside the backoff window, callers receive `McpHandleError::Degraded`
    /// so the UI keeps showing the degraded badge and remediation hint.
    Degraded,
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
pub(super) struct SpawnBackoff {
    /// Number of consecutive spawn failures so far.
    pub(super) failure_count: u32,
    /// The earliest `Instant` at which the next spawn attempt is allowed.
    pub(super) next_attempt: Instant,
}

impl SpawnBackoff {
    pub(super) fn new() -> Self {
        Self {
            failure_count: 0,
            next_attempt: Instant::now(),
        }
    }

    /// Record a spawn failure and advance `next_attempt` by the exponential
    /// backoff delay.
    pub(super) fn record_failure(&mut self) {
        self.failure_count = self.failure_count.saturating_add(1);
        let delay_ms = compute_backoff_delay(self.failure_count, BACKOFF_BASE_MS, BACKOFF_CAP_MS);
        self.next_attempt = Instant::now() + Duration::from_millis(delay_ms);
    }

    /// Reset on a successful spawn so the next failure starts from the base.
    pub(super) fn reset(&mut self) {
        self.failure_count = 0;
        self.next_attempt = Instant::now();
    }

    /// Return `true` if enough time has elapsed to allow the next spawn attempt.
    pub(super) fn should_attempt(&self) -> bool {
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
/// Test: Unit tests in `tests.rs` cover the absent-binary, backoff-delay pure
/// function, and parse-failure paths; the end-to-end smoke test covers the live
/// pipe.
pub struct McpServiceHandle {
    /// Absolute path or short name of the binary to spawn.
    pub(super) binary: String,
    /// Args to pass to the binary (e.g. `["mcp"]`).
    pub(super) args: Vec<String>,
    /// The supervised client state plus backoff tracking, protected by an async
    /// mutex so the console and the poller task can share the same handle.
    pub(super) state: Arc<Mutex<(Option<HandleState>, SpawnBackoff)>>,
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

    /// Prime this handle to the `Degraded` state — test helper only.
    ///
    /// Why: Integration tests in sibling modules (`server.rs`) need to drive the
    /// handle into `Degraded` without performing a real MCP spawn. Making
    /// `HandleState` pub(crate) just to enable test setup leaks internal state
    /// machine details; a targeted `#[cfg(test)]` helper keeps the internals
    /// opaque while giving tests a precise seam.
    /// What: Acquires the outer state lock and sets `state_opt =
    /// Some(HandleState::Degraded)`.  The backoff `next_attempt` is set to
    /// `Instant::now() + future_backoff` so callers can control whether the
    /// self-healing window has elapsed. Pass `Duration::ZERO` to simulate an
    /// already-expired window (self-heal is allowed); pass a large duration to
    /// hold the handle in-window (self-heal is suppressed).
    /// Test: Used by `server::tests::test_services_route_handle_degraded_overlay`
    /// and `mcp_handle_degraded_self_heals_after_backoff_window`.
    #[cfg(test)]
    pub async fn prime_degraded_for_test(&self) {
        self.prime_degraded_with_backoff_for_test(Duration::from_secs(60))
            .await;
    }

    /// Prime this handle to `Degraded` with a specific backoff offset — test helper.
    ///
    /// Why: Self-healing tests need to simulate both the in-window case (no
    /// re-probe) and the post-window case (re-probe triggered).
    /// What: Sets state to `Degraded`, `failure_count = 1`, and
    /// `next_attempt = Instant::now() + future_backoff`.
    /// Test: Used by `mcp_handle_degraded_self_heals_after_backoff_window`.
    #[cfg(test)]
    pub async fn prime_degraded_with_backoff_for_test(&self, future_backoff: Duration) {
        let mut guard = self.state.lock().await;
        let (state_opt, backoff) = &mut *guard;
        *state_opt = Some(HandleState::Degraded);
        backoff.failure_count = 1;
        backoff.next_attempt = Instant::now() + future_backoff;
    }

    /// Prime this handle to `Connected` with a tool set that excludes a specific
    /// tool — test helper for capability-gate regression tests.
    ///
    /// Why: Route handler tests need to verify that `call_tool_checked` returns
    /// `ToolUnavailable` when the connected daemon lacks a specific tool without
    /// spawning a real MCP process. This helper sets the `Connected` state with
    /// a curated tool set that includes `console_metrics` (the required sentinel)
    /// but excludes the named tool.
    /// What: Spawns `cat` as a placeholder child (never called through), wraps it
    /// in the standard `Arc<Mutex<Box<StdioMcpClient>>>`, builds a tool set with
    /// `console_metrics` plus any `extra_tools`, then sets state to `Connected`
    /// with `daemon_version = "0.0.0-test"`.
    /// Test: Used by route tests in `server.rs` to simulate a stale daemon
    /// (capability-gate regression for #1170).
    #[cfg(test)]
    pub async fn prime_connected_missing_tool_for_test(&self, missing_tool: &str) {
        let client =
            trusty_common::stdio_mcp_client::StdioMcpClient::spawn("cat", &[], "test-client")
                .await
                .expect("cat must be present for test setup");
        let client_arc = Arc::new(Mutex::new(Box::new(client)));
        let mut tool_names = HashSet::new();
        tool_names.insert(CONSOLE_METRICS_METHOD.to_string());
        // Add several common analyze tools but NOT the one being tested as absent.
        for t in &["extract_graph", "list_entities", "cluster_concepts"] {
            if *t != missing_tool {
                tool_names.insert(t.to_string());
            }
        }
        // Ensure the missing_tool is absent regardless of the defaults above.
        tool_names.remove(missing_tool);

        let mut guard = self.state.lock().await;
        let (state_opt, backoff) = &mut *guard;
        backoff.reset();
        *state_opt = Some(HandleState::Connected {
            client: Arc::clone(&client_arc),
            tool_names,
            daemon_version: "0.0.0-test".to_string(),
        });
    }

    /// Return the degraded hint string if the handle is in the `Degraded` state.
    ///
    /// Why: The services route needs to overlay the connector-reported status with
    /// the handle's known state so `GET /api/console/services` reflects whether
    /// the `tools/list` probe found `console_metrics` missing. Returning
    /// `Option<String>` lets the handler set both `status = Degraded` and
    /// `hint` in one call without importing the private `DEGRADED_HINT` constant
    /// or performing any I/O.
    /// What: Acquires the outer state lock briefly; returns
    /// `Some(DEGRADED_HINT.to_string())` when the state is
    /// `HandleState::Degraded`, `None` for every other state (`None` uninit,
    /// `Absent`, `Connected`).
    /// Test: `test_degraded_hint_returns_some_when_degraded` in `tests.rs`.
    pub async fn degraded_hint(&self) -> Option<String> {
        let guard = self.state.lock().await;
        let (state_opt, _) = &*guard;
        if matches!(state_opt, Some(HandleState::Degraded)) {
            Some(DEGRADED_HINT.to_string())
        } else {
            None
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
    pub async fn poll_metrics(&self) -> Result<ConsoleMetricsReport, McpHandleError> {
        let (client_arc, _tool_names) = self.ensure_connected().await?;

        let mut client_guard = client_arc.lock().await;
        let raw = client_guard
            .call_tool(CONSOLE_METRICS_METHOD, json!({}))
            .await
            .with_context(|| {
                format!(
                    "McpServiceHandle: {} tool call failed for {}",
                    CONSOLE_METRICS_METHOD, self.binary
                )
            });

        drop(client_guard);

        match raw {
            Ok(value) => {
                self.on_call_success().await;
                parse_report(&value)
                    .with_context(|| {
                        format!("McpServiceHandle: parse_report failed for {}", self.binary)
                    })
                    .map_err(McpHandleError::Other)
            }
            Err(e) => {
                self.on_call_failure().await;
                Err(McpHandleError::Other(e))
            }
        }
    }

    /// Call any named MCP tool and return the unwrapped data `Value`.
    ///
    /// Why: The console's on-demand routes (e.g. `/api/console/metrics/analyze/indexes`,
    /// `/api/console/metrics/analyze/visualize`) need to invoke arbitrary tools
    /// (like `list_analyze_indexes`, `extract_graph`, `list_entities`,
    /// `cluster_concepts`) without going through the browser → /proxy path.
    /// This is the mechanism that lets the console be a pure stdio MCP client
    /// for all analyze data, honouring the #1104 architecture principle.
    /// What: Shares the exact same lazy-init and backoff machinery as
    /// `poll_metrics` — re-uses an open connection when available, spawns or
    /// respawns on failure, gates retries behind `SpawnBackoff`. Unwraps the
    /// MCP content envelope (`{"content":[{"type":"text","text":"..."}]}`) so
    /// callers receive the payload `Value` directly.
    /// Test: `call_tool_raw_absent_binary_returns_error` covers the absent path;
    /// the on-demand route integration tests exercise the live path.
    pub async fn call_tool_raw(&self, tool: &str, args: Value) -> Result<Value, McpHandleError> {
        let (client_arc, _tool_names) = self.ensure_connected().await?;

        let mut client_guard = client_arc.lock().await;
        let result = client_guard.call_tool(tool, args).await.with_context(|| {
            format!(
                "McpServiceHandle: {} tool call failed for {}",
                tool, self.binary
            )
        });

        drop(client_guard);

        match result {
            Ok(raw) => {
                self.on_call_success().await;
                Ok(unwrap_mcp_content(raw))
            }
            Err(e) => {
                self.on_call_failure().await;
                Err(McpHandleError::Other(e))
            }
        }
    }

    /// Call a named MCP tool only if it is present in the cached tool set.
    ///
    /// Why: Calling a tool absent from `tools/list` causes the daemon to reply
    /// with JSON-RPC -32601 "unknown method", which surfaces as a raw 502 to the
    /// browser — an opaque, non-actionable error. This guard prevents that call
    /// entirely and returns a typed `McpHandleError::ToolUnavailable` with an
    /// actionable hint, which route handlers can map to a clean 503+JSON response.
    /// After the user rebuilds/upgrades the daemon and the handle self-heals, the
    /// re-probe populates the updated tool set, and the call becomes allowed again.
    /// What: Calls `ensure_connected` to obtain the cached tool set; if `tool` is
    /// not in the set, returns `McpHandleError::ToolUnavailable` immediately
    /// without making a JSON-RPC round-trip. Otherwise delegates to `call_tool_raw`.
    /// Test: `call_tool_checked_returns_tool_unavailable_when_tool_absent` in `tests.rs`.
    pub async fn call_tool_checked(
        &self,
        tool: &str,
        args: Value,
    ) -> Result<Value, McpHandleError> {
        let (client_arc, tool_names) = self.ensure_connected().await?;

        if !tool_names.contains(tool) {
            let hint = format!(
                "{} does not expose `{tool}` — rebuild/upgrade the daemon \
                 (run `cargo install {}`)",
                self.binary, self.binary
            );
            warn!(
                binary = %self.binary,
                tool = %tool,
                "McpServiceHandle: capability-gate rejected call — tool not in cached tool set"
            );
            return Err(McpHandleError::ToolUnavailable {
                tool: tool.to_string(),
                hint,
            });
        }

        let mut client_guard = client_arc.lock().await;
        let result = client_guard.call_tool(tool, args).await.with_context(|| {
            format!(
                "McpServiceHandle: {} tool call failed for {}",
                tool, self.binary
            )
        });
        drop(client_guard);

        match result {
            Ok(raw) => {
                self.on_call_success().await;
                Ok(unwrap_mcp_content(raw))
            }
            Err(e) => {
                self.on_call_failure().await;
                Err(McpHandleError::Other(e))
            }
        }
    }

    /// Return the daemon version string cached from the last successful `initialize` response.
    ///
    /// Why: The UI needs to display the daemon version so operators can confirm
    /// whether they are running the expected build after an upgrade — a pure
    /// capability-gate cannot catch version regressions, but version surfacing
    /// gives operators the context they need to diagnose unexpected behaviour.
    /// What: Acquires the outer state lock briefly; returns `Some(version)` when
    /// in `Connected` state, `None` for every other state (`None`, `Absent`,
    /// `Degraded`).
    /// Test: `test_daemon_version_returns_some_when_connected` in `tests.rs`.
    pub async fn daemon_version(&self) -> Option<String> {
        let guard = self.state.lock().await;
        let (state_opt, _) = &*guard;
        if let Some(HandleState::Connected { daemon_version, .. }) = state_opt {
            if daemon_version.is_empty() {
                None
            } else {
                Some(daemon_version.clone())
            }
        } else {
            None
        }
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Ensure the handle is in `Connected` state and return a clone of the
    /// inner client `Arc` plus the cached tool name set.
    ///
    /// Why: Separates the state-machine logic (lazy-init, absent detection,
    /// backoff gating, spawn) from the tool-call I/O. This allows the outer
    /// `state` lock to be released before the long `call_tool` await and
    /// before the `list_tools` probe (issue #1164 fix).
    /// What: Phase 1 — acquires the outer `state` lock; runs all synchronous
    /// state transitions (self-heal reset, absent detection, backoff gate,
    /// spawn, initialize). When a tools/list probe is needed, packages the
    /// client into `maybe_probe` and exits the phase-1 scoped block so the
    /// borrow on `guard` ends. Phase 2 — drops `guard` BEFORE `list_tools().await`,
    /// then re-acquires it to record the Connected/Degraded outcome. This mirrors
    /// the call_tool lock discipline and prevents blocking concurrent callers
    /// (poll_metrics / route handlers) for the full network round-trip.
    /// `Degraded` within the backoff window returns `McpHandleError::Degraded`
    /// immediately (UI keeps showing the hint). `Degraded` after the window
    /// resets to `None` and re-runs the full probe (self-healing path). On
    /// success returns a clone of the `Arc<Mutex<Box<StdioMcpClient>>>` and
    /// the cached tool name set so the caller can drop the outer lock before
    /// invoking `call_tool`.
    /// Test: Exercised transitively by all handle tests and route tests.
    /// Self-healing: `mcp_handle_degraded_self_heals_after_backoff_window`.
    /// Lock discipline: `outer_lock_not_held_during_probe_outer_lock_remains_acquirable`.
    async fn ensure_connected(
        &self,
    ) -> Result<(Arc<Mutex<Box<StdioMcpClient>>>, HashSet<String>), McpHandleError> {
        // Phase 1: acquire the outer lock, run all synchronous state transitions
        // (self-heal reset, absent detection, backoff gate, spawn, initialize).
        // If a tools/list probe is needed, we package the client_arc and break
        // out of the phase-1 scoped block so the borrow on `guard` can end
        // before the async I/O starts (lock discipline — see issue #1164).
        let mut guard = self.state.lock().await;
        // Holds the ready client + server_info when a tools/list probe is
        // needed after phase 1.
        let maybe_probe: Option<(Arc<Mutex<Box<StdioMcpClient>>>, String)>;

        {
            let (state_opt, backoff) = &mut *guard;

            // Self-healing: if Degraded but the backoff window has elapsed, drop
            // back to None so the probe re-runs on this call.  While still inside
            // the window, fall through to the Degraded arm below which returns the
            // Degraded error so callers (and the UI) keep showing the hint.
            if matches!(state_opt, Some(HandleState::Degraded)) && backoff.should_attempt() {
                warn!(
                    binary = %self.binary,
                    "McpServiceHandle: Degraded handle backoff window elapsed — \
                     re-probing to attempt self-heal"
                );
                *state_opt = None;
            }

            if state_opt.is_none() {
                let resolved = which::which(&self.binary).ok();
                if resolved.is_none() {
                    warn!(
                        binary = %self.binary,
                        "McpServiceHandle: binary not found on PATH — marking as Absent"
                    );
                    *state_opt = Some(HandleState::Absent);
                    maybe_probe = None;
                } else {
                    if !backoff.should_attempt() {
                        let next_in = backoff
                            .next_attempt
                            .saturating_duration_since(Instant::now());
                        warn!(
                            binary = %self.binary,
                            failure_count = backoff.failure_count,
                            next_attempt_secs = ?next_in,
                            "McpServiceHandle: spawn is in backoff — skipping this cycle"
                        );
                        return Err(McpHandleError::Backoff {
                            failure_count: backoff.failure_count,
                            next_attempt_in: next_in,
                        });
                    }

                    debug!(binary = %self.binary, "McpServiceHandle: spawning MCP child");
                    let args_ref: Vec<&str> = self.args.iter().map(String::as_str).collect();
                    match StdioMcpClient::spawn(&self.binary, &args_ref, "trusty-console").await {
                        Ok(mut client) => {
                            let server_info = match client.initialize().await.with_context(|| {
                                format!(
                                    "McpServiceHandle: MCP initialize failed for {}",
                                    self.binary
                                )
                            }) {
                                Ok(info) => info,
                                Err(e) => {
                                    backoff.record_failure();
                                    warn!(
                                        binary = %self.binary,
                                        failure_count = backoff.failure_count,
                                        error = %e,
                                        "McpServiceHandle: initialize failed — will retry after backoff"
                                    );
                                    return Err(McpHandleError::Other(e));
                                }
                            };

                            // tools/list probe needed. Wrap the client NOW but do NOT
                            // call list_tools yet — that I/O must happen outside the
                            // outer lock (issue #1164). Signal to phase 2 via maybe_probe.
                            let client_arc = Arc::new(Mutex::new(Box::new(client)));
                            maybe_probe = Some((Arc::clone(&client_arc), server_info.version));
                            // Leave state_opt as None; phase 2 will set it after the
                            // probe result is known.
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
                            return Err(McpHandleError::Other(e.context(format!(
                                "McpServiceHandle: failed to spawn {} (failure #{})",
                                self.binary, backoff.failure_count
                            ))));
                        }
                    }
                }
            } else {
                maybe_probe = None;
            }
        } // `state_opt` and `backoff` borrows of `guard` end here.

        // Phase 2: tools/list probe (if needed).
        //
        // Drop the outer guard BEFORE the async I/O so concurrent callers
        // (poll_metrics / route handlers) are not blocked for the full
        // tools/list network round-trip. Re-acquire afterward to record the
        // Connected/Degraded outcome. This mirrors the call_tool path.
        // Fixes issue #1164.
        if let Some((client_arc, daemon_version)) = maybe_probe {
            // tools/list probe: verify the service exposes the expected
            // console_metrics tool. A missing tool means the binary is
            // running but in the wrong mode (e.g. HTTP-only, not stdio MCP).
            // We mark the handle Degraded rather than silently failing —
            // poll_metrics will never succeed in this state.
            // Also cache the full tool name set for capability-gating.
            drop(guard);
            let probe_result = client_arc.lock().await.list_tools().await;
            // Re-acquire the outer guard to record the probe outcome.
            guard = self.state.lock().await;
            let (state_opt, backoff) = &mut *guard;
            match probe_result {
                Ok(tools) => {
                    let tool_names: HashSet<String> =
                        tools.iter().map(|t| t.name.clone()).collect();
                    let has_metrics = tool_names.contains(CONSOLE_METRICS_METHOD);
                    if !has_metrics {
                        // Record a failure so the backoff window is non-zero:
                        // the self-healing path in `ensure_connected` requires
                        // `backoff.should_attempt()` to return true before it
                        // resets Degraded → None and re-probes. Using
                        // record_failure (not reset) guarantees at least
                        // BACKOFF_BASE_MS (1 s) before the re-probe is
                        // triggered, so the UI shows the degraded badge for at
                        // least one full poll cycle.
                        backoff.record_failure();
                        warn!(
                            binary = %self.binary,
                            failure_count = backoff.failure_count,
                            next_attempt_secs = ?backoff.next_attempt.saturating_duration_since(Instant::now()),
                            "McpServiceHandle: tools/list OK but \
                             `console_metrics` not listed — marking Degraded; \
                             will self-heal after backoff window"
                        );
                        *state_opt = Some(HandleState::Degraded);
                    } else {
                        backoff.reset();
                        *state_opt = Some(HandleState::Connected {
                            client: Arc::clone(&client_arc),
                            tool_names,
                            daemon_version,
                        });
                    }
                }
                Err(e) => {
                    // tools/list failure is treated as a spawn failure so the
                    // backoff gate applies on the next attempt.
                    backoff.record_failure();
                    warn!(
                        binary = %self.binary,
                        failure_count = backoff.failure_count,
                        error = %e,
                        "McpServiceHandle: tools/list failed after \
                         initialize — will retry after backoff"
                    );
                    return Err(McpHandleError::Other(e.context(format!(
                        "McpServiceHandle: tools/list failed for {}",
                        self.binary
                    ))));
                }
            }
        }

        let (final_state, _) = &*guard;
        match final_state.as_ref() {
            Some(HandleState::Absent) => Err(McpHandleError::Absent),
            Some(HandleState::Degraded) => Err(McpHandleError::Degraded {
                hint: DEGRADED_HINT.to_string(),
            }),
            Some(HandleState::Connected {
                client, tool_names, ..
            }) => Ok((Arc::clone(client), tool_names.clone())),
            None => unreachable!("guard must be Some after init block"),
        }
    }

    /// Record a successful `call_tool` invocation by resetting the backoff.
    ///
    /// Why: On success we reset the failure counter so transient crashes don't
    /// permanently throttle the poller.
    /// What: Acquires the outer lock briefly and resets `SpawnBackoff`.
    /// Test: Exercised transitively by the live-pipe smoke test.
    async fn on_call_success(&self) {
        let mut guard = self.state.lock().await;
        let (_state_opt, backoff) = &mut *guard;
        backoff.reset();
    }

    /// Record a failed `call_tool` invocation: increment backoff and reset
    /// state to `None` so the next call re-enters lazy-init.
    ///
    /// Why: On error we record a failure and drop the client back to `None`
    /// so the next poll re-enters the lazy-init block and respects the
    /// backoff window.
    /// What: Acquires the outer lock briefly, calls `backoff.record_failure()`,
    /// and sets `state_opt = None`.
    ///
    /// ## Known TOCTOU behaviour (accepted, benign)
    ///
    /// Between `ensure_connected` returning the `Arc<Mutex<StdioMcpClient>>`
    /// and `on_call_failure` resetting `state_opt` back to `None`, a concurrent
    /// caller can observe the `Connected` state and increment the same connection's
    /// reference count. When `on_call_failure` then resets the state to `None`,
    /// that concurrent caller still holds a valid `Arc` to the old (possibly dead)
    /// client and may receive its own `call_tool` error — which itself calls
    /// `on_call_failure` again, resetting the state a second time and discarding
    /// any newer connection that a third concurrent caller may have just
    /// established in the intervening `ensure_connected` call.
    ///
    /// **Why this is accepted:** the extra reset is harmless — the state ends up
    /// `None` and `backoff.failure_count` is incremented by at most one extra
    /// count. The next successful `ensure_connected` + spawn resets the backoff
    /// entirely (`SpawnBackoff::reset`). The worst case is one unnecessary
    /// respawn cycle (the cost is a brief backoff window), not data loss or
    /// permanent failure. A generation-counter guard would prevent the redundant
    /// reset but adds complexity disproportionate to the benefit; deferring for
    /// a future refactor.
    ///
    /// Test: `mcp_handle_respawn_failure_applies_backoff`.
    async fn on_call_failure(&self) {
        let mut guard = self.state.lock().await;
        let (state_opt, backoff) = &mut *guard;
        backoff.record_failure();
        *state_opt = None;
        warn!(
            binary = %self.binary,
            failure_count = backoff.failure_count,
            next_attempt_secs = ?backoff.next_attempt.saturating_duration_since(Instant::now()),
            "McpServiceHandle: tool call/respawn failed — resetting to None, \
             will retry after backoff"
        );
    }
}

// ── MCP content envelope helper ──────────────────────────────────────────────

/// Unwrap the MCP tool-call response envelope to return the payload value.
///
/// Why: `StdioMcpClient::call_tool` returns the full MCP response object
/// `{"content":[{"type":"text","text":"<JSON-string>"}],"isError":false}`.
/// Route handlers need the inner payload, not the envelope, so they can
/// return clean JSON to the browser without the MCP framing.
/// What: Extracts `content[0].text`, tries to parse it as JSON. If the
/// text is not valid JSON (or the envelope shape is unexpected), returns the
/// raw Value unchanged so the caller always gets *something*.
/// Test: Inline unit test `unwrap_mcp_content_extracts_text_json` in `tests.rs`.
pub(super) fn unwrap_mcp_content(raw: Value) -> Value {
    // Expected shape: {"content":[{"type":"text","text":"..."}],"isError":false}
    if let Some(text) = raw
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|item| item.get("text"))
        .and_then(|t| t.as_str())
    {
        // Try to parse the inner text as JSON. If it parses, return the
        // parsed value. If not, return the raw string as a JSON string value.
        match serde_json::from_str::<Value>(text) {
            Ok(inner) => return inner,
            Err(_) => return Value::String(text.to_string()),
        }
    }
    // Envelope shape was not as expected — return raw.
    raw
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
