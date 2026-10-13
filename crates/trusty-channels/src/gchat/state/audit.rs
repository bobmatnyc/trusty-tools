//! `audit.jsonl`: one line per refused send, dropped or rate-limited inbound
//! message, quarantined ledger line, or route change.
//!
//! Why: a refused send and a dropped reply are the events an operator needs
//! to see, but the audit log must never become a copy of the conversation
//! (#9448 ruling 4). #9832 shares the record with the tm daemon's channel
//! audit, through [`crate::audit`].
//! What: [`AuditRecord`] has no text field at all: it records time, event,
//! a fixed reason code, the route/sender/space identifiers, the text length
//! in bytes, and for a route change the project, commit, author and time.
//! [`AuditLog::record`] appends one line; [`AuditLog::record_all`] appends a
//! batch with one write.
//! Test: `audit_lines_never_contain_message_text`,
//! `record_all_appends_every_line`, `owner_only_log_creates_file_0600`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::gchat::error::StateError;
use crate::gchat::state::{append_bytes, append_line, now_rfc3339};

/// The longest identifier copied into an audit line.
const MAX_FIELD_CHARS: usize = 254;

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
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
    /// #9832: the routes in effect changed, or were loaded for the first time.
    RouteChanged,
}

/// One audit line. It has no field that can hold message text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct AuditRecord {
    /// RFC 3339 time.
    pub ts: String,
    /// What happened.
    pub event: AuditEvent,
    /// A fixed reason code, e.g. `no_route` or `no_open_question`.
    pub reason: &'static str,
    /// The channel, e.g. `slack` (#9832).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<&'static str>,
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
    /// The project directory whose routes changed (#9832).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// The commit the changed routes were verified at (#9832).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Who changed the routes, always marked `(inferred)` (#9832).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// When that change was committed, RFC 3339 (#9832).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub committed_at: Option<String>,
    /// How many routes the file contributes now (#9832).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routes: Option<usize>,
}

impl AuditRecord {
    /// A record stamped now with only `event` and `reason` set.
    pub fn new(event: AuditEvent, reason: &'static str) -> Self {
        Self {
            ts: now_rfc3339(),
            event,
            reason,
            channel: None,
            route: None,
            kind: None,
            to: None,
            sender: None,
            space: None,
            message_id: None,
            length: None,
            project: None,
            commit: None,
            author: None,
            committed_at: None,
            routes: None,
        }
    }
}

/// Clip an identifier copied into an audit line to 254 characters.
pub fn clip(s: &str) -> String {
    s.chars().take(MAX_FIELD_CHARS).collect()
}

/// The append-only audit log.
#[derive(Debug, Clone)]
pub struct AuditLog {
    path: PathBuf,
    /// #9832: create the file 0600 and narrow an existing one.
    owner_only: bool,
}

impl AuditLog {
    /// A log at `path`; the file is created on first write.
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            owner_only: false,
        }
    }

    /// A log at `path` readable and writable by its owner only.
    ///
    /// Why: #9832 (A3): the tm daemon's channel audit names projects,
    /// commits and authors; other local users must not read it. gchat's
    /// state files keep the mode they are created with ([`AuditLog::new`]).
    /// What: as [`AuditLog::new`], except each write creates the file 0600
    /// and narrows an existing file to 0600 before it appends.
    /// Test: `owner_only_log_creates_file_0600`,
    /// `gchat_log_keeps_an_existing_file_mode`.
    pub fn owner_only(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            owner_only: true,
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

    /// Append every record in `records`.
    ///
    /// Why: #9832: a route change is durable before the routes go live, so
    /// one batch is one write and one fsync.
    /// What: encodes every record as one JSON line, then appends them all
    /// with one `write_all` and one `sync_data`; an empty batch writes
    /// nothing. An encode or I/O failure returns the error and appends
    /// nothing it can avoid; an owner-only log cuts a failed batch back off.
    /// A crash mid-write can still leave a torn last line; an owner-only
    /// append then starts on a line of its own (#8433).
    /// Test: `record_all_appends_every_line`, `owner_only_log_creates_file_0600`,
    /// `failed_owner_only_append_leaves_no_torn_line`,
    /// `append_after_a_left_fragment_starts_on_a_new_line`.
    pub fn record_all(&self, records: &[AuditRecord]) -> Result<(), StateError> {
        if records.is_empty() {
            return Ok(());
        }
        let mut bytes = Vec::new();
        for record in records {
            serde_json::to_writer(&mut bytes, record).map_err(|e| StateError::Corrupt {
                path: self.path.clone(),
                line: 0,
                reason: format!("cannot encode record: {e}"),
            })?;
            bytes.push(b'\n');
        }
        if self.owner_only {
            append_owner_only(&self.path, &bytes)
        } else {
            append_bytes(&self.path, &bytes)
        }
    }
}

/// Append `bytes` to `path` with one `write_all` and an fsync, creating the
/// file 0600 and narrowing an existing one to 0600 first (#9832 A3). A
/// failed write or fsync truncates the file back to its length before the
/// write, so no fragment is left for the next line to join.
#[cfg(unix)]
fn append_owner_only(path: &Path, bytes: &[u8]) -> Result<(), StateError> {
    use std::io::Write as _;
    append_owner_only_with(path, bytes, |file, bytes| file.write_all(bytes))
}

/// [`append_owner_only`], writing through `write` (a test's partial write).
///
/// Why: #8433: the tm daemon has more than one writer of one audit file; a
/// failed append's cut-back must never erase another writer's record, and
/// a torn last line must never swallow the next record.
/// What: takes an exclusive `flock` right after the open and holds it
/// through the length read, the write, the fsync and any cut-back; a lock
/// error returns before anything is written. Under the lock, a non-empty
/// file whose last byte is not `\n` gets `\n` written ahead of the batch;
/// the cut-back target stays the length before that `\n`.
/// Test: `failed_append_does_not_erase_a_concurrent_writers_record`,
/// `append_after_a_left_fragment_starts_on_a_new_line`,
/// `failed_owner_only_append_leaves_no_torn_line`.
#[cfg(unix)]
pub(crate) fn append_owner_only_with(
    path: &Path,
    bytes: &[u8],
    write: impl FnOnce(&mut std::fs::File, &[u8]) -> std::io::Result<()>,
) -> Result<(), StateError> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let io = |e: std::io::Error| StateError::io(path, &e);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .map_err(io)?;
    // #8433: hold the lock from the length read through the cut-back, so a
    // failed append never truncates a concurrent writer's record. It is
    // released when `file` drops.
    file.lock().map_err(io)?;
    // #9832: narrow through the open handle, so a swapped path is not chmodded.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(io)?;
    let before = file.metadata().map_err(io)?.len();
    // #8433: a torn last line keeps its own line; the batch starts on the next.
    let joined;
    let bytes = if ends_mid_line(&file, before).map_err(io)? {
        joined = [b"\n".as_slice(), bytes].concat();
        joined.as_slice()
    } else {
        bytes
    };
    write(&mut file, bytes)
        .and_then(|()| file.sync_data())
        .map_err(|e| {
            // #9832: cut a partial batch back off, so the next append starts
            // on a line boundary; the write's own error is what is returned.
            if let Err(cut) = file.set_len(before).and_then(|()| file.sync_data()) {
                tracing::error!(%cut, path = %path.display(), "audit log not cut back after a failed append");
            }
            io(e)
        })
}

/// True when `file`, `len` bytes long, is non-empty and its last byte is
/// not `\n` (a torn last line).
#[cfg(unix)]
fn ends_mid_line(file: &std::fs::File, len: u64) -> std::io::Result<bool> {
    use std::os::unix::fs::FileExt as _;
    let Some(last_at) = len.checked_sub(1) else {
        return Ok(false);
    };
    let mut last = [0u8; 1];
    file.read_exact_at(&mut last, last_at)?;
    Ok(last[0] != b'\n')
}

/// Owner-only modes are a Unix notion; elsewhere this is a plain append.
#[cfg(not(unix))]
fn append_owner_only(path: &Path, bytes: &[u8]) -> Result<(), StateError> {
    append_bytes(path, bytes)
}
