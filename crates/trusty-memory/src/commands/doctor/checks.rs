//! Individual diagnostic check functions for `trusty-memory doctor`.
//!
//! Why: factored out of the monolithic `doctor.rs` so the per-check
//! implementations live in their own focused file, separate from the
//! audit data types and the command entry-points.
//! What: exports check functions called by [`super::handle_doctor`]:
//! [`check_fastembed_cache`], [`check_launchd_plist`] (macOS),
//! [`check_daemon_health`], and [`check_stale_palace_locks`]. Also exports
//! private helpers used by the tests in `mod.rs`.
//! Test: individual check helpers are exercised by unit tests in `mod.rs`;
//! the async `check_daemon_health` test verifies fallback-port behaviour.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::CheckResult;

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

/// Verify the HTTP daemon responds to `GET /health`.
///
/// Why (issue #475): the most direct test of "is the daemon running and
/// accepting requests". The `http_addr` discovery file can be stale after an
/// unclean shutdown (SIGKILL, power loss) — the daemon writes the file on
/// bind but the cleanup in `run_http_on` only runs on clean exit. A stale
/// file with an ephemeral or wrong port must not produce a false "daemon not
/// running" result when the daemon IS live on the default port.
/// What: reads the daemon address from `trusty_common::read_daemon_addr`;
/// if the recorded address is absent or responds with anything other than
/// HTTP 2xx, falls back to probing the well-known default port range
/// (`DEFAULT_HTTP_PORT` 7070..=7079) before reporting failure. `Pass` on
/// any 2xx from either probe, `Fail` on all connection errors or non-2xx.
/// Test: `check_daemon_health_fails_cleanly_with_stale_addr_and_no_listener`.
pub async fn check_daemon_health() -> CheckResult {
    let label = "HTTP daemon".to_string();

    // Build a reusable reqwest client for the probes below.
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(e) => return CheckResult::fail(label, format!("could not build HTTP client: {e}")),
    };

    // Primary probe: use the recorded addr file when present.
    let recorded_url = match trusty_common::read_daemon_addr("trusty-memory") {
        Ok(Some(addr)) => {
            // `read_daemon_addr` returns a bare `host:port`; prepend scheme.
            let base = if addr.starts_with("http://") || addr.starts_with("https://") {
                addr.clone()
            } else {
                format!("http://{addr}")
            };
            Some(base)
        }
        Ok(None) => None,
        Err(e) => {
            // Filesystem error reading the addr file — skip primary probe.
            tracing::debug!("doctor: could not read daemon addr file: {e:#}");
            None
        }
    };

    if let Some(ref base) = recorded_url {
        let url = format!("{base}/health");
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                return CheckResult::pass(label, format!("{} → {}", url, resp.status()));
            }
            Ok(resp) => {
                // Non-2xx from the recorded addr: continue to fallback.
                tracing::debug!(
                    "doctor: recorded addr {url} returned {}; trying fallback ports",
                    resp.status()
                );
            }
            Err(_) => {
                // Connection refused or timeout: recorded addr is stale.
                // Continue to default-port fallback below.
                tracing::debug!(
                    "doctor: recorded addr {url} unreachable (stale?); trying fallback ports"
                );
            }
        }
    }

    // Fallback probe (issue #475): when the addr file is absent or its
    // recorded address does not respond, walk the well-known port range
    // 7070..=7079 so a daemon on the default port is not missed. This is
    // the same range `bind_dynamic_port` prefers, so the fallback succeeds
    // in the common case where the daemon self-assigned port 7070 but the
    // addr file was left stale from a previous ephemeral-port run.
    for port in crate::DEFAULT_HTTP_PORT..=crate::DEFAULT_HTTP_PORT.saturating_add(9) {
        let url = format!("http://127.0.0.1:{port}/health");
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                let note = if recorded_url.is_some() {
                    format!(
                        "{url} → {} (addr file was stale — daemon is live on fallback port {port})",
                        resp.status()
                    )
                } else {
                    format!(
                        "{url} → {} (no addr file; found daemon on default port {port})",
                        resp.status()
                    )
                };
                return CheckResult::pass(label, note);
            }
            _ => continue,
        }
    }

    // All probes failed.
    if recorded_url.is_some() {
        CheckResult::fail(
            label,
            "recorded address unreachable and no daemon found on default ports 7070-7079 \
             — start with `trusty-memory service start`"
                .to_string(),
        )
    } else {
        CheckResult::fail(
            label,
            "no daemon address recorded and no daemon found on default ports 7070-7079 \
             — start with `trusty-memory service start`"
                .to_string(),
        )
    }
}

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
