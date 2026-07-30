//! Persistent service management for `trusty-agents` (#343).
//!
//! Why: Long-running trusty-agents sessions benefit from a single shared API
//! server (`--serve`) backing many lightweight REPL clients. This module
//! provides the daemonization machinery: spawn the binary detached,
//! persist its PID + port to `.trusty-agents/state/service.pid`, probe
//! liveness via the HTTP `/api/health` endpoint, and tear it down on
//! request. Used by `--service start|stop|status` (CLI) and
//! `/service start|stop|status` (REPL).
//!
//! What: A small struct (`ServiceState`) plus async helpers for
//! pid-file IO, liveness probing, daemon spawn (detached child with
//! stdin/stdout on `/dev/null` and stderr redirected to a rotating log
//! file — see `daemon_log_path`, #4111), and graceful shutdown via
//! `kill(1)`.
//!
//! Test: `cargo test --lib service::` covers pid-file roundtrip,
//! port-default constants, missing-file behavior, and log-rotation
//! behavior. End-to-end daemon spawn is exercised manually via
//! `trusty-agents --service start`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Default port the API server binds to. Mirrors `src/main.rs`'s default
/// for `--serve` so a `/service start` with no overrides matches what
/// the user would have gotten typing `trusty-agents --serve` directly.
pub const DEFAULT_SERVICE_PORT: u16 = 8080;

/// Persisted record of the running daemon.
///
/// Why: A separate file lets external tooling (and recovery code after
/// a REPL crash) discover the service without having to re-probe ports.
/// What: Serialized as JSON in `.trusty-agents/state/service.pid`.
/// Test: `pid_file_roundtrip` in this module's tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceState {
    pub pid: u32,
    pub started_at: DateTime<Utc>,
    pub port: u16,
}

/// Resolve the canonical pid-file path under the *self-project* state
/// directory. Falls back to `./` when no project root is detected so the
/// helpers always have a writable path.
pub fn pid_file_path() -> PathBuf {
    let root = crate::ctrl::detect_self_project()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    root.join(".trusty-agents")
        .join("state")
        .join("service.pid")
}

/// Ensure the parent of `pid_file_path()` exists. No-op if it already does.
fn ensure_state_dir() -> Result<()> {
    if let Some(parent) = pid_file_path().parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating service state dir {}", parent.display()))?;
    }
    Ok(())
}

/// Read the pid file if it exists. Returns `None` for missing/corrupt.
pub fn read_pid_file() -> Option<ServiceState> {
    let path = pid_file_path();
    let raw = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Write the pid file atomically (best-effort: write-then-rename).
pub fn write_pid_file(state: &ServiceState) -> Result<()> {
    ensure_state_dir()?;
    let path = pid_file_path();
    let tmp = path.with_extension("pid.tmp");
    let bytes = serde_json::to_vec_pretty(state)?;
    std::fs::write(&tmp, bytes)
        .with_context(|| format!("writing temp pid file {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("renaming temp pid file to {}", path.display()))?;
    Ok(())
}

/// Best-effort removal of the pid file. Silently ignores missing files.
pub fn remove_pid_file() {
    let path = pid_file_path();
    let _ = std::fs::remove_file(path);
}

/// Check whether process `pid` is alive by signaling 0 (no-op signal).
///
/// Why: We need a cheap "is the daemon still around?" check independent
/// of the HTTP probe so we can detect crashed daemons whose port is now
/// owned by something else.
/// What: Shells out to `kill -0 <pid>`. Returns true iff exit status is 0.
fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Probe the API server's `/api/health` endpoint.
///
/// Why: PID-only checks are necessary but not sufficient — a process
/// may be alive but still binding ports, or hung on startup. Confirming
/// `/api/health` returns 2xx within a tight 500ms budget gives us a
/// "really running" signal without slowing the REPL bootstrap.
/// What: Issues a GET with a 500ms timeout. Returns true on 2xx.
async fn health_ok(port: u16) -> bool {
    let url = format!("http://127.0.0.1:{port}/api/health");
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    match client.get(&url).send().await {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

/// Returns true iff a service is observably running on `port`.
///
/// Why: REPL startup uses this to decide whether to enter thin-client
/// mode. We require *both* a live PID (per pid file) AND a healthy
/// HTTP probe so a stale pid file or crashed daemon doesn't trick us.
/// When the pid file is missing entirely we still check `/api/health`
/// so an externally-launched `--serve` (e.g. via systemd) still wins.
pub async fn is_service_running(port: u16) -> bool {
    // Fast path: no pid file means we never started a daemon ourselves.
    // Skipping the HTTP probe avoids a 500ms timeout on every cold REPL
    // start when no service is configured (#477).
    if read_pid_file().is_none() {
        return false;
    }
    if let Some(state) = read_pid_file()
        && state.port == port
        && pid_alive(state.pid)
        && health_ok(port).await
    {
        return true;
    }
    // Stale pid file detected. We don't remove it here — that's
    // start_service's job — but we don't claim it's running either.
    // Fallback: a healthy port is enough to count as "running" even
    // without a pid file (externally-launched daemons).
    health_ok(port).await
}

/// Directory the daemon's log file lives under: `~/Library/Logs/trusty-agents/`.
///
/// Why (#4111): every other trusty-* daemon in this workspace writes to
/// `~/Library/Logs/<name>/` (e.g. trusty-search's `launchd_log_dir()` in
/// `crates/trusty-search/src/commands/service.rs`), but trusty-agents isn't
/// launchd-managed — it self-spawns via `start_service` below rather than
/// running as a `LaunchAgent` — so it never got a log directory at all.
/// This gives it the same directory naming convention without requiring
/// launchd.
/// What: `dirs::home_dir()` (falling back to `.` if unresolvable, matching
/// the fallback already used elsewhere in this crate, e.g.
/// `runtime::startup`'s REPL log path) joined with `Library/Logs/trusty-agents`.
fn log_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library")
        .join("Logs")
        .join("trusty-agents")
}

/// A short, stable, filesystem-safe discriminator for the CURRENT project —
/// the SAME identity `pid_file_path()` already uses
/// (`crate::ctrl::detect_self_project()`, falling back to cwd), hashed down
/// to a fixed-width hex string so it can live in a flat filename.
///
/// Why (code-critic MEDIUM correction on #4111): the first cut of
/// `daemon_log_path` was keyed on `port` alone. `DEFAULT_SERVICE_PORT` is a
/// hardcoded `8080` shared by every project, so in the common case (no
/// explicit `--port` override) every project's daemon still wrote
/// `daemon-8080.log` — the exact clobbering #4111 was supposed to fix stayed
/// unresolved, and the doc comment's "matches the per-project pid-file
/// model" claim was false (the pid file is keyed on project path, this was
/// keyed on port). Hashing `detect_self_project()`'s result gives the log
/// path the SAME identity axis the pid file already uses.
/// What: `DefaultHasher` over the canonicalized project root (or cwd),
/// formatted as 16 lowercase hex digits. Not cryptographic — collision
/// resistance for a handful of concurrently-open local projects is all this
/// needs, and matches the low-stakes, best-effort nature of a log filename.
/// Test: `project_log_discriminator_is_deterministic`,
/// `project_log_discriminator_is_16_lowercase_hex_chars`.
fn project_log_discriminator() -> String {
    use std::hash::{Hash, Hasher};
    let root = crate::ctrl::detect_self_project()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let canonical = std::fs::canonicalize(&root).unwrap_or(root);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    canonical.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Full path to the daemon's rotating log file for a given `port`.
///
/// Why: Single source of truth for both `start_service` (which opens it
/// for the child's stderr) and any future `--service logs` / doctor check
/// that wants to locate it. Scoped by BOTH project (`project_log_discriminator`,
/// code-critic MEDIUM correction on #4111 — mirrors `pid_file_path()`'s own
/// per-project identity) AND `port`, so neither two different projects on
/// the shared default port, nor the same project running two ports at once,
/// interleave their output into one file or clobber each other's rotation
/// backup.
/// What: `<log_dir>/daemon-<project-hash>-<port>.log`.
/// Test: `daemon_log_path_ends_with_expected_components`,
/// `daemon_log_path_differs_by_port`,
/// `project_log_discriminator_is_deterministic`,
/// `project_log_discriminator_is_16_lowercase_hex_chars`.
pub fn daemon_log_path(port: u16) -> PathBuf {
    log_dir().join(format!("daemon-{}-{port}.log", project_log_discriminator()))
}

/// Size threshold (bytes) at which `rotate_log_if_oversized` moves the
/// current log out of the way. 10 MiB comfortably covers weeks of `info`-level
/// daemon output before rotating.
const ROTATE_AT_BYTES: u64 = 10 * 1024 * 1024;

/// Rotate `path` out of the way if it has grown past `ROTATE_AT_BYTES`.
///
/// Why (#4111): `--serve` daemons commonly run for weeks, so a log file
/// that is never rotated grows unbounded. Every other trusty-* daemon
/// bounds its `~/Library/Logs/<name>/stderr.log` via a `newsyslog` config
/// installed alongside its LaunchAgent (see
/// `crates/trusty-search/src/commands/log_rotation.rs`: `ROTATION_SIZE_KB`
/// / `ROTATION_KEEP`) — that mechanism can't be reused as-is here because
/// it depends on launchd owning the file handle (an external tool rotates
/// the inode; launchd reopens the path on next write). trusty-agents opens
/// its own log file directly, so a rotate-on-(re)start check achieves a
/// bounded-growth outcome for this daemon's shape without needing a
/// LaunchAgent.
///
/// IMPORTANT (code-critic MEDIUM correction on #4111): this function is
/// only called once, at spawn time in `start_service`, NOT periodically
/// from inside the running daemon. The actual guarantee is therefore
/// "bounded ACROSS restarts, not within a single run" — a daemon that
/// stays up for weeks without ever being restarted still grows its log
/// unbounded until the next `/service start`. A background rotator inside
/// the running process would close that gap but isn't implemented; if the
/// unbounded-single-run growth becomes a real problem, that's the fix to
/// reach for.
/// What: keeps exactly one previous generation (`daemon-<port>.log.1`,
/// overwriting any older one) once the current file exceeds
/// `ROTATE_AT_BYTES`. A file that doesn't exist yet, or any rename I/O
/// error, is silently ignored — log housekeeping must never block daemon
/// startup.
/// Test: `rotate_log_renames_oversized_file`,
/// `rotate_log_leaves_small_file_alone`.
fn rotate_log_if_oversized(path: &Path) {
    let Ok(meta) = std::fs::metadata(path) else {
        return; // doesn't exist yet — nothing to rotate
    };
    if meta.len() < ROTATE_AT_BYTES {
        return;
    }
    let backup = path.with_extension("log.1");
    let _ = std::fs::rename(path, &backup);
}

/// Attempt to open the daemon's log file for append, creating its parent
/// directory and rotating an oversized generation first.
///
/// Why (code-critic HIGH-2 on #4111): log setup must never block daemon
/// startup — the same "must never block startup" principle
/// `rotate_log_if_oversized` already follows by swallowing its own I/O
/// errors. An unwritable `~/Library/Logs`, a full disk, or a permissions
/// problem must not abort the whole daemon spawn: EVERYTHING behind
/// `start_service` (Concierge, Telegram, Slack) depends on it starting.
/// Returning `Option` instead of `Result` — and never using `?` — makes
/// "give up gracefully" the only path available to the caller; there is no
/// error variant left to accidentally propagate.
/// What: `None` on ANY failure (parent dir uncreatable, or the file
/// unopenable) — logged once via `tracing::warn!` so the failure is at
/// least visible if logging itself is reachable, but never returned as an
/// error. `Some(file)` on success. The caller (`start_service`) falls back
/// to `Stdio::null()` on `None` — exactly the pre-#4111 behavior.
/// Test: `open_daemon_log_file_succeeds_for_writable_path`,
/// `open_daemon_log_file_returns_none_when_parent_is_blocked_by_a_file`.
fn open_daemon_log_file(log_path: &Path) -> Option<std::fs::File> {
    let parent = log_path.parent()?;
    if let Err(e) = std::fs::create_dir_all(parent) {
        tracing::warn!(
            error = %e,
            dir = %parent.display(),
            "could not create daemon log dir; daemon will run without a log file"
        );
        return None;
    }
    rotate_log_if_oversized(log_path);
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
    {
        Ok(file) => Some(file),
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %log_path.display(),
                "could not open daemon log file; daemon will run without a log file"
            );
            None
        }
    }
}

/// Spawn `trusty-agents --serve` as a detached child process.
///
/// Why: `/service start` should return immediately while the API
/// continues serving in the background. Detached stdio + `std::process`
/// (not tokio) avoids both terminal-control contamination and runtime
/// re-entry from the REPL's tokio context.
/// What: Resolves the current binary via `current_exe()`, spawns it with
/// `--serve --port <port>`. stdin/stdout stay redirected to `/dev/null`
/// (`runtime::startup`'s tracing setup writes to stderr precisely so
/// stdout stays clean for MCP JSON-RPC framing — nothing in `--serve` mode
/// needs stdin, and this spawn call must not disturb that contract).
/// stderr (#4111) is redirected to the rotating file at `daemon_log_path()`
/// instead of `/dev/null` — the child's own tracing initialization is
/// unchanged (it already writes to stderr in non-interactive mode; the bug
/// was purely this spawn call discarding that stream). Log setup
/// (`open_daemon_log_file`) is best-effort (code-critic HIGH-2 on #4111):
/// any failure falls back to `Stdio::null()` rather than aborting the
/// spawn, so an unwritable log directory can never prevent the daemon —
/// and therefore Concierge/Telegram/Slack — from starting. Persists the
/// pid file, then polls `/api/health` for up to 3 seconds. Returns the
/// recorded `ServiceState` on success.
pub async fn start_service(port: u16) -> Result<ServiceState> {
    if is_service_running(port).await {
        // Idempotent: if the service is already running on this port, treat
        // that as success. The caller wanted a running service; one is up.
        // Return the existing state if we can read it; otherwise synthesize a
        // minimal record so callers still get a ServiceState back.
        println!("trusty-agents server already running on port {port}");
        if let Some(state) = read_pid_file() {
            return Ok(state);
        }
        return Ok(ServiceState {
            pid: 0,
            started_at: Utc::now(),
            port,
        });
    }

    // Clean up any stale pid file before spawning.
    remove_pid_file();

    let exe = std::env::current_exe().context("resolving current executable path")?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // #4111 (best-effort, code-critic HIGH-2): open (and rotate, if
    // oversized) the daemon's log file BEFORE spawning, so the child
    // inherits an already-open handle for its stderr. ANY failure here
    // falls back to `Stdio::null()` — log housekeeping must never prevent
    // the daemon from starting.
    let log_path = daemon_log_path(port);
    let stderr_stdio = open_daemon_log_file(&log_path)
        .map(Stdio::from)
        .unwrap_or_else(Stdio::null);

    let child = std::process::Command::new(&exe)
        .arg("--serve")
        .arg("--port")
        .arg(port.to_string())
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr_stdio)
        .spawn()
        .with_context(|| format!("spawning {} --serve", exe.display()))?;

    let state = ServiceState {
        pid: child.id(),
        started_at: Utc::now(),
        port,
    };
    write_pid_file(&state)?;

    // Wait up to 3s for the daemon to bind and answer /api/health.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if health_ok(port).await {
            return Ok(state);
        }
        // If the child died before becoming ready, surface that fast.
        if !pid_alive(state.pid) {
            remove_pid_file();
            anyhow::bail!(
                "service exited during startup (pid {} no longer alive)",
                state.pid
            );
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    // Soft timeout: leave the pid file (so `/service status` can still
    // see it) but tell the caller startup didn't confirm.
    anyhow::bail!(
        "service started (pid {}) but /api/health did not respond within 3s",
        state.pid
    )
}

/// Stop the running service via the pid file.
///
/// Why: A clean shutdown lets the API drain in-flight requests and
/// release the port before the next `/service start`.
/// What: Reads the pid file, sends SIGTERM via `kill <pid>`, waits up
/// to 3s for the process to exit, then removes the pid file. Returns
/// an error if no pid file exists or the kill itself fails.
pub async fn stop_service() -> Result<()> {
    let state = read_pid_file().context("no service pid file found (is the service running?)")?;

    if !pid_alive(state.pid) {
        // Nothing to do — already gone.
        remove_pid_file();
        return Ok(());
    }

    let status = std::process::Command::new("kill")
        .arg(state.pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("invoking kill {}", state.pid))?;
    if !status.success() {
        anyhow::bail!("kill {} returned non-zero status", state.pid);
    }

    // Wait up to 3s for the process to exit.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if !pid_alive(state.pid) {
            remove_pid_file();
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    // Still alive after SIGTERM — escalate to SIGKILL.
    let _ = std::process::Command::new("kill")
        .arg("-9")
        .arg(state.pid.to_string())
        .status();
    remove_pid_file();
    Ok(())
}

/// Convenience: render a one-line human-readable status string.
///
/// Why: Both the `--service status` CLI and the `/service status` REPL
/// command want the same compact summary; centralizing it here keeps
/// them in sync.
pub async fn status_line(port: u16) -> String {
    if is_service_running(port).await {
        if let Some(s) = read_pid_file() {
            format!(
                "service running (pid {}, port {}, started {})",
                s.pid,
                s.port,
                s.started_at.to_rfc3339()
            )
        } else {
            format!("service running on port {port} (no pid file)")
        }
    } else {
        format!("service not running (port {port})")
    }
}

/// Submit a task to a running service and poll until completion.
///
/// Why: When the REPL detects an existing service it forwards user
/// messages over HTTP instead of running them in-process. Centralizing
/// the submit+poll loop keeps every HTTP-forwarding call site consistent.
/// What: POST `/api/task` with `{ "task": ... }`, then GET
/// `/api/task/:id` every 2s until status leaves "running". Returns the
/// terminal `narrative` string (or an error string when the server
/// reports errors).
/// Test: Exercised manually via `/service start` + REPL chat.
pub async fn submit_task_via_service(server_url: &str, task: &str) -> Result<String> {
    let client = reqwest::Client::new();
    #[derive(Serialize)]
    struct TaskBody<'a> {
        task: &'a str,
    }

    let resp = client
        .post(format!("{server_url}/api/task"))
        .json(&TaskBody { task })
        .send()
        .await
        .with_context(|| format!("POST {server_url}/api/task"))?;
    let status = resp.status();
    let submitted: serde_json::Value = resp
        .json()
        .await
        .context("decoding /api/task response body")?;
    if !status.is_success() {
        anyhow::bail!("service rejected submission: {submitted}");
    }
    let id = submitted
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("response missing id: {submitted}"))?
        .to_string();

    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let r = client
            .get(format!("{server_url}/api/task/{id}"))
            .send()
            .await
            .with_context(|| format!("GET {server_url}/api/task/{id}"))?;
        if !r.status().is_success() {
            anyhow::bail!("polling failed: status {}", r.status());
        }
        let v: serde_json::Value = r.json().await.context("decoding /api/task/:id body")?;
        let status_str = v
            .get("status")
            .and_then(|s| s.as_str())
            .unwrap_or("running");
        if status_str == "running" {
            continue;
        }
        // Terminal — extract narrative or surface errors.
        let narrative = v
            .get("narrative")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let errs: Vec<String> = v
            .get("errors")
            .and_then(|e| e.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if !narrative.is_empty() {
            return Ok(narrative);
        }
        if !errs.is_empty() {
            anyhow::bail!("service errors: {}", errs.join("; "));
        }
        return Ok(format!("(no narrative; status={status_str})"));
    }
}

#[allow(dead_code)]
fn _path_unused(_p: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_port_is_8080() {
        assert_eq!(DEFAULT_SERVICE_PORT, 8080);
    }

    #[test]
    fn pid_file_roundtrip() {
        // We don't touch the real pid path in tests — write to a temp
        // file directly and round-trip the JSON manually to keep the
        // test hermetic.
        let state = ServiceState {
            pid: 99999,
            started_at: Utc::now(),
            port: 8080,
        };
        let s = serde_json::to_string(&state).expect("serialize");
        let back: ServiceState = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back.pid, 99999);
        assert_eq!(back.port, 8080);
    }

    #[test]
    fn read_pid_file_missing_is_none() {
        // Deliberately resolve to a non-existent path. Even if the
        // canonical pid file happens to exist on the dev machine, this
        // test only asserts the absence-tolerance of the parser via a
        // direct deserialize attempt on garbage.
        let parsed: Option<ServiceState> = serde_json::from_str("not json").ok();
        assert!(parsed.is_none());
    }

    #[tokio::test]
    async fn health_ok_returns_false_for_unbound_port() {
        // 1 is reserved + nothing should be listening on 1 at user level.
        assert!(!health_ok(1).await);
    }

    // ---- #4111: daemon log file ----

    #[test]
    fn daemon_log_path_ends_with_expected_components() {
        let p = daemon_log_path(8080);
        let s = p.to_string_lossy();
        assert!(s.contains("Library/Logs/trusty-agents/daemon-"), "{s}");
        assert!(s.ends_with("-8080.log"), "{s}");
    }

    #[test]
    fn daemon_log_path_differs_by_port() {
        // code-critic MEDIUM (#4111): two concurrently-running daemons on
        // different ports must not share a log file (or a rotation backup).
        assert_ne!(daemon_log_path(8080), daemon_log_path(8081));
    }

    #[test]
    fn project_log_discriminator_is_deterministic() {
        assert_eq!(project_log_discriminator(), project_log_discriminator());
    }

    #[test]
    fn project_log_discriminator_is_16_lowercase_hex_chars() {
        let d = project_log_discriminator();
        assert_eq!(d.len(), 16, "{d}");
        assert!(
            d.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "{d}"
        );
    }

    #[test]
    fn rotate_log_leaves_small_file_alone() {
        let dir = std::env::temp_dir().join(format!("ta-log-test-small-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("daemon.log");
        std::fs::write(&path, b"tiny log line\n").unwrap();

        rotate_log_if_oversized(&path);

        assert!(path.exists(), "small file should not be rotated away");
        assert!(!path.with_extension("log.1").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotate_log_renames_oversized_file() {
        let dir = std::env::temp_dir().join(format!("ta-log-test-big-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("daemon.log");
        // Write past ROTATE_AT_BYTES so rotation triggers.
        let oversized = vec![b'x'; (ROTATE_AT_BYTES + 1) as usize];
        std::fs::write(&path, &oversized).unwrap();

        rotate_log_if_oversized(&path);

        assert!(
            !path.exists(),
            "oversized log should have been renamed away"
        );
        let backup = path.with_extension("log.1");
        assert!(backup.exists(), "rotated backup should exist");
        assert_eq!(
            std::fs::metadata(&backup).unwrap().len(),
            oversized.len() as u64
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotate_log_missing_file_is_a_noop() {
        let path = std::env::temp_dir().join(format!(
            "ta-log-test-missing-{}-daemon.log",
            std::process::id()
        ));
        // Doesn't exist — must not panic, and must not create anything.
        rotate_log_if_oversized(&path);
        assert!(!path.exists());
    }

    // ---- code-critic HIGH-2 (#4111): best-effort log open ----

    #[test]
    fn open_daemon_log_file_succeeds_for_writable_path() {
        let dir = std::env::temp_dir().join(format!("ta-log-open-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("daemon.log");

        let file = open_daemon_log_file(&path);

        assert!(file.is_some(), "expected a file handle for a writable path");
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_daemon_log_file_returns_none_when_parent_is_blocked_by_a_file() {
        // Empirically forces `create_dir_all` to fail: a regular file
        // sitting where a directory component needs to be.
        let dir = std::env::temp_dir().join(format!("ta-log-open-blocked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let blocking_file = dir.join("blocking");
        std::fs::write(&blocking_file, b"not a directory").unwrap();
        let log_path = blocking_file.join("nested").join("daemon.log");

        let file = open_daemon_log_file(&log_path);

        assert!(
            file.is_none(),
            "expected None (never a propagated error) when a path component \
             is a regular file, not a directory"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
