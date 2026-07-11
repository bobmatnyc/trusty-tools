//! On-disk JSON persistence for Deliverable and Milestone records (§10.7).
//!
//! Why: Deliverables and Milestones must survive daemon restarts, and — like the
//! project registry — be visible regardless of which checkout (if any) is open,
//! so they live as central, daemon-owned sibling stores next to `projects.json`
//! (§13 Q5: CENTRAL, not local-checkout). The two record types have identical
//! persistence needs (UUID-keyed map, atomic temp-file + rename, reload-on-read
//! freshness), so rather than duplicate the `store.rs` pattern twice (>80%
//! similar → consolidate) this is ONE generic [`JsonStore<T>`] reused verbatim
//! for both via the [`Keyed`] trait.
//! What: [`JsonStore<T>`] loads/saves a `HashMap<String, T>` from/to a JSON file,
//! using the same atomic-write and `(mtime, len)` freshness fingerprint as
//! [`crate::project::store::ProjectStore`]. [`DeliverableStore`] and
//! [`MilestoneStore`] are the two concrete instantiations.
//! Test: `store_round_trip`, `store_upsert_idempotent`, `store_remove`,
//! `store_reload_picks_up_external_write`, `store_other_io_error_propagates`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;
use tokio::fs;
use tracing::debug;

use super::milestone::Milestone;
use super::record::Deliverable;

/// A record that a [`JsonStore`] can key by a stable string id.
///
/// Why: the generic store must know each record's map key without knowing the
/// concrete type; a tiny trait gives it that without reflection.
/// What: `store_key` returns the record's stable id as a string (the UUID);
/// `project_name` scopes the record to its owning project for list filtering.
/// Test: exercised via `DeliverableStore`/`MilestoneStore` in the store tests.
pub trait Keyed {
    /// The record's stable store key (its UUID, stringified).
    fn store_key(&self) -> String;
    /// The owning project's registry-B name, for project-scoped listing.
    fn project_name(&self) -> &str;
}

impl Keyed for Deliverable {
    fn store_key(&self) -> String {
        self.id.to_string()
    }
    fn project_name(&self) -> &str {
        &self.project_name
    }
}

impl Keyed for Milestone {
    fn store_key(&self) -> String {
        self.id.to_string()
    }
    fn project_name(&self) -> &str {
        &self.project_name
    }
}

/// Errors that can arise from Deliverable/Milestone store I/O or serialization.
///
/// Why: callers need structured error information to distinguish transient I/O
/// failures from data-corruption problems and missing records.
/// What: one variant per failure mode: I/O, serialization, or not-found.
/// Test: `store_other_io_error_propagates`, `store_get_missing_is_not_found`.
#[derive(Debug, Error)]
pub enum StoreError {
    /// An I/O operation on the backing file failed.
    #[error("store I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization or deserialization failed.
    #[error("store serialization error: {0}")]
    Serialize(String),

    /// The requested record id was not found in the store.
    #[error("record not found: {0}")]
    NotFound(String),
}

/// A cheap freshness fingerprint for the backing file (mtime + length).
///
/// Why: identical rationale to `ProjectStore::FileSig` — an mtime alone is
/// insufficient on coarse filesystems; pairing it with byte length catches
/// same-second writes that changed the size.
/// What: the file's last-modified time and length in bytes.
/// Test: `store_reload_picks_up_external_write`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct FileSig {
    mtime: Option<SystemTime>,
    len: u64,
}

/// Async, file-backed, UUID-keyed store for any [`Keyed`] record type (§10.7).
///
/// Why: both the deliverable and milestone stores need daemon-owned, atomic,
/// reload-on-read persistence identical to `ProjectStore`; making it generic
/// avoids duplicating that logic per record type.
/// What: holds the in-memory `HashMap<String, T>`, the backing file path, and
/// the last observed file signature. All mutations call `save()` immediately;
/// reads call `reload_if_changed()` first so out-of-process writes are seen.
/// Test: `store_round_trip`, `store_upsert_idempotent`, `store_remove`.
#[derive(Debug)]
pub struct JsonStore<T> {
    data: HashMap<String, T>,
    path: PathBuf,
    last_sig: Option<FileSig>,
}

impl<T> JsonStore<T>
where
    T: Keyed + Clone + Serialize + DeserializeOwned,
{
    /// Load (or create) the store at the given file path.
    ///
    /// Why: on startup each store restores state from the previous run; an absent
    /// file starts fresh so the daemon boots cleanly.
    /// What: reads and JSON-parses `path` if present; returns an empty store if
    /// the file is absent; propagates all other I/O errors.
    /// Test: `store_round_trip`, `store_not_found_starts_fresh`.
    pub async fn load(path: PathBuf) -> Result<Self, StoreError> {
        let (data, last_sig) = Self::read_file(&path).await?;
        Ok(Self {
            data,
            path,
            last_sig,
        })
    }

    /// Stat `path` and return its freshness fingerprint, or `None` if absent.
    async fn sig_of(path: &Path) -> Option<FileSig> {
        let meta = fs::metadata(path).await.ok()?;
        Some(FileSig {
            mtime: meta.modified().ok(),
            len: meta.len(),
        })
    }

    /// Read and parse the backing file, returning the data and its fingerprint.
    ///
    /// Why: `load` and `reload_if_changed` share "read or treat absence as
    /// empty" logic; only `NotFound` is treated as absent so a transient I/O
    /// error never silently overwrites the store with an empty map on the next
    /// `save` (the exact data-loss bug `ProjectStore` guards against).
    /// What: on success returns `(map, Some(sig))`; on `NotFound` returns
    /// `(empty, None)`; propagates all other I/O errors.
    /// Test: `store_round_trip`, `store_other_io_error_propagates`.
    async fn read_file(path: &Path) -> Result<(HashMap<String, T>, Option<FileSig>), StoreError> {
        match fs::read_to_string(path).await {
            Ok(raw) => {
                let data = serde_json::from_str::<HashMap<String, T>>(&raw)
                    .map_err(|e| StoreError::Serialize(e.to_string()))?;
                let sig = Self::sig_of(path).await.unwrap_or_default();
                Ok((data, Some(sig)))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                debug!(path = %path.display(), "no store file found; starting fresh");
                Ok((HashMap::new(), None))
            }
            Err(e) => Err(StoreError::Io(e)),
        }
    }

    /// Reload the in-memory map from disk if the backing file changed.
    ///
    /// Why: another process (or another store handle) may have written the file;
    /// reading first reconciles the in-memory view with the authoritative disk
    /// state (identical semantics to `ProjectStore::reload_if_changed`).
    /// What: stats the file; if its fingerprint differs from `last_sig`, re-reads
    /// and replaces `data`/`last_sig`. A matching fingerprint is a fast no-op.
    /// Test: `store_reload_picks_up_external_write`.
    pub async fn reload_if_changed(&mut self) -> Result<(), StoreError> {
        let current = Self::sig_of(&self.path).await;
        let unchanged = matches!((current, self.last_sig), (Some(a), Some(b)) if a == b);
        if unchanged {
            return Ok(());
        }
        let (data, sig) = Self::read_file(&self.path).await?;
        self.data = data;
        self.last_sig = sig;
        debug!(path = %self.path.display(), "store reloaded after external change");
        Ok(())
    }

    /// Persist the current in-memory state to disk atomically.
    ///
    /// Why: every mutation must flush for crash safety; temp-file + rename
    /// prevents concurrent readers from seeing a torn file.
    /// What: serializes to pretty JSON, writes a sibling `.tmp`, renames it over
    /// the real path, and records the resulting signature.
    /// Test: verified by every mutating store test.
    pub async fn save(&mut self) -> Result<(), StoreError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let json = serde_json::to_string_pretty(&self.data)
            .map_err(|e| StoreError::Serialize(e.to_string()))?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, json).await?;
        fs::rename(&tmp, &self.path).await?;
        self.last_sig = Self::sig_of(&self.path).await;
        debug!(path = %self.path.display(), "store saved");
        Ok(())
    }

    /// Insert or update a record, keyed by its [`Keyed::store_key`].
    ///
    /// Why: create and update both persist a record; reloading first ensures a
    /// concurrent write from another handle is not lost.
    /// What: reloads if changed, inserts/replaces by key, then saves.
    /// Test: `store_upsert_idempotent`.
    pub async fn upsert(&mut self, record: T) -> Result<(), StoreError> {
        self.reload_if_changed().await?;
        self.data.insert(record.store_key(), record);
        self.save().await
    }

    /// Look up a record by its string id, reloading from disk first.
    ///
    /// Why: the caller must see out-of-process writes; reloading before answering
    /// ensures freshness.
    /// What: reloads if changed, then returns a clone or [`StoreError::NotFound`].
    /// Test: `store_round_trip`, `store_get_missing_is_not_found`.
    pub async fn get(&mut self, id: &str) -> Result<T, StoreError> {
        self.reload_if_changed().await?;
        self.data
            .get(id)
            .cloned()
            .ok_or_else(|| StoreError::NotFound(id.to_string()))
    }

    /// Return all stored records, reloading from disk first.
    ///
    /// Why: the histogram/status rollup and project-scoped listing both need the
    /// full set and must reflect any external write.
    /// What: reloads if changed, then clones and collects all values.
    /// Test: `store_round_trip`, `store_by_project_filters`.
    pub async fn all(&mut self) -> Result<Vec<T>, StoreError> {
        self.reload_if_changed().await?;
        Ok(self.data.values().cloned().collect())
    }

    /// Return all records whose owning project matches `project_name`.
    ///
    /// Why: the CRUD list endpoints are project-scoped
    /// (`/api/v1/projects/{name}/deliverables`); filtering in the store keeps the
    /// handler thin.
    /// What: reloads if changed, then clones the values whose
    /// [`Keyed::project_name`] equals `project_name`.
    /// Test: `store_by_project_filters`.
    pub async fn by_project(&mut self, project_name: &str) -> Result<Vec<T>, StoreError> {
        self.reload_if_changed().await?;
        Ok(self
            .data
            .values()
            .filter(|r| r.project_name() == project_name)
            .cloned()
            .collect())
    }

    /// Remove a record by id, reloading first; errors if absent.
    ///
    /// Why: completeness of the CRUD surface; deleting a stale Deliverable/
    /// Milestone must persist and reconcile against concurrent writes.
    /// What: reloads if changed, removes by key (NotFound if absent), then saves.
    /// Test: `store_remove`.
    pub async fn remove(&mut self, id: &str) -> Result<T, StoreError> {
        self.reload_if_changed().await?;
        let removed = self
            .data
            .remove(id)
            .ok_or_else(|| StoreError::NotFound(id.to_string()))?;
        self.save().await?;
        Ok(removed)
    }
}

/// Central store for [`Deliverable`] records (`<framework_root>/deliverables.json`).
pub type DeliverableStore = JsonStore<Deliverable>;

/// Central store for [`Milestone`] records (`<framework_root>/milestones.json`).
pub type MilestoneStore = JsonStore<Milestone>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deliverable::record::{DeliverableId, DeliverableKind, EstimationTier};
    use crate::deliverable::status::DeliverableStatus;
    use chrono::Utc;
    use tempfile::TempDir;

    fn deliverable(project: &str, name: &str) -> Deliverable {
        Deliverable {
            id: DeliverableId::new(),
            project_name: project.into(),
            name: name.into(),
            description: String::new(),
            kind: DeliverableKind::Feature,
            ticket_ref: None,
            spec_ref: None,
            status: DeliverableStatus::Proposed,
            estimated_effort: EstimationTier::M,
            created_at: Utc::now(),
            target_date: None,
        }
    }

    fn path(dir: &TempDir) -> PathBuf {
        dir.path().join("deliverables.json")
    }

    #[tokio::test]
    async fn store_round_trip() {
        let dir = TempDir::new().unwrap();
        let mut store = DeliverableStore::load(path(&dir)).await.unwrap();
        assert!(store.all().await.unwrap().is_empty());

        let d = deliverable("alpha", "one");
        let key = d.store_key();
        store.upsert(d).await.unwrap();

        let mut store2 = DeliverableStore::load(path(&dir)).await.unwrap();
        let got = store2.get(&key).await.unwrap();
        assert_eq!(got.name, "one");
    }

    #[tokio::test]
    async fn store_upsert_idempotent() {
        let dir = TempDir::new().unwrap();
        let mut store = DeliverableStore::load(path(&dir)).await.unwrap();
        let mut d = deliverable("beta", "orig");
        let key = d.store_key();
        store.upsert(d.clone()).await.unwrap();
        d.name = "updated".into();
        store.upsert(d).await.unwrap();
        let all = store.all().await.unwrap();
        assert_eq!(all.len(), 1, "upsert on same key must not duplicate");
        assert_eq!(store.get(&key).await.unwrap().name, "updated");
    }

    #[tokio::test]
    async fn store_by_project_filters() {
        let dir = TempDir::new().unwrap();
        let mut store = DeliverableStore::load(path(&dir)).await.unwrap();
        store.upsert(deliverable("p1", "a")).await.unwrap();
        store.upsert(deliverable("p1", "b")).await.unwrap();
        store.upsert(deliverable("p2", "c")).await.unwrap();
        let p1 = store.by_project("p1").await.unwrap();
        assert_eq!(p1.len(), 2);
        assert!(p1.iter().all(|d| d.project_name == "p1"));
        assert_eq!(store.by_project("p2").await.unwrap().len(), 1);
        assert!(store.by_project("missing").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn store_get_missing_is_not_found() {
        let dir = TempDir::new().unwrap();
        let mut store = DeliverableStore::load(path(&dir)).await.unwrap();
        let err = store.get("nope").await;
        assert!(matches!(err, Err(StoreError::NotFound(_))));
    }

    #[tokio::test]
    async fn store_remove() {
        let dir = TempDir::new().unwrap();
        let mut store = DeliverableStore::load(path(&dir)).await.unwrap();
        let d = deliverable("g", "x");
        let key = d.store_key();
        store.upsert(d).await.unwrap();
        store.remove(&key).await.unwrap();
        assert!(matches!(
            store.get(&key).await,
            Err(StoreError::NotFound(_))
        ));
        assert!(matches!(
            store.remove(&key).await,
            Err(StoreError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn store_reload_picks_up_external_write() {
        let dir = TempDir::new().unwrap();
        let mut a = DeliverableStore::load(path(&dir)).await.unwrap();
        a.upsert(deliverable("p", "first")).await.unwrap();

        let mut b = DeliverableStore::load(path(&dir)).await.unwrap();
        let d = deliverable("p", "second");
        let key = d.store_key();
        b.upsert(d).await.unwrap();

        // `a` must observe `b`'s write on its next read.
        let got = a.get(&key).await.unwrap();
        assert_eq!(got.name, "second");
    }

    #[tokio::test]
    async fn store_not_found_starts_fresh() {
        let dir = TempDir::new().unwrap();
        let mut store = DeliverableStore::load(path(&dir)).await.unwrap();
        assert!(store.all().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn store_other_io_error_propagates() {
        let dir = TempDir::new().unwrap();
        // Plant a directory where the JSON file would live so read_to_string
        // returns a non-NotFound OS error.
        let p = path(&dir);
        tokio::fs::create_dir_all(&p).await.unwrap();
        let result = DeliverableStore::load(p).await;
        assert!(matches!(result, Err(StoreError::Io(_))));
    }

    #[tokio::test]
    async fn milestone_store_round_trips() {
        use crate::deliverable::milestone::{Milestone, MilestoneId, MilestoneStatus};
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("milestones.json");
        let mut store = MilestoneStore::load(p.clone()).await.unwrap();
        let m = Milestone {
            id: MilestoneId::new(),
            project_name: "trusty-tools".into(),
            name: "v1".into(),
            description: String::new(),
            target_date: Utc::now(),
            status: MilestoneStatus::Proposed,
            deliverables: vec![],
            created_at: Utc::now(),
        };
        let key = m.store_key();
        store.upsert(m).await.unwrap();
        let mut store2 = MilestoneStore::load(p).await.unwrap();
        assert_eq!(store2.get(&key).await.unwrap().name, "v1");
    }
}
