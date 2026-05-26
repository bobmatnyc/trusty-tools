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

use serde::{Deserialize, Serialize};

/// File name of the pairing record under the framework root.
const PAIRING_FILE: &str = "pairing.json";

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
}
