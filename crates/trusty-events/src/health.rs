//! `events.health`: the bus's status, version, uptime and counters, on the bus
//! socket (#10901).
//!
//! Why: with only `events.subscribe` registered, a monitor could prove a
//! connect, not that the bus answers. The owner's D8b ruling (2026-11-09)
//! adds a unary health method on the same socket (ADR-0032: UDS only), so
//! `tm tui`'s Events tile probes it like the search, memory and mpm tiles.
//! What: [`register`] adds [`METHOD_EVENTS_HEALTH`] to the connection router.
//! It answers through the same `handle_connection` peer check as every other
//! request, and reads [`EventBus::metrics`], which takes no lock.
//! Test: `super::serve_tests::events_health_reports_the_bus_counters`,
//! `tests/daemon_wire.rs::events_health_answers_on_the_real_socket`.

use std::sync::Arc;
use std::time::Instant;

use trusty_common::control_bus::{
    EVENTS_HEALTH_OK, EVENTS_SERVICE, EventsHealth, EventsHealthParams, METHOD_EVENTS_HEALTH,
};
use trusty_common::uds::server::{RpcError, RpcRouter};

use crate::bus::EventBus;

/// Register `events.health` on `router`, reporting `bus` and uptime counted
/// from `started_at`.
///
/// Why: one place builds the reply, so the unit test and the daemon serve the
/// same shape.
/// What: a liveness method (it does no caller work) over
/// [`EventsHealthParams`], answering [`EventsHealth`] with this crate's
/// version.
/// Test: `super::serve_tests::events_health_reports_the_bus_counters`,
/// `super::serve_tests::an_unknown_method_still_answers_an_rpc_error`.
pub(crate) fn register(router: RpcRouter, bus: Arc<EventBus>, started_at: Instant) -> RpcRouter {
    router.typed_liveness(METHOD_EVENTS_HEALTH, move |_: EventsHealthParams| {
        let reply = reply(&bus, started_at);
        async move { Ok::<_, RpcError>(reply) }
    })
}

/// The health reply for `bus` at this instant.
fn reply(bus: &EventBus, started_at: Instant) -> EventsHealth {
    let m = bus.metrics();
    EventsHealth {
        status: EVENTS_HEALTH_OK.to_string(),
        service: EVENTS_SERVICE.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_secs: started_at.elapsed().as_secs(),
        ingested: m.ingested,
        deduped: m.deduped,
        evicted: m.evicted,
        log_dropped: m.log_dropped,
        subscribers: m.subscribers,
        head_seq: m.head_seq,
    }
}
