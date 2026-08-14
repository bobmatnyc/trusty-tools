//! Starting the `trusty-search` daemon the whole audit stack stands on (#5670).
//!
//! Why: the audit's prerequisite chain is trusty-search → per-repository index →
//! trusty-analyze, and [`super::analyze`] closed only the last link. Starting
//! `trusty-analyze` is not enough on its own, in two different ways.
//!
//! On a cold machine `trusty-analyze serve` exits at its own trusty-search check
//! before it ever binds a port, so the analyze preflight spawns a process that is
//! already gone by the first poll and refuses the run. The operator's remedy was
//! to run `trusty-search start` by hand, which DOC-67 §2 does not allow: the
//! sweep gets one non-interactive shot and owes no manual prerequisite.
//!
//! The second case is the one a fresh-spawn fix misses. `trusty-analyze` answers
//! its own `/health` with `503 degraded` whenever trusty-search is unreachable,
//! and `probe_once` counts only a 2xx — so the analyze preflight re-reads
//! trusty-search's LIVE status on every run. An analyze daemon that has been up
//! for days on top of a trusty-search that died an hour ago fails the probe, the
//! spawned replacement exits at its own search check, the original keeps
//! answering 503, and the readiness poll refuses. Nothing about that run is a
//! cold start.
//!
//! What: [`ensure_search_daemon`], the address and binary rules it applies, and
//! [`SearchDaemonUnavailable`]. It runs BEFORE the analyze preflight in
//! `crate::commands::audit::run`, which is the whole point — analyze cannot boot
//! without it. The probe/spawn/poll loop is `trusty_common::daemon_guard`, the
//! workspace's one daemon-lifecycle entry point, and the address comes from
//! [`DaemonAddrLayout::TRUSTY_SEARCH`] rather than a second copy of
//! trusty-search's discovery rules.
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

use std::time::Duration;

use trusty_common::daemon_guard::{
    probe_once, spawn_detached, spin_until_ready, DaemonAddrLayout, DaemonGuardConfig,
    DEFAULT_POLL_INTERVAL,
};

use super::repo_index::{resolve_search_binary, ENV_SEARCH_BIN};

/// Wall-clock budget for a freshly-spawned `trusty-search` to answer `/health`.
///
/// Why: 60s, matching `trusty-search`'s own guard
/// (`crates/trusty-search/src/commands/daemon_guard.rs`'s `READY_TIMEOUT`) rather
/// than `daemon_guard::DEFAULT_STARTUP_TIMEOUT`'s 30s. The HTTP port binds in
/// about a second, but a first run on a machine with no model cache spends 15–30s
/// in ONNX load before it answers, and refusing an audit at 30s would turn a slow
/// cold start into a failed engagement.
pub const SEARCH_STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

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
    "trusty-search is not reachable at {url} and could not be started ({cause}). Every \
     analyze-derived section of the audit report stands on it: `trusty-analyze serve` exits \
     immediately when trusty-search is unreachable, and an analyze daemon that is already \
     running answers `503 degraded` for as long as it stays unreachable — so the findings table, \
     the complexity distribution and the health factors would all render empty, which reads as a \
     clean bill of health rather than as an outage. Start it first:\n\n    trusty-search start\n\n\
     Override the binary with {ENV_SEARCH_BIN}, and the address by pointing TRUSTY_DATA_DIR at \
     the instance's data directory."
)]
#[non_exhaustive]
pub struct SearchDaemonUnavailable {
    /// The address that was probed.
    pub url: String,
    /// The binary name or path that was tried.
    pub binary: String,
    /// What stopped it — a spawn failure, or the readiness timeout.
    pub cause: String,
}

/// Where and how [`ensure_search_daemon`] looks for the daemon.
///
/// Why: the address and binary come from the environment and the two budgets are
/// fixed, which makes the whole guard untestable if it reads them itself. Taking
/// them as a value is what lets a test drive the spawn-and-poll path against a
/// stub executable on an ephemeral port — the same split [`super::AnalyzeGuard`]
/// uses.
/// What: the daemon address, the binary to spawn, and the readiness budget.
/// Test: `super::tests::a_reachable_search_daemon_is_not_restarted`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SearchGuard {
    /// Base address of the daemon, e.g. `http://127.0.0.1:7878`.
    pub url: String,
    /// Binary name or path to spawn when the daemon is absent.
    pub binary: String,
    /// Wall-clock budget for a freshly-spawned daemon to answer `/health`.
    pub startup_timeout: Duration,
    /// Interval between readiness probes.
    pub poll_interval: Duration,
}

impl SearchGuard {
    /// The guard this process runs with, resolved from the environment.
    ///
    /// Why: the address is NOT a `tga` setting and there is no tga-side variable
    /// for it. `trusty-search` binds an OS-assigned port and records it in its own
    /// discovery files, so the only correct answer comes from reading those files
    /// the way every other client of that daemon reads them — which is
    /// [`DaemonAddrLayout::TRUSTY_SEARCH`], promoted into `trusty-common` for
    /// exactly this caller (#5670). Hard-coding `127.0.0.1:7878` here would miss
    /// an auto-ported daemon and every `TRUSTY_DATA_DIR`-isolated one.
    /// What: resolves the address from the discovery files and the binary from
    /// [`ENV_SEARCH_BIN`] via [`resolve_search_binary`] — the same resolver
    /// [`super::ensure_repositories_indexed`] uses, so the daemon this starts and
    /// the binary that indexes into it are never two different installs.
    /// Test: `super::tests::the_search_guard_resolves_its_address_from_the_shared_layout`.
    pub fn from_env() -> Self {
        Self {
            url: DaemonAddrLayout::TRUSTY_SEARCH.resolve_base_url(),
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

/// The health endpoint for a base address, tolerating a trailing slash.
fn health_url(base: &str) -> String {
    format!("{}/health", base.trim_end_matches('/'))
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
/// What: probes `<url>/health`; on a hit, returns without spawning anything. On a
/// miss, spawns `<binary> start --foreground` detached and polls until it answers.
/// Neither failure is downgraded — a spawn that fails and a daemon that never
/// answers both return `Err`.
///
/// The address is resolved once, before the spawn, and the poll reuses it. That
/// is what `trusty-search`'s own CLI does at every call site
/// (`commands::add`'s `ensure_daemon_running_or_exit(&daemon_base_url())`), so a
/// daemon this guard starts is found the same way the daemon's own client would
/// find it.
///
/// # Errors
///
/// [`SearchDaemonUnavailable`] carrying the spawn error, or the readiness timeout.
///
/// Test: `super::tests::{a_reachable_search_daemon_is_not_restarted,
/// an_unspawnable_search_binary_refuses_the_audit,
/// a_search_daemon_that_never_comes_up_refuses_the_audit,
/// the_search_guard_starts_the_daemon_the_analyze_preflight_then_needs}`.
pub async fn ensure_search_daemon_with(guard: &SearchGuard) -> Result<(), SearchDaemonUnavailable> {
    let health = health_url(&guard.url);
    if probe_once(&health).await {
        return Ok(());
    }

    let refuse = |cause: String| SearchDaemonUnavailable {
        url: guard.url.clone(),
        binary: guard.binary.clone(),
        cause,
    };

    eprintln!(
        "[tga audit] trusty-search is not answering at {}; starting `{} start --foreground`…",
        guard.url, guard.binary
    );
    let args = start_args();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    spawn_detached(&guard.binary, &args).map_err(|e| refuse(e.to_string()))?;

    let config = DaemonGuardConfig {
        health_url: health,
        service_name: "trusty-search".to_string(),
        startup_timeout: guard.startup_timeout,
        poll_interval: guard.poll_interval,
        timeout_hint: "run `trusty-search start` by hand to see why it will not stay up"
            .to_string(),
    };
    spin_until_ready(&config)
        .await
        .map_err(|e| refuse(e.to_string()))
}
