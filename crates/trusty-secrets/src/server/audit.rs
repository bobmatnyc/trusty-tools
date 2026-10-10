//! The credential access audit trail: its record and its file (#4567).
//!
//! Why: DOC-45 §9 (C-7.7, C-7.9, C-7.12) and owner Q2 — every credential
//! access is recorded per call, in one stream whose discriminator is
//! `credential_access`. The server is on demand and exits when idle (ruling
//! 28), so nothing resident could flush a buffer later: each record is
//! written and synced before the reply.
//! What: [`AuditRecord`], typed fields only — no value and no free text; a
//! vault, key or project root appears only after the server validated or
//! derived it. The crate-private `AuditSink` appends one JSON line per record
//! to [`ServerSettings::audit_log`](super::ServerSettings::audit_log): a 0600
//! file in a 0700 directory, both created with their mode, opened
//! `O_APPEND | O_NOFOLLOW` and judged on the open descriptor by the #9326 file
//! backend's mode, owner and symlink rule. It is never truncated; once it
//! reaches the size cap it is renamed to `<log>.1` the next time it is opened.
//! Which calls write records, and when, is `gate`'s job.
//! Test: `audit_tests.rs` — `audit_record_round_trips_a_pid_and_an_unknown_reason`,
//! `audit_sink_creates_0600_in_0700_and_refuses_a_wrong_mode`,
//! `audit_sink_rotates_at_open_and_never_truncates`.
//!
//! # Spec References
//! - DOC-45 §9 C-7.7, C-7.9, C-7.12 (`docs/specs/DOC-45-credential-authority-model.md`)

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use super::errors::ErrorKind;
use crate::api::{BackendId, SecretKey, SecretsError, VaultName};
use crate::store::file::{self, Kind};

/// The one audit stream's discriminator (owner Q2).
pub const AUDIT_STREAM: &str = "credential_access";

/// Which stream a record belongs to. There is one today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuditStream {
    /// [`AUDIT_STREAM`].
    CredentialAccess,
}

/// The audited `secrets.*` method. `scopes` and `doctor` are never audited.
///
/// What: serialized as the method's wire name. A name this build does not
/// know decodes as [`AuditMethod::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum AuditMethod {
    /// `secrets.set`.
    #[serde(rename = "secrets.set")]
    Set,
    /// `secrets.delete`.
    #[serde(rename = "secrets.delete")]
    Delete,
    /// `secrets.copy`, one record per key.
    #[serde(rename = "secrets.copy")]
    Copy,
    /// `secrets.list`, denials only.
    #[serde(rename = "secrets.list")]
    List,
    /// `secrets.grant`, one record per granted key, or one deny (#9070).
    #[serde(rename = "secrets.grant")]
    Grant,
    /// `secrets.resolve`, one record per key resolved or refused (#9070).
    #[serde(rename = "secrets.resolve")]
    Resolve,
    /// `secrets.revoke`, one record per call (#9070).
    #[serde(rename = "secrets.revoke")]
    Revoke,
    /// `secrets.set_agents_may_use`: one allow record before the backend
    /// call, then a deny if that call fails; one deny for a refusal (#9070).
    #[serde(rename = "secrets.set_agents_may_use")]
    SetAgentsMayUse,
    /// A method a newer server audits. Never written by this build.
    #[serde(other)]
    Other,
}

/// Whether the access was allowed and completed, or refused or failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuditDecision {
    /// The operation was allowed and succeeded.
    Allow,
    /// The operation was refused or failed; the record's reason says why.
    Deny,
}

/// A denial's reason: an [`ErrorKind::as_str`] wire string.
///
/// Why: the same closed vocabulary the error reply carries, so a record can
/// never hold text a caller sent. Decoding accepts any string, so a log
/// written by a newer server with a newer kind still reads.
/// Test: `audit_record_round_trips_a_pid_and_an_unknown_reason`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AuditReason(String);

impl AuditReason {
    /// The wire string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The kind, or `None` for a kind this build does not know.
    pub fn kind(&self) -> Option<ErrorKind> {
        ErrorKind::from_wire(&self.0)
    }
}

impl From<ErrorKind> for AuditReason {
    fn from(kind: ErrorKind) -> Self {
        Self(kind.as_str().to_owned())
    }
}

/// One line of the audit log.
///
/// Why: see the module docs. Every field is typed; none can carry a value.
/// What: `vault`, `key`, `backend` and `project_root` are `None` when the
/// call was refused before the server knew them (an undecodable request has
/// none). `caller_pid` is the socket peer's pid as the kernel reported it
/// (#9070), `None` when the kernel reported none. #9070 slice 3:
/// `agents_allowed` is the flag value a `secrets.set_agents_may_use` call
/// asked for, and `agent_parent` whether the caller's ancestry has a Claude
/// Code process, on `grant`, `resolve` and flag-set records once judged.
/// Unknown fields are ignored and missing optional fields default, so old
/// and new readers and writers interoperate.
/// Test: `audit_record_round_trips_a_pid_and_an_unknown_reason`,
/// `audit_flag_record_round_trips_and_an_older_record_decodes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AuditRecord {
    /// Seconds since the Unix epoch.
    pub ts: u64,
    /// Always [`AuditStream::CredentialAccess`] today.
    pub stream: AuditStream,
    /// The method called.
    pub method: AuditMethod,
    /// Allowed or denied.
    pub decision: AuditDecision,
    /// Why it was denied; `None` when allowed.
    #[serde(default)]
    pub reason: Option<AuditReason>,
    /// The vault named, once decoded.
    #[serde(default)]
    pub vault: Option<VaultName>,
    /// The key named, once decoded.
    #[serde(default)]
    pub key: Option<SecretKey>,
    /// The backend written to, once resolved.
    #[serde(default)]
    pub backend: Option<BackendId>,
    /// The project's checkout root, once resolved.
    #[serde(default)]
    pub project_root: Option<PathBuf>,
    /// The calling process: the socket peer's pid (#9070).
    #[serde(default)]
    pub caller_pid: Option<u32>,
    /// The "agents may use" value a flag-set call asked for (#9070).
    #[serde(default)]
    pub agents_allowed: Option<bool>,
    /// Whether the caller had a Claude Code ancestor, once judged (#9070).
    #[serde(default)]
    pub agent_parent: Option<bool>,
}

impl AuditRecord {
    /// A record for `method` with no subject: allowed when `denied` is
    /// `None`, otherwise denied with that kind as the reason.
    pub fn new(ts: u64, method: AuditMethod, denied: Option<ErrorKind>) -> Self {
        Self {
            ts,
            stream: AuditStream::CredentialAccess,
            method,
            decision: match denied {
                None => AuditDecision::Allow,
                Some(_) => AuditDecision::Deny,
            },
            reason: denied.map(AuditReason::from),
            vault: None,
            key: None,
            backend: None,
            project_root: None,
            caller_pid: None,
            agents_allowed: None,
            agent_parent: None,
        }
    }

    /// This record with `caller_pid` set.
    pub fn with_caller_pid(mut self, pid: Option<u32>) -> Self {
        self.caller_pid = pid;
        self
    }
}

/// The audit log file, opened per call.
///
/// What: holds the path and the cap only. The mutex serializes opens, so one
/// thread's rotation never races another's. A record written through a
/// handle opened before a rotation lands in `<log>.1`: kept, not lost.
#[derive(Debug)]
pub(crate) struct AuditSink {
    path: PathBuf,
    max_bytes: u64,
    opening: Mutex<()>,
}

/// An open, checked audit file that records can be appended to.
#[derive(Debug)]
pub(crate) struct AuditFile {
    file: File,
}

impl AuditSink {
    pub(crate) fn new(path: PathBuf, max_bytes: u64) -> Self {
        Self {
            path,
            max_bytes,
            opening: Mutex::new(()),
        }
    }

    /// Create or check the directory, rotate a full log, and open it.
    ///
    /// What: the parent is created 0700 (with missing ancestors) or checked;
    /// an existing log is `lstat`-judged as a 0600 file and, at or past the
    /// cap, renamed over `<log>.1`. The log is then opened append-only,
    /// created 0600 when absent, never following a symlink, and the open
    /// descriptor is judged again, so a swap between the checks is caught.
    /// #4567: after a create or a rotation the directory is fsynced, so the
    /// first record survives a power loss. A non-empty log whose last byte
    /// is not `\n` gets one first, so a torn line never swallows the next
    /// record.
    ///
    /// # Errors
    ///
    /// [`SecretsError::StorageRefused`] for a symlink, a wrong mode or
    /// another owner; [`SecretsError::Io`] for anything else.
    ///
    /// Test: `audit_sink_creates_0600_in_0700_and_refuses_a_wrong_mode`,
    /// `audit_sink_rotates_at_open_and_never_truncates`,
    /// `audit_sink_ends_a_torn_line_before_the_next_record`.
    pub(crate) fn open(&self) -> Result<AuditFile, SecretsError> {
        let _opening = self.opening.lock().unwrap_or_else(PoisonError::into_inner);
        let dir = self.path.parent().unwrap_or(Path::new("."));
        file::create_dir(dir, true)?;
        let existed = self.rotate_if_full(dir)?;
        let mut handle = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&self.path)
            .map_err(|source| file::open_failure(&self.path, source))?;
        let meta = handle.metadata().map_err(|source| SecretsError::Io {
            path: self.path.clone(),
            source,
        })?;
        file::judge(&self.path, &meta, Kind::File)?;
        let io = |source: std::io::Error| SecretsError::Io {
            path: self.path.clone(),
            source,
        };
        if !existed {
            // #4567: a new directory entry is durable only once synced.
            file::sync_dir(dir)?;
        }
        if meta.len() > 0 {
            // The read leaves the descriptor at end-of-file, where the next
            // append lands; macOS checks `RLIMIT_FSIZE` against this offset.
            let mut last = [0u8; 1];
            handle.seek(SeekFrom::End(-1)).map_err(io)?;
            handle.read_exact(&mut last).map_err(io)?;
            if last != *b"\n" {
                // #4567: end a torn line, so the next record parses.
                handle.write_all(b"\n").map_err(io)?;
            }
        }
        Ok(AuditFile { file: handle })
    }

    /// Rename a log at or past the cap to `<log>.1`, replacing the old one.
    /// `Ok(true)` when a log is still at the path afterwards.
    fn rotate_if_full(&self, dir: &Path) -> Result<bool, SecretsError> {
        let meta = match fs::symlink_metadata(&self.path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(source) => {
                return Err(SecretsError::Io {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        file::judge(&self.path, &meta, Kind::File)?;
        if meta.len() < self.max_bytes {
            return Ok(true);
        }
        let mut rotated = self.path.clone().into_os_string();
        rotated.push(".1");
        fs::rename(&self.path, &rotated).map_err(|source| SecretsError::Io {
            path: self.path.clone(),
            source,
        })?;
        file::sync_dir(dir)?;
        Ok(false)
    }
}

impl AuditFile {
    /// Append `record` as one JSON line in a single write, then sync it.
    ///
    /// # Errors
    ///
    /// Any serialization, write or sync failure; the caller decides what a
    /// failure means (see `gate`).
    pub(crate) fn append(&mut self, record: &AuditRecord) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(record).map_err(std::io::Error::other)?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        self.file.sync_data()
    }
}
