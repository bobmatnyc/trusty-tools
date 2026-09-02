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
//! What: [`ensure_analyze_daemon`], the binary/socket resolution rules it
//! applies, and [`AnalyzeDaemonUnavailable`].
//!
//! #6287 (ADR-0032) moved the daemon onto a Unix socket, which changes the
//! probe rather than the decision. `trusty_common::daemon_guard`'s
//! `spin_until_ready` is built around a health URL, so the poll loop is local
//! here now; `spawn_detached` from that module still starts the process. The
//! readiness verdict itself got STRICTER in one respect and stayed identical in
//! another: it still refuses a degraded daemon (see [`daemon_is_healthy`]), and
//! it now reads that verdict off a typed field rather than off an HTTP status
//! code the daemon no longer emits.
//!
//! Nothing here is fail-open. Both failure arms — the binary would not spawn,
//! and the daemon never answered — return `Err`, because a report missing those
//! sections reads as a clean pass rather than as an outage.
//! Test: `super::tests` against stubs; `super::real_binary_tests` (`#[ignore]`d)
//! against the real `trusty-analyze` binary.
//!
//! # Spec References
//! - [`SPEC-TGAUDIT-06~draft`](docs/specs/DOC-67-tga-audit-mode.md#SPEC-TGAUDIT-06~draft)
//! - [`SPEC-TGAUDIT-09~draft`](docs/specs/DOC-67-tga-audit-mode.md#SPEC-TGAUDIT-09~draft)

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use trusty_common::daemon_guard::{spawn_detached, DEFAULT_POLL_INTERVAL, DEFAULT_STARTUP_TIMEOUT};

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

/// Environment variable naming the analyze daemon's socket.
///
/// Why: `trusty-review` resolves the same variable for the socket it dials, so
/// the daemon this starts and the daemon the renderer queries are the same
/// process by construction — two spellings would let them disagree silently.
///
/// #6287 renamed it from `PR_INTELLIGENCE_ANALYZER_URL`. The value it carries
/// changed from a URL to a filesystem path, and a variable that still said URL
/// while holding a path would leave an operator setting `http://…` and getting
/// a dial failure that names a socket they never configured. Both crates read
/// the new name in the same change, so there is no window where they disagree.
pub const ENV_ANALYZE_SOCKET: &str = "PR_INTELLIGENCE_ANALYZER_SOCKET";

/// Default analyze daemon socket, as the daemon itself resolves it.
///
/// Why: the daemon binds `trusty_common::daemon_socket_path("trusty-analyze")`
/// and every consumer dials the same call, so there is nothing to keep in sync
/// by hand — which is what the retired `DEFAULT_ANALYZE_URL` constant needed,
/// against `trusty_review::config::DEFAULT_ANALYZER_URL`, and what a derived
/// path removes.
///
/// # Errors
///
/// When the data directory cannot be resolved or created.
pub fn default_analyze_socket() -> anyhow::Result<PathBuf> {
    trusty_common::daemon_socket_path("trusty-analyze")
}

/// The audit cannot proceed without the analyze daemon.
///
/// Why: this is refused before the sweep rather than reported after it, so the
/// message is the operator's whole remedy — and the remedy is almost never "run
/// the binary again". `trusty-analyze serve` exits immediately when
/// `trusty-search` is unreachable, which is the usual reason a spawn produces no
/// daemon, so the message names that ordering rather than the symptom.
/// What: the socket probed, the binary tried, and the underlying cause.
/// Test: `super::tests::an_analyze_daemon_that_never_comes_up_refuses_the_audit`.
#[derive(Debug, thiserror::Error)]
#[error(
    "trusty-analyze is not serving {socket} and could not be started ({cause}). `tga audit` \
     renders its report with `trusty-review report --analyze`, and DOC-67 §8 sources the findings \
     table, the complexity distribution and the health factors from that daemon and nowhere else — \
     without it those three sections render empty, which reads as a clean bill of health rather \
     than as an outage. Start the stack first:\n\n    trusty-search start\n    {binary} serve\n\n\
     `trusty-analyze serve` exits immediately when trusty-search is unreachable, so trusty-search \
     goes first. Override the binary with {ENV_ANALYZE_BIN} and the socket with \
     {ENV_ANALYZE_SOCKET}."
)]
#[non_exhaustive]
pub struct AnalyzeDaemonUnavailable {
    /// The socket that was probed, rendered as a path.
    pub socket: String,
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
/// executable on a temp-dir socket — the same split
/// [`super::review::binary_from_override`] uses for its rule.
/// What: the daemon socket, the binary to spawn, and the readiness budget.
/// Test: `super::tests::a_reachable_analyze_daemon_is_not_restarted`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AnalyzeGuard {
    /// The daemon's Unix socket, e.g. `<data dir>/trusty-analyze.sock`.
    pub socket: PathBuf,
    /// Binary name or path to spawn when the daemon is absent.
    pub binary: String,
    /// Wall-clock budget for a freshly-spawned daemon to answer `analyze.health`.
    pub startup_timeout: Duration,
    /// Interval between readiness probes.
    pub poll_interval: Duration,
}

impl AnalyzeGuard {
    /// The guard this process runs with, resolved from the environment.
    ///
    /// Why/What: reads [`ENV_ANALYZE_SOCKET`] and [`ENV_ANALYZE_BIN`], applying
    /// [`socket_from_override`] and [`binary_from_override`]. Reading the two
    /// variables is all this does; the rules live in those pure functions so the
    /// tests never call `std::env::set_var`, which is `unsafe` in edition 2024
    /// and unsound under parallel tests (#5308 review).
    ///
    /// # Errors
    ///
    /// When no override is set and the default socket path cannot be resolved.
    /// The pre-#6287 version was infallible because its default was a string
    /// literal; a derived path can fail, and guessing one would send the audit
    /// at a socket the daemon never binds.
    ///
    /// Test: `super::tests::analyze_resolution_prefers_the_env_overrides`.
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            socket: socket_from_override(std::env::var(ENV_ANALYZE_SOCKET).ok().as_deref())?,
            binary: binary_from_override(std::env::var(ENV_ANALYZE_BIN).ok().as_deref()),
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            poll_interval: DEFAULT_POLL_INTERVAL,
        })
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

/// The socket resolution rule: an override wins unless it is empty.
///
/// Why/What: see [`AnalyzeGuard::from_env`]. `None` and `Some("")` both fall
/// back to [`default_analyze_socket`].
///
/// # Errors
///
/// Only on the fallback path, when the data directory cannot be resolved.
///
/// Test: `super::tests::analyze_resolution_prefers_the_env_overrides`.
pub(super) fn socket_from_override(override_value: Option<&str>) -> anyhow::Result<PathBuf> {
    match override_value.filter(|s| !s.is_empty()) {
        Some(p) => Ok(PathBuf::from(p)),
        None => default_analyze_socket(),
    }
}

/// Is the daemon at `socket` up AND is its own dependency reachable?
///
/// Why: a daemon that is up but degraded — trusty-search unreachable — is not a
/// pass. DOC-67 §8 sources three sections from this daemon's analysis, all of
/// which need the search corpus, so a degraded daemon produces the same empty
/// sections a missing daemon does. Before #6287 that verdict came free: the
/// daemon answered HTTP 503 while degraded and `probe_once` counted only a 2xx.
/// A JSON-RPC health call answers with a RESULT frame either way, so the check
/// has to read `status` — and doing so is what keeps the audit's trusty-search
/// dependency hard on every run rather than only on a fresh spawn.
///
/// What: one `analyze.health` frame; `true` only for a result whose `status` is
/// `"ok"`. Every failure — dial, error frame, missing field, anything else — is
/// `false`, because none of them is evidence the daemon can serve the report.
///
/// Test: `super::tests::a_degraded_analyze_daemon_refuses_the_audit`.
async fn daemon_is_healthy(socket: &Path, timeout: Duration) -> bool {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": ANALYZE_HEALTH_METHOD,
    });
    let Ok(response) = trusty_common::uds::send_framed_request::<
        _,
        trusty_common::uds::server::RpcResponse,
    >(socket, &request, timeout)
    .await
    else {
        return false;
    };
    response
        .result
        .as_ref()
        .and_then(|r: &serde_json::Value| r.get("status"))
        .and_then(serde_json::Value::as_str)
        == Some("ok")
}

/// The method `trusty-analyze` answers a health probe on.
///
/// Duplicated as a literal rather than imported: `tga` has no Cargo edge on
/// `trusty-analyze`. `trusty_analyze::service::METHOD_HEALTH` is the definition;
/// `trusty-analyze/tests/uds_consumer_contract.rs` is what keeps them equal.
const ANALYZE_HEALTH_METHOD: &str = "analyze.health";

/// How long one health dial may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// The exact argument vector the guard hands `trusty-analyze`.
///
/// Why: this list IS the tga→trusty-analyze contract. Building it in a pure
/// function is what lets a test assert its contents without spawning anything —
/// the same reason [`super::review::report_args`] exists on the renderer side.
/// What: `serve`, with no socket argument. #6287: the daemon derives its socket
/// path from the data directory, and passing `--socket` would start a daemon on
/// a path the renderer does not dial. An operator who moved the socket with
/// [`ENV_ANALYZE_SOCKET`] is pointing at a daemon they started themselves; this
/// guard's spawn only ever produces one at the default path.
/// Test: `super::tests::the_spawn_arguments_are_a_bare_serve`.
pub(super) fn serve_args() -> Vec<String> {
    vec!["serve".to_string()]
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
    // #6287: resolving the socket can fail where reading a string literal could
    // not. It reports as the same refusal — the audit cannot proceed without the
    // daemon, and it cannot reach the daemon without a path.
    let guard = AnalyzeGuard::from_env().map_err(|e| AnalyzeDaemonUnavailable {
        socket: "<unresolved>".to_string(),
        binary: DEFAULT_ANALYZE_BIN.to_string(),
        cause: format!("could not resolve the trusty-analyze socket path: {e:#}"),
    })?;
    ensure_analyze_daemon_with(&guard).await
}

/// [`ensure_analyze_daemon`] with the address, binary and budgets already fixed.
///
/// Why: taking them as a value is what lets a test drive the whole spawn-and-poll
/// path against a stub executable on an ephemeral port, without touching the
/// process environment or leaving a daemon behind.
/// What: calls `analyze.health` on the socket; on a healthy answer, returns
/// without spawning anything. On a miss, spawns `<binary> serve` detached and
/// polls until it answers healthy. Neither failure is downgraded — a spawn that
/// fails and a daemon that never answers both return `Err`.
///
/// A spawn that succeeds proves only that a process started. `trusty-analyze
/// serve` exits 1 when trusty-search is unreachable, so the readiness poll — not
/// the PID — is what decides the verdict.
///
/// A daemon that is up but degraded — which is what `trusty-analyze` reports
/// while trusty-search is unreachable — is not a pass either; see
/// [`daemon_is_healthy`] for why that check has to be explicit since #6287.
/// That is what makes the audit's trusty-search dependency hard on every run
/// rather than only on a fresh spawn.
///
/// Concurrent calls are safe and independent: the guard is read-only shared
/// input, and nothing is cached between calls, so each reaches its own verdict
/// from its own probe. There is no cross-call spawn deduplication — see
/// `super::tests::two_concurrent_guards_both_resolve_against_one_slow_daemon`
/// for why none is owed.
///
/// # Errors
///
/// [`AnalyzeDaemonUnavailable`] carrying the spawn error, or the readiness
/// timeout.
///
/// Test: `super::tests::{a_reachable_analyze_daemon_is_not_restarted,
/// an_unspawnable_analyze_binary_refuses_the_audit,
/// an_analyze_daemon_that_never_comes_up_refuses_the_audit,
/// a_degraded_analyze_daemon_refuses_the_audit,
/// two_concurrent_guards_both_resolve_against_one_slow_daemon}`, and
/// `super::real_binary_tests` end to end.
pub async fn ensure_analyze_daemon_with(
    guard: &AnalyzeGuard,
) -> Result<(), AnalyzeDaemonUnavailable> {
    if daemon_is_healthy(&guard.socket, PROBE_TIMEOUT).await {
        return Ok(());
    }

    let refuse = |cause: String| AnalyzeDaemonUnavailable {
        socket: guard.socket.display().to_string(),
        binary: guard.binary.clone(),
        cause,
    };

    eprintln!(
        "[tga audit] trusty-analyze is not answering on {}; starting `{} serve`…",
        guard.socket.display(),
        guard.binary
    );
    let args = serve_args();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    spawn_detached(&guard.binary, &args).map_err(|e| refuse(e.to_string()))?;

    // #6287: a local poll loop rather than `spin_until_ready`, which takes a
    // health URL this daemon no longer has. The two budgets and the verdict are
    // unchanged; only the probe moved.
    let deadline = Instant::now() + guard.startup_timeout;
    while Instant::now() < deadline {
        tokio::time::sleep(guard.poll_interval).await;
        if daemon_is_healthy(&guard.socket, PROBE_TIMEOUT).await {
            return Ok(());
        }
    }
    Err(refuse(format!(
        "it did not answer healthy within {}s — it exits immediately when trusty-search is \
         unreachable, so start `trusty-search start` first",
        guard.startup_timeout.as_secs()
    )))
}
