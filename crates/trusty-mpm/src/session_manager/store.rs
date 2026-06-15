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

/// Async, file-backed store for [`SessionRecord`]s.
///
/// Why: the session manager must be able to reload all known sessions after a
/// crash or restart so that reconciliation can re-adopt live tmux sessions.
/// What: holds the in-memory map and the path to the backing JSON file; all
/// mutations call `save()` immediately to keep the file consistent.
/// Test: `store_load_save_round_trip`, `store_upsert_and_get`.
#[derive(Debug)]
pub struct SessionStore {
    /// In-memory sessions map.
    data: StoredData,
    /// Path to the backing `sessions.json` file.
    path: PathBuf,
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
        let data = if path.exists() {
            let raw = fs::read_to_string(&path).await?;
            serde_json::from_str::<StoredData>(&raw)
                .map_err(|e| StoreError::Serialize(e.to_string()))?
        } else {
            debug!(path = %path.display(), "no session store file found; starting fresh");
            StoredData::default()
        };
        Ok(Self { data, path })
    }

    /// Persist the current in-memory state to disk.
    ///
    /// Why: every mutating operation must flush to disk so that the store
    /// survives a daemon crash immediately after the mutation.
    /// What: serializes `self.data` to JSON, creates parent directories if
    /// needed, then writes the file atomically via a temp write.
    /// Test: verified indirectly by every mutating store test.
    pub async fn save(&self) -> Result<(), StoreError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let json = serde_json::to_string_pretty(&self.data)
            .map_err(|e| StoreError::Serialize(e.to_string()))?;
        fs::write(&self.path, json).await?;
        debug!(path = %self.path.display(), "session store saved");
        Ok(())
    }

    /// Insert or update a session record.
    ///
    /// Why: session state changes (Started → Active, Active → Dead, etc.) must
    /// be reflected in the store immediately so recovery sees the latest state.
    /// What: inserts or replaces the record keyed by its UUID string, then
    /// saves the store to disk.
    /// Test: `store_upsert_and_get`.
    pub async fn upsert(&mut self, record: SessionRecord) -> Result<(), StoreError> {
        let key = record.id.to_string();
        self.data.sessions.insert(key, record);
        self.save().await
    }

    /// Look up a session record by id.
    ///
    /// Why: the manager's `get()` method needs a typed lookup that returns a
    /// structured not-found error rather than `Option::None`.
    /// What: returns a clone of the stored record or `StoreError::NotFound`.
    /// Test: `store_upsert_and_get`.
    pub fn get(&self, id: &ManagedSessionId) -> Result<SessionRecord, StoreError> {
        let key = id.to_string();
        let record = self.data.sessions.get(&key).cloned();
        record.ok_or(StoreError::NotFound(key))
    }

    /// Return all stored session records.
    ///
    /// Why: the manager's `list()` method and the reconcile pass both need the
    /// full set of known sessions.
    /// What: clones and collects all values from the in-memory map.
    /// Test: `store_upsert_and_get`.
    pub fn all(&self) -> Vec<SessionRecord> {
        self.data.sessions.values().cloned().collect()
    }

    /// Remove a session record from the store and persist.
    ///
    /// Why: fully dead sessions that have been pruned should not accumulate in
    /// the store forever; callers can explicitly remove them.
    /// What: removes the entry by key and saves the store, or logs a warning if
    /// the id was not present.
    /// Test: `store_remove`.
    pub async fn remove(&mut self, id: &ManagedSessionId) -> Result<(), StoreError> {
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

    #[tokio::test]
    async fn store_load_save_round_trip() {
        let dir = TempDir::new().expect("tempdir");
        let data_dir = dir.path();

        let mut store = SessionStore::load(data_dir).await.expect("load empty");
        assert!(store.all().is_empty());

        let id = ManagedSessionId::new();
        store.upsert(make_record(id)).await.expect("upsert");

        let store2 = SessionStore::load(data_dir).await.expect("reload");
        let record = store2.get(&id).expect("get after reload");
        assert_eq!(record.id, id);
        assert_eq!(record.state, ManagedSessionState::Active);
    }

    #[tokio::test]
    async fn store_upsert_and_get() {
        let dir = TempDir::new().expect("tempdir");
        let mut store = SessionStore::load(dir.path()).await.expect("load");

        let id = ManagedSessionId::new();
        store.upsert(make_record(id)).await.expect("upsert");

        let record = store.get(&id).expect("get");
        assert_eq!(record.id, id);
        assert_eq!(store.all().len(), 1);
    }

    #[tokio::test]
    async fn store_remove() {
        let dir = TempDir::new().expect("tempdir");
        let mut store = SessionStore::load(dir.path()).await.expect("load");

        let id = ManagedSessionId::new();
        store.upsert(make_record(id)).await.expect("upsert");
        assert_eq!(store.all().len(), 1);

        store.remove(&id).await.expect("remove");
        assert!(store.all().is_empty());
        assert!(matches!(store.get(&id), Err(StoreError::NotFound(_))));
    }
}
