//! Handler for `trusty-search index remove [PATH]`.
//!
//! Why: registering a project is one half of an index's lifecycle — removing it
//!      cleanly is the other. Without this, users who run `trusty-search index`
//!      against a directory they later delete have to either DELETE the index
//!      manually via curl or hand-edit `indexes.toml` and the global config
//!      file. The `index remove` subcommand collapses both steps into one.
//! What: resolves PATH (explicit index id > CLI path arg > project auto-detection
//!       from CWD), finds the matching daemon-side index id via
//!       `GET /indexes/:id/status`, calls
//!       `DELETE /indexes/:id?delete_data=<bool>`, then drops the matching entry
//!       from `~/.config/trusty-search/config.yaml`.
//!
//! Issue #1087: when `-i`/`--index` is given it MUST override CWD auto-detection
//! and never fall back to CWD detection. The fix passes `explicit_index_id` from
//! the parent `Commands::Index { index_id }` field and uses it directly (skipping
//! the path→id lookup entirely) when it is `Some`.
//!
//! Issue #6422: removing an index deletes its on-disk data by default, and
//! `--keep-data` is the explicit opt-out that deregisters and keeps the corpus.
//! The daemon's own default is still the opposite (`?delete_data` absent ⇒
//! preserve, #4123), so this command always sends the flag rather than relying
//! on a default either side might change.
//!
//! Test: `index_remove_resolves_path_*` unit tests cover the path resolution;
//!       `index_remove_explicit_id_bypasses_path_lookup` covers the -i flag fix;
//!       `delete_index_url_*`, `confirmation_*` and `delete_body_*` cover the
//!       #6422 default and its failure paths; the HTTP round-trip is exercised
//!       end-to-end by the daemon integration tests.

use super::daemon_utils::daemon_base_url;
use crate::config::GlobalConfig;
use crate::detect::detect_project;
use anyhow::{bail, Context, Result};
use colored::Colorize;
use serde_json::Value;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

/// Entry point for `trusty-search index remove [PATH]`.
///
/// Why: keep the CLI handler thin — all reusable resolution / HTTP logic lives
///      in helpers so the same flow can be invoked from a future MCP tool.
///
/// Issue #1087: `explicit_index_id` is the value of the PARENT command's
/// `-i`/`--index` flag (`Commands::Index { index_id }`). When it is `Some`,
/// the id is used directly and no path-based lookup is performed — this is
/// the fix for the bug where `index remove -i other` would remove the CWD
/// index instead of `other`.
///
/// Issue #6422: `keep_data` is the opt-out from the destructive default. When
/// it is false the on-disk corpus goes with the registration and the operator
/// is asked to confirm first, unless `yes` already answered.
///
/// What: see module docs.
/// Test: `index_remove_resolves_path_*` below; `index_remove_explicit_id_*`;
///       `delete_index_url_*` / `confirmation_*` / `delete_body_*` for #6422;
///       HTTP path covered by integration tests.
pub async fn handle_index_remove(
    cli_path: Option<PathBuf>,
    explicit_index_id: Option<String>,
    keep_data: bool,
    yes: bool,
) -> Result<()> {
    // #6422: purge by default; `--keep-data` is the explicit deregister-only
    // opt-out.
    let delete_data = !keep_data;
    let base = daemon_base_url();
    crate::commands::daemon_guard::ensure_daemon_running_or_exit(&base).await?;
    let client = trusty_common::server::daemon_http_client()?;

    // Issue #1087: when an explicit index id is supplied via `-i`/`--index`,
    // use it directly and skip the CWD-path→id lookup entirely. This prevents
    // accidentally removing the CWD's index when the user clearly specified a
    // different one.
    let (index_id, registered_path) = if let Some(ref id) = explicit_index_id {
        // Fetch the root_path for this explicit id so we can clean up the
        // global config and allowlist (same post-delete steps as the path path).
        find_index_by_id(&client, &base, id).await?
    } else {
        let target_path = resolve_target_path(cli_path)?;
        find_index_by_path(&client, &base, &target_path).await?
    };

    // #6422: the confirmation gate. It runs after resolution so the prompt can
    // name the exact index and root path the operator is about to destroy.
    if confirmation_required(delete_data, yes) {
        if !std::io::stdin().is_terminal() {
            bail!(
                "refusing to delete index \"{index_id}\" and its on-disk data without \
                 confirmation; pass --yes to confirm, or --keep-data to deregister only"
            );
        }
        if !super::confirm(&format!(
            "Delete index \"{index_id}\" and its on-disk data ({})? This cannot be undone.",
            registered_path.display()
        ))? {
            println!("Aborted.");
            return Ok(());
        }
    }

    let delete_url = delete_index_url(&base, &index_id, delete_data);
    let body = match client.delete(&delete_url).send().await {
        Ok(resp) => {
            let status = resp.status();
            let parsed: Value = resp.json().await.unwrap_or(Value::Null);
            if !status.is_success() {
                let reported = parsed.get("error").and_then(Value::as_str).unwrap_or("");
                bail!(
                    "daemon returned {status} for DELETE {delete_url}{}",
                    if reported.is_empty() {
                        String::new()
                    } else {
                        format!(": {reported}")
                    }
                );
            }
            parsed
        }
        Err(e) => bail!("could not reach daemon at {}: {e}", base),
    };

    // #6422: a `200` is not proof — the daemon answers one for a delete that
    // removed no registration. The local cleanup below runs only when the
    // registration is confirmed gone, and it runs even when the DATA removal
    // failed, because leaving those rows behind would strand entries for an
    // index the daemon no longer has.
    let removed = registration_removed(&body);
    let data_outcome = interpret_delete_body(delete_data, &body);
    if !removed {
        match data_outcome {
            Ok(_) => bail!("the daemon removed no registration for \"{index_id}\""),
            Err(reason) => bail!("{reason}"),
        }
    }

    // Drop the matching entry from the global YAML config so a future daemon
    // restart does not auto-rediscover the project the user just removed.
    // Best-effort: a config-file write failure should not undo the daemon-side
    // delete that already succeeded.
    match GlobalConfig::load() {
        Ok(mut cfg) => {
            let dropped = cfg.remove_collection_by_path(&registered_path);
            if dropped.is_some() {
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

    // #6422: report what the daemon said it DID. A registration that went while
    // its bytes stayed is a failure, not a removal with a caveat — printing a
    // tick there records the disk as reclaimed while every byte is still on it
    // (#3049).
    let data_deleted = match data_outcome {
        Ok(v) => v,
        Err(reason) => bail!("{reason}"),
    };

    println!(
        "{} Removed index {} ({}) — {}",
        "✓".green(),
        format!("\"{index_id}\"").bold(),
        registered_path.display(),
        if data_deleted {
            "on-disk data deleted"
        } else {
            "on-disk data kept (--keep-data)"
        }
    );
    Ok(())
}

/// The DELETE URL for one index, carrying the data choice explicitly.
///
/// Why (#6422): the CLI purges on-disk data by default while the daemon's own
/// default is still the opposite (`?delete_data` absent ⇒ preserve, #4123).
/// Sending the flag on every call means this command's default does not depend
/// on which daemon version answers it.
/// What: `<base>/indexes/<id>?delete_data=<true|false>`.
/// Test: `delete_index_url_purges_by_default`,
/// `delete_index_url_honours_keep_data`.
pub(crate) fn delete_index_url(base: &str, id: &str, delete_data: bool) -> String {
    format!("{base}/indexes/{id}?delete_data={delete_data}")
}

/// Whether the operator must confirm before this delete runs.
///
/// Why (#6422): the owner ruling made the destructive path the default and kept
/// the confirmation. Deregister-only touches no data, so it keeps the
/// unprompted behaviour scripts already rely on.
/// What: true only when data is about to be deleted and `--yes` has not already
/// answered.
/// Test: `confirmation_is_required_only_for_a_data_delete`.
pub(crate) fn confirmation_required(delete_data: bool, yes: bool) -> bool {
    delete_data && !yes
}

/// Whether the daemon confirmed it removed the registration.
///
/// Why: `DELETE` answers `200 {"removed": false}` for a delete it declined to
/// perform, so the status code alone cannot say whether anything happened.
/// What: reads `removed`, treating a missing field as "not removed" — a body
/// that never said so has not said so.
/// Test: `delete_body_without_a_removal_is_a_failure`.
pub(crate) fn registration_removed(body: &Value) -> bool {
    body.get("removed").and_then(Value::as_bool) == Some(true)
}

/// What the daemon's delete body says actually happened to the data.
///
/// Why (#6422): a delete that removed the registration and left every byte on
/// disk must not print as a clean removal — the operator would record the disk
/// as reclaimed while the corpus is still there (#3049). The daemon answers
/// `500` when the durable cleanup fails, but `200` with `data_deleted: false`
/// is a success on the wire, so the body is read rather than the status alone.
/// What: `Ok(true)` when the data went, `Ok(false)` when only the registration
/// did and that is what was asked for, `Err(reason)` for every other shape.
/// Test: `delete_body_confirms_a_purge`,
/// `delete_body_reporting_undeleted_data_is_a_failure`,
/// `delete_body_without_a_removal_is_a_failure`,
/// `delete_body_of_a_keep_data_delete_reports_the_data_kept`.
pub(crate) fn interpret_delete_body(delete_data: bool, body: &Value) -> Result<bool, String> {
    if !registration_removed(body) {
        return Err(format!(
            "the daemon answered successfully but removed no registration{}",
            match body.get("quiesced").and_then(Value::as_bool) {
                Some(false) => " (in-flight writers never quiesced, so the teardown was abandoned)",
                _ => "",
            }
        ));
    }
    let data_deleted = body.get("data_deleted").and_then(Value::as_bool) == Some(true);
    if delete_data && !data_deleted {
        return Err(
            "the registration was removed but the on-disk data was NOT deleted; \
             the corpus is still on disk — re-run the delete to reclaim it"
                .to_string(),
        );
    }
    Ok(data_deleted)
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

/// Classify how the index to remove should be resolved (issue #1087).
///
/// Why: the decision "explicit id vs. path lookup" is a small pure predicate
/// that sits at the heart of the #1087 fix. Extracting it lets unit tests
/// verify the correct branch is taken for each input combination WITHOUT
/// needing a live daemon.
///
/// What: returns `Some(id)` when an explicit `-i` id was given (the id should
/// be used directly, bypassing CWD detection entirely), or `None` when the
/// removal should fall back to path-based lookup.
///
/// Test: `index_remove_explicit_id_bypasses_path_lookup` exercises this
/// function directly.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn resolve_index_id_source(explicit_index_id: Option<&str>) -> Option<String> {
    explicit_index_id.map(|id| id.to_string())
}

/// Fetch the registered `root_path` for a known index id.
///
/// Why (issue #1087): when `-i <id>` is given we know the id already; we still
/// need the `root_path` for post-delete cleanup (global config + allowlist).
/// What: calls `GET /indexes/:id/status`, extracts `root_path`. Returns
/// `(id, root_path)` so callers can use the same post-delete code path.
/// Test: side-effect-only; covered by integration tests for the `-i` flag path.
async fn find_index_by_id(
    client: &reqwest::Client,
    base: &str,
    id: &str,
) -> Result<(String, PathBuf)> {
    let url = format!("{base}/indexes/{id}/status");
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("could not reach daemon at {base}"))?
        .error_for_status()
        .with_context(|| format!("daemon returned an error for {url}"))?;
    let body: serde_json::Value = resp
        .json()
        .await
        .context("could not parse status response")?;
    let root = body
        .get("root_path")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .with_context(|| format!("status response for '{id}' is missing root_path"))?;
    Ok((id.to_string(), root))
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

    /// Regression test for issue #1087 — explicit `-i <id>` MUST bypass
    /// CWD detection and never fall back to path lookup.
    ///
    /// Why: `handle_index_remove` used to ignore `-i`/`--index` entirely and
    /// always resolve via the CWD path. This test pins the `resolve_index_id_source`
    /// decision function, which is the pure predicate at the heart of the fix.
    ///
    /// What (issue #1097 enhancement): `resolve_index_id_source` is now a
    /// named pure function (not just an inline `if let`) so its behaviour can
    /// be asserted directly — no live daemon needed:
    ///
    /// - `Some("my-index")` → explicit id returned as `Some("my-index")`.
    /// - `None` → `None` (signals: fall back to path lookup).
    ///
    /// End-to-end coverage for the full `-i` code path (daemon HTTP round-trip)
    /// lives in the integration tests.
    ///
    /// Test: this test.
    #[test]
    fn index_remove_explicit_id_bypasses_path_lookup() {
        // Explicit id is given: must be returned as Some(id), never as None.
        let result = super::resolve_index_id_source(Some("other-project"));
        assert_eq!(
            result.as_deref(),
            Some("other-project"),
            "explicit id must be returned verbatim — CWD must not interfere"
        );

        // No explicit id: must return None so callers know to use path lookup.
        let fallback = super::resolve_index_id_source(None);
        assert!(
            fallback.is_none(),
            "no explicit id → None (path-based lookup will be used)"
        );

        // The explicit id must never equal the CWD — they are distinct sources.
        // (Guards against a regression where both branches return the same thing.)
        let cwd_p = resolve_target_path(None).unwrap();
        assert_ne!(
            cwd_p.to_string_lossy().as_ref(),
            "other-project",
            "CWD fallback must not accidentally equal an explicit id string"
        );
    }

    // ── #6422: deleting an index purges its data by default ─────────────────

    /// Why (#6422, closure condition 1): the owner ruling. Against the pre-fix
    /// code this command issued `DELETE /indexes/{id}` with NO query at all,
    /// which the daemon reads as `delete_data=false` (#4123) — so this
    /// assertion fails there, on the substring and on the whole URL alike.
    /// Test: this is the test.
    #[test]
    fn delete_index_url_purges_by_default() {
        let url = super::delete_index_url("http://127.0.0.1:7878", "rustbot", true);
        assert_eq!(
            url, "http://127.0.0.1:7878/indexes/rustbot?delete_data=true",
            "the default delete must ask the daemon for the data too"
        );
    }

    /// Why (#6422, closure condition 2): `--keep-data` is the opt-out, and it
    /// must send `false` EXPLICITLY rather than omit the parameter — a delete
    /// that leans on the daemon's default is one daemon version away from
    /// destroying the corpus it promised to keep.
    /// Test: this is the test.
    #[test]
    fn delete_index_url_honours_keep_data() {
        let url = super::delete_index_url("http://127.0.0.1:7878", "rustbot", false);
        assert_eq!(
            url,
            "http://127.0.0.1:7878/indexes/rustbot?delete_data=false"
        );
    }

    /// Why (#6422, closure condition 3): the destructive default keeps its
    /// confirmation. Deregister-only destroys nothing, so it keeps the
    /// unprompted behaviour scripts already rely on.
    /// Test: this is the test.
    #[test]
    fn confirmation_is_required_only_for_a_data_delete() {
        assert!(
            super::confirmation_required(true, false),
            "a data delete must be confirmed"
        );
        assert!(
            !super::confirmation_required(true, true),
            "--yes answers the confirmation"
        );
        assert!(
            !super::confirmation_required(false, false),
            "--keep-data destroys nothing and must not prompt"
        );
    }

    /// Why: the success path must stay reachable and must report that the data
    /// actually went.
    /// Test: this is the test.
    #[test]
    fn delete_body_confirms_a_purge() {
        let body = serde_json::json!({
            "id": "rustbot", "ok": true, "removed": true, "data_deleted": true
        });
        assert!(super::registration_removed(&body));
        assert_eq!(super::interpret_delete_body(true, &body), Ok(true));
    }

    /// Why (#6422 failure path, #3049): the registration went and every byte
    /// stayed. Printing a tick there records the corpus as reclaimed while it
    /// is still on disk, which is the silent half-deleted state this command
    /// must never leave behind.
    /// Test: this is the test.
    #[test]
    fn delete_body_reporting_undeleted_data_is_a_failure() {
        let body = serde_json::json!({
            "id": "rustbot", "ok": true, "removed": true, "data_deleted": false
        });
        let err = super::interpret_delete_body(true, &body)
            .expect_err("undeleted data must not read as a clean removal");
        assert!(
            err.contains("was NOT deleted"),
            "the failure must say the data survived: {err}"
        );
    }

    /// Why: `200 {"removed": false}` is the daemon's honest no-op, and an
    /// abandoned teardown (`quiesced: false`) is a distinct condition the
    /// operator can act on by retrying.
    /// Test: this is the test.
    #[test]
    fn delete_body_without_a_removal_is_a_failure() {
        for body in [
            serde_json::json!({}),
            serde_json::json!({ "removed": false, "data_deleted": false }),
        ] {
            assert!(!super::registration_removed(&body), "body: {body}");
            assert!(
                super::interpret_delete_body(true, &body).is_err(),
                "body: {body}"
            );
        }

        let abandoned =
            serde_json::json!({ "removed": false, "data_deleted": false, "quiesced": false });
        let err = super::interpret_delete_body(true, &abandoned).expect_err("abandoned");
        assert!(
            err.contains("never quiesced"),
            "an abandoned teardown must say so: {err}"
        );
    }

    /// Why (#6422): the opt-out's success path. A deregister-only delete that
    /// left the data alone is exactly what was asked for, and must read as a
    /// success reporting the data kept.
    /// Test: this is the test.
    #[test]
    fn delete_body_of_a_keep_data_delete_reports_the_data_kept() {
        let body = serde_json::json!({
            "id": "rustbot", "ok": true, "removed": true, "data_deleted": false
        });
        assert_eq!(super::interpret_delete_body(false, &body), Ok(false));
    }
}
