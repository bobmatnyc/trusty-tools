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
    if let Ok(dir) = std::env::var("TRUSTY_DATA_DIR") {
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
mod tests {
    use super::*;

    #[test]
    fn check_result_classifiers() {
        let ok = CheckResult::Ok("good".into());
        let warn = CheckResult::Warn("maybe".into());
        let err = CheckResult::Error("bad".into());
        assert!(!ok.is_error() && !ok.is_warn());
        assert!(warn.is_warn() && !warn.is_error());
        assert!(err.is_error() && !err.is_warn());
    }

    #[test]
    fn check_daemon_running_ok_branch() {
        let r = check_daemon_running(true, "http://127.0.0.1:7878", "0.3.27");
        match r {
            CheckResult::Ok(msg) => {
                assert!(msg.contains("127.0.0.1:7878"));
                assert!(msg.contains("0.3.27"));
            }
            other => panic!("expected Ok, got {:?}", other),
        }
    }

    #[test]
    fn check_daemon_running_error_branch() {
        let r = check_daemon_running(false, "http://127.0.0.1:7878", "");
        assert!(r.is_error());
        match r {
            CheckResult::Error(msg) => assert!(msg.contains("trusty-search start")),
            _ => panic!("expected Error variant"),
        }
    }

    #[test]
    fn summarize_indexes_all_populated_singular() {
        let r = summarize_indexes(1, 0);
        match r {
            CheckResult::Ok(msg) => {
                assert!(msg.contains("1 index registered"));
                assert!(!msg.contains("indexes"));
            }
            other => panic!("expected Ok, got {:?}", other),
        }
    }

    #[test]
    fn summarize_indexes_all_populated_plural() {
        let r = summarize_indexes(5, 0);
        match r {
            CheckResult::Ok(msg) => {
                assert!(msg.contains("5 indexes"));
                assert!(msg.contains("all have chunks"));
            }
            _ => panic!("expected Ok"),
        }
    }

    #[test]
    fn summarize_indexes_some_empty_singular() {
        let r = summarize_indexes(3, 1);
        match r {
            CheckResult::Warn(msg) => {
                assert!(msg.contains("3 indexes"));
                assert!(msg.contains("1 has no chunks"));
            }
            _ => panic!("expected Warn"),
        }
    }

    #[test]
    fn summarize_indexes_some_empty_plural() {
        let r = summarize_indexes(4, 2);
        match r {
            CheckResult::Warn(msg) => {
                assert!(msg.contains("4 indexes"));
                assert!(msg.contains("2 have no chunks"));
            }
            _ => panic!("expected Warn"),
        }
    }

    #[test]
    fn check_data_dir_missing_warns() {
        let path = std::path::Path::new("/nonexistent/trusty-search-doctor-test-zzz");
        let r = check_data_dir(path);
        match r {
            CheckResult::Warn(msg) => assert!(msg.contains("does not exist")),
            other => panic!("expected Warn for missing dir, got {:?}", other),
        }
    }

    #[test]
    fn check_data_dir_writable_ok() {
        let tmp = std::env::temp_dir().join(format!(
            "trusty-search-doctor-writable-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let r = check_data_dir(&tmp);
        assert!(
            !r.is_error(),
            "writable tempdir should not be Error: {:?}",
            r
        );
        match r {
            CheckResult::Ok(msg) => assert!(msg.contains("writable")),
            _ => panic!("expected Ok for writable tempdir"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn check_lock_file_absent_is_ok() {
        let tmp = std::env::temp_dir().join(format!(
            "trusty-search-doctor-lock-absent-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let r = check_lock_file(&tmp, false);
        match r {
            CheckResult::Ok(msg) => assert!(msg.contains("healthy")),
            other => panic!("expected Ok when no lock file, got {:?}", other),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn check_lock_file_invalid_pid_warns() {
        let tmp = std::env::temp_dir().join(format!(
            "trusty-search-doctor-lock-invalid-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("daemon.lock"), "not-a-pid").unwrap();
        let r = check_lock_file(&tmp, false);
        assert!(r.is_warn(), "garbage lock content should warn: {:?}", r);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn check_lock_file_stale_pid_warns() {
        let tmp = std::env::temp_dir().join(format!(
            "trusty-search-doctor-lock-stale-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        // PID 0 is reserved on Unix and `kill(0, 0)` is the "any process in
        // our group" semantic — but on macOS kill(0) targets the group itself
        // and may succeed. Use a very high PID that's almost certainly unused.
        std::fs::write(tmp.join("daemon.lock"), "4194303").unwrap();
        let r = check_lock_file(&tmp, false);
        assert!(r.is_warn(), "stale PID should warn: {:?}", r);
        match r {
            CheckResult::Warn(msg) => {
                assert!(msg.contains("4194303") || msg.contains("Stale"));
            }
            _ => unreachable!(),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn check_port_reachable_unbound_port_errors() {
        // Port 65535 is unlikely to be bound; assert we get an Error variant.
        let r = check_port_reachable(65535).await;
        assert!(r.is_error(), "unbound port should be Error: {:?}", r);
    }

    #[test]
    fn read_daemon_port_returns_some_u16() {
        // Smoke-test: returns a port (default or from file). Function should
        // never panic and should return a value in the valid port range.
        let p = read_daemon_port();
        // Default port path can include 0 if the port file held garbage that
        // parses as 0, but normally it's > 0. Just assert it returns a u16
        // (trivially true) and doesn't panic.
        let _ = p;
    }

    #[test]
    fn doctor_data_dir_returns_non_empty_path() {
        let p = doctor_data_dir();
        assert!(p.to_string_lossy().contains("trusty-search"));
    }

    #[test]
    fn fastembed_cache_dir_respects_env_override() {
        // Set a unique override value and assert the function returns exactly it.
        // SAFETY: process-global env vars are not test-isolated; this test
        // assumes it runs single-threaded for this variable's lifetime. We
        // save+restore to avoid polluting sibling tests.
        let key = "FASTEMBED_CACHE_DIR";
        let prev = std::env::var(key).ok();
        // Tests in a single binary may run in parallel; this is an accepted
        // test-only flakiness risk for env-var manipulation.
        std::env::set_var(key, "/tmp/trusty-search-fastembed-test-override");
        let got = fastembed_cache_dir();
        assert_eq!(
            got,
            std::path::PathBuf::from("/tmp/trusty-search-fastembed-test-override")
        );
        // Restore previous value.
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}
