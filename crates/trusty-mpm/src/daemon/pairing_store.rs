//! Persistent Telegram pairing record.
//!
//! Why: the daemon binds a single Telegram `chat_id` after a `/pair` handshake,
//! but that binding lived only in memory — a daemon restart silently dropped it
//! and the operator had to re-pair. The pairing must survive restarts.
//! What: [`PairingRecord`] is the on-disk shape (`chat_id` + `paired_at`);
//! [`load`] reads `~/.trusty-mpm/pairing.json` at startup, [`save`] writes it
//! atomically (write `.tmp`, then rename), and [`clear`] deletes it when the
//! pairing is reset.
//! Test: `cargo test -p trusty-mpm-daemon pairing_store` round-trips a record
//! through save / load / clear against a temp directory.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// File name of the pairing record under the framework root.
const PAIRING_FILE: &str = "pairing.json";

/// File name of the *pending* (not-yet-confirmed) pairing code.
///
/// Why: the live pairing CODE used to live only in the daemon's in-memory
/// `pair_code` mutex (see #1500). That made the mint surface (`tm telegram pair`
/// → `POST /pair/request`) and the confirm surface (the bot's `/pair <code>`)
/// agree ONLY when both happened to hit the very same daemon process. A second
/// daemon instance (e.g. the silent ephemeral-port duplicate from #1499) holds a
/// DIFFERENT empty mutex, so a code minted on one is rejected as "invalid" on the
/// other — which is exactly the symptom operators worked around by pre-seeding
/// `pairing.json`. Persisting the pending code to a single canonical file under
/// the framework root makes it a shared source of truth that survives a restart
/// and is visible to every surface rooted at the same `~/.trusty-mpm`.
const PENDING_PAIR_FILE: &str = "pending_pair.json";

/// The persisted Telegram-pairing record.
///
/// Why: a daemon restart must restore the paired chat without a fresh `/pair`
/// handshake; the record is the minimal state needed to do so.
/// What: the paired Telegram `chat_id` and the ISO-8601 instant it was paired.
/// Test: `save_then_load_round_trips`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingRecord {
    /// The paired Telegram chat id.
    pub chat_id: i64,
    /// ISO-8601 timestamp of when the pairing was confirmed.
    pub paired_at: String,
}

impl PairingRecord {
    /// Build a record for `chat_id` stamped with the current UTC time.
    ///
    /// Why: `POST /pair/confirm` persists the binding the moment it succeeds.
    /// What: pairs `chat_id` with `chrono::Utc::now()` in RFC-3339 form.
    /// Test: `new_stamps_current_time`.
    pub fn new(chat_id: i64) -> Self {
        Self {
            chat_id,
            paired_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

/// Resolve the pairing file path under the given framework root.
///
/// Why: tests must point this at a temp directory rather than the real
/// `~/.trusty-mpm`; production callers pass the home-relative root.
/// What: joins `root/pairing.json`.
/// Test: exercised by `save_then_load_round_trips`.
pub fn pairing_path(root: &Path) -> PathBuf {
    root.join(PAIRING_FILE)
}

/// Load the pairing record from `root/pairing.json`, if present and valid.
///
/// Why: daemon startup restores the paired chat so push alerts keep working
/// across restarts without a fresh handshake.
/// What: returns `Some(record)` when the file exists and parses; `None` when it
/// is absent or malformed (a corrupt file must not abort startup — it is
/// logged and treated as "not paired").
/// Test: `load_missing_is_none`, `save_then_load_round_trips`.
pub fn load(root: &Path) -> Option<PairingRecord> {
    let path = pairing_path(root);
    let contents = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<PairingRecord>(&contents) {
        Ok(record) => Some(record),
        Err(e) => {
            tracing::warn!("ignoring malformed {}: {e}", path.display());
            None
        }
    }
}

/// Atomically write the pairing record to `root/pairing.json`.
///
/// Why: a crash mid-write must never leave a half-written, unparseable file;
/// writing to a `.tmp` sibling and renaming makes the swap atomic.
/// What: ensures `root` exists, serializes `record` to a `.tmp` file, then
/// renames it over the final path.
/// Test: `save_then_load_round_trips`.
pub fn save(root: &Path, record: &PairingRecord) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    let path = pairing_path(root);
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Delete the pairing record from `root/pairing.json`.
///
/// Why: `POST /pair/reset` (or any explicit unpair) must drop the persisted
/// binding so a restart does not resurrect it.
/// What: removes the file; an already-absent file is treated as success.
/// Test: `clear_removes_file`.
pub fn clear(root: &Path) -> std::io::Result<()> {
    let path = pairing_path(root);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// A pending, not-yet-confirmed pairing code persisted to disk.
///
/// Why: see [`PENDING_PAIR_FILE`] — the live code must be a shared source of
/// truth so any daemon instance (or `tm telegram` surface) rooted at the same
/// framework root validates a `/pair <code>` against the SAME outstanding code,
/// rather than each holding a private in-memory copy.
/// What: the six-character code plus the wall-clock instant it was issued, stored
/// as Unix epoch milliseconds so TTL math is portable across processes (an
/// in-memory `std::time::Instant` is not comparable across processes).
/// Test: `pending_round_trips`, `pending_expiry_uses_issued_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingPairCode {
    /// The outstanding one-time pairing code.
    pub code: String,
    /// Unix epoch milliseconds at which the code was issued.
    pub issued_at_ms: u64,
}

impl PendingPairCode {
    /// Build a pending code stamped with the current wall-clock time.
    ///
    /// Why: `POST /pair/request` mints a code and must record WHEN so a later
    /// confirm can enforce the TTL window using a process-portable timestamp.
    /// What: pairs `code` with the current `SystemTime` as epoch milliseconds.
    /// Test: `pending_round_trips`.
    pub fn new(code: String) -> Self {
        Self {
            code,
            issued_at_ms: now_unix_ms(),
        }
    }

    /// True when this code is still within `ttl` of its issue time.
    ///
    /// Why: confirmation must reject codes older than the TTL identically no
    /// matter which surface reads them, so the expiry rule lives here next to
    /// the timestamp it compares against rather than being re-derived per caller.
    /// What: returns `true` when `now - issued_at_ms < ttl`; a clock that has
    /// gone backwards (now < issued_at) is treated as "not expired" (saturating).
    /// Test: `pending_expiry_uses_issued_at`.
    pub fn is_fresh(&self, ttl: Duration) -> bool {
        let elapsed_ms = now_unix_ms().saturating_sub(self.issued_at_ms);
        elapsed_ms < u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX)
    }
}

/// Current Unix time in milliseconds (saturating at the epoch on a skewed clock).
///
/// Why: pending-code TTL is compared across processes, so it needs a wall-clock
/// timestamp rather than a monotonic `Instant`.
/// What: returns `SystemTime::now()` since the Unix epoch in whole milliseconds;
/// a pre-epoch clock yields `0`.
/// Test: exercised by `pending_round_trips` (issued_at is non-zero).
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Resolve the pending-pair file path under the given framework root.
///
/// Why: tests redirect this at a temp directory; production callers pass the
/// home-relative root so every surface agrees on one canonical location.
/// What: joins `root/pending_pair.json`.
/// Test: exercised by `pending_round_trips`.
pub fn pending_path(root: &Path) -> PathBuf {
    root.join(PENDING_PAIR_FILE)
}

/// Persist the outstanding pending pairing code under `root`.
///
/// Why: minting a code must make it visible to every confirm surface rooted at
/// the same framework root, not just the in-memory state of the minting process.
/// What: atomically writes `pending_pair.json` (write `.tmp`, then rename),
/// creating `root` if needed.
/// Test: `pending_round_trips`.
pub fn save_pending(root: &Path, pending: &PendingPairCode) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    let path = pending_path(root);
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(pending)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Load the outstanding pending pairing code from `root`, if present and valid.
///
/// Why: the confirm path reads the shared outstanding code so a code minted by a
/// different surface (or before a restart) still validates.
/// What: returns `Some(pending)` when the file exists and parses; `None` when it
/// is absent or malformed (a corrupt file is logged and treated as "no code").
/// Test: `pending_round_trips`, `load_pending_missing_is_none`.
pub fn load_pending(root: &Path) -> Option<PendingPairCode> {
    let path = pending_path(root);
    let contents = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<PendingPairCode>(&contents) {
        Ok(pending) => Some(pending),
        Err(e) => {
            tracing::warn!("ignoring malformed {}: {e}", path.display());
            None
        }
    }
}

/// Delete the outstanding pending pairing code under `root`.
///
/// Why: a code is single-use — once confirmed (or explicitly invalidated) the
/// shared store must drop it so it can never validate twice.
/// What: removes `pending_pair.json`; an already-absent file is success.
/// Test: `clear_pending_removes_file`.
pub fn clear_pending(root: &Path) -> std::io::Result<()> {
    let path = pending_path(root);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_stamps_current_time() {
        let record = PairingRecord::new(42);
        assert_eq!(record.chat_id, 42);
        // RFC-3339 timestamps always contain a `T` separator.
        assert!(record.paired_at.contains('T'));
    }

    #[test]
    fn load_missing_is_none() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(load(dir.path()).is_none());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().expect("temp dir");
        let record = PairingRecord::new(123_456_789);
        save(dir.path(), &record).expect("save succeeds");
        let loaded = load(dir.path()).expect("record loads");
        assert_eq!(loaded, record);
    }

    #[test]
    fn save_creates_missing_root() {
        let dir = tempfile::tempdir().expect("temp dir");
        let nested = dir.path().join("does/not/exist");
        let record = PairingRecord::new(7);
        save(&nested, &record).expect("save creates the directory");
        assert_eq!(load(&nested), Some(record));
    }

    #[test]
    fn clear_removes_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        save(dir.path(), &PairingRecord::new(1)).expect("save");
        assert!(load(dir.path()).is_some());
        clear(dir.path()).expect("clear succeeds");
        assert!(load(dir.path()).is_none());
        // Clearing an already-absent file is not an error.
        clear(dir.path()).expect("idempotent clear");
    }

    #[test]
    fn load_malformed_is_none() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(pairing_path(dir.path()), "{ not json").expect("write garbage");
        assert!(load(dir.path()).is_none());
    }

    #[test]
    fn pending_round_trips() {
        // A minted code persists to a canonical path and reads back identically,
        // with a non-zero issue timestamp — the basis for cross-process sharing.
        let dir = tempfile::tempdir().expect("temp dir");
        let pending = PendingPairCode::new("ABC123".to_string());
        assert!(pending.issued_at_ms > 0);
        save_pending(dir.path(), &pending).expect("save_pending succeeds");
        let loaded = load_pending(dir.path()).expect("pending loads");
        assert_eq!(loaded, pending);
    }

    #[test]
    fn load_pending_missing_is_none() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(load_pending(dir.path()).is_none());
    }

    #[test]
    fn clear_pending_removes_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        save_pending(dir.path(), &PendingPairCode::new("XYZ999".into())).expect("save");
        assert!(load_pending(dir.path()).is_some());
        clear_pending(dir.path()).expect("clear succeeds");
        assert!(load_pending(dir.path()).is_none());
        // Clearing an already-absent file is not an error.
        clear_pending(dir.path()).expect("idempotent clear");
    }

    #[test]
    fn pending_expiry_uses_issued_at() {
        // A code issued 10 minutes ago is stale under a 5-minute TTL; a code
        // issued "now" is fresh. Expiry is derived from the persisted timestamp
        // so every surface agrees regardless of process-local clocks.
        let ttl = Duration::from_secs(300);
        let fresh = PendingPairCode::new("FRESH1".into());
        assert!(fresh.is_fresh(ttl));

        let stale = PendingPairCode {
            code: "STALE1".into(),
            issued_at_ms: now_unix_ms().saturating_sub(600_000),
        };
        assert!(!stale.is_fresh(ttl));
    }

    #[test]
    fn load_pending_malformed_is_none() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(pending_path(dir.path()), "{ not json").expect("write garbage");
        assert!(load_pending(dir.path()).is_none());
    }
}
