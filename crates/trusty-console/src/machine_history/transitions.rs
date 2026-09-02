//! Per-service state transitions derived from `console_metrics` reports (#6641).
//!
//! Why: the dashboard's service timeline needs the MOMENTS a service changed
//! state, not a row per poll. Polling every few seconds and logging each result
//! would bury the two entries an operator cares about ("search went degraded at
//! 14:02, recovered at 14:05") under hundreds of identical lines. So this module
//! records a transition only when the derived state actually differs from the
//! previous observation.
//! What: [`ServiceState`] is the four-state derivation (`up` / `degraded` /
//! `down` / `unknown`); [`ServiceTransition`] is one logged change;
//! [`TransitionTracker`] holds the last observed state per service and emits a
//! transition only on a change. A service whose report has gone stale past
//! [`SERVICE_REPORT_GRACE_SECS`] — or that stops appearing in the report set for
//! that long — transitions to `down`, because a retained cache entry is not
//! evidence the service is alive (`metrics_poller::poll_once` keeps the previous
//! report when a poll fails).
//! Test: `first_observation_seeds_without_a_transition`,
//! `repeated_identical_reports_log_one_transition`,
//! `a_stale_report_transitions_to_down`,
//! `a_service_that_stops_reporting_goes_down_after_the_grace`,
//! `an_unseen_service_reads_unknown`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use trusty_common::console_metrics::{ConsoleMetricsReport, ServiceHealth};

/// How long a service may go without a fresh report before it reads `down`.
///
/// Why: the metrics poller retains the previous cached report when a poll fails
/// (`metrics_poller::poll_once`), so a dead service keeps presenting its last
/// healthy report forever. Freshness is therefore the only honest liveness
/// signal the console has. 60 s comfortably clears the default 15 s poll cadence
/// plus a slow stdio-MCP round trip, while still surfacing a dead daemon inside
/// a minute.
/// What: `60` seconds; [`TransitionTracker::new`] takes it as a `Duration` so a
/// test can shrink it.
/// Test: `a_stale_report_transitions_to_down`.
pub const SERVICE_REPORT_GRACE_SECS: u64 = 60;

/// The state a service is in, as the transition log records it (#6641).
///
/// Why: [`ServiceHealth`] describes what a service says about ITSELF and has no
/// way to say "we have not heard from you". The timeline needs that fourth
/// state, so the console derives its own.
/// What: `Up` / `Degraded` map from the service's own `Ok` / `Degraded`; `Down`
/// covers both a self-reported `Error` and a report that has gone stale past the
/// grace window; `Unknown` is the state of a service the tracker has never
/// observed. Serialised lowercase.
/// Test: `repeated_identical_reports_log_one_transition`,
/// `an_unseen_service_reads_unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceState {
    /// The service reported `ok` within the grace window.
    Up,
    /// The service reported `degraded` within the grace window.
    Degraded,
    /// The service reported `error`, or has not reported within the grace window.
    Down,
    /// The tracker has never observed this service.
    Unknown,
}

impl From<&ServiceHealth> for ServiceState {
    /// Map a service's self-reported health onto the log's state.
    fn from(h: &ServiceHealth) -> Self {
        match h {
            ServiceHealth::Ok => ServiceState::Up,
            ServiceHealth::Degraded => ServiceState::Degraded,
            ServiceHealth::Error => ServiceState::Down,
        }
    }
}

/// One recorded change of a service's state (#6641).
///
/// Why: the timeline draws a band per state, so it needs both endpoints of the
/// change and when it happened — a `to` alone would leave the UI re-deriving the
/// previous state by scanning backwards.
/// What: the service's id and display name (so the UI need not join against the
/// rollup), the state it left, the state it entered, and the wall-clock second
/// the console observed the change.
/// Test: `repeated_identical_reports_log_one_transition`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceTransition {
    /// Machine-readable service id (e.g. `"trusty-search"`).
    pub service_id: String,
    /// Human-readable display name for the timeline row.
    pub display_name: String,
    /// The state the service was in before this observation.
    pub from: ServiceState,
    /// The state the service is in now.
    pub to: ServiceState,
    /// Unix seconds when the console observed the change.
    pub at_unix: u64,
}

/// What the tracker remembers about one service between observations.
struct Observed {
    state: ServiceState,
    display_name: String,
    last_report: Instant,
}

/// Last-observed state per service, emitting a transition only on a change
/// (#6641).
///
/// Why: this is the "never on every poll" rule made mechanical. Callers hand it
/// the whole current report set on every tick and get back only the changes.
/// What: keyed by `service_id`. [`TransitionTracker::observe`] derives each
/// service's state, applies the grace window to stale and missing reports, and
/// returns one [`ServiceTransition`] per service whose state moved. The FIRST
/// observation of a service seeds its state and emits nothing — the log is a
/// record of changes, and a service's current state is already served by the
/// machine-status rollup.
/// Test: `first_observation_seeds_without_a_transition`,
/// `repeated_identical_reports_log_one_transition`,
/// `a_service_that_stops_reporting_goes_down_after_the_grace`.
pub struct TransitionTracker {
    grace: Duration,
    seen: HashMap<String, Observed>,
}

impl Default for TransitionTracker {
    fn default() -> Self {
        Self::new(Duration::from_secs(SERVICE_REPORT_GRACE_SECS))
    }
}

impl TransitionTracker {
    /// Build a tracker with an explicit staleness grace window.
    ///
    /// Why: the console uses [`SERVICE_REPORT_GRACE_SECS`]; a test needs a
    /// window it can cross without sleeping for a minute.
    /// What: stores `grace` and an empty per-service map.
    /// Test: `a_stale_report_transitions_to_down`.
    #[must_use]
    pub fn new(grace: Duration) -> Self {
        Self {
            grace,
            seen: HashMap::new(),
        }
    }

    /// The state currently attributed to `service_id`.
    ///
    /// Why: the history payload can state where each service stands without the
    /// caller replaying the log.
    /// What: the stored state, or [`ServiceState::Unknown`] for a service never
    /// observed.
    /// Test: `an_unseen_service_reads_unknown`.
    #[must_use]
    pub fn state_of(&self, service_id: &str) -> ServiceState {
        self.seen
            .get(service_id)
            .map_or(ServiceState::Unknown, |o| o.state)
    }

    /// Fold one observation of the whole report set into the log.
    ///
    /// Why: one entry point keeps the change rule in a single place — a caller
    /// cannot accidentally append on a poll that changed nothing.
    /// What: derives each reported service's state (stale reports past `grace`
    /// read `down`), then marks any previously-known service missing from
    /// `reports` for longer than `grace` as `down`. Returns a transition only
    /// where the derived state differs from the stored one; a service seen for
    /// the first time is seeded silently. `now` and `now_unix` are passed in so
    /// the grace window is testable without sleeping.
    /// Test: `repeated_identical_reports_log_one_transition`,
    /// `a_stale_report_transitions_to_down`,
    /// `a_service_that_stops_reporting_goes_down_after_the_grace`.
    pub fn observe(
        &mut self,
        reports: &[ConsoleMetricsReport],
        now: Instant,
        now_unix: u64,
    ) -> Vec<ServiceTransition> {
        let mut out = Vec::new();

        for report in reports {
            // #6641: a retained cache entry is not liveness — a report the
            // service collected before the grace window reads `down` whatever
            // health it claims.
            let stale = report
                .collected_at_unix
                .is_some_and(|t| now_unix.saturating_sub(t) > self.grace.as_secs());
            let state = if stale {
                ServiceState::Down
            } else {
                ServiceState::from(&report.status)
            };
            self.apply(
                &report.service_id,
                &report.display_name,
                state,
                now,
                now_unix,
                &mut out,
            );
        }

        // A service that has dropped out of the report set entirely — its
        // binary is gone, or the poller never got a first report — goes down
        // once the grace window has passed since its last report.
        let missing: Vec<(String, String)> = self
            .seen
            .iter()
            .filter(|(id, o)| {
                o.state != ServiceState::Down
                    && now.duration_since(o.last_report) > self.grace
                    && !reports.iter().any(|r| &r.service_id == *id)
            })
            .map(|(id, o)| (id.clone(), o.display_name.clone()))
            .collect();
        for (id, display_name) in missing {
            self.mark_down(&id, &display_name, now_unix, &mut out);
        }

        out
    }

    /// Store `state` for `service_id`, pushing a transition when it changed.
    ///
    /// Why: the single mutation point, so the "changed only" rule and the
    /// last-report timestamp update can never drift apart.
    /// What: seeds an unseen service silently; otherwise compares against the
    /// stored state and appends a [`ServiceTransition`] on a difference. Always
    /// refreshes `last_report`, because this path is only reached from a live
    /// report.
    /// Test: `first_observation_seeds_without_a_transition`.
    fn apply(
        &mut self,
        service_id: &str,
        display_name: &str,
        state: ServiceState,
        now: Instant,
        now_unix: u64,
        out: &mut Vec<ServiceTransition>,
    ) {
        match self.seen.get_mut(service_id) {
            None => {
                self.seen.insert(
                    service_id.to_string(),
                    Observed {
                        state,
                        display_name: display_name.to_string(),
                        last_report: now,
                    },
                );
            }
            Some(prev) => {
                prev.last_report = now;
                prev.display_name = display_name.to_string();
                if prev.state != state {
                    let from = prev.state;
                    prev.state = state;
                    out.push(ServiceTransition {
                        service_id: service_id.to_string(),
                        display_name: display_name.to_string(),
                        from,
                        to: state,
                        at_unix: now_unix,
                    });
                }
            }
        }
    }

    /// Move a known service to `Down` without refreshing its last-report time.
    ///
    /// Why: the missing-report path must not pretend it heard from the service;
    /// refreshing `last_report` here would re-arm the grace window forever.
    /// What: sets the stored state to `Down` and appends the transition.
    /// Test: `a_service_that_stops_reporting_goes_down_after_the_grace`.
    fn mark_down(
        &mut self,
        service_id: &str,
        display_name: &str,
        now_unix: u64,
        out: &mut Vec<ServiceTransition>,
    ) {
        if let Some(prev) = self.seen.get_mut(service_id) {
            let from = prev.state;
            prev.state = ServiceState::Down;
            out.push(ServiceTransition {
                service_id: service_id.to_string(),
                display_name: display_name.to_string(),
                from,
                to: ServiceState::Down,
                at_unix: now_unix,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_common::console_metrics::make_report;

    fn unix_now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }

    fn report(status: ServiceHealth, collected_at: u64) -> ConsoleMetricsReport {
        let mut r = make_report(
            "trusty-search",
            "Trusty Search",
            "1.0.0",
            status,
            serde_json::json!({}),
            1,
        );
        r.collected_at_unix = Some(collected_at);
        r
    }

    /// Why: a service the console has never seen has no previous state to have
    /// changed FROM, so its first report must not manufacture a transition.
    /// What: one report into a fresh tracker returns no transitions, and the
    /// state is nevertheless recorded.
    /// Test: this test.
    #[test]
    fn first_observation_seeds_without_a_transition() {
        let mut t = TransitionTracker::default();
        let now = Instant::now();
        let unix = unix_now();
        let out = t.observe(&[report(ServiceHealth::Ok, unix)], now, unix);
        assert!(out.is_empty(), "the first observation must log nothing");
        assert_eq!(t.state_of("trusty-search"), ServiceState::Up);
    }

    /// Why: this is the rule the whole module exists for — an entry is appended
    /// ONLY on a change, never on every poll.
    /// What: feeds five identical `ok` reports, then one `degraded` report, and
    /// asserts exactly ONE transition entry across all six observations.
    /// Test: this test.
    #[test]
    fn repeated_identical_reports_log_one_transition() {
        let mut t = TransitionTracker::default();
        let start = Instant::now();
        let unix = unix_now();
        let mut log: Vec<ServiceTransition> = Vec::new();

        for i in 0..5u64 {
            log.extend(t.observe(
                &[report(ServiceHealth::Ok, unix + i)],
                start + Duration::from_secs(i),
                unix + i,
            ));
        }
        assert!(
            log.is_empty(),
            "five identical reports must log nothing, got {log:?}"
        );

        log.extend(t.observe(
            &[report(ServiceHealth::Degraded, unix + 5)],
            start + Duration::from_secs(5),
            unix + 5,
        ));

        assert_eq!(log.len(), 1, "exactly one transition, got {log:?}");
        assert_eq!(log[0].from, ServiceState::Up);
        assert_eq!(log[0].to, ServiceState::Degraded);
        assert_eq!(log[0].service_id, "trusty-search");
        assert_eq!(log[0].display_name, "Trusty Search");
    }

    /// Why: `metrics_poller::poll_once` retains the previous report when a poll
    /// fails, so a dead service keeps presenting a healthy report. Freshness is
    /// the only honest liveness signal.
    /// What: seeds an `ok` report, then re-observes the SAME report 30 s later
    /// against a 10 s grace, and asserts it transitioned to `down`.
    /// Test: this test.
    #[test]
    fn a_stale_report_transitions_to_down() {
        let mut t = TransitionTracker::new(Duration::from_secs(10));
        let start = Instant::now();
        let unix = unix_now();
        assert!(
            t.observe(&[report(ServiceHealth::Ok, unix)], start, unix)
                .is_empty()
        );

        let out = t.observe(
            &[report(ServiceHealth::Ok, unix)],
            start + Duration::from_secs(30),
            unix + 30,
        );
        assert_eq!(out.len(), 1, "a stale report is a transition, got {out:?}");
        assert_eq!(out[0].to, ServiceState::Down);
        assert_eq!(t.state_of("trusty-search"), ServiceState::Down);
    }

    /// Why: a service whose binary vanishes drops out of the report set
    /// entirely; nothing else would ever move it off `up`.
    /// What: seeds an `ok` report, then observes an EMPTY report set inside the
    /// grace (no transition) and again past it (one `down` transition), then a
    /// third time to prove the down state is not re-logged.
    /// Test: this test.
    #[test]
    fn a_service_that_stops_reporting_goes_down_after_the_grace() {
        let mut t = TransitionTracker::new(Duration::from_secs(10));
        let start = Instant::now();
        let unix = unix_now();
        t.observe(&[report(ServiceHealth::Ok, unix)], start, unix);

        let inside = t.observe(&[], start + Duration::from_secs(5), unix + 5);
        assert!(inside.is_empty(), "inside the grace nothing changes");

        let past = t.observe(&[], start + Duration::from_secs(30), unix + 30);
        assert_eq!(past.len(), 1, "past the grace the service goes down");
        assert_eq!(past[0].from, ServiceState::Up);
        assert_eq!(past[0].to, ServiceState::Down);

        let again = t.observe(&[], start + Duration::from_secs(60), unix + 60);
        assert!(again.is_empty(), "down is not re-logged every tick");
    }

    /// Why: the payload must be able to state where a service stands without
    /// inventing a state for one it has never heard of.
    /// What: asserts `state_of` on an unobserved id reads `unknown`.
    /// Test: this test.
    #[test]
    fn an_unseen_service_reads_unknown() {
        let t = TransitionTracker::default();
        assert_eq!(t.state_of("trusty-memory"), ServiceState::Unknown);
    }
}
