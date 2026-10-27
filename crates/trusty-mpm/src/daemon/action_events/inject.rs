//! The daemon's [`InjectRecorder`]: each delivered inject as a `message`
//! event (#9167 AC5).
//!
//! Why: text the daemon types into a managed pane is an inter-process message
//! no hook sees (`session_manager::inject_record`). It rides the hook path's
//! buffer, so it shares its bound, flush task, failure handling and final
//! flush.
//! What: [`ActionPublisher`] implements [`InjectRecorder`]. One delivery is one
//! `tmux` `message` event — every inject is a `send-keys` into the session's
//! pane — from [`INJECT_FROM`] to the session's tmux name, built by
//! [`message_event`] (credential scan, then `bytes` and `sha256` of the scanned
//! text, body only when bodies are on). The event names the receiving Claude
//! session when the record knows it, and is a child of that session's root
//! when one was projected (none is, under `tm sm serve --stdio`). [`install`]
//! hands the attached publisher to the session manager, which takes it when it
//! is built.
//!
//! A bus write never fails on this path: `PushClient::send` only buffers. A
//! flush that cannot reach the bus keeps the event buffered for the next tick;
//! a full buffer drops the oldest event and counts it in
//! [`ActionPublisher::dropped`], which the flush task warns about. In neither
//! case does the inject itself fail — the text is already in the pane, and an
//! error would make a caller retry and type it twice.
//! Test: `inject_record_tests.rs` (session manager),
//! `inject_tests.rs` (this producer).

use std::sync::Arc;
use std::sync::atomic::Ordering;

use trusty_common::control_bus::{
    EventId, HarnessEvent, HarnessPayload, HarnessSource, MessageChannel,
};

use super::ActionPublisher;
use super::message::message_event;
use crate::core::session::SessionId;
use crate::daemon::DaemonState;
use crate::session_manager::inject_record::{Delivery, InjectRecorder};

/// The `from` of an inject's `message` event: the daemon typed it.
pub const INJECT_FROM: &str = "trusty-mpm";

impl InjectRecorder for ActionPublisher {
    /// Why: see the module doc.
    /// What: builds [`inject_event`] under the projection lock, as
    /// `publish` does, so `seq` follows buffer order. No I/O.
    /// Test: `an_inject_is_one_tmux_message_event_with_its_digest_9167`,
    /// `an_unreachable_bus_keeps_the_inject_event_buffered_9167`.
    fn delivered(&self, delivery: &Delivery<'_>) {
        let threader = self.threader.lock();
        let root = delivery
            .claude_session_id
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .and_then(|u| threader.root_of(&SessionId(u)));
        let mut event = inject_event(delivery, threader.message_bodies(), root);
        event.seq = self.seq.fetch_add(1, Ordering::Relaxed);
        self.client.send(event);
    }
}

/// The bus event for one delivered inject; see the module doc.
///
/// Test: `an_inject_is_one_tmux_message_event_with_its_digest_9167`.
pub fn inject_event(delivery: &Delivery<'_>, bodies: bool, root: Option<EventId>) -> HarnessEvent {
    let message = message_event(
        delivery.at,
        MessageChannel::Tmux,
        INJECT_FROM,
        delivery.tmux_name,
        delivery.text,
        bodies,
    );
    HarnessEvent {
        source: HarnessSource::Mpm,
        session: delivery.claude_session_id.map(str::to_string),
        seq: 0,
        at: delivery.at,
        payload: HarnessPayload::Message(message),
        id: EventId::new(),
        parent_id: root,
    }
}

/// [`super::start`], then [`install`]: the bus producer for hooks and for
/// delivered injects. Returns whether a recorder was installed.
///
/// Why: the daemon and `tm sm serve --stdio` both inject; each records on
/// its own producer, under the same `bus_allowed` rule.
pub fn start_with_injects(state: &DaemonState) -> bool {
    super::start(state);
    install(state)
}

/// Make the attached publisher `state`'s inject recorder.
///
/// Why: building the session manager reconciles the shared store
/// (`reconcile_on_boot`); a `tm sm serve --stdio` run that never touches a
/// session must not do that just to record injects.
/// What: `false`, installing nothing, when no publisher is attached (a
/// disallowed or unresolvable bus) or a recorder is already set. Otherwise
/// the recorder is kept on `state`, and `DaemonState::session_manager` hands
/// it to the manager when it builds one; a manager already built takes it
/// now. This never builds the manager.
/// Test: `install_needs_an_attached_publisher_9167`,
/// `installing_the_recorder_does_not_build_the_session_manager_9167`.
pub fn install(state: &DaemonState) -> bool {
    let Some(publisher) = state.action_events().publisher() else {
        return false;
    };
    let recorder: Arc<dyn InjectRecorder> = Arc::clone(publisher) as Arc<dyn InjectRecorder>;
    state.set_inject_recorder(recorder)
}

#[cfg(test)]
#[path = "inject_tests.rs"]
mod tests;
