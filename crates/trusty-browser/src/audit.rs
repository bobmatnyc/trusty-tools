//! Append-only, size-bounded `audit.jsonl`, line-compatible with Python 0.7.0.
//!
//! Why: the owner reads one audit file across a Python-to-Rust cutover and a
//! rollback, so line bytes, the field whitelist and rotation names must match
//! `audit.py` exactly.
//! What: one compact `ensure_ascii` JSON line per event, `ts` (seconds rounded
//! to 3 places) then `event` then the fields in call order, `None` fields
//! dropped; the file is owner-only 0600, rotated at 5 MiB keeping `.1`, `.2`.
//! Test: `audit_lines_replay_byte_identically`, `audit_rotates_like_python`,
//! `refusals_use_known_reasons` in `tests/parity_audit.rs`.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::private;
use crate::pyjson::Json;

/// Fields an audit line may carry besides `ts` and `event`.
pub const FIELDS: &[&str] = &[
    "event",
    "grant",
    "scope",
    "profile",
    "op",
    "tab",
    "outcome",
    "origin",
    "site",
    "reason",
    "email_domain",
    "replaced",
    // #11301: the URL hosts of the cross-origin child frames a read returned.
    "frames",
];

/// Refusal reason codes (`docs/activation-codes.md`, "Refusal log").
pub const REASONS: &[&str] = &[
    "peer_uid",
    "host_not_native_host",
    "host_parent_not_chrome",
    "host_unsafe_flag",
    "host_codesign",
    "host_timeout",
    "route_unknown",
    "ticket_mismatch",
    "proof_mismatch",
    "key_rotated",
    "unpair_bad_credential",
    "enroll_conflict_route",
    "enroll_conflict_key",
    "remove_unauthorized",
    "status_unauthorized",
];

/// Rotate once the file reaches this size.
pub const ROTATE_BYTES: u64 = 5 * 1024 * 1024;
/// Rotated generations kept (`audit.jsonl.1` .. `.KEEP`).
pub const KEEP: usize = 2;

/// Why an audit line was not recorded.
#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    /// A field outside [`FIELDS`] (Python `ValueError`).
    #[error("unknown audit field")]
    UnknownField,
    /// The log is not an owner-only regular file.
    #[error("audit log must be an owner-only regular file")]
    NotPrivate,
    /// A float value was NaN or infinite.
    #[error("audit value is not JSON compliant")]
    NonFinite,
    /// The write failed; callers fail closed (`AuditUnavailable`).
    #[error("audit write: {0}")]
    Io(#[from] io::Error),
}

/// Source of the `ts` field, injectable for tests.
pub trait Clock: Send + Sync {
    /// Seconds since the Unix epoch.
    fn now(&self) -> f64;
}

/// The system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64())
    }
}

/// The audit log of one state directory.
pub struct Audit {
    path: PathBuf,
    clock: Box<dyn Clock>,
}

impl std::fmt::Debug for Audit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Audit")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl Audit {
    /// `state/audit.jsonl` on the system clock.
    pub fn new(state: &Path) -> Audit {
        Audit::with_clock(state, Box::new(SystemClock))
    }

    /// `state/audit.jsonl` on `clock`.
    pub fn with_clock(state: &Path, clock: Box<dyn Clock>) -> Audit {
        Audit {
            path: state.join("audit.jsonl"),
            clock,
        }
    }

    /// The log path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The exact line `write` would append, without the trailing newline.
    pub fn line(&self, event: &str, fields: &[(&str, Option<Json>)]) -> Result<String, AuditError> {
        if fields.iter().any(|(k, _)| !FIELDS.contains(k)) {
            return Err(AuditError::UnknownField);
        }
        let ts = (self.clock.now() * 1000.0).round() / 1000.0;
        let mut record = Json::object([("ts", Json::Float(ts)), ("event", Json::from(event))]);
        for (k, v) in fields {
            if let Some(v) = v {
                record.insert(*k, v.clone());
            }
        }
        record.dumps(true).map_err(|_| AuditError::NonFinite)
    }

    /// Append one line; an error means it was not recorded (fail closed).
    ///
    /// Why: `connect` must not mint a grant it could not log.
    /// What: rotates first, opens `O_APPEND|O_NOFOLLOW` 0600, refuses a
    /// non-private file, writes the whole line in one call.
    /// Test: `audit_lines_replay_byte_identically`, `audit_refuses_a_shared_log`.
    pub fn write(&self, event: &str, fields: &[(&str, Option<Json>)]) -> Result<(), AuditError> {
        let mut line = self.line(event, fields)?;
        line.push('\n');
        self.rotate()?;
        let mut file = private::open_nofollow(&self.path, true)?;
        if !private::private_file(&file.metadata()?) {
            return Err(AuditError::NotPrivate);
        }
        file.write_all(line.as_bytes())?;
        Ok(())
    }

    /// `audit.jsonl` -> `.1` -> `.2` once the live file reaches 5 MiB.
    pub fn rotate(&self) -> io::Result<()> {
        let size = match fs::symlink_metadata(&self.path) {
            Ok(meta) => meta.len(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        if size < ROTATE_BYTES {
            return Ok(());
        }
        for index in (2..=KEEP).rev() {
            let older = self.generation(index - 1);
            if older.exists() {
                fs::rename(&older, self.generation(index))?;
            }
        }
        fs::rename(&self.path, self.generation(1))
    }

    /// The path of rotated generation `index`.
    pub fn generation(&self, index: usize) -> PathBuf {
        let mut name = self.path.as_os_str().to_owned();
        name.push(format!(".{index}"));
        PathBuf::from(name)
    }

    /// Record a revoke, lock or grant end; a failure only goes to stderr.
    pub fn best_effort(&self, event: &str, fields: &[(&str, Option<Json>)]) {
        if let Err(e) = self.write(event, fields) {
            eprintln!("trusty-browser: audit write failed for {event}: {e}");
        }
    }

    /// Log one refusal to stderr and the audit log; never fails.
    ///
    /// Why: owner ruling Dqw 2026-11-16, a refused Connect must say why.
    /// What: one stderr line, then a `refused` line with site, reason, profile,
    /// email domain and `outcome: refused`, in Python's field order.
    /// Test: `audit_lines_replay_byte_identically`.
    pub fn refuse(
        &self,
        site: &str,
        reason: &str,
        profile: Option<&str>,
        email_domain: Option<&str>,
    ) {
        eprintln!(
            "trusty-browser: refused site={site} reason={reason} profile={} domain={}",
            profile.unwrap_or("-"),
            email_domain.unwrap_or("-")
        );
        let fields = [
            ("site", Some(Json::from(site))),
            ("reason", Some(Json::from(reason))),
            ("profile", profile.map(Json::from)),
            ("email_domain", email_domain.map(Json::from)),
            ("outcome", Some(Json::from("refused"))),
        ];
        if let Err(e) = self.write("refused", &fields) {
            eprintln!("trusty-browser: audit write failed for refused: {e}");
        }
    }
}

/// The part of `email` after its last `@`, case-folded, if 1..=253 code points.
///
/// Why: audit lines carry the domain only, never the address.
/// What: Python's `rpartition("@")[2].strip().casefold()`; see [`casefold`].
/// Test: `email_domain_matches_python`.
pub fn email_domain(email: &str) -> Option<String> {
    let (_, domain) = email.rsplit_once('@')?;
    let domain = casefold(crate::casefold::py_strip(domain));
    let n = domain.chars().count();
    (n > 0 && n <= 253).then_some(domain)
}

/// Python `str.casefold`; see [`crate::casefold`].
pub fn casefold(s: &str) -> String {
    crate::casefold::casefold(s)
}
