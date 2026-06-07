//! Process-global event bus, the `HarnessEvent` envelope, and the
//! lagged-receiver helper for the unified harness event stream (ADR-0005).
//!
//! Why: Threading a `broadcast::Sender<HarnessEvent>` through every code path
//!      that might emit telemetry (workflow engine, agent runners, hooks,
//!      ctrl) would touch hundreds of function signatures. A `OnceLock` mirrors
//!      how `tracing` solves the same problem — emit-from-anywhere with zero
//!      overhead when no one is listening. The envelope wraps the per-domain
//!      payload with cross-cutting metadata (source, session, monotonic seq,
//!      timestamp) so any subscriber can order, correlate, and route events
//!      without inspecting the payload.
//! What: Defines `HarnessPayload` (the domain-tagged inner union),
//!       `HarnessEvent` (the envelope), the process-global `EVENT_BUS`, a
//!       monotonic `SEQ` counter, `bus`/`subscribe`/`publish`/`emit` helpers,
//!       the `__HARNESS_EVENT__` stderr relay prefix, and a `Lag` notice with
//!       `recv_with_lag` so a lagged subscriber is told how many events it
//!       skipped and can resume instead of tearing down its stream.
//! Test: `super::tests` covers serde round-trips of each payload arm, seq
//!       monotonicity, lagged handling against a constructible test bus, and
//!       the emit-line formatting helper.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;

use super::lifecycle::{HarnessSource, LifecycleEvent};

/// Stderr line prefix used to relay events from a child process to its parent.
///
/// Why: A harness may spawn workflow / agent work in a child process whose
///      events must still reach the parent's bus. Reusing stderr as the IPC
///      channel (the parent already reads child stderr) avoids inventing a new
///      transport. A stable, unambiguous prefix lets the parent detect relay
///      lines and re-publish them rather than mirroring them to the terminal.
/// What: Match by exact byte prefix; the payload after the space is one JSON
///       object decodable as `HarnessEvent`.
/// Test: `super::tests::event_line_prefix_is_stable` and
///       `super::tests::emit_line_has_prefix_and_parses`.
pub const EVENT_LINE_PREFIX: &str = "__HARNESS_EVENT__ ";

/// Broadcast channel capacity for the global bus.
///
/// Why: Event volume is high (every PM thought, every agent line, every tool
///      call). 1024 keeps memory bounded (~256 KB at 256 bytes/event) while
///      tolerating slow subscribers without dropping recent events under
///      typical load. A subscriber that still falls behind receives a `Lag`
///      notice rather than silently losing the stream.
/// What: Used both for the process-global bus and as the default for the
///       test-only constructible bus.
pub const CHANNEL_CAPACITY: usize = 1024;

/// Domain-tagged inner union carried inside a `HarnessEvent`.
///
/// Why: The "adapt, don't fold" decision (ADR-0005): rather than flattening
///      lifecycle, hook, and keepalive events into one giant enum, we tag by
///      *domain* so each harness can grow its own payload taxonomy
///      independently. Hooks in particular are open-ended (arbitrary
///      tool/event names + JSON data), so they get an untyped `Value` arm
///      instead of being modelled variant-by-variant.
/// What: `serde(tag = "domain", content = "event")` produces
///       `{"domain":"lifecycle","event":{...}}`, `{"domain":"hook","event":
///       {"kind":...,"data":...}}`, or `{"domain":"ping"}`. `Ping` is the
///       transport keepalive (promoted out of the lifecycle enum).
/// Test: `super::tests::payload_*_round_trips` assert the tag shape for each
///       arm.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "domain", content = "event", rename_all = "snake_case")]
pub enum HarnessPayload {
    /// A structured PM-lifecycle event (session/agent/tool/phase/LLM/...).
    Lifecycle(LifecycleEvent),
    /// An open-ended hook event: `kind` names the hook, `data` is its payload.
    Hook { kind: String, data: Value },
    /// Transport keepalive so long-lived SSE connections don't time out.
    Ping,
}

impl HarnessPayload {
    /// The domain string for this payload (`"lifecycle"`, `"hook"`, `"ping"`).
    ///
    /// Why: `Filter` matches on the domain without serialising the whole
    ///      payload; keeping the mapping here is the single source of truth.
    /// What: Returns the same string serde uses for the `domain` tag.
    /// Test: `super::tests::payload_domain_matches_serde_tag`.
    pub fn domain(&self) -> &'static str {
        match self {
            HarnessPayload::Lifecycle(_) => "lifecycle",
            HarnessPayload::Hook { .. } => "hook",
            HarnessPayload::Ping => "ping",
        }
    }
}

/// Cross-harness event envelope: metadata + domain-tagged payload.
///
/// Why: Subscribers need to order (`seq`), time-stamp (`at`), attribute
///      (`source`), and correlate (`session`) events uniformly, regardless of
///      which harness produced them or which domain the payload belongs to.
///      Carrying that metadata in the envelope keeps the payload taxonomies
///      free of repeated bookkeeping fields.
/// What: `source` is the originating harness; `session` is the optional task
///       correlation key (omitted from JSON when `None`); `seq` is a
///       process-monotonic counter; `at` is the emit-time UTC timestamp;
///       `payload` is the domain-tagged union. All fields derive serde.
/// Test: `super::tests::harness_event_round_trips` and the per-arm payload
///       tests build full envelopes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HarnessEvent {
    /// Which harness produced this event.
    pub source: HarnessSource,
    /// Optional task/session correlation key. Omitted from JSON when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Process-monotonic sequence number assigned at publish time.
    pub seq: u64,
    /// Emit-time UTC timestamp.
    pub at: DateTime<Utc>,
    /// Domain-tagged payload.
    pub payload: HarnessPayload,
}

/// Notice that a subscriber fell behind and skipped `skipped` events.
///
/// Why: `broadcast::Receiver` returns `RecvError::Lagged(n)` and then resumes
///      from the oldest still-buffered event. Silently swallowing that loses
///      the "you missed N events" signal a UI needs to show a gap; tearing the
///      stream down on lag is worse. Surfacing it as a typed value lets
///      subscribers render a gap marker and keep going.
/// What: A thin newtype-ish struct wrapping the skipped count, returned as the
///       `Err` arm of `recv_with_lag`.
/// Test: `super::tests::lagged_receiver_yields_lag_then_resumes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lag {
    /// Number of events the receiver skipped past.
    pub skipped: u64,
}

/// Process-global event bus, initialised on first access.
///
/// Why: Emit-from-anywhere telemetry without threading a sender through every
///      signature. First access wins; all later accesses share the same sender.
/// What: `Sender::send` is non-blocking; with no subscribers it returns an
///       error which `publish` silently drops (no listeners is the baseline).
static EVENT_BUS: OnceLock<broadcast::Sender<HarnessEvent>> = OnceLock::new();

/// Process-monotonic sequence source for envelope `seq` assignment.
///
/// Why: Subscribers need a total order over events even when timestamps
///      collide at sub-millisecond resolution. A single atomic counter gives a
///      cheap, lock-free monotonic sequence shared across all emit sites.
/// What: Incremented once per `publish`/`emit`; wraps after 2^64 events
///       (practically never).
static SEQ: AtomicU64 = AtomicU64::new(0);

/// Get (or initialise) the process-global event bus sender.
///
/// Why: Called from any code path that wants to emit. Cheap on the hot path
///      (single atomic load once initialised).
/// What: Returns a clone of the global sender. Receivers are created per
///       subscriber via `subscribe()`.
/// Test: `super::tests::bus_is_singleton`.
pub fn bus() -> broadcast::Sender<HarnessEvent> {
    EVENT_BUS
        .get_or_init(|| broadcast::channel(CHANNEL_CAPACITY).0)
        .clone()
}

/// Subscribe to all future events on the global bus.
///
/// Why: Subscribers (relays, aggregators, future SSE handlers) each need their
///      own `Receiver`; `broadcast::Receiver::recv` takes `&mut self`.
/// What: Returns a fresh `Receiver` that begins receiving events emitted after
///       the call returns.
/// Test: `super::tests::publish_round_trips_through_subscribe`.
pub fn subscribe() -> broadcast::Receiver<HarnessEvent> {
    bus().subscribe()
}

/// Allocate the next monotonic sequence number.
///
/// Why: Centralising the counter keeps every emit site assigning sequence
///      numbers from the same source.
/// What: Fetch-adds the global `SEQ` with `Relaxed` ordering (only monotonic
///       uniqueness is required, not cross-field synchronisation).
fn next_seq() -> u64 {
    SEQ.fetch_add(1, Ordering::Relaxed)
}

/// Build a fully-populated `HarnessEvent`, assigning `seq` and `at`.
///
/// Why: `publish` and `emit` both need to stamp the envelope identically; one
///      helper avoids drift between the two paths.
/// What: Allocates the next seq, reads `Utc::now()`, and assembles the envelope.
/// Test: Exercised by `publish_round_trips_through_subscribe` (seq/at present).
fn make_event(
    source: HarnessSource,
    session: Option<String>,
    payload: HarnessPayload,
) -> HarnessEvent {
    HarnessEvent {
        source,
        session,
        seq: next_seq(),
        at: Utc::now(),
        payload,
    }
}

/// Publish one event to the global bus (best-effort, never panics).
///
/// Why: Events are telemetry, not control flow. Having no subscribers is the
///      normal baseline, so a send failure is expected and dropped.
/// What: Stamps the envelope (`seq`, `at`) via `make_event`, then sends on the
///       broadcast channel, ignoring `SendError`. Returns the assigned `seq` so
///       callers (and tests) can observe ordering.
/// Test: `super::tests::publish_round_trips_through_subscribe`,
///       `super::tests::seq_is_monotonic`.
pub fn publish(source: HarnessSource, session: Option<String>, payload: HarnessPayload) -> u64 {
    let event = make_event(source, session, payload);
    let seq = event.seq;
    let _ = bus().send(event);
    seq
}

/// Publish an event AND emit it on stderr with the `EVENT_LINE_PREFIX` so a
/// parent process can re-broadcast on its own bus.
///
/// Why: Child-process emit sites should not have to know whether they are
///      running standalone or under a parent relay; doing both in one call
///      keeps call sites uniform.
/// What: Builds the envelope once, publishes it for in-process subscribers,
///       then writes one NDJSON line to stderr prefixed with `EVENT_LINE_PREFIX`.
///       Returns the assigned `seq`.
/// Test: `super::tests::emit_line_has_prefix_and_parses` covers the line
///       formatting via the shared `format_event_line` helper.
pub fn emit(source: HarnessSource, session: Option<String>, payload: HarnessPayload) -> u64 {
    let event = make_event(source, session, payload);
    let seq = event.seq;
    let line = format_event_line(&event);
    let _ = bus().send(event);
    if let Some(line) = line {
        eprintln!("{line}");
    }
    seq
}

/// Format a single stderr relay line for `event`, or `None` if it cannot be
/// serialised.
///
/// Why: Splitting the formatting out of `emit` makes the wire shape unit-
///      testable without capturing real stderr.
/// What: Returns `EVENT_LINE_PREFIX` concatenated with the compact JSON of the
///       envelope; `None` only on the (practically impossible) serde failure.
/// Test: `super::tests::emit_line_has_prefix_and_parses`.
pub fn format_event_line(event: &HarnessEvent) -> Option<String> {
    serde_json::to_string(event)
        .ok()
        .map(|json| format!("{EVENT_LINE_PREFIX}{json}"))
}

/// Receive the next event, translating broadcast lag into a typed `Lag`.
///
/// Why: Raw `broadcast::Receiver::recv` returns `RecvError::Lagged(n)` and then
///      `RecvError::Closed`; a subscriber that wants to keep going needs the
///      lag count surfaced as data, not an opaque error it must re-interpret at
///      every call site. This helper does that translation once.
/// What: On a successful receive returns `Ok(Ok(event))`; on lag returns
///       `Ok(Err(Lag { skipped }))` (the stream is still alive — call again to
///       resume); on a closed channel returns `Err(())`.
/// Test: `super::tests::lagged_receiver_yields_lag_then_resumes`.
pub async fn recv_with_lag(
    rx: &mut broadcast::Receiver<HarnessEvent>,
) -> Result<Result<HarnessEvent, Lag>, ()> {
    match rx.recv().await {
        Ok(event) => Ok(Ok(event)),
        Err(broadcast::error::RecvError::Lagged(n)) => Ok(Err(Lag { skipped: n })),
        Err(broadcast::error::RecvError::Closed) => Err(()),
    }
}
