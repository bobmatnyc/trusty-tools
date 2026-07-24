//! Pure check helpers used by the doctor pipeline.
//!
//! Why: the original `main.rs` carried ~250 lines of pure check functions
//! plus the supporting types (`CheckResult`, `EmptyIndex`). Lifting them into
//! this module shrinks `main.rs` and keeps the checks independently testable
//! without dragging in async runtime or HTTP client setup.
//! What: pure helpers + the two value types the pipeline produces.
//! Test: `cargo test --workspace` — the doctor integration tests exercise
//! these end-to-end.

use super::daemon_utils::{daemon_port_path, port_reachable};
use super::format::{dir_size_bytes, fmt_bytes, format_with_commas};
use colored::Colorize;

/// Outcome of a single doctor check.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckResult {
    /// Check passed.
    Ok(String),
    /// Non-fatal issue; doctor continues.
    Warn(String),
    /// Fatal issue; counted as an error.
    Error(String),
}

impl CheckResult {
    pub fn print(&self) {
        match self {
            CheckResult::Ok(msg) => println!("{} {}", "✓".green(), msg),
            CheckResult::Warn(msg) => println!("{} {}", "⚠".yellow(), msg),
            CheckResult::Error(msg) => println!("{} {}", "✗".red(), msg),
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(self, CheckResult::Error(_))
    }

    pub fn is_warn(&self) -> bool {
        matches!(self, CheckResult::Warn(_))
    }
}

/// Represents an index that has no chunks (fixable via reindex).
#[derive(Debug)]
pub struct EmptyIndex {
    pub name: String,
    pub root_path: String,
}

/// Return the directory where fastembed caches ONNX models.
///
/// Why: fastembed uses `FASTEMBED_CACHE_DIR` env var when set, otherwise
/// `.fastembed_cache` relative to the process CWD. For the daemon the CWD
/// is wherever the user launched it — so we check the env var first, then
/// fall back to the cache path next to the trusty-search data dir.
pub fn fastembed_cache_dir() -> std::path::PathBuf {
    if let Ok(s) = std::env::var("FASTEMBED_CACHE_DIR") {
        return std::path::PathBuf::from(s);
    }
    if let Some(d) = dirs::data_local_dir() {
        let candidate = d.join("trusty-search").join(".fastembed_cache");
        if candidate.exists() {
            return candidate;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        let candidate = exe
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join(".fastembed_cache");
        if candidate.exists() {
            return candidate;
        }
    }
    std::path::PathBuf::from(".fastembed_cache")
}

/// Read the daemon port from the port file (or return the default port).
pub fn read_daemon_port() -> u16 {
    daemon_port_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<u16>().ok())
        .unwrap_or(trusty_search::service::DEFAULT_PORT)
}

/// Why: separates the network probe from the result-formatting so the
/// formatting check can be tested without async/HTTP.
/// What: returns `(running, version)` by hitting `/health`.
pub async fn probe_daemon_health(client: &reqwest::Client, base: &str) -> (bool, String) {
    let health_result = client.get(format!("{}/health", base)).send().await;
    match health_result {
        Ok(r) if r.status().is_success() => {
            let body: serde_json::Value = r.json().await.unwrap_or_else(|_| serde_json::json!({}));
            let ver = body
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string();
            (true, ver)
        }
        _ => (false, String::new()),
    }
}

/// Pure formatting of the daemon liveness verdict.
pub fn check_daemon_running(running: bool, base: &str, version: &str) -> CheckResult {
    if running {
        CheckResult::Ok(format!("Daemon running at {} (v{})", base, version))
    } else {
        CheckResult::Error("Daemon not running — run `trusty-search start`".to_string())
    }
}

/// Inspect the fastembed model cache.
pub fn check_model_cache() -> CheckResult {
    let model_cache = fastembed_cache_dir();
    let model_name = "all-MiniLM-L6-v2";
    let model_subdir = model_cache.join("models--Qdrant--all-MiniLM-L6-v2-onnx");
    if model_subdir.exists() {
        let size = dir_size_bytes(&model_cache);
        CheckResult::Ok(format!(
            "Model cache: {} ({}, {})",
            model_cache.display(),
            fmt_bytes(size),
            model_name
        ))
    } else if model_cache.exists() {
        CheckResult::Warn(format!(
            "Model cache directory exists ({}) but {} not found — will download on first start",
            model_cache.display(),
            model_name
        ))
    } else {
        CheckResult::Warn(
            "Model not cached — will download on first `trusty-search start`".to_string(),
        )
    }
}

/// Return the per-user data directory path.
///
/// Why: doctor subcommands need a data-dir path independent of the typed
/// `DaemonError` return from `daemon_dir()`. Honouring `TRUSTY_DATA_DIR`
/// here ensures `trusty-search doctor` inspects the same directory as the
/// running daemon when an isolated data dir is active (issue #281).
/// What: returns `$TRUSTY_DATA_DIR` when set, otherwise the platform-default
/// `<data_local_dir>/trusty-search`.
/// Test: set `TRUSTY_DATA_DIR=/tmp/ts-x`; assert `doctor_data_dir()` returns
/// `PathBuf::from("/tmp/ts-x")`.
pub fn doctor_data_dir() -> std::path::PathBuf {
    doctor_data_dir_from(std::env::var("TRUSTY_DATA_DIR").ok())
}

/// Pure core of [`doctor_data_dir`] — takes the `TRUSTY_DATA_DIR` value as a
/// parameter instead of reading the process-global env var directly.
///
/// Why (issue #3697): `doctor_data_dir()`'s own test previously had to
/// `remove_var`/`set_var` the shared `TRUSTY_DATA_DIR` process env var and
/// rely on `#[serial]` for mutual exclusion against the 40+ other tests in
/// this test binary that also mutate it. Every such call site already
/// carries the crate's bare `#[serial]` tag (audited across
/// `commands/migrate_storage/*`, `commands/start/tests.rs`,
/// `commands/daemon_utils.rs`, `service/data_dir.rs`,
/// `service/daemon_tests.rs`, `service/server/*_tests.rs`,
/// `service/reindex/root_hijack_tests.rs`, and
/// `service/warm_boot/warm_boot_tests.rs`), so the logical race the
/// `#3673`/`#3686` fix targeted is already closed — yet PR #3690's gate still
/// observed a rare failure. Splitting out this parameter-injectable core
/// (mirroring the `SearchAppState::with_registry_path` fix for the same
/// class of flake in `service/server/list_repo_identity_tests.rs`, issue
/// #2717) removes the test's dependency on process env entirely, so it can
/// no longer race ANY other test regardless of `#[serial]` coverage.
/// What: mirrors the `TRUSTY_DATA_DIR`-set / unset branches of
/// `doctor_data_dir()` exactly, but reads the value from `data_dir_env`
/// rather than `std::env::var`.
/// Test: `doctor_data_dir_returns_non_empty_path` (passes `None` to
/// deterministically exercise the platform-default fallback branch).
fn doctor_data_dir_from(data_dir_env: Option<String>) -> std::path::PathBuf {
    if let Some(dir) = data_dir_env {
        return std::path::PathBuf::from(dir);
    }
    dirs::data_local_dir()
        .map(|d| d.join("trusty-search"))
        .unwrap_or_else(|| std::path::PathBuf::from("~/.local/share/trusty-search"))
}

/// Verify the data directory exists and is writable.
pub fn check_data_dir(data_dir: &std::path::Path) -> CheckResult {
    if !data_dir.exists() {
        return CheckResult::Warn(format!(
            "Data directory {} does not exist (will be created on first start)",
            data_dir.display()
        ));
    }
    let probe = data_dir.join(".write_probe");
    let writable = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    if writable {
        CheckResult::Ok(format!("Data directory: {} (writable)", data_dir.display()))
    } else {
        CheckResult::Error(format!(
            "Data directory {} is not writable",
            data_dir.display()
        ))
    }
}

/// Look for a stale daemon lockfile.
pub fn check_lock_file(data_dir: &std::path::Path, daemon_running: bool) -> CheckResult {
    let lock_path = data_dir.join("daemon.lock");
    if !lock_path.exists() {
        return CheckResult::Ok("Lock file: healthy (no stale lock)".into());
    }
    let pid_opt = std::fs::read_to_string(&lock_path)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok());
    let Some(pid) = pid_opt else {
        return CheckResult::Warn(format!(
            "Lock file exists but contains no valid PID ({})",
            lock_path.display()
        ));
    };
    let alive = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok();
    if !alive {
        return CheckResult::Warn(format!(
            "Stale lock file: PID {} is not running ({})",
            pid,
            lock_path.display()
        ));
    }
    if daemon_running {
        CheckResult::Ok(format!("Lock file: healthy (PID {} is running)", pid))
    } else {
        CheckResult::Warn(format!(
            "Lock file contains PID {} which is alive but /health failed",
            pid
        ))
    }
}

/// GET `/indexes` and extract the names array.
pub async fn fetch_index_names(client: &reqwest::Client, base: &str) -> Vec<String> {
    let list = client.get(format!("{}/indexes", base)).send().await;
    let list_body: serde_json::Value = match list {
        Ok(r) if r.status().is_success() => {
            r.json().await.unwrap_or_else(|_| serde_json::json!({}))
        }
        _ => serde_json::json!({"indexes": []}),
    };
    let empty_arr: Vec<serde_json::Value> = Vec::new();
    list_body
        .get("indexes")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty_arr)
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect()
}

/// Concurrently fetch `/indexes/:name/status` for each name and return sorted.
pub async fn fetch_index_statuses(
    client: &reqwest::Client,
    base: &str,
    names: &[String],
) -> Vec<(String, serde_json::Value)> {
    let mut joinset = tokio::task::JoinSet::new();
    for name in names {
        let n = name.clone();
        let url = format!("{}/indexes/{}/status", base, n);
        let c = client.clone();
        joinset.spawn(async move {
            let body: serde_json::Value = match c.get(&url).send().await {
                Ok(r) if r.status().is_success() => {
                    r.json().await.unwrap_or_else(|_| serde_json::json!({}))
                }
                _ => serde_json::json!({}),
            };
            (n, body)
        });
    }
    let mut per_index: Vec<(String, serde_json::Value)> = Vec::new();
    while let Some(j) = joinset.join_next().await {
        if let Ok(pair) = j {
            per_index.push(pair);
        }
    }
    per_index.sort_by(|a, b| a.0.cmp(&b.0));
    per_index
}

/// Build the indexes summary line.
pub fn summarize_indexes(total: usize, zero_count: usize) -> CheckResult {
    if zero_count == 0 {
        CheckResult::Ok(format!(
            "{} index{} registered, all have chunks",
            total,
            if total == 1 { "" } else { "es" }
        ))
    } else {
        CheckResult::Warn(format!(
            "{} index{} registered, {} {} no chunks yet:",
            total,
            if total == 1 { "" } else { "es" },
            zero_count,
            if zero_count == 1 { "has" } else { "have" }
        ))
    }
}

/// Print one indented line per index and record empty indexes.
pub fn print_index_breakdown(
    per_index: &[(String, serde_json::Value)],
    empty_indexes: &mut Vec<EmptyIndex>,
) {
    for (name, body) in per_index {
        let chunks = body
            .get("chunk_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let root = body
            .get("root_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let chunks_fmt = format_with_commas(chunks);
        if chunks == 0 {
            println!(
                "    {} {:<16} {:>12} chunks  {} — run `trusty-search index` to populate",
                "⚠".yellow(),
                name.bold(),
                chunks_fmt,
                root.dimmed()
            );
            empty_indexes.push(EmptyIndex {
                name: name.clone(),
                root_path: root,
            });
        } else {
            println!(
                "    {} {:<16} {:>12} chunks  {}",
                "✓".green(),
                name.bold(),
                chunks_fmt,
                root.dimmed()
            );
        }
    }
}

/// TCP-reachability probe for the daemon port.
pub async fn check_port_reachable(port: u16) -> CheckResult {
    if port_reachable("127.0.0.1", port).await {
        CheckResult::Ok(format!("Port {} is reachable", port))
    } else {
        CheckResult::Error(format!("Port {} is not reachable", port))
    }
}

/// Check whether `stderr.log` rotation is configured (issue #127).
///
/// Why: launchd writes the daemon's stderr to
/// `~/Library/Logs/trusty-search/stderr.log` and never truncates it, so the
/// file grows unbounded. `doctor --fix` can install a newsyslog config + a
/// daily LaunchAgent that caps it at 1 MB × 7 archives; this check tells the
/// operator whether that is already in place.
/// What: on macOS, returns Ok when a rotation config exists, Warn otherwise.
/// On other platforms returns Ok with a "not applicable" note — Linux service
/// managers (systemd/journald) handle log rotation themselves.
/// Test: `cargo test --workspace` — exercised by the doctor integration tests.
pub fn check_log_rotation() -> CheckResult {
    #[cfg(target_os = "macos")]
    {
        if super::log_rotation::rotation_configured() {
            CheckResult::Ok("Log rotation configured for stderr.log (1 MB × 7 archives)".into())
        } else {
            CheckResult::Warn(
                "stderr.log has no rotation policy — it will grow unbounded; \
                 run `trusty-search doctor --fix` to install one"
                    .into(),
            )
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        CheckResult::Ok(
            "Log rotation: handled by the platform service manager (systemd/journald)".into(),
        )
    }
}

// ── Python/MPS sidecar checks (epic #3524 slice 5) ──────────────────────────

/// Is the opt-in Python/MPS sidecar selected for this environment?
///
/// Why: the detailed checks below (`uv`, venv, launcher, device) are only
/// meaningful when `TRUSTY_EMBEDDER=python` is set — otherwise they'd report
/// spurious warnings about an embedder backend the operator never opted into.
/// What: `TRUSTY_EMBEDDER == "python"`.
/// Test: `python_embedder_enabled_*` in `tests.rs`.
pub fn python_embedder_enabled() -> bool {
    std::env::var("TRUSTY_EMBEDDER")
        .map(|v| v == "python")
        .unwrap_or(false)
}

/// Check `uv` presence + version (required to bootstrap the Python/MPS venv).
pub fn check_python_uv() -> CheckResult {
    match trusty_embedderd_py::bootstrap::locate_uv() {
        Ok(path) => {
            let version = std::process::Command::new(&path)
                .arg("--version")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_else(|| "version unknown".to_string());
            CheckResult::Ok(format!("uv: {} ({})", version, path.display()))
        }
        Err(e) => CheckResult::Error(format!(
            "uv not found: {e:#} — required to bootstrap the Python/MPS embedder \
             (TRUSTY_EMBEDDER=python); install uv or set TRUSTY_UV_BIN"
        )),
    }
}

/// Check venv presence + `.ready` sentinel + lockfile-hash-current state.
///
/// Why: mirrors (without duplicating the process-spawning parts of)
/// `trusty_embedderd_py::bootstrap`'s private `is_ready()` — a doctor check
/// must stay read-only and fast, never spawning `uv`/python itself.
/// What: `Ok` when the venv python exists AND `.ready` matches the current
/// lockfile hash; `Warn("not yet bootstrapped")` when neither the venv nor
/// `.ready` exist yet (first run); `Warn("stale or corrupt")` for every other
/// combination (a half-built venv, a hash mismatch after a lock update, or a
/// `.ready` sentinel with no matching venv python).
/// Test: `check_python_venv_*` in `tests.rs`.
pub fn check_python_venv(layout: &trusty_embedderd_py::VenvLayout) -> CheckResult {
    let want_hash = trusty_embedderd_py::bootstrap::lockfile_hash();
    let ready_path = layout.base.join(".ready");
    let ready_hash = std::fs::read_to_string(&ready_path).ok();
    let hash_current = ready_hash.as_deref().map(str::trim) == Some(want_hash.as_str());
    let venv_present = layout.venv_python.is_file();

    if venv_present && hash_current {
        CheckResult::Ok(format!(
            "Python/MPS venv: ready at {} (lockfile hash {} current)",
            layout.venv_dir.display(),
            want_hash
        ))
    } else if !ready_path.exists() && !venv_present {
        CheckResult::Warn(format!(
            "Python/MPS venv: not yet bootstrapped ({}) — will build on the next \
             `trusty-search start` with TRUSTY_EMBEDDER=python (one-time ~2-3 GB \
             download); run `trusty-search doctor --fix` to bootstrap it now",
            layout.venv_dir.display()
        ))
    } else {
        CheckResult::Warn(format!(
            "Python/MPS venv: stale or corrupt at {} (venv_present={venv_present}, \
             lockfile_hash_current={hash_current}) — run `trusty-search doctor --fix` \
             to rebuild it",
            layout.venv_dir.display()
        ))
    }
}

/// Check that the `trusty-embedderd-py` launcher binary is discoverable.
pub fn check_python_launcher() -> CheckResult {
    match trusty_embedderd_py::locate_launcher_binary() {
        Ok(p) => CheckResult::Ok(format!("trusty-embedderd-py launcher: {}", p.display())),
        Err(e) => CheckResult::Error(format!("trusty-embedderd-py launcher not found: {e:#}")),
    }
}

/// Informational note on which device would actually be selected.
///
/// Why (issue #3493 P1): the real device is resolved by `torch` INSIDE the
/// sidecar at startup — there is no way for this Rust-only, torch-free
/// doctor check to predict it honestly. Rather than guess (and risk another
/// #3493-style wrong prediction), this reports the *requested* policy and
/// points the operator at the real, wire-reported readback surfaced by
/// `GET /health` once the daemon is running (see
/// `crate::core::Embedder::resolved_provider_label` /
/// `StdioEmbedderClient::last_reported_device`).
pub fn check_python_device_note() -> CheckResult {
    let requested = std::env::var("TRUSTY_DEVICE").unwrap_or_else(|_| "auto".to_string());
    CheckResult::Ok(format!(
        "Python/MPS device: requested='{requested}' (mps if available, else cuda, else cpu) \
         — the actual device is resolved by torch at sidecar startup; check GET /health's \
         embedder_info.provider once the daemon is running for the real readback"
    ))
}

/// Remove a stale lock file and report the outcome.
pub fn fix_stale_lock(data_dir: &std::path::Path) {
    let lock_path = data_dir.join("daemon.lock");
    if lock_path.exists() {
        let pid_opt = std::fs::read_to_string(&lock_path)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());
        let stale = pid_opt
            .map(|pid| {
                nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_err()
            })
            .unwrap_or(true);
        if stale {
            match std::fs::remove_file(&lock_path) {
                Ok(()) => println!(
                    "  {} Removed stale lock file {}",
                    "✓".green(),
                    lock_path.display()
                ),
                Err(e) => println!(
                    "  {} Could not remove lock file {}: {e}",
                    "✗".red(),
                    lock_path.display()
                ),
            }
        } else {
            println!(
                "  {} Lock file is held by a live process — not removing",
                "⚠".yellow()
            );
        }
    }
}

#[cfg(test)]
mod tests;
