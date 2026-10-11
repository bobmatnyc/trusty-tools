//! Inner-loop work counters for the here-document scanners (#9720).
//!
//! Why: a scan whose work grows with the square of its `<<` count turns a
//! 64 KiB command into a hook timeout, and a timed-out hook lets the command
//! through. A wall-clock test of that depends on the host; a count of the
//! inner-loop steps does not.
//! What: [`charge`] adds `units` to one [`Site`]'s counter. Outside tests it
//! does nothing. Under `cfg(test)` the counters are thread-local, so tests
//! running in parallel do not share them, and the test-only `take` reads and
//! resets one.
//! Test: `heredoc_operators_track_pending_lows_in_linear_work_9720`,
//! `heredoc_bodies_look_up_operators_in_log_work_9720`,
//! `heredoc_operators_find_each_line_end_once_9720`,
//! `heredoc_bodies_answer_span_queries_in_log_work_9720`,
//! `heredoc_bodies_check_arithmetic_shifts_in_linear_work_9720`.

/// The scan loops #9720 found quadratic.
#[derive(Clone, Copy)]
pub(crate) enum Site {
    /// The walk's low-water marks of the operators waiting for a body.
    PendingLow,
    /// Finding a line's `<<` in the walk's operator list.
    OperatorLookup,
    /// Bytes searched for the end of an operator's line.
    LineEnd,
    /// Span-list probes behind a `HeredocBodies` query.
    SpanProbe,
    /// Lines compared while asking whether an arithmetic `<<` could end.
    AmbiguousEnd,
}

/// Count `units` of work at `site`; a no-op outside tests.
#[cfg(not(test))]
#[inline(always)]
pub(crate) fn charge(_site: Site, _units: usize) {}

/// Count `units` of work at `site` on this thread.
#[cfg(test)]
pub(crate) fn charge(site: Site, units: usize) {
    counters::UNITS.with(|all| {
        let cell = &all[site as usize];
        cell.set(cell.get().saturating_add(units as u64));
    });
}

/// This thread's count at `site` since the last call, which resets it.
#[cfg(test)]
pub(crate) fn take(site: Site) -> u64 {
    counters::UNITS.with(|all| all[site as usize].replace(0))
}

#[cfg(test)]
mod counters {
    use std::cell::Cell;

    thread_local! {
        /// One counter per [`super::Site`].
        pub(super) static UNITS: [Cell<u64>; 5] = const {
            [Cell::new(0), Cell::new(0), Cell::new(0), Cell::new(0), Cell::new(0)]
        };
    }
}
