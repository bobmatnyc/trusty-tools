//! 30-day retention: evict idle duplicate and unregistered indexes (#10534).
//!
//! Why: #10533 parks an idle index (unloads it) but nothing ever deletes one,
//! so a worktree-session index or a decommissioned project's index keeps its
//! corpus on disk forever. Owner ruling (2026-11-28 05:20Z): a duplicate or an
//! unregistered index is evicted after 30 days idle; the 7-day orphan grace is
//! unchanged. Ruling Dta (08:19Z): an idle worktree-session index is evicted by
//! the same rule.
//! What: [`retention_class`] names which indexes the rule covers, and
//! [`retention_verdict`] is the pure keep-or-evict decision. [`evict_idle_once`]
//! is one pass, run on the orphan-reaper ticker: it reads the registry, the
//! project map and tm's active set, and evicts through the normal delete core
//! (`delete_data: true`, root pinned). Every input it cannot read keeps every
//! index and logs why.
//! Test: `retention_10534_tests`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use crate::core::project_map::{now_unix, ParkReason, ProjectMap};
use crate::core::project_map_io;
use crate::core::registry::IndexId;
use crate::service::persistence::PersistedIndex;
use crate::service::project_map_state::MapContext;
use crate::service::project_resolve::RootKind;
use crate::service::reindex::ReindexStatus;

use super::state::SearchAppState;

/// How long a covered index stays idle before it is evicted: 30 days.
pub(super) const RETENTION_SECS: u64 = 30 * 24 * 3600;

/// Which part of the 30-day rule covers an index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RetentionClass {
    /// A worktree root (parked or not) or an operator-parked same-root
    /// sibling. A worktree-session index is this class.
    Duplicate,
    /// tm decommissioned the index's project (`reason = decommissioned`).
    Unregistered,
}

/// Why an index is kept, for the debug log and the tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KeepReason {
    /// Registered and not a duplicate, or its root is missing or unprobed
    /// (the orphan path owns those).
    NotCovered,
    /// A trusted active set names it.
    LiveSession,
    /// No activity stamp was ever recorded.
    UnknownAge,
    /// Idle for less than [`RETENTION_SECS`].
    Recent,
}

/// The decision for one index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Verdict {
    Keep(KeepReason),
    Evict {
        class: RetentionClass,
        idle_secs: u64,
    },
}

/// The facts [`retention_verdict`] decides on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RetentionInput {
    /// `None`: the rule does not cover this index.
    pub class: Option<RetentionClass>,
    /// Latest of `last_queried_unix`, `last_indexed_unix` and the in-memory
    /// query stamp; `None` when none was ever recorded.
    pub last_activity_unix: Option<u64>,
    /// When the project map parked it, if it is parked.
    pub parked_unix: Option<u64>,
    /// A trusted active set names it.
    pub live: bool,
}

/// Which retention class covers an index, or `None` when none does.
///
/// Why: the rule must never reach a registered, non-duplicate index, and a
/// missing root belongs to the orphan path and its 7-day grace.
/// What: an `Orphaned` or `Indeterminate` root → `None`, whatever the park
/// row says. A park row: `worktree` / `operator` → `Duplicate`,
/// `decommissioned` → `Unregistered`, `orphaned` → `None`. No row: a
/// `Worktree` root → `Duplicate`, anything else → `None`.
/// Test: `retention_class_maps_park_reasons_and_root_kinds`.
pub(super) fn retention_class(
    parked: Option<ParkReason>,
    kind: RootKind,
) -> Option<RetentionClass> {
    if matches!(kind, RootKind::Orphaned | RootKind::Indeterminate) {
        return None;
    }
    match parked {
        Some(ParkReason::Worktree | ParkReason::Operator) => Some(RetentionClass::Duplicate),
        Some(ParkReason::Decommissioned) => Some(RetentionClass::Unregistered),
        Some(ParkReason::Orphaned) => None,
        None if kind == RootKind::Worktree => Some(RetentionClass::Duplicate),
        None => None,
    }
}

/// Keep or evict one index at `now_unix`.
///
/// Why: Fail-Open Check — an index whose age is unknown is kept, never
/// treated as idle since the epoch.
/// What: idle time runs from the later of the last activity and the park
/// time, saturating (a clock behind the stamp reads 0). Evict only when a
/// class covers it, no live session names it, an activity stamp exists, and
/// the idle time is at least [`RETENTION_SECS`].
/// Test: `retention_keeps_29_days_and_evicts_30_and_31`,
/// `retention_keeps_a_registered_index_however_idle`,
/// `retention_keeps_an_index_of_unknown_age`,
/// `retention_keeps_a_live_session_member`,
/// `retention_restarts_the_clock_at_the_park_time`.
pub(super) fn retention_verdict(input: RetentionInput, now_unix: u64) -> Verdict {
    let Some(class) = input.class else {
        return Verdict::Keep(KeepReason::NotCovered);
    };
    if input.live {
        return Verdict::Keep(KeepReason::LiveSession);
    }
    let Some(activity) = input.last_activity_unix else {
        return Verdict::Keep(KeepReason::UnknownAge);
    };
    let since = activity.max(input.parked_unix.unwrap_or(0));
    let idle_secs = now_unix.saturating_sub(since);
    if idle_secs >= RETENTION_SECS {
        Verdict::Evict { class, idle_secs }
    } else {
        Verdict::Keep(KeepReason::Recent)
    }
}

/// Classify `root` without touching a missing or external one.
///
/// What: `/Volumes/…` → `Indeterminate` (never `stat`ed); a root that is not
/// found → `Orphaned`; any other `stat` error → `Indeterminate`; else
/// [`crate::service::project_resolve::classify_root_kind`]. Blocking.
fn probe_kind(root: &Path) -> RootKind {
    if root.starts_with("/Volumes") {
        return RootKind::Indeterminate;
    }
    match std::fs::symlink_metadata(root) {
        Ok(_) => crate::service::project_resolve::classify_root_kind(
            root,
            &crate::service::constants::ephemeral_dir_names(),
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => RootKind::Orphaned,
        Err(_) => RootKind::Indeterminate,
    }
}

/// The latest recorded activity of `row`, or `None` when none was recorded.
fn last_activity(state: &SearchAppState, row: &PersistedIndex) -> Option<u64> {
    let cached = state
        .last_queried_write_cache
        .get(&IndexId::new(row.id.clone()))
        .map(|stamp| *stamp);
    [row.last_queried_unix, row.last_indexed_unix, cached]
        .into_iter()
        .flatten()
        .max()
}

/// Is `id` loaded, warm-pinned or reindexing? Such an index may be in use.
fn busy(state: &SearchAppState, id: &str) -> bool {
    let key = IndexId::new(id.to_string());
    state.registry.get(&key).is_some()
        || state.warm.is_pinned(id)
        || state
            .reindex_progress
            .get(&key)
            .is_some_and(|p| p.status.load() == ReindexStatus::Running)
}

/// The inputs this pass reads; `None` (and an error log) when one is unreadable.
fn read_inputs(state: &SearchAppState) -> Option<(Vec<PersistedIndex>, ProjectMap)> {
    let ctx = match MapContext::capture(state) {
        Ok(ctx) => ctx,
        Err(e) => {
            tracing::error!("retention: index registry unreadable, nothing evicted: {e} (#10534)");
            return None;
        }
    };
    match project_map_io::load(&ctx.map_path) {
        Ok(map) => Some((ctx.persisted, map.unwrap_or_default())),
        Err(e) => {
            tracing::error!("retention: project map unreadable, nothing evicted: {e} (#10534)");
            None
        }
    }
}

/// One retention pass; returns how many indexes it evicted.
///
/// Why: see the module doc. Rung 5: this deletes corpora, so every unknown
/// keeps the index.
/// What: evicts nothing when tm's active set is not trusted (stale, absent,
/// empty), or the registry or the map cannot be read. Otherwise each
/// registered row that is not loaded, warm-pinned or reindexing gets a
/// [`retention_verdict`]; an `Evict` runs the delete core with
/// `delete_data: true` and `expected_root_path` set to the row's root, so a
/// root re-registered since the read is refused. A row with a non-UTF-8 root
/// is kept. Root probes run on the blocking pool.
/// Test: `an_idle_worktree_session_index_is_evicted_at_30_days`,
/// `retention_evicts_nothing_when_the_map_or_registry_is_unreadable`,
/// `retention_evicts_nothing_without_a_trusted_active_set`.
pub(super) async fn evict_idle_once(state: &Arc<SearchAppState>) -> usize {
    evict_idle_at(state, Instant::now(), now_unix()).await
}

/// [`evict_idle_once`] at an injected clock.
pub(super) async fn evict_idle_at(
    state: &Arc<SearchAppState>,
    now: Instant,
    now_unix: u64,
) -> usize {
    let registered = super::residency_pins::registered_in(state);
    // Design section 3: an untrusted active set means no evictions.
    let Some(members) = state.residency_pins.members(now, &registered) else {
        tracing::debug!("retention: active set not trusted; nothing evicted (#10534)");
        return 0;
    };
    let Some((rows, map)) = read_inputs(state) else {
        return 0;
    };
    let rows: Vec<PersistedIndex> = rows.into_iter().filter(|r| !busy(state, &r.id)).collect();
    let roots: Vec<PathBuf> = rows.iter().map(|r| r.root_path.clone()).collect();
    let Ok(kinds) = tokio::task::spawn_blocking(move || {
        roots.iter().map(|r| probe_kind(r)).collect::<Vec<_>>()
    })
    .await
    else {
        tracing::error!("retention: root probe task failed; nothing evicted (#10534)");
        return 0;
    };
    let mut evicted = 0usize;
    for (row, kind) in rows.iter().zip(kinds) {
        let park = map.parked_row(&row.id);
        let input = RetentionInput {
            class: retention_class(park.map(|p| p.reason), kind),
            last_activity_unix: last_activity(state, row),
            parked_unix: park.map(|p| p.parked_unix),
            live: members.contains(&row.id),
        };
        let Verdict::Evict { class, idle_secs } = retention_verdict(input, now_unix) else {
            continue;
        };
        if evict(state, row, class, idle_secs).await {
            evicted += 1;
        }
    }
    evicted
}

/// Delete `row` with its data, pinned to its root; `true` when it was removed.
async fn evict(
    state: &Arc<SearchAppState>,
    row: &PersistedIndex,
    class: RetentionClass,
    idle_secs: u64,
) -> bool {
    let Some(root) = row.root_path.to_str() else {
        tracing::warn!(
            "retention: '{}' has a non-UTF-8 root; kept (#10534)",
            row.id
        );
        return false;
    };
    let params = super::DeleteIndexParams {
        delete_data: true,
        expected_root_path: Some(root.to_string()),
    };
    match super::search::delete_index_report(state, &row.id, params).await {
        Ok(_) => {
            tracing::info!(
                index_id = %row.id,
                idle_days = idle_secs / 86_400,
                "retention: evicted {class:?} index '{}' ({root}) after 30 days idle; \
                 corpus deleted (#10534)",
                row.id
            );
            true
        }
        Err((status, body)) => {
            tracing::error!(
                "retention: evict of '{}' refused ({status}): {body}; kept (#10534)",
                row.id
            );
            false
        }
    }
}
