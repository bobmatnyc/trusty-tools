//! Assistant-owned knowledge planning. No upstream content is read or extracted here.
pub mod execution;
pub mod extraction;
mod inbox;
pub mod inference;
pub(crate) mod persistence;
mod planning;
pub mod search_binding;
mod types;
use crate::assistants::AssistantHome;
use chrono::{DateTime, Utc};
use std::{collections::BTreeMap, path::PathBuf};
pub use types::*;

/// The API must validate the home belongs to a discovered Assistant instance.
pub struct KnowledgeStore {
    home: AssistantHome,
}
impl KnowledgeStore {
    pub fn new(home: AssistantHome) -> Self {
        Self { home }
    }
    fn directory(&self) -> PathBuf {
        self.home.path().join("stores/knowledge-pipeline")
    }
    fn state_path(&self) -> PathBuf {
        self.directory().join("state.json")
    }
    pub fn status(&self) -> Result<Option<KnowledgeState>> {
        let state: Option<KnowledgeState> = persistence::read(&self.state_path())?;
        if let Some(s) = &state {
            if s.schema_version != 1
                || s.assistant_id != self.home.id().as_str()
                || s.history_months == 0
                || s.history_months > 120
                || s.jobs.len() > planning::MAX_JOBS
                || s.sources.len() > 128
                || s.admitted_events.len() > 10000
            {
                return Err(KnowledgeError::InvalidState(
                    "Knowledge state identity or schema is invalid".into(),
                ));
            }
            self.validate_store(&s.store)?;
            if !s.store.root.is_dir() {
                return Err(KnowledgeError::InvalidState(
                    "Protected OKG root is missing; explicit repair is required".into(),
                ));
            }
        }
        Ok(state)
    }
    fn validate_store(&self, store: &ProtectedStore) -> Result<()> {
        let home = self.home.path();
        if !store.protected
            || !store.root.is_absolute()
            || !store.root.starts_with(home)
            || store.root == home
            || store
                .root
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            || store.root.starts_with(self.directory())
            || self.directory().starts_with(&store.root)
            || store.index_id.is_empty()
            || store.index_id.len() > 128
        {
            return Err(KnowledgeError::InvalidState(
                "Protected store must be exclusively inside this assistant home".into(),
            ));
        }
        persistence::safe_path(&store.root)?;
        if store.root.exists() && !store.root.is_dir() {
            return Err(KnowledgeError::InvalidState(
                "Protected OKG root is not a directory".into(),
            ));
        }
        Ok(())
    }
    fn tighten_store(&self, store: &ProtectedStore) -> Result<()> {
        self.validate_store(store)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let directory = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
                .open(&store.root)?;
            directory.set_permissions(std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    pub fn confirm_binding_with_legacy(
        &self,
        revision: &str,
        legacy: Option<crate::stores::AgentStoreBinding>,
    ) -> Result<KnowledgeState> {
        self.mutate(revision, |s| {
            s.binding_confirmed = true;
            s.legacy_binding = legacy;
            Ok(())
        })
    }
    pub fn confirm_binding(&self, revision: &str) -> Result<KnowledgeState> {
        self.mutate(revision, |s| {
            s.binding_confirmed = true;
            Ok(())
        })
    }
    pub fn initialize(
        &self,
        now: DateTime<Utc>,
        selected_store: Option<ProtectedStore>,
    ) -> Result<KnowledgeState> {
        let store = selected_store.unwrap_or_else(|| ProtectedStore {
            root: self.home.okg_dir(),
            index_id: format!(
                "assistant-okg-{}",
                &planning::digest(&[&self.home.path().to_string_lossy()])[..24]
            ),
            protected: true,
        });
        self.validate_store(&store)?;
        persistence::private_dir(&self.directory())?;
        let _guard = persistence::lock(&self.directory().join("state.lock"))?;
        if let Some(state) = persistence::read::<KnowledgeState>(&self.state_path())? {
            if state.store != store {
                return Err(KnowledgeError::InvalidState(
                    "Existing protected store cannot be replaced".into(),
                ));
            }
            self.tighten_store(&store)?;
            return self
                .status()?
                .ok_or_else(|| KnowledgeError::InvalidState("State disappeared".into()));
        }
        persistence::private_dir(&store.root)?;
        self.tighten_store(&store)?;
        let mut state = KnowledgeState {
            schema_version: 1,
            assistant_id: self.home.id().as_str().into(),
            revision: String::new(),
            anchor_at: now,
            history_months: 1,
            paused: false,
            binding_confirmed: false,
            legacy_binding: None,
            store,
            assistant_projects: vec![],
            projects_by_chat: BTreeMap::new(),
            sources: vec![],
            jobs: vec![],
            admitted_events: vec![],
        };
        persistence::write(&self.state_path(), &mut state)?;
        Ok(state)
    }
    fn mutate(
        &self,
        revision: &str,
        f: impl FnOnce(&mut KnowledgeState) -> Result<()>,
    ) -> Result<KnowledgeState> {
        if !self.directory().is_dir() {
            return Err(KnowledgeError::InvalidState(
                "Initialize knowledge first".into(),
            ));
        }
        let _guard = persistence::lock(&self.directory().join("state.lock"))?;
        let mut state = self
            .status()?
            .ok_or_else(|| KnowledgeError::InvalidState("Initialize knowledge first".into()))?;
        if revision != state.revision {
            return Err(KnowledgeError::Conflict);
        }
        let before = state.clone();
        f(&mut state)?;
        if state != before {
            persistence::write(&self.state_path(), &mut state)?;
        }
        Ok(state)
    }
    pub fn reconcile(
        &self,
        revision: &str,
        sources: &[SourceDescriptor],
        now: DateTime<Utc>,
    ) -> Result<KnowledgeState> {
        self.mutate(revision, |s| planning::reconcile_state(s, sources, now))
    }
    pub fn update_projects(
        &self,
        revision: &str,
        chat_id: &str,
        project_paths: &[String],
        sources: &[SourceDescriptor],
        now: DateTime<Utc>,
    ) -> Result<KnowledgeState> {
        if chat_id.is_empty()
            || chat_id.len() > 256
            || project_paths.len() > 64
            || project_paths
                .iter()
                .any(|p| p.len() > 4096 || !std::path::Path::new(p).is_absolute())
        {
            return Err(KnowledgeError::BadRequest(
                "Invalid chat project attachment selection".into(),
            ));
        }
        self.mutate(revision, |s| {
            if !s.projects_by_chat.contains_key(chat_id) && s.projects_by_chat.len() >= 256 {
                return Err(KnowledgeError::BadRequest(
                    "Chat attachment limit reached".into(),
                ));
            }
            let mut paths = project_paths.to_vec();
            paths.sort();
            paths.dedup();
            if paths.is_empty() {
                s.projects_by_chat.remove(chat_id);
            } else {
                s.projects_by_chat.insert(chat_id.into(), paths);
            }
            planning::reconcile_state(s, sources, now)
        })
    }
    pub fn set_paused(
        &self,
        revision: &str,
        paused: bool,
        _now: DateTime<Utc>,
    ) -> Result<KnowledgeState> {
        self.mutate(revision, |s| {
            s.paused = paused;
            Ok(())
        })
    }

    /// Why: selected projects must survive chat creation and application restart.
    /// What: update assistant selections under the same CAS lock, preserving explicit chat attachments.
    /// Test: `assistant_projects_preserve_explicit_chat_attachments`.
    pub fn update_assistant_projects(
        &self,
        revision: &str,
        paths: &[String],
        sources: &[SourceDescriptor],
        now: DateTime<Utc>,
    ) -> Result<KnowledgeState> {
        if paths.len() > 64
            || paths
                .iter()
                .any(|p| p.len() > 4096 || !std::path::Path::new(p).is_absolute())
        {
            return Err(KnowledgeError::BadRequest(
                "Invalid assistant project selection".into(),
            ));
        }
        self.mutate(revision, |state| {
            state.assistant_projects = paths.to_vec();
            state.assistant_projects.sort();
            state.assistant_projects.dedup();
            planning::reconcile_state(state, sources, now)
        })
    }
    pub fn extend_history(
        &self,
        revision: &str,
        months: u32,
        now: DateTime<Utc>,
    ) -> Result<KnowledgeState> {
        self.mutate(revision, |s| {
            s.history_months = s
                .history_months
                .checked_add(months)
                .filter(|m| months > 0 && *m <= 120)
                .ok_or_else(|| {
                    KnowledgeError::BadRequest(
                        "History extension must be positive and total at most 120 months".into(),
                    )
                })?;
            planning::reconcile_state(s, &s.sources.clone(), now)
        })
    }
    pub fn admit_event(
        &self,
        revision: &str,
        source_id: &str,
        source_revision: &str,
        event_id: &str,
        event_time: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<KnowledgeState> {
        if event_id.is_empty() || event_id.len() > 1024 || event_time > now {
            return Err(KnowledgeError::BadRequest(
                "Invalid event identity or future timestamp".into(),
            ));
        }
        self.mutate(revision, |s| {
            let source = s
                .sources
                .iter()
                .find(|x| x.id == source_id && x.revision == source_revision)
                .cloned()
                .ok_or_else(|| {
                    KnowledgeError::BadRequest("Event source is not currently admitted".into())
                })?;
            if s.paused {
                return Ok(());
            }
            let key = planning::digest(&[source_id, source_revision, event_id]);
            if s.admitted_events.contains(&key) {
                return Ok(());
            }
            let window = planning::windows(s, now, true)?
                .into_iter()
                .find(|w| now >= w.start && now < w.end)
                .ok_or_else(|| {
                    KnowledgeError::InvalidState("Arrival interval unavailable".into())
                })?;
            if s.admitted_events.len() >= 10000 || s.jobs.len() >= planning::MAX_JOBS {
                return Err(KnowledgeError::InvalidState(
                    "Event admission limit reached".into(),
                ));
            }
            // An incoming record authorizes only itself, even when its timestamp predates retained history.
            let mut one = s.clone();
            one.jobs.clear();
            planning::add_job(&mut one, &source, window)?;
            let mut job = one.jobs.remove(0);
            job.id = planning::digest(&[&s.assistant_id, &key, "record"]);
            job.record = Some(RecordAdmission {
                event_digest: key.clone(),
                record_time: event_time,
            });
            s.jobs.push(job);
            s.admitted_events.push(key);
            Ok(())
        })
    }
}
#[cfg(test)]
mod tests;

/// Stable admitted event identity, shared by replay and extraction.
pub fn event_digest(source: &str, revision: &str, event: &str) -> String {
    planning::digest(&[source, revision, event])
}
