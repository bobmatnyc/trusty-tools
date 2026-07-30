//! Auto-prune definitively-dead (`unresumable`) managed-session records at
//! listing time (owner request 2026-07-29).
//!
//! Why: extracted out of `session_picker.rs` (which sits at the 500-SLOC
//! production cap, mirroring why `session_picker_render.rs`/
//! `session_picker_rename.rs` were split out before it). Before this module
//! existed, a session flagged `unresumable` (its workspace GC-pruned out from
//! under it) sat in the picker forever printing `DEAD: workspace removed; use
//! [d<N>] to remove the record` — the operator had to notice the row and type
//! the number themselves, every time.
//!
//! What: [`auto_prune_dead_records`] is the pure-ish partition+drive seam
//! (unit-testable up to its one I/O primitive); [`decommission_dead_record`]
//! is that primitive — a best-effort POST to the existing `/decommission`
//! route (the SAME dirty-worktree-gated teardown path #4344, `114de333`,
//! hardened), never a raw `fs::remove` or a hand-edited store entry.
//!
//! Test: `auto_prune_dead_records_removes_unresumable_records`,
//! `auto_prune_dead_records_keeps_workspace_present_records`,
//! `auto_prune_dead_records_is_noop_when_nothing_is_dead` in
//! `tests_behavior_d_tests.rs`.

use trusty_mpm::client::ManagedSessionSummary;

/// Auto-prune definitively-dead (`unresumable`) session records at listing
/// time (owner request 2026-07-29).
///
/// Why: before this, an operator had to notice the `DEAD: workspace removed`
/// row and manually type `d<N>` for every session whose workspace had been
/// GC-pruned out from under it — Bob's ask was for the picker to just clean
/// these up on its own.
///
/// SAFETY BOUNDARY (#4344, `114de333`): a record whose worktree removal was
/// previously REFUSED because the tree was dirty keeps its `workspace_path`
/// pointed at a directory that still genuinely exists on disk. `unresumable`
/// (`session_manager::resume_workdir::is_unresumable`) probes that exact
/// path with `tokio::fs::try_exists` and returns `false` the instant ANY
/// candidate (`last_cwd`/`workspace_path`/`cwd`) is found present — so a
/// dirty-retained record can never read `unresumable == true` in the first
/// place. This function's partition below is therefore sufficient on its own
/// to keep such a record out of scope; no additional workspace-existence
/// check is needed here.
/// What: partitions `sessions` into `(keep, dead)` by `s.unresumable`; for
/// each `dead` record, POSTs the existing `/decommission` route (the SAME
/// dirty-worktree-gated teardown path #4344 hardened — never a raw
/// `fs::remove` or a hand-edited store entry) via
/// [`decommission_dead_record`]. Returns the `keep` list with every
/// SUCCESSFULLY-decommissioned record dropped, plus the count actually
/// removed. A record whose decommission attempt fails (network error,
/// already gone, etc.) is kept in the returned list — best-effort, and it
/// still carries its manual `[d<N>]` remedy rather than silently vanishing
/// from the operator's view without ever having been removed.
/// Test: `auto_prune_dead_records_removes_unresumable_records`,
/// `auto_prune_dead_records_keeps_workspace_present_records`,
/// `auto_prune_dead_records_is_noop_when_nothing_is_dead`.
pub(crate) async fn auto_prune_dead_records(
    client: &reqwest::Client,
    url: &str,
    sessions: Vec<ManagedSessionSummary>,
) -> (Vec<ManagedSessionSummary>, usize) {
    let (dead, mut kept): (Vec<_>, Vec<_>) = sessions.into_iter().partition(|s| s.unresumable);
    if dead.is_empty() {
        return (kept, 0);
    }
    let mut pruned = 0usize;
    for s in dead {
        if decommission_dead_record(client, url, &s.id).await {
            pruned += 1;
        } else {
            kept.push(s);
        }
    }
    (kept, pruned)
}

/// POST the existing `/decommission` route for one dead record, best-effort.
///
/// Why: kept as the single I/O primitive behind [`auto_prune_dead_records`] so
/// a per-record transport/HTTP failure can never abort the rest of the
/// listing — one stuck record must not hide every other session behind it.
/// What: returns `true` only on a 2xx response; any other status or a
/// transport error is logged to stderr and returns `false`.
/// Test: `auto_prune_dead_records_removes_unresumable_records` drives this
/// through a real loopback daemon.
async fn decommission_dead_record(client: &reqwest::Client, url: &str, id: &str) -> bool {
    match client
        .post(format!("{url}/api/v1/sessions/managed/{id}/decommission"))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => true,
        Ok(resp) => {
            eprintln!(
                "tm: auto-prune: failed to remove dead record {id}: HTTP {}",
                resp.status()
            );
            false
        }
        Err(e) => {
            eprintln!("tm: auto-prune: failed to remove dead record {id}: {e}");
            false
        }
    }
}
