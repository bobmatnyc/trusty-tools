//! Durable identity-only inbox. State failures do not discard previously admitted event identities.
use super::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pending {
    source_id: String,
    source_revision: String,
    event_digest: String,
    record_time: DateTime<Utc>,
    received_at: DateTime<Utc>,
}
impl KnowledgeStore {
    /// Persist before touching pipeline state. Caller has already matched the effective binding.
    pub fn enqueue_event(
        &self,
        source_id: &str,
        source_revision: &str,
        event_id: &str,
        record_time: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<()> {
        if source_id.is_empty()
            || source_id.len() > 512
            || source_revision.is_empty()
            || source_revision.len() > 512
            || event_id.is_empty()
            || event_id.len() > 1024
            || record_time > now
        {
            return Err(KnowledgeError::BadRequest(
                "Invalid incoming event identity".into(),
            ));
        }
        if !self.directory().is_dir() {
            return Err(KnowledgeError::InvalidState(
                "Knowledge is not initialized".into(),
            ));
        }
        let _guard = persistence::lock(&self.directory().join("state.lock"))?;
        let path = self.directory().join("inbox.json");
        let mut pending: Vec<Pending> = persistence::read(&path)?.unwrap_or_default();
        let event_digest = planning::digest(&[source_id, source_revision, event_id]);
        if pending.iter().any(|p| p.event_digest == event_digest) {
            return Ok(());
        }
        if pending.len() >= 10000 {
            return Err(KnowledgeError::InvalidState(
                "Incoming event inbox limit reached; source replay required".into(),
            ));
        }
        pending.push(Pending {
            source_id: source_id.into(),
            source_revision: source_revision.into(),
            event_digest,
            record_time,
            received_at: now,
        });
        persistence::write_bytes(
            &path,
            &serde_json::to_vec(&pending)
                .map_err(|e| KnowledgeError::InvalidState(e.to_string()))?,
        )
    }
    /// Reconcile and admit all pending identities under one lock; revision races cannot lose events.
    pub fn replay_inbox(
        &self,
        revision: &str,
        sources: &[SourceDescriptor],
        now: DateTime<Utc>,
    ) -> Result<KnowledgeState> {
        let _guard = persistence::lock(&self.directory().join("state.lock"))?;
        let mut state = self
            .status()?
            .ok_or_else(|| KnowledgeError::InvalidState("Knowledge is not initialized".into()))?;
        if state.revision != revision {
            return Err(KnowledgeError::Conflict);
        }
        let path = self.directory().join("inbox.json");
        let pending: Vec<Pending> = persistence::read(&path)?.unwrap_or_default();
        if pending.len() > 10000 {
            return Err(KnowledgeError::InvalidState(
                "Incoming event inbox limit exceeded".into(),
            ));
        }
        let before = state.clone();
        planning::reconcile_state(&mut state, sources, now)?;
        if state.paused {
            if state != before {
                persistence::write(&self.state_path(), &mut state)?;
            }
            return Ok(state);
        }
        for event in pending {
            let Some(source) = sources
                .iter()
                .find(|s| s.id == event.source_id && s.revision == event.source_revision)
            else {
                continue;
            };
            if state.admitted_events.contains(&event.event_digest) {
                continue;
            }
            if state.admitted_events.len() >= 10000 || state.jobs.len() >= planning::MAX_JOBS {
                return Err(KnowledgeError::InvalidState(
                    "Knowledge event capacity reached; inbox retained".into(),
                ));
            }
            let received = event.received_at.max(state.anchor_at);
            let window = planning::windows(&state, received, true)?
                .into_iter()
                .find(|w| received >= w.start && received < w.end)
                .ok_or_else(|| KnowledgeError::InvalidState("Arrival window unavailable".into()))?;
            let mut one = state.clone();
            one.jobs.clear();
            planning::add_job(&mut one, source, window)?;
            let mut job = one.jobs.remove(0);
            job.id = planning::digest(&[&state.assistant_id, &event.event_digest, "record"]);
            job.record = Some(RecordAdmission {
                event_digest: event.event_digest.clone(),
                record_time: event.record_time,
            });
            state.jobs.push(job);
            state.admitted_events.push(event.event_digest);
        }
        if state != before {
            persistence::write(&self.state_path(), &mut state)?;
        }
        // A crash before this clear replays safely against the committed event digest ledger.
        persistence::write_bytes(&path, b"[]")?;
        Ok(state)
    }
}
