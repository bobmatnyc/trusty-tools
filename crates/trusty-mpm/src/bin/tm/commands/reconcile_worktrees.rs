//! `tm session reconcile-worktrees` — render the report-only worktree
//! inventory (#4207 slice 3, #4288).
//!
//! Why: `managed.rs` sits against the 500-SLOC production cap, and this is a
//! self-contained renderer rather than another managed-lifecycle verb — it
//! issues one GET and formats the result, sharing no state with anything else
//! in that file.
//! What: one client function; see its doc for the output shape.
//! Test: `cli_parses_session_reconcile_worktrees`,
//! `cli_reconcile_worktrees_takes_no_destructive_flag`.

/// `tm session reconcile-worktrees` — print the report-only worktree
/// inventory (#4207 slice 3, #4288).
///
/// Why: an operator's only worktree view today is the reclaim-candidate list,
/// which hides every excluded worktree — including a `workspace_path` that is
/// really an ordinary subdirectory of a live checkout, and worktrees nested
/// inside a live one. Both are landmines precisely because nothing prints them.
/// What: GETs the reconcile route and prints `STATE  path` followed by the
/// reason, then the proposed-adoption list (names only — this slice writes
/// nothing). `--json` emits the raw report for scripting. There is no
/// destructive form of this command.
/// Test: `cli_parses_session_reconcile_worktrees`,
/// `cli_reconcile_worktrees_takes_no_destructive_flag`.
pub(crate) async fn session_reconcile_worktrees(
    client: &reqwest::Client,
    url: &str,
    json: bool,
) -> anyhow::Result<()> {
    let resp = client
        .get(format!("{url}/api/v1/sessions/managed/reconcile-worktrees"))
        .send()
        .await?;
    let body: serde_json::Value = resp.error_for_status()?.json().await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    let str_at = |v: &serde_json::Value, key: &str| -> String {
        v.get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("<unreported>")
            .to_owned()
    };
    let entries = body
        .get("entries")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    for e in &entries {
        // Deepest-first, as the report orders them: a nested worktree is always
        // listed before the worktree that contains it.
        println!(
            "{:<9} {}",
            str_at(e, "state").to_uppercase(),
            str_at(e, "path")
        );
        println!("          {}", str_at(e, "reason"));
    }
    let n = |key: &str| -> u64 {
        body.pointer(&format!("/counts/{key}"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default()
    };
    println!(
        "{} worktree(s): {} live, {} orphaned, {} unknown \
         ({} reclaim candidates, {} excluded, {} named only by sessions.json)",
        n("total"),
        n("live"),
        n("orphaned"),
        n("unknown"),
        n("admitted"),
        n("excluded"),
        n("record_only"),
    );

    let adoptions = body
        .get("proposed_adoptions")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if adoptions.is_empty() {
        return Ok(());
    }
    println!(
        "\nwould be adopted by a later slice ({} — NOTHING was written):",
        adoptions.len()
    );
    for a in &adoptions {
        println!("  {}", str_at(a, "path"));
        println!(
            "    owner {} — {}",
            str_at(a, "inferred_owner"),
            str_at(a, "evidence")
        );
    }
    Ok(())
}
