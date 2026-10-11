//! `audit.jsonl`: one line per refused send, dropped or rate-limited inbound
//! message, or quarantined ledger line.
//!
//! Why: a refused send and a dropped reply are the events an operator needs
//! to see, but the audit log must never become a copy of the conversation
//! (#9448 ruling 4).
//! What: [`AuditRecord`] has no text field at all: it records time, event,
//! a fixed reason code, the route/sender/space identifiers, and the text
//! length in bytes. [`AuditLog::record`] appends one line.
//! Test: `audit_lines_never_contain_message_text`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::gchat::error::StateError;
use crate::gchat::state::{append_line, now_rfc3339};

/// The longest identifier copied into an audit line.
const MAX_FIELD_CHARS: usize = 254;

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEvent {
    /// The egress gate refused an outbound message.
    SendRefused,
    /// An inbound message was dropped.
    InboundDropped,
    /// A pulled message did not decode into a Chat event.
    InboundUnparseable,
    /// A torn final `questions.jsonl` line was moved aside at open.
    LedgerLineQuarantined,
    /// An inbound message was dropped over its rate limit (#8454); one line
    /// per bucket per window.
    RateLimited,
}

/// One audit line. It has no field that can hold message text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditRecord {
    /// RFC 3339 time.
    pub ts: String,
    /// What happened.
    pub event: AuditEvent,
    /// A fixed reason code, e.g. `no_route` or `no_open_question`.
    pub reason: &'static str,
    /// The route involved, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    /// The message kind of a refused send.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<&'static str>,
    /// The requested route name or recipient of a refused send.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// The inbound sender (email, else user resource name).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender: Option<String>,
    /// The inbound space.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space: Option<String>,
    /// The Pub/Sub message id of an inbound message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// Text length in bytes, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length: Option<usize>,
}

impl AuditRecord {
    /// A record stamped now with only `event` and `reason` set.
    pub fn new(event: AuditEvent, reason: &'static str) -> Self {
        Self {
            ts: now_rfc3339(),
            event,
            reason,
            route: None,
            kind: None,
            to: None,
            sender: None,
            space: None,
            message_id: None,
            length: None,
        }
    }
}

/// Clip an identifier copied into an audit line.
pub(crate) fn clip(s: &str) -> String {
    s.chars().take(MAX_FIELD_CHARS).collect()
}

/// The append-only audit log.
#[derive(Debug, Clone)]
pub struct AuditLog {
    path: PathBuf,
}

impl AuditLog {
    /// A log at `path`; the file is created on first write.
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
        }
    }

    /// The log path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append `record`.
    pub fn record(&self, record: &AuditRecord) -> Result<(), StateError> {
        append_line(&self.path, record)
    }
}
