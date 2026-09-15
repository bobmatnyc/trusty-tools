//! The sidecar health probes `tm doctor` runs: trusty-memory and trusty-search.
//!
//! Why (#7685): `doctor.rs` is the check REGISTRY — it decides which rows a run
//! produces and in what order — and it sat exactly at the 500-SLOC production
//! cap, so the next check to be added could not land. These probes are the
//! largest coherent block that is not registry logic: they talk to two daemons
//! over JSON-RPC and turn what they observed into a verdict, and nothing else in
//! `doctor.rs` calls them except the two registration lines. Moving them is a
//! pure relocation — no behaviour, no assertion, and no verdict string changes.
//! What: [`check_memory`] and [`check_search`], the retry/classify machinery
//! behind them ([`probe_health`], [`probe_health_once`], [`ProbeOutcome`],
//! [`interpret_health`]) and the two search-index helpers, plus the three
//! `PROBE_*` budget constants they share.
//! Test: `memory_unreachable_is_fail`, `memory_timeout_is_unknown_not_fail`,
//! `memory_wedged_worker_pool_is_not_ok`, `memory_warming_is_warn_not_fail`,
//! `memory_slow_but_serving_daemon_is_ok`, `health_body_without_worker_block_is_unknown`,
//! `search_unreachable_is_fail`, `search_reports_the_expected_index`,
//! `search_without_the_expected_index_is_warn`,
//! `expected_search_index_id_derives_from_project_dir_not_hardcoded`,
//! `index_present_matches_each_shape` — all in `doctor_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::doctor::{CheckStatus, DoctorCheck};

use super::super::search_rpc;

/// Per-probe network timeout.
///
/// Why: a sidecar that is down or wedged must not stall the whole diagnostic;
/// a bounded probe turns "hung" into a clean verdict.
///
/// Raised from 2 s to 10 s for issue #4005. The old bound produced
/// "trusty-memory unreachable at 127.0.0.1:7070" against a daemon whose MCP
/// surface was verifiably serving in the same minutes: trusty-memory's
/// `/health` samples process RSS/CPU behind a mutex and enumerates open file
/// descriptors, none of which the MCP request path touches, so under load
/// `/health` can exceed a budget that real traffic never approaches. The probe
/// was measuring its own impatience. Note that the timeout is only half the
/// fix — a timeout now resolves to [`CheckStatus::Unknown`] rather than a
/// false `Fail` (see [`probe_health`]).
pub(crate) const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Number of attempts a health probe makes before giving up.
///
/// Why (issue #4005): the probe was single-shot, so one unlucky sample — a GC
/// pause, a burst of concurrent MCP traffic, the warm-up window right after a
/// `tm` restart that the issue explicitly calls out — became a hard failure
/// verdict with no second opinion. Two retries cost nothing on the healthy
/// path (the first attempt succeeds and returns immediately) and remove the
/// single-sample fragility on the unhealthy one.
const PROBE_ATTEMPTS: usize = 3;

/// Delay between health-probe attempts.
///
/// Why: long enough to let a transient spike pass, short enough that three
/// attempts stay well inside an interactive `tm doctor` run.
const PROBE_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

/// Probe trusty-memory's health (#6286).
///
/// Why: memory recall and store route through trusty-memory; if it is down the
/// PM silently loses its long-term memory, so the operator must know.
/// What: derives the daemon's socket — there is no address to discover and no
/// `~/.trusty-memory/http_addr` to read since ADR-0032 — and hands it to
/// [`probe_health`], which retries, distinguishes a refusal from a timeout, and
/// reads the daemon's own worker-pool observation out of the answer.
///
/// `home` is unused now and stays in the signature because `run_checks` threads
/// one `home` into every check; the search half still needs it.
/// Test: `memory_unreachable_is_fail`, `memory_timeout_is_unknown_not_fail`,
/// `memory_wedged_worker_pool_is_not_ok`, `memory_warming_is_warn_not_fail`.
pub(super) async fn check_memory(_home: &Path) -> DoctorCheck {
    let socket = trusty_common::memory_rpc::resolve_memory_socket_or_unreachable();
    let addr = socket.display().to_string();
    probe_health("memory", "trusty-memory", &socket, &addr).await
}

/// Outcome of one `/health` request attempt.
///
/// Why (issue #4005): the old code collapsed every non-success into a single
/// `Err`, which is what made "timed out" and "connection refused" produce the
/// same "unreachable" verdict despite meaning opposite things operationally.
/// Naming the cases is what lets the caller be honest about which it saw.
/// What: a 2xx with its parsed body, a non-2xx, a timeout, or a refusal.
enum ProbeOutcome {
    /// 2xx. The body is `None` when it could not be read or parsed as JSON.
    Success(Option<serde_json::Value>),
    /// The service answered, but not with a 2xx.
    NonSuccess(u16),
    /// No answer within [`PROBE_TIMEOUT`] — we learned nothing.
    TimedOut,
    /// Connection refused / DNS / other transport failure — nothing is there.
    Unreachable(String),
}

/// Issue one `memory.health` call and classify the outcome (#6286).
///
/// Why the three failure arms are told apart here rather than collapsed: they
/// mean opposite things operationally, and #4005 is the incident that proves
/// it. A JSON-RPC error is the daemon ANSWERING and refusing — it is up. A dial
/// that fails inside the budget is a refusal — nothing is there. A failure that
/// consumed the budget is a timeout — we learned nothing, and must not claim
/// the daemon is down.
///
/// What: `NonSuccess` carries the daemon's own error code (as an absolute
/// value, since the caller renders it like a status); `Unreachable` carries the
/// transport error; `TimedOut` is the budget overrun.
async fn probe_health_once(socket: &Path) -> ProbeOutcome {
    let started = std::time::Instant::now();
    match trusty_common::memory_rpc::call_memory_tool_at_with_timeout(
        socket,
        "memory.health",
        serde_json::json!({}),
        PROBE_TIMEOUT,
    )
    .await
    {
        Ok(body) => ProbeOutcome::Success(Some(body)),
        Err(e) => {
            if let Some(rpc) = e.downcast_ref::<trusty_common::memory_rpc::MemoryRpcError>() {
                return ProbeOutcome::NonSuccess(rpc.code.unsigned_abs() as u16);
            }
            if started.elapsed() >= PROBE_TIMEOUT {
                return ProbeOutcome::TimedOut;
            }
            ProbeOutcome::Unreachable(format!("{e:#}"))
        }
    }
}

/// Probe a sidecar's `/health` and turn what was actually observed into a
/// [`DoctorCheck`] (issues #4005, #4001).
///
/// Why: this function encodes the principle both issues share. Doctor used to
/// infer health from a cheap proxy — "the socket answered" — instead of
/// observing the thing it claims to report. That produced a false NEGATIVE
/// when the proxy was merely slow (#4005) and a false POSITIVE when the
/// listener was fine but every worker was parked (#4001). So: retry before
/// concluding anything, separate "nothing is listening" from "nothing
/// answered in time", and prefer the daemon's own observation of its workers
/// over our inference from the status code.
/// What: up to [`PROBE_ATTEMPTS`] attempts. Returns `Ok` only when the daemon
/// positively reports a healthy worker pool; `Fail` on a refusal, a non-2xx,
/// or a reported wedge; `Warn` while warming or degraded; and `Unknown` when
/// every attempt timed out or the body carried no worker observation.
/// Test: `memory_unreachable_is_fail`, `memory_timeout_is_unknown_not_fail`,
/// `memory_wedged_worker_pool_is_not_ok`, `memory_warming_is_warn_not_fail`,
/// `memory_slow_but_serving_daemon_is_ok`.
pub(super) async fn probe_health(
    check: &str,
    service: &str,
    socket: &Path,
    addr: &str,
) -> DoctorCheck {
    let mut last_timeout = false;
    let mut last_err: Option<String> = None;
    let mut last_status: Option<u16> = None;

    for attempt in 0..PROBE_ATTEMPTS {
        match probe_health_once(socket).await {
            ProbeOutcome::Success(body) => {
                return interpret_health(check, service, addr, body.as_ref());
            }
            ProbeOutcome::NonSuccess(code) => {
                last_status = Some(code);
                last_timeout = false;
            }
            ProbeOutcome::TimedOut => {
                last_timeout = true;
            }
            ProbeOutcome::Unreachable(e) => {
                last_err = Some(e);
                last_timeout = false;
            }
        }
        if attempt + 1 < PROBE_ATTEMPTS {
            tokio::time::sleep(PROBE_RETRY_DELAY).await;
        }
    }

    if last_timeout {
        // Issue #4005: THE false negative. Do not claim the daemon is down —
        // we never established that. A slow /health is exactly what a healthy
        // daemon under load looks like from here.
        return DoctorCheck::new(
            check,
            CheckStatus::Unknown,
            format!(
                "{service} at {addr} did not answer its health probe within {}s across \
                 {PROBE_ATTEMPTS} \
                 attempts, but the connection was NOT refused — the daemon may be alive and \
                 merely slow. Health could not be determined. Check the MCP surface before \
                 restarting anything.",
                PROBE_TIMEOUT.as_secs()
            ),
        );
    }

    if let Some(code) = last_status {
        return DoctorCheck::new(
            check,
            CheckStatus::Fail,
            format!("{service} at {addr} refused the health probe with code {code}"),
        );
    }

    DoctorCheck::new(
        check,
        CheckStatus::Fail,
        format!(
            "{service} unreachable at {addr}: {}",
            last_err.unwrap_or_else(|| "connection refused".to_string())
        ),
    )
}

/// Map a 2xx `/health` body to a status (issues #4001, #4005).
///
/// Why: a 2xx proves a listener accepted a socket, not that the daemon is
/// doing work — which is precisely how #3992 kept `tm doctor` green for the
/// duration of an incident in which six threads were parked and a
/// `memory_remember` had been hung for ~1800 s. When the daemon reports its
/// own worker occupancy, that observation wins over our inference.
/// What: `Fail` on a reported wedge, `Warn` while warming or degraded,
/// `Unknown` when no worker block is present (an older daemon, or an
/// unreadable body — we cannot claim health we did not observe) and equally
/// when the daemon's palace-lock stall detector is stopped or absent, `Ok`
/// otherwise.
/// Test: `memory_wedged_worker_pool_is_not_ok`, `memory_warming_is_warn_not_fail`,
/// `health_body_without_worker_block_is_unknown`,
/// `memory_without_stall_tracking_is_unknown_not_warn`.
fn interpret_health(
    check: &str,
    service: &str,
    addr: &str,
    body: Option<&serde_json::Value>,
) -> DoctorCheck {
    let Some(body) = body else {
        return DoctorCheck::new(
            check,
            CheckStatus::Unknown,
            format!(
                "{service} at {addr} answered /health but the body was unreadable — the \
                 listener is up; whether its workers are progressing is UNKNOWN"
            ),
        );
    };

    let worker = body.get("worker");
    let wedged = worker
        .and_then(|w| w.get("wedged"))
        .and_then(serde_json::Value::as_bool);

    match wedged {
        Some(true) => {
            let oldest = worker
                .and_then(|w| w.get("oldest_age_secs"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default();
            let in_flight = worker
                .and_then(|w| w.get("in_flight"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default();
            return DoctorCheck::new(
                check,
                CheckStatus::Fail,
                format!(
                    "{service} at {addr} is answering /health BUT reports a WEDGED worker \
                     pool: oldest in-flight operation {oldest}s, {in_flight} in flight. The \
                     listener responding does not mean writes are progressing (issue #3992)."
                ),
            );
        }
        None => {
            return DoctorCheck::new(
                check,
                CheckStatus::Unknown,
                format!(
                    "{service} at {addr} is reachable but does not report worker-pool \
                     occupancy (pre-#4001 build) — liveness confirmed, progress UNKNOWN"
                ),
            );
        }
        Some(false) => {}
    }

    // #4001: `wedged: false` is worth exactly what the detector behind it is
    // worth. A daemon whose palace-lock stall tracking stopped reports
    // `degraded` below, which reads as a warning and leaves the run green on
    // the detector's own failure; a daemon with no detector at all omits the
    // field (a plain bool, never skipped) while still reporting `wedged`
    // (#3992), so it never reaches the `None` arm above. Neither is `Ok`.
    if worker
        .and_then(|w| w.get("stall_tracking_ok"))
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        let detail = body
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap_or("this daemon predates the palace-lock stall detector (#4001)");
        return DoctorCheck::new(
            check,
            CheckStatus::Unknown,
            format!(
                "{service} at {addr} is reachable but its palace-lock stall detector is not \
                 reporting: {detail} — whether a palace lock is wedged is UNKNOWN"
            ),
        );
    }

    // Issue #4005 explicitly calls out post-restart warm-up: it is a normal
    // transient state, not a failure, and must not read as fully healthy either.
    if body.get("daemon_state").and_then(|v| v.as_str()) == Some("warming") {
        return DoctorCheck::new(
            check,
            CheckStatus::Warn,
            format!(
                "{service} at {addr} is WARMING UP (embedder initialising) — normal shortly after a restart"
            ),
        );
    }

    // #7685: read the status through the one shared client-side parser.
    if trusty_common::memory_rpc::MemoryHealthStatus::from_health_body(body)
        == trusty_common::memory_rpc::MemoryHealthStatus::Degraded
    {
        let detail = body
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap_or("no detail reported");
        return DoctorCheck::new(
            check,
            CheckStatus::Warn,
            format!("{service} at {addr} reports DEGRADED: {detail}"),
        );
    }

    DoctorCheck::new(
        check,
        CheckStatus::Ok,
        format!("{service} healthy at {addr}, workers progressing"),
    )
}

/// Probe the trusty-search sidecar's health and this project's index.
///
/// Why: code search backs the PM's "search before grep" rule; both the service
/// being up *and* this project's index existing are required for it to work.
/// What (#6285): derives the daemon's socket — there is no address to discover
/// and no `~/.trusty-search/http_addr` to read since ADR-0032 — calls
/// `search.health` (a refusal or a transport failure is `Fail`), then calls
/// `search.indexes.list` for the index id [`expected_search_index_id`] resolves
/// for `project_dir`. A healthy service missing that index is `Warn`.
///
/// `home` is unused now and stays in the signature because `run_checks` threads
/// one `home` into every check.
/// Test: `search_unreachable_is_fail`, `search_reports_the_expected_index`,
/// `search_without_the_expected_index_is_warn`.
pub(super) async fn check_search(_home: &Path, project_dir: Option<&Path>) -> DoctorCheck {
    let socket = match search_rpc::search_socket() {
        Ok(socket) => socket,
        Err(e) => {
            return DoctorCheck::new(
                "search",
                CheckStatus::Fail,
                format!("cannot resolve the trusty-search socket: {e:#}"),
            );
        }
    };
    let at = socket.display().to_string();

    if let Err(e) = search_rpc::call_at(
        &socket,
        search_rpc::METHOD_HEALTH,
        serde_json::json!({}),
        PROBE_TIMEOUT,
    )
    .await
    {
        return DoctorCheck::new(
            "search",
            CheckStatus::Fail,
            format!("trusty-search unreachable at {at}: {e:#}"),
        );
    }

    // Service is up — confirm the expected index exists. #4003: the expected
    // id is DERIVED from the project (same rule `session_launch` and
    // `trusty-search`'s own `detect_project` use), not a hardcoded literal —
    // see `expected_search_index_id`.
    let expected_index = expected_search_index_id(project_dir);
    match search_rpc::call_at(
        &socket,
        search_rpc::METHOD_INDEXES_LIST,
        serde_json::json!({}),
        PROBE_TIMEOUT,
    )
    .await
    {
        Ok(body) if index_present(&body, &expected_index) => DoctorCheck::new(
            "search",
            CheckStatus::Ok,
            format!("trusty-search healthy at {at}, `{expected_index}` index present"),
        ),
        Ok(_) => DoctorCheck::new(
            "search",
            CheckStatus::Warn,
            format!("trusty-search healthy at {at} but the `{expected_index}` index is missing"),
        ),
        Err(e) => DoctorCheck::new(
            "search",
            CheckStatus::Warn,
            format!("trusty-search healthy at {at} but listing indexes failed: {e:#}"),
        ),
    }
}

/// Resolve the trusty-search index id `tm doctor` should expect for
/// `project_dir` (#4003).
///
/// Why: the probe previously hardcoded a literal expected index name
/// (`"trusty-mpm"` — the crate name), which diverges from a repo's actual
/// registered index id (e.g. this repo registers as `"trusty-tools"`), so a
/// healthy, fully-indexed project permanently reported "index missing".
/// What: walks up from `project_dir` (falling back to the process cwd when
/// `None`, matching the daemon's own `run_doctor` default) to the nearest
/// git root via [`trusty_common::resolve_project_root`], then derives the id
/// via [`trusty_common::derive_index_id`] — the exact same rule
/// `core::session_launch` uses to register-and-pin a session's index and
/// trusty-search's own `detect_project` uses to resolve a bare `search`
/// call, so all three agree on one id per project (#1373).
/// Test: `expected_search_index_id_derives_from_project_dir_not_hardcoded`.
pub(super) fn expected_search_index_id(project_dir: Option<&Path>) -> String {
    let start = match project_dir {
        Some(dir) => dir.to_path_buf(),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    let root = trusty_common::resolve_project_root(&start);
    trusty_common::derive_index_id(&root)
}

/// True when `body` mentions an index named `name`.
///
/// Why: the `/indexes` payload shape varies (a bare string array, or objects
/// with an `id`/`name` field); a tolerant scan avoids coupling the probe to one
/// exact wire form.
/// What: returns true when any array element equals `name` directly or carries
/// an `id`/`name`/`index_id` field equal to `name`.
/// Test: `index_present_matches_each_shape`.
pub(super) fn index_present(body: &serde_json::Value, name: &str) -> bool {
    // The array may be the top-level value or nested under `indexes`.
    let array = body
        .as_array()
        .or_else(|| body.get("indexes").and_then(|v| v.as_array()));
    let Some(array) = array else {
        return false;
    };
    array.iter().any(|entry| {
        if entry.as_str() == Some(name) {
            return true;
        }
        ["id", "name", "index_id"]
            .iter()
            .any(|key| entry.get(key).and_then(|v| v.as_str()) == Some(name))
    })
}
