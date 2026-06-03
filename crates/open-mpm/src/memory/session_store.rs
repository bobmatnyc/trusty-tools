//! Per-session agent memory store + cross-session registry.
//!
//! Why: Each PM run wants a private agent-memory namespace so parallel or
//! overlapping sessions don't overwrite each other's `run:` / `children:` /
//! `followed_by:` bookkeeping. The code index is shared (see `CodeStore`)
//! because code lives in the repo, but agent sessions belong to exactly one
//! orchestration run. Giving each run its own redb + usearch files also
//! makes it trivial to archive, inspect, or delete a session in isolation.
//! What: `SessionStore` wraps an inner `RedbUsearchStore` limited to
//! `Segment::AgentMemory` and lives at `sessions/<run_id>/`. `SessionRegistry`
//! tracks which run_ids have been created, persisted in
//! `sessions/index.redb`. A `SessionMeta` captures per-session metadata.
//! Test: See `tests` — isolation between run_ids, registry round-trip.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

use super::redb_usearch::RedbUsearchStore;
use super::store::{MemoryResult, MemoryStore, Segment};

/// Redb table keyed by run_id -> JSON-encoded `SessionMeta`.
const SESSION_INDEX_TABLE: TableDefinition<&str, &str> = TableDefinition::new("session_index");

/// On-disk location of the session registry, relative to the sessions root.
const REGISTRY_FILE: &str = "index.redb";

/// Human- and machine-readable metadata for a single agent orchestration run.
///
/// Why: The session list (`memory sessions` CLI) needs enough context to be
/// useful: the run_id for targeting, a timestamp for sorting, and a short
/// preview of the task so a human can recognize it. We keep the struct small
/// to stay friendly to serde and to table rendering.
/// What: Plain serde struct persisted as a JSON string in redb.
/// Test: `registry_lists_registered_sessions`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionMeta {
    pub run_id: String,
    pub started_at: DateTime<Utc>,
    pub task_preview: String,
}

/// Agent-memory store scoped to a single run_id.
///
/// Why: See module-level docs. Each spawn gets its own on-disk namespace so
/// concurrent runs don't collide. Unlike `CodeStore`, no advisory lock is
/// needed because exactly one process owns the session dir.
/// What: Holds the inner `RedbUsearchStore` + the run_id so callers can
/// reflect "which session am I writing into?" (useful for logs / telemetry).
/// Test: `session_store_isolated_from_other_sessions`.
pub struct SessionStore {
    inner: RedbUsearchStore,
    pub run_id: String,
}

impl SessionStore {
    /// Open (or create) a session store rooted at `sessions_dir/<run_id>/`.
    ///
    /// Why: Encapsulate path construction so callers only need to know the
    /// parent `sessions/` directory + the run_id. Also registers the session
    /// in the registry so `memory sessions` can list it.
    /// What: Creates the per-session subdir, opens the inner store, and
    /// writes a `SessionMeta` with `started_at = now` and empty preview. The
    /// preview can be updated later via `SessionRegistry::register`.
    /// Test: Covered indirectly in every test — opening is a prerequisite.
    pub fn open(sessions_dir: &Path, run_id: &str, vector_dim: usize) -> Result<Self> {
        if run_id.is_empty() {
            bail!("run_id must not be empty");
        }
        let session_dir = sessions_dir.join(run_id);
        std::fs::create_dir_all(&session_dir)
            .with_context(|| format!("creating session dir {}", session_dir.display()))?;
        let inner = RedbUsearchStore::open(&session_dir, vector_dim)
            .context("opening inner session store")?;

        // Register the session (idempotent insert with current timestamp if
        // not already present).
        let registry = SessionRegistry::open(sessions_dir)?;
        if registry.get(run_id)?.is_none() {
            registry.register(&SessionMeta {
                run_id: run_id.to_string(),
                started_at: Utc::now(),
                task_preview: String::new(),
            })?;
        }

        Ok(Self {
            inner,
            run_id: run_id.to_string(),
        })
    }

    /// Construct directly as an `Arc<Self>`.
    pub fn new_arc(sessions_dir: &Path, run_id: &str, vector_dim: usize) -> Result<Arc<Self>> {
        Ok(Arc::new(Self::open(sessions_dir, run_id, vector_dim)?))
    }
}

#[async_trait]
impl MemoryStore for SessionStore {
    async fn insert(
        &self,
        segment: Segment,
        id: &str,
        vector: &[f32],
        payload: serde_json::Value,
    ) -> Result<()> {
        if !matches!(segment, Segment::AgentMemory) {
            bail!("SessionStore only accepts Segment::AgentMemory (got {segment:?})");
        }
        self.inner.insert(segment, id, vector, payload).await
    }

    async fn search(
        &self,
        segment: Segment,
        query_vec: &[f32],
        top_k: usize,
    ) -> Result<Vec<MemoryResult>> {
        if !matches!(segment, Segment::AgentMemory) {
            bail!("SessionStore only accepts Segment::AgentMemory (got {segment:?})");
        }
        self.inner.search(segment, query_vec, top_k).await
    }

    async fn get(&self, segment: Segment, id: &str) -> Result<Option<serde_json::Value>> {
        if !matches!(segment, Segment::AgentMemory) {
            bail!("SessionStore only accepts Segment::AgentMemory (got {segment:?})");
        }
        self.inner.get(segment, id).await
    }

    async fn delete(&self, segment: Segment, id: &str) -> Result<()> {
        if !matches!(segment, Segment::AgentMemory) {
            bail!("SessionStore only accepts Segment::AgentMemory (got {segment:?})");
        }
        self.inner.delete(segment, id).await
    }
}

/// Persistent registry of sessions that have been opened under a given
/// `sessions/` directory.
///
/// Why: Without a registry, `memory sessions` would have to stat every
/// subdirectory and guess which are valid runs. Storing metadata in a tiny
/// redb file lets us record `started_at` and `task_preview` authoritatively.
/// What: One redb table keyed by `run_id`, value = JSON-encoded `SessionMeta`.
/// Test: `registry_lists_registered_sessions`.
pub struct SessionRegistry {
    db: Database,
    #[allow(dead_code)]
    path: PathBuf,
}

impl SessionRegistry {
    /// Open (or create) the registry at `sessions_dir/index.redb`.
    ///
    /// Why: Separate from `SessionStore::open` so CLI commands that only
    /// enumerate sessions (not open them) don't pay the usearch setup cost.
    /// What: Creates `sessions_dir` if absent, opens the registry db, ensures
    /// the main table exists.
    /// Test: `registry_lists_registered_sessions`.
    pub fn open(sessions_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(sessions_dir)
            .with_context(|| format!("creating sessions dir {}", sessions_dir.display()))?;
        // Issue #702: recovery-aware open — a stale redb-2.x registry file is
        // moved aside and a fresh empty registry is created rather than crashing.
        let path = sessions_dir.join(REGISTRY_FILE);
        let db = crate::memory::redb_recovery::open_redb_or_recreate(&path)?;
        // Ensure the table exists.
        {
            let w = db.begin_write()?;
            {
                let _ = w.open_table(SESSION_INDEX_TABLE)?;
            }
            w.commit()?;
        }
        Ok(Self { db, path })
    }

    /// Insert or overwrite the metadata for `run_id`.
    pub fn register(&self, meta: &SessionMeta) -> Result<()> {
        let encoded = serde_json::to_string(meta).context("serializing SessionMeta")?;
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(SESSION_INDEX_TABLE)?;
            t.insert(meta.run_id.as_str(), encoded.as_str())?;
        }
        w.commit()?;
        Ok(())
    }

    /// Fetch a single session's metadata by run_id.
    pub fn get(&self, run_id: &str) -> Result<Option<SessionMeta>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(SESSION_INDEX_TABLE)?;
        let Some(raw) = t.get(run_id)? else {
            return Ok(None);
        };
        let meta: SessionMeta =
            serde_json::from_str(raw.value()).context("deserializing SessionMeta")?;
        Ok(Some(meta))
    }

    /// List all known sessions, sorted by `started_at` ascending.
    pub fn list(&self) -> Result<Vec<SessionMeta>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(SESSION_INDEX_TABLE)?;
        let mut out: Vec<SessionMeta> = Vec::new();
        for entry in t.iter()? {
            let (_k, v) = entry?;
            let meta: SessionMeta =
                serde_json::from_str(v.value()).context("deserializing SessionMeta")?;
            out.push(meta);
        }
        out.sort_by_key(|m| m.started_at);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    fn vec4(a: f32, b: f32, c: f32, d: f32) -> Vec<f32> {
        vec![a, b, c, d]
    }

    #[tokio::test]
    async fn session_store_isolated_from_other_sessions() {
        let dir = tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");

        let store_a = SessionStore::open(&sessions_dir, "run-a", 4).unwrap();
        let store_b = SessionStore::open(&sessions_dir, "run-b", 4).unwrap();

        store_a
            .insert(
                Segment::AgentMemory,
                "sess-a1",
                &vec4(1.0, 0.0, 0.0, 0.0),
                json!({"where": "a"}),
            )
            .await
            .unwrap();
        store_b
            .insert(
                Segment::AgentMemory,
                "sess-b1",
                &vec4(1.0, 0.0, 0.0, 0.0),
                json!({"where": "b"}),
            )
            .await
            .unwrap();

        let a_hits = store_a
            .search(Segment::AgentMemory, &vec4(1.0, 0.0, 0.0, 0.0), 5)
            .await
            .unwrap();
        let b_hits = store_b
            .search(Segment::AgentMemory, &vec4(1.0, 0.0, 0.0, 0.0), 5)
            .await
            .unwrap();

        assert_eq!(a_hits.len(), 1);
        assert_eq!(a_hits[0].id, "sess-a1");
        assert_eq!(b_hits.len(), 1);
        assert_eq!(b_hits[0].id, "sess-b1");
    }

    #[tokio::test]
    async fn session_store_rejects_code_index_segment() {
        let dir = tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");
        let store = SessionStore::open(&sessions_dir, "run-x", 4).unwrap();
        let err = store
            .insert(
                Segment::CodeIndex,
                "nope",
                &vec4(1.0, 0.0, 0.0, 0.0),
                json!({}),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("SessionStore only accepts"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn registry_lists_registered_sessions() {
        let dir = tempdir().unwrap();
        let reg = SessionRegistry::open(dir.path()).unwrap();

        let m1 = SessionMeta {
            run_id: "r1".to_string(),
            started_at: Utc::now(),
            task_preview: "first task".to_string(),
        };
        let m2 = SessionMeta {
            run_id: "r2".to_string(),
            started_at: Utc::now(),
            task_preview: "second task".to_string(),
        };
        reg.register(&m1).unwrap();
        reg.register(&m2).unwrap();

        let all = reg.list().unwrap();
        assert_eq!(all.len(), 2);
        let ids: Vec<&str> = all.iter().map(|m| m.run_id.as_str()).collect();
        assert!(ids.contains(&"r1"));
        assert!(ids.contains(&"r2"));

        let one = reg.get("r1").unwrap().unwrap();
        assert_eq!(one.task_preview, "first task");
    }

    #[test]
    fn registry_get_returns_none_for_unknown() {
        let dir = tempdir().unwrap();
        let reg = SessionRegistry::open(dir.path()).unwrap();
        assert!(reg.get("missing").unwrap().is_none());
    }
}
