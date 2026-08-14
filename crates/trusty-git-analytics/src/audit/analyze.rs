//! Starting the `trusty-analyze` daemon the audit's report is built from (#5670).
//!
//! Why: `tga audit` always renders with `--analyze` (see [`super::review`]), and
//! DOC-67 §8 sources the findings table, the complexity distribution and the
//! health factors from that daemon alone. Nothing started it. The renderer's
//! client only ever issues GETs, and its fail-open contract (DOC-67 §9) turns a
//! refused connection into one gap line inside the artifact — so an unattended
//! audit on a machine with no daemon delivered a report whose three
//! analyze-derived sections were empty, and exited 0.
//!
//! This module is the decision that closes it: `tga audit` starts the daemon
//! itself, as a whole-run preflight, and refuses the run when it cannot. It sits
//! beside the two preflights already in `crate::commands::audit` — the inference
//! credential and the renderer version — for the same DOC-67 §2 reason: the
//! sweep gets one non-interactive shot, and a precondition knowable in
//! milliseconds must not be discovered after minutes of collection.
//!
//! What: [`ensure_analyze_daemon`], the binary/URL resolution rules it applies,
//! and [`AnalyzeDaemonUnavailable`]. The probe/spawn/poll loop itself is
//! `trusty_common::daemon_guard`, the workspace's one daemon-lifecycle entry
//! point — the same one `trusty-analyze`'s own guard routes through.
//!
//! Nothing here is fail-open. Both failure arms — the binary would not spawn,
//! and the daemon never answered — return `Err`, because a report missing those
//! sections reads as a clean pass rather than as an outage.
//! Test: `super::tests`.
//!
//! # Spec References
//! - [`SPEC-TGAUDIT-06~draft`](../../../../docs/specs/DOC-67-tga-audit-mode.md#SPEC-TGAUDIT-06~draft)
//! - [`SPEC-TGAUDIT-09~draft`](../../../../docs/specs/DOC-67-tga-audit-mode.md#SPEC-TGAUDIT-09~draft)

use std::time::Duration;

use trusty_common::daemon_guard::{
    probe_once, spawn_detached, spin_until_ready, DaemonGuardConfig, DEFAULT_POLL_INTERVAL,
    DEFAULT_STARTUP_TIMEOUT,
};

/// Environment variable that overrides the `trusty-analyze` binary path.
///
/// Why: `trusty-audit` already sets this on every `tga audit` child it spawns
/// (`crates/trusty-audit/src/run.rs`) so the sweep runs the engagement's pinned
/// copy rather than whatever is on the operator's PATH. Until now nothing on
/// this path read it — #5663 set it believing it pinned the analyze step, and it
/// reached only `trusty-review review`'s subprocess client. Reading it here is
/// what makes that belief true.
pub const ENV_ANALYZE_BIN: &str = "TRUSTY_ANALYZE_BIN";

/// Default binary name searched on PATH.
pub const DEFAULT_ANALYZE_BIN: &str = "trusty-analyze";

/// Environment variable naming the analyze daemon's address.
///
/// Why: `trusty-review` resolves the same variable for the URL it fetches from,
/// so the daemon this starts and the daemon the renderer queries are the same
/// process by construction — two spellings would let them disagree silently.
pub const ENV_ANALYZE_URL: &str = "PR_INTELLIGENCE_ANALYZER_URL";

/// Default analyze daemon address, matching `trusty-review`'s own default.
pub const DEFAULT_ANALYZE_URL: &str = "http://localhost:7879";

/// The port `trusty-analyze serve` binds when the URL names none.
pub const DEFAULT_ANALYZE_PORT: u16 = 7879;

/// The audit cannot proceed without the analyze daemon.
///
/// Why: this is refused before the sweep rather than reported after it, so the
/// message is the operator's whole remedy — and the remedy is almost never "run
/// the binary again". `trusty-analyze serve` exits immediately when
/// `trusty-search` is unreachable, which is the usual reason a spawn produces no
/// daemon, so the message names that ordering rather than the symptom.
/// What: the address probed, the binary tried, and the underlying cause.
/// Test: `super::tests::an_analyze_daemon_that_never_comes_up_refuses_the_audit`.
#[derive(Debug, thiserror::Error)]
#[error(
    "trusty-analyze is not reachable at {url} and could not be started ({cause}). `tga audit` \
     renders its report with `trusty-review report --analyze`, and DOC-67 §8 sources the findings \
     table, the complexity distribution and the health factors from that daemon and nowhere else — \
     without it those three sections render empty, which reads as a clean bill of health rather \
     than as an outage. Start the stack first:\n\n    trusty-search start\n    {binary} serve\n\n\
     `trusty-analyze serve` exits immediately when trusty-search is unreachable, so trusty-search \
     goes first. Override the binary with {ENV_ANALYZE_BIN} and the address with {ENV_ANALYZE_URL}."
)]
#[non_exhaustive]
pub struct AnalyzeDaemonUnavailable {
    /// The address that was probed.
    pub url: String,
    /// The binary name or path that was tried.
    pub binary: String,
    /// What stopped it — a spawn failure, or the readiness timeout.
    pub cause: String,
}

/// Where and how [`ensure_analyze_daemon`] looks for the daemon.
///
/// Why: the two values come from the environment and the two budgets are fixed,
/// which makes the whole guard untestable if it reads them itself. Taking them
/// as a value is what lets a test drive the spawn-and-poll path against a stub
/// executable on an ephemeral port — the same split
/// [`super::review::binary_from_override`] uses for its rule.
/// What: the daemon address, the binary to spawn, and the readiness budget.
/// Test: `super::tests::a_reachable_analyze_daemon_is_not_restarted`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AnalyzeGuard {
    /// Base address of the daemon, e.g. `http://localhost:7879`.
    pub url: String,
    /// Binary name or path to spawn when the daemon is absent.
    pub binary: String,
    /// Wall-clock budget for a freshly-spawned daemon to answer `/health`.
    pub startup_timeout: Duration,
    /// Interval between readiness probes.
    pub poll_interval: Duration,
}

impl AnalyzeGuard {
    /// The guard this process runs with, resolved from the environment.
    ///
    /// Why/What: reads [`ENV_ANALYZE_URL`] and [`ENV_ANALYZE_BIN`], applying
    /// [`url_from_override`] and [`binary_from_override`]. Reading the two
    /// variables is all this does; the rules live in those pure functions so the
    /// tests never call `std::env::set_var`, which is `unsafe` in edition 2024
    /// and unsound under parallel tests (#5308 review).
    /// Test: `super::tests::analyze_resolution_prefers_the_env_overrides`.
    pub fn from_env() -> Self {
        Self {
            url: url_from_override(std::env::var(ENV_ANALYZE_URL).ok().as_deref()),
            binary: binary_from_override(std::env::var(ENV_ANALYZE_BIN).ok().as_deref()),
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            poll_interval: DEFAULT_POLL_INTERVAL,
        }
    }
}

/// The binary resolution rule: an override wins unless it is empty.
///
/// Why/What/Test: see [`AnalyzeGuard::from_env`]. `None` and `Some("")` both
/// fall back to [`DEFAULT_ANALYZE_BIN`].
pub(super) fn binary_from_override(override_value: Option<&str>) -> String {
    override_value
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_ANALYZE_BIN)
        .to_string()
}

/// The address resolution rule: an override wins unless it is empty.
///
/// Why/What/Test: see [`AnalyzeGuard::from_env`]. `None` and `Some("")` both
/// fall back to [`DEFAULT_ANALYZE_URL`].
pub(super) fn url_from_override(override_value: Option<&str>) -> String {
    override_value
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_ANALYZE_URL)
        .to_string()
}

/// The port to bind, read out of the daemon's address.
///
/// Why: `trusty-analyze serve` takes `--port`, not a URL, so an operator who
/// moved the daemon with [`ENV_ANALYZE_URL`] must get a daemon on THAT port —
/// spawning on 7879 and then probing the override would hang for the full
/// budget and refuse a correctly-configured run.
/// What: the authority's trailing `:<port>`, or [`DEFAULT_ANALYZE_PORT`] for an
/// address that names none or names one this cannot read.
/// Test: `super::tests::the_spawn_port_comes_from_the_configured_url`.
pub(super) fn port_of(url: &str) -> u16 {
    url.trim_start_matches("http://")
        .trim_start_matches("https://")
        .split('/')
        .next()
        .and_then(|authority| authority.rsplit(':').next())
        .and_then(|port| port.parse::<u16>().ok())
        .unwrap_or(DEFAULT_ANALYZE_PORT)
}

/// The health endpoint for a base address, tolerating a trailing slash.
fn health_url(base: &str) -> String {
    format!("{}/health", base.trim_end_matches('/'))
}

/// The exact argument vector the guard hands `trusty-analyze`.
///
/// Why: this list IS the tga→trusty-analyze contract. Building it in a pure
/// function is what lets a test assert its contents without spawning anything —
/// the same reason [`super::review::report_args`] exists on the renderer side.
/// What: `serve --port <port>`.
/// Test: `super::tests::the_spawn_arguments_are_serve_on_the_configured_port`.
pub(super) fn serve_args(port: u16) -> Vec<String> {
    vec!["serve".to_string(), "--port".to_string(), port.to_string()]
}

/// Ensure the analyze daemon is up before the sweep starts.
///
/// Why/What: see the module docs. Resolves the guard from the environment and
/// delegates to [`ensure_analyze_daemon_with`], so the environment is read
/// exactly once, at the public entry point.
///
/// # Errors
///
/// [`AnalyzeDaemonUnavailable`] when the daemon is absent and cannot be started.
///
/// Test: `super::tests::a_reachable_analyze_daemon_is_not_restarted`.
pub async fn ensure_analyze_daemon() -> Result<(), AnalyzeDaemonUnavailable> {
    ensure_analyze_daemon_with(&AnalyzeGuard::from_env()).await
}

/// [`ensure_analyze_daemon`] with the address, binary and budgets already fixed.
///
/// Why: taking them as a value is what lets a test drive the whole spawn-and-poll
/// path against a stub executable on an ephemeral port, without touching the
/// process environment or leaving a daemon behind.
/// What: probes `<url>/health`; on a hit, returns without spawning anything. On a
/// miss, spawns `<binary> serve --port <port>` detached and polls until it
/// answers. Neither failure is downgraded — a spawn that fails and a daemon that
/// never answers both return `Err`.
///
/// A spawn that succeeds proves only that a process started. `trusty-analyze
/// serve` exits 1 when trusty-search is unreachable, so the readiness poll — not
/// the PID — is what decides the verdict.
///
/// # Errors
///
/// [`AnalyzeDaemonUnavailable`] carrying the spawn error, or the readiness
/// timeout.
///
/// Test: `super::tests::{a_reachable_analyze_daemon_is_not_restarted,
/// an_unspawnable_analyze_binary_refuses_the_audit,
/// an_analyze_daemon_that_never_comes_up_refuses_the_audit}`.
pub async fn ensure_analyze_daemon_with(
    guard: &AnalyzeGuard,
) -> Result<(), AnalyzeDaemonUnavailable> {
    let health = health_url(&guard.url);
    if probe_once(&health).await {
        return Ok(());
    }

    let refuse = |cause: String| AnalyzeDaemonUnavailable {
        url: guard.url.clone(),
        binary: guard.binary.clone(),
        cause,
    };

    eprintln!(
        "[tga audit] trusty-analyze is not answering at {}; starting `{} serve`…",
        guard.url, guard.binary
    );
    let args = serve_args(port_of(&guard.url));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    spawn_detached(&guard.binary, &args).map_err(|e| refuse(e.to_string()))?;

    let config = DaemonGuardConfig {
        health_url: health,
        service_name: "trusty-analyze".to_string(),
        startup_timeout: guard.startup_timeout,
        poll_interval: guard.poll_interval,
        timeout_hint: "it exits immediately when trusty-search is unreachable — start \
                       `trusty-search start` first"
            .to_string(),
    };
    spin_until_ready(&config)
        .await
        .map_err(|e| refuse(e.to_string()))
}
