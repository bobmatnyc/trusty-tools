//! Daemon health probes for the trusty-* subsystem daemons `system_status`
//! reports on (epic #3052).
//!
//! Why: The owner asked that the assistant be able to answer "what subsystems
//! are running" on demand. A daemon being down is a NORMAL, reportable state —
//! not a tool-call error — so every probe here degrades to
//! [`DaemonStatus::down`] rather than propagating an `Err`. Each probe reuses
//! the shared `trusty_common::resolve_daemon_base_url` discovery convention
//! (the `http_addr` file every trusty-* daemon writes via `write_daemon_addr`,
//! confirmed for trusty-search/trusty-memory/trusty-analyze/trusty-mpm) rather
//! than guessing a hardcoded port, and is bounded by [`PROBE_TIMEOUT`] so a
//! wedged daemon degrades to "down" instead of hanging the calling agent turn.
//! What: [`DaemonStatus`] plus one `probe_*` function per subsystem daemon.
//! `system_status::gather` runs all four concurrently via `tokio::join!`.
//! Test: `super::tests` — an unregistered daemon name yields `up: false`
//! without erroring; a listener that never responds times out within
//! [`PROBE_TIMEOUT`] rather than hanging.

use std::time::Duration;

use serde::Serialize;

/// Per-probe timeout — bounds both the HTTP client and an outer
/// `tokio::time::timeout` so a daemon that accepts the TCP connection but
/// never writes a response (rather than refusing the connection outright)
/// still degrades to "down" within a couple of seconds.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// One subsystem daemon's reported health.
///
/// Why: a uniform shape lets `format::render_text` and the JSON output treat
/// every daemon identically regardless of which wire shape its `/health`
/// endpoint actually returns.
/// What: `up` is the single machine-readable liveness flag; `version` and
/// `detail` are best-effort extras parsed from the daemon's own response
/// shape (absent when down, or when the daemon predates that field).
/// Test: `super::tests::down_daemon_reports_up_false`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DaemonStatus {
    pub name: &'static str,
    pub up: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl DaemonStatus {
    /// The uniform "not running / not discoverable / unreachable" state.
    ///
    /// Why: every probe failure mode (no address file, connection refused,
    /// non-2xx, malformed body, timeout) collapses to this one reportable
    /// state — callers never need to distinguish them.
    fn down(name: &'static str) -> Self {
        Self {
            name,
            up: false,
            version: None,
            detail: None,
        }
    }
}

/// Discover `app_name`'s recorded address, probe `GET /health` with a bounded
/// timeout, and hand the parsed JSON body to `parse` for daemon-specific
/// field extraction.
///
/// Why: discovery is identical across every trusty-* daemon (same
/// `write_daemon_addr`/`resolve_daemon_base_url` convention); only the
/// response-shape parsing differs, so that is the one thing callers supply.
/// What: returns [`DaemonStatus::down`] on any failure (undiscoverable,
/// connection error, non-2xx, malformed JSON, or timeout); otherwise `up:
/// true` with whatever `parse` extracted.
/// Test: `super::tests::down_daemon_reports_up_false`,
/// `super::tests::unresponsive_daemon_times_out_rather_than_hanging`.
async fn probe(
    app_name: &'static str,
    parse: impl FnOnce(&serde_json::Value) -> (Option<String>, Option<String>),
) -> DaemonStatus {
    let Some(base) = trusty_common::resolve_daemon_base_url(app_name) else {
        return DaemonStatus::down(app_name);
    };
    let Ok(client) = reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() else {
        return DaemonStatus::down(app_name);
    };

    let fetch = async {
        let resp = client.get(format!("{base}/health")).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.json::<serde_json::Value>().await.ok()
    };

    match tokio::time::timeout(PROBE_TIMEOUT, fetch).await {
        Ok(Some(body)) => {
            let (version, detail) = parse(&body);
            DaemonStatus {
                name: app_name,
                up: true,
                version,
                detail,
            }
        }
        _ => DaemonStatus::down(app_name),
    }
}

fn version_field(body: &serde_json::Value) -> Option<String> {
    body.get("version")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Probe the trusty-search daemon (issue #40's #34/#873 `/health` shape).
///
/// Why: prior work (issue #873) references the daemon's `indexes` count and
/// `warmboot_summary.warm_boot_degraded` flag as the machine-readable
/// warm-boot health signal — surfacing both here means `system_status` can
/// report the exact same "cargo install dropped FDA" symptom operators
/// already watch for without tailing logs.
/// What: reads `version`, `indexes` (index count), and
/// `warmboot_summary.warm_boot_degraded`, folding the last two into `detail`.
/// Test: `super::tests::down_daemon_reports_up_false` (generic path); the
/// daemon-specific parse is exercised live in the manual verification.
pub async fn probe_search() -> DaemonStatus {
    probe("trusty-search", |body| {
        let version = version_field(body);
        let indexes = body.get("indexes").and_then(|v| v.as_u64());
        let degraded = body
            .get("warmboot_summary")
            .and_then(|w| w.get("warm_boot_degraded"))
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        let detail = indexes.map(|n| {
            if degraded {
                format!("{n} indexes (warm-boot degraded)")
            } else {
                format!("{n} indexes")
            }
        });
        (version, detail)
    })
    .await
}

/// Probe the trusty-memory daemon's `/health` (cheap liveness path — no
/// `?probe=true`, so this never triggers the expensive ONNX round-trip).
pub async fn probe_memory() -> DaemonStatus {
    probe("trusty-memory", |body| (version_field(body), None)).await
}

/// Probe the trusty-analyze daemon's `/health`.
///
/// Why: trusty-analyze has a hard runtime dependency on trusty-search
/// (`search_reachable`) — surfacing that in `detail` distinguishes "analyze
/// is down" from "analyze is up but degraded because search is down", which
/// is exactly the distinction its own `/health` handler encodes via HTTP 503.
pub async fn probe_analyze() -> DaemonStatus {
    probe("trusty-analyze", |body| {
        let version = version_field(body);
        let detail = body
            .get("search_reachable")
            .and_then(|v| v.as_bool())
            .map(|r| format!("upstream trusty-search reachable: {r}"));
        (version, detail)
    })
    .await
}

/// Probe the trusty-mpm (`tm`) daemon's `/health`.
///
/// Why: trusty-mpm's daemon writes its bound address via the same
/// `write_daemon_addr("trusty-mpm", …)` convention as the other three
/// daemons (`bin/tm/commands/daemon_run.rs`), so discovery is uniform even
/// though trusty-agents does not depend on the `trusty-mpm` crate directly —
/// this probe only ever touches the shared `trusty-common` discovery file
/// and a plain HTTP GET, never trusty-mpm's Rust types.
pub async fn probe_mpm() -> DaemonStatus {
    probe("trusty-mpm", |body| (version_field(body), None)).await
}

#[cfg(test)]
mod tests {
    // Why: `unresponsive_daemon_times_out_rather_than_hanging` holds
    // `crate::test_env::ENV_LOCK` across `.await` points by design — it
    // serializes against every other test that mutates the process-wide
    // `$HOME`/env vars for the duration of the body. Matches the established
    // crate-wide convention (see e.g. `runtime::direct_mode`,
    // `runtime::pm_mode`) rather than reaching for an async-aware Mutex just
    // for this one test-only lock.
    #![allow(clippy::await_holding_lock)]

    use super::*;

    /// Why: an app name that has never registered an address file must
    /// report `up: false` — never an `Err`, never a panic. This is the core
    /// "down is a normal, reportable state" contract.
    /// Test: itself.
    #[tokio::test]
    async fn down_daemon_reports_up_false() {
        let name = "trusty-system-status-test-never-registered";
        let status = probe(name, |_| (None, None)).await;
        assert!(!status.up);
        assert_eq!(status.name, name);
        assert!(status.version.is_none());
        assert!(status.detail.is_none());
    }

    /// Why: a daemon whose recorded address accepts the TCP connection but
    /// never writes a response must still resolve within [`PROBE_TIMEOUT`] —
    /// the agent turn must never hang on a wedged daemon.
    /// What: binds a listener that accepts and then sits idle, points
    /// `probe` at it via the shared `trusty_common` discovery file (guarded
    /// by the process-wide env/HOME locks), and asserts the probe completes
    /// (reporting down) inside a generous bound above `PROBE_TIMEOUT`.
    /// Test: itself.
    #[tokio::test]
    async fn unresponsive_daemon_times_out_rather_than_hanging() {
        let _env_guard = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // Accept connections but never read/write — simulates a wedged daemon.
        let _accept_task = tokio::spawn(async move {
            loop {
                if listener.accept().await.is_err() {
                    break;
                }
            }
        });

        let tmp = tempfile::TempDir::new().unwrap();
        let prev = std::env::var_os(trusty_common::DATA_DIR_OVERRIDE_ENV);
        // SAFETY: ENV_LOCK held for the whole body.
        unsafe {
            std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, tmp.path());
        }
        let app_name = "trusty-system-status-test-wedged";
        trusty_common::write_daemon_addr(app_name, &addr.to_string()).unwrap();

        let t0 = std::time::Instant::now();
        let status = probe(app_name, |_| (None, None)).await;
        let elapsed = t0.elapsed();

        // SAFETY: lock still held.
        unsafe {
            match prev {
                Some(v) => std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, v),
                None => std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV),
            }
        }

        assert!(!status.up, "a wedged daemon must report down, not hang");
        assert!(
            elapsed < Duration::from_secs(5),
            "probe must resolve near PROBE_TIMEOUT ({PROBE_TIMEOUT:?}), took {elapsed:?}"
        );
    }
}
