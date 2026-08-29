//! Starting the `trusty-search` daemon the whole audit stack stands on (#5670).
//!
//! Why: the audit's prerequisite chain is trusty-search → per-repository index →
//! trusty-analyze, and [`super::analyze`] closed only the last link. Starting
//! `trusty-analyze` is not enough on its own, in two different ways.
//!
//! On a cold machine `trusty-analyze serve` exits at its own trusty-search check
//! before it ever binds a socket, so the analyze preflight spawns a process that
//! is already gone by the first poll and refuses the run. The operator's remedy was
//! to run `trusty-search start` by hand, which DOC-67 §2 does not allow: the
//! sweep gets one non-interactive shot and owes no manual prerequisite.
//!
//! The second case is the one a fresh-spawn fix misses. `trusty-analyze` reports
//! itself degraded whenever trusty-search is unreachable, and
//! [`super::analyze`]'s health check counts only `status: "ok"` — so the analyze
//! preflight re-reads trusty-search's LIVE status on every run. An analyze daemon
//! that has been up for days on top of a trusty-search that died an hour ago
//! fails the probe, the spawned replacement exits at its own search check, the
//! original keeps reporting degraded, and the readiness poll refuses. Nothing
//! about that run is a cold start.
//!
//! What: [`ensure_search_daemon`], the socket and binary rules it applies, and
//! [`SearchDaemonUnavailable`]. It runs BEFORE the analyze preflight in
//! `crate::commands::audit::run`, which is the whole point — analyze cannot boot
//! without it.
//!
//! #6285: trusty-search binds one hardened Unix socket and speaks framed
//! JSON-RPC, so the discovery files this used to read describe a listener that
//! is being retired. The daemon and this caller now DERIVE the same path from
//! [`trusty_common::daemon_socket_path`], which is what removes the resolution
//! step entirely — there is no address to discover, no port to fall back to,
//! and nothing for a stale `http_addr` to contradict. This is the same shape
//! [`super::analyze`] took for trusty-analyze in #6287, and it is the crate's
//! only trusty-search dial.
//!
//! Nothing here is fail-open. Both failure arms — the binary would not spawn, and
//! the daemon never answered — return `Err`, for the same reason the analyze
//! preflight refuses: a report with its findings, complexity and health sections
//! empty reads as a clean bill of health rather than as an outage.
//! Test: `super::tests` against stubs; `super::real_binary_tests` (`#[ignore]`d)
//! against the real `trusty-search` binary.
//!
//! # Spec References
//! - [`SPEC-TGAUDIT-06~draft`](../../../../docs/specs/DOC-67-tga-audit-mode.md#SPEC-TGAUDIT-06~draft)
//! - [`SPEC-TGAUDIT-09~draft`](../../../../docs/specs/DOC-67-tga-audit-mode.md#SPEC-TGAUDIT-09~draft)

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use trusty_common::daemon_guard::{spawn_detached, DEFAULT_POLL_INTERVAL};

use super::repo_index::{resolve_search_binary, ENV_SEARCH_BIN};

/// Wall-clock budget for a freshly-spawned `trusty-search` to report healthy.
///
/// Why: 60s, matching `trusty-search`'s own guard
/// (`crates/trusty-search/src/commands/daemon_guard.rs`'s `READY_TIMEOUT`) rather
/// than `daemon_guard::DEFAULT_STARTUP_TIMEOUT`'s 30s. The socket binds in about
/// a second, but a first run on a machine with no model cache spends 15–30s in
/// ONNX load before it answers, and refusing an audit at 30s would turn a slow
/// cold start into a failed engagement.
pub const SEARCH_STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

/// Environment variable overriding the trusty-search daemon's socket.
///
/// Why: it replaces `TRUSTY_DATA_DIR` as the way a rig points this guard at a
/// daemon it started, without redirecting every other trusty-* client in the
/// same process. Same name trusty-mpm and trusty-audit read, so an operator
/// pins one daemon for the whole stack with one export.
pub const ENV_SEARCH_SOCKET: &str = "TRUSTY_SEARCH_SOCKET";

/// The method `trusty-search` answers a health probe on.
///
/// Duplicated as a literal rather than imported: `tga` has no Cargo edge on
/// `trusty-search`. `trusty_search::service::socket::METHOD_HEALTH` is the
/// definition, and the daemon's own
/// `rpc_router_registers_every_documented_method` is what keeps its router
/// equal to it. A name that drifted answers `method_not_found`, which
/// [`search_is_healthy`] reports as an unhealthy daemon.
const SEARCH_HEALTH_METHOD: &str = "search.health";

/// How long one health dial may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// The audit cannot proceed without the trusty-search daemon.
///
/// Why: this is refused before the sweep rather than reported after it, so the
/// message is the operator's whole remedy. It names trusty-search as the FIRST
/// link rather than describing the analyze symptom, because an operator reading
/// "trusty-analyze is degraded" reaches for the wrong daemon.
/// What: the address probed, the binary tried, and the underlying cause.
/// Test: `super::tests::an_unspawnable_search_binary_refuses_the_audit`.
#[derive(Debug, thiserror::Error)]
#[error(
    "trusty-search is not serving {socket} and could not be started ({cause}). Every \
     analyze-derived section of the audit report stands on it: `trusty-analyze serve` exits \
     immediately when trusty-search is unreachable, and an analyze daemon that is already \
     running reports itself degraded for as long as it stays unreachable — so the findings \
     table, the complexity distribution and the health factors would all render empty, which \
     reads as a clean bill of health rather than as an outage. Start it first:\n\n    \
     trusty-search start\n\n\
     Override the binary with {ENV_SEARCH_BIN}, and the socket with {ENV_SEARCH_SOCKET}."
)]
#[non_exhaustive]
pub struct SearchDaemonUnavailable {
    /// The socket that was probed, rendered as a path.
    pub socket: String,
    /// The binary name or path that was tried.
    pub binary: String,
    /// What stopped it — a spawn failure, or the readiness timeout.
    pub cause: String,
}

/// Where and how [`ensure_search_daemon`] looks for the daemon.
///
/// Why: the socket and binary come from the environment and the two budgets are
/// fixed, which makes the whole guard untestable if it reads them itself. Taking
/// them as a value is what lets a test drive the spawn-and-poll path against a
/// stub executable on a temp socket — the same split [`super::AnalyzeGuard`]
/// uses.
/// What: the daemon socket, the binary to spawn, and the readiness budget.
/// Test: `super::tests::a_reachable_search_daemon_is_not_restarted`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SearchGuard {
    /// The daemon's Unix socket (#6285, ADR-0032).
    pub socket: PathBuf,
    /// Binary name or path to spawn when the daemon is absent.
    pub binary: String,
    /// Wall-clock budget for a freshly-spawned daemon to report healthy.
    pub startup_timeout: Duration,
    /// Interval between readiness probes.
    pub poll_interval: Duration,
}

impl SearchGuard {
    /// The guard this process runs with, resolved from the environment.
    ///
    /// #6285: the socket is DERIVED, not discovered. The daemon binds
    /// `trusty_common::daemon_socket_path("trusty-search")`
    /// (`trusty_search::service::socket::socket_path`) and this guard computes
    /// the identical path, so there is nothing published between them for a
    /// stale write to contradict — which is what the `http_addr` discovery file
    /// this used to read could do. [`ENV_SEARCH_SOCKET`] overrides it for a rig
    /// that started its own daemon. A resolution failure yields an empty path,
    /// which [`ensure_search_daemon_with`] then reports as unreachable — the
    /// same outcome a wrong path would produce, without the guess.
    ///
    /// The binary comes from [`ENV_SEARCH_BIN`] via [`resolve_search_binary`] —
    /// the same resolver [`super::ensure_repositories_indexed`] uses, so the
    /// daemon this starts and the binary that indexes into it are never two
    /// different installs.
    /// Test: `super::tests::the_search_guard_derives_the_socket_the_daemon_binds`.
    pub fn from_env() -> Self {
        Self {
            socket: search_socket(),
            binary: resolve_search_binary(),
            startup_timeout: SEARCH_STARTUP_TIMEOUT,
            poll_interval: DEFAULT_POLL_INTERVAL,
        }
    }
}

impl Default for SearchGuard {
    fn default() -> Self {
        Self::from_env()
    }
}

/// The trusty-search daemon's socket: [`ENV_SEARCH_SOCKET`], else the derived
/// default.
///
/// Why/What: see [`SearchGuard::from_env`].
/// Test: `super::tests::the_search_guard_derives_the_socket_the_daemon_binds`.
#[must_use]
pub fn search_socket() -> PathBuf {
    socket_from_override(std::env::var(ENV_SEARCH_SOCKET).ok().as_deref())
}

/// The override rule itself: a non-empty value wins, everything else defaults.
///
/// Split out so the rule is asserted without any test reading or writing the
/// process environment — `set_var` is `unsafe` in edition 2024 and unsound under
/// the parallel harness.
fn socket_from_override(value: Option<&str>) -> PathBuf {
    match value.filter(|s| !s.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => trusty_common::daemon_socket_path("trusty-search").unwrap_or_default(),
    }
}

/// Is the trusty-search daemon at `socket` answering?
///
/// Why any result frame counts: `GET /health` on trusty-search answered 200
/// unconditionally — its handler returns a body with no status code of its own —
/// so `probe_once` treated a daemon that was up and warning about one index as
/// reachable. Reading `status` here would newly respawn such a daemon, which is
/// a behaviour change #6285 does not owe. [`super::analyze`]'s probe reads
/// `status` because trusty-analyze's HTTP route really did answer 503.
///
/// What: one `search.health` frame; `true` only for a result frame. A dial
/// failure and an error frame are both `false`, so an RPC error can never read
/// as a healthy daemon and let the audit proceed onto a daemon that is not
/// serving.
/// Test: `super::tests::{a_reachable_search_daemon_is_not_restarted,
/// a_search_daemon_that_refuses_health_is_not_reachable}`.
async fn search_is_healthy(socket: &Path, timeout: Duration) -> bool {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": SEARCH_HEALTH_METHOD,
    });
    let Ok(response) = trusty_common::uds::send_framed_request::<
        _,
        trusty_common::uds::server::RpcResponse,
    >(socket, &request, timeout)
    .await
    else {
        return false;
    };
    response.result.is_some()
}

/// The exact argument vector the guard hands `trusty-search`.
///
/// Why: this list IS the tga→trusty-search contract, and `--foreground` is
/// load-bearing rather than decorative — a bare `trusty-search start` re-spawns
/// itself as a background daemon and the parent exits, which is why
/// trusty-search's own guard passes the flag
/// (`crates/trusty-search/src/commands/daemon_guard.rs`'s
/// `spawn_daemon_with_device`). We already detach the child ourselves, so the
/// second fork would only cost the guard its view of the process it started.
/// Building the vector in a pure function is what lets a test assert its contents
/// without spawning anything.
/// What: `start --foreground`.
/// Test: `super::tests::the_search_spawn_arguments_are_start_in_the_foreground`.
pub(super) fn start_args() -> Vec<String> {
    vec!["start".to_string(), "--foreground".to_string()]
}

/// Ensure the trusty-search daemon is up before anything else in the audit.
///
/// Why/What: see the module docs. Resolves the guard from the environment and
/// delegates to [`ensure_search_daemon_with`], so the environment and the
/// discovery files are read exactly once, at the public entry point.
///
/// # Errors
///
/// [`SearchDaemonUnavailable`] when the daemon is absent and cannot be started.
///
/// Test: `super::tests::a_reachable_search_daemon_is_not_restarted`.
pub async fn ensure_search_daemon() -> Result<(), SearchDaemonUnavailable> {
    ensure_search_daemon_with(&SearchGuard::from_env()).await
}

/// [`ensure_search_daemon`] with the address, binary and budgets already fixed.
///
/// Why: taking them as a value is what lets a test drive the whole spawn-and-poll
/// path against a stub executable on an ephemeral port, without touching the
/// process environment or leaving a daemon behind.
/// What: calls `search.health` on the guard's socket; on a result frame,
/// returns without spawning anything. On anything else, spawns
/// `<binary> start --foreground` detached and polls until it answers. Neither
/// failure is downgraded — a spawn that fails and a daemon that never answers
/// both return `Err`.
///
/// The socket is resolved once, before the spawn, and the poll reuses it, so a
/// daemon this guard starts is found on the same path the daemon's own clients
/// derive.
///
/// # Errors
///
/// [`SearchDaemonUnavailable`] carrying the spawn error, or the readiness timeout.
///
/// Test: `super::tests::{a_reachable_search_daemon_is_not_restarted,
/// an_unspawnable_search_binary_refuses_the_audit,
/// a_search_daemon_that_never_comes_up_refuses_the_audit,
/// the_search_guard_recovers_a_stale_degraded_analyze_daemon}`.
pub async fn ensure_search_daemon_with(guard: &SearchGuard) -> Result<(), SearchDaemonUnavailable> {
    if search_is_healthy(&guard.socket, PROBE_TIMEOUT).await {
        return Ok(());
    }

    let refuse = |cause: String| SearchDaemonUnavailable {
        socket: guard.socket.display().to_string(),
        binary: guard.binary.clone(),
        cause,
    };

    eprintln!(
        "[tga audit] trusty-search is not serving {}; starting `{} start --foreground`…",
        guard.socket.display(),
        guard.binary
    );
    let args = start_args();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    spawn_detached(&guard.binary, &args).map_err(|e| refuse(e.to_string()))?;

    // #6285: a local poll loop rather than `spin_until_ready`, which takes a
    // health URL this daemon no longer has. The budget and the verdict are
    // unchanged; only the probe moved. Same shape as `super::analyze`'s loop.
    let deadline = Instant::now() + guard.startup_timeout;
    while Instant::now() < deadline {
        tokio::time::sleep(guard.poll_interval).await;
        if search_is_healthy(&guard.socket, PROBE_TIMEOUT).await {
            return Ok(());
        }
    }
    Err(refuse(format!(
        "it did not report healthy within {}s — run `trusty-search start` by hand to see why it \
         will not stay up",
        guard.startup_timeout.as_secs()
    )))
}
