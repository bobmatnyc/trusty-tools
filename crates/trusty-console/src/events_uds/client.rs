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
//! and events already relayed are dropped by `EventId`. Any other end — a
//! failed dial, a hang-up, a server error, a malformed frame, a clean end, a
//! socket that is no longer served — is recorded on the relay and retried
//! after a doubling, capped backoff.
//! Limits: before the first event there is no cursor, so a `Live` client that
//! reconnects resumes live. Dedupe remembers [`DEDUPE_WINDOW`] ids; after a
//! resync, an event older than that is relayed again. A daemon that hangs
//! without closing the connection looks quiet, not dead.
//! Test: `super::tests` (one test per arm), `tests/events_uds_live.rs`.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use trusty_common::control_bus::{
    EventId, SubscribeFilter, SubscribeItem, SubscribeParams, SubscribeRequest,
};
use trusty_common::uds::server::CODE_INVALID_PARAMS;
use trusty_common::uds::{
    FramedStream, SocketVerdict, UdsRpcError, probe_socket_verdict, send_framed_stream_request,
};

use super::relay::EventsRelay;
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
    relay.note_target(&config.socket);
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

/// Why one attempt ended.
enum Ended {
    /// The bus refused the request's parameters.
    Refused(String),
    /// Anything else.
    Failed(String),
}

/// The client's state across attempts.
struct Session {
    config: ClientConfig,
    relay: Arc<EventsRelay>,
    /// The newest event relayed or recognised as a duplicate.
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
        }
    }

    /// One subscription, then the decision about the next.
    async fn attempt(&mut self) {
        let params = self.params();
        let resuming = params.since_seq.is_some_and(|seq| seq > 0);
        match self.subscribe(params).await {
            // #9076: the bus no longer holds the resume point (a restart
            // reassigned the seq, or retention dropped it). Replay what it
            // retains and drop what was already relayed; no backoff, because
            // `since_seq: 0` has no resume point to refuse.
            Ended::Refused(reason) if resuming => {
                tracing::warn!(%reason, "event bus refused the resume point; resyncing from 0");
                self.cursor = None;
                self.resync = true;
                self.relay.note_resync(reason);
            }
            Ended::Refused(reason) | Ended::Failed(reason) => {
                self.failures = self.failures.saturating_add(1);
                let delay = backoff(
                    self.failures,
                    self.config.backoff_initial,
                    self.config.backoff_max,
                );
                if self.failures == 1 {
                    tracing::warn!(%reason, retry_in = ?delay, "event bus subscription lost");
                } else {
                    tracing::debug!(%reason, retry_in = ?delay, failures = self.failures, "event bus still unreachable");
                }
                self.relay.note_failure(reason, delay, self.failures);
                tokio::time::sleep(delay).await;
            }
        }
    }

    /// Open one subscription and read it until it ends.
    async fn subscribe(&mut self, params: SubscribeParams) -> Ended {
        let socket = self.config.socket.clone();
        let request = SubscribeRequest::new(1, params);
        let opened = tokio::time::timeout(
            self.config.open_timeout,
            send_framed_stream_request::<_, SubscribeItem>(
                &socket,
                &request,
                self.config.frame_timeout,
            ),
        )
        .await;
        let mut stream = match opened {
            Ok(Ok(stream)) => stream,
            Ok(Err(e)) => return Ended::Failed(format!("cannot subscribe: {e}")),
            Err(_) => {
                return Ended::Failed(format!(
                    "opening a subscription on {} took longer than {:?}",
                    socket.display(),
                    self.config.open_timeout
                ));
            }
        };
        self.relay.note_connected();
        loop {
            let next = match self.next_while_live(&mut stream).await {
                Ok(next) => next,
                Err(reason) => return Ended::Failed(reason),
            };
            match next {
                Some(Ok(item)) => self.accept(item),
                Some(Err(UdsRpcError::Stream { error, .. }))
                    if error.code == CODE_INVALID_PARAMS =>
                {
                    return Ended::Refused(format!(
                        "the bus refused the subscription: {}",
                        error.message
                    ));
                }
                Some(Err(e @ UdsRpcError::Decode { .. })) => {
                    return Ended::Failed(format!("malformed frame from the bus: {e}"));
                }
                Some(Err(e)) => return Ended::Failed(format!("subscription failed: {e}")),
                None => return Ended::Failed("the bus ended the subscription".to_string()),
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
        stream: &mut FramedStream<SubscribeItem>,
    ) -> Result<Option<Result<SubscribeItem, UdsRpcError>>, String> {
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
                        return Err(format!(
                            "{} is no longer served; this subscription is attached to a bus \
                             that producers cannot reach",
                            self.config.socket.display()
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

    /// Relay one item and move the cursor.
    fn accept(&mut self, item: SubscribeItem) {
        self.failures = 0;
        match item {
            SubscribeItem::Event { event, persisted } => {
                let token = ResumeToken::of(&event);
                self.cursor = Some(token);
                self.resync = false;
                if self.seen.insert(event.id) {
                    self.relay.publish_event(token, Arc::from(event), persisted);
                } else {
                    self.relay.note_duplicate(token);
                }
            }
            SubscribeItem::Gap { skipped } => self.relay.publish_gap(skipped, self.cursor),
            // `SubscribeItem` is non_exhaustive; serde rejects an unknown
            // `type`, so a variant this build lacks cannot be decoded here.
            other => {
                tracing::warn!(item = ?other, "event bus sent an item this console does not relay")
            }
        }
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
