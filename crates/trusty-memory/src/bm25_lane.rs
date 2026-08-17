//! In-process BM25 lexical lane — one resident index per hot palace.
//!
//! Why (#5329): BM25 used to run as `trusty-bm25-daemon`, a per-palace
//! subprocess reached over a UDS socket and supervised by `Bm25Supervisor`. That
//! architecture was justified by an analogy to `trusty-embedderd`, where the
//! subprocess amortises an ONNX model load. An in-memory inverted index pays no
//! such cost, and `trusty-search` has always run the identical
//! `trusty_common::bm25::BM25Index` in-process alongside its own redb/usearch
//! locks. The gate was never enabled in any shipped configuration (#5186), so
//! the whole spawn/probe/reap state machine was carrying risk for a lane nobody
//! had switched on.
//!
//! What: [`Bm25Lane`] owns an LRU-bounded map of resident
//! [`PalaceBm25Index`](crate::bm25_index::PalaceBm25Index) values keyed by
//! palace id, plus a background task that coalesces snapshot flushes and
//! enforces a corpus-memory budget. Every operation the old UDS client exposed —
//! `index`, `search`, `delete`, `stats`, `missing_docs` — has a method here with
//! the same meaning, so callers changed their transport and nothing else.
//!
//! The bounds that survived the collapse, and what replaced them:
//!
//! | Subprocess-era control | In-process equivalent |
//! |---|---|
//! | live-daemon cap (#2845) | [`ENV_MAX_PALACES`] — resident indexes |
//! | per-child RSS ceiling (#2846) | [`ENV_TEXT_BUDGET_MB`] — retained corpus text |
//! | 50 ms write-coalescing window | [`FLUSH_INTERVAL`] flush tick |
//! | SIGTERM → flush → SIGKILL | [`Bm25Lane::shutdown`] |
//!
//! What did NOT survive, stated plainly: process-level fault isolation. A
//! runaway BM25 index can no longer be SIGKILLed independently of the recall
//! path. The text budget is what bounds that risk now.
//!
//! Test: `bm25_lane_tests.rs`, `tests/bm25_lane_concurrency.rs`,
//! `tests/bm25_lane_e2e.rs`.

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::{Context, Result};
use lru::LruCache;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::bm25_index::PalaceBm25Index;

/// Environment variable that caps how many palace indexes stay resident.
///
/// Why (#2845, carried forward from the daemon cap): the lane is touched once
/// per palace, and one `memory_recall_all` reaches every palace on disk — ~99 on
/// this host. Without a cap the lane would hold every palace's corpus in memory
/// for the rest of the process's life. Drawer distribution is heavily skewed, so
/// the working set is a handful of palaces and a small cap costs almost nothing.
/// What: `TRUSTY_BM25_MAX_PALACES`, parsed as `usize`; `0` and unparseable
/// values fall back to [`DEFAULT_MAX_RESIDENT`].
/// Test: `max_resident_honours_env_override`.
pub const ENV_MAX_PALACES: &str = "TRUSTY_BM25_MAX_PALACES";

/// Environment variable that overrides the retained-corpus budget, in MB.
///
/// Why (#2846): trusty-search declared an `rss_limit_mb` and never compared it
/// against anything; the process grew to 2.2x that limit and was OOM-killed. The
/// daemon-era answer was a per-child RSS ceiling, which has no meaning now that
/// there is no child. `PalaceBm25Index` retains every document's full text, so
/// summed `total_text_bytes` is the figure that actually tracks the lane's
/// memory — and unlike RSS it is attributable to a specific palace, so the
/// eviction it triggers can pick the right victim.
/// What: `TRUSTY_BM25_TEXT_BUDGET_MB`. `0` disables enforcement; unparseable
/// values fall back to [`DEFAULT_TEXT_BUDGET_MB`].
/// Test: `text_budget_honours_env_override`, `over_budget_evicts_the_coldest`.
pub const ENV_TEXT_BUDGET_MB: &str = "TRUSTY_BM25_TEXT_BUDGET_MB";

/// Default cap on resident palace indexes.
///
/// Why: three covers the realistic working set — the palace you are in, the one
/// you just cross-referenced, and one in flight. Same number the daemon cap used
/// for the same reason; an evicted palace costs one snapshot reload, not a lost
/// corpus.
/// Test: `default_cap_is_three`.
pub const DEFAULT_MAX_RESIDENT: usize = 3;

/// Default retained-corpus budget in megabytes.
///
/// Why: the whole drawer corpus across ~99 palaces is single-digit MB of text,
/// so a lane retaining more than 512 MB is not holding drawers — it is leaking,
/// and #2846 is the record of what happens when nobody notices.
/// Test: `text_budget_honours_env_override`.
pub const DEFAULT_TEXT_BUDGET_MB: u64 = 512;

/// How often the background task flushes dirty snapshots and checks the budget.
///
/// Why: a flush rewrites the whole snapshot, so flushing per write would make a
/// 1311-drawer backfill quadratic in bytes. Coalescing on a tick keeps the write
/// path O(1) and bounds what a SIGKILL loses to one interval's writes — the
/// daemon's own coalescing window was 50 ms, and 100 ms buys back the tick's
/// wakeup cost without materially changing that exposure. A graceful exit loses
/// nothing at all; see [`Bm25Lane::shutdown`].
/// Test: `a_write_reaches_disk_without_an_explicit_flush`.
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(100);

/// One BM25 search hit.
///
/// Why: the recall path fuses these with vector hits via RRF and needs both the
/// document id and the score. Kept a typed struct so call sites stay free of
/// `serde_json::Value` plumbing.
/// What: `doc_id` is whatever string the caller indexed under; `score` is the
/// BM25 score. `Serialize`/`Deserialize` are retained because the web recall
/// routes render hits into JSON responses.
/// Test: `bm25_hit_round_trips`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BM25Hit {
    pub doc_id: String,
    pub score: f32,
}

/// Corpus-coverage figures for one palace.
///
/// Why: an empty `search` result is ambiguous — it means either "the query
/// matched nothing" or "this palace holds nothing to match against". Callers
/// that cannot tell those apart serve partial results as if they were complete.
/// What: `doc_count` resolves the ambiguity; `total_text_bytes` is what the
/// lane budgets memory against.
/// Test: `stats_report_docs_and_bytes`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Bm25Stats {
    /// Live documents the palace's index is serving.
    pub doc_count: usize,
    /// Summed byte length of the retained document text.
    pub total_text_bytes: u64,
}

/// Coverage answer for a specific set of document ids.
///
/// Why: [`Bm25Stats`] answers "how many", which is not the question a caller
/// establishing coverage is asking. `missing.is_empty()` is a statement about
/// the SET of documents held, and stays correct however many documents the index
/// holds that the caller never asked about (#5048, #5053).
/// What: `missing` names the requested ids the index does not hold; `checked`
/// echoes how many were examined.
/// Test: `missing_docs_answers_by_identity`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Bm25Coverage {
    /// Requested doc ids the index does not hold.
    pub missing: Vec<String>,
    /// How many ids were examined.
    pub checked: usize,
}

/// The in-process BM25 lane.
///
/// Why: see the module doc. This is the single owner of every resident palace
/// index, which is what makes "the mpsc channel is the lock" unnecessary — one
/// `tokio::sync::Mutex` around the LRU serialises every mutation, and each
/// operation under it is pure in-memory work measured in microseconds.
/// What: methods mirror the retired UDS client one-for-one, each taking the
/// palace id the old client baked into its socket path. Every method is `&self`
/// so the lane lives behind an `Arc` in `AppState`.
/// Test: `bm25_lane_tests.rs` and `tests/bm25_lane_concurrency.rs`.
pub struct Bm25Lane {
    /// Daemon data root; a palace's index lives at `<data_root>/<palace>/bm25/`.
    data_root: PathBuf,
    max_resident: usize,
    /// `None` disables budget enforcement.
    text_budget_bytes: Option<u64>,
    resident: Mutex<LruCache<String, PalaceBm25Index>>,
    loaded: AtomicU64,
    evicted: AtomicU64,
    /// The flush ticker, so [`Self::shutdown`] can stop it after the final
    /// flush rather than leaving it to race the process exit.
    flusher: parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl std::fmt::Debug for Bm25Lane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bm25Lane")
            .field("data_root", &self.data_root)
            .field("max_resident", &self.max_resident)
            .field("text_budget_bytes", &self.text_budget_bytes)
            .finish_non_exhaustive()
    }
}

impl Bm25Lane {
    /// Construct a lane with limits read from the environment and start its
    /// flush ticker.
    ///
    /// Why: resolving the limits once here — rather than on each call — means a
    /// mid-flight env mutation cannot make the cap wobble between two concurrent
    /// operations.
    /// What: returns an `Arc` because the ticker holds a `Weak` back-reference,
    /// so the task exits on its own if the lane is dropped without
    /// [`Self::shutdown`].
    /// Test: `default_cap_is_three`, `max_resident_honours_env_override`.
    pub fn new(data_root: PathBuf) -> Arc<Self> {
        Self::with_limits(data_root, max_resident_from_env(), text_budget_from_env())
    }

    /// Construct a lane with explicit limits, bypassing the environment.
    ///
    /// Why: the limit tests must pin a cap of 1 or a budget of a few bytes
    /// without mutating process-global env vars that sibling tests race against.
    /// What: `max_resident` is clamped to at least 1 — a cap of zero would evict
    /// every index the instant it loaded. `text_budget_mb: None` disables budget
    /// enforcement.
    /// Test: `cap_is_clamped_to_at_least_one`, `over_budget_evicts_the_coldest`.
    pub fn with_limits(
        data_root: PathBuf,
        max_resident: usize,
        text_budget_mb: Option<u64>,
    ) -> Arc<Self> {
        let max_resident = max_resident.max(1);
        let capacity = NonZeroUsize::new(max_resident).unwrap_or(NonZeroUsize::MIN);
        let lane = Arc::new(Self {
            data_root,
            max_resident,
            text_budget_bytes: text_budget_mb.map(|mb| mb.saturating_mul(1024 * 1024)),
            resident: Mutex::new(LruCache::new(capacity)),
            loaded: AtomicU64::new(0),
            evicted: AtomicU64::new(0),
            flusher: parking_lot::Mutex::new(None),
        });
        let handle = tokio::spawn(flush_loop(Arc::downgrade(&lane)));
        *lane.flusher.lock() = Some(handle);
        lane
    }

    /// Cap on resident palace indexes.
    pub fn max_resident(&self) -> usize {
        self.max_resident
    }

    /// Retained-corpus budget in bytes; `None` means enforcement is off.
    pub fn text_budget_bytes(&self) -> Option<u64> {
        self.text_budget_bytes
    }

    /// How many palace indexes have been loaded from disk.
    ///
    /// Why: distinguishes "the cap is configured" from "the cap did something"
    /// when read alongside [`Self::evicted_count`] — #2846 is the record of a
    /// limit that only ever made the first claim.
    /// Test: `a_concurrent_fanout_never_exceeds_the_cap`.
    pub fn loaded_count(&self) -> u64 {
        self.loaded.load(Ordering::Relaxed)
    }

    /// How many palace indexes have been evicted by the cap or the budget.
    /// [`Self::shutdown`] does not increment it.
    pub fn evicted_count(&self) -> u64 {
        self.evicted.load(Ordering::Relaxed)
    }

    /// How many palace indexes are resident right now.
    pub async fn resident_count(&self) -> usize {
        self.resident.lock().await.len()
    }

    /// On-disk directory holding a palace's BM25 snapshot.
    ///
    /// Why: `<data_root>/<palace>/bm25/` is the path `trusty-bm25-daemon` was
    /// handed via `--data-dir`, and #5329 keeps it verbatim so an existing
    /// snapshot is found where the operator left it.
    /// Test: `data_dir_matches_the_daemon_era_layout`.
    pub fn data_dir_for_palace(&self, palace: &str) -> PathBuf {
        self.data_root.join(palace).join("bm25")
    }

    /// Index (or replace) a document in a palace's corpus.
    ///
    /// Why: `memory_remember` / `memory_note` call this after persisting a
    /// drawer to redb so the lexical lane can answer subsequent recalls.
    /// What: keyed by `doc_id`, so a re-index overwrites rather than duplicates.
    /// The write lands in memory and is marked dirty; the flush tick persists it.
    /// Errors only when the palace's snapshot cannot be loaded at all.
    /// Test: `index_then_search_finds_the_document`.
    pub async fn index(&self, palace: &str, doc_id: &str, text: &str) -> Result<()> {
        self.with_index(palace, |idx| idx.index_doc(doc_id, text))
            .await
    }

    /// Search a palace's corpus.
    ///
    /// What: `top_k` is forwarded verbatim; hits come back in descending score
    /// order.
    /// Test: `index_then_search_finds_the_document`.
    pub async fn search(&self, palace: &str, query: &str, top_k: usize) -> Result<Vec<BM25Hit>> {
        self.with_index(palace, |idx| idx.search(query, top_k))
            .await
    }

    /// Remove a document from a palace's corpus.
    ///
    /// What: idempotent — succeeds whether or not the document was present.
    /// Test: `delete_removes_the_document`.
    pub async fn delete(&self, palace: &str, doc_id: &str) -> Result<()> {
        self.with_index(palace, |idx| {
            idx.delete_doc(doc_id);
        })
        .await
    }

    /// Corpus figures for a palace.
    ///
    /// Test: `stats_report_docs_and_bytes`.
    pub async fn stats(&self, palace: &str) -> Result<Bm25Stats> {
        self.with_index(palace, |idx| Bm25Stats {
            doc_count: idx.doc_count(),
            total_text_bytes: idx.total_text_bytes(),
        })
        .await
    }

    /// Which of `doc_ids` a palace's corpus does not hold.
    ///
    /// Why: this is the only call that establishes coverage — see
    /// [`Bm25Coverage`]. It returns `Err` rather than an empty `missing` list
    /// when the index cannot be loaded, because a coverage question that could
    /// not be asked must never read as a coverage answer.
    /// Test: `missing_docs_answers_by_identity`.
    pub async fn missing_docs(&self, palace: &str, doc_ids: &[String]) -> Result<Bm25Coverage> {
        self.with_index(palace, |idx| Bm25Coverage {
            missing: idx.missing_docs(doc_ids),
            checked: doc_ids.len(),
        })
        .await
    }

    /// Flush one palace's snapshot now, if it is resident and dirty.
    ///
    /// Why: the backfill calls this when it finishes so a hard kill immediately
    /// afterwards cannot lose a whole sweep's work waiting for the next tick.
    /// What: a no-op for a palace that is not resident — a non-resident index was
    /// flushed when it was evicted.
    /// Test: `flush_persists_a_pending_write`.
    pub async fn flush(&self, palace: &str) -> Result<()> {
        let mut resident = self.resident.lock().await;
        match resident.get_mut(palace) {
            Some(idx) => idx.flush(),
            None => Ok(()),
        }
    }

    /// Flush every resident palace, logging rather than propagating failures.
    ///
    /// Why: this runs on the flush tick and on the exit path, where one palace's
    /// unwritable snapshot must not stop the others from being persisted.
    /// Test: `a_write_reaches_disk_without_an_explicit_flush`.
    pub async fn flush_all(&self) {
        let mut resident = self.resident.lock().await;
        for (palace, idx) in resident.iter_mut() {
            if let Err(e) = idx.flush() {
                tracing::warn!(palace = %palace, "bm25 snapshot flush failed: {e:#}");
            }
        }
    }

    /// Flush everything and stop the background ticker.
    ///
    /// Why: trusty-memory's normal exit is a SIGTERM from launchd or a ctrl-c.
    /// This is the in-process replacement for the supervisor's SIGTERM→reap
    /// sequence, and it is strictly stronger: there is no signal to deliver, no
    /// child to wait for, and no window in which a SIGKILL can land mid-flush.
    /// What: flushes every resident index, then aborts the ticker. Idempotent —
    /// a second call flushes nothing and finds no ticker.
    /// Test: `shutdown_flushes_and_is_idempotent`.
    pub async fn shutdown(&self) {
        self.flush_all().await;
        let handle = self.flusher.lock().take();
        if let Some(h) = handle {
            h.abort();
        }
    }

    /// Run `f` against a palace's index, loading it if it is not resident.
    ///
    /// Why: every public operation funnels through here so that residency,
    /// eviction and load-failure handling exist in exactly one place.
    ///
    /// 🔴 The LRU lock is held across the cold load, and that is the correctness
    /// requirement, not a simplification. An earlier version loaded outside the
    /// lock and re-checked after re-acquiring it, which is unsound under a
    /// fanout wider than the cap: task B starts a load, task A writes a document
    /// and is then EVICTED (flushing its snapshot), and B's re-check now misses,
    /// so B inserts the index it read before A's write existed. B's own flush
    /// then publishes a snapshot with A's document gone — a silently lost write,
    /// with no error anywhere. `a_concurrent_fanout_never_exceeds_the_cap`
    /// caught exactly that: a 12-palace fanout under a cap of 3 lost `doc-0`.
    /// Serialising the load closes it, because no eviction can interleave.
    ///
    /// What: the load itself still runs on the blocking pool — it parses a JSON
    /// snapshot and replays it through the tokenizer — so a cold palace never
    /// blocks a runtime thread, only other lane operations, and only for as long
    /// as one snapshot read. `f` is always pure in-memory work, and the index is
    /// never handed out, so no caller can mutate one the LRU has evicted.
    ///
    /// Room for the loaded index is made by [`evict_coldest_flushed`], never by
    /// `LruCache::push`: `push` hands back a victim it has ALREADY removed, so a
    /// failed flush there had nothing left to keep and dropped the victim's
    /// unflushed documents (#5887). When no resident palace can be persisted the
    /// load FAILS instead — the caller loses this operation, which is recoverable,
    /// rather than another palace losing a write, which is not.
    /// Test: `a_concurrent_fanout_never_exceeds_the_cap`,
    /// `concurrent_callers_for_one_palace_share_one_index`,
    /// `a_cold_load_failure_propagates`,
    /// `a_cold_load_refuses_to_evict_an_unflushable_victim`.
    async fn with_index<R>(
        &self,
        palace: &str,
        f: impl FnOnce(&mut PalaceBm25Index) -> R,
    ) -> Result<R> {
        let mut resident = self.resident.lock().await;
        if let Some(idx) = resident.get_mut(palace) {
            return Ok(f(idx));
        }

        let dir = self.data_dir_for_palace(palace);
        let loaded = tokio::task::spawn_blocking(move || PalaceBm25Index::load_or_create(&dir))
            .await
            .context("bm25 snapshot load task failed")?
            .with_context(|| format!("load bm25 snapshot for palace {palace}"))?;

        self.loaded.fetch_add(1, Ordering::Relaxed);
        if resident.len() >= self.max_resident {
            let (victim, _) = evict_coldest_flushed(&mut resident).with_context(|| {
                format!(
                    "no resident bm25 palace could be flushed, so palace {palace} \
                     cannot be made room for without losing another palace's writes"
                )
            })?;
            self.evicted.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(palace = %victim, "bm25 lane evicted a palace to make room");
        }
        resident.put(palace.to_string(), loaded);
        let idx = resident
            .get_mut(palace)
            .context("bm25 index vanished from the LRU immediately after insertion")?;
        Ok(f(idx))
    }

    /// Evict the coldest palaces until the retained corpus fits the budget.
    ///
    /// Why (#2846): this is the check the daemon-era RSS ceiling never had an
    /// in-process equivalent for. It runs on the flush tick rather than on the
    /// write path because summing the retained text is O(documents), which does
    /// not belong in a per-drawer write.
    /// What: no-op when enforcement is off or the total fits. Otherwise evicts
    /// via [`evict_coldest_flushed`], which removes an index only once its
    /// snapshot is on disk, and stops at one resident index, because evicting the
    /// last one would just make the next operation reload it.
    ///
    /// When no resident snapshot can be written the lane STAYS OVER BUDGET rather
    /// than dropping an unflushed write (#5887). That is the deliberate trade
    /// under memory pressure: exceeding a memory budget is visible, bounded by
    /// the retained text of the palaces that cannot be persisted, and retried on
    /// every tick, so it clears itself the moment the snapshot becomes writable.
    /// A dropped write is silent and permanent. Palaces that CAN be flushed are
    /// still evicted, so one unwritable palace does not disable enforcement.
    /// Test: `over_budget_evicts_the_coldest`,
    /// `the_budget_keeps_a_palace_whose_snapshot_cannot_be_flushed`.
    async fn enforce_text_budget(&self) {
        let Some(budget) = self.text_budget_bytes else {
            return;
        };
        let mut resident = self.resident.lock().await;
        let mut total: u64 = resident.iter().map(|(_, idx)| idx.total_text_bytes()).sum();
        while total > budget && resident.len() > 1 {
            let Some((victim, freed)) = evict_coldest_flushed(&mut resident) else {
                tracing::warn!(
                    budget_bytes = budget,
                    total_bytes = total,
                    "bm25 lane is over its retained-text budget and no resident snapshot \
                     could be flushed — staying over budget rather than dropping an \
                     unflushed write; retrying on the next tick"
                );
                return;
            };
            total = total.saturating_sub(freed);
            self.evicted.fetch_add(1, Ordering::Relaxed);
            tracing::info!(
                palace = %victim,
                budget_bytes = budget,
                "bm25 lane over its retained-text budget — evicted coldest palace"
            );
        }
    }
}

/// Evict the coldest palace whose snapshot flush succeeds.
///
/// Why (#5887): both eviction sites used to remove an index from the LRU FIRST
/// and only then flush it, logging any failure. The flushed value was owned and
/// dropped at the end of the arm, so an unwritable snapshot took the index's
/// unflushed documents with it — precisely the loss the flush exists to prevent,
/// and invisible beyond one `warn!` line. `PalaceBm25Index::flush` keeps the
/// dirty bit set on failure so the next tick retries, but a dropped index has no
/// next tick.
/// What: walks residents coldest-first and flushes each in place with `peek_mut`,
/// which does not disturb LRU order, until one succeeds; that one is removed and
/// returned with the retained-text bytes it frees. A palace whose flush fails
/// stays resident and the next-coldest is tried. `None` means nothing could be
/// persisted, and the caller must not drop an index anyway.
/// Test: `the_budget_keeps_a_palace_whose_snapshot_cannot_be_flushed`,
/// `a_cold_load_refuses_to_evict_an_unflushable_victim`.
fn evict_coldest_flushed(
    resident: &mut LruCache<String, PalaceBm25Index>,
) -> Option<(String, u64)> {
    // `iter` is most-recently-used first, so `rev` is coldest-first. Collected
    // up front because the flush below needs `&mut` access to the cache.
    let coldest_first: Vec<String> = resident.iter().rev().map(|(k, _)| k.clone()).collect();
    for key in coldest_first {
        let Some(idx) = resident.peek_mut(&key) else {
            continue;
        };
        if let Err(e) = idx.flush() {
            tracing::warn!(
                palace = %key,
                "bm25 snapshot flush failed — keeping the index resident rather than \
                 dropping its unflushed writes: {e:#}"
            );
            continue;
        }
        let freed = idx.total_text_bytes();
        // The `None` arm is unreachable: `peek_mut` just resolved this key, and
        // the caller holds the lane lock across both calls.
        resident.pop(&key)?;
        return Some((key, freed));
    }
    None
}

/// Background loop: coalesce snapshot flushes and enforce the memory budget.
///
/// Why: holding a `Weak` rather than an `Arc` means this task cannot keep the
/// lane alive. A lane dropped without [`Bm25Lane::shutdown`] — which is what
/// every short-lived test `AppState` does — lets the upgrade fail and the loop
/// exit on its next tick instead of leaking a task for the process's lifetime.
/// Test: `a_write_reaches_disk_without_an_explicit_flush`.
async fn flush_loop(lane: Weak<Bm25Lane>) {
    let mut ticker = tokio::time::interval(FLUSH_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let Some(lane) = lane.upgrade() else {
            return;
        };
        lane.flush_all().await;
        lane.enforce_text_budget().await;
    }
}

/// Resolve the resident cap from [`ENV_MAX_PALACES`].
///
/// Why: an operator who fans out wider than the default working set needs a
/// knob, and a knob that silently ignores a typo is worse than no knob.
/// Test: `max_resident_honours_env_override`.
fn max_resident_from_env() -> usize {
    match std::env::var(ENV_MAX_PALACES) {
        Ok(raw) => match raw.trim().parse::<usize>() {
            Ok(n) if n >= 1 => n,
            _ => {
                tracing::warn!(
                    "{ENV_MAX_PALACES}={raw:?} is not a positive integer — \
                     using default {DEFAULT_MAX_RESIDENT}"
                );
                DEFAULT_MAX_RESIDENT
            }
        },
        Err(_) => DEFAULT_MAX_RESIDENT,
    }
}

/// Resolve the retained-corpus budget from [`ENV_TEXT_BUDGET_MB`].
///
/// Why: same knob-with-a-typo argument as [`max_resident_from_env`], plus one
/// specific to this limit — `0` must be an explicit, documented way to turn
/// enforcement off, not an accident of parsing.
/// Test: `text_budget_honours_env_override`.
fn text_budget_from_env() -> Option<u64> {
    match std::env::var(ENV_TEXT_BUDGET_MB) {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(0) => None,
            Ok(n) => Some(n),
            Err(_) => {
                tracing::warn!(
                    "{ENV_TEXT_BUDGET_MB}={raw:?} is not an integer — \
                     using default {DEFAULT_TEXT_BUDGET_MB}"
                );
                Some(DEFAULT_TEXT_BUDGET_MB)
            }
        },
        Err(_) => Some(DEFAULT_TEXT_BUDGET_MB),
    }
}

/// Per-palace BM25 data directory derived from a daemon data root.
///
/// Why: `tools::bm25` builds this path before it has a lane in hand (the enqueue
/// path runs whether or not the lane is on), so the arithmetic lives here rather
/// than only as a method.
/// What: `<data_root>/<palace>/bm25` — the daemon-era layout, kept verbatim.
/// Test: `data_dir_matches_the_daemon_era_layout`.
pub fn data_dir_for_palace(data_root: &Path, palace: &str) -> PathBuf {
    data_root.join(palace).join("bm25")
}

#[cfg(test)]
#[path = "bm25_lane_tests.rs"]
mod tests;
