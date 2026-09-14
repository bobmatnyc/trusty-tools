//! `GET /api/console/events/stream` — the event-bus ring as one JSON page
//! (issue #6850, DOC-73 §4.4).
//!
//! Why: console is the workspace's only event bus (owner ruling 2026-09-05,
//! DOC-73 §4.1), and a programmatic reader — plus the dashboard's own initial
//! backfill before it opens the SSE stream — needs a cursor-based pull over
//! what the bus retains. The ruling also removed the alternative: there is no
//! drained per-daemon cursor and no 250ms poller, so this route reads the ring
//! `#6848` built, directly.
//!
//! Why filtering is server-side: DOC-73 §4.4 — "server-side filtering is what
//! lets a wall display subscribe to one session without shipping every event
//! to it". The four axes are `source`, `session`, `kind` and `actor`, ANDed,
//! extending `trusty_common::control_bus::Filter`'s source/session/domain
//! predicate with the two axes DOC-73 §3.2 puts on an `ActionEvent`.
//!
//! What: [`events_stream_handler`] parses the query into an [`EventsRequest`],
//! reads the ring once through
//! [`EventBus::events_since`](crate::event_bus::EventBus::events_since), and
//! answers [`EventsPage`] — `{events, next_seq, dropped}`, the exact shape
//! DOC-73 §4.4's table names. Every parse failure is a `400`; nothing here
//! falls back to returning more than was asked for.
//!
//! Test: this module's own `tests`, driving the built router end to end.
//! `crate::event_bus::tests::events_since_*` covers the ring read underneath.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use trusty_common::control_bus::{Actor, HarnessEvent, HarnessPayload, HarnessSource};

use crate::event_bus::RingSlice;
use crate::server::AppState;

/// The largest page this route will return, and the ceiling on `limit`.
///
/// Matches `event_bus::bus::DEFAULT_CAPACITY`, so the default ring can be
/// drained in one request and a caller cannot ask for a page larger than the
/// bus could ever hold.
const MAX_LIMIT: usize = 8192;

/// The six `kind` values DOC-73 §3.2 defines, in `ActionEvent::kind()`'s
/// spelling. A `kind` outside this set is a `400`, not an empty result — see
/// [`EventsRequest::parse`].
const ACTION_KINDS: [&str; 6] = [
    "workflow",
    "agent",
    "file",
    "tool",
    "session",
    "inference",
];

/// The raw query string, every axis a `String`.
///
/// Why not typed fields: axum's own `Query` rejection says only that
/// deserialization failed, and a dashboard operator debugging a filter needs
/// to know WHICH parameter it rejected and what the legal values are.
/// Parsing in [`EventsRequest::parse`] buys that message.
/// Why `deny_unknown_fields`: a misspelled filter (`sess=abc`) would otherwise
/// be ignored and the route would answer with everything — a filter silently
/// widening to match-all is the fail-open this route must not have.
/// Test: `unknown_query_parameter_is_a_400`, `bad_source_is_a_400`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsQuery {
    since_seq: Option<String>,
    source: Option<String>,
    session: Option<String>,
    kind: Option<String>,
    actor: Option<String>,
    limit: Option<String>,
}

/// One validated request: the cursor, the page size, and the ANDed filter.
#[derive(Debug, Default, PartialEq)]
struct EventsRequest {
    /// Exclusive lower bound on `seq`; `0` means "everything retained".
    since_seq: u64,
    /// Maximum events in the response, `1..=MAX_LIMIT`.
    limit: usize,
    filter: EventFilter,
}

/// The four filter axes, ANDed. `None` on an axis means "do not constrain".
#[derive(Debug, Default, PartialEq)]
struct EventFilter {
    source: Option<HarnessSource>,
    session: Option<String>,
    /// One of [`ACTION_KINDS`]. Only an `action` payload carries a kind, so a
    /// kind constraint never matches a lifecycle, hook or ping event.
    kind: Option<String>,
    /// An agent's display name or `agent_id`, or the literal `operator` /
    /// `system`. Free-form by construction — an agent name is not a closed
    /// set — so an unrecognised value is an empty result, not a `400`.
    actor: Option<String>,
}

impl EventsRequest {
    /// Validate one query string, or say exactly which parameter was wrong.
    ///
    /// Why it returns the message rather than a status: the caller renders it
    /// as the `400` body, and keeping the text here puts the legal values
    /// beside the check that enforces them.
    /// What: `since_seq` and `limit` must parse as integers, `limit` must be
    /// `1..=MAX_LIMIT`, and `source` and `kind` must name one of their closed
    /// sets. An absent parameter takes its default (`since_seq` 0, `limit`
    /// [`MAX_LIMIT`], every filter axis unconstrained). `session` and `actor`
    /// are free-form and cannot fail.
    /// Test: `bad_source_is_a_400`, `bad_kind_is_a_400`,
    /// `bad_since_seq_is_a_400`, `zero_limit_is_a_400`,
    /// `over_max_limit_is_a_400`.
    fn parse(q: &EventsQuery) -> Result<Self, String> {
        let since_seq = match &q.since_seq {
            Some(raw) => raw
                .parse::<u64>()
                .map_err(|_| format!("since_seq must be a non-negative integer, got {raw:?}"))?,
            None => 0,
        };
        let limit = match &q.limit {
            Some(raw) => {
                let n = raw
                    .parse::<usize>()
                    .map_err(|_| format!("limit must be a positive integer, got {raw:?}"))?;
                if n == 0 || n > MAX_LIMIT {
                    return Err(format!("limit must be between 1 and {MAX_LIMIT}, got {n}"));
                }
                n
            }
            None => MAX_LIMIT,
        };
        let source = match q.source.as_deref() {
            Some("agents") => Some(HarnessSource::Agents),
            Some("mpm") => Some(HarnessSource::Mpm),
            Some("code") => Some(HarnessSource::Code),
            Some(other) => {
                return Err(format!(
                    "source must be one of agents, mpm, code — got {other:?}"
                ));
            }
            None => None,
        };
        let kind = match q.kind.as_deref() {
            Some(k) if ACTION_KINDS.contains(&k) => Some(k.to_string()),
            Some(other) => {
                return Err(format!(
                    "kind must be one of {} — got {other:?}",
                    ACTION_KINDS.join(", ")
                ));
            }
            None => None,
        };
        Ok(Self {
            since_seq,
            limit,
            filter: EventFilter {
                source,
                session: q.session.clone(),
                kind,
                actor: q.actor.clone(),
            },
        })
    }
}

impl EventFilter {
    /// Whether `ev` satisfies every constraint that is present.
    ///
    /// Why an empty filter passes everything: the same rule
    /// `trusty_common::control_bus::Filter::matches` already established, so a
    /// caller that omits an axis is not silently narrowed.
    /// What: ANDs the present constraints and returns on the first mismatch.
    /// `source` and `session` read the envelope; `kind` and `actor` read the
    /// `ActionEvent` DOC-73 §3.2 defines them on, so an event whose payload is
    /// not an action fails either of those constraints rather than matching
    /// vacuously.
    /// Test: `each_filter_axis_alone_selects_its_events`,
    /// `filters_are_anded_together`,
    /// `a_kind_filter_excludes_non_action_payloads`,
    /// `an_actor_filter_matches_name_or_agent_id`.
    fn matches(&self, ev: &HarnessEvent) -> bool {
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
        if self.kind.is_none() && self.actor.is_none() {
            return true;
        }
        let HarnessPayload::Action(action) = &ev.payload else {
            return false;
        };
        if let Some(kind) = &self.kind
            && kind != action.kind()
        {
            return false;
        }
        if let Some(actor) = &self.actor
            && !actor_matches(&action.meta().actor, actor)
        {
            return false;
        }
        true
    }
}

/// Whether `actor` names this [`Actor`].
///
/// Why both `name` and `agent_id` match: a wall display filters by the agent
/// name an operator can read, while a link from the object viewer (DOC-73 §6)
/// carries the stable id. One axis answering both keeps the query string from
/// needing two.
/// What: an `Agent` matches its display name or its `agent_id`; `Operator` and
/// `System` match those literal words. `Actor` is `#[non_exhaustive]`, so a
/// variant added later matches nothing until this function names it — the
/// fail-closed direction.
/// Test: `an_actor_filter_matches_name_or_agent_id`,
/// `an_actor_filter_matches_operator_and_system`.
fn actor_matches(actor: &Actor, want: &str) -> bool {
    match actor {
        Actor::Agent { name, agent_id } => name == want || agent_id == want,
        Actor::Operator => want == "operator",
        Actor::System => want == "system",
        _ => false,
    }
}

/// One page of the bus ring — the `{events, next_seq, dropped}` shape DOC-73
/// §4.4's route table names.
///
/// Why `next_seq` is not simply the last event's `seq`: the scan examines
/// events the filter rejects, and a caller that resumed from the last MATCHED
/// seq would re-scan every rejected event on the next request forever. It is
/// the last `seq` examined instead, so the cursor always advances.
/// Why `dropped` exists at all: the ring is bounded (DOC-73 §4.3), so a caller
/// whose cursor has fallen behind retention has permanently lost events. This
/// route reports that count rather than returning a shorter list that looks
/// complete — DOC-73 §4.3's "overflow is reported, never silent". `dropped`
/// counts the gap in the `seq` space, before filtering: it says how many
/// events existed and are gone, not how many of them would have matched.
/// What: `events` is oldest-first; `next_seq` is what to pass as the next
/// request's `since_seq`; `dropped` is `oldest_retained - since_seq - 1` when
/// retention has moved past the cursor, and `0` otherwise.
/// Test: `dropped_is_zero_when_the_cursor_is_inside_the_ring`,
/// `dropped_counts_the_gap_when_the_cursor_predates_retention`,
/// `next_seq_advances_past_filtered_out_events`.
#[derive(Debug, Serialize)]
pub struct EventsPage {
    /// Matching retained events with `seq > since_seq`, oldest first.
    pub events: Vec<HarnessEvent>,
    /// The cursor to send as `since_seq` on the next request.
    pub next_seq: u64,
    /// How many events between the cursor and the oldest retained event were
    /// evicted before this read. `0` when nothing was lost.
    pub dropped: u64,
}

impl EventsPage {
    /// Derive the page from one ring read and the cursor that produced it.
    fn from_slice(slice: RingSlice, since_seq: u64) -> Self {
        // The caller asked for `since_seq + 1` onward; retention starts at
        // `oldest`. Everything in between is gone. `saturating_*` covers both
        // a cursor already at or past `oldest` and a `since_seq` of `u64::MAX`.
        let dropped = slice
            .oldest_seq
            .map(|oldest| oldest.saturating_sub(since_seq.saturating_add(1)))
            .unwrap_or(0);
        Self {
            events: slice.events,
            next_seq: slice.last_examined_seq.unwrap_or(since_seq),
            dropped,
        }
    }
}

/// `GET /api/console/events/stream` — one cursor-based page of the bus ring.
///
/// Why: DOC-73 §4.4's first row — the programmatic reader's route, and the
/// browser's initial backfill before it attaches to the SSE stream (#6851).
/// What: `200` with an [`EventsPage`] on success. `400` with the offending
/// parameter named when the query does not validate — including an unknown
/// parameter, which would otherwise be an ignored filter silently widening the
/// result. `503` when this console has no event bus wired at all, which is a
/// wiring fault and distinct from a bus that is merely empty: an empty bus
/// answers `200` with an empty list.
/// Test: `empty_bus_returns_an_empty_page`, `bus_absent_is_a_503`, and the
/// filter/cursor cases in this module's `tests`.
pub async fn events_stream_handler(
    State(state): State<AppState>,
    Query(query): Query<EventsQuery>,
) -> Response {
    let Some(bus) = state.event_bus() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the console event bus is not running",
        )
            .into_response();
    };
    let request = match EventsRequest::parse(&query) {
        Ok(request) => request,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    let slice = bus.events_since(
        request.since_seq,
        &|ev| request.filter.matches(ev),
        request.limit,
    );
    axum::Json(EventsPage::from_slice(slice, request.since_seq)).into_response()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use chrono::Utc;
    use http_body_util::BodyExt;
    use serde_json::Value;
    use tower::ServiceExt;
    use trusty_common::control_bus::{
        ActionEvent, ActionMeta, Actor, EventId, HarnessEvent, HarnessPayload, HarnessSource,
        SessionPhase,
    };

    use super::*;
    use crate::event_bus::{EventBus, EventBusConfig};
    use crate::server::build_router;

    /// A `ping`-payload event — no kind, no actor — for the axes that live on
    /// the envelope.
    fn ping(source: HarnessSource, session: Option<&str>) -> HarnessEvent {
        HarnessEvent {
            source,
            session: session.map(str::to_string),
            seq: 0,
            at: Utc::now(),
            payload: HarnessPayload::Ping,
            id: EventId::new(),
            parent_id: None,
        }
    }

    /// A `session`-kind action event attributed to `actor`.
    fn action(source: HarnessSource, session: Option<&str>, actor: Actor) -> HarnessEvent {
        let id = EventId::new();
        HarnessEvent {
            source,
            session: session.map(str::to_string),
            seq: 0,
            at: Utc::now(),
            payload: HarnessPayload::Action(ActionEvent::Session {
                meta: ActionMeta {
                    id,
                    at: Utc::now(),
                    source,
                    session: session.map(str::to_string),
                    parent_id: None,
                    actor,
                    objects: Vec::new(),
                    schema_version: 1,
                },
                phase: SessionPhase::Started,
            }),
            id,
            parent_id: None,
        }
    }

    fn agent(name: &str, agent_id: &str) -> Actor {
        Actor::Agent {
            name: name.to_string(),
            agent_id: agent_id.to_string(),
        }
    }

    /// A router whose state carries a bus seeded with `events`, in order.
    fn router_with(events: Vec<HarnessEvent>) -> axum::Router {
        router_with_capacity(events, 64)
    }

    fn router_with_capacity(events: Vec<HarnessEvent>, capacity: usize) -> axum::Router {
        let bus = Arc::new(EventBus::new(EventBusConfig { capacity }));
        for event in events {
            bus.ingest(event);
        }
        build_router(AppState::new(Vec::new()).with_event_bus(bus))
    }

    async fn get(router: axum::Router, uri: &str) -> (StatusCode, Vec<u8>) {
        let response = router
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route responds");
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("collect body")
            .to_bytes()
            .to_vec();
        (status, body)
    }

    /// `(status, page)` for a `200`; panics with the body on anything else.
    async fn page(router: axum::Router, uri: &str) -> Value {
        let (status, body) = get(router, uri).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "expected 200 from {uri}, body: {}",
            String::from_utf8_lossy(&body)
        );
        serde_json::from_slice(&body).expect("page is JSON")
    }

    /// The `seq` values a page returned, in order.
    fn seqs(page: &Value) -> Vec<u64> {
        page["events"]
            .as_array()
            .expect("events is an array")
            .iter()
            .map(|e| e["seq"].as_u64().expect("seq is a number"))
            .collect()
    }

    // ─── the route exists at all ────────────────────────────────────────────

    /// Why: the whole slice is this route existing; before it is mounted the
    /// request 404s. This is the red-before-green case.
    #[tokio::test]
    async fn the_route_is_mounted() {
        let (status, _) = get(router_with(Vec::new()), "/api/console/events/stream").await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn empty_bus_returns_an_empty_page() {
        let page = page(router_with(Vec::new()), "/api/console/events/stream").await;
        assert_eq!(seqs(&page), Vec::<u64>::new());
        assert_eq!(page["next_seq"], 0);
        assert_eq!(page["dropped"], 0);
    }

    #[tokio::test]
    async fn bus_absent_is_a_503() {
        // `AppState::new` alone wires no bus — a console that failed to build
        // one must say so rather than answer an empty page that reads as "no
        // events happened".
        let router = build_router(AppState::new(Vec::new()));
        let (status, _) = get(router, "/api/console/events/stream").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    // ─── the cursor and the dropped count ───────────────────────────────────

    #[tokio::test]
    async fn dropped_is_zero_when_the_cursor_is_inside_the_ring() {
        let events = (0..5).map(|_| ping(HarnessSource::Mpm, None)).collect();
        let page = page(
            router_with(events),
            "/api/console/events/stream?since_seq=2",
        )
        .await;

        assert_eq!(seqs(&page), vec![3, 4, 5], "exactly the events after 2");
        assert_eq!(page["dropped"], 0);
        assert_eq!(page["next_seq"], 5);
    }

    #[tokio::test]
    async fn dropped_counts_the_gap_when_the_cursor_predates_retention() {
        // Capacity 3, six ingests: seqs 1-3 evicted, 4-6 retained. A caller at
        // since_seq=1 wanted 2 onward, so events 2 and 3 are gone.
        let events = (0..6).map(|_| ping(HarnessSource::Mpm, None)).collect();
        let page = page(
            router_with_capacity(events, 3),
            "/api/console/events/stream?since_seq=1",
        )
        .await;

        assert_eq!(page["dropped"], 2, "seqs 2 and 3 were evicted");
        assert_eq!(
            seqs(&page),
            vec![4, 5, 6],
            "the retained events still come back — a gap is reported, not substituted"
        );
    }

    #[tokio::test]
    async fn a_cursor_at_the_head_returns_nothing_and_holds_its_place() {
        let events = (0..3).map(|_| ping(HarnessSource::Mpm, None)).collect();
        let page = page(
            router_with(events),
            "/api/console/events/stream?since_seq=3",
        )
        .await;

        assert_eq!(seqs(&page), Vec::<u64>::new());
        assert_eq!(page["next_seq"], 3, "the cursor does not move backwards");
        assert_eq!(page["dropped"], 0);
    }

    #[tokio::test]
    async fn next_seq_advances_past_filtered_out_events() {
        // seq 1 matches; seqs 2 and 3 are examined and rejected.
        let page = page(
            router_with(vec![
                ping(HarnessSource::Mpm, None),
                ping(HarnessSource::Code, None),
                ping(HarnessSource::Code, None),
            ]),
            "/api/console/events/stream?source=mpm",
        )
        .await;

        assert_eq!(seqs(&page), vec![1]);
        assert_eq!(
            page["next_seq"], 3,
            "resuming from 1 would re-scan the rejected events forever"
        );
    }

    #[tokio::test]
    async fn limit_pages_the_response_and_the_cursor_resumes() {
        let events: Vec<_> = (0..5).map(|_| ping(HarnessSource::Mpm, None)).collect();
        let first = page(
            router_with(events.clone()),
            "/api/console/events/stream?limit=2",
        )
        .await;
        assert_eq!(seqs(&first), vec![1, 2]);
        assert_eq!(first["next_seq"], 2);

        let second = page(
            router_with(events),
            "/api/console/events/stream?limit=2&since_seq=2",
        )
        .await;
        assert_eq!(seqs(&second), vec![3, 4]);
    }

    // ─── the four filter axes, alone and combined ───────────────────────────

    /// Seven events covering every axis, ingested in this order (seq 1-7).
    fn mixed_fixture() -> Vec<HarnessEvent> {
        vec![
            ping(HarnessSource::Mpm, Some("s1")),                          // 1
            ping(HarnessSource::Code, Some("s2")),                         // 2
            action(HarnessSource::Mpm, Some("s1"), agent("qa", "a-qa")),   // 3
            action(HarnessSource::Mpm, Some("s2"), agent("qa", "a-qa")),   // 4
            action(HarnessSource::Code, Some("s1"), Actor::Operator),      // 5
            action(HarnessSource::Agents, Some("s1"), Actor::System),      // 6
            action(HarnessSource::Mpm, Some("s1"), agent("eng", "a-eng")), // 7
        ]
    }

    #[tokio::test]
    async fn each_filter_axis_alone_selects_its_events() {
        let by_source = page(
            router_with(mixed_fixture()),
            "/api/console/events/stream?source=mpm",
        )
        .await;
        assert_eq!(seqs(&by_source), vec![1, 3, 4, 7]);

        let by_session = page(
            router_with(mixed_fixture()),
            "/api/console/events/stream?session=s2",
        )
        .await;
        assert_eq!(seqs(&by_session), vec![2, 4]);

        // Every action in the fixture is a `session`-kind action; the two
        // pings are not actions and so carry no kind.
        let by_kind = page(
            router_with(mixed_fixture()),
            "/api/console/events/stream?kind=session",
        )
        .await;
        assert_eq!(seqs(&by_kind), vec![3, 4, 5, 6, 7]);

        let by_actor = page(
            router_with(mixed_fixture()),
            "/api/console/events/stream?actor=qa",
        )
        .await;
        assert_eq!(seqs(&by_actor), vec![3, 4]);
    }

    #[tokio::test]
    async fn filters_are_anded_together() {
        // source=mpm AND session=s1 AND kind=session AND actor=qa → only seq 3.
        let all_four = page(
            router_with(mixed_fixture()),
            "/api/console/events/stream?source=mpm&session=s1&kind=session&actor=qa",
        )
        .await;
        assert_eq!(seqs(&all_four), vec![3]);

        // Two axes: source=mpm AND session=s1 keeps the ping too.
        let two = page(
            router_with(mixed_fixture()),
            "/api/console/events/stream?source=mpm&session=s1",
        )
        .await;
        assert_eq!(seqs(&two), vec![1, 3, 7]);

        // A combination nothing satisfies is an empty page, not everything.
        let none = page(
            router_with(mixed_fixture()),
            "/api/console/events/stream?source=code&actor=qa",
        )
        .await;
        assert_eq!(seqs(&none), Vec::<u64>::new());
    }

    #[tokio::test]
    async fn a_kind_filter_excludes_non_action_payloads() {
        let page = page(
            router_with(vec![ping(HarnessSource::Mpm, None)]),
            "/api/console/events/stream?kind=tool",
        )
        .await;
        assert_eq!(
            seqs(&page),
            Vec::<u64>::new(),
            "a ping has no kind, so it never satisfies a kind constraint"
        );
    }

    #[tokio::test]
    async fn a_session_filter_excludes_events_with_no_session() {
        let page = page(
            router_with(vec![
                ping(HarnessSource::Mpm, None),
                ping(HarnessSource::Mpm, Some("s1")),
            ]),
            "/api/console/events/stream?session=s1",
        )
        .await;
        assert_eq!(seqs(&page), vec![2]);
    }

    #[tokio::test]
    async fn an_actor_filter_matches_name_or_agent_id() {
        let by_name = page(
            router_with(mixed_fixture()),
            "/api/console/events/stream?actor=eng",
        )
        .await;
        assert_eq!(seqs(&by_name), vec![7]);

        let by_id = page(
            router_with(mixed_fixture()),
            "/api/console/events/stream?actor=a-eng",
        )
        .await;
        assert_eq!(seqs(&by_id), vec![7]);
    }

    #[tokio::test]
    async fn an_actor_filter_matches_operator_and_system() {
        let operator = page(
            router_with(mixed_fixture()),
            "/api/console/events/stream?actor=operator",
        )
        .await;
        assert_eq!(seqs(&operator), vec![5]);

        let system = page(
            router_with(mixed_fixture()),
            "/api/console/events/stream?actor=system",
        )
        .await;
        assert_eq!(seqs(&system), vec![6]);
    }

    #[tokio::test]
    async fn an_unknown_actor_is_an_empty_page_not_an_error() {
        // An agent name is not a closed set, so an unrecognised one is a
        // legitimate "nothing matched" — unlike `source` and `kind`.
        let (status, body) = get(
            router_with(mixed_fixture()),
            "/api/console/events/stream?actor=nobody",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let page: Value = serde_json::from_slice(&body).expect("page is JSON");
        assert_eq!(seqs(&page), Vec::<u64>::new());
    }

    #[tokio::test]
    async fn filters_and_the_cursor_compose() {
        let page = page(
            router_with(mixed_fixture()),
            "/api/console/events/stream?source=mpm&since_seq=3",
        )
        .await;
        assert_eq!(seqs(&page), vec![4, 7], "the cursor bounds the filtered set");
    }

    // ─── a bad filter is a 400, never a wider result ────────────────────────

    async fn assert_400(uri: &str) {
        let (status, body) = get(router_with(mixed_fixture()), uri).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{uri} must be rejected, got body: {}",
            String::from_utf8_lossy(&body)
        );
    }

    #[tokio::test]
    async fn bad_source_is_a_400() {
        assert_400("/api/console/events/stream?source=mpm2").await;
    }

    #[tokio::test]
    async fn bad_kind_is_a_400() {
        assert_400("/api/console/events/stream?kind=toolz").await;
    }

    #[tokio::test]
    async fn bad_since_seq_is_a_400() {
        assert_400("/api/console/events/stream?since_seq=abc").await;
        assert_400("/api/console/events/stream?since_seq=-1").await;
    }

    #[tokio::test]
    async fn zero_limit_is_a_400() {
        assert_400("/api/console/events/stream?limit=0").await;
    }

    #[tokio::test]
    async fn over_max_limit_is_a_400() {
        assert_400(&format!(
            "/api/console/events/stream?limit={}",
            MAX_LIMIT + 1
        ))
        .await;
    }

    #[tokio::test]
    async fn unknown_query_parameter_is_a_400() {
        // The fail-open this route must not have: `sess` is not `session`, and
        // ignoring it would answer with every event instead of one session's.
        assert_400("/api/console/events/stream?sess=s1").await;
    }

    #[test]
    fn parse_defaults_are_unconstrained_and_unlimited() {
        let query = EventsQuery {
            since_seq: None,
            source: None,
            session: None,
            kind: None,
            actor: None,
            limit: None,
        };
        let request = EventsRequest::parse(&query).expect("an empty query is valid");
        assert_eq!(request.since_seq, 0);
        assert_eq!(request.limit, MAX_LIMIT);
        assert_eq!(request.filter, EventFilter::default());
    }
}
