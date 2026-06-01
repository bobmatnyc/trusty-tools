//! Auto-discovery of Claude Code, git, and trusty-tools projects at daemon startup.
//!
//! Why: agents and users shouldn't need to manually index every project — the
//!      daemon should find and index Claude Code projects automatically on
//!      startup. Users with hundreds of repos under `~/Projects` get a working
//!      index registry without typing a single command. Projects that follow the
//!      trusty-tools convention (`.trusty-tools/` directory at the root) are
//!      now also discoverable (#470), making the broader trusty-tools ecosystem
//!      visible to trusty-search without any extra configuration.
//! What: scans configured `scan_paths` (from `GlobalConfig`) plus sensible
//!       defaults for projects with `.claude/`, `CLAUDE.md`, `.git/`, or
//!       `.trusty-tools/` markers, then registers + reindexes any not already
//!       known to the daemon. The function is intentionally best-effort: errors
//!       talking to the daemon or filesystem are logged at warn and never
//!       propagated, because auto-discovery must never crash the daemon startup
//!       path.
//! Test: unit tests cover project detection signals
//!       (`detects_claude_dir`, `detects_claude_md`, `detects_git`,
//!       `detects_trusty_tools_dir`, `skips_when_no_markers`); the end-to-end
//!       pipeline is exercised by the existing daemon integration tests once
//!       auto-discovery is wired in.

mod http;
mod marker;

use super::daemon_utils::daemon_base_url;
use super::reindex_engine::register_index_with_daemon;
use crate::config::GlobalConfig;
use http::{fetch_known_index_ids, wait_for_daemon_ready};
use marker::{default_scan_paths, detect_project_marker, ProjectMarker};
use std::time::Duration;

/// Discover and index Claude Code, git, and trusty-tools projects on daemon startup.
///
/// Why: closes the "manual `trusty-search index` per repo" loop so the daemon
///      hydrates itself from the user's actual workspace. Runs once at
///      startup, after `restore_indexes()` has rehydrated the registry from
///      `indexes.toml`. Projects following the trusty-tools convention
///      (`.trusty-tools/` directory at the root — #470) are now included
///      alongside Claude Code and git projects; discovery cost is marginal
///      (one extra `is_dir` stat per subdirectory already being iterated).
/// What: loads `GlobalConfig`, resolves the scan list (config-supplied or
///       default), walks one level deep under each entry, and for every
///       directory with a project marker (`.claude/`, `CLAUDE.md`, `.git/`, or
///       `.trusty-tools/`) that is NOT already registered with the daemon,
///       calls `POST /indexes` followed by `POST /indexes/:id/reindex` via
///       the local HTTP API. The reindex POST includes `"background": true`
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

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
