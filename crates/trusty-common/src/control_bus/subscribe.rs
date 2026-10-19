//! The subscribe half of the event-bus wire contract (ADR-0065, DOC-73 §4.4).
//!
//! Why: ADR-0065 runs the bus as its own `trusty-events` daemon, and no other
//!      crate links that daemon's library (decision 1). Producers and readers
//!      meet it only on its socket, so the socket's path and the
//!      `events.subscribe { since_seq, filters }` method are a contract between
//!      independently installed binaries. Decision 3 puts that contract here,
//!      beside the envelope it carries, so the daemon (#9075) and console's
//!      client (#9076) build against one definition.
//! What: The service and method names, [`events_socket_path`], the request
//!       params ([`SubscribeParams`]), the server-side predicate
//!       ([`SubscribeFilter`]) and the streamed response item
//!       ([`SubscribeItem`]). Types only: the request frame that needs the
//!       UDS envelope lives in `subscribe_frame.rs`, behind the `uds` feature.
//!       Request types reject unknown fields, so a filter the server does not
//!       understand fails instead of silently widening the subscription.
//!       Response items accept unknown fields, so a newer daemon can add one
//!       without breaking an older client (DOC-73 §3.3, additive rule).
//! Test: `super::subscribe_tests`.

// #9075: new module. PR1 of epic #9074, before the bus moves out of
// trusty-console; nothing in the workspace calls it yet.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::action::{ActionEvent, Actor};
use super::envelope::{HarnessEvent, HarnessPayload};
use super::filter::Filter;
use super::lifecycle::HarnessSource;

/// The bus daemon's service name: its data-dir name and its socket's stem.
pub const EVENTS_SERVICE: &str = "trusty-events";

/// The streaming JSON-RPC method a reader opens on the bus socket.
pub const METHOD_EVENTS_SUBSCRIBE: &str = "events.subscribe";

/// Resolve the `trusty-events` socket, which producers and readers both dial.
///
/// Why: ADR-0065 decision 2 names the socket after the bus, not its host, and
/// requires every client to resolve it through this one helper. A producer and
/// a reader that each spelled the path would drift, and the daemon would read
/// as down to whichever spelled it wrong (the #4246 class).
/// What: [`crate::daemon_addr::daemon_socket_path`] for [`EVENTS_SERVICE`],
/// that is `<data dir>/trusty-events/trusty-events.sock`. It mirrors that
/// helper exactly, including the ADR-0032 rules it carries: a UDS path, never a
/// TCP port, redirectable in tests through `TRUSTY_DATA_DIR_OVERRIDE`, and
/// unconditional so a caller that only prints the path compiles no socket
/// code. Resolving it creates the data directory, as the mirrored helper does.
///
/// # Errors
///
/// Whatever [`crate::daemon_addr::daemon_socket_path`] returns: the data
/// directory could not be resolved or created. The error is passed through
/// unchanged, so this helper and the one it mirrors fail the same way.
///
/// Test: `events_socket_path_is_named_after_the_bus`.
pub fn events_socket_path() -> anyhow::Result<PathBuf> {
    crate::daemon_addr::daemon_socket_path(EVENTS_SERVICE)
}

/// The params of one `events.subscribe` request.
///
/// Why: DOC-73 §4.3 — a reader that opens mid-session replays what it missed
/// from the durable log, then follows the live ring. `since_seq` is that
/// resume point, and it is also how a client reconnects without a gap after a
/// hang-up or a [`SubscribeItem::Gap`].
/// What: `since_seq: None` streams live events only. `Some(n)` first replays
/// every retained event with `seq > n`, in `seq` order, then streams live;
/// `Some(0)` replays everything retained. `filters` is applied server-side and
/// defaults to match-all, so `{}` is a valid request. Unknown fields are
/// rejected.
/// Test: `subscribe_params_golden_roundtrip`,
/// `subscribe_params_empty_object_is_live_and_match_all`,
/// `subscribe_params_rejects_an_unknown_field`,
/// `subscribe_params_rejects_a_mistyped_field`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscribeParams {
    /// Replay events after this bus `seq` before streaming live ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_seq: Option<u64>,
    /// Which events to deliver. Defaults to every event.
    #[serde(default)]
    pub filters: SubscribeFilter,
}

/// A conjunctive (AND) filter over `HarnessEvent`s that crosses the socket.
///
/// Why: DOC-73 §4.4 — filters apply server-side, so a wall display following
/// one session is not sent every event. [`Filter`] cannot cross a socket: its
/// `domains` are `&'static str` and it is not serde. This is its owned, serde
/// twin, extended with the `kind` and `actor` axes §4.4 names.
/// What: each axis is optional and `None` does not constrain. `source`,
/// `session` and `domains` mean exactly what they mean on [`Filter`].
/// `kind` matches an `Action` payload's kind tag (`"file"`, `"tool"`, ...).
/// `actor` matches an `Action` payload's actor: an agent's `name` or
/// `agent_id`, or the literal `"operator"` or `"system"`. An event that is not
/// an `Action` has neither a kind nor an actor, so it never matches a `kind`
/// or `actor` constraint, as an event with no session never matches a
/// `session` constraint. Values are not checked against a closed set, so a
/// newer client's kind matches nothing on an older daemon; field names are, and
/// an unknown one is a deserialisation error.
/// Test: `subscribe_filter_golden_roundtrip`,
/// `subscribe_filter_agrees_with_filter`,
/// `subscribe_filter_kind_axis_keeps_only_that_action_kind`,
/// `subscribe_filter_actor_axis_keeps_only_that_actor`,
/// `subscribe_filter_rejects_an_unknown_field`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscribeFilter {
    /// Only events from this harness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<HarnessSource>,
    /// Only events carrying exactly this session key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Only events whose payload domain is in this list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domains: Option<Vec<String>>,
    /// Only `Action` events of this kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Only `Action` events performed by this actor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
}

impl SubscribeFilter {
    /// Whether `ev` satisfies every constraint present in this filter.
    ///
    /// Why: the daemon runs this per event per subscriber, and a client can run
    /// the same predicate over a replay it already holds; one definition keeps
    /// both answers equal.
    /// What: returns `false` on the first set axis `ev` fails; see the type's
    /// doc for each axis. Agrees with [`Filter::matches`] on the three axes
    /// both carry.
    /// Test: `subscribe_filter_agrees_with_filter`,
    /// `subscribe_filter_kind_axis_keeps_only_that_action_kind`,
    /// `subscribe_filter_actor_axis_keeps_only_that_actor`.
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
            && !domains.iter().any(|d| d == ev.payload.domain())
        {
            return false;
        }
        let action = match &ev.payload {
            HarnessPayload::Action(action) => Some(action),
            _ => None,
        };
        if let Some(kind) = &self.kind
            && action.map(ActionEvent::kind) != Some(kind.as_str())
        {
            return false;
        }
        if let Some(actor) = &self.actor
            && !action.is_some_and(|a| actor_matches(&a.meta().actor, actor))
        {
            return false;
        }
        true
    }
}

/// Whether `actor` is the one a filter's `actor` value names.
fn actor_matches(actor: &Actor, wanted: &str) -> bool {
    match actor {
        Actor::Agent { name, agent_id } => name == wanted || agent_id == wanted,
        Actor::Operator => wanted == "operator",
        Actor::System => wanted == "system",
    }
}

/// [`Filter`] carried onto the wire, one axis for one axis.
impl From<&Filter> for SubscribeFilter {
    fn from(filter: &Filter) -> Self {
        Self {
            source: filter.source,
            session: filter.session.clone(),
            domains: filter
                .domains
                .as_ref()
                .map(|domains| domains.iter().map(|d| (*d).to_string()).collect()),
            kind: None,
            actor: None,
        }
    }
}

/// One item of an `events.subscribe` stream: the `result` of each
/// `"stream":"item"` frame.
///
/// Why: DOC-73 §4.3 — ring overflow is reported, never silent, and a reader
/// must tell a replayed, durable event from a live one whose write may still be
/// in flight. A bare `HarnessEvent` can say neither.
/// What: internally tagged on `type`. `Event` carries the event, whose `seq` is
/// the bus's own, and `persisted`: `true` when it was read back from the
/// durable log, `false` on live fan-out. `Gap` says the subscriber fell behind
/// the ring and `skipped` events were not delivered; the reader resubscribes
/// with `since_seq` set to the last `seq` it holds to recover them from the
/// log. Unknown fields are ignored; an unknown `type` or a missing field is an
/// error. `#[non_exhaustive]` so a later item type is not a Rust-API break.
/// Test: `subscribe_item_event_golden_roundtrip`,
/// `subscribe_item_gap_golden_roundtrip`,
/// `subscribe_item_ignores_an_unknown_field`,
/// `subscribe_item_rejects_a_malformed_item`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SubscribeItem {
    /// One event, replayed or live.
    Event {
        /// The event, with the `seq` the bus assigned on ingest. Boxed so a
        /// `Gap` is not the size of an event; the wire shape is unchanged.
        event: Box<HarnessEvent>,
        /// `true` when replayed from the durable log.
        persisted: bool,
    },
    /// The subscriber lagged the ring and missed this many events.
    Gap {
        /// How many events were not delivered.
        skipped: u64,
    },
}
