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
//!
//! **Grouping caveat (post-review, #2306):** `source_id` is derived from the
//! git remote and is per-REPOSITORY
//! ([`super::adopt::derive_source_id_for_record`]), not per-workspace — every
//! concurrent worktree session of one repo shares the same `source_id`. A
//! naive "keep only the newest per group" policy would therefore collapse
//! *legitimate concurrent sessions*, and worse: an in-project worktree record
//! is `workspace_owned = false` BY DESIGN, yet `decommission` still runs
//! `git worktree remove --force` (+ branch delete) for such a path via
//! [`super::decommission::is_session_worktree`] — so a naive policy would
//! destroy real, possibly-uncommitted work. See the eligibility rule below,
//! which is the actual fix for that danger.
//!
//! What: groups non-`Decommissioned` records by `source_id` (fallback: resolved
//! `workspace_path`). Within a quiesced group (no member has a live tmux
//! session — see the live-group guard), a record is only ever a dedup
//! **loser** when it is unresolved-or-dead: [`is_resolved_existing`] is
//! `false` for it (no `workspace_path`, the `/unknown` sentinel, or a path
//! that no longer exists on disk). A record with a resolved, still-existing
//! `workspace_path` is NEVER a loser — regardless of group size or recency —
//! because such records are the concurrent-session case the naive policy got
//! wrong. Consequently:
//! - a group with 2+ resolved-existing members collapses nothing among them;
//!   only its unresolved/dead members (if any) are decommissioned, and only
//!   because a resolved-existing (or live) member supersedes them;
//! - a group with ZERO resolved-existing members (e.g. `/unknown`-only or
//!   all-dead) keeps the most-recently-active/created member and
//!   decommissions the rest — there is nothing on disk to lose in that case.
//!
//! An unresolved/dead loser is additionally rechecked against the SAME
//! [`is_resolved_existing`] predicate immediately before decommission runs
//! (belt-and-suspenders against a TOCTOU race); a loser that fails the
//! recheck is skipped with a `warn!`, never decommissioned.
//!
//! Test: `plan_dedup_*`, `is_resolved_existing_*`, and `reconcile_dedup_*` in
//! `dedup_tests.rs`.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use tracing::{info, warn};

use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};

impl SessionManager {
    /// Collapse stale duplicate session records per project (#2306).
    ///
    /// Why: see the module docs — quiesced projects with a canonical record AND
    /// a stale `/unknown` adopted record must converge on the canonical one so
    /// resume/re-attach stops targeting the dead record, WITHOUT ever touching
    /// a resolved, still-existing workspace (the review-fixed danger: naively
    /// collapsing by `source_id` alone would destroy legitimate concurrent
    /// worktree sessions of one repo).
    /// What: re-derives the live managed-tmux name set, reads all records,
    /// plans the losers via [`plan_dedup`] (which already excludes every
    /// resolved-existing record categorically), logs the full plan at `info`
    /// level, removes any planned loser from `to_resume` (never auto-resume a
    /// record about to be decommissioned), then for each loser: re-fetches it
    /// and reruns [`is_resolved_existing`] as a belt-and-suspenders recheck
    /// (skips + `warn!`s if the path has reappeared since planning) before
    /// decommissioning through the standard `decommission` path. Decommission
    /// failures are logged, not fatal. Returns the ids actually decommissioned.
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

        info!(
            count = losers.len(),
            ids = ?losers.iter().map(ManagedSessionId::to_string).collect::<Vec<_>>(),
            "dedup: plan computed stale duplicate(s) to decommission (#2306)"
        );

        // Never auto-resume a record we are about to decommission.
        to_resume.retain(|id| !losers.contains(id));

        let mut decommissioned = Vec::new();
        for id in &losers {
            // Belt-and-suspenders (#2306 review fix): re-fetch and rerun the
            // EXACT safety predicate used during planning immediately before
            // acting, guarding a TOCTOU race between the plan snapshot above
            // and this decommission call. A loser is only ever planned when
            // unresolved/dead; if its workspace_path now resolves to an
            // existing directory, refuse to decommission rather than risk
            // deleting live work.
            let current = match self.get(id).await {
                Ok(r) => r,
                Err(e) => {
                    warn!(id = %id, "dedup: loser vanished before decommission: {e}");
                    continue;
                }
            };
            if is_resolved_existing(&current) {
                warn!(
                    id = %id,
                    workspace = ?current.workspace_path,
                    "dedup: belt-and-suspenders skip — loser's workspace_path now \
                     resolves to an existing directory; refusing to decommission (#2306)"
                );
                continue;
            }
            match self.decommission(id).await {
                Ok((rec, _removed)) => {
                    info!(
                        id = %id,
                        name = %rec.tmux_name,
                        "dedup: decommissioned stale duplicate session record (#2306)"
                    );
                    decommissioned.push(*id);
                }
                Err(e) => warn!(id = %id, "dedup: decommission of stale duplicate failed: {e}"),
            }
        }
        Ok(decommissioned)
    }
}

/// Grouping key for a record, or `None` if it cannot be grouped.
///
/// Why: two records for the SAME project must land in the same group so a
/// quiesced duplicate can be collapsed; a record with neither a `source_id`
/// nor a `workspace_path` (a bare `/unknown` with no provenance) is not safely
/// attributable to any project and is therefore left alone. NOTE: `source_id`
/// is per-repository, not per-workspace (see module docs) — the safety
/// boundary against over-grouping is [`is_resolved_existing`] at selection
/// time, not this key.
/// What: prefers `source_id`; falls back to the `workspace_path` string. The
/// key is namespaced (`src:` / `ws:`) so a source id can never collide with a
/// path.
/// Test: covered indirectly by `plan_dedup_distinct_source_ids_untouched` and
/// `plan_dedup_unknown_only_group_keeps_most_recent`.
fn group_key(record: &SessionRecord) -> Option<String> {
    if let Some(sid) = &record.source_id {
        return Some(format!("src:{sid}"));
    }
    record
        .workspace_path
        .as_ref()
        .map(|ws| format!("ws:{}", ws.display()))
}

/// True when `record.workspace_path` is a resolved, real, currently-existing
/// directory on disk.
///
/// Why (#2306 review fix): this is THE safety boundary for dedup. A record
/// that passes this check represents a legitimate on-disk workspace — which
/// may be one of several concurrent sessions of the SAME repo, since
/// `source_id` groups by repository, not by workspace. Such a record must
/// never be treated as a stale duplicate, no matter the group size or its
/// recency relative to siblings — an in-project worktree record is
/// `workspace_owned = false` by design, so the `workspace_owned` flag alone
/// cannot distinguish "safe to decommission" from "real work that would be
/// `git worktree remove --force`'d". Only a record that FAILS this check (no
/// `workspace_path`, the literal `/unknown` sentinel, or a path that no
/// longer exists — e.g. already removed by other means) is eligible to be a
/// dedup loser.
/// What: `true` iff `workspace_path` is `Some(p)`, `p != Path::new("/unknown")`,
/// and `p.exists()`.
/// Test: `is_resolved_existing_true_for_real_dir`,
/// `is_resolved_existing_false_for_missing_dir`,
/// `is_resolved_existing_false_for_none_and_unknown_sentinel`.
pub(crate) fn is_resolved_existing(record: &SessionRecord) -> bool {
    match &record.workspace_path {
        Some(p) => p.as_path() != Path::new("/unknown") && p.exists(),
        None => false,
    }
}

/// Order two records by recency only (greater = more recently active/created).
///
/// Why: used ONLY to break a tie among unresolved/dead candidates in a group
/// with zero resolved-existing members — there is nothing on disk to lose in
/// that case, so "most recently touched" is a reasonable survivor heuristic.
/// What: ranks by `last_activity_at` (`None` sorts earliest), then
/// `created_at`.
/// Test: `plan_dedup_unknown_only_group_keeps_most_recent`.
fn by_recency(a: &SessionRecord, b: &SessionRecord) -> Ordering {
    a.last_activity_at
        .cmp(&b.last_activity_at)
        .then(a.created_at.cmp(&b.created_at))
}

/// Plan which records to decommission to dedup stale duplicates (#2306).
///
/// Why: the decision must be a pure function of the current record set, disk
/// state (via [`is_resolved_existing`]'s `exists()` check), and the live-tmux
/// name set so it is trivially unit-testable without a real store or tmux
/// driver (the #1790 no-real-tmux guard).
/// What: groups non-`Decommissioned` records via [`group_key`]; skips any
/// group of size <2 or with a live-tmux member. For the remaining (quiesced,
/// size>1) groups, partitions members into resolved-existing vs.
/// unresolved/dead via [`is_resolved_existing`]. If ANY member is
/// resolved-existing, every unresolved/dead member is a loser (the
/// resolved-existing members are NEVER touched, even if there are several).
/// If NONE are resolved-existing, the most-recent member (by [`by_recency`])
/// survives and every other member is a loser.
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

        // Safety partition (#2306 review fix): a resolved-existing member can
        // NEVER be a loser. Only unresolved/dead members are candidates.
        let (resolved_existing, candidates): (Vec<&SessionRecord>, Vec<&SessionRecord>) = members
            .iter()
            .copied()
            .partition(|r| is_resolved_existing(r));

        if resolved_existing.is_empty() {
            // Nothing in this group is resolved-existing (an /unknown-only or
            // all-dead group): nothing on disk to lose. Keep the most
            // recently active/created candidate, decommission the rest.
            if let Some(survivor) = candidates.iter().copied().max_by(|a, b| by_recency(a, b)) {
                losers.extend(
                    candidates
                        .iter()
                        .filter(|r| r.id != survivor.id)
                        .map(|r| r.id),
                );
            }
        } else {
            // At least one resolved-existing (or live-checked-clean) member
            // supersedes every unresolved/dead candidate. The
            // resolved-existing members themselves are left untouched — even
            // if there are 2+ of them (legitimate concurrent sessions).
            losers.extend(candidates.iter().map(|r| r.id));
        }
    }
    losers
}
