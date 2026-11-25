//! Enrolled presence devices: `architect-launch/devices/<credential>.json`
//! (#8878 PR2b-1a).
//!
//! Why: plan-pr2-devices.md §4 — an approval from an enrolled device (a
//! passkey on Bob's iPhone or Mac) proves presence like the Studio's Touch ID.
//! The store says which credentials count. A same-uid writer could drop its
//! own credential file, so every record is HMAC-sealed with the presence
//! [`GrantKey`] (plan-pr2b1.md §3), and enrol and revoke need a
//! local-presence [`Ticket`].
//! What: [`Device`] is one credential: its id, a name, the verifier's public
//! key material (opaque here), dates, the signature counter and the backup
//! flags. [`load`] reads one record fail-closed; [`enrol`] and [`revoke`]
//! check a ticket first; [`record_use`] persists the counter after a
//! verified assertion, re-reading the record under [`lock_store`]. A revoked
//! device has no record: [`revoke`] deletes it first, then every grant that
//! names it, under the same lock, so a verify in flight cannot write the
//! record back or mint after the sweep; a grant whose sweep failed is still
//! refused by `presence::check`, which re-reads the record.
//! Test: `presence_device_tests.rs`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::architect_launch::ARCHITECT_DIR;
use crate::core::presence::{Grant, PRESENCE_EXT};
use crate::core::presence_enrol::{Ticket, TicketPurpose, TicketRefusal, check_ticket};
use crate::core::presence_key::GrantKey;
use crate::core::presence_store::{FileFault, read_owner_only, sealed_message, valid_token};

/// The device directory, inside the anchor directory.
pub const DEVICES_DIR: &str = "devices";

/// The grant method prefix of a device approval: `device:<credential id>`.
pub const METHOD_DEVICE_PREFIX: &str = "device:";

/// The longest credential id accepted (base64url of a 1023-byte id fits).
pub const MAX_CREDENTIAL_ID: usize = 1400;

/// The longest device name accepted, in bytes.
pub const MAX_NAME: usize = 64;

/// The largest device record read.
const MAX_RECORD_BYTES: u64 = 16 << 10;

/// The seal's record tag.
const TAG: &str = "tm-presence-device-v1";

/// The store lock, inside the device directory (not a `.json`, so [`list`]
/// skips it).
const LOCK_FILE: &str = ".lock";

/// One enrolled device, as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    /// The credential id, base64url; also the file stem.
    pub credential_id: String,
    /// The name Bob gave the device.
    pub name: String,
    /// The verifier's public key material, opaque to the store.
    pub public_key: String,
    /// When the device was enrolled, Unix seconds.
    pub created: u64,
    /// When an assertion from it last verified, Unix seconds.
    pub last_used: u64,
    /// The authenticator's signature counter at that assertion.
    pub sign_count: u32,
    /// The credential may be backed up (a synced passkey).
    pub backup_eligible: bool,
    /// The credential is backed up.
    pub backup_state: bool,
    /// HMAC-SHA256 over every other field, hex.
    pub mac: String,
}

impl Device {
    fn message(&self) -> Vec<u8> {
        sealed_message(
            TAG,
            &[
                &self.credential_id,
                &self.name,
                &self.public_key,
                &self.created.to_string(),
                &self.last_used.to_string(),
                &self.sign_count.to_string(),
                if self.backup_eligible { "1" } else { "0" },
                if self.backup_state { "1" } else { "0" },
            ],
        )
    }

    fn sealed(mut self, key: &GrantKey) -> Self {
        self.mac = key.seal(&self.message());
        self
    }

    /// The grant method a verified assertion from this device mints.
    pub fn method(&self) -> String {
        format!("{METHOD_DEVICE_PREFIX}{}", self.credential_id)
    }
}

/// What a verifier hands the store when a registration completes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDevice {
    /// The credential id, base64url.
    pub credential_id: String,
    /// The name Bob gave the device.
    pub name: String,
    /// The verifier's public key material.
    pub public_key: String,
    /// The counter the registration reported.
    pub sign_count: u32,
    /// The backup-eligible flag.
    pub backup_eligible: bool,
    /// The backup-state flag.
    pub backup_state: bool,
}

/// Why a device operation was refused or failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeviceError {
    /// The credential id is not a safe base64url token.
    #[error("the credential id is not a base64url token of at most {MAX_CREDENTIAL_ID} bytes")]
    InvalidId,
    /// The name is empty, too long or holds control characters.
    #[error("the device name must be 1 to {MAX_NAME} bytes with no control characters")]
    InvalidName,
    /// No record exists for the credential: unknown or revoked.
    #[error("no enrolled device has this credential (unknown or revoked)")]
    Unknown,
    /// A record already exists for the credential.
    #[error("a device with this credential is already enrolled")]
    Exists,
    /// The record could not be read safely.
    #[error("the device record is unusable: {0}")]
    Unsafe(FileFault),
    /// The record does not parse or names another credential.
    #[error("the device record is malformed")]
    Malformed,
    /// The seal does not verify: the record was not written by tm.
    #[error("the device record's seal does not verify (not written by tm)")]
    Forged,
    /// The local-presence ticket was refused.
    #[error("the local-presence ticket was refused: {0}")]
    Ticket(TicketRefusal),
    /// A write or removal failed.
    #[error("the device store could not be written: {0}")]
    Write(String),
    /// The counter did not move past the stored one (#8878: a concurrent
    /// verify saved a higher one first).
    #[error("the signature counter did not increase past the stored one")]
    CounterRegressed,
    /// #8878: revoke removed the record and swept grants without the store
    /// lock, so a verify already holding it may still mint a grant.
    #[error(
        "the device is removed, but the device store lock could not be taken ({0}); \
         a verify in flight may still mint a grant: run revoke again"
    )]
    Unlocked(String),
}

/// `root/architect-launch/devices`.
pub fn devices_dir(root: &Path) -> PathBuf {
    root.join(ARCHITECT_DIR).join(DEVICES_DIR)
}

/// The record path for `credential_id`, or [`DeviceError::InvalidId`].
pub fn device_path(root: &Path, credential_id: &str) -> Result<PathBuf, DeviceError> {
    if !valid_token(credential_id, MAX_CREDENTIAL_ID) {
        return Err(DeviceError::InvalidId);
    }
    Ok(devices_dir(root).join(format!("{credential_id}.json")))
}

/// The enrolled device for `credential_id`, read fail-closed.
///
/// Why/What: a symlink, a non-0600 file, an oversized or unparseable record,
/// one whose `credential_id` differs from its file name, or one whose seal
/// does not verify under `key` is refused; a missing one is
/// [`DeviceError::Unknown`].
/// Test: `a_forged_device_record_with_a_bad_seal_is_refused_8878`,
/// `unreadable_or_malformed_device_record_refuses_8878`,
/// `device_record_swapped_for_symlink_refuses_8878`.
pub fn load(root: &Path, key: &GrantKey, credential_id: &str) -> Result<Device, DeviceError> {
    let path = device_path(root, credential_id)?;
    let anchor = root.join(ARCHITECT_DIR);
    let dir = devices_dir(root);
    let raw = match read_owner_only(&[&anchor, &dir], &path, MAX_RECORD_BYTES) {
        Ok(raw) => raw,
        Err(FileFault::Missing) => return Err(DeviceError::Unknown),
        Err(fault) => return Err(DeviceError::Unsafe(fault)),
    };
    let device: Device = serde_json::from_slice(&raw).map_err(|_| DeviceError::Malformed)?;
    if !key.verify(&device.message(), &device.mac) {
        return Err(DeviceError::Forged);
    }
    if device.credential_id != credential_id {
        return Err(DeviceError::Malformed);
    }
    Ok(device)
}

/// Enrol `new` at `now`, authorised by an [`TicketPurpose::Enrol`] ticket
/// for its name.
///
/// Why: plan-pr2b1.md §1 — no device record without a fresh Studio
/// presence; Q4 — before PR2b-2 only tests complete a registration.
/// What: checks the ticket, the id and the name, refuses an existing record,
/// spends the ticket under the store lock (#8878: single-use), then writes
/// the sealed record 0600 through a temp file and a rename.
/// Test: `an_enrolled_device_loads_back_sealed_8878`,
/// `register_start_with_no_ticket_is_refused_8878`, `an_enrol_ticket_enrols_once_8878`.
pub fn enrol(
    root: &Path,
    key: &GrantKey,
    ticket: &Ticket,
    new: NewDevice,
    now: u64,
) -> Result<Device, DeviceError> {
    check_ticket(key, ticket, TicketPurpose::Enrol, &new.name, now).map_err(DeviceError::Ticket)?;
    let path = device_path(root, &new.credential_id)?;
    if new.name.is_empty() || new.name.len() > MAX_NAME || new.name.chars().any(char::is_control) {
        return Err(DeviceError::InvalidName);
    }
    if std::fs::symlink_metadata(&path).is_ok() {
        return Err(DeviceError::Exists);
    }
    // #8878 (1a review): an enrol ticket is single-use within its TTL.
    std::fs::create_dir_all(devices_dir(root)).map_err(|e| DeviceError::Write(e.to_string()))?;
    let _lock = lock_store(root)?;
    spend_ticket(root, ticket, now)?;
    let device = Device {
        credential_id: new.credential_id,
        name: new.name,
        public_key: new.public_key,
        created: now,
        last_used: now,
        sign_count: new.sign_count,
        backup_eligible: new.backup_eligible,
        backup_state: new.backup_state,
        mac: String::new(),
    }
    .sealed(key);
    write(root, &device)?;
    Ok(device)
}

/// The prefix of a spent enrol ticket's marker in the device directory.
const SPENT_PREFIX: &str = ".ticket-";

/// Mark `ticket` spent, under the store lock; a marker already present is
/// [`TicketRefusal::Spent`].
///
/// Why: #8878 (PR #10884 review) — a ticket that enrolled one device must not
/// enrol a second within its TTL.
/// What: `devices/.ticket-<nonce>` holds the ticket's expiry and is written
/// before the record, so a failed record write leaves the ticket spent
/// (fail-closed). A marker is removed only when its expiry reads, parses and
/// is below `now`; an unreadable or unparsable marker is kept.
/// Test: `an_enrol_ticket_enrols_once_8878`,
/// `an_unreadable_spent_marker_keeps_the_ticket_spent_8878`.
fn spend_ticket(root: &Path, ticket: &Ticket, now: u64) -> Result<(), DeviceError> {
    if !valid_token(&ticket.nonce, 64) {
        return Err(DeviceError::Ticket(TicketRefusal::Forged));
    }
    let dir = devices_dir(root);
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let name = entry.file_name();
        // #8878: fail closed — a read or parse failure keeps the marker.
        let expired = name.to_str().is_some_and(|n| n.starts_with(SPENT_PREFIX))
            && std::fs::read_to_string(entry.path())
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .is_some_and(|expires| expires < now);
        if expired {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let marker = dir.join(format!("{SPENT_PREFIX}{}", ticket.nonce));
    if std::fs::symlink_metadata(&marker).is_ok() {
        return Err(DeviceError::Ticket(TicketRefusal::Spent));
    }
    crate::core::architect_session::write_owner_only(
        &marker,
        ticket.expires_at.to_string().as_bytes(),
    )
    .map_err(DeviceError::Write)
}

/// An exclusive hold on the device store; dropping it releases the lock.
#[derive(Debug)]
pub struct StoreLock {
    _file: std::fs::File,
}

/// Take the device store's exclusive lock, waiting for any other holder.
///
/// Why: #8878 — a revoke that runs while a verify is in flight must win: the
/// verify must neither write the record back nor mint after the sweep, and
/// two verifies on one device must not save their counters out of order.
/// What: opens `devices/.lock` (0600, no symlink) and takes an exclusive
/// `flock`, which also serialises threads in one process and is released if
/// the holder dies. A missing device directory is [`DeviceError::Unknown`];
/// any other failure is [`DeviceError::Write`].
/// Test: `revoke_during_verify_does_not_resurrect_the_device_8878`,
/// `concurrent_verifies_never_move_the_sign_count_backwards_8878`.
pub fn lock_store(root: &Path) -> Result<StoreLock, DeviceError> {
    let path = devices_dir(root).join(LOCK_FILE);
    try_lock(&path).map_err(|e| lock_error(&path, &e))
}

/// Open `path` (0600, no symlink) and take its exclusive `flock`.
fn try_lock(path: &Path) -> std::io::Result<StoreLock> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.lock()?;
    Ok(StoreLock { _file: file })
}

/// Map a [`try_lock`] failure: a missing directory is [`DeviceError::Unknown`].
fn lock_error(path: &Path, e: &std::io::Error) -> DeviceError {
    if e.kind() == std::io::ErrorKind::NotFound {
        DeviceError::Unknown
    } else {
        DeviceError::Write(format!("{}: {e}", path.display()))
    }
}

/// Persist a verified assertion's counter and backup state on the device
/// `credential_id`, under `_lock`.
///
/// Why: plan-pr2b1.md §5 Fail-Open 3 — a counter that is not persisted lets
/// the same assertion verify again, so a failed write refuses the grant.
/// What: re-reads the record under the lock (#8878): a record revoked since
/// the verifier ran is [`DeviceError::Unknown`] and is not written back; a
/// `sign_count` not above the stored one (0 on 0 allowed) is
/// [`DeviceError::CounterRegressed`], so the stored counter never moves
/// backwards.
/// Test: `sign_count_persist_failure_refuses_the_grant_8878`,
/// `revoke_during_verify_does_not_resurrect_the_device_8878`,
/// `concurrent_verifies_never_move_the_sign_count_backwards_8878`.
pub fn record_use(
    root: &Path,
    key: &GrantKey,
    _lock: &StoreLock,
    credential_id: &str,
    sign_count: u32,
    backup_state: bool,
    now: u64,
) -> Result<Device, DeviceError> {
    // #8878: the copy the verifier saw may be stale; the disk under the lock wins.
    let current = load(root, key, credential_id)?;
    let both_zero = sign_count == 0 && current.sign_count == 0;
    if sign_count <= current.sign_count && !both_zero {
        return Err(DeviceError::CounterRegressed);
    }
    let next = Device {
        sign_count,
        backup_state,
        last_used: now.max(current.last_used),
        ..current
    }
    .sealed(key);
    write(root, &next)?;
    Ok(next)
}

/// One [`list`] row: a credential id and its record or why it is unusable.
pub type Listed = (String, Result<Device, DeviceError>);

/// Every enrolled device's id and its record or the reason it is unusable.
///
/// Test: `an_enrolled_device_loads_back_sealed_8878`.
pub fn list(root: &Path, key: &GrantKey) -> Result<Vec<Listed>, DeviceError> {
    let entries = match std::fs::read_dir(devices_dir(root)) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(DeviceError::Write(e.to_string())),
    };
    let mut out: Vec<_> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            name.strip_suffix(".json").map(str::to_owned)
        })
        .map(|id| {
            let device = load(root, key, &id);
            (id, device)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// What a revoke removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revoked {
    /// The grant files that named the device and were removed.
    pub grants_removed: Vec<PathBuf>,
}

/// Revoke `credential_id`, authorised by a [`TicketPurpose::Revoke`] ticket
/// for it.
///
/// Why: plan-pr2-devices.md §4 — a stolen device's grant dies at once, not at
/// the 120-minute idle edge.
/// What: checks the ticket, takes the store lock (#8878: a verify holding it
/// finishes first and its grant is swept; a later verify finds no record),
/// deletes the record (the kill switch: a device grant is refused without
/// it), then deletes every `*.presence` whose method names the credential.
/// An interrupted lock is retried once. A lock that still cannot be taken
/// does not stop the delete and sweep, but the call is then
/// [`DeviceError::Unlocked`]: a verify already holding the lock can still
/// mint after the sweep, so the operator runs revoke again. A grant that
/// cannot be read or removed is [`DeviceError::Write`] after the sweep; the
/// record is already gone, so the grant is refused anyway. An unknown
/// credential is [`DeviceError::Unknown`] after the sweep.
/// Test: `revoke_kills_a_live_grant_8878`, `revoke_with_no_ticket_is_refused_8878`,
/// `revoke_partial_failure_is_an_error_and_check_still_refuses_8878`,
/// `revoke_without_the_store_lock_is_an_error_8878`,
/// `revoke_waits_for_the_store_lock_and_sweeps_a_later_grant_8878`.
pub fn revoke(
    root: &Path,
    key: &GrantKey,
    ticket: &Ticket,
    credential_id: &str,
    now: u64,
) -> Result<Revoked, DeviceError> {
    check_ticket(key, ticket, TicketPurpose::Revoke, credential_id, now)
        .map_err(DeviceError::Ticket)?;
    let path = device_path(root, credential_id)?;
    // #8878: held until the sweep ends, so no verify mints in between. A
    // missing store has no record to race; any other failure is reported.
    let lock_path = devices_dir(root).join(LOCK_FILE);
    let locked = match try_lock(&lock_path) {
        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => try_lock(&lock_path),
        other => other,
    };
    let (_lock, unlocked) = match locked {
        Ok(lock) => (Some(lock), None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (None, None),
        Err(e) => {
            tracing::warn!("#8878: revoking without the device store lock: {e}");
            (None, Some(format!("{}: {e}", lock_path.display())))
        }
    };
    let known = match std::fs::remove_file(&path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(DeviceError::Write(format!("{}: {e}", path.display()))),
    };
    let method = format!("{METHOD_DEVICE_PREFIX}{credential_id}");
    let (grants_removed, failures) = sweep_grants(root, &method);
    if !failures.is_empty() {
        return Err(DeviceError::Write(format!(
            "the device record is removed, but these grants were not: {}",
            failures.join("; ")
        )));
    }
    if let Some(cause) = unlocked {
        return Err(DeviceError::Unlocked(cause));
    }
    if !known {
        return Err(DeviceError::Unknown);
    }
    Ok(Revoked { grants_removed })
}

/// Remove every grant naming `method`; returns (removed, failures).
fn sweep_grants(root: &Path, method: &str) -> (Vec<PathBuf>, Vec<String>) {
    let anchor = root.join(ARCHITECT_DIR);
    let mut removed = Vec::new();
    let mut failures = Vec::new();
    let entries = match std::fs::read_dir(&anchor) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (removed, failures),
        Err(e) => {
            failures.push(format!("{}: {e}", anchor.display()));
            return (removed, failures);
        }
    };
    for path in entries.flatten().map(|e| e.path()) {
        if path.extension().is_none_or(|e| e != PRESENCE_EXT) {
            continue;
        }
        let names_it = match read_owner_only(&[&anchor], &path, 4 << 10) {
            Ok(raw) => serde_json::from_slice::<Grant>(&raw).is_ok_and(|g| g.method == method),
            // A file that is not a readable grant is refused by `check` anyway.
            Err(_) => false,
        };
        if !names_it {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => removed.push(path),
            Err(e) => failures.push(format!("{}: {e}", path.display())),
        }
    }
    (removed, failures)
}

fn write(root: &Path, device: &Device) -> Result<(), DeviceError> {
    let path = device_path(root, &device.credential_id)?;
    let body = serde_json::to_vec(device).map_err(|e| DeviceError::Write(e.to_string()))?;
    crate::core::architect_session::write_owner_only(&path, &body).map_err(DeviceError::Write)
}

#[cfg(test)]
#[path = "presence_device_tests.rs"]
mod tests;
