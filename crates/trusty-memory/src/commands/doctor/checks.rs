//! Individual diagnostic check functions for `trusty-memory doctor`.
//!
//! Why: factored out of the monolithic `doctor.rs` so the per-check
//! implementations live in their own focused file, separate from the
//! audit data types and the command entry-points.
//! What: exports check functions called by [`super::handle_doctor`]:
//! [`check_fastembed_cache`], `check_launchd_plist` (macOS),
//! [`check_daemon_health`], [`check_stale_palace_locks`], and the #6652
//! [`check_kg_redb_size`] disk-growth row. Also exports
//! private helpers used by the tests in `mod.rs`.
//! Test: individual check helpers are exercised by unit tests in `mod.rs`;
//! the async `check_daemon_health` test verifies fallback-port behaviour.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{CheckResult, CheckStatus};

/// Per-request budget for the `/health` probe.
///
/// Why (issue #4005): the old budget was 2 s, and on 2026-07-26 that produced
/// a hard "unreachable" verdict against a daemon whose MCP surface was
/// verifiably serving `memory_recall` / `memory_remember` in the same minutes.
/// The default `/health` handler is cheap but not free — it samples process
/// RSS/CPU through a `tokio::Mutex` and enumerates the process's open file
/// descriptors, neither of which is on the MCP request path. Under load those
/// can exceed 2 s while ordinary MCP calls sail through, so the probe was
/// measuring its own impatience rather than the daemon's health. 10 s is
/// comfortably above the observed worst case while still bounding the check.
/// The budget is only half the fix: a timeout now yields
/// [`super::CheckStatus::Unknown`] instead of a false failure. The
/// slow-but-serving case is covered end-to-end against a real listener by
/// `trusty-mpm`'s `memory_slow_but_serving_daemon_is_ok`, which exercises the
/// same timeout/refusal split from the `tm doctor` side.
/// Test: `check_daemon_health_fails_cleanly_with_no_listener`.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Verify the fastembed model cache exists and is readable.
///
/// Why: GH #58/#62 — when the daemon can't reach a writable cache path it
/// fails with EROFS on first embed and never goes ready. Checking for the
/// resolved cache dir up-front catches both "env var unset" (resolver falls
/// back to `$HOME/.cache/fastembed` which might not exist) and "directory
/// pinned but missing".
/// What: calls `trusty_common::embedder::resolve_fastembed_cache_dir()`,
/// then checks the path exists and is a directory we can read. Returns
/// `Pass` when the dir contains at least one model file, `Warn` when it
/// exists but is empty (pre-warm never ran), `Fail` when it does not exist.
/// Test: `fastembed_cache_check_reports_missing_dir`.
pub fn check_fastembed_cache() -> CheckResult {
    let cache = trusty_common::embedder::resolve_fastembed_cache_dir();
    let label = "fastembed cache".to_string();
    if !cache.exists() {
        return CheckResult::fail(
            label,
            format!(
                "missing: {} — run `trusty-memory setup` to pre-warm",
                cache.display()
            ),
        );
    }
    if !cache.is_dir() {
        return CheckResult::fail(label, format!("not a directory: {}", cache.display()));
    }
    match fastembed_cache_has_models(&cache) {
        Ok(true) => CheckResult::pass(label, format!("ready at {}", cache.display())),
        Ok(false) => CheckResult::warn(
            label,
            format!(
                "{} exists but is empty — daemon will download on first request",
                cache.display()
            ),
        ),
        Err(e) => CheckResult::fail(label, format!("cannot read {}: {e}", cache.display())),
    }
}

/// Check whether the fastembed cache directory holds at least one entry.
///
/// Why: an empty `~/.cache/fastembed` is operationally equivalent to a
/// missing one — the daemon will still have to download on first call.
/// What: returns `Ok(true)` if `read_dir` yields any entry, `Ok(false)` if
/// it's empty, `Err` if `read_dir` itself fails (permissions, etc.).
/// Test: `fastembed_cache_has_models_detects_entries`.
pub fn fastembed_cache_has_models(path: &Path) -> std::io::Result<bool> {
    let mut iter = std::fs::read_dir(path)?;
    Ok(iter.next().is_some())
}

/// Verify the launchd plist exists and contains `FASTEMBED_CACHE_PATH`.
///
/// Why: GH #62 — the whole point of the plist update is that
/// `FASTEMBED_CACHE_PATH` (and/or `FASTEMBED_CACHE_DIR`) is wired into the
/// daemon's environment. If an older plist is still installed without the
/// env var, the daemon will silently fail with EROFS. Detecting this is
/// the single most useful thing `doctor` can do.
/// What: resolves `~/Library/LaunchAgents/com.trusty.memory.plist`, reads
/// it as text, and looks for the `FASTEMBED_CACHE_PATH` key. `Pass` when
/// present, `Fail` when the file exists but the key is missing, `Fail`
/// when the file is missing entirely.
/// Test: `plist_check_detects_missing_env_var`.
#[cfg(target_os = "macos")]
pub fn check_launchd_plist() -> CheckResult {
    let label = "launchd plist".to_string();
    let Some(home) = dirs::home_dir() else {
        return CheckResult::fail(label, "could not resolve $HOME".to_string());
    };
    let plist = home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{}.plist", crate::commands::service::LAUNCHD_LABEL));
    if !plist.exists() {
        return CheckResult::fail(
            label,
            format!(
                "missing: {} — run `trusty-memory service install`",
                plist.display()
            ),
        );
    }
    match plist_contains_fastembed_cache_path(&plist) {
        Ok(true) => CheckResult::pass(label, format!("{} ok", plist.display())),
        Ok(false) => CheckResult::fail(
            label,
            format!(
                "{} is missing FASTEMBED_CACHE_PATH — reinstall via `trusty-memory service install`",
                plist.display()
            ),
        ),
        Err(e) => CheckResult::fail(label, format!("cannot read {}: {e}", plist.display())),
    }
}

/// Check whether a plist file contains the `FASTEMBED_CACHE_PATH` key.
///
/// Why: keeping the parse trivial (substring search) avoids pulling in an
/// XML/plist crate just for one diagnostic. The key string is unique enough
/// inside a launchd plist that a false positive is implausible.
/// What: reads the file as UTF-8 text and returns true iff the literal
/// `FASTEMBED_CACHE_PATH` substring appears.
/// Test: `plist_check_detects_missing_env_var`.
#[cfg(target_os = "macos")]
pub fn plist_contains_fastembed_cache_path(path: &Path) -> std::io::Result<bool> {
    let contents = std::fs::read_to_string(path)?;
    Ok(contents.contains("FASTEMBED_CACHE_PATH"))
}

/// Is the daemon reachable, and is it doing work?
///
/// Why the shape changed (#6286, ADR-0032): this used to read the `http_addr`
/// discovery file, GET `/health` at whatever it recorded, and — because that
/// file goes stale after a SIGKILL, which leaves it written but never cleaned —
/// fall back to walking ports 7070..=7079 so a live daemon on the default port
/// was not reported dead (#475). None of that applies to a socket. The path is
/// DERIVED rather than published, so there is no file to be stale and nothing
/// to walk: the daemon binds `trusty_common::daemon_socket_path` and every
/// consumer computes the same one.
///
/// What survives is the part that was never about the transport: a connection
/// proves a LISTENER is alive and nothing more, so this reads the daemon's own
/// account of its worker pool before calling it a pass — the #4001 fix, and the
/// reason #3992 stayed green for the length of an incident.
///
/// A timeout is still held apart from a refusal (#4005). A refused connection
/// proves nothing is serving; a timeout proves only that the daemon did not
/// answer inside OUR budget, which a busy-but-healthy daemon can miss. Only the
/// first justifies telling an operator to start the daemon.
///
/// Test: `check_daemon_health_fails_cleanly_with_no_listener`.
pub async fn check_daemon_health() -> CheckResult {
    let label = "daemon socket".to_string();

    let socket = match crate::transport::uds::socket_path() {
        Ok(path) => path,
        Err(e) => {
            return CheckResult::fail(
                label,
                format!("could not resolve the trusty-memory data directory: {e:#}"),
            )
        }
    };
    check_daemon_health_at(label, &socket, PROBE_TIMEOUT).await
}

/// [`check_daemon_health`] against an explicit socket and budget (#4001).
///
/// Why: the resolver reads this host's data directory, so a test could reach
/// the probe only by mutating process-wide env. This seam lets an in-process
/// socket that accepts and never answers prove the timeout arm stays bounded
/// and never reads as a pass, without touching a live daemon.
/// What: one framed `memory.health` call bounded by `timeout`. A timeout is
/// `Unknown`, any other transport error `Fail`, a JSON-RPC error `Fail`, and a
/// result goes through [`interpret_health_body`].
/// Test: `a_socket_that_accepts_and_never_answers_is_unknown_within_the_budget`,
/// `an_answer_without_a_health_body_is_never_a_healthy_run`,
/// `a_responsive_daemon_passes` in `checks_tests.rs`.
pub(super) async fn check_daemon_health_at(
    label: String,
    socket: &Path,
    timeout: Duration,
) -> CheckResult {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": crate::transport::uds::METHOD_HEALTH,
        "params": {},
    });

    let response: trusty_common::uds::server::RpcResponse =
        match trusty_common::uds::send_framed_request(socket, &request, timeout).await {
            Ok(response) => response,
            Err(trusty_common::uds::UdsRpcError::Timeout { .. }) => {
                return CheckResult::unknown(
                    label,
                    format!(
                        "{} did not answer {} within {timeout:?}. The connection was not \
                         refused, so the daemon may be alive and slow, or wedged — health \
                         could not be determined. Re-run when load subsides rather than \
                         restarting anything.",
                        socket.display(),
                        crate::transport::uds::METHOD_HEALTH,
                    ),
                );
            }
            Err(e) => {
                return CheckResult::fail(
                    label,
                    format!(
                        "no daemon is serving {} — it is unreachable ({e}). \
                         Start it with `trusty-memory service start`.",
                        socket.display()
                    ),
                );
            }
        };

    let url = socket.display().to_string();
    match (response.result, response.error) {
        (Some(body), _) => interpret_health_body(label, &url, 200, Some(&body)),
        (None, Some(e)) => CheckResult::fail(
            label,
            format!(
                "{} refused the health call: {} ({})",
                url, e.message, e.code
            ),
        ),
        (None, None) => CheckResult::unknown(
            label,
            format!("{url} answered with neither a result nor an error"),
        ),
    }
}

/// Turn a 2xx `/health` payload into a verdict (issues #4001, #4005).
///
/// Why: this function is the fix's thesis in one place. A 2xx from `/health`
/// proves that a listener accepted a socket and returned bytes — it does not
/// prove the daemon is doing work. Doctor used to stop at the status code,
/// which is how #3992 stayed green for the length of an incident. The body now
/// carries an observation of the worker pool, and this maps that observation
/// to a status, keeping "observed healthy" separate from "could not determine".
/// What: `Fail` when the daemon reports a wedge — naming the palace lock or the
/// worker pool according to `worker.wedged_reason`, since a sub-threshold
/// `stalled_lock` is reported beside a pool wedge. `Warn` when it reports
/// `degraded` or is still warming up. `Unknown` when the body is missing or
/// unparseable (a 2xx with no readable body tells us nothing about the
/// workers), and equally when the daemon reports its own stall tracking
/// stopped or never had a stall detector at all. `Pass` only when the daemon
/// positively reported a healthy worker pool it was still watching.
/// Test: `wedged_body_is_fail`, `warming_body_is_warn`,
/// `indeterminate_probe_renders_as_unknown_not_pass`,
/// `body_without_worker_block_is_unknown`, `healthy_body_is_pass`,
/// `degraded_body_is_warn`,
/// `stopped_stall_tracking_is_undetermined_not_a_warning`,
/// `a_daemon_with_no_stall_detector_is_undetermined_not_a_pass`,
/// `a_pool_wedge_beside_a_benign_stamp_names_the_pool`.
pub(super) fn interpret_health_body(
    label: String,
    url: &str,
    status: u16,
    body: Option<&serde_json::Value>,
) -> CheckResult {
    let Some(body) = body else {
        return CheckResult::unknown(
            label,
            format!(
                "{url} → {status}, but the response body could not be read or parsed. The \
                 listener is up; whether its workers are making progress is UNKNOWN."
            ),
        );
    };

    // Worker-pool observation (issue #4001) — the signal that would have
    // caught #3992. Checked first: a wedge outranks every other reading.
    let worker = body.get("worker");
    let wedged = worker
        .and_then(|w| w.get("wedged"))
        .and_then(serde_json::Value::as_bool);
    let oldest = worker
        .and_then(|w| w.get("oldest_age_secs"))
        .and_then(serde_json::Value::as_u64);
    let in_flight = worker
        .and_then(|w| w.get("in_flight"))
        .and_then(serde_json::Value::as_u64);

    // #4001: a held handle lock may have no tracked holder; name it instead.
    // `stalled_lock` is reported under the threshold too, so its presence does
    // not mean the lock is what wedged — `wedged_reason` says which arm tripped.
    let stalled_lock = worker.and_then(|w| w.get("stalled_lock"));
    let wedged_reason = worker
        .and_then(|w| w.get("wedged_reason"))
        .and_then(serde_json::Value::as_str);
    // A daemon predating `wedged_reason` reports only the lock's presence.
    let lock_wedge = wedged_reason.map_or_else(|| stalled_lock.is_some(), |r| r == "lock");
    match wedged {
        Some(true) if lock_wedge && stalled_lock.is_some() => {
            let field = |k: &str| stalled_lock.and_then(|s| s.get(k));
            let text = |k: &str| {
                field(k)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
            };
            return CheckResult::fail(
                label,
                format!(
                    "{url} → {status} BUT the daemon reports a WEDGED palace: {} lock of \
                     palace {} has been unavailable for at least {}s. Writes to it time out. \
                     Inspect with a thread sample before restarting.",
                    text("lock"),
                    text("palace"),
                    field("age_secs")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or_default()
                ),
            );
        }
        Some(true) => {
            return CheckResult::fail(
                label,
                format!(
                    "{url} → {status} BUT the daemon reports a WEDGED worker pool: oldest \
                     in-flight palace operation has been running {}s with {} in flight. The \
                     HTTP listener answering does not mean writes are progressing (issue \
                     #3992). Inspect with a thread sample before restarting.",
                    oldest.unwrap_or_default(),
                    in_flight.unwrap_or_default()
                ),
            );
        }
        None => {
            // No `worker` block: an older daemon build. Say so rather than
            // silently reporting a pass we cannot actually support.
            return CheckResult::unknown(
                label,
                format!(
                    "{url} → {status}, but this daemon does not report worker-pool occupancy \
                     (pre-#4001 build). Liveness is confirmed; whether workers are making \
                     progress is UNKNOWN. Upgrade the daemon to get a real answer."
                ),
            );
        }
        Some(false) => {}
    }

    // #4001: the `wedged: false` above is only worth what the detector behind
    // it is worth. The field is a plain bool with no `skip_serializing_if`, so
    // absent means a daemon with no detector at all — the 2026-09-13 incident
    // build, which reports `worker.wedged` (#3992) and so never reaches the
    // `None` arm above. Neither state may pass.
    match worker
        .and_then(|w| w.get("stall_tracking_ok"))
        .and_then(serde_json::Value::as_bool)
    {
        Some(true) => {}
        Some(false) => {
            let detail = body
                .get("detail")
                .and_then(|v| v.as_str())
                .unwrap_or("no detail reported");
            return CheckResult::unknown(
                label,
                format!(
                    "{url} → {status}, but the daemon's palace-lock stall tracking is not \
                     running: {detail}. Whether a palace lock is wedged is UNKNOWN. Restart \
                     the daemon to restore the detector."
                ),
            );
        }
        None => {
            return CheckResult::unknown(
                label,
                format!(
                    "{url} → {status}, but this daemon has no palace-lock stall detector \
                     (pre-#4001 build). It reports worker-pool occupancy only, and the \
                     2026-09-13 wedge held a lock no pool gauge could see, so whether a \
                     palace lock is wedged is UNKNOWN. Upgrade the daemon."
                ),
            );
        }
    }

    let daemon_state = body.get("daemon_state").and_then(|v| v.as_str());
    let reported = body.get("status").and_then(|v| v.as_str());

    // Issue #4005: a warming daemon is a normal post-restart state, not a
    // failure. It is also not fully healthy, so it warns rather than passes.
    if daemon_state == Some("warming") {
        return CheckResult::warn(
            label,
            format!(
                "{url} → {status}, daemon is WARMING UP (embedder still initialising). This is \
                 normal shortly after a restart — recall falls back to the non-embedder path \
                 until it finishes."
            ),
        );
    }

    if reported == Some("degraded") {
        let detail = body
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap_or("no detail reported");
        return CheckResult::warn(
            label,
            format!("{url} → {status}, daemon reports DEGRADED: {detail}"),
        );
    }

    let occupancy = match (in_flight, oldest) {
        (Some(n), Some(secs)) => format!(", {n} in flight, oldest {secs}s"),
        (Some(n), None) => format!(", {n} in flight"),
        _ => String::new(),
    };
    CheckResult::pass(
        label,
        format!("{url} → {status}, workers progressing{occupancy}"),
    )
}

/// The overall `trusty-memory doctor` verdict (issue #4001).
///
/// Why: a health probe that times out yields `Unknown` (#4005), and the run
/// used to print a green check mark and exit 0 whenever nothing had `Fail`ed.
/// A daemon wedged hard enough that `memory.health` never answered therefore
/// ended the run looking healthy. An undetermined check has not passed.
/// What: the one-line tally, with indeterminate checks in their own column,
/// and `healthy` true only when no check failed and none was undetermined.
/// `Warn` still does not flip it. The rule is deliberately wider than the
/// health probe: every `Unknown` counts, including
/// `tier_s::check_tier_s_reaffirmation`'s against a daemon predating #4890 and
/// `mcp_registration::check_mcp_registrations`'s when `$HOME` will not
/// resolve. Those runs now exit 1 on an otherwise clean
/// machine, which is the intended reading — doctor did not determine them.
/// Test: `an_undetermined_check_is_not_a_healthy_run`,
/// `warnings_alone_are_a_healthy_run`,
/// `a_non_health_undetermined_check_also_ends_the_run_unhealthy` in
/// `checks_tests.rs`.
pub(super) struct DoctorSummary {
    /// "N passed, N warnings, N undetermined, N failed."
    pub(super) line: String,
    /// Whether the run may end green with exit 0.
    pub(super) healthy: bool,
}

/// Tally `results` into a [`DoctorSummary`]. See its docs for the rule.
pub(super) fn summarize(results: &[CheckResult]) -> DoctorSummary {
    let count = |status: CheckStatus| results.iter().filter(|r| r.status == status).count();
    let (passed, warned) = (count(CheckStatus::Pass), count(CheckStatus::Warn));
    let (unknown, failed) = (count(CheckStatus::Unknown), count(CheckStatus::Fail));
    DoctorSummary {
        line: format!(
            "{passed} passed, {warned} warnings, {unknown} undetermined, {failed} failed."
        ),
        // #4001: a timed-out probe must never map to a healthy run.
        healthy: failed == 0 && unknown == 0,
    }
}

#[cfg(test)]
#[path = "checks_tests.rs"]
mod checks_tests;

/// Scan the data directory for stray `*.lock` files left over from a
/// crashed daemon.
///
/// Why: redb leaves a sidecar lock file when a previous owner exits
/// uncleanly; opening the palace from a fresh daemon then fails until the
/// stale lock is removed. Surfacing this in `doctor` saves users from a
/// confusing "palace won't load" symptom that has nothing to do with the
/// palace itself.
/// What: walks the trusty-memory data dir (one level deep into each palace
/// directory) and lists any `*.lock` file. `Pass` when none found, `Warn`
/// when at least one is present (the daemon may be running and using it,
/// so we can't safely call this a `Fail`).
/// Test: `stale_lock_check_warns_when_lock_present`.
pub fn check_stale_palace_locks() -> CheckResult {
    let label = "palace locks".to_string();
    let data_dir = match trusty_common::resolve_data_dir("trusty-memory") {
        Ok(d) => d,
        Err(e) => return CheckResult::fail(label, format!("could not resolve data dir: {e}")),
    };
    let root = crate::resolve_palace_registry_dir(data_dir);
    let locks = find_lock_files(&root);
    if locks.is_empty() {
        CheckResult::pass(label, format!("{} clean", root.display()))
    } else {
        let preview = locks
            .iter()
            .take(3)
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let suffix = if locks.len() > 3 {
            format!(" (+{} more)", locks.len() - 3)
        } else {
            String::new()
        };
        CheckResult::warn(
            label,
            format!(
                "{} lock file(s) found: {preview}{suffix} — if the daemon is stopped, these can be removed",
                locks.len()
            ),
        )
    }
}

/// Collect `*.lock` files one level deep beneath `root`.
///
/// Why: keeps the scan cheap (no recursive walk) while still catching the
/// common case of `<palace>/kg.redb.lock` sidecars from redb crashes.
/// What: returns every `*.lock` path in `root` itself and in each
/// immediate subdirectory of `root`. Missing or unreadable directories are
/// silently skipped (the surrounding check handles fatal data-dir errors).
/// Test: `find_lock_files_returns_paths`.
pub fn find_lock_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if is_lock_file(&path) {
            out.push(path.clone());
        }
        if path.is_dir() {
            if let Ok(sub) = std::fs::read_dir(&path) {
                for child in sub.flatten() {
                    let cpath = child.path();
                    if is_lock_file(&cpath) {
                        out.push(cpath);
                    }
                }
            }
        }
    }
    out
}

pub(super) fn is_lock_file(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()) == Some("lock")
}

/// Size at which a palace's `kg.redb` earns a warning (#6652).
const KG_REDB_WARN_BYTES: u64 = 100 * 1024 * 1024;

/// Size at which it earns a failure.
const KG_REDB_FAIL_BYTES: u64 = 500 * 1024 * 1024;

/// Report the largest `kg.redb` on disk and whether it needs compacting.
///
/// Why (#6652): redb never returns freed pages to the filesystem, so a palace's
/// knowledge-graph store grows monotonically no matter how much is retracted or
/// forgotten. `trusty-tools` reached 342 MB against 2,425 drawers, and nothing
/// in `doctor` said so — the operator's first signal was a slow write, traced
/// by hand. A size row makes the growth visible before it is a symptom.
/// What: stats `<palace>/kg.redb` for every palace under the registry root and
/// reports the largest. `Pass` under 100 MB, `Warn` from 100 MB to 500 MB with
/// the compaction command named, `Fail` above 500 MB. The verdict is about disk
/// only — a large store still serves every read correctly.
/// Test: `kg_redb_size_verdict_matches_the_thresholds`.
pub fn check_kg_redb_size() -> CheckResult {
    let label = "kg.redb size".to_string();
    let data_dir = match trusty_common::resolve_data_dir("trusty-memory") {
        Ok(d) => d,
        Err(e) => return CheckResult::fail(label, format!("could not resolve data dir: {e}")),
    };
    let root = crate::resolve_palace_registry_dir(data_dir);
    let Some((palace, bytes)) = largest_kg_redb(&root) else {
        return CheckResult::pass(label, format!("no palace store under {}", root.display()));
    };
    kg_redb_verdict(label, &palace, bytes)
}

/// The `(palace_id, bytes)` of the biggest `kg.redb` one level under `root`.
///
/// Why/What: the same one-level scan [`find_lock_files`] uses — a palace's
/// store is always `<root>/<palace>/kg.redb`, so a recursive walk would only
/// cost time. Unreadable entries are skipped rather than failing the check.
/// Test: `kg_redb_size_verdict_matches_the_thresholds`.
pub fn largest_kg_redb(root: &Path) -> Option<(String, u64)> {
    let mut best: Option<(String, u64)> = None;
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Ok(meta) = std::fs::metadata(dir.join("kg.redb")) else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if best.as_ref().is_none_or(|(_, b)| meta.len() > *b) {
            best = Some((name, meta.len()));
        }
    }
    best
}

/// Turn a size into a check verdict.
///
/// Why: the thresholds and their wording are the part worth unit-testing, and
/// they should not need a filesystem to exercise.
/// Test: `kg_redb_size_verdict_matches_the_thresholds`.
pub fn kg_redb_verdict(label: String, palace: &str, bytes: u64) -> CheckResult {
    let mb = bytes / (1024 * 1024);
    if bytes >= KG_REDB_FAIL_BYTES {
        CheckResult::fail(
            label,
            format!(
                "palace '{palace}' kg.redb is {mb} MB — run `trusty-memory palace compact \
                 {palace}` (reads are still correct; this is disk, not data)"
            ),
        )
    } else if bytes >= KG_REDB_WARN_BYTES {
        CheckResult::warn(
            label,
            format!(
                "palace '{palace}' kg.redb is {mb} MB — consider `trusty-memory palace stats \
                 {palace}` to see what is reclaimable"
            ),
        )
    } else {
        CheckResult::pass(label, format!("largest is '{palace}' at {mb} MB"))
    }
}
