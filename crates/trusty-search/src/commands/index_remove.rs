//! Handler for `trusty-search index remove [PATH]`.
//!
//! Why: registering a project is one half of an index's lifecycle — removing it
//!      cleanly is the other. Without this, users who run `trusty-search index`
//!      against a directory they later delete have to either DELETE the index
//!      manually via curl or hand-edit `indexes.toml` and the global config
//!      file. The `index remove` subcommand collapses both steps into one.
//! What: resolves PATH (CLI arg > project auto-detection from CWD), finds the
//!       matching daemon-side index id via `GET /indexes/:id/status`, calls
//!       `DELETE /indexes/:id`, then drops the matching entry from
//!       `~/.config/trusty-search/config.yaml`.
//! Test: `index_remove_resolves_path_*` unit tests cover the path resolution;
//!       the HTTP round-trip is exercised end-to-end by the daemon integration
//!       tests.

use super::daemon_utils::daemon_base_url;
use crate::config::GlobalConfig;
use crate::detect::detect_project;
use anyhow::{bail, Context, Result};
use colored::Colorize;
use std::path::{Path, PathBuf};

/// Entry point for `trusty-search index remove [PATH]`.
///
/// Why: keep the CLI handler thin — all reusable resolution / HTTP logic lives
///      in helpers so the same flow can be invoked from a future MCP tool.
/// What: see module docs.
/// Test: `index_remove_resolves_path_*` below; HTTP path covered by integration
///       tests.
pub async fn handle_index_remove(cli_path: Option<PathBuf>) -> Result<()> {
    let target_path = resolve_target_path(cli_path)?;
    let base = daemon_base_url();
    crate::commands::daemon_guard::ensure_daemon_running_or_exit(&base).await?;

    let client = trusty_common::server::daemon_http_client()?;
    let (index_id, registered_path) = find_index_by_path(&client, &base, &target_path).await?;

    let delete_url = format!("{}/indexes/{}", base, index_id);
    match client.delete(&delete_url).send().await {
        Ok(resp) if resp.status().is_success() => {}
        Ok(resp) => bail!(
            "daemon returned {} for DELETE {}",
            resp.status(),
            delete_url
        ),
        Err(e) => bail!("could not reach daemon at {}: {e}", base),
    }

    // Drop the matching entry from the global YAML config so a future daemon
    // restart does not auto-rediscover the project the user just removed.
    // Best-effort: a config-file write failure should not undo the daemon-side
    // delete that already succeeded.
    match GlobalConfig::load() {
        Ok(mut cfg) => {
            let removed = cfg.remove_collection_by_path(&registered_path);
            if removed.is_some() {
                if let Err(e) = cfg.save() {
                    tracing::warn!("could not update global config after removal: {e:#}");
                }
            }
        }
        Err(e) => {
            tracing::warn!("could not load global config to remove entry: {e:#}");
        }
    }

    // Issue #767: also remove from the opt-in allowlist so the path cannot
    // be re-registered without explicit re-approval.  Best-effort.
    if let Err(e) = crate::allowlist::remove_from_allowlist(&registered_path, None) {
        tracing::warn!(
            path = %registered_path.display(),
            error = %e,
            "could not remove path from allowlist after index removal"
        );
    }

    println!(
        "{} Removed index {} ({})",
        "✓".green(),
        format!("\"{index_id}\"").bold(),
        registered_path.display()
    );
    Ok(())
}

/// Resolve the path argument: CLI value wins; otherwise auto-detect from CWD.
///
/// Why: same precedence rule as `search`, `watch`, and `reindex` — keeps the
///      mental model consistent across project-scoped commands.
/// What: returns the CLI path verbatim when present, otherwise walks upward
///       from CWD looking for `.git` / `.trusty-search` markers via
///       `detect::detect_project`. Falls back to the CWD itself if no marker
///       is found (mirrors the `Fallback` branch elsewhere).
/// Test: `index_remove_resolves_path_uses_cli` and
///       `index_remove_resolves_path_falls_back_to_cwd`.
fn resolve_target_path(cli_path: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = cli_path {
        return Ok(p);
    }
    let cwd = std::env::current_dir().context("could not resolve current directory")?;
    let ctx = detect_project(&cwd);
    Ok(ctx.root_path)
}

/// Find the daemon-side index id whose `root_path` matches `target`.
///
/// Why: the CLI takes a path, but the daemon's REST API is keyed by index id.
///      Walking the registry once and comparing canonicalised paths is the
///      least surprising way to bridge the two views.
/// What: lists all indexes, queries `/indexes/:id/status` for each, returns
///       the first id whose `root_path` canonicalises to the same value as
///       `target`. Errors out with a clear message when no match is found.
/// Test: side-effect-only at this level; covered by integration tests that
///       register an index and then exercise the remove subcommand.
async fn find_index_by_path(
    client: &reqwest::Client,
    base: &str,
    target: &Path,
) -> Result<(String, PathBuf)> {
    let list_url = format!("{base}/indexes");
    let list_body: serde_json::Value = client
        .get(&list_url)
        .send()
        .await
        .with_context(|| format!("could not reach daemon at {base}"))?
        .error_for_status()
        .with_context(|| format!("daemon error for {list_url}"))?
        .json()
        .await
        .context("could not parse /indexes response")?;
    let empty: Vec<serde_json::Value> = Vec::new();
    let ids: Vec<String> = list_body
        .get("indexes")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty)
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();

    let canonical_target = std::fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());

    for id in ids {
        let url = format!("{base}/indexes/{id}/status");
        let resp = match client.get(&url).send().await {
            Ok(r) if r.status().is_success() => r,
            _ => continue,
        };
        let body: serde_json::Value = match resp.json().await {
            Ok(b) => b,
            Err(_) => continue,
        };
        let root = body
            .get("root_path")
            .and_then(|v| v.as_str())
            .map(PathBuf::from);
        let Some(root) = root else {
            continue;
        };
        let canonical_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        if canonical_root == canonical_target {
            return Ok((id, root));
        }
    }
    bail!(
        "no index registered for path {}; run `trusty-search list` to see registered indexes",
        target.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn index_remove_resolves_path_uses_cli() {
        let p = resolve_target_path(Some(PathBuf::from("/explicit/path"))).unwrap();
        assert_eq!(p, PathBuf::from("/explicit/path"));
    }

    #[test]
    fn index_remove_resolves_path_falls_back_to_cwd() {
        // We don't assert a specific path (depends on the test runner CWD) but
        // the call must succeed and return a non-empty path.
        let p = resolve_target_path(None).unwrap();
        assert!(!p.as_os_str().is_empty());
    }

    #[test]
    fn index_remove_resolves_path_uses_detected_root() {
        // When CWD is inside a directory that has a `.git` marker, we should
        // detect that root rather than returning the CWD-as-is.
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let tmp = std::env::temp_dir().join(format!("trusty-idxrm-{pid}-{nanos}"));
        fs::create_dir_all(tmp.join(".git")).unwrap();
        let nested = tmp.join("a");
        fs::create_dir_all(&nested).unwrap();

        // Exercise the same helper detect uses by passing through detect_project
        // directly — we cannot safely change CWD inside a parallel test runner.
        let ctx = detect_project(&nested);
        assert_eq!(ctx.root_path, tmp);

        let _ = fs::remove_dir_all(&tmp);
    }
}
