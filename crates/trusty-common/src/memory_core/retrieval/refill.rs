//! The bounded refill a recall lane runs when expired slots empty its window (#9390).
//!
//! Why: ADR-0028 D6 never deletes an expired Tier C slot, and every recall lane
//! drops expired drawers only after it fetched its window. A pile of expired
//! slots that matches a query better than the live slot can fill that window,
//! so the filter leaves the live slot unreturned. Refilling past the pile keeps
//! the D6 retention ruling and still returns the current fact.
//! What: [`next_refill_window`] is the one stop rule both lanes use: the vector
//! lane (`candidates::search_live_candidates`, L2 and L3) and trusty-memory's
//! BM25 warming lane. Each lane fetches its usual window first, so recall with
//! no expired hit is unchanged.
//! Test: `the_refill_stops_on_every_bound`, `l2_and_l3_refill_past_expired_drawers`.

/// The most candidates one lane fetches for one recall while refilling.
///
/// Why 1024: a refill round re-runs the lane's search and joins every hit to
/// its drawer, so the cost grows with the window. 1024 is 17 times the default
/// L2 window (60 at `top_k` 10), reached from that window in five doublings and
/// from a window of one in ten, so one recall's worst case stays bounded. A
/// pile larger than the cap can still starve a slot; deleting expired slots is
/// the retention question ADR-0028 D6 leaves to the owner.
pub const EXPIRED_REFILL_FETCH_CAP: usize = 1024;

/// The next window a lane fetches, or `None` when it stops.
///
/// What: stops when no fetched hit was expired (nothing to refill past), when
/// `live` already reaches `target`, when the lane returned fewer than it asked
/// for (the corpus is exhausted), or when `fetched` reached
/// [`EXPIRED_REFILL_FETCH_CAP`]. Otherwise doubles the window, capped. The
/// window strictly grows until the cap, so a loop over this ends.
/// Test: `the_refill_stops_on_every_bound`.
pub fn next_refill_window(
    fetched: usize,
    returned: usize,
    live: usize,
    expired: usize,
    target: usize,
) -> Option<usize> {
    let done = fetched == 0
        || expired == 0
        || live >= target
        || returned < fetched
        || fetched >= EXPIRED_REFILL_FETCH_CAP;
    (!done).then(|| fetched.saturating_mul(2).min(EXPIRED_REFILL_FETCH_CAP))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_refill_stops_on_every_bound() {
        // Starved and not exhausted: double.
        assert_eq!(next_refill_window(60, 60, 0, 60, 20), Some(120));
        // Nothing expired, enough live, exhausted, at the cap, empty window.
        assert_eq!(next_refill_window(60, 60, 0, 0, 20), None);
        assert_eq!(next_refill_window(60, 60, 20, 40, 20), None);
        assert_eq!(next_refill_window(60, 41, 1, 40, 20), None);
        assert_eq!(next_refill_window(1024, 1024, 0, 1024, 20), None);
        assert_eq!(next_refill_window(0, 0, 0, 0, 0), None);
        // The last step is capped, not doubled past the cap.
        assert_eq!(next_refill_window(640, 640, 0, 640, 20), Some(1024));
        // Every starved loop ends within the cap.
        let (mut window, mut rounds) = (1, 0);
        while let Some(next) = next_refill_window(window, window, 0, window, 20) {
            assert!(next > window, "the window must grow");
            window = next;
            rounds += 1;
        }
        assert_eq!((window, rounds), (EXPIRED_REFILL_FETCH_CAP, 10));
    }
}
