//! The `events.health` half of the event-bus wire contract (#10901).
//!
//! Why: the `trusty-events` socket served only `events.subscribe`, so a
//! monitor could prove the socket accepts a connect but not that the bus
//! behind it answers. The owner's D8b ruling (2026-11-09) and ADR-0032 (UDS
//! only) put a health method on the same socket, and the contract lives here
//! beside [`super::METHOD_EVENTS_SUBSCRIBE`] for the reason that one does: the
//! daemon and every monitor build against one definition.
//! What: [`METHOD_EVENTS_HEALTH`], the request params ([`EventsHealthParams`])
//! and the reply ([`EventsHealth`]). Types only. The reply carries the
//! `status`/`version`/`uptime_secs` fields every other trusty-* health reply
//! carries (`search.health`, `memory.health`, `mpm.health`), plus the bus's own
//! counters.
//! Test: `tests` below.

// #10901: new module; the daemon serves it, `tm tui`'s Events tile probes it.

use serde::{Deserialize, Serialize};

/// The unary JSON-RPC method that reports the bus's health on its socket.
///
/// Why: a monitor needs one request that only a live bus answers, on the same
/// socket producers and readers dial (ADR-0032: UDS only).
/// What: `"events.health"`; the reply is [`EventsHealth`].
/// Test: `events_health_golden_reply_roundtrip`.
pub const METHOD_EVENTS_HEALTH: &str = "events.health";

/// The `status` value of a bus that answers health.
pub const EVENTS_HEALTH_OK: &str = "ok";

/// The params of one `events.health` request: there are none.
///
/// Why: `RpcRouter::typed` decodes `params` before the handler runs, and a
/// probe sends `{}`, `null` or nothing at all. A unit struct refuses `null`,
/// so every no-params probe would read as `invalid_params`.
/// What: accepts any JSON value and keeps nothing, as `mpm.health`'s
/// `NoParams` does; a stray field is not refused.
/// Test: `events_health_params_accept_any_shape`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct EventsHealthParams {}

impl<'de> Deserialize<'de> for EventsHealthParams {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        serde::de::IgnoredAny::deserialize(deserializer)?;
        Ok(Self {})
    }
}

/// The reply to `events.health`.
///
/// Why: `status` and `version` are what every health probe in the workspace
/// reads (`tm tui`'s home tiles key "Up" off `status`), and the counters let an
/// operator see dedup, eviction and durable-log backpressure without a
/// subscription.
/// What: `status` is [`EVENTS_HEALTH_OK`]; `service` is
/// [`super::EVENTS_SERVICE`]; `uptime_secs` counts from when the daemon began
/// serving. `ingested`, `deduped`, `evicted` and `log_dropped` are the bus's
/// counters since start; `subscribers` is the number of open live receivers;
/// `head_seq` is the last `seq` assigned, `0` before the first event. Each
/// counter is read on its own, so the set is not one atomic snapshot. Unknown
/// fields are accepted, so a newer daemon can add one.
/// Test: `events_health_golden_reply_roundtrip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsHealth {
    /// [`EVENTS_HEALTH_OK`] when the bus answers.
    pub status: String,
    /// [`super::EVENTS_SERVICE`].
    pub service: String,
    /// The daemon's crate version.
    pub version: String,
    /// Seconds since the daemon began serving.
    pub uptime_secs: u64,
    /// Frames accepted into the ring.
    pub ingested: u64,
    /// Frames dropped because their id was already in the ring.
    pub deduped: u64,
    /// Frames evicted from the ring to make room.
    pub evicted: u64,
    /// Frames whose durable-log write was skipped under backpressure.
    pub log_dropped: u64,
    /// Open live receivers, one per `events.subscribe` stream.
    pub subscribers: u64,
    /// The last `seq` assigned; `0` before the first event.
    pub head_seq: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn golden_reply() -> serde_json::Value {
        json!({
            "status": "ok",
            "service": "trusty-events",
            "version": "0.1.0",
            "uptime_secs": 12,
            "ingested": 3,
            "deduped": 1,
            "evicted": 0,
            "log_dropped": 0,
            "subscribers": 2,
            "head_seq": 3,
        })
    }

    #[test]
    fn events_health_golden_reply_roundtrip() {
        assert_eq!(METHOD_EVENTS_HEALTH, "events.health");
        let reply = EventsHealth {
            status: EVENTS_HEALTH_OK.into(),
            service: super::super::EVENTS_SERVICE.into(),
            version: "0.1.0".into(),
            uptime_secs: 12,
            ingested: 3,
            deduped: 1,
            evicted: 0,
            log_dropped: 0,
            subscribers: 2,
            head_seq: 3,
        };
        assert_eq!(
            serde_json::to_value(&reply).expect("encode"),
            golden_reply()
        );
        let mut newer = golden_reply();
        newer["added_later"] = json!(true);
        let back: EventsHealth = serde_json::from_value(newer).expect("unknown field accepted");
        assert_eq!(back, reply);
    }

    #[test]
    fn events_health_params_accept_any_shape() {
        for params in [json!(null), json!({}), json!({"stray": 1})] {
            let decoded: EventsHealthParams =
                serde_json::from_value(params.clone()).expect("any params decode");
            assert_eq!(decoded, EventsHealthParams {}, "{params}");
        }
    }
}
