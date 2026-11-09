//! Architect presence grants: `architect-launch/<pid>.presence` (#8878 PR2a).
//!
//! Why: owner ruling 2026-11-08 makes the bound Architect the operator's twin,
//! and plan.md §3 adds a presence check: a trust-anchor write by the Architect
//! needs proof that Bob is at the Studio. Touch ID cannot run inside a hook,
//! so `tm presence local` authenticates once and leaves a grant the hook reads.
//! What: [`mint`] writes a [`Grant`] for one `claude` (PID and start time),
//! 0600 through a temp file and a rename, sealed with the [`GrantKey`] HMAC.
//! [`check`] accepts it only when the file is a 0600 regular file of this
//! user's (no symlink), parses strictly, carries a valid tag, names the same
//! PID and start time as the live `claude`, uses a known method, and was used
//! within [`IDLE_LIMIT`] (Bob D5b Q2: 120 min). [`refresh`] moves `last_used`
//! forward on each guarded call. Every failure is a [`PresenceRefusal`].
//! FAIL-CLOSED: no key, an unreadable directory or file, a future date, or a
//! tag that does not verify is a refusal; nothing here ever allows.
//! Test: `presence_tests.rs`.

use std::io::Read as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::core::architect_launch::ARCHITECT_DIR;
use crate::core::presence_key::GrantKey;
use crate::core::twin_identity::ClaudeProcess;

/// A grant file's extension.
pub const PRESENCE_EXT: &str = "presence";

/// How long a grant stays valid without use (Bob D5b Q2).
pub const IDLE_LIMIT: Duration = Duration::from_secs(120 * 60);

/// The one method PR2a mints: the Studio's LocalAuthentication.
pub const METHOD_LOCAL: &str = "local";

/// [`refresh`] rewrites a grant at most this often, in seconds.
const REFRESH_EVERY: u64 = 60;

/// Clock skew tolerated for a `last_used` in the future, in seconds.
const SKEW: u64 = 60;

/// The largest grant file read.
const MAX_GRANT_BYTES: u64 = 4 << 10;

/// One presence grant, as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    /// The `claude` the grant is for.
    pub pid: u32,
    /// That process's start time, Unix seconds.
    pub start_time: u64,
    /// How presence was proven: [`METHOD_LOCAL`].
    pub method: String,
    /// When presence was proven, Unix seconds.
    pub granted_at: u64,
    /// When a guarded call last used the grant, Unix seconds.
    pub last_used: u64,
    /// HMAC-SHA256 over every other field, hex.
    pub mac: String,
}

impl Grant {
    /// The sealed bytes: a version tag and every field but `mac`.
    fn message(&self) -> Vec<u8> {
        format!(
            "tm-presence-v1\0{}\0{}\0{}\0{}\0{}",
            self.pid, self.start_time, self.method, self.granted_at, self.last_used
        )
        .into_bytes()
    }

    fn sealed(mut self, key: &GrantKey) -> Self {
        self.mac = key.seal(&self.message());
        self
    }
}

/// Why a grant does not let a call through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PresenceRefusal {
    /// No grant file exists for the `claude`.
    #[error("no presence grant exists for this `claude`")]
    NoGrant,
    /// The anchor directory or the grant file could not be read.
    #[error("the presence grant could not be read")]
    Unreadable,
    /// The grant is a symlink or not a regular file.
    #[error("the presence grant is a symlink or not a regular file")]
    NotRegularFile,
    /// The grant is readable by others or owned by another user.
    #[error("the presence grant is not a 0600 file of this user")]
    UnsafeMode,
    /// The grant does not parse.
    #[error("the presence grant is malformed")]
    Malformed,
    /// The grant key could not be read.
    #[error("the presence key could not be read from the Keychain")]
    NoKey,
    /// The tag does not verify: the grant was not written by tm.
    #[error("the presence grant's seal does not verify (not minted by tm)")]
    Forged,
    /// The grant names another PID.
    #[error("the presence grant was minted for another process")]
    OtherProcess,
    /// The grant's start time differs: the PID was reused.
    #[error("the presence grant's start time differs from this `claude`'s (PID reuse)")]
    StaleStartTime,
    /// The grant names a method tm does not mint.
    #[error("the presence grant names an unknown method")]
    UnknownMethod,
    /// The grant was last used in the future.
    #[error("the presence grant is dated in the future")]
    FutureDated,
    /// The grant has not been used for [`IDLE_LIMIT`].
    #[error("the presence grant expired after 120 idle minutes")]
    Idle,
}

/// `root/architect-launch/<pid>.presence`, `root` being `~/.trusty-mpm`.
pub fn grant_path(root: &Path, pid: u32) -> PathBuf {
    root.join(ARCHITECT_DIR)
        .join(format!("{pid}.{PRESENCE_EXT}"))
}

/// Seconds since the Unix epoch at `now`; 0 before it.
pub fn unix_secs(now: SystemTime) -> u64 {
    now.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Write a grant for `claude`, proven by `method` at `now`, under `root`.
///
/// Why: the one grant writer; `tm presence local` and `tm fleet bind` call it
/// only after LocalAuthentication answered `Granted`.
/// What: a [`Grant`] with `granted_at = last_used = now`, sealed with `key`,
/// written 0600 through a temp file and a rename (a symlink at the path is
/// replaced, never followed). Returns the path.
/// Test: `a_minted_grant_passes_the_check`.
pub fn mint(
    root: &Path,
    claude: &ClaudeProcess,
    method: &str,
    key: &GrantKey,
    now: u64,
) -> Result<PathBuf, String> {
    let grant = Grant {
        pid: claude.pid,
        start_time: claude.start_time,
        method: method.to_owned(),
        granted_at: now,
        last_used: now,
        mac: String::new(),
    }
    .sealed(key);
    write(root, &grant)
}

fn write(root: &Path, grant: &Grant) -> Result<PathBuf, String> {
    let path = grant_path(root, grant.pid);
    let body = serde_json::to_vec(grant).map_err(|e| e.to_string())?;
    crate::core::architect_session::write_owner_only(&path, &body)?;
    Ok(path)
}

/// The grant that lets `claude` through at `now`, or why none does.
///
/// Why/What: see the module doc. `key` runs only once a grant file has been
/// read, so a call with no grant never touches the Keychain.
/// Test: `a_minted_grant_passes_the_check`, `a_grant_copied_to_another_pid_is_refused`,
/// `a_grant_with_a_stale_start_time_is_refused`,
/// `a_forged_grant_without_the_key_is_refused`, `a_symlinked_grant_is_refused`,
/// `a_grant_idle_for_121_minutes_is_refused`, `a_malformed_or_open_grant_is_refused`.
pub fn check(
    root: &Path,
    claude: &ClaudeProcess,
    key: impl FnOnce() -> Option<GrantKey>,
    now: u64,
) -> Result<Grant, PresenceRefusal> {
    let raw = read_grant(root, claude.pid)?;
    let grant: Grant = serde_json::from_slice(&raw).map_err(|_| PresenceRefusal::Malformed)?;
    let key = key().ok_or(PresenceRefusal::NoKey)?;
    if !key.verify(&grant.message(), &grant.mac) {
        return Err(PresenceRefusal::Forged);
    }
    if grant.pid != claude.pid {
        return Err(PresenceRefusal::OtherProcess);
    }
    if grant.start_time != claude.start_time {
        return Err(PresenceRefusal::StaleStartTime);
    }
    if grant.method != METHOD_LOCAL {
        return Err(PresenceRefusal::UnknownMethod);
    }
    if grant.last_used > now + SKEW || grant.granted_at > grant.last_used {
        return Err(PresenceRefusal::FutureDated);
    }
    if now.saturating_sub(grant.last_used) > IDLE_LIMIT.as_secs() {
        return Err(PresenceRefusal::Idle);
    }
    Ok(grant)
}

/// Move a checked `grant`'s `last_used` to `now`, resealed; at most once a
/// minute.
///
/// Test: `a_used_grant_is_refreshed`.
pub fn refresh(root: &Path, grant: &Grant, key: &GrantKey, now: u64) -> Result<(), String> {
    if now.saturating_sub(grant.last_used) < REFRESH_EVERY {
        return Ok(());
    }
    let next = Grant {
        last_used: now,
        ..grant.clone()
    }
    .sealed(key);
    write(root, &next).map(drop)
}

/// The grant file's bytes, read without following a symlink.
fn read_grant(root: &Path, pid: u32) -> Result<Vec<u8>, PresenceRefusal> {
    let dir = root.join(ARCHITECT_DIR);
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) if meta.is_dir() => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(PresenceRefusal::NoGrant);
        }
        _ => return Err(PresenceRefusal::Unreadable),
    }
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(grant_path(root, pid))
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(PresenceRefusal::NoGrant);
        }
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
            return Err(PresenceRefusal::NotRegularFile);
        }
        Err(_) => return Err(PresenceRefusal::Unreadable),
    };
    let meta = file.metadata().map_err(|_| PresenceRefusal::Unreadable)?;
    if !meta.is_file() {
        return Err(PresenceRefusal::NotRegularFile);
    }
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    if meta.mode() & 0o077 != 0 || meta.uid() != euid {
        return Err(PresenceRefusal::UnsafeMode);
    }
    let mut raw = Vec::new();
    file.take(MAX_GRANT_BYTES)
        .read_to_end(&mut raw)
        .map_err(|_| PresenceRefusal::Unreadable)?;
    Ok(raw)
}

#[cfg(test)]
#[path = "presence_tests.rs"]
mod tests;
