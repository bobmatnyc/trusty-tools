//! Getting the two daemons the code analysis stands on to answer (#6081).
//!
//! Why: the chain is trusty-search → trusty-analyze → the report's analyze
//! sections, and it is a CHAIN — `trusty-analyze serve` exits immediately when
//! trusty-search is unreachable, and an analyze daemon that is already running
//! answers `503 degraded` for as long as it stays unreachable. So the search
//! daemon is ensured first and its failure is stated as itself rather than as
//! the analyze symptom it will later produce: an operator reading
//! "trusty-analyze is degraded" reaches for the wrong daemon.
//!
//! What: [`ensure_search`] and [`ensure_analyze`], plus the pure rules they
//! apply — the addresses, the ports, and the two argument vectors — so each can
//! be asserted without spawning anything.
//!
//! ## Nothing here is implemented twice
//!
//! Detaching a child is [`trusty_common::daemon_guard`]'s, which is where it
//! was promoted for exactly this kind of caller (#5670, #985). Dialling either
//! daemon is [`super::search_rpc`] for one and [`analyze_is_healthy`] for the
//! other. This module contributes the two socket paths, the two argument
//! vectors, the readiness rule, and the failure text — nothing else.
//!
//! #6285: both daemons speak framed JSON-RPC over a Unix socket now, so the
//! probe and the two-phase readiness wait are ONE implementation shared by
//! both — see [`wait_socket_ready`]. The TCP half that trusty-search needed
//! until this change (`probe_once`, an `accepting` connect, a base URL parsed
//! for its authority) is gone with the listener it probed.
//!
//! Test: `daemon_tests`, and `super::grounding_tests` for the live arms.

use std::path::{Path, PathBuf};
use std::time::Duration;

use std::time::Instant;

use trusty_common::daemon_guard::spawn_detached;

use super::Tools;
use super::search_rpc;

/// Environment variable naming the `trusty-search` binary.
///
/// The variable `tga audit` already reads (`tga::audit::repo_index`), and the one
/// `crate::run` exports onto every sweep child — so an engagement that pinned its
/// tools pins this leg the same way, with no second name to set.
pub const ENV_SEARCH_BIN: &str = "TRUSTY_SEARCH_BIN";

/// Environment variable naming the `trusty-analyze` binary.
pub const ENV_ANALYZE_BIN: &str = "TRUSTY_ANALYZE_BIN";

/// Environment variable overriding the trusty-analyze daemon's socket.
///
/// Why a literal rather than a shared constant: this crate has no Cargo edge to
/// `tga`, which owns the same names (`tga::audit::ENV_ANALYZE_SOCKET`), and
/// linking a workspace-version `tga` into a client whose entire discipline is
/// running PINNED binaries would defeat the pinning. It is a copied
/// cross-process contract — unlike the index id, which since #6149 is one
/// shared function in `trusty-common` rather than three copies — and it is
/// pinned by `daemon_tests::the_analyze_defaults_match_the_contract_tga_uses`.
///
/// #6287 renamed it from `TRUSTY_ANALYZE_URL`: the value it carries is a
/// filesystem path now, and a variable still saying URL would leave an operator
/// setting `http://…` and getting a dial failure naming a socket they never
/// configured.
pub const ENV_ANALYZE_SOCKET: &str = "TRUSTY_ANALYZE_SOCKET";

/// Environment variable overriding the trusty-search daemon's socket.
///
/// Re-exported from [`super::search_rpc`], which owns the whole trusty-search
/// dial, so a caller reading this module finds both daemons' overrides in one
/// place. See [`search_rpc::ENV_SEARCH_SOCKET`] for why the variable exists.
pub use super::search_rpc::ENV_SEARCH_SOCKET;

/// The method `trusty-analyze` answers a health probe on.
///
/// Copied for the same reason [`ENV_ANALYZE_SOCKET`] is.
/// `trusty_analyze::service::METHOD_HEALTH` is the definition;
/// `trusty-analyze/tests/uds_consumer_contract.rs` is what keeps them equal, by
/// driving [`ensure_analyze`] against a live router rather than a stub.
const ANALYZE_HEALTH_METHOD: &str = "analyze.health";

/// Wall-clock budget for a freshly-spawned daemon to report itself healthy.
///
/// Why 60s rather than `daemon_guard::DEFAULT_STARTUP_TIMEOUT`'s 30s: the socket
/// binds in about a second, but a first run on a recipient's machine with no
/// model cache spends 15–30s in ONNX load before trusty-search answers, and
/// giving up at 30s would turn a slow cold start into an unassessed engagement.
/// Matches `trusty-search`'s own guard and `tga::audit::SEARCH_STARTUP_TIMEOUT`.
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a freshly-spawned daemon has to BIND its socket before it is given
/// up on. See [`wait_socket_ready`] for why this is separate from
/// [`STARTUP_TIMEOUT`].
pub const BIND_TIMEOUT: Duration = Duration::from_secs(3);

/// How long a single socket connection attempt may take.
///
/// A reachable socket answers in microseconds and an absent one is refused
/// immediately. This bounds the third case — a daemon that accepts and then
/// stops reading — rather than the two that matter.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(200);

/// The trusty-search daemon's socket, as every client of it resolves one.
///
/// #6285: the discovery files this used to read described a TCP listener that
/// no longer exists. Both the daemon and this caller now DERIVE the path from
/// [`trusty_common::daemon_socket_path`], so there is nothing published between
/// them for a stale write to contradict. A thin forward to
/// [`search_rpc::search_socket`], kept named here because `Tools::pinned` and
/// the tests read better against a per-daemon resolver.
#[must_use]
pub fn search_socket() -> PathBuf {
    search_rpc::search_socket()
}

/// The trusty-analyze daemon's socket: [`ENV_ANALYZE_SOCKET`], else the derived
/// default.
///
/// #6287: the default is DERIVED through the same `daemon_socket_path` call the
/// daemon binds, which is what removes the hand-synced address constant this
/// used to fall back to. A resolution failure yields an empty path, which
/// [`ensure_analyze`] then reports as unreachable — the same outcome a wrong
/// path would produce, without the guess.
#[must_use]
pub fn analyze_socket() -> PathBuf {
    socket_from_override(std::env::var(ENV_ANALYZE_SOCKET).ok().as_deref())
}

/// The override rule itself: a non-empty value wins, everything else defaults.
///
/// Split out so the rule is asserted without any test reading or writing the
/// process environment — `set_var` is `unsafe` in edition 2024 and unsound under
/// the parallel harness.
fn socket_from_override(value: Option<&str>) -> PathBuf {
    match value.filter(|s| !s.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => trusty_common::daemon_socket_path("trusty-analyze").unwrap_or_default(),
    }
}

/// Is the trusty-analyze daemon at `socket` up AND is trusty-search reachable
/// from it?
///
/// Why: a degraded analyze daemon serves an empty hotspot list, which reads as
/// "nothing complex" rather than as an outage. Before #6287 the verdict came
/// free — the daemon answered HTTP 503 while degraded and `probe_once` counted
/// only a 2xx — and a JSON-RPC health call answers with a result frame either
/// way, so the check has to read `status` explicitly to keep it.
///
/// What: one `analyze.health` frame; `true` only for a result whose `status` is
/// `"ok"`.
async fn analyze_is_healthy(socket: &Path, timeout: Duration) -> bool {
    // #6555: a params-less frame decodes to null, which a struct-bound method rejects with -32602.
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": ANALYZE_HEALTH_METHOD,
        "params": {},
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

/// Is the trusty-search daemon at `socket` answering?
///
/// Why any result frame counts: `GET /health` on trusty-search answered 200
/// unconditionally — `health_handler` returns a bare `Json<HealthResponse>`
/// with no status code of its own — so `probe_once` treated a degraded daemon
/// as reachable. Reading `status` here would newly respawn a daemon that is up
/// and warning about one index, which is a behaviour change #6285 does not owe.
/// The analyze probe reads `status` because its HTTP route really did answer
/// 503 (see [`analyze_is_healthy`]).
///
/// What: one `search.health` frame; `true` only for a result frame. A dial
/// failure and an error frame are both `false`, so an RPC error can never
/// read as a healthy daemon.
/// Test: `super::grounding_tests::{a_search_daemon_that_will_not_start_is_a_named_gap,
/// a_reachable_search_daemon_is_not_restarted,
/// a_daemon_that_binds_but_never_answers_is_a_named_gap}`.
async fn search_is_healthy(socket: &Path, timeout: Duration) -> bool {
    search_rpc::call(
        socket,
        search_rpc::METHOD_HEALTH,
        serde_json::Value::Null,
        timeout,
    )
    .await
    .is_ok()
}

/// The argument vector `trusty-search` is started with.
///
/// `--foreground` is load-bearing rather than decorative: a bare
/// `trusty-search start` re-spawns itself as a background daemon and the parent
/// exits, so the PID [`spawn_detached`] returns would describe a process that is
/// already gone. We detach the child ourselves, so the flag is what keeps the
/// spawned process and the daemon the same one.
fn search_start_args() -> [&'static str; 2] {
    ["start", "--foreground"]
}

/// The argument vector `trusty-analyze` is started with.
///
/// #6287: a bare `serve`. The daemon derives its socket path from the data
/// directory, so passing one would start a daemon on a path
/// [`hotspots::fetch`] does not dial.
///
/// [`hotspots::fetch`]: super::hotspots::fetch
fn analyze_serve_args() -> [&'static str; 1] {
    ["serve"]
}

/// Ensure the trusty-search daemon answers, starting it if it does not.
///
/// # Errors
///
/// One line, safe to show the recipient, naming the socket and the binary, when
/// the binary will not spawn or the daemon never becomes ready. The caller turns
/// it into a gap; nothing here fails a run.
///
/// Test: `super::grounding_tests::{a_search_daemon_that_will_not_start_is_a_named_gap,
/// a_reachable_search_daemon_is_not_restarted,
/// a_daemon_that_binds_but_never_answers_is_a_named_gap}`.
pub async fn ensure_search(tools: &Tools) -> Result<(), String> {
    let socket = &tools.search_socket;
    let at = socket.display().to_string();
    if search_is_healthy(socket, CONNECT_TIMEOUT).await {
        return Ok(());
    }
    let binary = tools.search.display().to_string();
    spawn_detached(&tools.search, &search_start_args())
        .map_err(|e| refusal("trusty-search", &at, &binary, &e.to_string()))?;
    wait_socket_ready(socket, tools, || search_is_healthy(socket, CONNECT_TIMEOUT))
        .await
        .map_err(|cause| refusal("trusty-search", &at, &binary, &cause))
}

/// Ensure the trusty-analyze daemon answers, starting it if it does not.
///
/// # Errors
///
/// The same shape [`ensure_search`] returns. A daemon that is up but degraded
/// reports `status: "degraded"`, so it is reported here rather than surfacing
/// later as an empty hotspot list — see [`analyze_is_healthy`].
///
/// The readiness wait is [`wait_socket_ready`], shared with [`ensure_search`]
/// since #6285.
///
/// Test: `super::grounding_tests::an_analyze_daemon_that_will_not_start_is_a_named_gap`.
pub async fn ensure_analyze(tools: &Tools) -> Result<(), String> {
    let at = tools.analyze_socket.display().to_string();
    if analyze_is_healthy(&tools.analyze_socket, CONNECT_TIMEOUT).await {
        return Ok(());
    }
    let binary = tools.analyze.display().to_string();
    spawn_detached(&tools.analyze, &analyze_serve_args())
        .map_err(|e| refusal("trusty-analyze", &at, &binary, &e.to_string()))?;
    wait_socket_ready(&tools.analyze_socket, tools, || {
        analyze_is_healthy(&tools.analyze_socket, CONNECT_TIMEOUT)
    })
    .await
    .map_err(|cause| refusal("trusty-analyze", &at, &binary, &cause))
}

/// Wait for a daemon this call just spawned, in two phases.
///
/// Why not `trusty_common::daemon_guard::spin_until_ready`: it applies ONE
/// budget to both of the things that can be slow, and they are slow for
/// unrelated reasons. Binding the socket takes about a second whatever the
/// machine; answering a health call afterwards can take 15-30s on a first run
/// that has to load ONNX weights. Under one budget a binary that dies
/// immediately — which is exactly what `trusty-analyze serve` does when
/// trusty-search is unreachable — costs the FULL startup budget before anything
/// is reported. In an unattended engagement that is 60 seconds per repository
/// spent waiting for a process that is already gone.
///
/// #6285: one loop for both daemons. Until this change trusty-search took the
/// TCP form of it (a `connect` for phase one, `probe_once` for phase two) and
/// trusty-analyze the UDS form, and the two drifted apart by construction.
/// Both dial a socket now, so the phases are a single implementation and
/// `healthy` is the only thing that varies.
///
/// What: the socket must accept a connection within [`Tools::bind_timeout`],
/// and only then does `healthy` get the full [`Tools::startup_timeout`].
/// `spin_until_ready`'s spinner is dropped with it, which this crate wants
/// anyway — it writes to the same stderr the sweep's progress relay is reading.
///
/// Test: `super::grounding_tests::{a_search_daemon_that_will_not_start_is_a_named_gap,
/// a_daemon_that_binds_but_never_answers_is_a_named_gap,
/// an_analyze_daemon_that_will_not_start_is_a_named_gap}`.
async fn wait_socket_ready<F, Fut>(socket: &Path, tools: &Tools, healthy: F) -> Result<(), String>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let bound_by = Instant::now() + tools.bind_timeout;
    loop {
        if healthy().await {
            return Ok(());
        }
        if trusty_common::uds::socket_is_serving(socket, CONNECT_TIMEOUT).await {
            break;
        }
        if Instant::now() >= bound_by {
            return Err(format!(
                "nothing was serving it {}s after it was started",
                tools.bind_timeout.as_secs_f32()
            ));
        }
        tokio::time::sleep(tools.poll_interval).await;
    }
    let ready_by = Instant::now() + tools.startup_timeout;
    loop {
        tokio::time::sleep(tools.poll_interval).await;
        if healthy().await {
            return Ok(());
        }
        if Instant::now() >= ready_by {
            return Err(format!(
                "it is serving but did not report healthy within {}s",
                tools.startup_timeout.as_secs_f32()
            ));
        }
    }
}

/// One unavailable daemon, rendered as one line the recipient can act on.
///
/// Why the cause is quoted rather than replaced: the remedies differ — a binary
/// that is not installed needs an install, a daemon that will not stay up needs
/// its own log — and only the spawn or the readiness poll knows which fired.
fn refusal(service: &str, at: &str, binary: &str, cause: &str) -> String {
    format!("{service} is not reachable at {at} and `{binary}` could not start it ({cause})")
}

#[cfg(test)]
mod daemon_tests {
    use super::*;

    /// The values this crate copies rather than imports, pinned against the
    /// contract `tga::audit::analyze` spells on the other side of the wire.
    ///
    /// #6287: the address and port constants are gone with the transport. What
    /// is left to pin is the variable name and the method name — and the socket
    /// DEFAULT no longer needs pinning at all, because both sides derive it from
    /// `trusty_common::daemon_socket_path` rather than each spelling a literal.
    #[test]
    fn the_analyze_defaults_match_the_contract_tga_uses() {
        assert_eq!(ENV_ANALYZE_SOCKET, "TRUSTY_ANALYZE_SOCKET");
        assert_eq!(ANALYZE_HEALTH_METHOD, "analyze.health");
        assert_eq!(
            socket_from_override(None),
            trusty_common::daemon_socket_path("trusty-analyze").unwrap_or_default(),
            "the fallback must be the same path the daemon binds"
        );
    }

    #[test]
    fn an_absent_or_empty_override_falls_back_to_the_default_socket() {
        let derived = trusty_common::daemon_socket_path("trusty-analyze").unwrap_or_default();
        assert_eq!(socket_from_override(None), derived);
        assert_eq!(socket_from_override(Some("")), derived);
        assert_eq!(
            socket_from_override(Some("/tmp/pinned-analyze.sock")),
            PathBuf::from("/tmp/pinned-analyze.sock")
        );
    }

    /// #6285: the search leg's half of the same contract. The daemon derives
    /// its socket from `daemon_socket_path("trusty-search")`
    /// (`trusty_search::service::socket::socket_path`), so a resolver that
    /// disagreed would dial a path nothing binds.
    #[test]
    fn the_search_socket_is_the_path_the_daemon_binds() {
        assert_eq!(search_rpc::ENV_SEARCH_SOCKET, "TRUSTY_SEARCH_SOCKET");
        assert_eq!(
            search_socket(),
            trusty_common::daemon_socket_path("trusty-search").unwrap_or_default(),
            "the default must be the same path the daemon binds"
        );
    }

    /// `--foreground` is the flag that keeps the spawned process and the daemon
    /// the same one; a later edit dropping it would otherwise pass unnoticed.
    #[test]
    fn the_search_start_stays_in_the_foreground() {
        assert_eq!(search_start_args(), ["start", "--foreground"]);
    }

    /// #6287: a bare `serve`. The daemon derives its socket from the data
    /// directory, so passing one would start a daemon on a path
    /// `hotspots::fetch` does not dial.
    #[test]
    fn the_analyze_spawn_is_a_bare_serve() {
        assert_eq!(analyze_serve_args(), ["serve"]);
    }

    #[test]
    fn a_refusal_names_the_service_the_address_the_binary_and_the_cause() {
        let line = refusal(
            "trusty-analyze",
            "/w/sockets/trusty-analyze.sock",
            "/w/tools/trusty-analyze",
            "no such file",
        );
        assert!(line.contains("trusty-analyze"), "{line}");
        assert!(line.contains("/w/sockets/trusty-analyze.sock"), "{line}");
        assert!(line.contains("/w/tools/trusty-analyze"), "{line}");
        assert!(line.contains("no such file"), "{line}");
        assert_eq!(line.lines().count(), 1, "must stay one line: {line}");
    }
}
