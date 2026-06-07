//! Subscriber-side event filter for the unified harness bus (ADR-0005).
//!
//! Why: The global bus broadcasts *everything* to *every* subscriber. A given
//!      consumer (a task-scoped SSE stream, a single-harness relay, a
//!      hook-only aggregator) usually wants a slice of that firehose. Rather
//!      than running N filtered channels, subscribers filter in their own recv
//!      loop with a small predicate. Phase 0 deliberately avoids adding a
//!      Stream dependency: `Filter::matches` is the lightweight primitive
//!      consumers will call inside `recv_with_lag` loops in later phases.
//! What: A `Filter` of optional constraints (source, session, domains) plus a
//!       `matches` predicate that ANDs the present constraints. An empty /
//!       default filter matches everything.
//! Test: `super::tests::filter_*` cover each axis, combinations, and the
//!       default-matches-all case.

use super::bus::HarnessEvent;
use super::lifecycle::HarnessSource;

/// A conjunctive (AND) filter over `HarnessEvent`s.
///
/// Why: Subscribers express "only events from harness X, for session Y, in
///      domains Z" without bespoke matching logic at each call site. Optional
///      fields make every constraint opt-in: `None` means "don't constrain on
///      this axis", so `Filter::default()` is the match-all filter.
/// What: `source` constrains the originating harness; `session` constrains the
///       correlation key (an event with no session never matches a session
///       constraint); `domains` constrains the payload domain string
///       (`"lifecycle"`, `"hook"`, `"ping"`). `&'static str` is used for
///       `domains` because the legal domain set is fixed and known at compile
///       time, avoiding allocation for the common constants.
/// Test: `super::tests::filter_default_matches_all` and the per-axis cases.
#[derive(Default, Clone)]
pub struct Filter {
    /// Only match events from this harness, if set.
    pub source: Option<HarnessSource>,
    /// Only match events carrying exactly this session key, if set.
    pub session: Option<String>,
    /// Only match events whose payload domain is in this list, if set.
    pub domains: Option<Vec<&'static str>>,
}

impl Filter {
    /// Whether `ev` satisfies every constraint present in this filter.
    ///
    /// Why: The single predicate subscribers call per received event. ANDing
    ///      only the *present* constraints means an empty filter is a pass-all,
    ///      which is the ergonomic default for "subscribe to everything".
    /// What: For each set field, checks the corresponding envelope field;
    ///       returns `false` on the first mismatch. A `session` constraint
    ///       requires the event to carry that exact session (events with
    ///       `session = None` do not match a session constraint). A `domains`
    ///       constraint matches if the event's domain is in the list.
    /// Test: `super::tests::filter_by_source`, `filter_by_session`,
    ///       `filter_by_domain`, `filter_combination`, `filter_default_matches_all`.
    pub fn matches(&self, ev: &HarnessEvent) -> bool {
        if let Some(source) = self.source
            && source != ev.source
        {
            return false;
        }
        if let Some(session) = &self.session
            && ev.session.as_deref() != Some(session.as_str())
        {
            return false;
        }
        if let Some(domains) = &self.domains
            && !domains.contains(&ev.payload.domain())
        {
            return false;
        }
        true
    }
}
