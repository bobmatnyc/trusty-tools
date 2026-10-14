//! Delivering a resolved channel answer to its asker, tainted first (#9873,
//! #8433 P5).
//!
//! Why: an answer is untrusted text from outside the machine. The asker's
//! session must read tainted before the text reaches its pane, so the egress
//! guard refuses its path tool calls from then on. This is a Fail-Open Check
//! site: no failure may send before the taint, and none may be retried or
//! hidden; once the question resolved, a lost answer is final (#8433 Q4).
//! What: [`deliver`] runs over a [`DeliverEnv`] in a fixed order: the asker's
//! record ([`check_asker`]: the live Architect, a plain Claude session id, an
//! absolute workspace), the taint (Claude id file, then managed marker), a
//! `pending` line, the send of one [`frame_answer`] line, then the settle line.
//! Every failure writes a `failed` line and returns [`Delivery::Failed`].
//! Test: `deliver_tests.rs`.

use std::future::Future;
use std::path::Path;
use std::path::PathBuf;

use chrono::Utc;
use trusty_channels::policy::{Channel, QuestionId};

use crate::channels::inbound::Resolved;
use crate::core::channels_delivery::{DeliveryLine, DeliveryState};
use crate::core::channels_taint::TaintMark;
use crate::session_manager::{ManagedSessionState, SessionKind, SessionRecord};

/// What the deliverer needs to know about the asker's managed record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskerView {
    /// The Claude session id the taint file is named for.
    pub claude_session_id: Option<String>,
    /// The workspace the taint is scoped to.
    pub workspace: Option<PathBuf>,
    /// The record's kind; only the Architect is asked (#9873 Q3).
    pub kind: SessionKind,
    /// The record's state; only `Active` takes a delivery.
    pub state: ManagedSessionState,
}

impl AskerView {
    /// The view of `record`.
    pub fn of(record: &SessionRecord) -> Self {
        Self {
            claude_session_id: record.claude_session_id.clone(),
            workspace: record.workspace_path.clone(),
            kind: record.kind,
            state: record.state.clone(),
        }
    }
}

/// RED STUB (#9873 S4): accepts any asker; the next commit replaces it.
pub fn check_asker(view: &AskerView) -> Result<(&str, &Path), &'static str> {
    Ok((
        view.claude_session_id.as_deref().unwrap_or_default(),
        view.workspace.as_deref().unwrap_or(Path::new("")),
    ))
}

/// RED STUB (#9873 S4): the text unquoted; the next commit replaces it.
pub fn frame_answer(id: QuestionId, channel: Channel, from: &str, text: &str) -> String {
    format!("[Q-{id} {} {from}] {text}", channel.as_str())
}

/// The outcome of one delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// The pane took the answer and the `delivered` line is written.
    Delivered,
    /// Not delivered, or not confirmed; the question stays resolved.
    Failed {
        /// The step that failed.
        code: &'static str,
        /// Whether the send ran (a `delivery_unconfirmed` settle).
        sent: bool,
    },
}

/// Everything [`deliver`] reads or writes outside itself.
///
/// Why: each step is a Fail-Open Check arm that a test must make fail.
/// What: the daemon's is `DaemonDeliverEnv` (`managed_routes::inbound`).
pub trait DeliverEnv: Sync {
    /// The asker's managed record, as a view.
    fn asker(&self, managed: &str) -> impl Future<Output = Result<AskerView, String>> + Send;
    /// Taint `managed` and its Claude id `claude_session_id` with `mark`.
    fn taint(
        &self,
        managed: &str,
        claude_session_id: &str,
        mark: &TaintMark,
    ) -> impl Future<Output = Result<(), String>> + Send;
    /// Append one line to the delivery trace.
    fn record(&self, line: &DeliveryLine) -> impl Future<Output = Result<(), String>> + Send;
    /// Send `line` to `managed`'s pane, submitted.
    fn send(&self, managed: &str, line: &str) -> impl Future<Output = Result<(), String>> + Send;
}

/// RED STUB (#9873 S4): sends without the taint or the trace; the next
/// commit replaces it.
pub async fn deliver(env: &impl DeliverEnv, resolved: &Resolved) -> Delivery {
    let question = resolved.question();
    let asker = question.ask.asker_managed_session.as_str();
    let line = frame_answer(
        question.id,
        question.ask.channel,
        resolved.answerer(),
        resolved.text(),
    );
    let _ = (Utc::now(), DeliveryState::Pending, DeliveryLine::new);
    match env.send(asker, &line).await {
        Ok(()) => Delivery::Delivered,
        Err(_) => Delivery::Failed {
            code: "send_failed",
            sent: false,
        },
    }
}

#[cfg(test)]
#[path = "deliver_tests.rs"]
mod tests;
