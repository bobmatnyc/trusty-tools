//! Pending presence requests: `architect-launch/pending/<id>.json`
//! (#8878 PR2b-1a).
//!
//! Why: plan-pr2-devices.md §3 — a device approves one request for one
//! `claude`, never presence in general. The request records which `claude`
//! (PID and start time) and which action asked, and expires after the
//! configured TTL (Bob D5b Q2: 5 min). Architect Q3: the daemon keeps the
//! request record and checks it at verify; Q8: at most [`PENDING_CAP`] live
//! requests and one per PID per [`RATE_WINDOW`].
//! What: [`create`] writes a sealed [`Pending`] 0600; [`load`] reads one
//! fail-closed (seal, id, dates); [`claim`] consumes it by an atomic rename,
//! so one request yields at most one grant. The action text comes from the
//! caller and is always shown with [`UNVERIFIED_ORIGIN`] (Architect Q6).
//! Test: `presence_request_tests.rs`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::core::architect_launch::ARCHITECT_DIR;
use crate::core::presence_key::GrantKey;
use crate::core::presence_store::{
    FileFault, random_hex, read_owner_only, sealed_message, valid_token,
};

/// The pending-request directory, inside the anchor directory.
pub const PENDING_DIR: &str = "pending";

/// The most live requests kept at once (Architect Q8).
pub const PENDING_CAP: usize = 16;

/// One request per PID per this window (Architect Q8).
pub const RATE_WINDOW: Duration = Duration::from_secs(120);

/// The label every caller-supplied action text carries (Architect Q6).
pub const UNVERIFIED_ORIGIN: &str = "unverified origin";

/// The longest action text accepted, in bytes.
pub const MAX_ACTION: usize = 1024;

/// The longest project or session label accepted, in bytes.
pub const MAX_LABEL: usize = 256;

/// The longest TTL a request may carry.
pub const MAX_TTL: Duration = Duration::from_secs(30 * 60);

/// The largest request file read.
const MAX_PENDING_BYTES: u64 = 8 << 10;

/// A request id: 128 random bits, hex.
const ID_LEN: usize = 32;

/// The seal's record tag.
const TAG: &str = "tm-presence-request-v1";

/// What a caller asks presence for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRequest {
    /// The `claude` that needs presence.
    pub pid: u32,
    /// That process's start time, Unix seconds.
    pub start_time: u64,
    /// The project label.
    pub project: String,
    /// The session label.
    pub session: String,
    /// What the call wants to do, as the caller describes it.
    pub action: String,
}

/// One pending request, as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pending {
    /// 128 random bits, hex; also the file stem.
    pub id: String,
    /// The `claude` the grant would be for.
    pub pid: u32,
    /// That process's start time, Unix seconds.
    pub start_time: u64,
    /// The project label.
    pub project: String,
    /// The session label.
    pub session: String,
    /// The caller's action text; see [`Pending::action_label`].
    pub action: String,
    /// SHA-256 of `action`, hex.
    pub action_hash: String,
    /// When the request was made, Unix seconds.
    pub created: u64,
    /// When it stops being approvable, Unix seconds.
    pub expires: u64,
    /// HMAC-SHA256 over every other field, hex.
    pub mac: String,
}

impl Pending {
    fn message(&self) -> Vec<u8> {
        sealed_message(
            TAG,
            &[
                &self.id,
                &self.pid.to_string(),
                &self.start_time.to_string(),
                &self.project,
                &self.session,
                &self.action,
                &self.action_hash,
                &self.created.to_string(),
                &self.expires.to_string(),
            ],
        )
    }

    /// The action text as any approver sees it: always labelled
    /// [`UNVERIFIED_ORIGIN`], because the caller wrote it (Architect Q6).
    ///
    /// Test: `the_action_text_is_labelled_unverified_origin_8878`.
    pub fn action_label(&self) -> String {
        format!("{} ({UNVERIFIED_ORIGIN})", self.action)
    }
}

/// SHA-256 of an action text, hex.
pub fn action_hash(action: &str) -> String {
    hex::encode(Sha256::digest(action.as_bytes()))
}

/// Why a request was refused or could not be stored.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RequestError {
    /// The id is not 32 hex characters.
    #[error("the request id is malformed")]
    InvalidId,
    /// A label or the action text is too long, or the TTL is out of range.
    #[error("the request is out of bounds: {0}")]
    OutOfBounds(&'static str),
    /// [`PENDING_CAP`] requests are already pending.
    #[error("{PENDING_CAP} presence requests are already pending")]
    Full,
    /// This PID asked within [`RATE_WINDOW`].
    #[error("this `claude` already asked for presence in the last two minutes")]
    RateLimited,
    /// No request has this id (never made, expired and removed, or claimed).
    #[error("no pending request has this id")]
    NotFound,
    /// The request file could not be read safely.
    #[error("the pending request is unusable: {0}")]
    Unsafe(FileFault),
    /// The request does not parse or names another id.
    #[error("the pending request is malformed")]
    Malformed,
    /// The seal does not verify: the request was not written by tm.
    #[error("the pending request's seal does not verify (not written by tm)")]
    Forged,
    /// The request expired.
    #[error("the pending request expired")]
    Expired,
    /// The request is dated after now, or valid longer than [`MAX_TTL`].
    #[error("the pending request's dates are out of range")]
    BadDates,
    /// The request changed or vanished between its check and its claim.
    #[error("the pending request was claimed or changed by another caller")]
    Claimed,
    /// A write or removal failed.
    #[error("the pending request could not be written: {0}")]
    Write(String),
}

/// `root/architect-launch/pending`.
pub fn pending_dir(root: &Path) -> PathBuf {
    root.join(ARCHITECT_DIR).join(PENDING_DIR)
}

/// The file path for request `id`, or [`RequestError::InvalidId`].
pub fn pending_path(root: &Path, id: &str) -> Result<PathBuf, RequestError> {
    if id.len() != ID_LEN || !valid_token(id, ID_LEN) || !id.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(RequestError::InvalidId);
    }
    Ok(pending_dir(root).join(format!("{id}.json")))
}

/// Make a request for `new` at `now`, valid for `ttl`.
///
/// Why/What: see the module doc. Expired requests are removed first. Any
/// other request file, readable or not, counts toward [`PENDING_CAP`]; a
/// readable one for the same PID made within [`RATE_WINDOW`] refuses.
/// Test: `a_request_round_trips_sealed_8878`, `the_pending_cap_is_16_8878`,
/// `one_request_per_pid_per_two_minutes_8878`.
pub fn create(
    root: &Path,
    key: &GrantKey,
    new: NewRequest,
    ttl: Duration,
    now: u64,
) -> Result<Pending, RequestError> {
    if new.action.len() > MAX_ACTION
        || new.project.len() > MAX_LABEL
        || new.session.len() > MAX_LABEL
    {
        return Err(RequestError::OutOfBounds(
            "a label or the action text is too long",
        ));
    }
    if ttl.is_zero() || ttl > MAX_TTL {
        return Err(RequestError::OutOfBounds(
            "the TTL is zero or over 30 minutes",
        ));
    }
    let mut live = 0usize;
    for (id, found) in scan(root, key, now)? {
        match found {
            Err(RequestError::Expired) => {
                if let Ok(path) = pending_path(root, &id) {
                    let _ = std::fs::remove_file(path);
                }
            }
            Ok(p) if p.pid == new.pid && now.saturating_sub(p.created) < RATE_WINDOW.as_secs() => {
                return Err(RequestError::RateLimited);
            }
            _ => live += 1,
        }
    }
    if live >= PENDING_CAP {
        return Err(RequestError::Full);
    }
    let mut pending = Pending {
        id: random_hex::<16>().map_err(RequestError::Write)?,
        pid: new.pid,
        start_time: new.start_time,
        project: new.project,
        session: new.session,
        action_hash: action_hash(&new.action),
        action: new.action,
        created: now,
        expires: now + ttl.as_secs(),
        mac: String::new(),
    };
    pending.mac = key.seal(&pending.message());
    let path = pending_path(root, &pending.id)?;
    let body = serde_json::to_vec(&pending).map_err(|e| RequestError::Write(e.to_string()))?;
    crate::core::architect_session::write_owner_only(&path, &body).map_err(RequestError::Write)?;
    Ok(pending)
}

/// One [`scan`] row: a request id and its request or why it is unusable.
type Scanned = (String, Result<Pending, RequestError>);

/// Every request file's id and its request or the reason it is unusable.
fn scan(root: &Path, key: &GrantKey, now: u64) -> Result<Vec<Scanned>, RequestError> {
    let entries = match std::fs::read_dir(pending_dir(root)) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(RequestError::Write(e.to_string())),
    };
    Ok(entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|name| name.strip_suffix(".json").map(str::to_owned))
        .map(|id| {
            let found = load(root, key, &id, now);
            (id, found)
        })
        .collect())
}

/// The live request `id` at `now`, read fail-closed.
///
/// Why/What: a symlink, a non-0600 or oversized file, an unparseable one, a
/// bad seal, an id that differs from the file name, a request made after
/// `now`, valid longer than [`MAX_TTL`], or past its expiry is refused.
/// Reading never removes or changes the file.
/// Test: `pending_symlink_oversized_or_malformed_refuses_and_is_not_consumed_into_a_grant_8878`,
/// `clock_before_created_at_refuses_8878`, `an_expired_request_is_refused_8878`.
pub fn load(root: &Path, key: &GrantKey, id: &str, now: u64) -> Result<Pending, RequestError> {
    let path = pending_path(root, id)?;
    let anchor = root.join(ARCHITECT_DIR);
    let dir = pending_dir(root);
    let raw = match read_owner_only(&[&anchor, &dir], &path, MAX_PENDING_BYTES) {
        Ok(raw) => raw,
        Err(FileFault::Missing) => return Err(RequestError::NotFound),
        Err(fault) => return Err(RequestError::Unsafe(fault)),
    };
    let pending: Pending = serde_json::from_slice(&raw).map_err(|_| RequestError::Malformed)?;
    if !key.verify(&pending.message(), &pending.mac) {
        return Err(RequestError::Forged);
    }
    if pending.id != id || pending.action_hash != action_hash(&pending.action) {
        return Err(RequestError::Malformed);
    }
    if now < pending.created
        || pending.expires < pending.created
        || pending.expires - pending.created > MAX_TTL.as_secs()
    {
        return Err(RequestError::BadDates);
    }
    if now >= pending.expires {
        return Err(RequestError::Expired);
    }
    Ok(pending)
}

/// Consume `pending`: rename its file aside, confirm the bytes still hold
/// the same sealed request, and remove it.
///
/// Why: plan-pr2b1.md §5 Fail-Open 6 — two verifies racing for one request
/// mint exactly one grant; the rename succeeds for one caller only.
/// What: `<id>.json` is renamed to `<id>.claimed`; a missing source is
/// [`RequestError::Claimed`]. The renamed file is re-read and must equal
/// `pending` under `key`; it is removed either way, so a claimed request can
/// never be submitted again.
/// Test: `two_concurrent_verifies_for_one_request_mint_exactly_one_grant_8878`.
pub fn claim(root: &Path, key: &GrantKey, pending: &Pending) -> Result<(), RequestError> {
    let path = pending_path(root, &pending.id)?;
    let claimed = path.with_extension("claimed");
    match std::fs::rename(&path, &claimed) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(RequestError::Claimed),
        Err(e) => return Err(RequestError::Write(format!("{}: {e}", path.display()))),
    }
    let anchor = root.join(ARCHITECT_DIR);
    let dir = pending_dir(root);
    let same = read_owner_only(&[&anchor, &dir], &claimed, MAX_PENDING_BYTES)
        .ok()
        .and_then(|raw| serde_json::from_slice::<Pending>(&raw).ok())
        .is_some_and(|found| found == *pending && key.verify(&found.message(), &found.mac));
    if let Err(e) = std::fs::remove_file(&claimed) {
        tracing::warn!("#8878: a claimed presence request was not removed: {e}");
    }
    if same {
        Ok(())
    } else {
        Err(RequestError::Claimed)
    }
}

#[cfg(test)]
#[path = "presence_request_tests.rs"]
mod tests;
