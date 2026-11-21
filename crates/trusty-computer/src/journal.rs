//! The per-grant `request_id` journal: a retried call is answered, never re-run.
//!
//! Why: an agent that lost an ack retries with the same `request_id`; running
//! a click twice would press the button twice (spike `broker.py:82-87`, plan
//! section 4 step 1).
//! What: [`request_id`] requires a non-empty string id. [`Journal`] maps an id
//! to its reply, in memory, capped at [`JOURNAL_LIMIT`] entries as
//! trusty-browser's `leases.rs`. An entry can be marked
//! [`Outcome::Unknown`] when the daemon session ended mid-call (plan
//! section 3); its replay says so instead of claiming success.
//! Test: `tests/spike_journal.rs`.

use std::collections::HashMap;

use serde_json::Value;

/// Entries one grant's journal may hold.
pub const JOURNAL_LIMIT: usize = 4096;

/// Why a journal operation was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JournalError {
    /// The call carried no non-empty string `request_id`.
    #[error("request_id_required: every call needs a request_id")]
    RequestIdRequired,
    /// The journal already holds [`JOURNAL_LIMIT`] entries.
    #[error("journal_full: the grant journal holds {JOURNAL_LIMIT} entries")]
    Full,
    /// The id was recorded already; a replay must be answered instead.
    #[error("request_id_reused: {0} already has a reply")]
    Duplicate(String),
}

impl JournalError {
    /// Stable refusal code for audit lines and replies.
    pub fn code(&self) -> &'static str {
        match self {
            JournalError::RequestIdRequired => "request_id_required",
            JournalError::Full => "journal_full",
            JournalError::Duplicate(_) => "request_id_reused",
        }
    }
}

/// Whether a recorded call is known to have completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The daemon answered; the reply is what it said.
    Known,
    /// The daemon session ended during the call; it may or may not have run.
    Unknown,
}

/// One recorded reply.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The reply sent for the first attempt.
    pub reply: Value,
    /// Whether that reply reflects a completed call.
    pub outcome: Outcome,
}

/// The `request_id` of a broker request.
///
/// Why: a call without an id cannot be made idempotent, so it is refused.
/// What: the request's `request_id` when it is a non-empty string.
/// Test: `call_without_request_id_is_refused`.
pub fn request_id(request: &Value) -> Result<&str, JournalError> {
    match request.get("request_id").and_then(Value::as_str) {
        Some(id) if !id.is_empty() => Ok(id),
        _ => Err(JournalError::RequestIdRequired),
    }
}

/// One grant's journal.
#[derive(Debug, Default, Clone)]
pub struct Journal {
    entries: HashMap<String, Entry>,
}

impl Journal {
    /// An empty journal (a broker restart starts here, plan section 4).
    pub fn new() -> Journal {
        Journal::default()
    }

    /// Entries held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no entry is held.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The recorded entry for `id`; `Some` means answer it and do not re-run.
    pub fn replay(&self, id: &str) -> Option<&Entry> {
        self.entries.get(id)
    }

    /// Refuse before running when a new id would not fit.
    ///
    /// Why: a call must not run when its reply cannot be journaled.
    /// What: `Full` when `id` is new and the journal is at the limit.
    /// Test: `journal_refuses_a_new_id_when_full`.
    pub fn reserve(&self, id: &str) -> Result<(), JournalError> {
        if !self.entries.contains_key(id) && self.entries.len() >= JOURNAL_LIMIT {
            return Err(JournalError::Full);
        }
        Ok(())
    }

    /// Record the reply of a first attempt.
    ///
    /// Why: the reply must be stored before the next retry can arrive.
    /// What: `Duplicate` when `id` is held, `Full` at the limit, else stored.
    /// Postcondition: on `Ok`, `replay(id)` returns `reply` with `outcome`.
    /// Test: `request_id_retry_is_not_rerun`, `journal_refuses_a_new_id_when_full`.
    pub fn record(&mut self, id: &str, reply: Value, outcome: Outcome) -> Result<(), JournalError> {
        if self.entries.contains_key(id) {
            return Err(JournalError::Duplicate(id.to_owned()));
        }
        self.reserve(id)?;
        self.entries.insert(id.to_owned(), Entry { reply, outcome });
        debug_assert!(self.entries.len() <= JOURNAL_LIMIT);
        Ok(())
    }

    /// Drop every entry, as at grant end.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}
