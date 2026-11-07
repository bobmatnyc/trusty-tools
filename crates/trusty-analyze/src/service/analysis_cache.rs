//! Per-index cache of corpus-wide analysis results (#10781).
//!
//! Why: `analyze.complexity_hotspots`, `analyze.smells` and `analyze.quality`
//! paged the whole index on every call — about two minutes on a 315K-chunk
//! index, against a 60 s client budget. An unscoped call answers from a
//! result computed earlier whenever one exists.
//!
//! What: one slot per index id. A slot holds the last good [`CorpusAnalysis`],
//! the [`IndexFingerprint`] it was computed against, and at most one
//! background refresh. The cache key is the index id; an entry is current
//! only while trusty-search's fingerprint for that index equals the stored
//! one and the entry is younger than [`CacheConfig::max_age`]. Anything else
//! is served marked stale and starts a refresh. A cold slot waits for its
//! refresh to finish, as the pre-#10781 in-request walk did; only a caller
//! that opts in (`allow_warming`) waits at most [`CacheConfig::warm_wait`]
//! and then answers a retryable "warming" error. Concurrent callers share
//! one refresh. A slot exists only for an index whose status was read; one
//! trusty-search reports as not found is removed.
//!
//! Test: `analysis_cache_tests.rs`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::watch;

use crate::core::quality::{self, HotspotItem, QualityReport};
use crate::core::{IndexFingerprint, TrustySearchClient};
use crate::service::events::ApiError;
use crate::types::complexity::CodeSmell;
use crate::types::CodeChunk;
use trusty_common::search_rpc::SearchRpcError;

#[cfg(test)]
#[path = "analysis_cache_tests.rs"]
pub(crate) mod tests;

/// How many hotspots a cached entry keeps, worst first.
///
/// Why: a hotspot carries its chunk's source, so keeping every chunk of a
/// 315K-chunk index would hold the whole corpus in memory. A request for at
/// most this many reads the same rows the uncached path returned, because
/// the ranking is a stable sort followed by a truncation.
pub const HOTSPOT_CACHE_CAP: usize = 1000;

/// The corpus-wide results the three cached methods serve.
///
/// What: the quality aggregate, the top [`HOTSPOT_CACHE_CAP`] hotspots, and
/// every smelly chunk with its smells, all computed by the same `quality`
/// functions the uncached handlers called.
/// Test: `unscoped_results_match_the_uncached_computation`.
#[derive(Debug)]
pub struct CorpusAnalysis {
    pub quality: QualityReport,
    pub hotspots: Vec<HotspotItem>,
    pub smells: Vec<(CodeChunk, Vec<CodeSmell>)>,
}

impl CorpusAnalysis {
    /// Compute every cached result from one corpus walk.
    pub fn compute(chunks: &[CodeChunk]) -> Self {
        Self {
            quality: quality::aggregate_quality(chunks),
            hotspots: quality::complexity_hotspots(chunks, HOTSPOT_CACHE_CAP),
            smells: quality::smelly_chunks_with_smells(chunks),
        }
    }
}

/// One computed result and what it was computed against.
#[derive(Debug)]
pub struct CachedAnalysis {
    pub fingerprint: IndexFingerprint,
    pub computed_at: chrono::DateTime<chrono::Utc>,
    computed_instant: Instant,
    pub analysis: CorpusAnalysis,
}

/// A served result plus the freshness verdict a caller reads.
pub struct CacheHit {
    pub entry: Arc<CachedAnalysis>,
    /// `None` when the entry is current; otherwise why it is not.
    pub stale_reason: Option<String>,
    /// The last background refresh failure, when one is outstanding.
    pub refresh_error: Option<String>,
}

impl CacheHit {
    /// The additive `cache` object every unscoped response carries.
    ///
    /// What: `status` is `"fresh"` or `"stale"`; `stale_reason` and
    /// `refresh_error` appear only when set.
    pub fn meta(&self) -> Value {
        let mut meta = json!({
            "status": if self.stale_reason.is_some() { "stale" } else { "fresh" },
            "computed_at": self.entry.computed_at.to_rfc3339(),
            "fingerprint": self.entry.fingerprint,
        });
        if let Some(reason) = &self.stale_reason {
            meta["stale_reason"] = json!(reason);
        }
        if let Some(err) = &self.refresh_error {
            meta["refresh_error"] = json!(err);
        }
        meta
    }
}

/// Cache timing bounds.
#[derive(Clone, Debug)]
pub struct CacheConfig {
    /// An entry older than this is stale even when the fingerprint matches.
    /// It bounds how long an edit the fingerprint cannot see stays "fresh".
    pub max_age: Duration,
    /// How long an `allow_warming` cold call waits before answering "warming".
    pub warm_wait: Duration,
    /// After a failed refresh, no new refresh starts for this long.
    pub retry_cooldown: Duration,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_age: Duration::from_secs(15 * 60),
            warm_wait: Duration::from_secs(5),
            retry_cooldown: Duration::from_secs(30),
        }
    }
}

#[derive(Default)]
struct Slot {
    entry: Option<Arc<CachedAnalysis>>,
    in_flight: Option<watch::Receiver<bool>>,
    last_error: Option<(Instant, String)>,
}

type Slots = Arc<Mutex<HashMap<String, Slot>>>;

/// The per-index cache. Cheap to clone; clones share the slots.
#[derive(Clone, Default)]
pub struct AnalysisCache {
    slots: Slots,
    config: CacheConfig,
}

fn lock(slots: &Slots) -> MutexGuard<'_, HashMap<String, Slot>> {
    // A panic while holding the lock leaves plain data behind; keep serving.
    slots.lock().unwrap_or_else(|p| p.into_inner())
}

impl AnalysisCache {
    /// A cache with explicit bounds.
    pub fn with_config(config: CacheConfig) -> Self {
        Self {
            slots: Slots::default(),
            config,
        }
    }

    /// Test shorthand: [`Self::get_with`] without `allow_warming`.
    #[cfg(test)]
    pub(crate) async fn get(
        &self,
        search: &TrustySearchClient,
        index_id: &str,
    ) -> Result<CacheHit, ApiError> {
        self.get_with(search, index_id, false).await
    }

    /// Serve `index_id`'s corpus-wide analysis.
    ///
    /// Why: see the module doc. Every branch that cannot prove an entry
    /// current marks it stale; none returns an unverified entry as fresh.
    /// What: reads the fingerprint, then — current entry: fresh hit. Stale or
    /// unverifiable entry: stale hit, plus a refresh when the fingerprint was
    /// readable. No entry: start or join the refresh and wait for it — up to
    /// `warm_wait` when `allow_warming`, else until it ends — then answer
    /// the result, the refresh's failure (`bad_gateway`), or `warming`. A
    /// failed fingerprint read creates no slot: with no entry it is
    /// `bad_gateway` and starts no refresh, because its result could not be
    /// keyed; a not-found answer also removes the slot (`not_found`).
    /// Test: `second_call_is_served_without_a_page_fetch`,
    /// `a_changed_fingerprint_is_never_served_as_fresh`,
    /// `status_failure_marks_the_entry_stale`,
    /// `a_cold_walk_past_warm_wait_answers_warming`,
    /// `a_failed_refresh_is_an_error_not_an_empty_result`,
    /// `unknown_index_ids_leave_no_slot`,
    /// `a_vanished_index_loses_its_slot`.
    pub(crate) async fn get_with(
        &self,
        search: &TrustySearchClient,
        index_id: &str,
        allow_warming: bool,
    ) -> Result<CacheHit, ApiError> {
        let fingerprint = search.index_fingerprint(index_id).await;
        let waiter = {
            let mut slots = lock(&self.slots);
            // #10781: a failed status read never creates a slot.
            let slot = match &fingerprint {
                Ok(_) => slots.entry(index_id.to_string()).or_default(),
                Err(e) if is_not_found(e) => {
                    slots.remove(index_id);
                    return Err(ApiError::not_found(format!(
                        "index '{index_id}' not found in trusty-search: {e:#}"
                    )));
                }
                Err(e) => match slots.get_mut(index_id) {
                    Some(slot) => slot,
                    None => return Err(unkeyable(index_id, e)),
                },
            };
            if let Some(entry) = slot.entry.clone() {
                let stale_reason = self.stale_reason(&entry, &fingerprint);
                if stale_reason.is_some() {
                    if let Ok(fp) = &fingerprint {
                        self.ensure_refresh(slot, search, index_id, fp.clone());
                    }
                }
                return Ok(CacheHit {
                    entry,
                    stale_reason,
                    refresh_error: slot.last_error.as_ref().map(|(_, e)| e.clone()),
                });
            }
            match &fingerprint {
                Ok(fp) => self.ensure_refresh(slot, search, index_id, fp.clone()),
                Err(e) => return Err(unkeyable(index_id, e)),
            }
        };
        if let Some(mut rx) = waiter {
            if allow_warming {
                let _ =
                    tokio::time::timeout(self.config.warm_wait, rx.wait_for(|done| *done)).await;
            } else {
                // #10781: the pre-#10781 shape walked to completion in-request.
                let _ = rx.wait_for(|done| *done).await;
            }
        }
        let slots = lock(&self.slots);
        let slot = slots.get(index_id);
        if let Some(entry) = slot.and_then(|s| s.entry.clone()) {
            return Ok(CacheHit {
                stale_reason: self.stale_reason(&entry, &fingerprint),
                entry,
                refresh_error: None,
            });
        }
        if let Some((_, err)) = slot.and_then(|s| s.last_error.as_ref()) {
            return Err(ApiError::bad_gateway(format!(
                "index '{index_id}': corpus analysis refresh failed: {err}"
            )));
        }
        if !allow_warming {
            return Err(ApiError::internal(format!(
                "index '{index_id}': corpus analysis refresh ended without a result"
            )));
        }
        Err(ApiError::warming(format!(
            "index '{index_id}': corpus-wide analysis is warming in the background; retry \
             shortly, or pass `paths`/`path_prefix` to analyze a scope now"
        )))
    }

    /// Why `entry` is not current, or `None` when it is.
    fn stale_reason(
        &self,
        entry: &CachedAnalysis,
        fingerprint: &anyhow::Result<IndexFingerprint>,
    ) -> Option<String> {
        match fingerprint {
            Err(e) => Some(format!("index status unavailable: {e:#}")),
            Ok(fp) if !fp.is_verifiable() => {
                Some("index status carries no last_indexed or chunk_count".to_string())
            }
            Ok(fp) if *fp != entry.fingerprint => {
                Some("index changed since the result was computed".to_string())
            }
            Ok(_) if entry.computed_instant.elapsed() > self.config.max_age => Some(format!(
                "result is older than the {}s maximum age",
                self.config.max_age.as_secs()
            )),
            Ok(_) => None,
        }
    }

    /// Start a refresh unless one is running or the cooldown holds.
    ///
    /// What: returns a receiver that turns `true` when the refresh ends, or
    /// `None` when no refresh is running.
    fn ensure_refresh(
        &self,
        slot: &mut Slot,
        search: &TrustySearchClient,
        index_id: &str,
        fingerprint: IndexFingerprint,
    ) -> Option<watch::Receiver<bool>> {
        if let Some(rx) = &slot.in_flight {
            // A dropped sender means the task died; start another.
            if rx.has_changed().is_ok() {
                return Some(rx.clone());
            }
        }
        if let Some((at, _)) = &slot.last_error {
            if at.elapsed() < self.config.retry_cooldown {
                return None;
            }
        }
        let (tx, rx) = watch::channel(false);
        slot.in_flight = Some(rx.clone());
        tokio::spawn(refresh(
            Arc::clone(&self.slots),
            search.clone(),
            index_id.to_string(),
            fingerprint,
            tx,
        ));
        Some(rx)
    }
}

/// True when trusty-search answered that the index does not exist.
fn is_not_found(e: &anyhow::Error) -> bool {
    e.downcast_ref::<SearchRpcError>()
        .is_some_and(SearchRpcError::is_not_found)
}

/// The `bad_gateway` for a status read that cannot key the cache.
fn unkeyable(index_id: &str, e: &anyhow::Error) -> ApiError {
    ApiError::bad_gateway(format!(
        "index '{index_id}': cannot read search.index.status to key the analysis cache: {e:#}"
    ))
}

/// Walk the corpus, compute, and store the result or the failure.
///
/// What: `fingerprint` is the one read BEFORE the walk, so an index that
/// changes mid-walk reads as changed on the next call and refreshes again.
/// A failed walk keeps the previous entry and records the error. A slot
/// removed meanwhile (the index vanished) is not re-created.
async fn refresh(
    slots: Slots,
    search: TrustySearchClient,
    index_id: String,
    fingerprint: IndexFingerprint,
    done: watch::Sender<bool>,
) {
    let outcome = match search.get_chunks(&index_id).await {
        Ok(chunks) => tokio::task::spawn_blocking(move || CorpusAnalysis::compute(&chunks))
            .await
            .map_err(|e| format!("analysis task failed: {e}")),
        Err(e) => Err(format!("{e:#}")),
    };
    {
        let mut slots = lock(&slots);
        // #10781: never re-create a slot `get_with` removed.
        let Some(slot) = slots.get_mut(&index_id) else {
            drop(slots);
            let _ = done.send(true);
            return;
        };
        slot.in_flight = None;
        match outcome {
            Ok(analysis) => {
                slot.entry = Some(Arc::new(CachedAnalysis {
                    fingerprint,
                    computed_at: chrono::Utc::now(),
                    computed_instant: Instant::now(),
                    analysis,
                }));
                slot.last_error = None;
            }
            Err(msg) => {
                tracing::warn!("analysis cache refresh for index '{index_id}' failed: {msg}");
                slot.last_error = Some((Instant::now(), msg));
            }
        }
    }
    let _ = done.send(true);
}
