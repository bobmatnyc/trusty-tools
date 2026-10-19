//! What the console's bus client has seen: its connection state, and the
//! events it relays in-process.
//!
//! Why: DOC-73 §8.5 requires "cannot see" to render differently from
//! "nothing is happening". A relay that only forwarded events would look the
//! same healthy-and-empty as dead, so the client's state is published beside
//! the events, and served on a route (#9076).
//! What: [`EventsRelay`] holds a `watch` of [`BusStatus`] and a `broadcast`
//! of [`RelayItem`]s. The client in `super::client` writes both; the status
//! route and, later, the event routes (#6850, #6851) read them.
//! [`status_handler`] serves the status as JSON, `200` when connected and
//! `503` otherwise.
//! Test: `super::tests` (status transitions per error arm),
//! `crate::server::tests::events_status_route_reports_a_bus_that_was_never_reached`.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Serialize;
use tokio::sync::{broadcast, watch};
use trusty_common::control_bus::HarnessEvent;

use super::token::ResumeToken;
use crate::server::AppState;

/// Relay items buffered for an in-process reader before it lags.
pub const RELAY_BUFFER: usize = 1024;

/// One thing the bus told the console, as relayed to in-process readers.
///
/// Why: a gap must reach the page as a gap (DOC-73 §4.3), so it is an item
/// of its own rather than a counter a reader might not look at.
/// What: `Event` carries the event, the token an SSE `id:` line takes, and
/// whether the bus read it back from its log. `Gap` says `skipped` events
/// were lost after `after`, the last event this client holds.
/// Test: `super::tests::a_gap_is_relayed_not_swallowed`.
#[derive(Debug, Clone)]
pub enum RelayItem {
    /// One event, replayed or live.
    Event {
        /// `<seq>:<uuid>` of this event: the SSE `id:` for it.
        token: ResumeToken,
        /// The event.
        event: Arc<HarnessEvent>,
        /// `true` when the bus replayed it from its log.
        persisted: bool,
    },
    /// The bus could not deliver `skipped` events.
    Gap {
        /// How many events were lost.
        skipped: u64,
        /// The last event held before the loss, if any.
        after: Option<ResumeToken>,
    },
}

impl RelayItem {
    /// The SSE `id:` for this item: the event's token, or `None` for a gap,
    /// which must not move a reader's `Last-Event-ID`.
    pub fn sse_id(&self) -> Option<String> {
        match self {
            Self::Event { token, .. } => Some(token.to_string()),
            Self::Gap { .. } => None,
        }
    }
}

/// The client's connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BusState {
    /// No attempt has finished yet.
    Starting,
    /// A subscription is open.
    Connected,
    /// The last attempt failed or was refused; another follows.
    Reconnecting,
    /// The socket path cannot be resolved; no attempt is made.
    Unavailable,
}

/// Why an attempt ended, as the status route names it.
///
/// Why a category and not the error text: the route is served on every TCP
/// listener, tailnet peers included, and the raw text carries absolute paths
/// and daemon messages (#9076 review). The text goes to the log.
/// What: one variant per way an attempt ends; serialised `snake_case`.
/// Test: `super::tests::a_missing_socket_is_reported_and_retried_with_backoff`,
/// `super::tests::a_malformed_frame_is_reported_and_the_client_reconnects`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FailureKind {
    /// No socket file: trusty-events is not running.
    NotRunning,
    /// The socket exists but the dial failed (permissions, refused connect).
    DialFailed,
    /// Opening or reading took longer than its budget.
    Timeout,
    /// The bus closed the connection mid-stream.
    HungUp,
    /// The bus refused the resume point.
    Refused,
    /// The bus ended the stream with an error other than a refusal.
    ServerError,
    /// A frame that is not a stream frame, or an item with no seq to skip.
    Decode,
    /// The bus ended the stream cleanly.
    Ended,
    /// The socket path stopped being served under an open subscription.
    SocketLost,
    /// The socket path could not be resolved.
    Unavailable,
}

/// A snapshot of the client, as the status route serves it.
///
/// Why: each failure arm must leave a visible trace, so the reason, the
/// retry delay and the counters are part of the answer, not only the log.
/// What: `last_error` is the most recent failure's category and is kept
/// after recovery; `retry_in_ms` is set only while waiting to retry.
/// `last_event` is the token of the newest event held. The counters are
/// totals since start. No path is served (#9076 review).
/// Test: `super::tests::a_missing_socket_is_reported_and_retried_with_backoff`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BusStatus {
    /// See [`BusState`].
    pub state: BusState,
    /// When `state` last changed, in Unix milliseconds.
    pub since_unix_ms: u64,
    /// The most recent failure.
    pub last_error: Option<FailureKind>,
    /// How long until the next attempt, while waiting.
    pub retry_in_ms: Option<u64>,
    /// Failed attempts since the last delivered item.
    pub failures: u32,
    /// Token of the newest event held.
    pub last_event: Option<String>,
    /// Events relayed.
    pub delivered: u64,
    /// Events dropped as already relayed (after a resync from 0).
    pub duplicates: u64,
    /// Gap items received.
    pub gaps: u64,
    /// Events those gaps skipped, in total.
    pub skipped: u64,
    /// Resume points the bus refused, each followed by a resync from 0.
    pub resyncs: u64,
    /// Items this build could not decode and passed over. An undecodable
    /// event is also relayed as a one-event gap.
    pub undecodable: u64,
}

impl BusStatus {
    fn starting() -> Self {
        Self {
            state: BusState::Starting,
            since_unix_ms: now_ms(),
            last_error: None,
            retry_in_ms: None,
            failures: 0,
            last_event: None,
            delivered: 0,
            duplicates: 0,
            gaps: 0,
            skipped: 0,
            resyncs: 0,
            undecodable: 0,
        }
    }

    fn enter(&mut self, state: BusState) {
        if self.state != state {
            self.state = state;
            self.since_unix_ms = now_ms();
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The client's published state and its in-process event fan-out.
///
/// Why: one owner for "what the bus said", shared by the client task that
/// writes it and every route that reads it.
/// What: see the module docs. Readers call [`Self::status`],
/// [`Self::watch`] or [`Self::subscribe`]; the `note_*` and `publish_*`
/// methods are the client's.
/// Test: `super::tests`.
#[derive(Debug)]
pub struct EventsRelay {
    status: watch::Sender<BusStatus>,
    items: broadcast::Sender<RelayItem>,
}

impl Default for EventsRelay {
    fn default() -> Self {
        Self::new()
    }
}

impl EventsRelay {
    /// A relay in [`BusState::Starting`] with no readers.
    pub fn new() -> Self {
        let (status, _) = watch::channel(BusStatus::starting());
        let (items, _) = broadcast::channel(RELAY_BUFFER);
        Self { status, items }
    }

    /// The current status.
    pub fn status(&self) -> BusStatus {
        self.status.borrow().clone()
    }

    /// A receiver that sees every status change.
    pub fn watch(&self) -> watch::Receiver<BusStatus> {
        self.status.subscribe()
    }

    /// A receiver for every item relayed from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<RelayItem> {
        self.items.subscribe()
    }

    pub(crate) fn note_unavailable(&self) {
        self.status.send_modify(|s| {
            s.enter(BusState::Unavailable);
            s.last_error = Some(FailureKind::Unavailable);
        });
    }

    pub(crate) fn note_connected(&self) {
        self.status.send_modify(|s| {
            s.enter(BusState::Connected);
            s.retry_in_ms = None;
        });
    }

    pub(crate) fn note_failure(&self, kind: FailureKind, retry_in: Duration, failures: u32) {
        self.status.send_modify(|s| {
            s.enter(BusState::Reconnecting);
            s.last_error = Some(kind);
            s.retry_in_ms = Some(u64::try_from(retry_in.as_millis()).unwrap_or(u64::MAX));
            s.failures = failures;
        });
    }

    pub(crate) fn note_resync(&self) {
        self.status.send_modify(|s| {
            s.enter(BusState::Reconnecting);
            s.last_error = Some(FailureKind::Refused);
            s.retry_in_ms = Some(0);
            s.resyncs += 1;
        });
    }

    pub(crate) fn note_progress(&self) {
        self.status.send_if_modified(|s| {
            let changed = s.failures != 0;
            s.failures = 0;
            changed
        });
    }

    pub(crate) fn publish_event(
        &self,
        token: ResumeToken,
        event: Arc<HarnessEvent>,
        persisted: bool,
    ) {
        self.status.send_modify(|s| {
            s.delivered += 1;
            s.failures = 0;
            s.last_event = Some(token.to_string());
        });
        // No reader is not an error: the status above still records the event.
        let _ = self.items.send(RelayItem::Event {
            token,
            event,
            persisted,
        });
    }

    pub(crate) fn note_duplicate(&self, token: ResumeToken) {
        self.status.send_modify(|s| {
            s.duplicates += 1;
            s.failures = 0;
            s.last_event = Some(token.to_string());
        });
    }

    /// #9076: an event this build cannot decode — a one-event gap after
    /// `after`, counted as undecodable too.
    pub(crate) fn publish_undecodable_event(&self, after: Option<ResumeToken>) {
        self.status.send_modify(|s| s.undecodable += 1);
        self.publish_gap(1, after);
    }

    /// #9076: an item of a kind this build does not know; no event is lost.
    pub(crate) fn note_unknown_item(&self) {
        self.status.send_modify(|s| {
            s.undecodable += 1;
            s.failures = 0;
        });
    }

    pub(crate) fn publish_gap(&self, skipped: u64, after: Option<ResumeToken>) {
        self.status.send_modify(|s| {
            s.gaps += 1;
            s.skipped += skipped;
            s.failures = 0;
        });
        let _ = self.items.send(RelayItem::Gap { skipped, after });
    }
}

/// `GET /api/console/events/status`: the bus client's state.
///
/// Why: DOC-73 §8.5 — a page must tell "no events" from "cannot see the bus",
/// and condition 4 of ruling 2a keeps that answer off `/health`, which stays
/// `ok` while the bus is down.
/// What: [`BusStatus`] as JSON; `200` when connected, `503` otherwise.
/// Test: `crate::server::tests::events_status_route_reports_a_bus_that_was_never_reached`.
pub async fn status_handler(State(state): State<AppState>) -> impl IntoResponse {
    let status = state.events().status();
    let code = if status.state == BusState::Connected {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, axum::Json(status))
}
