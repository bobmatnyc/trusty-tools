//! `events.console_metrics`: the bus's counters by source, message direction
//! and project, on the bus socket (#10906, owner ruling D95).
//!
//! Why: D95 has each service report its own metrics over UDS, pulled by the
//! reader (ADR-0032), in the report type every other service's
//! `console_metrics` returns.
//! What: [`register`] adds `METHOD_EVENTS_CONSOLE_METRICS` to the connection
//! router, beside `events.health`. The reply is a [`ConsoleMetricsReport`]
//! whose `metrics` is [`crate::breakdown::Breakdown::to_json`], shaped as
//! `trusty_common::control_bus` documents for that method.
//! Test: `super::serve_tests::events_console_metrics_reports_by_source_messages_and_by_project`,
//! `tests/daemon_wire.rs::events_console_metrics_answers_on_the_real_socket`.

use std::sync::Arc;
use std::time::Instant;

use trusty_common::console_metrics::{ConsoleMetricsReport, ServiceHealth, make_report};
use trusty_common::control_bus::{
    EVENTS_SERVICE, EventsConsoleMetricsParams, METHOD_EVENTS_CONSOLE_METRICS,
};
use trusty_common::uds::server::{RpcError, RpcRouter};

use crate::bus::EventBus;

/// Bumped when the shape of the report's `metrics` changes.
pub(crate) const METRICS_SCHEMA_VERSION: u32 = 1;

/// Register `events.console_metrics` on `router`, reporting `bus` and uptime
/// counted from `started_at`.
///
/// Why: a liveness method like `events.health`: reading counters does no
/// caller work.
/// What: answers [`report`].
/// Test: `super::serve_tests::events_console_metrics_reports_by_source_messages_and_by_project`.
pub(crate) fn register(router: RpcRouter, bus: Arc<EventBus>, started_at: Instant) -> RpcRouter {
    router.typed_liveness(
        METHOD_EVENTS_CONSOLE_METRICS,
        move |_: EventsConsoleMetricsParams| {
            let reply = report(&bus, started_at);
            async move { Ok::<_, RpcError>(reply) }
        },
    )
}

/// The report for `bus` at this instant.
///
/// What: service id `trusty-events`, status `ok` (the bus is answering, as
/// `events.health` reports), this crate's version, and
/// [`METRICS_SCHEMA_VERSION`].
fn report(bus: &EventBus, started_at: Instant) -> ConsoleMetricsReport {
    make_report(
        EVENTS_SERVICE,
        "Events",
        env!("CARGO_PKG_VERSION"),
        ServiceHealth::Ok,
        bus.breakdown().to_json(started_at.elapsed().as_secs()),
        METRICS_SCHEMA_VERSION,
    )
}
