//! The seam a scripted pane write is recorded through (#9167 PR2).
//!
//! Why: the owner asked that every inter-session message reach the
//! trusty-events bus, and the poller's wake types one into the Architect's
//! pane. This crate is a leaf: it must not depend on `trusty-mpm`, which owns
//! the bus producer, the credential scan and the digest rule. So the poller
//! reports what it delivered to a sink its caller supplies, as it already
//! does for notifications ([`super::steps::notify_macos::Notifier`]).
//! What: [`PaneMessage`] is one delivered write: its kind, the session and
//! pane it went to, and the exact text typed. [`MessageSink::record`] is
//! called once per delivered write, after the write, so a sink can neither
//! block nor undo it. [`NoMessages`] records nothing (tests, a caller with no
//! bus).
//! Test: `a_submitted_wake_is_recorded_once_with_the_typed_text_9167`,
//! `a_sink_error_never_stops_the_wake_and_is_logged_9167`,
//! `a_wake_that_was_not_submitted_records_nothing_9167`
//! (tests/wake_messages.rs).

use crate::error::MessageSinkError;

/// What kind of scripted write a [`PaneMessage`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PaneMessageKind {
    /// The poller's wake pointer, `[poll HH:MMZ] ...`.
    PollWake,
}

/// One write a script typed into a pane and submitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneMessage<'a> {
    /// What kind of write it was.
    pub kind: PaneMessageKind,
    /// The tmux session the pane belongs to.
    pub session: &'a str,
    /// The pane id the keys went to (`%N`).
    pub pane: &'a str,
    /// The exact text typed, without the Enter that submitted it.
    pub text: &'a str,
}

/// Records delivered pane writes for the caller.
pub trait MessageSink {
    /// Records `message`, which has already been delivered.
    ///
    /// # Errors
    /// The message could not be recorded. The caller logs it; the write it
    /// describes stands.
    fn record(&self, message: &PaneMessage<'_>) -> Result<(), MessageSinkError>;
}

/// A sink that records nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoMessages;

impl MessageSink for NoMessages {
    fn record(&self, _: &PaneMessage<'_>) -> Result<(), MessageSinkError> {
        Ok(())
    }
}
