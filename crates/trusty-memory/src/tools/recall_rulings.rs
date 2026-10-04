//! User-scope rulings in every project palace's recall at L1 (#9143).
//!
//! Why: an owner ruling stored in one palace could not be recalled from
//! another. Standing rules are user-scope by nature — they constrain how work
//! is done in every project — so a project recall must reach them without
//! fanning out over the whole estate, which costs seconds (#9138 O3). The leg
//! is optional and generic: trusty-memory names no palace, and with
//! [`RULINGS_PALACES_ENV`] unset recall is exactly what it was before.
//! What: [`RulingsLeg`] holds the operator's palace list, the per-recall time
//! bound and a short cache of failed palaces. [`fetch_user_rulings`] searches
//! every listed palace concurrently, each on the blocking pool under
//! [`RULINGS_TIMEOUT`]; [`fold_rulings`] keeps ruling-tagged drawers
//! ([`super::recall_rank::is_ruling`]), drops duplicates, caps the leg's share
//! and merges the rest into the project recall at layer 1. Every palace that
//! failed comes back as a [`RulingsDegraded`] entry, which the recall
//! envelope reports as `rulings_degraded`; the project's own hits are always
//! returned.
//! Test: `tests/recall_rulings_leg.rs`; `tools::recall_rulings_tests`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use trusty_common::memory_core::embed::Embedder;
use trusty_common::memory_core::palace::PalaceId;
use trusty_common::memory_core::registry::PalaceRegistry;
use trusty_common::memory_core::retrieval::{
    expand_query, retrieve_l2_scoped, RecallResult, RecallScope,
};

use super::bm25::{bm25_search_optional, fuse_bm25_into_recall};
use super::helpers::open_palace_handle;
use super::recall_rank::is_ruling;
use crate::AppState;

/// Environment variable naming the user-scope rulings palaces.
///
/// What: a comma-separated list of palace ids, e.g. `<palace>,<palace>`.
/// Unset or blank means no user-scope leg.
pub const RULINGS_PALACES_ENV: &str = "TRUSTY_MEMORY_RULINGS_PALACES";

/// Upper bound on the whole rulings leg of one recall.
///
/// Why (#9143 review): a slow or hung rulings palace must not stall the
/// project recall. Palaces run concurrently, so this bounds the leg, not each
/// palace in turn.
pub const RULINGS_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a failed rulings palace is skipped before it is tried again.
///
/// Why: an absent, unreadable or hung palace would otherwise cost an open or
/// a full [`RULINGS_TIMEOUT`] on every recall. It is still reported as
/// degraded while skipped.
pub const RULINGS_RETRY_AFTER: Duration = Duration::from_secs(60);

/// Candidates fetched from each rulings palace before the ruling filter.
///
/// Why: a rulings palace also holds non-ruling drawers, and the filter runs
/// after the vector search, so asking for `top_k` alone could return none.
/// What: `max(top_k * 4, 32)`; a rulings palace is small (tens to hundreds
/// of drawers), so the wider HNSW search is cheap.
fn rulings_window(top_k: usize) -> usize {
    top_k.saturating_mul(4).max(32)
}

/// One rulings palace the leg could not use, as the recall envelope shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RulingsDegraded {
    /// The configured palace id.
    pub palace: String,
    /// Why it contributed nothing: absent, open failed, search failed,
    /// timed out, or skipped because it failed within [`RULINGS_RETRY_AFTER`].
    pub reason: String,
}

/// What one rulings palace returned: its hits, or the reason it failed.
pub(crate) type PalaceOutcome = (String, Result<Vec<RecallResult>, String>);

/// The configured user-scope rulings leg (#9143).
///
/// Why: the palace list, the time bound and the failed-palace cache travel
/// together on `AppState`, so a recall reads one value and tests can replace
/// the list or the bound without the process environment.
/// What: an immutable, de-duplicated palace list and timeout, plus a mutex
/// guarded map of palace id to (failure time, reason).
/// Test: `a_failed_palace_is_skipped_until_the_retry_window_passes`.
pub struct RulingsLeg {
    palaces: Vec<String>,
    timeout: Duration,
    failed: parking_lot::Mutex<HashMap<String, (Instant, String)>>,
}

impl RulingsLeg {
    /// A leg over `palaces` (de-duplicated, order kept) bounded by `timeout`.
    pub fn new(palaces: Vec<String>, timeout: Duration) -> Self {
        let mut seen = HashSet::new();
        let palaces = palaces
            .into_iter()
            .filter(|p| seen.insert(p.clone()))
            .collect();
        Self {
            palaces,
            timeout,
            failed: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    /// The leg configured in [`RULINGS_PALACES_ENV`], validated once.
    ///
    /// Why: read once per `AppState`, like `PalaceRegistry::from_env`, so the
    /// recall hot path never touches the environment; an odd value (blank
    /// entries, duplicates) is reported here at warn, once, not per recall.
    /// What: the parsed list, or empty when the variable is unset or not UTF-8.
    pub fn from_env() -> Self {
        let raw = std::env::var(RULINGS_PALACES_ENV).unwrap_or_default();
        let palaces = parse_rulings_palaces(&raw);
        let entries = raw.split(',').count();
        if !raw.trim().is_empty() && entries != palaces.len() {
            tracing::warn!(
                "#9143: {RULINGS_PALACES_ENV} has {entries} entries but {} distinct \
                 palace ids; blank and repeated entries are ignored",
                palaces.len()
            );
        }
        Self::new(palaces, RULINGS_TIMEOUT)
    }

    /// The configured palace ids.
    pub fn palaces(&self) -> &[String] {
        &self.palaces
    }

    /// The reason `palace` failed, when that was within [`RULINGS_RETRY_AFTER`].
    fn cached_failure(&self, palace: &str) -> Option<String> {
        let failed = self.failed.lock();
        let (at, reason) = failed.get(palace)?;
        (at.elapsed() < RULINGS_RETRY_AFTER).then(|| format!("{reason} (cached; not retried)"))
    }

    /// Remember a failure, or forget one once the palace answers again.
    fn record(&self, palace: &str, outcome: &Result<Vec<RecallResult>, String>) {
        let mut failed = self.failed.lock();
        match outcome {
            Ok(_) => {
                failed.remove(palace);
            }
            Err(reason) => {
                failed.insert(palace.to_string(), (Instant::now(), reason.clone()));
            }
        }
    }
}

/// Parse a comma-separated palace list: blanks and surrounding space dropped,
/// repeats kept once in first-seen order.
pub fn parse_rulings_palaces(raw: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty() && seen.insert(*s))
        .map(str::to_string)
        .collect()
}

impl AppState {
    /// Replace the user-scope rulings palaces (#9143), keeping the time bound.
    ///
    /// Why: tests and embedders need the list without writing the process
    /// environment, which every other test in the binary would see.
    #[must_use]
    pub fn with_rulings_palaces(mut self, palaces: Vec<String>) -> Self {
        self.rulings = Arc::new(RulingsLeg::new(palaces, self.rulings.timeout));
        self
    }

    /// Replace the rulings leg's time bound (#9143), keeping the palace list.
    #[must_use]
    pub fn with_rulings_timeout(mut self, timeout: Duration) -> Self {
        self.rulings = Arc::new(RulingsLeg::new(self.rulings.palaces.clone(), timeout));
        self
    }
}

/// Whether the rulings leg runs for this recall.
///
/// Why (#9143 review): a room- or wing-scoped recall asked for one slice of
/// one palace; a ruling from another palace is outside that slice. A
/// multi-tenant daemon has no per-caller authorization for the rulings
/// palaces yet, so the leg fails closed there rather than leak across tenants.
/// What: `true` only with palaces configured, `RecallScope::All`, and
/// single-tenant mode.
/// Test: `a_room_scoped_recall_skips_the_rulings_leg`,
/// `multi_tenant_mode_skips_the_rulings_leg`.
fn leg_applies(state: &AppState, scope: &RecallScope) -> bool {
    // #9143: scope and tenancy gates, before any palace is opened.
    !state.rulings.palaces.is_empty()
        && matches!(scope, RecallScope::All)
        && !state.multi_tenant_mode
}

/// Search every configured rulings palace other than `target`, concurrently.
///
/// Why: see the module doc. Running here, beside the project lanes, keeps the
/// leg off the recall's critical path except for its own bound.
/// What: empty when [`leg_applies`] is false or `top_k` is 0. Otherwise, for
/// each palace, a cached failure is returned as is; anything else runs
/// [`search_one_bounded`] and the outcome updates the failure cache. Failures
/// are logged at warn when first seen.
/// Test: `an_absent_rulings_palace_degrades_and_is_not_retried_at_once`.
pub(crate) async fn fetch_user_rulings(
    state: &AppState,
    target: &PalaceId,
    embedder: Arc<dyn Embedder + Send + Sync>,
    query: &str,
    scope: &RecallScope,
    top_k: usize,
) -> Vec<PalaceOutcome> {
    if top_k == 0 || !leg_applies(state, scope) {
        return Vec::new();
    }
    let leg = state.rulings.clone();
    let search = Arc::new(LegSearch {
        expanded: expand_query(query),
        query: query.to_string(),
        window: rulings_window(top_k),
        target: target.clone(),
        embedder,
    });
    let legs = leg
        .palaces
        .iter()
        .filter(|p| p.as_str() != target.as_str())
        .map(|palace| {
            let (leg, search, state) = (leg.clone(), search.clone(), state.clone());
            let palace = palace.clone();
            async move {
                if let Some(reason) = leg.cached_failure(&palace) {
                    tracing::debug!(palace = %palace, "#9143: rulings palace skipped: {reason}");
                    return (palace, Err(reason));
                }
                let outcome = search_one_bounded(state, &palace, search, leg.timeout).await;
                if let Err(reason) = &outcome {
                    tracing::warn!(palace = %palace, "#9143: rulings palace degraded: {reason}");
                }
                leg.record(&palace, &outcome);
                (palace, outcome)
            }
        });
    futures::future::join_all(legs).await
}

/// The per-recall inputs every rulings palace search shares.
struct LegSearch {
    expanded: String,
    query: String,
    window: usize,
    target: PalaceId,
    embedder: Arc<dyn Embedder + Send + Sync>,
}

/// Open and search one rulings palace on the blocking pool, under `timeout`.
///
/// Why: a palace open is disk I/O and the L2 join takes the palace's drawer
/// lock synchronously; either can stall. On the blocking pool a stall holds a
/// blocking thread, never a tokio worker, and the timeout returns the recall.
/// Score scale (#9143 review): the project hits carry the vector score plus an
/// RRF bonus from the project palace's BM25 lane, so each rulings palace's hits
/// get the same fusion from that palace's own BM25 lane — one scorer, one
/// scale. With no BM25 lane for that palace a ruling lacks the bonus (at most
/// `1/61`), which errs toward the project hit.
/// What: `Err` with a reason for an absent palace, any other open failure, a
/// search failure, a panicked task, or the timeout; `Ok(vec![])` when the
/// palace is an alias of the project palace.
/// Test: `a_hung_rulings_palace_returns_within_the_bound_as_degraded`.
async fn search_one_bounded(
    state: AppState,
    palace: &str,
    search: Arc<LegSearch>,
    timeout: Duration,
) -> Result<Vec<RecallResult>, String> {
    let palace = palace.to_string();
    // #9143: blocking pool, so a hung open or lock never blocks a worker.
    let task = tokio::task::spawn_blocking(move || {
        let handle = match open_palace_handle(&state, &palace) {
            Ok(h) => h,
            Err(e) if PalaceRegistry::open_error_is_absent(&e) => {
                return Err("absent: no palace with this id".to_string());
            }
            Err(e) => return Err(format!("open failed: {e:#}")),
        };
        if handle.id == search.target {
            return Ok(Vec::new()); // an alias of the project palace itself
        }
        tokio::runtime::Handle::current().block_on(async {
            let s = &search;
            let mut hits = retrieve_l2_scoped(
                &handle,
                s.embedder.as_ref(),
                &s.expanded,
                &RecallScope::All,
                s.window,
            )
            .await
            .map_err(|e| format!("search failed: {e:#}"))?;
            let lexical = bm25_search_optional(&state, handle.id.as_str(), &s.query, s.window);
            if let Some(bm25_hits) = lexical.await {
                fuse_bm25_into_recall(&mut hits, &bm25_hits, s.window);
            }
            Ok(hits)
        })
    });
    match tokio::time::timeout(timeout, task).await {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(join)) => Err(format!("search task failed: {join}")),
        Err(_) => Err(format!("timed out after {} ms", timeout.as_millis())),
    }
}

/// Merge the rulings leg's outcomes into `results` and report the failures.
///
/// Why: layer 1 marks the hits as grounding rather than a project search hit,
/// matching #9143's "at L1". `min_score` is applied here because
/// `apply_score_floor` exempts layer 1 by design. The cap keeps the leg from
/// crowding the project's own answer (#9143 review).
/// What: every `Err` becomes a [`RulingsDegraded`]. From the `Ok` hits it keeps
/// ruling-tagged drawers at or above `min_score` whose drawer id and content
/// hash are new — against `results` and against every earlier ruling, so one
/// ruling stored in two palaces appears once — then the best
/// `ceil(top_k / 3)` by score, appended as layer 1. `results` is never
/// shortened; the caller re-sorts.
/// Test: `a_search_error_degrades_and_keeps_every_primary_hit`,
/// `the_same_ruling_from_two_palaces_appears_once`,
/// `rulings_contribute_at_most_a_third_of_top_k`.
pub(crate) fn fold_rulings(
    results: &mut Vec<RecallResult>,
    outcomes: Vec<PalaceOutcome>,
    top_k: usize,
    min_score: Option<f32>,
) -> Vec<RulingsDegraded> {
    let mut degraded = Vec::new();
    let mut ids: HashSet<_> = results.iter().map(|r| r.drawer.id).collect();
    let mut hashes: HashSet<_> = results.iter().map(|r| r.drawer.content_hash()).collect();
    let mut rulings = Vec::new();
    for (palace, outcome) in outcomes {
        match outcome {
            Err(reason) => degraded.push(RulingsDegraded { palace, reason }),
            Ok(hits) => {
                for hit in hits {
                    let admitted = is_ruling(&hit.drawer)
                        && min_score.is_none_or(|floor| hit.score >= floor)
                        // #9143: dedup by drawer id AND by content hash.
                        && ids.insert(hit.drawer.id)
                        && hashes.insert(hit.drawer.content_hash());
                    if admitted {
                        rulings.push(hit);
                    }
                }
            }
        }
    }
    rulings.sort_by(|a, b| b.score.total_cmp(&a.score));
    // #9143: the leg contributes at most ceil(top_k / 3) hits.
    rulings.truncate(top_k.div_ceil(3));
    for mut hit in rulings {
        hit.layer = 1;
        results.push(hit);
    }
    degraded
}

#[cfg(test)]
#[path = "recall_rulings_tests.rs"]
mod recall_rulings_tests;
