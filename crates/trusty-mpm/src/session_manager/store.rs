//! On-disk JSON persistence for managed session records.
//!
//! Why: the session manager must survive daemon restarts without losing track
//! of which sessions it owns. A simple JSON file at a well-known path gives
//! crash recovery without requiring a database dependency.
//! What: [`SessionStore`] loads and saves a map of [`SessionRecord`]s from/to
//! `~/.trusty-mpm/session-manager/sessions.json`, with async I/O via `tokio::fs`.
//! Test: `store_load_save_round_trip`, `store_upsert_and_get`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs;
use tracing::{debug, warn};

use super::record::{ManagedSessionId, SessionRecord};

/// Errors that can arise from store I/O or serialization.
///
/// Why: callers need structured error information to distinguish transient I/O
/// failures from data-corruption problems and to surface actionable messages
/// to operators.
/// What: one variant per failure mode with human-readable context strings.
/// Test: exercised indirectly through `SessionStore` method tests.
#[derive(Debug, Error)]
pub enum StoreError {
    /// An I/O operation on the backing file failed.
    #[error("session store I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization or deserialization failed.
    #[error("session store serialization error: {0}")]
    Serialize(String),

    /// The requested session id was not found in the store.
    #[error("session not found: {0}")]
    NotFound(String),
}

/// In-memory representation of the serialized store file.
///
/// Why: serde needs a stable top-level shape for the JSON file; wrapping the
/// map in a versioned struct makes future schema migrations possible.
/// What: a flat map from stringified UUID to [`SessionRecord`].
/// Test: round-tripped implicitly by `SessionStore` tests.
#[derive(Debug, Default, Serialize, Deserialize)]
struct StoredData {
    /// All managed sessions, keyed by stringified UUID.
    sessions: HashMap<String, SessionRecord>,
}

/// A cheap freshness fingerprint for the backing file.
///
/// Why: the cross-process reload check (#1219) keys off "did the file change
/// since we last touched it". An mtime alone is insufficient on filesystems
/// whose mtime resolution is coarse (1s on some): two writes in the same second
/// would compare equal and the reader would miss the second write. Pairing the
/// mtime with the byte length catches a same-second write that changed the file
/// size, which a state transition (different JSON length) almost always does.
/// What: the file's last-modified `SystemTime` and its length in bytes. `None`
/// for either component means "unknown" (file absent or metadata unsupported).
/// Test: `store_reload_picks_up_external_write`, `store_reload_noop_when_unchanged`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct FileSig {
    /// File modification time, if the platform/filesystem reports one.
    mtime: Option<SystemTime>,
    /// File length in bytes.
    len: u64,
}

/// Async, file-backed store for [`SessionRecord`]s.
///
/// Why: the session manager must be able to reload all known sessions after a
/// crash or restart so that reconciliation can re-adopt live tmux sessions.
/// Critically (#1219), the daemon and the supervisor run as SEPARATE PROCESSES
/// over the SAME `sessions.json`; without a freshness check the daemon's
/// load-once in-memory map serves stale state forever once the supervisor writes
/// a transition. Tracking the file's mtime lets the store detect another
/// process's write and reload before answering a read, keeping the on-disk file
/// the single source of truth across processes.
/// What: holds the in-memory map, the path to the backing JSON file, and the
/// file modification time observed at the last load/save; all mutations call
/// `save()` immediately to keep the file consistent, and reads first call
/// [`Self::reload_if_changed`] to pick up out-of-process writes.
/// Test: `store_load_save_round_trip`, `store_upsert_and_get`,
/// `store_reload_picks_up_external_write`.
#[derive(Debug)]
pub struct SessionStore {
    /// In-memory sessions map.
    data: StoredData,
    /// Path to the backing `sessions.json` file.
    path: PathBuf,
    /// Freshness fingerprint of `path` as of the last successful load or save.
    ///
    /// Why: comparing this against the file's current fingerprint lets a read
    /// detect that another process (e.g. the supervisor) wrote the file since
    /// this store last touched it, triggering a reload. `None` means the file did
    /// not exist at last observation (a fresh, never-saved store).
    /// What: the (mtime, len) pair captured from the file's metadata.
    /// Test: `store_reload_picks_up_external_write`.
    last_sig: Option<FileSig>,
}

impl SessionStore {
    /// Load (or create) the session store at the given data directory.
    ///
    /// Why: on startup the manager needs to restore state from the previous
    /// run; if no file exists a fresh empty store is returned so the manager
    /// can start cleanly.
    /// What: reads `<data_dir>/sessions.json` if it exists; returns an empty
    /// store if the file is absent; propagates I/O and JSON errors.
    /// Test: `store_load_save_round_trip`.
    pub async fn load(data_dir: &Path) -> Result<Self, StoreError> {
        let path = data_dir.join("sessions.json");
        let (data, last_sig) = Self::read_file(&path).await?;
        Ok(Self {
            data,
            path,
            last_sig,
        })
    }

    /// Stat `path` and return its freshness fingerprint, or `None` if absent.
    ///
    /// Why: both the reload check and `save` need to capture the same (mtime,
    /// len) signature; centralising it keeps "what counts as the file's
    /// identity" in one place.
    /// What: on `metadata` success returns `Some(FileSig { mtime, len })`; on any
    /// stat error (most commonly: file does not exist) returns `None`.
    /// Test: exercised via `store_reload_*` tests.
    async fn sig_of(path: &Path) -> Option<FileSig> {
        let meta = fs::metadata(path).await.ok()?;
        Some(FileSig {
            // `modified()` can be unsupported on exotic filesystems; `None` there
            // makes the (mtime, len) pair compare unequal so reads reload — correct,
            // just less efficient — rather than risk serving stale data.
            mtime: meta.modified().ok(),
            len: meta.len(),
        })
    }

    /// Read and parse the backing file, returning the data and its fingerprint.
    ///
    /// Why: both [`Self::load`] and [`Self::reload_if_changed`] need the exact
    /// same "read the file (or treat absence as empty) and capture its signature"
    /// logic; factoring it out keeps the two callers in lock-step so a reload can
    /// never diverge from an initial load.
    /// What: if `path` exists, reads and JSON-parses it and returns `(data,
    /// Some(sig))`; if absent, returns `(empty, None)`. The signature is read
    /// from the same metadata, so it reflects the bytes just parsed.
    /// Test: `store_reload_picks_up_external_write`, `store_load_save_round_trip`.
    async fn read_file(path: &Path) -> Result<(StoredData, Option<FileSig>), StoreError> {
        match fs::metadata(path).await {
            Ok(meta) => {
                let raw = fs::read_to_string(path).await?;
                let data = serde_json::from_str::<StoredData>(&raw)
                    .map_err(|e| StoreError::Serialize(e.to_string()))?;
                let sig = FileSig {
                    mtime: meta.modified().ok(),
                    len: meta.len(),
                };
                Ok((data, Some(sig)))
            }
            Err(_) => {
                debug!(path = %path.display(), "no session store file found; starting fresh");
                Ok((StoredData::default(), None))
            }
        }
    }

    /// Reload the in-memory map from disk if the backing file changed.
    ///
    /// Why: #1219 — another process (the supervisor) may have written a state
    /// transition to the shared `sessions.json` since this store last touched it.
    /// A read that does not first reconcile with disk serves stale state. The
    /// on-disk file is authoritative across processes, so reads reload from it.
    /// What: stats the file; if its (mtime, len) fingerprint differs from
    /// `last_sig` (covering appeared / disappeared / modified, and same-second
    /// writes that changed the length), re-reads and replaces `data` and
    /// `last_sig`. A matching fingerprint is a fast no-op (one stat, no parse).
    /// This is purely additive over disk state: in-memory writes are always
    /// flushed via `save()` before this store yields its lock, so a reload can
    /// never drop an unpersisted local mutation.
    /// Test: `store_reload_picks_up_external_write`,
    /// `store_reload_noop_when_unchanged`.
    pub async fn reload_if_changed(&mut self) -> Result<(), StoreError> {
        let current = Self::sig_of(&self.path).await;
        // Reload unless the file is present AND its fingerprint exactly matches
        // what we last observed. A `None` on either side (file absent, or never
        // saved) is treated as "changed" so we never miss an external write.
        let unchanged = matches!((current, self.last_sig), (Some(a), Some(b)) if a == b);
        if unchanged {
            return Ok(());
        }
        let (data, sig) = Self::read_file(&self.path).await?;
        self.data = data;
        self.last_sig = sig;
        debug!(path = %self.path.display(), "session store reloaded after external change");
        Ok(())
    }

    /// Persist the current in-memory state to disk atomically.
    ///
    /// Why: every mutating operation must flush to disk so that the store
    /// survives a daemon crash immediately after the mutation. Because the
    /// daemon and the supervisor read this file from SEPARATE processes (#1219),
    /// the write must be atomic — a plain `fs::write` truncates then rewrites, so
    /// a concurrent reader can observe a half-written (unparseable) file. Writing
    /// a temp file and renaming it into place makes the swap atomic on POSIX, so
    /// a cross-process reader always sees either the old or the new file whole.
    /// What: serializes `self.data` to JSON, creates parent directories if
    /// needed, writes a sibling `sessions.json.tmp`, then renames it over the
    /// real path; finally records the resulting mtime so a subsequent
    /// [`Self::reload_if_changed`] treats our own write as "unchanged".
    /// Test: verified indirectly by every mutating store test;
    /// `store_reload_picks_up_external_write` exercises the cross-process path.
    pub async fn save(&mut self) -> Result<(), StoreError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let json = serde_json::to_string_pretty(&self.data)
            .map_err(|e| StoreError::Serialize(e.to_string()))?;
        // Atomic swap: write to a temp sibling then rename over the target so a
        // concurrent reader in another process never sees a torn file.
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, json).await?;
        fs::rename(&tmp, &self.path).await?;
        // Record the fingerprint of the bytes we just wrote so a subsequent
        // `reload_if_changed` treats our own write as "unchanged" and does not
        // pointlessly re-read the file we just authored (#1219).
        self.last_sig = Self::sig_of(&self.path).await;
        debug!(path = %self.path.display(), "session store saved");
        Ok(())
    }

    /// Insert or update a session record.
    ///
    /// Why: session state changes (Started → Active, Active → Dead, etc.) must
    /// be reflected in the store immediately so recovery sees the latest state.
    /// A write must not clobber a concurrent out-of-process write (#1219): if
    /// the supervisor changed OTHER records since this store last loaded, blindly
    /// re-serializing our stale map would lose their changes. Reloading first
    /// applies this single-record mutation on top of the freshest disk state.
    /// What: reloads from disk if the file changed, inserts or replaces the
    /// record keyed by its UUID string, then saves the store to disk.
    /// Test: `store_upsert_and_get`, `store_upsert_preserves_concurrent_write`.
    pub async fn upsert(&mut self, record: SessionRecord) -> Result<(), StoreError> {
        self.reload_if_changed().await?;
        let key = record.id.to_string();
        self.data.sessions.insert(key, record);
        self.save().await
    }

    /// Look up a session record by id, reloading from disk first if it changed.
    ///
    /// Why: the manager's `get()` method needs a typed lookup that returns a
    /// structured not-found error rather than `Option::None`. It must also see
    /// out-of-process writes (#1219): if the supervisor changed the file, the
    /// daemon's lookup has to reflect that, so this reloads-on-read first.
    /// What: calls [`Self::reload_if_changed`], then returns a clone of the
    /// stored record or `StoreError::NotFound`. Takes `&mut self` because a
    /// reload mutates the in-memory map.
    /// Test: `store_upsert_and_get`, `store_reload_picks_up_external_write`.
    pub async fn get(&mut self, id: &ManagedSessionId) -> Result<SessionRecord, StoreError> {
        self.reload_if_changed().await?;
        let key = id.to_string();
        let record = self.data.sessions.get(&key).cloned();
        record.ok_or(StoreError::NotFound(key))
    }

    /// Return all stored session records, reloading from disk first if changed.
    ///
    /// Why: the manager's `list()` method and the reconcile pass both need the
    /// full set of known sessions, and (since #1219) must reflect any write made
    /// by another process before answering.
    /// What: calls [`Self::reload_if_changed`], then clones and collects all
    /// values from the in-memory map. Takes `&mut self` for the reload.
    /// Test: `store_upsert_and_get`, `store_reload_picks_up_external_write`.
    pub async fn all(&mut self) -> Result<Vec<SessionRecord>, StoreError> {
        self.reload_if_changed().await?;
        Ok(self.data.sessions.values().cloned().collect())
    }

    /// Remove a session record from the store and persist.
    ///
    /// Why: fully dead sessions that have been pruned should not accumulate in
    /// the store forever; callers can explicitly remove them.
    /// What: removes the entry by key and saves the store, or logs a warning if
    /// the id was not present.
    /// Test: `store_remove`.
    pub async fn remove(&mut self, id: &ManagedSessionId) -> Result<(), StoreError> {
        // Reload first so a concurrent out-of-process write (#1219) is not lost
        // when we re-serialize, and so the not-found warning is accurate.
        self.reload_if_changed().await?;
        let key = id.to_string();
        if self.data.sessions.remove(&key).is_none() {
            warn!(id = %key, "remove: session not found in store");
        }
        self.save().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_manager::record::{ManagedSessionState, SessionRecord};
    use chrono::Utc;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn make_record(id: ManagedSessionId) -> SessionRecord {
        SessionRecord {
            id,
            tmux_name: format!("tmpm-test-{id}"),
            cwd: PathBuf::from("/tmp"),
            task: "test task".into(),
            state: ManagedSessionState::Active,
            created_at: Utc::now(),
            last_activity_at: None,
            workspace_path: None,
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
        }
    }

    /// Build a record with an explicit state so tests can write a specific
    /// transition straight to disk (simulating an out-of-process write).
    fn make_record_with_state(id: ManagedSessionId, state: ManagedSessionState) -> SessionRecord {
        SessionRecord {
            state,
            ..make_record(id)
        }
    }

    #[tokio::test]
    async fn store_load_save_round_trip() {
        let dir = TempDir::new().expect("tempdir");
        let data_dir = dir.path();

        let mut store = SessionStore::load(data_dir).await.expect("load empty");
        assert!(store.all().await.expect("all").is_empty());

        let id = ManagedSessionId::new();
        store.upsert(make_record(id)).await.expect("upsert");

        let mut store2 = SessionStore::load(data_dir).await.expect("reload");
        let record = store2.get(&id).await.expect("get after reload");
        assert_eq!(record.id, id);
        assert_eq!(record.state, ManagedSessionState::Active);
    }

    #[tokio::test]
    async fn store_upsert_and_get() {
        let dir = TempDir::new().expect("tempdir");
        let mut store = SessionStore::load(dir.path()).await.expect("load");

        let id = ManagedSessionId::new();
        store.upsert(make_record(id)).await.expect("upsert");

        let record = store.get(&id).await.expect("get");
        assert_eq!(record.id, id);
        assert_eq!(store.all().await.expect("all").len(), 1);
    }

    #[tokio::test]
    async fn store_remove() {
        let dir = TempDir::new().expect("tempdir");
        let mut store = SessionStore::load(dir.path()).await.expect("load");

        let id = ManagedSessionId::new();
        store.upsert(make_record(id)).await.expect("upsert");
        assert_eq!(store.all().await.expect("all").len(), 1);

        store.remove(&id).await.expect("remove");
        assert!(store.all().await.expect("all").is_empty());
        assert!(matches!(store.get(&id).await, Err(StoreError::NotFound(_))));
    }

    /// Why: #1219 — when another process writes the shared `sessions.json`, a
    /// store that already loaded must pick up the change on its next read rather
    /// than serving stale state forever. This is the core reload-on-read guard.
    /// What: store A loads and reads a record (state Active). A SECOND store B
    /// over the same dir writes a changed record (state Stopped) — standing in
    /// for the supervisor's out-of-process write. Then store A's `get` must
    /// return the NEW (Stopped) state, and `all` must reflect it too.
    /// Test: this test.
    #[tokio::test]
    async fn store_reload_picks_up_external_write() {
        let dir = TempDir::new().expect("tempdir");
        let id = ManagedSessionId::new();

        // Store A seeds an Active record and reads it into memory.
        let mut store_a = SessionStore::load(dir.path()).await.expect("load A");
        store_a
            .upsert(make_record_with_state(id, ManagedSessionState::Active))
            .await
            .expect("upsert A");
        let before = store_a.get(&id).await.expect("get A before");
        assert_eq!(before.state, ManagedSessionState::Active);

        // Store B (a different process's view) overwrites the record as Stopped.
        let mut store_b = SessionStore::load(dir.path()).await.expect("load B");
        store_b
            .upsert(make_record_with_state(id, ManagedSessionState::Stopped))
            .await
            .expect("upsert B");

        // Store A must now observe the external write, not its stale Active copy.
        let after = store_a.get(&id).await.expect("get A after");
        assert_eq!(
            after.state,
            ManagedSessionState::Stopped,
            "store A must reload the external Stopped write"
        );
        let all = store_a.all().await.expect("all A");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].state, ManagedSessionState::Stopped);
    }

    /// Why: the reload check must be a cheap no-op when nothing changed, so the
    /// common (no concurrent writer) path does not re-parse the file on every
    /// read. This guards that an unchanged file does not perturb in-memory state.
    /// What: seeds a record, calls `reload_if_changed` directly, and asserts the
    /// data is unchanged and the record still readable.
    /// Test: this test.
    #[tokio::test]
    async fn store_reload_noop_when_unchanged() {
        let dir = TempDir::new().expect("tempdir");
        let id = ManagedSessionId::new();
        let mut store = SessionStore::load(dir.path()).await.expect("load");
        store.upsert(make_record(id)).await.expect("upsert");

        // No external write happened, so this must be a no-op that preserves data.
        store.reload_if_changed().await.expect("reload no-op");
        let record = store.get(&id).await.expect("get");
        assert_eq!(record.id, id);
    }

    /// Why: an upsert must not clobber a concurrent out-of-process write to a
    /// DIFFERENT record (#1219). Because upsert reloads before saving, the other
    /// process's record must survive this store's single-record write.
    /// What: store A holds record X. Store B writes a new record Y to disk. Then
    /// store A upserts an update to X. Both X and Y must be present afterward.
    /// Test: this test.
    #[tokio::test]
    async fn store_upsert_preserves_concurrent_write() {
        let dir = TempDir::new().expect("tempdir");
        let id_x = ManagedSessionId::new();
        let id_y = ManagedSessionId::new();

        let mut store_a = SessionStore::load(dir.path()).await.expect("load A");
        store_a.upsert(make_record(id_x)).await.expect("seed X");

        // Concurrent process writes Y.
        let mut store_b = SessionStore::load(dir.path()).await.expect("load B");
        store_b.upsert(make_record(id_y)).await.expect("write Y");

        // Store A updates X (state change). It must reload Y first, not drop it.
        store_a
            .upsert(make_record_with_state(id_x, ManagedSessionState::Stopped))
            .await
            .expect("update X");

        let all = store_a.all().await.expect("all A");
        assert_eq!(all.len(), 2, "both X and Y must survive: {all:?}");
        assert!(store_a.get(&id_x).await.is_ok(), "X present");
        assert!(store_a.get(&id_y).await.is_ok(), "Y must survive A's write");
    }
}
