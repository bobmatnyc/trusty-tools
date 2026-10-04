//! The `last_queried_unix` stamp a per-index search writes (#993, #8275).
//!
//! Why: warm-boot ranks indexes on this stamp and, since #8275, refuses to
//! eagerly load any index whose stamp is older than
//! `TRUSTY_WARMBOOT_MAX_AGE_HOURS`. A sweep that sends one query text to many
//! indexes is not use of any of them, yet it stamped every index it touched:
//! on 2026-10-01 one test query stamped 10 stale indexes within a second and
//! put them all in the next boot set.
//! What: [`stamp_last_queried`] — the rate-limited write `search_report` used
//! to do inline — now holds each stamp for [`QUERY_BURST_WINDOW`] and drops it
//! when the same query text reached a second index inside that window. The
//! global `POST /search` fan-out never calls in here, so it stamps nothing.
//! Test: `a_fan_out_burst_leaves_recency_unchanged_and_a_targeted_query_advances_it`
//! in `recency_stamp_tests.rs`.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::core::registry::IndexId;
use crate::service::lazy_loader::LAST_QUERIED_WRITE_INTERVAL_SECS;
use crate::service::SearchAppState;

/// How long a stamp waits to learn whether its query was part of a sweep.
///
/// Why: the 2026-10-01 sweep reached 10 indexes inside one second; a client
/// looping over indexes one at a time is slower, so the window is wider.
/// What: also the delay before a targeted query's stamp reaches
/// `indexes.toml`, which only the next boot reads.
pub const QUERY_BURST_WINDOW: Duration = Duration::from_secs(5);

/// Groups per-index searches by query text within a short window (#8275).
///
/// Why: a sweep reaches the daemon as N independent per-index searches, so no
/// single request can tell it is one. Only the group can.
/// What: one slot per query text, keyed by the trimmed text and holding the
/// index ids that text reached since the slot opened. A slot older than the
/// window is replaced on the next observation of its text and pruned on any
/// observation, so the map only holds the last window's queries.
/// Test: `a_fan_out_burst_leaves_recency_unchanged_and_a_targeted_query_advances_it`.
pub struct QueryBurstGate {
    window: Duration,
    slots: DashMap<String, BurstSlot>,
}

struct BurstSlot {
    opened: Instant,
    ids: Arc<Mutex<HashSet<String>>>,
}

/// One search's view of the burst it joined.
pub struct BurstTicket {
    ids: Arc<Mutex<HashSet<String>>>,
}

impl BurstTicket {
    /// `true` once the query text has reached more than one index.
    pub fn is_fan_out(&self) -> bool {
        self.ids
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
            > 1
    }
}

impl QueryBurstGate {
    /// A gate whose bursts last `window`.
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            slots: DashMap::new(),
        }
    }

    /// How long a stamp waits before it is written or dropped.
    pub fn window(&self) -> Duration {
        self.window
    }

    /// Record that `text` reached `index_id`; return this search's ticket.
    pub fn observe(&self, text: &str, index_id: &str) -> BurstTicket {
        let now = Instant::now();
        let window = self.window;
        self.slots
            .retain(|_, slot| now.duration_since(slot.opened) <= window);
        let slot = self
            .slots
            .entry(text.trim().to_string())
            .or_insert_with(|| BurstSlot {
                opened: now,
                ids: Arc::default(),
            });
        slot.ids
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(index_id.to_string());
        BurstTicket {
            ids: Arc::clone(&slot.ids),
        }
    }
}

impl Default for QueryBurstGate {
    fn default() -> Self {
        Self::new(QUERY_BURST_WINDOW)
    }
}

/// Stamp `index_id` as queried, unless this search is part of a sweep.
///
/// Why: see the module doc. Rate-limited exactly as before (PR #1103): at most
/// one write per [`LAST_QUERIED_WRITE_INTERVAL_SECS`] per index, decided from
/// the in-memory cache with no disk read on the query path.
/// What: every call joins the text's burst, so a sweep is seen even across
/// indexes whose rate-limit slot is still closed. A call whose slot is open
/// claims it and spawns a task that waits out the window. If the text reached
/// a second index meanwhile, the task drops the stamp and releases the slot so
/// the next targeted query can stamp; otherwise it writes `indexes.toml`.
/// Test: `a_fan_out_burst_leaves_recency_unchanged_and_a_targeted_query_advances_it`.
pub(crate) fn stamp_last_queried(state: &SearchAppState, index_id: &IndexId, query_text: &str) {
    let ticket = state.query_burst_gate.observe(query_text, &index_id.0);
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let slot_open = state
        .last_queried_write_cache
        .get(index_id)
        .map(|prev| now_unix.saturating_sub(*prev) >= LAST_QUERIED_WRITE_INTERVAL_SECS)
        .unwrap_or(true);
    if !slot_open {
        return;
    }
    // Claim the slot now so concurrent queries in the same interval do not
    // all spawn a write.
    state
        .last_queried_write_cache
        .insert(index_id.clone(), now_unix);
    let cache = Arc::clone(&state.last_queried_write_cache);
    let window = state.query_burst_gate.window();
    let id = index_id.clone();
    tokio::spawn(async move {
        tokio::time::sleep(window).await;
        if ticket.is_fan_out() {
            // #8275: a sweep, not use of this index. Release only our own claim.
            cache.remove_if(&id, |_, claimed| *claimed == now_unix);
            tracing::debug!(index_id = %id, "last_queried_unix not stamped: fan-out query (#8275)");
            return;
        }
        // Review MEDIUM (#4871): the write takes the process-wide registry
        // mutex and does synchronous file I/O, so it runs on the blocking pool.
        let id_str = id.0.clone();
        let write = tokio::task::spawn_blocking(move || {
            crate::service::persistence::update_last_queried_unix(&id_str, now_unix)
        })
        .await;
        match write {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::debug!("last_queried_unix update failed for '{id}': {e}"),
            Err(e) => tracing::debug!("last_queried_unix update task failed for '{id}': {e}"),
        }
    });
}

#[cfg(test)]
#[path = "recency_stamp_tests.rs"]
mod tests;
