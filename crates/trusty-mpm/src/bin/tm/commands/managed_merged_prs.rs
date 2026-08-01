//! Operator-facing rendering for the merged-PR reclaim pass (#2919).
//!
//! Why: split out of `managed.rs` to keep that file under the 500-SLOC
//! production cap. It is also a genuinely separate concern: the merged-PR
//! pass's refusals have a different shape from the orphan sweep's — a worktree
//! spared here because its pull request is still OPEN is not a dirty-tree skip
//! and must not be filed as one.
//! What: [`print_merged_pr_pass`], which reads the `merged_prs` object the
//! prune-worktrees route returns and prints reclaimed paths, the byte total,
//! and every re-check refusal.
//! Test: `merged_pr_pass_prints_nothing_for_a_null_body`,
//! `merged_pr_pass_reports_reclaimed_paths_and_bytes`,
//! `merged_pr_pass_surfaces_recheck_refusals`.

/// Render the `merged_prs` half of a prune-worktrees reply (#2919).
///
/// Why: a reclaim pass that deleted directories and said nothing is
/// indistinguishable from one that did nothing — the post-mortem's eighth
/// constraint ("report real before/after numbers … not merely a claim") exists
/// because that has happened here before.
/// What: prints one line per reclaimed path to stdout, then a summary naming
/// either the candidate count (dry run) or the reclaimed count and bytes, then
/// every re-check refusal to stderr. A `null`/absent object prints nothing —
/// that is the shape the route returns when the pass was not requested.
/// Test: `merged_pr_pass_prints_nothing_for_a_null_body`,
/// `merged_pr_pass_reports_reclaimed_paths_and_bytes`,
/// `merged_pr_pass_surfaces_recheck_refusals`.
pub(crate) fn print_merged_pr_pass(merged: Option<&serde_json::Value>, dry_run: bool) {
    let Some(merged) = merged.filter(|m| !m.is_null()) else {
        return;
    };
    if let Some(err) = merged.get("error").and_then(serde_json::Value::as_str) {
        eprintln!("merged-PR pass failed: {err} — nothing was reclaimed (#2919)");
        return;
    }
    let array = |key: &str| {
        merged
            .get(key)
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let reclaimed = array("removed");
    for p in &reclaimed {
        if let Some(s) = p.as_str() {
            println!("{s}");
        }
    }
    if dry_run {
        let candidates = merged
            .get("reclaimable")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let bytes = merged
            .get("reclaimable_bytes")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        println!("merged-PR pass: {candidates} worktree(s) / {bytes} byte(s) reclaimable (#2919)");
    } else {
        let bytes = merged
            .get("removed_bytes")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        println!(
            "merged-PR pass: reclaimed {} worktree(s), {bytes} byte(s) (#2919)",
            reclaimed.len()
        );
    }
    // A candidate the survey approved but the fresh re-check refused is the
    // most interesting line in the output: it means the workspace changed
    // underneath the pass, and staying quiet about it hides a near-miss.
    for r in &array("refused_at_recheck") {
        if let Some(s) = r.as_str() {
            eprintln!("  refused at re-check: {s}");
        }
    }
    for f in &array("removal_failed") {
        if let Some(s) = f.as_str() {
            eprintln!("  removal FAILED (still on disk): {s}");
        }
    }
}

/// `tm session prune --worktrees [--dry-run]` — remove orphaned per-session worktrees (#1840).
///
/// Why: sessions decommissioned before Fix 1a (#1840), or where
/// `git worktree remove` failed, leave stale `.worktrees/<session-id>/`
/// directories on disk. This verb lets operators clean them up safely — only
/// dirs without a corresponding active session are ever removed.
/// What: POSTs `/api/v1/sessions/managed/prune-worktrees` with
/// `{ dry_run, discard_dirty }`; prints one path per removed (or would-remove)
/// directory and a summary count, then prints every worktree the #4091
/// dirty-tree gate refused to touch (path + reason) to stderr so a skip is
/// never silent — a skipped worktree still holds work the operator needs to
/// deal with by hand.
/// Test: HTTP path covered by integration test; CLI parse by
/// `cli_parses_session_prune_worktrees` and
/// `cli_prune_worktrees_discard_dirty_is_opt_in`.
pub(crate) async fn session_prune_worktrees(
    client: &reqwest::Client,
    url: &str,
    dry_run: bool,
    discard_dirty: bool,
    merged_prs: bool,
) -> anyhow::Result<()> {
    let resp = client
        .post(format!("{url}/api/v1/sessions/managed/prune-worktrees"))
        .json(&serde_json::json!({
            "dry_run": dry_run,
            "discard_dirty": discard_dirty,
            // #2919: the merged-PR reclaim pass, off unless explicitly asked for.
            "merged_prs": merged_prs,
        }))
        .send()
        .await?;
    let body: serde_json::Value = resp.error_for_status()?.json().await?;
    let paths = body
        .get("paths")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut printed = 0usize;
    // Item 6 (#1845): non-string entries in the `paths` array are unexpected
    // (the server controls the format) but must not crash the CLI. Warn to
    // stderr so the operator is aware, rather than silently dropping the entry.
    for p in &paths {
        if let Some(s) = p.as_str() {
            println!("{s}");
            printed += 1;
        } else {
            eprintln!("warning: prune-worktrees: unexpected non-string path entry: {p}");
        }
    }
    let verb = if dry_run { "would remove" } else { "removed" };
    println!("{verb} {printed} orphaned worktree dir(s)");

    // #4091: surface every dirty-skip. These are worktrees that WOULD have
    // been reclaimed but still hold unsaved work; staying quiet about them is
    // how the sweep silently loses work.
    let skipped = body
        .get("skipped_dirty")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !skipped.is_empty() {
        eprintln!(
            "skipped {} worktree(s) holding uncommitted or unpushed work (#4091) — \
             review them by hand; `--discard-dirty` would destroy this work:",
            skipped.len()
        );
        for entry in &skipped {
            let path = entry
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("<unknown path>");
            let reason = entry
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("<no reason reported>");
            eprintln!("  {path}: {reason}");
        }
    }

    // #2919: the merged-PR pass reports separately, because its refusals have a
    // different shape — a worktree can be spared here for being on an OPEN PR,
    // which is not a dirty-tree skip and must not be filed as one.
    if merged_prs {
        print_merged_pr_pass(body.get("merged_prs"), dry_run);
    }
    Ok(())
}

#[cfg(test)]
#[path = "managed_merged_prs_tests.rs"]
mod managed_merged_prs_tests;
