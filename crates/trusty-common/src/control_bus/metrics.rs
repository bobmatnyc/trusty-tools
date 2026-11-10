//! The `events.console_metrics` half of the event-bus wire contract (#10906).
//!
//! Why: owner ruling D95 has the bus report its counters, broken down by
//! source and by project, over its own socket, pulled by the reader. The
//! method name and the reserved breakdown keys live here beside
//! [`super::METHOD_EVENTS_HEALTH`] so the daemon and every reader build
//! against one definition.
//! What: [`METHOD_EVENTS_CONSOLE_METRICS`], its params
//! ([`EventsConsoleMetricsParams`]), and the reserved `by_project` keys
//! [`UNATTRIBUTED_PROJECT`] and [`OVERFLOW_PROJECT`]. Types only. The reply is
//! a `console_metrics::ConsoleMetricsReport` (feature `console-metrics`),
//! whose `metrics` follows the `by_*` convention documented in
//! `console_metrics::by_convention`. The trusty-events `metrics` shape:
//!
//! ```json
//! {
//!   "uptime_secs": 12,
//!   "events": 3,
//!   "by_source": {"mpm": {"events": 3, "by_domain": {"message": 2, "hook": 1}}},
//!   "messages": {"sent": 1, "received": 1},
//!   "by_project": {
//!     "unattributed": {"events": 3, "by_source": {}, "messages": {"sent": 1, "received": 1}}
//!   }
//! }
//! ```
//!
//! `events` counts the frames the bus accepted (a dedup hit is not counted);
//! `by_source` splits them by `HarnessEvent::source`, then by payload domain;
//! `messages` counts `message`-domain events by `MessageEvent::direction`,
//! with both keys always present. Each `by_project` entry has the same three
//! fields for that project's events alone. (The example's `unattributed`
//! entry abbreviates its `by_source`.)
//! Test: `super::tests::events_console_metrics_contract_names_10906`.

// #10906: new module; trusty-events serves it.

/// The unary JSON-RPC method that answers the bus's counters on its socket.
pub const METHOD_EVENTS_CONSOLE_METRICS: &str = "events.console_metrics";

/// The params of one `events.console_metrics` request: there are none, and
/// any shape is accepted, as for [`super::EventsHealthParams`].
pub type EventsConsoleMetricsParams = super::EventsHealthParams;

/// The `by_project` key for an event whose `project` is absent or blank.
pub const UNATTRIBUTED_PROJECT: &str = "unattributed";

/// The `by_project` key for an event whose project arrived after the bus
/// already holds its maximum number of distinct projects.
///
/// Why: `project` is a producer-supplied string, so an unbounded map keyed by
/// it would grow for the daemon's whole life (convention rule 6).
pub const OVERFLOW_PROJECT: &str = "overflow";
