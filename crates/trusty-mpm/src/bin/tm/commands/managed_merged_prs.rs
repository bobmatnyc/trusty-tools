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
/// [`diagnostic_lines`] to stderr. A `null`/absent object prints nothing —
/// that is the shape the route returns when the pass was not requested.
/// Test: `merged_pr_pass_prints_nothing_for_a_null_body`,
/// `merged_pr_pass_reports_reclaimed_paths_and_bytes`,
/// `merged_pr_pass_surfaces_recheck_refusals`,
/// `merged_pr_pass_surfaces_an_agent_owned_skip`.
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
        // #2919: the byte figure is a sum over the MEASURED subset, so it never
        // appears without its measured-of-total qualifier — a single 17.8 GiB
        // worktree can eat the whole measurement budget and leave the rest of
        // the reclaimable set uncounted.
        let measured = merged
            .get("reclaimable_measured")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        // #6561: the classification clause rides on the SAME line as the
        // reclaimable count, so "0 reclaimable" can never be read without
        // whether anything was actually classified.
        let unclassified = classification_clause(merged);
        println!(
            "merged-PR pass: {candidates} worktree(s) reclaimable; {bytes} byte(s) \
             across {measured} of {candidates} measured{unclassified} (#2919)"
        );
    } else {
        let bytes = merged
            .get("removed_bytes")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let unclassified = classification_clause(merged);
        println!(
            "merged-PR pass: reclaimed {} worktree(s), {bytes} byte(s){unclassified} (#2919)",
            reclaimed.len()
        );
    }
    for line in diagnostic_lines(merged) {
        eprintln!("{line}");
    }
}

/// What the pass could NOT classify, as a clause for the summary line (#6561).
///
/// Why: a reclaimable count of zero has two causes that read identically —
/// nothing had landed, or nothing could be looked up. Live on 2026-09-02 the
/// second held for all 261 registered worktrees (the daemon inherits neither
/// `GH_TOKEN` nor `GH_CONFIG_DIR`, so `gh pr list` exited 4) and the pass
/// printed `0 worktree(s) reclaimable; 0 byte(s) across 0 of 0 measured` with
/// no hint of it. These three counts are what tell them apart, so they share
/// the reclaimable count's line rather than sitting in a stderr diagnostic the
/// operator may not have kept.
/// What: `"; could not classify: …"` naming any non-zero
/// `lookup_failed` (with the reason `gh` gave), `pr_state_unknown`, and
/// `not_inspected`. All three zero — the healthy case — returns an empty
/// string, so a clean run's line is unchanged.
/// Test: `merged_pr_pass_names_a_failed_lookup_beside_the_reclaimable_count`,
/// `merged_pr_pass_adds_no_clause_when_everything_classified`.
fn classification_clause(merged: &serde_json::Value) -> String {
    let count = |key: &str| {
        merged
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    let (failed, unknown, skipped) = (
        count("lookup_failed"),
        count("pr_state_unknown"),
        count("not_inspected"),
    );
    let mut parts = Vec::new();
    if failed > 0 {
        let reason = merged
            .get("lookup_failure")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("no reason given");
        parts.push(format!("{failed} pull-request lookup(s) FAILED ({reason})"));
    }
    if unknown > 0 {
        parts.push(format!("{unknown} pull-request state(s) indeterminate"));
    }
    if skipped > 0 {
        parts.push(format!("{skipped} not inspected before the deadline"));
    }
    if parts.is_empty() {
        return String::new();
    }
    format!("; could not classify: {}", parts.join(", "))
}

/// Every stderr diagnostic the merged-PR pass owes the operator (#2919, #5829).
///
/// Why: pure so the lines can be ASSERTED rather than merely executed. The
/// previous renderer printed straight to stderr, so its tests could only prove
/// it did not panic — which is why `spared_agent_owned` being dropped entirely
/// was invisible to them.
/// What: three families, each `"  <label>: <path>: <reason>"`. A worktree spared
/// because a dispatched agent owns it comes FIRST: it is the only one naming a
/// tree that is still being written to, and #5829 exists because the sweep
/// stayed silent about it. Then the re-check near-misses, then removals that
/// failed. An absent or non-array key contributes nothing.
/// Test: `merged_pr_pass_surfaces_an_agent_owned_skip`,
/// `merged_pr_pass_surfaces_recheck_refusals`.
fn diagnostic_lines(merged: &serde_json::Value) -> Vec<String> {
    let strings = |key: &str| -> Vec<String> {
        merged
            .get(key)
            .and_then(serde_json::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut out = Vec::new();
    // #5829: a live agent's tree was spared. The operator has to see this — it
    // is the difference between "nothing was reclaimable" and "something was
    // deliberately protected, and here is who for".
    for s in strings("spared_agent_owned") {
        out.push(format!("  spared — a dispatched agent owns it: {s}"));
    }
    // A candidate the survey approved but the fresh re-check refused is a
    // near-miss: the workspace changed underneath the pass.
    for s in strings("refused_at_recheck") {
        out.push(format!("  refused at re-check: {s}"));
    }
    for s in strings("removal_failed") {
        out.push(format!("  removal FAILED (still on disk): {s}"));
    }
    out
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
/// `cli_prune_worktrees_discard_dirty_is_opt_in`; the #5830 timeout override by
/// `merged_pr_request_outlives_the_default_client_timeout`.
pub(crate) async fn session_prune_worktrees(
    client: &reqwest::Client,
    url: &str,
    dry_run: bool,
    discard_dirty: bool,
    merged_prs: bool,
) -> anyhow::Result<()> {
    let mut request = client
        .post(format!("{url}/api/v1/sessions/managed/prune-worktrees"))
        .json(&serde_json::json!({
            "dry_run": dry_run,
            "discard_dirty": discard_dirty,
            // #2919: the merged-PR reclaim pass, off unless explicitly asked for.
            "merged_prs": merged_prs,
        }));
    if merged_prs {
        // #5830: the merged-PR survey runs synchronously in the handler and
        // takes minutes, so the client's 10s default aborted every invocation.
        request = request.timeout(trusty_mpm::client::http_client::RECLAIM_SURVEY_REQUEST_TIMEOUT);
        // The wait is long and the daemon streams nothing, so say what is
        // happening — an operator with no output cannot tell a running survey
        // from a wedged one.
        eprintln!(
            "merged-PR pass: surveying every registered worktree (classify, then byte-walk) \
             — this takes minutes on a large workspace and prints nothing until it \
             finishes (#5830)"
        );
    }
    let resp = request.send().await?;
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
