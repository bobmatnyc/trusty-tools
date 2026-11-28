//! The rate-limited `last_queried_unix` write every read path records.
//!
//! Why: the warm-boot LRU key and the 30-day retention clock (#10534) both
//! read `last_queried_unix`. Before #10534 only `search_report` wrote it, so
//! an index used only through grep, chunk paging or the fan-out read as idle
//! once it was unloaded, and could be evicted while in use.
//! What: [`record_query_stamp`] — the former inline block of `search_report`
//! (#993, PR #1103), shared by search, grep, global grep, chunk pages and the
//! fan-out.
//! Test: `a_grep_chunk_or_fan_out_read_keeps_an_idle_worktree_index`.

use crate::core::registry::IndexId;
use crate::service::lazy_loader::LAST_QUERIED_WRITE_INTERVAL_SECS;

use super::state::SearchAppState;

/// Record a read of `index_id` in `last_queried_unix`, at most once per
/// [`LAST_QUERIED_WRITE_INTERVAL_SECS`].
///
/// What: the in-memory `last_queried_write_cache` decides staleness with no
/// disk read and is updated at once, so concurrent reads in one interval
/// spawn one write. The write runs on the blocking pool: it takes the
/// registry `std::sync::Mutex` (#4871) and does synchronous file I/O. A
/// failed write is logged at debug.
pub(super) async fn record_query_stamp(state: &SearchAppState, index_id: &IndexId) {
    let now_unix = crate::core::project_map::now_unix();
    let stale = state
        .last_queried_write_cache
        .get(index_id)
        .map(|prev| now_unix.saturating_sub(*prev) >= LAST_QUERIED_WRITE_INTERVAL_SECS)
        .unwrap_or(true); // absent = never written for this session → write now
    if !stale {
        return;
    }
    state
        .last_queried_write_cache
        .insert(index_id.clone(), now_unix);
    let id_str = index_id.0.clone();
    let _stamp = tokio::task::spawn_blocking(move || {
        if let Err(e) = crate::service::persistence::update_last_queried_unix(&id_str, now_unix) {
            tracing::debug!("last_queried_unix update failed for '{id_str}': {e}");
        }
    });
    // #9684: a test joins the stamp so it writes inside its serial_test guard.
    #[cfg(test)]
    let _ = _stamp.await;
}
