//! Durable extraction checkpoints and shared publication/configuration exclusion (#4283).
use super::{JobStatus, KnowledgeStore, StageStatus, extraction::Extraction, persistence};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    pub source_id: String,
    pub item_id: String,
    pub fingerprint: String,
    pub model: String,
    pub status: String,
    pub attempts: u32,
    pub next_attempt_at: i64,
    pub lease_owner: String,
    pub lease_until: i64,
    pub output: Option<Extraction>,
    /// Generation that may have written entities, independent of pending inference.
    #[serde(default)]
    pub materialized_fingerprint: Option<String>,
    pub last_error: Option<String>,
}
impl Checkpoint {
    pub fn needs_withdrawal(&self) -> bool {
        self.materialized_fingerprint.is_some() || self.output.is_some()
    }
    /// Why: private inference candidates must not appear in public status responses.
    /// What: expose progress metadata only, with a fixed error notice instead of provider output.
    /// Test: `reviewed_extraction_status_never_exposes_pending_candidates`.
    pub fn public_status(&self) -> serde_json::Value {
        serde_json::json!({"source_id":self.source_id,"item_id":self.item_id,
            "fingerprint":self.fingerprint,"model":self.model,"status":self.status,
            "attempts":self.attempts,"next_attempt_at":self.next_attempt_at,
            "last_error":self.last_error.as_ref().map(|_| "Extraction pending; inspect local service diagnostics")})
    }
}
impl KnowledgeStore {
    pub fn checkpoints(&self) -> super::Result<BTreeMap<String, Checkpoint>> {
        Ok(persistence::read(&self.directory().join("extraction.json"))?.unwrap_or_default())
    }
    /// Why: completed inference must survive crashes and publication retries.
    /// What: replace one bounded checkpoint under an OS lock with an atomic file write.
    /// Test: `checkpoints_survive_reopen_and_workers_exclude`.
    pub fn checkpoint(&self, key: &str, value: Checkpoint) -> super::Result<()> {
        let _lock = persistence::lock(&self.directory().join("extraction-state.lock"))?;
        let mut all = self.checkpoints()?;
        if all.len() >= 10000 && !all.contains_key(key) {
            return Err(super::KnowledgeError::Unavailable(
                "Extraction checkpoint limit reached".into(),
            ));
        }
        all.insert(key.into(), value);
        persistence::write_bytes(
            &self.directory().join("extraction.json"),
            &serde_json::to_vec(&all)
                .map_err(|e| super::KnowledgeError::InvalidState(e.to_string()))?,
        )
    }
    /// Why: separate API processes must not run concurrent extraction for one assistant.
    /// What: acquire a nonblocking OS lock; contention returns None and I/O failures propagate.
    /// Test: `checkpoints_survive_reopen_and_workers_exclude`.
    pub fn worker_lock(&self) -> super::Result<Option<std::fs::File>> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.directory().join("extraction-worker.lock"))?;
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => Ok(Some(file)),
            Err(fs4::TryLockError::WouldBlock) => Ok(None),
            Err(fs4::TryLockError::Error(e)) => Err(e.into()),
        }
    }
    pub fn execution_status(
        &self,
        job_id: &str,
        completed: bool,
        reason: &str,
    ) -> super::Result<()> {
        let Some(state) = self.status()? else {
            return Ok(());
        };
        self.mutate(&state.revision, |s| {
            if s.paused {
                return Ok(());
            }
            for job in &mut s.jobs {
                if job.id == job_id && job.status != JobStatus::Cancelled {
                    let status = if completed {
                        JobStatus::Completed
                    } else {
                        JobStatus::Retryable
                    };
                    job.status = status.clone();
                    if completed {
                        job.dependency_reasons.clear();
                    }
                    for stage in [
                        &mut job.indexing,
                        &mut job.extraction,
                        &mut job.cleanup,
                        &mut job.publication,
                    ] {
                        *stage = StageStatus {
                            status: status.clone(),
                            reason: reason.into(),
                        };
                    }
                }
            }
            Ok(())
        })?;
        Ok(())
    }
}
/// Why: config revocation and publication cannot interleave durable side effects.
/// What: one cross-process manifest lock shared by Settings and the worker, outside state JSON locks.
/// Test: `manifest_mutations_are_exclusive`.
pub async fn mutation_guard(manifest: &Path) -> anyhow::Result<std::fs::File> {
    let path = manifest.canonicalize()?.with_extension("mutation.lock");
    Ok(tokio::task::spawn_blocking(move || persistence::lock(&path)).await??)
}
#[cfg(test)]
mod tests {
    #[test]
    fn checkpoints_survive_reopen_and_workers_exclude() {
        use super::*;
        use crate::assistants::{AssistantHome, AssistantInstanceId};
        let root = tempfile::tempdir().unwrap();
        let home = AssistantHome::under(
            root.path().canonicalize().unwrap(),
            AssistantInstanceId::new("fixture").unwrap(),
        );
        let store = KnowledgeStore::new(home.clone());
        persistence::private_dir(&store.directory()).unwrap();
        let checkpoint = Checkpoint {
            source_id: "source".into(),
            item_id: "item".into(),
            fingerprint: "v1".into(),
            model: "fixture".into(),
            status: "completed".into(),
            attempts: 1,
            next_attempt_at: 0,
            lease_owner: "".into(),
            lease_until: 0,
            output: None,
            materialized_fingerprint: None,
            last_error: None,
        };
        store.checkpoint("item", checkpoint).unwrap();
        let first = store.worker_lock().unwrap().unwrap();
        let reopened = KnowledgeStore::new(home);
        assert_eq!(reopened.checkpoints().unwrap()["item"].fingerprint, "v1");
        assert!(reopened.worker_lock().unwrap().is_none());
        drop(first);
        assert!(reopened.worker_lock().unwrap().is_some());
    }
    #[tokio::test]
    async fn manifest_mutations_are_exclusive() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("agent.toml");
        std::fs::write(&path, "fixture").unwrap();
        let first = super::mutation_guard(&path).await.unwrap();
        let second = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path.with_extension("mutation.lock"))
            .unwrap();
        assert!(fs4::FileExt::try_lock(&second).is_err());
        drop(first);
        assert!(fs4::FileExt::try_lock(&second).is_ok());
    }
}
