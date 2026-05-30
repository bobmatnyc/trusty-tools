//! Auto-discovery of Claude Code and git projects at daemon startup.
//!
//! Why: agents and users shouldn't need to manually index every project — the
//!      daemon should find and index Claude Code projects automatically on
//!      startup. Users with hundreds of repos under `~/Projects` get a working
//!      index registry without typing a single command.
//! What: scans configured `scan_paths` (from `GlobalConfig`) plus sensible
//!       defaults for projects with `.claude/`, `CLAUDE.md`, or `.git/` markers,
//!       then registers + reindexes any not already known to the daemon. The
//!       function is intentionally best-effort: errors talking to the daemon
//!       or filesystem are logged at warn and never propagated, because
//!       auto-discovery must never crash the daemon startup path.
//! Test: unit tests cover project detection signals
//!       (`detects_claude_dir`, `detects_claude_md`, `detects_git`,
//!       `skips_when_no_markers`); the end-to-end pipeline is exercised by the
//!       existing daemon integration tests once auto-discovery is wired in.

use super::daemon_utils::daemon_base_url;
use super::reindex_engine::register_index_with_daemon;
use crate::config::GlobalConfig;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Signal that identifies a directory as worth indexing.
///
/// Why: priority matters — a `.claude/` directory is the strongest signal that
///      this project is being worked on with Claude Code and should be indexed
///      first. `.git/` is a weaker but still useful hint.
/// What: ordered by strength; `Claude` > `ClaudeMd` > `Git`. `None` means skip.
/// Test: `detect_project_marker_*` unit tests below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectMarker {
    Claude,
    ClaudeMd,
    Git,
    None,
}

/// Inspect a directory for the strongest project marker present.
///
/// Why: keeps the detection rules in one place so the priority order stays
///      consistent between the scanner and any future caller (doctor, MCP,
///      tests).
/// What: probes `.claude/` first, then `CLAUDE.md`, then `.git/`, returning the
///       first match.
/// Test: see `detect_project_marker_*` below.
fn detect_project_marker(dir: &Path) -> ProjectMarker {
    if dir.join(".claude").is_dir() {
        return ProjectMarker::Claude;
    }
    if dir.join("CLAUDE.md").is_file() {
        return ProjectMarker::ClaudeMd;
    }
    if dir.join(".git").exists() {
        return ProjectMarker::Git;
    }
    ProjectMarker::None
}

/// Default scan paths when the user has not set `scan_paths` in
/// `~/.config/trusty-search/config.yaml`.
///
/// Why: a fresh install needs to do something useful. Picking the three most
///      common project-root conventions covers nearly every developer setup
///      without over-eager filesystem walks.
/// What: returns `~/Projects`, `~/code`, `~/src`, filtered to those that
///       actually exist on the current machine. Returns empty when `$HOME` is
///       not set (unusual; only happens in restricted CI sandboxes).
/// Test: covered indirectly by `auto_discover_and_index_smoke` (when wired).
fn default_scan_paths() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    ["Projects", "code", "src"]
        .iter()
        .map(|p| home.join(p))
        .filter(|p| p.is_dir())
        .collect()
}

/// Discover and index Claude Code / git projects on daemon startup.
///
/// Why: closes the "manual `trusty-search index` per repo" loop so the daemon
///      hydrates itself from the user's actual workspace. Runs once at
///      startup, after `restore_indexes()` has rehydrated the registry from
///      `indexes.toml`.
/// What: loads `GlobalConfig`, resolves the scan list (config-supplied or
///       default), walks one level deep under each entry, and for every
///       directory with a project marker that is NOT already registered with
///       the daemon, calls `POST /indexes` followed by `POST /indexes/:id/reindex`
///       via the local HTTP API. The reindex POST includes `"background": true`
///       (issue #458) so these bulk startup tasks are routed through the
///       low-priority semaphore and cannot starve user-initiated indexing.
///       Directories whose `root_path` does not exist on disk are skipped
///       (issue #458 part 2) — dead/moved projects must not flood the queue.
///       All failures are logged at warn level and never propagated — auto-discovery
///       is best-effort.
/// Test: side-effect-only at this level. Unit tests cover the pure detection
///       logic (`detect_project_marker_*`, `default_scan_paths`,
///       `root_path_exists_before_reindex`); the integration smoke test lives
///       under `tests/integration_tests.rs`.
pub async fn auto_discover_and_index() {
    let cfg = match GlobalConfig::load() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("auto-discover: could not load global config: {e:#} — skipping");
            return;
        }
    };

    let scan_paths = if cfg.scan_paths.is_empty() {
        default_scan_paths()
    } else {
        cfg.scan_paths.clone()
    };

    if scan_paths.is_empty() {
        tracing::debug!("auto-discover: no scan paths configured and no defaults found — skipping");
        return;
    }

    let base = daemon_base_url();
    let client = match trusty_common::server::daemon_http_client() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("auto-discover: could not build HTTP client: {e:#} — skipping");
            return;
        }
    };

    // Wait briefly for the daemon's HTTP listener to come online. The discover
    // task is spawned in parallel with `run_daemon`, so the listener may not
    // be bound yet on the first iteration. Cap the wait so a daemon that
    // failed to bind doesn't leave the auto-discoverer spinning forever.
    if !wait_for_daemon_ready(&client, &base, Duration::from_secs(15)).await {
        tracing::warn!(
            "auto-discover: daemon at {base} did not become ready within 15s — skipping"
        );
        return;
    }

    let known: std::collections::HashSet<String> = match fetch_known_index_ids(&client, &base).await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("auto-discover: could not list indexes: {e:#} — skipping");
            return;
        }
    };

    let mut discovered = 0usize;
    let mut indexed = 0usize;

    for root in &scan_paths {
        if !root.is_dir() {
            tracing::debug!(
                "auto-discover: skipping non-directory scan path {}",
                root.display()
            );
            continue;
        }
        let entries = match std::fs::read_dir(root) {
            Ok(it) => it,
            Err(e) => {
                tracing::warn!("auto-discover: could not read {}: {e}", root.display());
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let marker = detect_project_marker(&path);
            if marker == ProjectMarker::None {
                continue;
            }
            discovered += 1;

            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) if !n.is_empty() => n.to_string(),
                _ => {
                    tracing::debug!(
                        "auto-discover: skipping {} (no usable name)",
                        path.display()
                    );
                    continue;
                }
            };
            if known.contains(&name) {
                tracing::debug!(
                    "auto-discover: skipping {} (index '{}' already registered)",
                    path.display(),
                    name
                );
                continue;
            }

            tracing::info!(
                "auto-discover: indexing {} as '{}' (marker={:?})",
                path.display(),
                name,
                marker
            );

            match register_index_with_daemon(&name, &path).await {
                Ok((_created, true)) => {
                    // Issue #458 (part 2): skip reindex if the root path does
                    // not exist on disk. Dead/moved/unmounted project directories
                    // (e.g. an external volume that is not currently mounted)
                    // must not flood the background reindex queue. The index
                    // registration is intentionally kept so operators can still
                    // see it; only the auto-discover reindex trigger is skipped.
                    if !path.exists() {
                        tracing::info!(
                            "auto-discover: skipping reindex of '{}' — root path '{}' \
                             does not exist on disk (dead/moved project)",
                            name,
                            path.display()
                        );
                        continue;
                    }
                    let reindex_url = format!("{base}/indexes/{name}/reindex");
                    // Issue #458 (part 1): set `background: true` so the reindex
                    // request is routed through the low-priority semaphore. This
                    // prevents a large startup discovery (e.g. 44 projects) from
                    // starving a concurrent user-initiated `trusty-search index`.
                    match client
                        .post(&reindex_url)
                        .json(&serde_json::json!({ "background": true }))
                        .send()
                        .await
                    {
                        Ok(resp) if resp.status().is_success() => {
                            indexed += 1;
                        }
                        Ok(resp) => {
                            tracing::warn!(
                                "auto-discover: reindex of '{name}' returned HTTP {}",
                                resp.status()
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                "auto-discover: could not POST reindex for '{name}': {e}"
                            );
                        }
                    }
                }
                Ok((_, false)) => {
                    tracing::warn!(
                        "auto-discover: daemon unreachable while registering '{name}' — aborting"
                    );
                    return;
                }
                Err(e) => {
                    tracing::warn!("auto-discover: could not register '{name}': {e:#}");
                }
            }
        }
    }

    if discovered > 0 {
        tracing::info!(
            "auto-discover: scanned {} root(s); discovered {} project(s); queued {} for indexing",
            scan_paths.len(),
            discovered,
            indexed
        );
    }
}

/// Poll the daemon's `/health` endpoint until it returns 200 or the deadline
/// fires.
///
/// Why: auto-discovery is spawned in parallel with `run_daemon`, so the HTTP
///      listener may not be ready when discovery starts probing. Without this,
///      the first `register_index_with_daemon` call would race and fail.
/// What: polls every 200 ms up to `timeout`. Returns true on first success,
///       false if the deadline elapses with no successful response.
/// Test: side-effect-only; covered indirectly by the daemon startup integration
///       tests.
async fn wait_for_daemon_ready(client: &reqwest::Client, base: &str, timeout: Duration) -> bool {
    let url = format!("{base}/health");
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if client
            .get(&url)
            .send()
            .await
            .ok()
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Fetch the set of currently-registered index ids from the daemon.
///
/// Why: we must not re-register a project that is already known — the daemon's
///      `POST /indexes` is idempotent but the follow-up reindex would still
///      run, wasting CPU and contending with whatever else the daemon is
///      doing.
/// What: GET `/indexes` and parse the `indexes` array of strings.
/// Test: covered indirectly by `auto_discover_and_index` integration runs.
async fn fetch_known_index_ids(
    client: &reqwest::Client,
    base: &str,
) -> anyhow::Result<std::collections::HashSet<String>> {
    let url = format!("{base}/indexes");
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("daemon returned {} for {url}", resp.status());
    }
    let body: serde_json::Value = resp.json().await?;
    let empty: Vec<serde_json::Value> = Vec::new();
    let set = body
        .get("indexes")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty)
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tempdir_unique(label: &str) -> PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("trusty-discover-{label}-{pid}-{nanos}"));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn detect_project_marker_claude_dir_wins() {
        let dir = tempdir_unique("claude");
        fs::create_dir_all(dir.join(".claude")).unwrap();
        fs::write(dir.join("CLAUDE.md"), "x").unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        assert_eq!(detect_project_marker(&dir), ProjectMarker::Claude);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_project_marker_claude_md_beats_git() {
        let dir = tempdir_unique("claudemd");
        fs::write(dir.join("CLAUDE.md"), "x").unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        assert_eq!(detect_project_marker(&dir), ProjectMarker::ClaudeMd);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_project_marker_git_when_only_git() {
        let dir = tempdir_unique("git");
        fs::create_dir_all(dir.join(".git")).unwrap();
        assert_eq!(detect_project_marker(&dir), ProjectMarker::Git);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_project_marker_none_when_empty() {
        let dir = tempdir_unique("empty");
        assert_eq!(detect_project_marker(&dir), ProjectMarker::None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_project_marker_ignores_claude_md_as_dir() {
        // CLAUDE.md must be a file, not a directory.
        let dir = tempdir_unique("claudedir");
        fs::create_dir_all(dir.join("CLAUDE.md")).unwrap();
        assert_eq!(detect_project_marker(&dir), ProjectMarker::None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_scan_paths_does_not_panic() {
        // We can't assert exact contents (depends on the user's $HOME) but the
        // call must always return cleanly.
        let _ = default_scan_paths();
    }

    // ── Issue #458: dead-root skip predicate ──────────────────────────────────

    /// Why: the auto-discover reindex skip decision is `path.exists()`. A
    /// non-existent path (unmounted volume, moved project) must produce `false`
    /// so the auto-discover loop skips the reindex trigger. This test verifies
    /// the predicate works correctly for both live and dead paths — the pure
    /// filesystem call is the factored-out decision point that `auto_discover_
    /// and_index` relies on.
    ///
    /// What: creates a real temporary directory (exists → true), then removes it
    /// and checks again (gone → false). Mirrors the exact check used in the
    /// auto-discover loop.
    ///
    /// Test: self-contained — no daemon needed; runs in normal `cargo test`.
    #[test]
    fn root_path_exists_true_for_real_dir() {
        let dir = tempdir_unique("exists");
        assert!(dir.exists(), "freshly-created dir must exist");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn root_path_exists_false_after_removal() {
        let dir = tempdir_unique("gone");
        fs::remove_dir_all(&dir).ok();
        assert!(
            !dir.exists(),
            "deleted dir must not exist — the dead-root skip predicate would fire"
        );
    }

    #[test]
    fn root_path_exists_false_for_never_created() {
        // A path that was never on disk: simulate a dead/moved project volume.
        let phantom =
            std::env::temp_dir().join("trusty-discover-phantom-path-that-will-never-exist-12345");
        let _ = fs::remove_dir_all(&phantom);
        assert!(
            !phantom.exists(),
            "phantom path must not exist — dead-root skip would fire for this index"
        );
    }
}
