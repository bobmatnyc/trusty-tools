//! Stale duplicate-record dedup for `reconcile_on_boot` (#2306).
//!
//! Why: a project can accrue two conflicting non-`Decommissioned` records — one
//! canonical (resolved `workspace_path`) and one adopted record permanently
//! stuck at `cwd`/`workspace_path = /unknown` (adopted before #2163's
//! `get_pane_cwd` resolution existed). `reconcile_on_boot` only resolves
//! `/unknown` when a LIVE pane is discovered; it never revisits a `/unknown`
//! record once its pane is gone, and nothing collapses N records per project,
//! so `tm` keeps re-attaching to the wrong/stale one. #2001/#2004 (zombie
//! reconcile) and #2148 (non-destructive resume) are single-record and do not
//! cover this.
//! What: groups non-`Decommissioned` records by `source_id` (fallback: resolved
//! `workspace_path`); for a group of >1 records in which NO member has a live
//! tmux session, keeps the best (resolved `workspace_path` beats `/unknown`;
//! tie-break most-recent `last_activity_at`, then `created_at`) and
//! decommissions the rest via the existing safe path (adopted `/unknown`
//! records are `workspace_owned = false`, so no disk removal happens). A group
//! with ANY live tmux member is left untouched — the live one is authoritative.
//! Test: `plan_dedup_*` and `reconcile_dedup_*` in `dedup_tests.rs`.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use tracing::{info, warn};

use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};

impl SessionManager {
    /// Collapse stale duplicate session records per project (#2306).
    ///
    /// Why: see the module docs — quiesced projects with a canonical record AND
    /// a stale `/unknown` adopted record must converge on the canonical one so
    /// resume/re-attach stops targeting the dead record.
    /// What: re-derives the live managed-tmux name set, reads all records, plans
    /// the losers via [`plan_dedup`], removes any planned loser from `to_resume`
    /// (never auto-resume a record about to be decommissioned), then
    /// decommissions each loser through the standard `decommission` path (safe:
    /// `/unknown` losers are `workspace_owned = false` → no disk mutation).
    /// Decommission failures are logged, not fatal. Returns the ids selected for
    /// decommission.
    /// Test: `reconcile_dedup_collapses_stopped_duplicates` and siblings.
    pub(crate) async fn dedup_stale_duplicates(
        &self,
        to_resume: &mut Vec<ManagedSessionId>,
    ) -> Result<Vec<ManagedSessionId>, ManagedError> {
        let live_names: HashSet<String> = self
            .tmux
            .list_sessions()
            .unwrap_or_default()
            .into_iter()
            .filter(|n| crate::core::names::is_managed_session_name(n))
            .collect();
        let records = self.store.write().await.all().await?;
        let losers = plan_dedup(&records, &live_names);
        if losers.is_empty() {
            return Ok(losers);
        }
        // Never auto-resume a record we are about to decommission.
        to_resume.retain(|id| !losers.contains(id));
        for id in &losers {
            match self.decommission(id).await {
                Ok((rec, _removed)) => info!(
                    id = %id,
                    name = %rec.tmux_name,
                    "dedup: decommissioned stale duplicate session record (#2306)"
                ),
                Err(e) => warn!(id = %id, "dedup: decommission of stale duplicate failed: {e}"),
            }
        }
        Ok(losers)
    }
}

/// Grouping key for a record, or `None` if it cannot be grouped.
///
/// Why: two records for the SAME project must land in the same group so a
/// quiesced duplicate can be collapsed; a record with neither a `source_id`
/// nor a `workspace_path` (a bare `/unknown` with no provenance) is not safely
/// attributable to any project and is therefore left alone.
/// What: prefers `source_id`; falls back to the `workspace_path` string. The
/// key is namespaced (`src:` / `ws:`) so a source id can never collide with a
/// path.
/// Test: covered indirectly by `plan_dedup_distinct_source_ids_untouched` and
/// `plan_dedup_unknown_only_group`.
fn group_key(record: &SessionRecord) -> Option<String> {
    if let Some(sid) = &record.source_id {
        return Some(format!("src:{sid}"));
    }
    record
        .workspace_path
        .as_ref()
        .map(|ws| format!("ws:{}", ws.display()))
}

/// Order two records so the greater one is the preferred survivor.
///
/// Why: within a quiesced group the survivor must be the most authoritative
/// record — a resolved on-disk workspace over a `/unknown` stub, then the most
/// recently active/created.
/// What: ranks by `workspace_path.is_some()` (resolved beats `/unknown`), then
/// `last_activity_at` (`None` sorts earliest), then `created_at`.
/// Test: `plan_dedup_prefers_resolved_over_unknown`,
/// `plan_dedup_unknown_only_group`.
fn survivor_pref(a: &SessionRecord, b: &SessionRecord) -> Ordering {
    a.workspace_path
        .is_some()
        .cmp(&b.workspace_path.is_some())
        .then(a.last_activity_at.cmp(&b.last_activity_at))
        .then(a.created_at.cmp(&b.created_at))
}

/// Plan which records to decommission to dedup stale duplicates (#2306).
///
/// Why: the decision must be a pure function of the current record set and the
/// live-tmux name set so it is trivially unit-testable without a real store or
/// tmux (the #1790 no-real-tmux guard).
/// What: groups non-`Decommissioned` records via [`group_key`]; for each group
/// of size >1 that has NO live-tmux member, selects the best survivor via
/// [`survivor_pref`] and returns every OTHER member's id as a loser. Groups of
/// size 1, ungroupable records, and any group with a live-tmux member yield no
/// losers.
/// Test: the `plan_dedup_*` tests in `dedup_tests.rs`.
pub(crate) fn plan_dedup(
    records: &[SessionRecord],
    live_names: &HashSet<String>,
) -> Vec<ManagedSessionId> {
    let mut groups: HashMap<String, Vec<&SessionRecord>> = HashMap::new();
    for record in records {
        if matches!(record.state, ManagedSessionState::Decommissioned) {
            continue;
        }
        if let Some(key) = group_key(record) {
            groups.entry(key).or_default().push(record);
        }
    }

    let mut losers = Vec::new();
    for members in groups.values() {
        if members.len() < 2 {
            continue;
        }
        // Live-group guard: if ANY member has a live tmux session, the live one
        // is authoritative — never touch a group that is not fully quiesced.
        if members.iter().any(|r| live_names.contains(&r.tmux_name)) {
            continue;
        }
        let Some(survivor) = members.iter().copied().max_by(|a, b| survivor_pref(a, b)) else {
            continue;
        };
        for record in members {
            if record.id != survivor.id {
                losers.push(record.id);
            }
        }
    }
    losers
}
