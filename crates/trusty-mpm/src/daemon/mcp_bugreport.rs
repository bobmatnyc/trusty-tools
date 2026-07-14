//! Daemon-side implementation of the three Phase-3 bug-reporting MCP tools
//! (`list_recent_errors`, `preview_bug_report`, `report_bug`).
//!
//! Why: the MCP `StateBackend` (in `mcp_backend.rs`) must service these three
//! tools, but their bodies are the largest chunk of that file's original single
//! `impl OrchestratorBackend` block. Extracting them into their own sibling
//! module — mirroring the existing `mcp_session`/`mcp_console`/`mcp_project`
//! convention — keeps `mcp_backend.rs` comfortably under its frozen 500-SLOC-cap
//! allowlist budget (#2562 review) as new tool groups (like #2550's proxy
//! tools) are added. None of the three touch `DaemonState` — they read the
//! local `bug_report` stores directly — so, unlike the other sibling modules,
//! these take no `&Arc<DaemonState>` parameter.
//! What: three free async functions taking parsed MCP arguments and returning
//! the same JSON shapes the original inline methods returned, unchanged.
//! Test: `cargo test -p trusty-mpm daemon::mcp_backend` — the existing
//! `list_recent_errors_returns_valid_json`,
//! `preview_bug_report_unknown_fingerprint_errors`,
//! `report_bug_no_confirm_returns_preview_only`, and
//! `report_bug_confirm_no_token_graceful_failure` tests in `mcp_backend.rs`
//! exercise these bodies through the unchanged `StateBackend` trait delegates.

use serde_json::{Value, json};

/// Return recent captured errors across all known daemon stores
/// (`list_recent_errors` tool).
///
/// Why: aggregates errors from trusty-search, trusty-memory, trusty-analyze,
/// and trusty-mpm JSONL stores so the MCP user sees a unified view.
/// What: calls [`super::bug_report::aggregate_errors`] with `limit` capped at
/// 100, then serializes the aggregated error list as JSON.
/// Test: `mcp_backend::tests::list_recent_errors_returns_valid_json`.
pub async fn list_recent_errors(limit: u64) -> Result<Value, String> {
    let limit = (limit as usize).min(100);
    let errors = super::bug_report::aggregate_errors(limit);
    let summaries: Vec<Value> = errors
        .iter()
        .map(|e| {
            json!({
                "fingerprint": e.record.fingerprint,
                "crate_target": e.record.crate_target,
                "crate_version": e.record.crate_version,
                "summary": e.record.summary(),
                "occurrences": e.occurrences,
                "timestamp_secs": e.record.timestamp_secs,
                "os": e.record.os,
                "arch": e.record.arch,
            })
        })
        .collect();
    Ok(json!({
        "errors": summaries,
        "total": summaries.len(),
        "limit": limit,
    }))
}

/// Build and return the scrubbed issue preview for the given fingerprint
/// (`preview_bug_report` tool).
///
/// Why: the user must review the exact body that will be filed before
/// consenting. The preview IS the filed body — no transformation happens
/// between preview and filing.
/// What: calls [`super::bug_report::aggregate_errors`] to load errors, finds
/// the one with the matching fingerprint, runs
/// [`super::bug_report::build_preview`], and serializes the result. Returns an
/// error string when the fingerprint is not found.
/// Test: `mcp_backend::tests::preview_bug_report_unknown_fingerprint_errors`.
pub async fn preview_bug_report(fingerprint: &str) -> Result<Value, String> {
    let errors = super::bug_report::aggregate_errors(500);
    let found = errors
        .into_iter()
        .find(|e| e.record.fingerprint == fingerprint)
        .ok_or_else(|| {
            format!(
                "fingerprint `{fingerprint}` not found in local error stores; \
                 run list_recent_errors to see available fingerprints"
            )
        })?;
    let preview = super::bug_report::build_preview(&found);
    let changes: Vec<Value> = preview
        .scrub_changes
        .iter()
        .map(|c| json!({ "pattern": c.pattern, "hint": c.hint }))
        .collect();
    Ok(json!({
        "fingerprint": preview.fingerprint,
        "title": preview.title,
        "body": preview.body,
        "labels": preview.labels,
        "scrub_changes": changes,
        "note": "This is the exact content that will be filed. Call report_bug with confirm:true to file.",
    }))
}

/// File or increment a GitHub issue for the given fingerprint (`report_bug`
/// tool).
///
/// Why: the consent gate — nothing is filed unless `confirm` is `true`. When
/// `confirm` is false, returns the same preview as `preview_bug_report`. When
/// `true`, resolves the token via the full provider chain (Fix 1 / #498) and
/// calls [`super::bug_report::file_issue`].
///
/// Fixes implemented here:
///   - Fix 1 (#498, P0): uses `ResolvedProvider` (PAT → file → GitHub App →
///     NoToken) instead of the narrower `EnvFileTokenProvider`, so the GitHub
///     App path is reachable.
///   - Fix 3 (P2): the `RateLimitGuard` is checked before any GitHub call; a
///     blocked call returns `{ filed:false, rate_limited:true }`. After a
///     successful filing `record_filed` is called. State-file failures are
///     non-fatal (logged via `record_filed`'s own warning).
///
/// What: a `confirm:false` call is pure-preview (no network call). A
/// `confirm:true` call with no token returns a graceful failure with an
/// actionable message. A rate-limited call returns `{ filed:false,
/// rate_limited:true, note:… }`. A successful filing returns `{ filed,
/// deduped, issue_url, issue_number }`.
/// Test: `mcp_backend::tests::report_bug_no_confirm_returns_preview_only`,
/// `mcp_backend::tests::report_bug_confirm_no_token_graceful_failure`.
pub async fn report_bug(fingerprint: &str, confirm: bool) -> Result<Value, String> {
    // Step 1: load the error regardless of confirm — preview is always built.
    let errors = super::bug_report::aggregate_errors(500);
    let found = errors
        .into_iter()
        .find(|e| e.record.fingerprint == fingerprint)
        .ok_or_else(|| format!("fingerprint `{fingerprint}` not found; run list_recent_errors"))?;
    let preview = super::bug_report::build_preview(&found);

    if !confirm {
        // Preview-only path — nothing filed.
        let changes: Vec<Value> = preview
            .scrub_changes
            .iter()
            .map(|c| json!({ "pattern": c.pattern, "hint": c.hint }))
            .collect();
        return Ok(json!({
            "filed": false,
            "note": "confirm:false — preview only. Call with confirm:true to file.",
            "preview": {
                "fingerprint": preview.fingerprint,
                "title": preview.title,
                "body": preview.body,
                "labels": preview.labels,
                "scrub_changes": changes,
            }
        }));
    }

    // Fix 3 (P2): check the rate-limit guard before any GitHub call.
    let guard = super::bug_report::RateLimitGuard::production();
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let rl_decision = guard.check(fingerprint, now_secs);
    if !rl_decision.is_allowed() {
        return Ok(json!({
            "filed": false,
            "rate_limited": true,
            "note": rl_decision.block_reason(),
        }));
    }

    // Step 2: attempt to file via GitHub.
    // Fix 1 (P0): use the full resolution chain — PAT → file → GitHub App → NoToken.
    // Use spawn_blocking because the real reqwest client is blocking.
    let fp_owned = fingerprint.to_string();
    let provider = super::bug_report::ResolvedProvider;
    let result =
        tokio::task::spawn_blocking(move || super::bug_report::file_issue(&preview, &provider))
            .await
            .map_err(|e| format!("internal error: spawn_blocking failed: {e}"))?;

    match result {
        Ok(filing) => {
            // Fix 3 (P2): record the successful filing; write failures are
            // non-fatal — record_filed logs warnings internally.
            guard.record_filed(&fp_owned, now_secs);
            Ok(json!({
                "filed": filing.filed,
                "deduped": filing.deduped,
                "issue_url": filing.issue_url,
                "issue_number": filing.issue_number,
            }))
        }
        Err(super::bug_report::GithubFilingError::NoToken) => Ok(json!({
            "filed": false,
            "note": super::bug_report::GithubFilingError::NoToken.to_string(),
        })),
        Err(e) => Err(format!("GitHub filing failed: {e}")),
    }
}
