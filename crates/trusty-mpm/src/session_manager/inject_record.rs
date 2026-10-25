//! The inject path's `message` record (#9167 AC5).
//!
//! Why: the owner asked that every inter-process message reach the
//! trusty-events bus. Text the daemon types into a managed pane — the idle
//! nudge, a managed inbound line, `sm_stdio`, the MCP proxy, the Telegram and
//! Slack focus paths — never passes a Claude Code tool call, so no `tm hook`
//! record sees it. Every one of those callers reaches
//! `SessionManager::inject_into_owned`, so that is where it is recorded, once.
//! What: [`InjectRecorder`] is the seam the process that owns a bus producer
//! installs (`daemon::action_events::inject`); `session_manager` sits below the
//! daemon and cannot name its publisher. `inject_into_owned` calls
//! [`SessionManager::record_delivery`] after tmux accepted the text and before
//! the record is written. With no recorder installed — every hermetic test, an
//! isolated daemon, the supervisor — nothing is recorded.
//! Test: `inject_record_tests.rs`.

use std::sync::{Arc, OnceLock};

use chrono::{DateTime, Utc};

use super::SessionManager;
use super::record::SessionRecord;

/// One text the inject path delivered to a pane.
#[derive(Debug, Clone, Copy)]
pub struct Delivery<'a> {
    /// When tmux accepted the text.
    pub at: DateTime<Utc>,
    /// The receiving session's tmux name.
    pub tmux_name: &'a str,
    /// The receiving session's Claude session id, when known.
    pub claude_session_id: Option<&'a str>,
    /// The text as typed, before any scan.
    pub text: &'a str,
}

/// Records each delivered inject; implemented over the daemon's bus producer.
///
/// Why: see the module doc.
/// What: `delivered` must not block or do I/O — it runs on the inject path,
/// after the text is typed. It returns nothing: the text is already in the
/// pane, so a recording failure must never turn the inject into an error a
/// caller would retry (and type twice). The implementation owns what a failed
/// bus write means (see `daemon::action_events::inject`).
/// Test: `an_inject_is_exactly_one_message_event_9167`.
pub trait InjectRecorder: Send + Sync {
    /// Record one delivered text.
    fn delivered(&self, delivery: &Delivery<'_>);
}

/// A recorder that is attached after its holder is built, shared by clones.
///
/// Why: the control-plane `TmuxBackend` types into a pane without a
/// `SessionManager`, and the registry that builds it exists before the daemon
/// attaches its bus producer (#9167 AC5).
/// What: set once; [`Self::record`] is a no-op until then.
/// Test: `a_delivered_send_is_one_message_event_9167`.
#[derive(Clone, Default)]
pub struct InjectSlot(Arc<OnceLock<Arc<dyn InjectRecorder>>>);

impl std::fmt::Debug for InjectSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("InjectSlot")
            .field(&self.0.get().is_some())
            .finish()
    }
}

impl InjectSlot {
    /// Keep `recorder`; `false` when one is already kept.
    pub fn set(&self, recorder: Arc<dyn InjectRecorder>) -> bool {
        self.0.set(recorder).is_ok()
    }

    /// Report `text`, just typed into the pane of `tmux_name`; no-op when empty.
    pub fn record(&self, tmux_name: &str, text: &str) {
        if let Some(recorder) = self.0.get() {
            recorder.delivered(&Delivery {
                at: Utc::now(),
                tmux_name,
                claude_session_id: None,
                text,
            });
        }
    }
}

impl SessionManager {
    /// Report an answer just typed into `id`'s pane (the claimed-decision path).
    ///
    /// What: names the receiving Claude session when the record is readable;
    /// an unreadable record still records, without it. No-op with no recorder.
    /// Test: `a_delivered_answer_is_one_message_event_9167`.
    pub(super) async fn record_answer_delivery(
        &self,
        id: &super::ManagedSessionId,
        tmux_name: &str,
        text: &str,
    ) {
        let Some(recorder) = self.inject_recorder.get() else {
            return;
        };
        let claude = self.get(id).await.ok().and_then(|r| r.claude_session_id);
        recorder.delivered(&Delivery {
            at: Utc::now(),
            tmux_name,
            claude_session_id: claude.as_deref(),
            text,
        });
    }

    /// Install the recorder the inject path reports to; once only.
    ///
    /// Test: `installing_an_inject_recorder_twice_keeps_the_first_9167`.
    pub fn install_inject_recorder(&self, recorder: Arc<dyn InjectRecorder>) -> bool {
        self.inject_recorder.set(recorder).is_ok()
    }

    /// Report `text`, just typed into `record`'s pane, to the installed
    /// recorder; a no-op when none is installed.
    pub(super) fn record_delivery(&self, record: &SessionRecord, text: &str) {
        if let Some(recorder) = self.inject_recorder.get() {
            recorder.delivered(&Delivery {
                at: Utc::now(),
                tmux_name: &record.tmux_name,
                claude_session_id: record.claude_session_id.as_deref(),
                text,
            });
        }
    }
}

#[cfg(test)]
#[path = "inject_record_tests.rs"]
mod tests;
