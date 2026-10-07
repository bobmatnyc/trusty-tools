//! Bounded, streaming palace fan-out for the recall-all family (issue #7125).
//!
//! Why: the three recall-all entry points opened every palace on disk into one
//! `Vec<Arc<PalaceHandle>>` and held all of them for the whole query. On a
//! 94-palace estate that hydrates the drawer table, HNSW graph, KG adjacency
//! and redb page cache of all 94 at once — roughly 13x the on-disk bytes — and
//! when the local `Vec` drops the LRU is left holding its 64 most recent
//! victims, a multi-GB floor that persists until the idle sweep runs. The
//! 64-slot cap bounded the cache, never the peak, and never the residue. Peak
//! residency must track the batch size, not the palace count, and the daemon's
//! steady state after a recall-all must match its steady state before one.
//! What: [`recall_streamed`] walks the palace list in [`RECALL_PALACE_BATCH`]
//! batches. Each batch is opened on the blocking pool, handed to the caller's
//! search, then released — so at most one batch is resident at a time. Palaces
//! the registry already held when the call started are never released, so an
//! operator's working set survives a recall-all untouched. Per-batch results
//! are merged with the same dedup-by-drawer-id/sort/truncate rule
//! `recall_across_palaces` applies within a batch, so the global top-k is
//! unchanged: it is always a subset of the union of the per-batch top-ks.
//!
//! ADR-0071 (#9299) adds the scope and the coverage report. The default
//! [`RecallAllScope::Resident`] searches only the palaces the registry holds
//! and opens none; [`RecallAllScope::All`] is the streamed walk above.
//! [`recall_all_scoped`] runs either and returns a [`RecallCoverage`] that
//! counts every palace on disk as searched, skipped, or not searched.
//! Test: `recall_all_returns_open_palaces_to_baseline` and
//! `recall_all_keeps_palaces_that_were_already_open` in
//! `tests/recall_all_residency.rs`;
//! `recall_all_releases_every_batch_when_the_search_fails` and
//! `recall_streamed_visits_every_palace_in_bounded_batches` in
//! `recall_stream_tests`; the ADR-0071 criteria in `recall_scope_tests`.

use crate::AppState;
use anyhow::{bail, Result};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use trusty_common::memory_core::palace::{Palace, PalaceId};
use trusty_common::memory_core::retrieval::CrossPalaceResult;
use trusty_common::memory_core::PalaceHandle;
use uuid::Uuid;

use super::helpers::{list_palaces_blocking, open_palaces_blocking};

/// Which palaces a recall_all searches (ADR-0071 D1, D3).
///
/// Why (#9299): the old fan-out cold-opened every non-resident palace on each
/// call. The default now answers from the resident set; a caller that needs
/// the whole estate asks for it.
/// What: `Resident` (default) searches the palaces the registry holds at call
/// start and opens none. `All` runs the streamed walk over every non-empty
/// palace on disk, with no time budget.
/// Test: `recall_all_rejects_an_unknown_scope`,
/// `default_scope_opens_no_palace_and_keeps_the_resident_set`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RecallAllScope {
    /// Search resident palaces only.
    #[default]
    Resident,
    /// Search every non-empty palace on disk, opening as needed.
    All,
}

impl RecallAllScope {
    /// Parse the optional `scope` argument; absent or null means `Resident`.
    ///
    /// Why: an unknown value must not fall back to a default the caller did not
    /// ask for, so it is an error rather than `Resident`.
    /// Test: `recall_all_rejects_an_unknown_scope`.
    pub fn parse(value: Option<&Value>) -> Result<Self> {
        match value {
            None | Some(Value::Null) => Ok(Self::Resident),
            Some(Value::String(s)) if s == "resident" => Ok(Self::Resident),
            Some(Value::String(s)) if s == "all" => Ok(Self::All),
            Some(other) => {
                bail!("unknown recall_all scope {other}; expected \"resident\" or \"all\"")
            }
        }
    }

    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Resident => "resident",
            Self::All => "all",
        }
    }
}

/// How much of the estate a recall_all searched (ADR-0071 D4).
///
/// Why (#9299): a partial answer must say it is partial. The old response
/// counted attempted opens as searched and dropped open failures silently.
/// What: per-reason counts for every palace on disk. The invariant
/// `palaces_total == palaces_searched + palaces_skipped + palaces_not_searched()`
/// holds by construction in [`recall_all_scoped`] and is asserted in
/// [`Self::insert_into`].
/// Test: `open_failure_is_reported_on_every_surface`,
/// `default_scope_opens_no_palace_and_keeps_the_resident_set`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecallCoverage {
    /// Palaces on disk at call start.
    pub palaces_total: usize,
    /// Palaces actually searched.
    pub palaces_searched: usize,
    /// Provably empty palaces, skipped unopened (#9141).
    pub palaces_skipped: usize,
    /// Non-resident, non-empty palaces the default scope did not search.
    pub not_resident: usize,
    /// Non-resident palaces left unclassified because the empty check failed.
    pub filter_failed: usize,
    /// Palaces left unsearched because `top_k` was 0.
    pub top_k_zero: usize,
    /// Ids of palaces that failed to open, in walk order.
    pub open_failed: Vec<String>,
}

impl RecallCoverage {
    /// Palaces on disk that were neither searched nor skipped as empty.
    pub fn palaces_not_searched(&self) -> usize {
        self.not_resident + self.filter_failed + self.top_k_zero + self.open_failed.len()
    }

    /// Add the ADR-0071 D4 fields and `scope` to a response object.
    ///
    /// What: `coverage` is `"complete"` only when nothing went unsearched.
    /// `not_searched_by_reason` lists only reasons with a non-zero count.
    pub fn insert_into(&self, scope: RecallAllScope, out: &mut Map<String, Value>) {
        let not_searched = self.palaces_not_searched();
        debug_assert_eq!(
            self.palaces_total,
            self.palaces_searched + self.palaces_skipped + not_searched,
            "ADR-0071 D4 coverage invariant"
        );
        let mut by_reason = BTreeMap::new();
        for (reason, n) in [
            ("not_resident", self.not_resident),
            ("open_failed", self.open_failed.len()),
            ("filter_failed", self.filter_failed),
            ("top_k_zero", self.top_k_zero),
        ] {
            if n > 0 {
                by_reason.insert(reason, n);
            }
        }
        let coverage = if not_searched == 0 {
            "complete"
        } else {
            "partial"
        };
        out.insert("scope".into(), json!(scope.as_str()));
        out.insert("coverage".into(), json!(coverage));
        out.insert("palaces_total".into(), json!(self.palaces_total));
        out.insert("palaces_searched".into(), json!(self.palaces_searched));
        out.insert("palaces_skipped".into(), json!(self.palaces_skipped));
        out.insert("palaces_not_searched".into(), json!(not_searched));
        out.insert("not_searched_by_reason".into(), json!(by_reason));
        out.insert("open_failed".into(), json!(self.open_failed));
    }
}

/// A recall_all's merged hits and its coverage report.
#[derive(Debug)]
pub(crate) struct RecallAllOutcome {
    /// Merged, sorted, cut to the caller's window.
    pub(crate) results: Vec<CrossPalaceResult>,
    /// What was and was not searched.
    pub(crate) coverage: RecallCoverage,
}

/// Run a recall_all over `scope`, reporting coverage (ADR-0071, #9299).
///
/// Why: the three recall_all surfaces share one implementation, so the
/// scope, the coverage report and the idle-clock rule cannot drift apart.
/// What: lists the palaces on disk, then runs the resident path or the
/// streamed walk. The whole search runs inside
/// `PalaceHandle::without_idle_touch`, so no searched palace's idle clock is
/// reset (ADR-0071 D2). A palace that was kept but neither searched nor failed
/// can only come from `top_k == 0`, and is counted under `top_k_zero`.
/// Test: `default_scope_opens_no_palace_and_keeps_the_resident_set`,
/// `default_scope_does_not_reset_the_idle_clock`,
/// `open_failure_is_reported_on_every_surface`,
/// `scope_all_top5_matches_the_golden`,
/// `default_top5_is_the_resident_subset_of_scope_all`.
pub(crate) async fn recall_all_scoped<F, Fut>(
    state: &AppState,
    scope: RecallAllScope,
    label: &'static str,
    window: usize,
    search: F,
) -> Result<RecallAllOutcome>
where
    F: FnMut(Vec<Arc<PalaceHandle>>) -> Fut,
    Fut: Future<Output = Result<Vec<CrossPalaceResult>>>,
{
    let palaces = list_palaces_blocking(state).await?;
    PalaceHandle::without_idle_touch(async move {
        match scope {
            RecallAllScope::Resident => recall_resident(state, palaces, window, search).await,
            RecallAllScope::All => {
                let palaces_total = palaces.len();
                let (kept, palaces_skipped) = skip_empty_palaces(state, palaces).await;
                let streamed = recall_streamed(state, &kept, label, window, search).await?;
                let open_failed: Vec<String> = streamed
                    .open_failed
                    .iter()
                    .map(|id| id.as_str().to_string())
                    .collect();
                let top_k_zero = kept
                    .len()
                    .saturating_sub(streamed.searched + open_failed.len());
                Ok(RecallAllOutcome {
                    results: streamed.results,
                    coverage: RecallCoverage {
                        palaces_total,
                        palaces_searched: streamed.searched,
                        palaces_skipped,
                        top_k_zero,
                        open_failed,
                        ..RecallCoverage::default()
                    },
                })
            }
        }
    })
    .await
}

/// The default scope: search the resident palaces, open nothing (ADR-0071 D1).
///
/// Why (#9299): a cold open replays the HNSW graph and runs the stranded-point
/// scan, and the old fan-out paid that for every non-resident palace on every
/// call, then released it again.
/// What: takes each listed palace's handle with `PalaceRegistry::peek`, which
/// neither opens nor promotes it in the LRU. The rest are classified on the
/// blocking pool as skipped (provably empty) or `not_resident`. If that check
/// itself fails, they are counted under `filter_failed`. The resident handles
/// are searched in `RECALL_PALACE_BATCH` chunks and merged with the streamed
/// path's rule.
/// Test: `default_scope_opens_no_palace_and_keeps_the_resident_set`.
async fn recall_resident<F, Fut>(
    state: &AppState,
    palaces: Vec<Palace>,
    window: usize,
    mut search: F,
) -> Result<RecallAllOutcome>
where
    F: FnMut(Vec<Arc<PalaceHandle>>) -> Fut,
    Fut: Future<Output = Result<Vec<CrossPalaceResult>>>,
{
    let palaces_total = palaces.len();
    let mut resident: Vec<Arc<PalaceHandle>> = Vec::new();
    let mut cold: Vec<Palace> = Vec::new();
    for palace in palaces {
        match state.registry.peek(&palace.id) {
            Some(handle) => resident.push(handle),
            None => cold.push(palace),
        }
    }
    let cold_count = cold.len();
    let mut coverage = RecallCoverage {
        palaces_total,
        ..RecallCoverage::default()
    };
    match tokio::task::spawn_blocking(move || cold.iter().filter(|p| is_empty_on_disk(p)).count())
        .await
    {
        Ok(empty) => {
            coverage.palaces_skipped = empty;
            coverage.not_resident = cold_count - empty;
        }
        Err(e) => {
            // #9299: an unclassified palace is reported, never silently dropped.
            tracing::warn!("recall-all empty-palace filter failed: {e}");
            coverage.filter_failed = cold_count;
        }
    }
    if window == 0 {
        coverage.top_k_zero = resident.len();
        return Ok(RecallAllOutcome {
            results: Vec::new(),
            coverage,
        });
    }
    coverage.palaces_searched = resident.len();
    let mut merger = Merger::default();
    for batch in resident.chunks(RECALL_PALACE_BATCH) {
        merger.add(search(batch.to_vec()).await?);
    }
    Ok(RecallAllOutcome {
        results: merger.finish(window),
        coverage,
    })
}

/// What [`recall_streamed`] searched, and which palaces it could not open.
#[derive(Debug)]
pub(crate) struct Streamed {
    /// Merged, sorted, cut to `top_k`.
    pub(crate) results: Vec<CrossPalaceResult>,
    /// Palaces opened and handed to the search.
    pub(crate) searched: usize,
    /// Palaces that failed to open (#9299).
    pub(crate) open_failed: Vec<PalaceId>,
}

/// How many palaces a recall-all holds open at once (issue #7125).
///
/// Why: this is the peak-residency knob. Each open palace costs its drawer
/// table, HNSW graph, KG adjacency and redb page cache — on the estate that
/// prompted #7125, ~13x its on-disk bytes across 94 palaces. Eight keeps the
/// fan-out's own concurrency (`recall_across_palaces` joins the batch) worth
/// having while bounding peak residency at roughly an eighth of the old
/// whole-estate open. Raising it trades RAM for wall-clock; lowering it does
/// the reverse.
/// What: the chunk width [`recall_streamed`] slices the palace list into.
/// Test: `recall_all_returns_open_palaces_to_baseline` exercises an estate
/// wider than one batch, so the batching loop actually iterates.
pub(crate) const RECALL_PALACE_BATCH: usize = 8;

/// Run `search` over every palace in `palaces`, one bounded batch at a time.
///
/// Why (#7125): see the module doc — the old shape's peak and its residue both
/// scaled with the palace count. This is the shared replacement, so all three
/// recall-all entry points (`MemoryService::recall_all`,
/// `handle_memory_recall_all`, `chat::tools::execute_recall_all`) inherit the
/// bound rather than each growing their own.
/// What: snapshots which palaces the registry already holds, then for each
/// batch opens the handles (on the blocking pool, via `open_palaces_blocking`),
/// awaits `search`, and releases every handle the batch itself brought in.
/// Release runs BEFORE the caller's error is propagated, so a failing search
/// cannot leak a batch. `search` takes the handles by value and its future is
/// dropped before the release, so the registry sees `strong_count == 1` and
/// `release_if_unreferenced` can actually pop; a palace another task is holding
/// stays put by that same guard. Results are merged across batches by drawer
/// id (highest score wins), sorted by score descending, and truncated to
/// `top_k`. The ids of palaces that failed to open are returned, not dropped
/// (#9299).
/// Test: `recall_all_returns_open_palaces_to_baseline`,
/// `recall_all_keeps_palaces_that_were_already_open`,
/// `recall_all_releases_every_batch_when_the_search_fails`,
/// `recall_streamed_visits_every_palace_in_bounded_batches`.
pub(crate) async fn recall_streamed<F, Fut>(
    state: &AppState,
    palaces: &[Palace],
    label: &'static str,
    top_k: usize,
    mut search: F,
) -> Result<Streamed>
where
    F: FnMut(Vec<Arc<PalaceHandle>>) -> Fut,
    Fut: Future<Output = Result<Vec<CrossPalaceResult>>>,
{
    let mut streamed = Streamed {
        results: Vec::new(),
        searched: 0,
        open_failed: Vec::new(),
    };
    if palaces.is_empty() || top_k == 0 {
        return Ok(streamed);
    }
    // #7125: palaces already resident when the query arrived are the operator's
    // working set, not this query's residue — releasing them would evict a warm
    // cache the next request is about to want.
    let pinned = resident_ids(state);

    let mut merger = Merger::default();

    for batch in palaces.chunks(RECALL_PALACE_BATCH) {
        let outcome = open_palaces_blocking(state, batch, label).await;
        // #9299: a failed open is reported in the coverage, not just logged.
        streamed.open_failed.extend(outcome.failed);
        let handles = outcome.opened;
        if handles.is_empty() {
            // Every palace in this batch failed to open. Nothing to release,
            // nothing to search.
            continue;
        }
        let opened: Vec<PalaceId> = handles.iter().map(|h| h.id.clone()).collect();
        let found = search(handles).await;
        // #7125: release before `?` — an erroring search must not leak a batch.
        release_batch(state, &opened, &pinned);
        merger.add(found?);
        streamed.searched += opened.len();
    }

    streamed.results = merger.finish(top_k);
    Ok(streamed)
}

/// Split `palaces` into the ones a recall-all must search and a count of the
/// empty ones it can skip without opening.
///
/// Why (#9141): a recall-all opened every palace on disk, and on the live
/// estate 61 of 104 held no drawer — about a quarter of the fan-out's cold-open
/// time spent proving there was nothing to find.
/// What: a palace the registry already holds is kept (its open costs nothing).
/// Any other palace is read with `console_metrics::disk_stats::read`, a
/// shared-lock header read that never opens the palace; it is skipped only
/// when it reports 0 drawers and 0 vectors AND the palace has no L1 snapshot
/// drawer. A read that fails (no store yet, a writer holds it) keeps the
/// palace, so a skip is never a guess. Runs on the blocking pool.
/// Test: `recall_all_skips_empty_palaces_without_opening_them`.
pub(crate) async fn skip_empty_palaces(
    state: &AppState,
    palaces: Vec<Palace>,
) -> (Vec<Palace>, usize) {
    let registry = Arc::clone(&state.registry);
    let all = palaces.clone();
    let kept = tokio::task::spawn_blocking(move || {
        palaces
            .into_iter()
            .filter(|p| registry.peek(&p.id).is_some() || !is_empty_on_disk(p))
            .collect::<Vec<_>>()
    })
    .await;
    match kept {
        Ok(kept) => {
            let skipped = all.len() - kept.len();
            (kept, skipped)
        }
        Err(e) => {
            // A failed filter must not hide the corpus: search everything.
            tracing::warn!("recall-all empty-palace filter failed: {e}");
            (all, 0)
        }
    }
}

/// True only when `palace` is provably empty on disk. See [`skip_empty_palaces`].
fn is_empty_on_disk(palace: &Palace) -> bool {
    let empty_store = crate::console_metrics::disk_stats::read(&palace.data_dir)
        .is_ok_and(|s| s.drawer_count == 0 && s.vector_count == 0);
    empty_store
        && trusty_common::memory_core::store::L1Cache::load_l1_cache(&palace.data_dir)
            .is_ok_and(|l1| l1.is_empty())
}

/// Ids the registry holds open right now.
///
/// Why (#7125): the baseline a recall-all must return to. Taken once, before
/// the first batch opens anything, so it names the caller's working set rather
/// than the query's own footprint.
/// What: snapshots `PalaceRegistry::list` into a set for O(1) membership.
/// Test: `recall_all_keeps_palaces_that_were_already_open`.
fn resident_ids(state: &AppState) -> HashSet<PalaceId> {
    state.registry.list().into_iter().collect()
}

/// Hand back the handles this batch opened, keeping the pre-call working set.
///
/// Why (#7125): without this the LRU keeps whatever the fan-out last touched,
/// so the daemon's floor after a recall-all is `min(palace_count, max_open)`
/// fully hydrated palaces regardless of what the operator was actually using.
/// What: for each id the batch opened that was NOT already resident, calls
/// `PalaceRegistry::release_if_unreferenced` (#7106), which pops only when the
/// cache holds the sole reference — so a concurrent recall, remember, or dream
/// cycle holding the same `Arc` is never disturbed. redb stays the source of
/// truth; the next access transparently reopens.
/// Test: `recall_all_returns_open_palaces_to_baseline`,
/// `recall_all_keeps_palaces_that_were_already_open`.
fn release_batch(state: &AppState, opened: &[PalaceId], pinned: &HashSet<PalaceId>) {
    for id in opened {
        if pinned.contains(id) {
            continue;
        }
        state.registry.release_if_unreferenced(id);
    }
}

/// The running cross-palace result set, merged one batch at a time.
///
/// Why (#7125): batching splits a fan-out that used to merge once, so the
/// dedup-by-drawer-id rule `recall_across_palaces` applies within a batch has
/// to be reapplied across batches or the same drawer could appear twice. The
/// resident path (#9299) merges with the same rule, so both scopes rank alike.
/// What: `add` keeps the highest-scoring occurrence of each drawer id,
/// indexing through `by_drawer` so a later higher score replaces in place;
/// `finish` sorts by score descending and truncates.
/// Test: `recall_all_returns_open_palaces_to_baseline` drives the merge across
/// more than one batch.
#[derive(Default)]
struct Merger {
    merged: Vec<CrossPalaceResult>,
    by_drawer: HashMap<Uuid, usize>,
}

impl Merger {
    fn add(&mut self, hits: Vec<CrossPalaceResult>) {
        for candidate in hits {
            let drawer_id = candidate.result.drawer.id;
            match self.by_drawer.get(&drawer_id).copied() {
                Some(idx) if self.merged[idx].result.score >= candidate.result.score => {}
                Some(idx) => self.merged[idx] = candidate,
                None => {
                    self.by_drawer.insert(drawer_id, self.merged.len());
                    self.merged.push(candidate);
                }
            }
        }
    }

    fn finish(mut self, top_k: usize) -> Vec<CrossPalaceResult> {
        self.merged.sort_by(|a, b| {
            b.result
                .score
                .partial_cmp(&a.result.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        self.merged.truncate(top_k);
        self.merged
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::{RecallAllScope, RecallCoverage};
    use serde_json::{json, Map};

    /// Why (#9299): each not-searched reason must reach the wire and turn
    /// `coverage` partial; a filter failure has no runtime route in a test,
    /// so its reporting is pinned here.
    /// What: one row per reason, plus the all-searched row.
    /// Test: this test.
    #[test]
    fn every_not_searched_reason_is_reported_and_partial() {
        let base = RecallCoverage {
            palaces_total: 3,
            palaces_searched: 1,
            palaces_skipped: 1,
            ..RecallCoverage::default()
        };
        let rows = [
            (
                "not_resident",
                RecallCoverage {
                    not_resident: 1,
                    ..base.clone()
                },
            ),
            (
                "filter_failed",
                RecallCoverage {
                    filter_failed: 1,
                    ..base.clone()
                },
            ),
            (
                "top_k_zero",
                RecallCoverage {
                    top_k_zero: 1,
                    ..base.clone()
                },
            ),
            (
                "open_failed",
                RecallCoverage {
                    open_failed: vec!["p".into()],
                    ..base.clone()
                },
            ),
        ];
        for (reason, cov) in rows {
            let mut out = Map::new();
            cov.insert_into(RecallAllScope::All, &mut out);
            assert_eq!(out["coverage"], "partial", "{reason}");
            assert_eq!(out["palaces_not_searched"], 1, "{reason}");
            assert_eq!(out["not_searched_by_reason"], json!({ reason: 1 }));
        }
        let mut out = Map::new();
        let complete = RecallCoverage {
            palaces_total: 2,
            ..base
        };
        complete.insert_into(RecallAllScope::Resident, &mut out);
        assert_eq!(out["coverage"], "complete");
        assert_eq!(out["scope"], "resident");
        assert_eq!(out["not_searched_by_reason"], json!({}));
    }

    /// Why: absent and null mean the default; anything unknown is an error.
    /// Test: this test.
    #[test]
    fn scope_parses_known_values_and_rejects_the_rest() {
        assert_eq!(
            RecallAllScope::parse(None).ok(),
            Some(RecallAllScope::Resident)
        );
        assert_eq!(
            RecallAllScope::parse(Some(&json!(null))).ok(),
            Some(RecallAllScope::Resident)
        );
        assert_eq!(
            RecallAllScope::parse(Some(&json!("all"))).ok(),
            Some(RecallAllScope::All)
        );
        for bad in [json!("ALL"), json!(""), json!(true)] {
            assert!(RecallAllScope::parse(Some(&bad)).is_err(), "{bad}");
        }
    }
}
