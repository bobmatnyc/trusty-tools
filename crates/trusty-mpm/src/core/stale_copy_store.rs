//! The observed-version record store of the stale-copy guard (#10574).
//!
//! Why: a replacing write is safe only when it starts from the version last
//! observed, so the guard needs that version on disk between hook processes.
//! What: one `<key>.json` record (path, SHA-256, size, whether a copy was
//! saved, timestamp) and one `<key>.copy` saved copy per artifact, keyed by
//! the SHA-256 of the absolute path so two worktrees never share a record.
//! Bounded: after each insert the oldest records are evicted until the copies
//! fit [`super::STORE_BUDGET_BYTES`] and the count fits [`super::MAX_RECORDS`].
//! An evicted record reads as "never read", which denies; eviction can cost a
//! re-read, never an unchecked write.
//! Test: `stale_copy_tests.rs`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{MAX_COPY_BYTES, MAX_RECORDS, STORE_BUDGET_BYTES, STORE_DIR};

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

/// One observed version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedRecord {
    /// The absolute path observed.
    pub path: String,
    /// SHA-256 of the bytes observed.
    pub sha256: String,
    /// Size of the bytes observed.
    pub bytes: u64,
    /// Whether `<key>.copy` holds those bytes (false above [`MAX_COPY_BYTES`]).
    pub copy_saved: bool,
    /// RFC 3339 time of the observation.
    pub observed_at: String,
}

/// The store could not be read or written.
#[derive(Debug, thiserror::Error)]
#[error("stale-copy store {}: {detail}", .path.display())]
pub struct StoreError {
    /// The file or directory that failed.
    pub path: PathBuf,
    /// What failed.
    pub detail: String,
}

fn err(path: &Path, detail: impl std::fmt::Display) -> StoreError {
    StoreError {
        path: path.to_path_buf(),
        detail: detail.to_string(),
    }
}

/// Whether a replacing write would start from the last observed version.
#[derive(Debug, PartialEq, Eq)]
pub enum Freshness {
    /// The file matches its record.
    Fresh,
    /// The file does not exist: a write creates it and replaces nothing.
    Absent,
    /// No record: the file was never read (or the record was evicted).
    NeverRead,
    /// The file changed since it was observed.
    Changed {
        /// The hash on record.
        recorded: String,
        /// The hash on disk now.
        current: String,
    },
    /// The store or the file could not be read: the guard cannot judge.
    Unreadable(String),
}

/// The record store rooted at one directory.
#[derive(Debug, Clone)]
pub struct ObservedStore {
    dir: PathBuf,
}

impl ObservedStore {
    /// The store for `project_dir`: `<harness .trusty-mpm>/stale-copy`.
    pub fn for_project(project_dir: &Path) -> Self {
        Self::at(crate::core::harness_root::harness_dir(project_dir).join(STORE_DIR))
    }

    /// A store rooted at `dir`.
    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn key(abs: &Path) -> String {
        sha256_hex(abs.to_string_lossy().as_bytes())
    }

    fn record_path(&self, abs: &Path) -> PathBuf {
        self.dir.join(format!("{}.json", Self::key(abs)))
    }

    /// The saved copy's path for `abs`.
    pub fn copy_path(&self, abs: &Path) -> PathBuf {
        self.dir.join(format!("{}.copy", Self::key(abs)))
    }

    /// Record `content` as the observed version of `abs`.
    ///
    /// Test: `record_then_lookup_round_trips_with_a_saved_copy`,
    /// `store_evicts_the_oldest_records_past_the_count_cap`.
    pub fn record(&self, abs: &Path, content: &[u8]) -> Result<ObservedRecord, StoreError> {
        std::fs::create_dir_all(&self.dir).map_err(|e| err(&self.dir, e))?;
        let copy_saved = (content.len() as u64) <= MAX_COPY_BYTES;
        let copy = self.copy_path(abs);
        if copy_saved {
            write_atomic(&self.dir, &copy, content)?;
        } else if copy.exists() {
            std::fs::remove_file(&copy).map_err(|e| err(&copy, e))?;
        }
        let record = ObservedRecord {
            path: abs.to_string_lossy().into_owned(),
            sha256: sha256_hex(content),
            bytes: content.len() as u64,
            copy_saved,
            observed_at: chrono::Utc::now().to_rfc3339(),
        };
        let json = serde_json::to_vec(&record).map_err(|e| err(&self.dir, e))?;
        write_atomic(&self.dir, &self.record_path(abs), &json)?;
        self.enforce_budget(&self.record_path(abs))?;
        Ok(record)
    }

    /// The record for `abs`; `Ok(None)` when there is none.
    ///
    /// Test: `an_unreadable_record_is_a_store_error`.
    pub fn lookup(&self, abs: &Path) -> Result<Option<ObservedRecord>, StoreError> {
        let path = self.record_path(abs);
        match std::fs::read(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(err(&path, e)),
            Ok(raw) => serde_json::from_slice(&raw)
                .map(Some)
                .map_err(|e| err(&path, e)),
        }
    }

    /// Drop the record and copy for `abs`; absent is not an error.
    pub fn forget(&self, abs: &Path) -> Result<(), StoreError> {
        for path in [self.record_path(abs), self.copy_path(abs)] {
            match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(err(&path, e));
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Whether `abs` on disk matches its record.
    ///
    /// Test: `freshness_covers_every_arm`.
    pub fn freshness(&self, abs: &Path) -> Freshness {
        let current = match std::fs::read(abs) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Freshness::Absent,
            Err(e) => return Freshness::Unreadable(format!("{}: {e}", abs.display())),
            Ok(bytes) => sha256_hex(&bytes),
        };
        match self.lookup(abs) {
            Err(e) => Freshness::Unreadable(e.to_string()),
            Ok(None) => Freshness::NeverRead,
            Ok(Some(rec)) if rec.sha256 == current => Freshness::Fresh,
            Ok(Some(rec)) => Freshness::Changed {
                recorded: rec.sha256,
                current,
            },
        }
    }

    /// Evict the oldest records (never `keep`) until both bounds hold.
    fn enforce_budget(&self, keep: &Path) -> Result<(), StoreError> {
        let mut records = Vec::new();
        let mut total: u64 = 0;
        let entries = std::fs::read_dir(&self.dir).map_err(|e| err(&self.dir, e))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let copy = path.with_extension("copy");
            let size = std::fs::metadata(&copy).map(|m| m.len()).unwrap_or(0);
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            total += size;
            records.push((modified, path, copy, size));
        }
        records.sort_by_key(|r| r.0);
        let mut count = records.len();
        for (_, record, copy, size) in records {
            if total <= STORE_BUDGET_BYTES && count <= MAX_RECORDS {
                break;
            }
            if record == keep {
                continue;
            }
            let _ = std::fs::remove_file(&copy);
            // #10574: a concurrent hook may have evicted it already; keep going.
            match std::fs::remove_file(&record) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(err(&record, e));
                }
                _ => {}
            }
            total -= size;
            count -= 1;
        }
        Ok(())
    }
}

/// Write `bytes` to `dest` through a temp file in `dir`.
fn write_atomic(dir: &Path, dest: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    use std::io::Write as _;
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|e| err(dir, e))?;
    tmp.write_all(bytes).map_err(|e| err(dest, e))?;
    tmp.persist(dest).map_err(|e| err(dest, e.error))?;
    Ok(())
}
