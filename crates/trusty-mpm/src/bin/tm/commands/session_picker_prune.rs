//! Auto-prune definitively-dead (`unresumable`) managed-session records at
//! listing time (owner request 2026-07-29; hardened per code-critic WARN,
//! PR #4384 review).
//!
//! Why: extracted out of `session_picker.rs` (which sits at the 500-SLOC
//! production cap, mirroring why `session_picker_render.rs`/
//! `session_picker_rename.rs` were split out before it). Before this module
//! existed, a session flagged `unresumable` (its workspace GC-pruned out from
//! under it) sat in the picker forever printing `DEAD: workspace removed; use
//! [d<N>] to remove the record` — the operator had to notice the row and type
//! the number themselves, every time.
//!
//! The critic's WARN identified this feature's first cut as converting an
//! advisory flag into an unconditional, unconfirmed, uncapped destructive
//! action. Three independent hardenings close that gap:
//!   1. **Record-only removal** — [`decommission_dead_record`] calls the
//!      `/decommission` route with `record_only=true`
//!      (`SessionManager::decommission_record_only`), which never touches
//!      the filesystem. A remount between the listing-time probe and this
//!      call can never lose data, because nothing is ever deleted here.
//!   2. **Two-sightings confirmation** — [`auto_prune_dead_records_at`] only
//!      acts on a session id whose `unresumable` flag was ALSO seen on a
//!      strictly earlier call, tracked via a small persisted marker file
//!      ([`load_seen`]/[`save_seen`]). A single-shot blip (one bad listing)
//!      never triggers a prune.
//!   3. **Per-call cap** — at most [`AUTO_PRUNE_CAP`] confirmed records are
//!      decommissioned per call; the rest are reported as "pending
//!      confirmation" rather than acted on. A bad-mount day cannot
//!      mass-tombstone an entire fleet in one `tm ls`.
//!
//! What: [`auto_prune_dead_records`] is the production entry point (resolves
//! the marker file under `~/.trusty-mpm`); [`auto_prune_dead_records_at`] is
//! the testable core (explicit marker-file path); [`AutoPruneOutcome`] is the
//! three-part result (`kept`, `pruned`, `pending`) `fetch_live_sessions`
//! reports to the operator.
//!
//! Test: `auto_prune_dead_records_removes_confirmed_unresumable_records`,
//! `auto_prune_dead_records_first_sighting_is_not_pruned`,
//! `auto_prune_dead_records_keeps_workspace_present_records`,
//! `auto_prune_dead_records_is_noop_when_nothing_is_dead`,
//! `auto_prune_dead_records_honors_the_cap` in `tests_behavior_d_tests.rs`;
//! the record-only server-side guarantee is pinned by
//! `decommission_record_only_never_removes_existing_workspace`
//! (`session_manager::tests`).

use std::collections::HashMap;
use std::path::Path;

use trusty_mpm::client::ManagedSessionSummary;

/// Maximum number of CONFIRMED dead records one call may decommission (critic
/// HIGH finding #1, owner request 2026-07-29).
///
/// Why: "a bad-mount day must not mass-tombstone every session in one `tm
/// ls`" — even a session flagged dead on two consecutive listings might all
/// share one now-unmounted volume. Capping bounds the blast radius of any
/// single call regardless of how many records are confirmed-eligible.
const AUTO_PRUNE_CAP: usize = 5;

/// Basename of the two-sightings confirmation marker file, under the
/// framework root (`~/.trusty-mpm/` by default).
const SEEN_MARKER_FILENAME: &str = "auto-prune-seen.json";

/// Result of one [`auto_prune_dead_records`] / [`auto_prune_dead_records_at`]
/// call.
///
/// Why: the caller (`fetch_live_sessions`) needs three independent numbers to
/// report honestly — what it removed, what it left alone because a
/// confirmation window or the cap wasn't met, and the surviving list to
/// render — rather than collapsing them into a single count.
pub(crate) struct AutoPruneOutcome {
    /// Every session NOT successfully decommissioned this call — healthy
    /// sessions, first-sightings awaiting confirmation, confirmed sessions
    /// over the cap, and any record whose decommission attempt failed.
    pub(crate) kept: Vec<ManagedSessionSummary>,
    /// Count of records actually decommissioned this call (never more than
    /// [`AUTO_PRUNE_CAP`]).
    pub(crate) pruned: usize,
    /// Count of `unresumable` records NOT acted on this call — either a
    /// first sighting (not yet confirmed) or confirmed-but-over-the-cap.
    pub(crate) pending: usize,
}

/// Production entry point: resolve the confirmation marker file under the
/// default framework root and delegate to [`auto_prune_dead_records_at`].
///
/// Why: the framework root (`~/.trusty-mpm`) is where every other piece of
/// `tm`'s per-user state already lives (`FrameworkPaths::default`);
/// resolving it here (rather than inside the testable core) keeps the core
/// injectable for tests without touching the real home directory.
/// What: `FrameworkPaths::default().root` joined with
/// [`SEEN_MARKER_FILENAME`].
/// Test: covered transitively by every `fetch_live_sessions` call site; the
/// decision logic itself is tested via [`auto_prune_dead_records_at`]
/// directly.
pub(crate) async fn auto_prune_dead_records(
    client: &reqwest::Client,
    url: &str,
    sessions: Vec<ManagedSessionSummary>,
) -> AutoPruneOutcome {
    let marker_path = trusty_mpm::core::paths::FrameworkPaths::default()
        .root
        .join(SEEN_MARKER_FILENAME);
    auto_prune_dead_records_at(client, url, sessions, &marker_path).await
}

/// Auto-prune definitively-dead (`unresumable`) session records at listing
/// time — the testable core (owner request 2026-07-29).
///
/// Why: before this, an operator had to notice the `DEAD: workspace removed`
/// row and manually type `d<N>` for every session whose workspace had been
/// GC-pruned out from under it — Bob's ask was for the picker to just clean
/// these up on its own. The critic's WARN required this to never be a
/// single-observation, uncapped, disk-mutating action — see the module doc's
/// three hardenings.
///
/// SAFETY BOUNDARY (#4344, `114de333`): a record whose worktree removal was
/// previously REFUSED because the tree was dirty keeps its `workspace_path`
/// pointed at a directory that still genuinely exists on disk. `unresumable`
/// (`session_manager::resume_workdir::is_unresumable`) probes that exact
/// path with `tokio::fs::try_exists` and returns `false` the instant ANY
/// candidate (`last_cwd`/`workspace_path`/`cwd`) is found present — so a
/// dirty-retained record can never read `unresumable == true` in the first
/// place, and never enters this function's `dead` partition at all.
///
/// What: partitions `sessions` into `(keep, dead)` by `s.unresumable`. Any
/// `keep` session that still has a confirmation-marker entry has recovered
/// (its workspace reappeared) — that entry is dropped so a later
/// re-appearance starts a fresh confirmation window. Each `dead` record is
/// then classified against the persisted marker file ([`load_seen`]): a
/// fresh id is recorded as a first sighting (NOT acted on this call); an id
/// already present (seen on a STRICTLY earlier call) is CONFIRMED-eligible.
/// At most [`AUTO_PRUNE_CAP`] confirmed records are decommissioned via
/// [`decommission_dead_record`] — the record-only route, never a raw
/// `fs::remove` or a hand-edited store entry. Everything else (healthy,
/// first-sighting, over-cap, or a failed decommission attempt) is returned
/// in `kept`. The marker file is persisted only when it actually changed.
/// Test: `auto_prune_dead_records_removes_confirmed_unresumable_records`,
/// `auto_prune_dead_records_first_sighting_is_not_pruned`,
/// `auto_prune_dead_records_keeps_workspace_present_records`,
/// `auto_prune_dead_records_is_noop_when_nothing_is_dead`,
/// `auto_prune_dead_records_honors_the_cap`.
pub(crate) async fn auto_prune_dead_records_at(
    client: &reqwest::Client,
    url: &str,
    sessions: Vec<ManagedSessionSummary>,
    marker_path: &Path,
) -> AutoPruneOutcome {
    let (dead, mut kept): (Vec<_>, Vec<_>) = sessions.into_iter().partition(|s| s.unresumable);

    let mut seen = load_seen(marker_path);
    let mut changed = false;

    // A session that surfaced HEALTHY this call has recovered — clear any
    // stale marker so a later re-appearance of unresumable starts fresh.
    for s in &kept {
        if seen.remove(&s.id).is_some() {
            changed = true;
        }
    }

    if dead.is_empty() {
        if changed {
            save_seen(marker_path, &seen);
        }
        return AutoPruneOutcome {
            kept,
            pruned: 0,
            pending: 0,
        };
    }

    let now = chrono::Utc::now().to_rfc3339();
    let mut confirmed = Vec::new();
    let mut first_sighting = Vec::new();
    for s in dead {
        if seen.contains_key(&s.id) {
            confirmed.push(s);
        } else {
            seen.insert(s.id.clone(), now.clone());
            changed = true;
            first_sighting.push(s);
        }
    }

    let cap = AUTO_PRUNE_CAP.min(confirmed.len());
    let to_prune: Vec<_> = confirmed.drain(..cap).collect();

    let mut pruned = 0usize;
    for s in to_prune {
        if decommission_dead_record(client, url, &s.id).await {
            pruned += 1;
            seen.remove(&s.id);
            changed = true;
        } else {
            kept.push(s);
        }
    }

    let pending = confirmed.len() + first_sighting.len();
    kept.extend(confirmed);
    kept.extend(first_sighting);

    if changed {
        save_seen(marker_path, &seen);
    }

    AutoPruneOutcome {
        kept,
        pruned,
        pending,
    }
}

/// Load the persisted first-sighting map, tolerating a missing or corrupt
/// file (treated as empty — a listing must never fail merely because its
/// confirmation marker couldn't be read).
///
/// What: `{ "<session-id>": "<rfc3339 first-sighting timestamp>" }`.
fn load_seen(path: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Best-effort persist of the first-sighting map; a write failure is logged
/// to stderr and never propagated (the listing must never fail merely
/// because the marker file couldn't be written).
fn save_seen(path: &Path, seen: &HashMap<String, String>) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string(seen) {
        Ok(json) => {
            if let Err(e) = std::fs::write(path, json) {
                eprintln!("tm: auto-prune: failed to persist confirmation marker: {e}");
            }
        }
        Err(e) => eprintln!("tm: auto-prune: failed to serialize confirmation marker: {e}"),
    }
}

/// POST the existing `/decommission` route in RECORD-ONLY mode for one
/// confirmed-dead record, best-effort (critic HIGH finding #1).
///
/// Why: kept as the single I/O primitive behind [`auto_prune_dead_records_at`]
/// so a per-record transport/HTTP failure can never abort the rest of the
/// listing — one stuck record must not hide every other session behind it.
/// What: POSTs `?record_only=true`, which routes the daemon to
/// `SessionManager::decommission_record_only` — the removal branches are
/// skipped entirely, so this call can never delete filesystem content even
/// if the workspace has reappeared since the listing-time probe. Returns
/// `true` only on a 2xx response; any other status or a transport error is
/// logged to stderr and returns `false`.
/// Test: `auto_prune_dead_records_removes_confirmed_unresumable_records`
/// drives this through a real loopback daemon.
async fn decommission_dead_record(client: &reqwest::Client, url: &str, id: &str) -> bool {
    match client
        .post(format!("{url}/api/v1/sessions/managed/{id}/decommission"))
        .query(&[("record_only", "true")])
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
