//! The reconnecting `events.subscribe` client (#9076).
//!
//! Why: a subscription to the bus ends in more ways than a request does — the
//! daemon restarts, refuses a resume point, reports a gap, sends a frame the
//! client cannot read, or stays quiet for hours. Each must leave a visible
//! state and lead to a bounded retry, never a stream that is silently empty
//! (DOC-73 §8.5). And a quiet bus must not read as a dead one.
//! What: [`run`] loops over attempts. Each attempt opens one subscription
//! under [`OPEN_TIMEOUT`] and reads it with a day-long per-frame budget, so a
//! quiet stream is not cut; while it waits, every [`LIVENESS_INTERVAL`] it
//! checks that the socket is still served. A dead daemon closes the
//! connection, which is a hang-up. Every relayed event moves the cursor
//! (`seq` + `EventId`), and a later attempt resumes from it with
//! `since_seq` and `since_id`. When the bus refuses a resume
//! (`invalid_params`), the next attempt starts at once from `since_seq: 0`,
//! and events already relayed are dropped by `EventId`. An event this build
//! cannot decode is stepped over as a one-event gap (`super::decode`). Any
//! other end — a failed dial, a hang-up, a server error, a malformed frame, a
//! clean end, a socket that is no longer served — is recorded on the relay as
//! a `FailureKind` (the text goes to the log) and retried after a doubling,
//! capped backoff.
//! Limits: before the first event there is no cursor, so a `Live` client that
//! reconnects resumes live. Dedupe remembers [`DEDUPE_WINDOW`] ids; after a
//! resync, an event older than that is relayed again. A daemon that hangs
//! without closing the connection looks quiet, not dead.
//! Test: `super::tests` (one test per arm), `tests/events_uds_live.rs`.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use trusty_common::control_bus::{
    EventId, SubscribeFilter, SubscribeItem, SubscribeParams, SubscribeRequest,
};
use trusty_common::uds::server::CODE_INVALID_PARAMS;
use trusty_common::uds::{
    FramedStream, SocketVerdict, UdsRpcError, probe_socket_verdict, send_framed_stream_request,
};

use super::decode::{Decoded, decode};
use super::relay::{EventsRelay, FailureKind};
use super::token::ResumeToken;

/// How long opening a subscription may take: the dial and the request write.
///
/// Why separate from [`FRAME_TIMEOUT`]: the shared stream helper applies its
/// one timeout to the open and to every frame, so the open is bounded here,
/// around the call, as `search_uds::STREAM_OPEN_TIMEOUT` does.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// The per-frame budget of an open subscription: a day, in effect none.
///
/// Why: #9075 review — `FramedStream` applies its timeout per frame, and a
/// bus with nothing to say says nothing. A short budget would cut a healthy
/// quiet subscription. Liveness is [`LIVENESS_INTERVAL`]'s job instead. Not
/// `Duration::MAX`, which overflows tokio's timer.
/// Test: `super::tests::a_quiet_subscription_is_not_dropped`.
pub const FRAME_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// How often a quiet subscription checks that its socket is still served.
///
/// Why: a daemon that exits closes the connection, which the read sees. A
/// socket file that is removed or replaced under a live connection does not
/// close it; only a fresh connect to the path shows that nothing serves it.
/// Test: `super::tests::a_socket_that_vanishes_under_a_quiet_subscription_is_detected`.
pub const LIVENESS_INTERVAL: Duration = Duration::from_secs(15);

/// The heartbeat interval the client asks the bus for (#10071).
///
/// Why: a daemon that is stopped or wedged keeps its socket and accepts
/// connections from the kernel's backlog, so only a frame proves it answers.
/// Test: `super::tests::a_daemon_that_accepts_and_goes_silent_is_reported_hung`.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

/// How long the client waits for any frame before it reports the bus hung:
/// two and a half heartbeats, so one lost beat plus scheduler jitter is not a
/// hang (#10071). 12.5 s for [`HEARTBEAT_INTERVAL`].
///
/// Test: `super::tests::a_daemon_that_accepts_and_goes_silent_is_reported_hung`.
pub fn hung_bound(heartbeat: Duration) -> Duration {
    heartbeat.saturating_mul(5) / 2
}

/// How long one liveness connect may take before it counts as inconclusive.
const LIVENESS_PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// The first pause after a failed attempt.
pub const BACKOFF_INITIAL: Duration = Duration::from_millis(250);

/// The longest pause between two attempts.
pub const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// How many recently relayed `EventId`s are remembered for deduplication.
///
/// Why this many: a resync replays everything the bus retains, and the ids
/// held must cover what this console relayed before it. 65,536 ids is eight
/// rings of the bus's default 8192 and about 2 MiB.
pub const DEDUPE_WINDOW: usize = 65_536;

/// Where a new client starts reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// Events published from now on.
    Live,
    /// Every event the bus retains, then live.
    Beginning,
    /// Events after this one, then live: a reader resuming from its
    /// `Last-Event-ID`.
    After(ResumeToken),
}

/// What one client dials, reads and how patiently.
///
/// Why: production and tests differ only in their timings; one type keeps
/// the code path the same.
/// What: [`ClientConfig::new`] gives the production values.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// The bus socket.
    pub socket: PathBuf,
    /// Server-side filter.
    pub filters: SubscribeFilter,
    /// Where the first subscription starts.
    pub start: Start,
    /// See [`OPEN_TIMEOUT`].
    pub open_timeout: Duration,
    /// See [`FRAME_TIMEOUT`].
    pub frame_timeout: Duration,
    /// See [`LIVENESS_INTERVAL`].
    pub liveness_interval: Duration,
    /// See [`HEARTBEAT_INTERVAL`] and [`hung_bound`].
    pub heartbeat: Duration,
    /// See [`BACKOFF_INITIAL`].
    pub backoff_initial: Duration,
    /// See [`BACKOFF_MAX`].
    pub backoff_max: Duration,
    /// See [`DEDUPE_WINDOW`].
    pub dedupe_window: usize,
}

impl ClientConfig {
    /// The production configuration for `socket`: every event, live.
    pub fn new(socket: PathBuf) -> Self {
        Self {
            socket,
            filters: SubscribeFilter::default(),
            start: Start::Live,
            open_timeout: OPEN_TIMEOUT,
            frame_timeout: FRAME_TIMEOUT,
            liveness_interval: LIVENESS_INTERVAL,
            heartbeat: HEARTBEAT_INTERVAL,
            backoff_initial: BACKOFF_INITIAL,
            backoff_max: BACKOFF_MAX,
            dedupe_window: DEDUPE_WINDOW,
        }
    }
}

/// Run [`run`] as a task. Abort the handle to stop the client.
pub fn spawn(config: ClientConfig, relay: Arc<EventsRelay>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run(config, relay))
}

/// The pause before attempt `failures + 1`: `initial` doubling, capped at `max`.
///
/// Test: `super::tests::backoff_doubles_and_stops_at_its_ceiling`.
pub(crate) fn backoff(failures: u32, initial: Duration, max: Duration) -> Duration {
    let doublings = failures.saturating_sub(1).min(20);
    initial.saturating_mul(1 << doublings).min(max)
}

/// Subscribe to the bus and relay what it sends, for as long as the task runs.
///
/// Why: the one owner of the subscription's lifecycle; see the module docs.
/// What: never returns. Publishes every state change and item on `relay`.
/// Test: `super::tests`, `tests/events_uds_live.rs`.
pub async fn run(config: ClientConfig, relay: Arc<EventsRelay>) {
    let mut session = Session {
        seen: SeenIds::new(config.dedupe_window),
        config,
        relay,
        cursor: None,
        resync: false,
        failures: 0,
    };
    loop {
        session.attempt().await;
    }
}

/// Why one attempt ended: the category the status shows, and the detail the
/// log keeps (#9076 review: no raw text on the route).
struct Failure {
    kind: FailureKind,
    detail: String,
}

impl Failure {
    fn new(kind: FailureKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    /// The category of an error the stream reported.
    fn of_stream(e: UdsRpcError) -> Self {
        let kind = match &e {
            UdsRpcError::Stream { error, .. } if error.code == CODE_INVALID_PARAMS => {
                FailureKind::Refused
            }
            UdsRpcError::Stream { .. } => FailureKind::ServerError,
            UdsRpcError::Decode { .. }
            | UdsRpcError::FrameTooLarge { .. }
            | UdsRpcError::NotAStream { .. } => FailureKind::Decode,
            UdsRpcError::Timeout { .. } => FailureKind::Timeout,
            _ => FailureKind::HungUp,
        };
        Self::new(kind, e.to_string())
    }
}

/// The client's state across attempts.
struct Session {
    config: ClientConfig,
    relay: Arc<EventsRelay>,
    /// The newest event relayed, recognised as a duplicate, or passed over as
    /// undecodable.
    cursor: Option<ResumeToken>,
    /// The next attempt replays from 0: the bus refused the cursor.
    resync: bool,
    seen: SeenIds,
    /// Failed attempts since the last item.
    failures: u32,
}

impl Session {
    /// The parameters of the next attempt.
    fn params(&self) -> SubscribeParams {
        let (since_seq, since_id) = if self.resync {
            (Some(0), None)
        } else {
            match (self.cursor, self.config.start) {
                (Some(at), _) | (None, Start::After(at)) => (Some(at.seq), Some(at.id)),
                (None, Start::Beginning) => (Some(0), None),
                (None, Start::Live) => (None, None),
            }
        };
        SubscribeParams {
            since_seq,
            since_id,
            filters: self.config.filters.clone(),
            heartbeat_ms: None,
        }
    }

    /// One subscription, then the decision about the next.
    async fn attempt(&mut self) {
        let params = self.params();
        let resuming = params.since_seq.is_some_and(|seq| seq > 0);
        let failure = self.subscribe(params).await;
        // #9076: the bus no longer holds the resume point (a restart
        // reassigned the seq, or retention dropped it). Replay what it
        // retains and drop what was already relayed; no backoff, because
        // `since_seq: 0` has no resume point to refuse. A refusal of a
        // request that was not a resume backs off like any other failure.
        if failure.kind == FailureKind::Refused && resuming {
            tracing::warn!(detail = %failure.detail, "event bus refused the resume point; resyncing from 0");
            self.cursor = None;
            self.resync = true;
            self.relay.note_resync();
            return;
        }
        self.failures = self.failures.saturating_add(1);
        let delay = backoff(
            self.failures,
            self.config.backoff_initial,
            self.config.backoff_max,
        );
        if self.failures == 1 {
            tracing::warn!(kind = ?failure.kind, detail = %failure.detail, retry_in = ?delay, "event bus subscription lost");
        } else {
            tracing::debug!(kind = ?failure.kind, detail = %failure.detail, retry_in = ?delay, failures = self.failures, "event bus still unreachable");
        }
        self.relay.note_failure(failure.kind, delay, self.failures);
        tokio::time::sleep(delay).await;
    }

    /// Open one subscription and read it until it ends.
    async fn subscribe(&mut self, params: SubscribeParams) -> Failure {
        let socket = self.config.socket.clone();
        let request = SubscribeRequest::new(1, params);
        let opened = tokio::time::timeout(
            self.config.open_timeout,
            send_framed_stream_request::<_, Value>(&socket, &request, self.config.frame_timeout),
        )
        .await;
        let mut stream = match opened {
            Ok(Ok(stream)) => stream,
            // Resolving the path creates its directory 0755, which the dial
            // then reports as a permission fault; name the absent socket first.
            Ok(Err(e)) if !socket.exists() => {
                return Failure::new(FailureKind::NotRunning, e.to_string());
            }
            Ok(Err(e @ UdsRpcError::Timeout { .. })) => {
                return Failure::new(FailureKind::Timeout, e.to_string());
            }
            Ok(Err(e)) => return Failure::new(FailureKind::DialFailed, e.to_string()),
            Err(_) => {
                return Failure::new(
                    FailureKind::Timeout,
                    format!(
                        "opening {} took longer than {:?}",
                        socket.display(),
                        self.config.open_timeout
                    ),
                );
            }
        };
        self.relay.note_connected();
        loop {
            let next = match self.next_while_live(&mut stream).await {
                Ok(next) => next,
                Err(failure) => return failure,
            };
            match next {
                Some(Ok(value)) => {
                    if let Err(detail) = self.accept(value) {
                        return Failure::new(FailureKind::Decode, detail);
                    }
                }
                Some(Err(e)) => return Failure::of_stream(e),
                None => return Failure::new(FailureKind::Ended, "the bus ended the subscription"),
            }
        }
    }

    /// The next frame, or why the connection is no longer worth reading.
    ///
    /// The read is never cancelled while the socket is served: `FramedStream`
    /// drops a partly read line when its future is dropped, so the liveness
    /// check runs beside the pending read, not in place of it.
    async fn next_while_live(
        &mut self,
        stream: &mut FramedStream<Value>,
    ) -> Result<Option<Result<Value, UdsRpcError>>, Failure> {
        let read = stream.next_frame();
        tokio::pin!(read);
        let every = self.config.liveness_interval;
        let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
        loop {
            tokio::select! {
                next = &mut read => return Ok(next),
                _ = tick.tick() => {
                    let verdict = probe_socket_verdict(&self.config.socket, LIVENESS_PROBE_TIMEOUT).await;
                    if verdict == SocketVerdict::NotServing {
                        return Err(Failure::new(
                            FailureKind::SocketLost,
                            format!(
                                "{} is no longer served; this subscription is attached to a \
                                 bus that producers cannot reach",
                                self.config.socket.display()
                            ),
                        ));
                    }
                    // A connection that outlives an interval is healthy:
                    // the next failure starts the backoff over.
                    self.failures = 0;
                    self.relay.note_progress();
                }
            }
        }
    }

    /// Relay one item and move the cursor. `Err` when the item has no seq to
    /// move past, so the subscription must be reopened.
    fn accept(&mut self, value: Value) -> Result<(), String> {
        match decode(value) {
            Decoded::Item(item) => {
                self.failures = 0;
                match item {
                    SubscribeItem::Event { event, persisted } => {
                        let token = ResumeToken::of(&event);
                        if self.advance(token) {
                            self.relay.publish_event(token, Arc::from(event), persisted);
                        }
                    }
                    SubscribeItem::Gap { skipped } => {
                        self.relay.publish_gap(skipped, self.cursor);
                    }
                    // `decode` reports a kind this build lacks as `Unknown`.
                    _ => self.relay.note_unknown_item(),
                }
                Ok(())
            }
            // #9076 review: an event from a newer daemon (DOC-73 §3.3). Its
            // seq and id still read, so it is passed over as a one-event gap
            // and the cursor moves past it: a replay cannot stall on it.
            Decoded::UndecodableEvent { token, error } => {
                self.failures = 0;
                tracing::warn!(%token, %error, "event bus sent an event this console cannot decode; relayed as a gap");
                let after = self.cursor;
                if self.advance(token) {
                    self.relay.publish_undecodable_event(after);
                }
                Ok(())
            }
            Decoded::Unknown(kind) => {
                self.failures = 0;
                tracing::warn!(%kind, "event bus sent an item kind this console does not know");
                self.relay.note_unknown_item();
                Ok(())
            }
            Decoded::Malformed(error) => Err(format!("malformed item from the bus: {error}")),
        }
    }

    /// Move the cursor to `token`. `true` when its event is new; a duplicate
    /// is counted on the relay.
    fn advance(&mut self, token: ResumeToken) -> bool {
        self.cursor = Some(token);
        self.resync = false;
        let new = self.seen.insert(token.id);
        if !new {
            self.relay.note_duplicate(token);
        }
        new
    }
}

/// The last `capacity` relayed ids, oldest evicted first.
struct SeenIds {
    capacity: usize,
    order: VecDeque<EventId>,
    set: HashSet<EventId>,
}

impl SeenIds {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            order: VecDeque::new(),
            set: HashSet::new(),
        }
    }

    /// `true` when `id` is new, and remember it.
    fn insert(&mut self, id: EventId) -> bool {
        if !self.set.insert(id) {
            return false;
        }
        self.order.push_back(id);
        if self.order.len() > self.capacity
            && let Some(oldest) = self.order.pop_front()
        {
            self.set.remove(&oldest);
        }
        true
    }
}
