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
use std::path::{Path, PathBuf};

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
        let mut losers = plan_dedup(&records, &live_names);

        // #3396: also collapse EXACT-workspace_path duplicates — two+
        // non-Decommissioned records resolving to the literal SAME on-disk
        // worktree. `plan_dedup` above deliberately never touches this shape
        // (its `source_id` grouping is per-repository, and a
        // resolved-existing member is categorically immune there — both
        // correct for the LEGITIMATE concurrent-worktree case, which is why
        // the #3396 duplicate persisted across every prior dedup pass). See
        // `plan_workspace_duplicates`'s doc for why "same literal path" is
        // safe to treat differently. Track which ids came from THIS mechanism
        // so the belt-and-suspenders recheck below applies the right predicate
        // to each.
        let workspace_dup_losers: HashSet<ManagedSessionId> =
            plan_workspace_duplicates(&records, &live_names)
                .into_iter()
                .collect();
        for id in &workspace_dup_losers {
            if !losers.contains(id) {
                losers.push(*id);
            }
        }

        if losers.is_empty() {
            return Ok(losers);
        }

        info!(
            count = losers.len(),
            ids = ?losers.iter().map(ManagedSessionId::to_string).collect::<Vec<_>>(),
            "dedup: plan computed stale duplicate(s) to decommission (#2306, #3396)"
        );

        // Never auto-resume a record we are about to decommission.
        to_resume.retain(|id| !losers.contains(id));

        let mut decommissioned = Vec::new();
        for id in &losers {
            // Belt-and-suspenders: re-fetch immediately before acting, guarding
            // a TOCTOU race between the plan snapshot above and this
            // decommission call.
            let current = match self.get(id).await {
                Ok(r) => r,
                Err(e) => {
                    warn!(id = %id, "dedup: loser vanished before decommission: {e}");
                    continue;
                }
            };
            if workspace_dup_losers.contains(id) {
                // #3396 recheck: an exact-workspace-duplicate loser is
                // EXPECTED to be resolved-existing (that is the grouping key),
                // so the #2306 predicate below does not apply here. Instead
                // refuse when the record turned out to be SM-owned (deletion
                // would risk `git worktree remove --force`ing real work — see
                // `plan_workspace_duplicates`'s categorical owned-record
                // exclusion, rechecked here against a TOCTOU) or when its OWN
                // tmux session has since gone live (decommission kills the
                // whole tmux session — never do that to a live one).
                if current.workspace_owned {
                    warn!(
                        id = %id,
                        "dedup: #3396 belt-and-suspenders skip — loser is SM-owned; \
                         refusing to decommission"
                    );
                    continue;
                }
                if live_names.contains(&current.tmux_name) {
                    warn!(
                        id = %id,
                        name = %current.tmux_name,
                        "dedup: #3396 belt-and-suspenders skip — loser's tmux session is now \
                         live; refusing to decommission an active session"
                    );
                    continue;
                }
            } else if is_resolved_existing(&current) {
                // Belt-and-suspenders (#2306 review fix): re-run the EXACT
                // safety predicate used during planning immediately before
                // acting. A `plan_dedup` loser is only ever planned when
                // unresolved/dead; if its workspace_path now resolves to an
                // existing directory, refuse to decommission rather than risk
                // deleting live work.
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
                        "dedup: decommissioned stale duplicate session record (#2306, #3396)"
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

/// Plan losers among records that share the IDENTICAL, resolved-existing
/// `workspace_path` (#3396).
///
/// Why: [`plan_dedup`]'s `source_id` grouping is per-REPOSITORY by design
/// (see the module docs) — it deliberately never touches two
/// resolved-existing records with DIFFERENT `workspace_path`s, because that
/// shape is the legitimate concurrent-worktree-sessions case, and it treats
/// ANY resolved-existing member as categorically immune regardless of group
/// size. But two records pointing at the exact SAME `workspace_path` are
/// never that: a single worktree directory hosts at most one live managed
/// identity, so a group of 2+ non-`Decommissioned` records sharing one
/// resolved-existing path is ALWAYS a duplicate-identity bug (#3396 — e.g. a
/// live tmux session renamed/replaced out from under its record, then
/// re-discovered and adopted under a different name against the same cwd by
/// [`super::reconcile::SessionManager::reconcile_on_boot`]'s external-adopt
/// loop, before that loop's own #3396 duplicate-prevention check existed).
/// `plan_dedup`'s live-group guard ("skip the WHOLE group if ANY member is
/// live") and its resolved-existing categorical immunity both, correctly,
/// refuse to touch this shape — which is exactly why a #3396 stale duplicate
/// persists across every `plan_dedup`-only pass. This function is the
/// narrowly-scoped exact-path counterpart: it groups by canonicalized
/// `workspace_path` alone (ignoring `source_id`), and — because "same
/// directory" is unambiguous where "same repository" is not — is allowed to
/// pick a survivor even when one member is currently live.
/// What: groups non-`Decommissioned`, resolved-existing (per
/// [`is_resolved_existing`]) records by canonicalized `workspace_path`, then
/// for each group of 2+:
/// - A `workspace_owned = true` member (the SM provisioned this directory
///   itself, e.g. via clone) is NEVER a loser — categorical protection
///   against ever risking a real, SM-owned repository, mirroring
///   [`is_resolved_existing`]'s role in `plan_dedup`. Two-or-more owned
///   members sharing one path should structurally never happen; rather than
///   guess, that shape leaves the WHOLE group untouched.
/// - Exactly one owned member: it is the automatic survivor, and every
///   `workspace_owned = false` sibling whose `tmux_name` is currently DEAD is
///   a loser (a live one is left alone too — never decommission a currently
///   active session).
/// - Zero owned members: the survivor is chosen among the unowned members —
///   if exactly one has a live `tmux_name` it survives; if none are live, the
///   most recently active/created one survives (mirrors `plan_dedup`'s
///   /unknown tie-break); if two or more are simultaneously live, the group
///   is left untouched — two genuinely-live panes pointed at the same
///   directory is ambiguous enough that guessing a survivor risks tearing
///   down a real session, so this defers to the operator rather than picking
///   one.
///
/// A loser selected here therefore always has both `workspace_owned = false`
/// (decommission never deletes the directory) and a currently-DEAD
/// `tmux_name` (decommission never kills a live tmux session).
/// Test: the `plan_workspace_duplicates_*` tests in `dedup_tests.rs`.
pub(crate) fn plan_workspace_duplicates(
    records: &[SessionRecord],
    live_names: &HashSet<String>,
) -> Vec<ManagedSessionId> {
    let mut groups: HashMap<PathBuf, Vec<&SessionRecord>> = HashMap::new();
    for record in records {
        if matches!(record.state, ManagedSessionState::Decommissioned) {
            continue;
        }
        if !is_resolved_existing(record) {
            continue;
        }
        // is_resolved_existing() only returns true when workspace_path is
        // Some, so this is always populated for a grouped record.
        let Some(path) = record.workspace_path.as_ref() else {
            continue;
        };
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
        groups.entry(canon).or_default().push(record);
    }

    let mut losers = Vec::new();
    for members in groups.values() {
        if members.len() < 2 {
            continue;
        }
        let (owned, unowned): (Vec<&SessionRecord>, Vec<&SessionRecord>) =
            members.iter().copied().partition(|r| r.workspace_owned);

        if owned.len() >= 2 {
            // 2+ SM-owned records for one literal path should never happen;
            // do not guess which is "real" — leave the whole group alone.
            continue;
        }
        if owned.len() == 1 {
            // The owned record is the permanent survivor. Every unowned
            // sibling that is NOT currently live is a duplicate of it.
            losers.extend(
                unowned
                    .iter()
                    .filter(|r| !live_names.contains(&r.tmux_name))
                    .map(|r| r.id),
            );
            continue;
        }

        // owned.is_empty(): pick a survivor among the unowned members.
        if unowned.len() < 2 {
            continue;
        }
        let live_candidates: Vec<&SessionRecord> = unowned
            .iter()
            .copied()
            .filter(|r| live_names.contains(&r.tmux_name))
            .collect();
        match live_candidates.len() {
            0 => {
                if let Some(survivor) = unowned.iter().copied().max_by(|a, b| by_recency(a, b)) {
                    losers.extend(unowned.iter().filter(|r| r.id != survivor.id).map(|r| r.id));
                }
            }
            1 => {
                let survivor_id = live_candidates[0].id;
                losers.extend(unowned.iter().filter(|r| r.id != survivor_id).map(|r| r.id));
            }
            _ => {
                // 2+ simultaneously-live candidates for one directory: leave
                // the whole group untouched (see doc above).
            }
        }
    }
    losers
}
