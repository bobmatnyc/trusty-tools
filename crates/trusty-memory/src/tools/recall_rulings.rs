//! User-scope rulings in every project palace's recall at L1 (#9143).
//!
//! Why: an owner ruling stored in one palace could not be recalled from
//! another. Standing rules are user-scope by nature — they constrain how work
//! is done in every project — so a project recall must reach them without
//! fanning out over the whole estate, which costs seconds (#9138 O3). The leg
//! is optional and generic: trusty-memory names no palace, and with
//! [`RULINGS_PALACES_ENV`] unset recall is exactly what it was before.
//! What: [`RulingsLeg`] holds the operator's palace list, the per-recall time
//! bound and one [`PalaceState`] per palace: the running search, if any, and
//! the last failure. [`fetch_user_rulings`] searches every listed palace
//! concurrently, each on the blocking pool under [`RULINGS_TIMEOUT`];
//! [`fold_rulings`] keeps ruling-tagged drawers
//! ([`super::recall_rank::is_ruling`]), drops duplicates, caps the leg's share
//! and merges the rest into the project recall at layer 1, naming the ones
//! that answer the query for the rank floor (`super::recall_rulings_floor`).
//! Every palace that contributed nothing comes back as a [`RulingsDegraded`]
//! entry with a fixed [`DegradedReason`] code, which the recall envelope
//! reports as `rulings_degraded`; the project's own hits are always returned.
//! Each palace also reads the `superseded_by` edges among its own hits from
//! its own KG while its handle is open (#9421), and the fold hands them to the
//! caller's demotion.
//! Test: `tests/recall_rulings_leg.rs`; `tools::recall_rulings_tests`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use trusty_common::memory_core::embed::Embedder;
use trusty_common::memory_core::palace::PalaceId;
use trusty_common::memory_core::registry::PalaceRegistry;
use trusty_common::memory_core::retrieval::{
    expand_query, retrieve_l2_scoped, RecallResult, RecallScope,
};
use uuid::Uuid;

use super::bm25::{bm25_search_optional, fuse_bm25_into_recall};
use super::helpers::open_palace_handle;
use super::recall_rank::is_ruling;
use super::recall_rulings_floor::ruling_answers_query;
use super::recall_supersede::{supersessions_for_within, Supersessions, LOOKUP_BUDGET};
use crate::commands::prompt_context::BODY_DEADLINE;
use crate::AppState;

/// Environment variable naming the user-scope rulings palaces.
///
/// What: a comma-separated list of palace ids, e.g. `<palace>,<palace>`.
/// Unset or blank means no user-scope leg. Read only by
/// [`AppState::with_rulings_palaces_from_env`], which the daemon calls.
pub const RULINGS_PALACES_ENV: &str = "TRUSTY_MEMORY_RULINGS_PALACES";

/// Upper bound on the whole rulings leg of one recall.
///
/// Why (#9143 review): a slow or hung rulings palace must not stall the
/// project recall. `memory_recall` joins the leg with the project lanes, so a
/// recall takes `max(project lanes, this bound)`, and the UserPromptSubmit
/// hook gives its whole body `BODY_DEADLINE` (1.5 s). At 500 ms a stalled
/// rulings palace leaves a second of that budget for the hook's other calls
/// and its compose; a warm rulings search is the same work as the project's
/// vector lane. Palaces run concurrently, so this bounds the leg, not each
/// palace in turn. A search that outlives the bound keeps running and counts
/// as stalled; see [`DegradedReason::InFlight`].
/// Test: `a_hung_rulings_palace_leaves_the_recall_inside_the_hook_budget`.
pub const RULINGS_TIMEOUT: Duration = Duration::from_millis(500);

// See #9143: the leg's bound stays at most half the hook's body budget.
const _: () = assert!(RULINGS_TIMEOUT.as_millis() * 2 <= BODY_DEADLINE.as_millis());

/// How long a failed rulings palace is skipped before it is tried again.
///
/// Why: an absent or unreadable palace would otherwise cost an open on every
/// recall. It is still reported, with `cached: true`, while skipped.
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

/// Why one rulings palace contributed nothing, as a fixed code.
///
/// Why (#9143 review): the reason reaches the recall caller, and an error's
/// text can carry a filesystem path or panic text. The code says which way
/// the palace failed; the full error chain goes to the `warn!` log only.
/// What: serialized as its snake_case name.
/// Test: `an_unreadable_rulings_palace_degrades_and_primary_hits_survive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DegradedReason {
    /// `absent`: no palace has this id.
    Absent,
    /// `unreadable`: the palace exists but could not be opened.
    Unreadable,
    /// `search_failed`: the palace opened but its vector search failed.
    SearchFailed,
    /// `task_failed`: the search task panicked or was dropped unrun.
    TaskFailed,
    /// `timed_out`: this recall's search outlived [`RULINGS_TIMEOUT`]. The
    /// search keeps running and records its own outcome when it ends.
    TimedOut,
    /// `in_flight`: a search an earlier recall started has run past
    /// [`RULINGS_TIMEOUT`] and is still running (stalled), so this recall
    /// started none.
    InFlight,
}

/// One rulings palace the leg could not use, as the recall envelope shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RulingsDegraded {
    /// The configured palace id.
    pub palace: String,
    /// Why it contributed nothing.
    pub reason: DegradedReason,
    /// `true` when `reason` is a failure recorded within
    /// [`RULINGS_RETRY_AFTER`] and the palace was not tried for this recall.
    pub cached: bool,
}

/// What one rulings palace returned: its hits, or why it contributed nothing.
pub(crate) type PalaceOutcome = Result<RulingsHits, RulingsDegraded>;

/// One rulings palace's hits and the supersessions among them (#9421).
///
/// Why: a ruling and the ruling that replaced it both live in the rulings
/// palace, so the `superseded_by` edge between them lives in that palace's
/// KG. The project palace's lookup cannot see it.
#[derive(Debug, Default)]
pub(crate) struct RulingsHits {
    /// The palace's ranked hits.
    pub hits: Vec<RecallResult>,
    /// `superseded -> replacement` among `hits`, from this palace's KG.
    pub superseded: Supersessions,
}

impl From<Vec<RecallResult>> for RulingsHits {
    fn from(hits: Vec<RecallResult>) -> Self {
        Self {
            hits,
            superseded: Supersessions::new(),
        }
    }
}

/// One rulings palace's search state (#9143 review).
///
/// Why: a timed-out search used to be detached and forgotten, so repeated
/// stalls piled up blocking tasks and a late success was thrown away.
/// What: `generation` counts the searches started; only the newest may record
/// an outcome. `running` maps each unfinished search's generation to its start
/// time, oldest first. `failure` is the last recorded failure and when it was
/// recorded.
#[derive(Debug, Default)]
struct PalaceState {
    generation: u64,
    running: BTreeMap<u64, Instant>,
    failure: Option<(Instant, DegradedReason)>,
}

/// The configured user-scope rulings leg (#9143).
///
/// Why: the palace list, the time bound and the per-palace state travel
/// together on `AppState`, so a recall reads one value and tests can replace
/// the list or the bound without the process environment.
/// What: an immutable, de-duplicated palace list and timeout, plus a mutex
/// guarded map of palace id to [`PalaceState`].
/// Test: `a_failed_palace_is_skipped_until_the_retry_window_passes`,
/// `a_stalled_search_admits_no_second_search`,
/// `overlapping_healthy_searches_are_each_admitted`.
pub struct RulingsLeg {
    palaces: Vec<String>,
    timeout: Duration,
    states: parking_lot::Mutex<HashMap<String, PalaceState>>,
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
            states: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    /// The configured palace ids.
    pub fn palaces(&self) -> &[String] {
        &self.palaces
    }

    /// Whether any search of `palace` is running now.
    pub fn in_flight(&self, palace: &str) -> bool {
        self.states
            .lock()
            .get(palace)
            .is_some_and(|s| !s.running.is_empty())
    }

    /// How many searches of `palace` this leg has started.
    pub fn searches_started(&self, palace: &str) -> u64 {
        self.states.lock().get(palace).map_or(0, |s| s.generation)
    }

    /// Start a search of `palace`, or say why this recall must not.
    ///
    /// What: `Err(InFlight)` while a running search has run for the leg's
    /// timeout or longer; the cached failure, with `cached: true`, within
    /// [`RULINGS_RETRY_AFTER`] of it; otherwise records a new running search
    /// and returns its generation. A search still inside the timeout refuses
    /// nothing, so overlapping healthy recalls each search.
    fn admit(&self, palace: &str) -> Result<u64, (DegradedReason, bool)> {
        let mut states = self.states.lock();
        let state = states.entry(palace.to_string()).or_default();
        // See #9143: only a stalled search refuses a new one, so a stall never
        // piles up blocking tasks and overlapping recalls all get rulings.
        let oldest = state.running.values().next();
        if oldest.is_some_and(|started| started.elapsed() >= self.timeout) {
            return Err((DegradedReason::InFlight, false));
        }
        if let Some((at, reason)) = state.failure {
            if at.elapsed() < RULINGS_RETRY_AFTER {
                return Err((reason, true));
            }
        }
        state.generation += 1;
        state.running.insert(state.generation, Instant::now());
        Ok(state.generation)
    }

    /// Record the outcome of search `generation` of `palace`.
    ///
    /// What: the search stops running whatever its generation. Its outcome is
    /// ignored unless `generation` is the newest search started, so an older
    /// result never overwrites a newer one; otherwise a success clears the
    /// failure and a failure replaces it.
    fn finish(&self, palace: &str, generation: u64, outcome: Result<(), DegradedReason>) {
        let mut states = self.states.lock();
        let Some(state) = states.get_mut(palace) else {
            return;
        };
        state.running.remove(&generation);
        // #9143 review: a stale result never overwrites a newer one.
        if generation != state.generation {
            return;
        }
        state.failure = outcome.err().map(|reason| (Instant::now(), reason));
    }
}

/// Records one search's outcome exactly once, on every exit path.
///
/// Why: the blocking task records its own outcome so a search that outlives
/// the recall still clears or sets the palace's state. A panic, or a task the
/// runtime drops unrun, would otherwise leave the palace `in_flight` forever.
/// What: [`Self::record`] records the outcome; dropping it unrecorded records
/// [`DegradedReason::TaskFailed`].
struct FinishGuard {
    leg: Arc<RulingsLeg>,
    palace: String,
    generation: u64,
    recorded: bool,
}

impl FinishGuard {
    fn record(mut self, outcome: Result<(), DegradedReason>) {
        self.recorded = true;
        self.leg.finish(&self.palace, self.generation, outcome);
    }
}

impl Drop for FinishGuard {
    fn drop(&mut self) {
        if !self.recorded {
            let failed = Err(DegradedReason::TaskFailed);
            self.leg.finish(&self.palace, self.generation, failed);
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

    /// Read the rulings palaces from [`RULINGS_PALACES_ENV`] (#9143).
    ///
    /// Why (#9143 review): `AppState::new` reads no environment, so a test
    /// state is hermetic on a machine that exports the variable. The daemon
    /// opts in at startup, like `with_bm25_lane_from_env`. An odd value (blank
    /// entries, duplicates) is reported here at warn, once, not per recall.
    /// What: [`Self::with_rulings_palaces`] over the parsed list; empty when
    /// the variable is unset or not UTF-8.
    /// Test: `a_default_state_ignores_the_rulings_env_until_the_daemon_opts_in`.
    #[must_use]
    pub fn with_rulings_palaces_from_env(self) -> Self {
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
        self.with_rulings_palaces(palaces)
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
/// each palace, [`RulingsLeg::admit`] either refuses (`in_flight` behind a
/// stalled search, or a cached failure) or starts [`search_one_bounded`].
/// Test: `an_absent_rulings_palace_degrades_and_is_not_retried_at_once`,
/// `a_hung_rulings_palace_runs_one_search_and_its_late_success_counts`.
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
                let generation = match leg.admit(&palace) {
                    Ok(generation) => generation,
                    Err((reason, cached)) => {
                        tracing::debug!(palace = %palace, ?reason, cached, "#9143: rulings palace not searched");
                        return Err(RulingsDegraded {
                            palace,
                            reason,
                            cached,
                        });
                    }
                };
                let outcome = search_one_bounded(state, &palace, search, leg, generation).await;
                outcome.map_err(|reason| RulingsDegraded {
                    palace,
                    reason,
                    cached: false,
                })
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

/// Run search `generation` of one rulings palace on the blocking pool, and
/// wait for it at most the leg's timeout.
///
/// Why: a palace open is disk I/O and the L2 join takes the palace's drawer
/// lock synchronously; either can stall. On the blocking pool a stall holds a
/// blocking thread, never a tokio worker, and the timeout returns the recall.
/// The task records its own outcome through a [`FinishGuard`], so a search
/// that ends after the recall gave up still updates the palace's state.
/// What: the hits, or the code for an absent or unreadable palace, a failed
/// search, a failed task, or the timeout.
/// Test: `a_hung_rulings_palace_runs_one_search_and_its_late_success_counts`.
async fn search_one_bounded(
    state: AppState,
    palace: &str,
    search: Arc<LegSearch>,
    leg: Arc<RulingsLeg>,
    generation: u64,
) -> Result<RulingsHits, DegradedReason> {
    let timeout = leg.timeout;
    // #9421: the edge read inside the task must end before the leg's bound.
    let deadline = Instant::now() + timeout;
    let guard = FinishGuard {
        leg,
        palace: palace.to_string(),
        generation,
        recorded: false,
    };
    // #9143: blocking pool, so a hung open or lock never blocks a worker.
    let task = tokio::task::spawn_blocking(move || {
        let outcome = search_palace(&state, &guard.palace, &search, deadline);
        guard.record(outcome.as_ref().map(|_| ()).map_err(|reason| *reason));
        outcome
    });
    match tokio::time::timeout(timeout, task).await {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(join)) => {
            tracing::warn!(palace = %palace, "#9143: rulings search task failed: {join}");
            Err(DegradedReason::TaskFailed)
        }
        Err(_) => {
            tracing::warn!(
                palace = %palace,
                "#9143: rulings search still running after {} ms; reported as timed out",
                timeout.as_millis()
            );
            Err(DegradedReason::TimedOut)
        }
    }
}

/// Open and search one rulings palace. Runs on the blocking pool.
///
/// Score scale (#9143 review): the project hits carry the vector score plus an
/// RRF bonus from the project palace's BM25 lane, so each rulings palace's hits
/// get the same fusion from that palace's own BM25 lane — one scorer, one
/// scale. With no BM25 lane for that palace a ruling lacks the bonus (at most
/// `1/61`), which errs toward the project hit.
/// What: the hits, plus the `superseded_by` edges among them read from this
/// palace's KG (#9421) within [`LOOKUP_BUDGET`] or the time left before
/// `deadline`, whichever is shorter; a failed or late edge read logs a warning
/// and demotes nothing for this palace. Empty when the palace is an alias of
/// the project palace. Each search failure is logged with its full error chain
/// and returned as a bare code.
/// Test: `a_superseded_ruling_from_a_rulings_palace_ranks_below_its_replacement`.
fn search_palace(
    state: &AppState,
    palace: &str,
    search: &LegSearch,
    deadline: Instant,
) -> Result<RulingsHits, DegradedReason> {
    let handle = match open_palace_handle(state, palace) {
        Ok(h) => h,
        Err(e) if PalaceRegistry::open_error_is_absent(&e) => {
            tracing::warn!(palace = %palace, "#9143: rulings palace absent");
            return Err(DegradedReason::Absent);
        }
        Err(e) => {
            // #9143 review: the chain can name a path; it goes to the log only.
            tracing::warn!(palace = %palace, "#9143: rulings palace unreadable: {e:#}");
            return Err(DegradedReason::Unreadable);
        }
    };
    if handle.id == search.target {
        return Ok(RulingsHits::default()); // an alias of the project palace itself
    }
    tokio::runtime::Handle::current().block_on(async {
        let mut hits = retrieve_l2_scoped(
            &handle,
            search.embedder.as_ref(),
            &search.expanded,
            &RecallScope::All,
            search.window,
        )
        .await
        .map_err(|e| {
            tracing::warn!(palace = %palace, "#9143: rulings search failed: {e:#}");
            DegradedReason::SearchFailed
        })?;
        let lexical = bm25_search_optional(state, handle.id.as_str(), &search.query, search.window);
        if let Some(bm25_hits) = lexical.await {
            fuse_bm25_into_recall(&mut hits, &bm25_hits, search.window);
        }
        // #9421: read while the handle is open; never past the leg's bound.
        let budget = LOOKUP_BUDGET.min(deadline.saturating_duration_since(Instant::now()));
        let superseded = supersessions_for_within(&handle, &hits, budget).await;
        Ok(RulingsHits { hits, superseded })
    })
}

/// What the fold hands the caller's cut (#9143).
#[derive(Debug, Default)]
pub(crate) struct RulingsFold {
    /// Rulings palaces that contributed nothing, and why.
    pub degraded: Vec<RulingsDegraded>,
    /// Drawer ids of folded rulings that answer the query, best first; the cut
    /// gives them reserved slots (`super::recall_rulings_floor`).
    pub floored: Vec<Uuid>,
    /// `superseded -> replacement` edges the rulings palaces reported (#9421).
    pub superseded: Supersessions,
}

/// Merge the rulings leg's outcomes into `results` and report the failures.
///
/// Why: layer 1 marks the hits as grounding rather than a project search hit,
/// matching #9143's "at L1". `min_score` is applied here because
/// `apply_score_floor` exempts layer 1 by design. The cap keeps the leg from
/// crowding the project's own answer (#9143 review).
/// What: every `Err` is returned in `degraded`. From the `Ok` hits it keeps
/// ruling-tagged drawers at or above `min_score` whose drawer id and content
/// hash are new — against `results` and against every earlier ruling, so one
/// ruling stored in two palaces appears once — then the best
/// `ceil(top_k / 3)` by score, appended as layer 1. Of those, the ones that
/// answer `query` ([`ruling_answers_query`]) are listed in `floored`. Every
/// palace's `superseded_by` edges are merged into `superseded` (#9421).
/// `results` is never shortened; the caller re-sorts.
/// Test: `a_search_error_degrades_and_keeps_every_primary_hit`,
/// `the_same_ruling_from_two_palaces_appears_once`,
/// `rulings_contribute_at_most_a_third_of_top_k`,
/// `only_capped_rulings_that_answer_the_query_are_floored`.
pub(crate) fn fold_rulings(
    results: &mut Vec<RecallResult>,
    outcomes: Vec<PalaceOutcome>,
    query: &str,
    top_k: usize,
    min_score: Option<f32>,
) -> RulingsFold {
    let mut degraded = Vec::new();
    let mut ids: HashSet<_> = results.iter().map(|r| r.drawer.id).collect();
    let mut hashes: HashSet<_> = results.iter().map(|r| r.drawer.content_hash()).collect();
    let mut rulings = Vec::new();
    let mut superseded = Supersessions::new();
    for outcome in outcomes {
        match outcome {
            Err(failed) => degraded.push(failed),
            Ok(RulingsHits {
                hits,
                superseded: edges,
            }) => {
                // #9421: drawer ids are UUIDs, so palaces' maps never collide.
                superseded.extend(edges);
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
    let mut floored = Vec::new();
    for mut hit in rulings {
        // #9143 AC2: only a capped ruling that answers the query is floored.
        if ruling_answers_query(query, hit.drawer.content()) {
            floored.push(hit.drawer.id);
        }
        hit.layer = 1;
        results.push(hit);
    }
    RulingsFold {
        degraded,
        floored,
        superseded,
    }
}

#[cfg(test)]
#[path = "recall_rulings_tests.rs"]
mod recall_rulings_tests;
