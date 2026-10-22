//! `tm architect poll`'s trusty-events producer for the wakes it types (#9167).
//!
//! Why: the owner asked that every inter-session message reach the event bus,
//! and the poller's wake (`[poll HH:MMZ] ...`) is one. `trusty-architect` is a
//! leaf crate with no bus dependency, so it reports each submitted wake to a
//! [`MessageSink`] this binary supplies; the event follows the same
//! credential-scan, digest and body rules as `tm hook`'s relays, because it is
//! built by the same [`message_event`].
//! What: [`poll_sink`] picks the sink for this process: [`BusMessages`] when
//! the process may reach a bus (the daemon's `bus_allowed` rule) and its
//! socket resolves, [`NoMessages`] in an isolated test environment, and
//! [`NoBus`] — which reports every wake as not recorded — when the socket
//! does not resolve. [`BusMessages::record`] buffers one `poll_wake` event and
//! flushes it within [`FLUSH_BUDGET`]; a failed flush keeps the event in
//! memory for the next wake and returns the reason, which the poller writes
//! to `poll.log`.
//! Test: `architect_messages_tests.rs`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use trusty_architect::MessageSinkError;
use trusty_architect::poller::{MessageSink, NoMessages, PaneMessage, PaneMessageKind};
use trusty_common::control_bus::{
    EventId, HarnessEvent, HarnessPayload, HarnessSource, MessageChannel, PushClient,
    PushFlushOutcome,
};
use trusty_mpm::daemon::action_events::bus_allowed;
use trusty_mpm::daemon::action_events::message::{bodies_from_env, message_event};

/// The longest one wake waits on the bus. A wake comes at most once per poll
/// interval (60 s by default), and it has already been typed.
pub(crate) const FLUSH_BUDGET: Duration = Duration::from_secs(1);

/// The sink `tm architect poll` records its wakes through; see the module doc.
///
/// Why: a poller under a scratch `$HOME` or data root must never push into the
/// operator's bus, the same rule the daemon's producer follows.
/// What: `from` names the poller (its tmux session). Bodies follow
/// `TRUSTY_MPM_EVENTS_MESSAGE_BODY`, as for `tm hook`.
/// Test: `an_unresolvable_bus_reports_every_wake_as_not_recorded_9167`.
pub(crate) fn poll_sink(from: &str) -> Box<dyn MessageSink> {
    let overridden =
        std::env::var_os(trusty_common::DATA_DIR_OVERRIDE_ENV).is_some_and(|v| !v.is_empty());
    let real = trusty_mpm::core::host_state_gate::host_state_access().is_allowed();
    if !bus_allowed(overridden, real) {
        return Box::new(NoMessages);
    }
    match trusty_common::control_bus::events_socket_path() {
        Ok(socket) => Box::new(BusMessages::new(socket, from, bodies_from_env())),
        Err(e) => Box::new(NoBus(format!("no trusty-events socket: {e:#}"))),
    }
}

/// A sink that cannot reach a bus and says so on every wake.
pub(crate) struct NoBus(pub(crate) String);

impl MessageSink for NoBus {
    fn record(&self, _: &PaneMessage<'_>) -> Result<(), MessageSinkError> {
        Err(MessageSinkError(self.0.clone()))
    }
}

/// A buffered producer of `poll_wake` events on one trusty-events socket.
pub(crate) struct BusMessages {
    client: PushClient,
    from: String,
    bodies: bool,
    budget: Duration,
    seq: AtomicU64,
}

impl BusMessages {
    /// A producer dialling `socket` for the poller named `from`.
    pub(crate) fn new(socket: impl Into<PathBuf>, from: &str, bodies: bool) -> Self {
        Self {
            client: PushClient::new(socket),
            from: from.to_string(),
            bodies,
            budget: FLUSH_BUDGET,
            seq: AtomicU64::new(0),
        }
    }

    /// Events not yet delivered.
    #[cfg(test)]
    pub(crate) fn buffered_len(&self) -> usize {
        self.client.buffered_len()
    }
}

impl MessageSink for BusMessages {
    /// Why: the wake is already typed and submitted; this only reports it.
    /// What: buffers one [`pane_message_event`], then one bounded flush on a
    /// scoped thread with its own runtime (the poller is synchronous and may
    /// run inside `tm`'s runtime). `Err` names the reason; the event stays
    /// buffered for the next flush.
    /// Test: `a_poller_wake_is_one_poll_wake_event_with_its_bytes_and_sha256_9167`,
    /// `a_down_bus_never_stops_the_wake_and_records_it_once_9167`.
    fn record(&self, message: &PaneMessage<'_>) -> Result<(), MessageSinkError> {
        let mut event = pane_message_event(Utc::now(), &self.from, message, self.bodies);
        event.seq = self.seq.fetch_add(1, Ordering::Relaxed);
        self.client.send(event);
        flush_within(&self.client, self.budget)
    }
}

/// The bus event for one delivered pane write.
///
/// What: a `message` event from `from` to the pane's session, on the channel
/// the write's kind names, built by [`message_event`] (credential scan first;
/// `bytes` and `sha256` of the scanned text; body only when `bodies`).
/// Test: `a_poller_wake_is_one_poll_wake_event_with_its_bytes_and_sha256_9167`.
pub(crate) fn pane_message_event(
    at: DateTime<Utc>,
    from: &str,
    message: &PaneMessage<'_>,
    bodies: bool,
) -> HarnessEvent {
    let channel = match message.kind {
        PaneMessageKind::PollWake => MessageChannel::PollWake,
        // A kind added to the leaf crate later is still a typed pane write.
        _ => MessageChannel::Tmux,
    };
    let event = message_event(at, channel, from, message.session, message.text, bodies);
    HarnessEvent {
        source: HarnessSource::Mpm,
        session: None,
        seq: 0,
        at,
        payload: HarnessPayload::Message(event),
        id: EventId::new(),
        parent_id: None,
    }
}

/// Flush `client` once within `budget`, off the caller's runtime.
fn flush_within(client: &PushClient, budget: Duration) -> Result<(), MessageSinkError> {
    let flush = || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| MessageSinkError(format!("no runtime for the bus flush: {e}")))?;
        // The timer must be made inside the runtime it runs on.
        match runtime.block_on(async { tokio::time::timeout(budget, client.flush()).await }) {
            Ok(PushFlushOutcome { error: None, .. }) => Ok(()),
            Ok(PushFlushOutcome { error: Some(e), .. }) => Err(MessageSinkError(format!(
                "trusty-events unreachable, event kept for the next wake: {e}"
            ))),
            Err(_) => Err(MessageSinkError(format!(
                "trusty-events gave no answer within {} ms, event kept for the next wake",
                budget.as_millis()
            ))),
        }
    };
    std::thread::scope(|scope| {
        scope
            .spawn(flush)
            .join()
            .unwrap_or_else(|_| Err(MessageSinkError("the bus flush panicked".to_string())))
    })
}

#[cfg(test)]
#[path = "architect_messages_tests.rs"]
mod tests;
