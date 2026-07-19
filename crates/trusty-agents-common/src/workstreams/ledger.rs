//! [`WorkstreamLedger`] — the DOC-44 Phase 2 heterogeneous workstream
//! registry (issue #3008, twin Phase 2, DOC-44 §8 "Workstream Ledger &
//! Heterogeneous Registry").
//!
//! Why: the lead agent (DOC-44 Layer 2, not built by this ticket) needs
//! create/list/update/query over workstreams that span both `tm` and
//! `tcode`. DOC-44 §4.2 describes the registry as "trusty-memory-backed, or
//! a heterogeneous ledger in trusty-agents-common" — this ticket builds the
//! latter as the working default (see [`super::LedgerRecovery`] for how a
//! future trusty-memory-backed source plugs in without changing this type's
//! API). The on-disk shape and atomic-write approach deliberately mirror the
//! existing `session_registry::SessionsRegistry` (flat JSON, `open` +
//! typed operations) and `agents::manifest::atomic_write`
//! (temp-file-then-`rename`) precedents already in this crate, rather than
//! importing either directly: `session_registry` is a narrow, already-wired
//! run-tracking registry with a live call site in
//! `trusty-agents::runtime::startup` (out of this ticket's file scope —
//! see that module's docs), and `agents::manifest::atomic_write` is scoped
//! to the agent-deploy-ownership domain. A four-line temp+rename helper
//! duplicated across two unrelated domains is below the "same domain
//! over 80 percent similarity, different domain over 50 percent similarity"
//! consolidation threshold this workspace uses (CLAUDE.md "Duplicate
//! Elimination"); a future `fs_util` extraction is noted as follow-up if a
//! third caller appears.
//! What: [`WorkstreamLedger`] reads/writes a single `workstreams.json` file
//! under a caller-supplied state dir. Every mutation (`create`,
//! `update_status`, `add_session`) is serialized by an in-process
//! [`std::sync::Mutex`] and persisted via temp-file-then-`rename` (atomic on
//! any POSIX filesystem when both paths share a mount point). **Concurrency
//! model:** this guarantees safety across concurrent *threads within one
//! process* (the mutex serializes read-modify-write cycles) but, unlike
//! `trusty-agents::state_writer::atomic_write` (which takes an `fs4`
//! advisory file lock), does **not** guarantee safety across *multiple
//! processes* writing the same ledger file concurrently — the last writer's
//! `rename` wins and an interleaved reader sees either the old or new file,
//! never a torn one, but two processes racing a read-modify-write can lose
//! an update. `fs4` isn't in this crate's dependency set (`trusty-agents-common`
//! is a dependency leaf; adding a new external crate for one module was
//! judged not worth it for the Phase 2 ticket) — this is the documented
//! single-writer-per-process assumption the issue asked for. Callers that
//! need cross-process safety should route all ledger writes through one
//! process (the eventual lead agent, DOC-44 Layer 2) rather than writing
//! from multiple processes directly.
//! Test: `ledger::tests` covers create/list/get/query, `update_status` and
//! `add_session` round-tripping through a fresh read, the `NotFound` and
//! `ImmutableField` error paths, and `concurrent_creates_do_not_lose_writes`
//! (proves the in-process mutex serializes concurrent creates).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::error::LedgerError;
use super::types::{NewWorkstream, Workstream, WorkstreamStatus};

/// Filename of the ledger within a state dir.
const LEDGER_FILE: &str = "workstreams.json";

/// On-disk JSON shape: `{"workstreams": [...]}`.
///
/// Why: wrapping the array in a top-level object (matching
/// `session_registry::SessionsFile`'s precedent) leaves room for future
/// metadata (schema_version) without breaking parsing.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct LedgerFile {
    #[serde(default)]
    workstreams: Vec<Workstream>,
}

/// JSON-backed heterogeneous workstream ledger.
///
/// Why/What: see module docs.
pub struct WorkstreamLedger {
    path: PathBuf,
    /// Serializes read-modify-write cycles within this process. See module
    /// docs' "Concurrency model" for what this does and does not guarantee.
    write_lock: Mutex<()>,
}

impl WorkstreamLedger {
    /// Open (or create) a ledger rooted at `state_dir/workstreams.json`.
    ///
    /// Why: centralizes path construction so callers only pass the state
    /// dir, matching `SessionsRegistry::open`'s precedent.
    /// What: ensures the parent dir exists; does not create the file until
    /// the first write.
    /// Test: `ledger::tests::open_creates_state_dir`.
    pub fn open(state_dir: &Path) -> Result<Self, LedgerError> {
        std::fs::create_dir_all(state_dir)?;
        Ok(Self {
            path: state_dir.join(LEDGER_FILE),
            write_lock: Mutex::new(()),
        })
    }

    /// Path of the backing file (for diagnostics / tests).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Create a new workstream. Assigns `id` (UUIDv4), `created_at` /
    /// `last_updated` (now), and `status = Proposed`.
    ///
    /// Why: the entry point for the lead agent to bring a new workstream
    /// into the ledger — see [`NewWorkstream`]'s docs for why callers can't
    /// forge these fields directly.
    /// What: appends the new record and persists atomically before
    /// returning it.
    /// Test: `ledger::tests::create_assigns_id_and_proposed_status`.
    pub fn create(&self, new: NewWorkstream) -> Result<Workstream, LedgerError> {
        let _guard = self.lock();
        let mut file = self.read()?;
        let now = Utc::now();
        let ws = Workstream {
            id: Uuid::new_v4().to_string(),
            ticket_ref: new.ticket_ref,
            title: new.title,
            assigned_harness: new.assigned_harness,
            session_ids: Vec::new(),
            status: WorkstreamStatus::Proposed,
            created_at: now,
            target_date: new.target_date,
            priority: new.priority,
            blockers: Vec::new(),
            dependencies: Vec::new(),
            last_updated: now,
        };
        file.workstreams.push(ws.clone());
        self.write(&file)?;
        Ok(ws)
    }

    /// Return every workstream in the ledger, insertion order.
    ///
    /// Why: the lead's fan-out poll and any CLI/diagnostic surface read
    /// this to build a full picture.
    /// Test: `ledger::tests::list_returns_all_entries_in_order`.
    pub fn list(&self) -> Result<Vec<Workstream>, LedgerError> {
        Ok(self.read()?.workstreams)
    }

    /// Point lookup for one workstream.
    ///
    /// Why: cheaper than re-listing when the caller already has an id.
    /// What: [`LedgerError::NotFound`] for an unknown `id`.
    /// Test: `ledger::tests::get_returns_not_found_for_unknown_id`.
    pub fn get(&self, id: &str) -> Result<Workstream, LedgerError> {
        self.read()?
            .workstreams
            .into_iter()
            .find(|w| w.id == id)
            .ok_or_else(|| LedgerError::NotFound(id.to_string()))
    }

    /// Return every workstream matching `predicate`.
    ///
    /// Why: a single closure-based filter covers "by harness", "by status",
    /// "by priority", or any combination without a combinatorial explosion
    /// of `list_by_*` methods.
    /// What: applies `predicate` over a fresh `list()` snapshot.
    /// Test: `ledger::tests::query_filters_by_harness`.
    pub fn query(
        &self,
        predicate: impl Fn(&Workstream) -> bool,
    ) -> Result<Vec<Workstream>, LedgerError> {
        Ok(self.list()?.into_iter().filter(|w| predicate(w)).collect())
    }

    /// Update a workstream's `status` and bump `last_updated`.
    ///
    /// Why: the primary lifecycle transition (DOC-44 §4.2's
    /// `WorkstreamStatus`) a caller performs after creation.
    /// What: [`LedgerError::NotFound`] for an unknown `id`.
    /// Test: `ledger::tests::update_status_persists_and_bumps_last_updated`.
    pub fn update_status(
        &self,
        id: &str,
        status: WorkstreamStatus,
    ) -> Result<Workstream, LedgerError> {
        self.mutate(id, |w| w.status = status)
    }

    /// Attach a backend session id to a workstream (idempotent — adding the
    /// same id twice is a no-op).
    ///
    /// Why: `tcode` workstreams may accumulate more than one concurrent
    /// session (DOC-44 §4.2); a `tm` workstream typically records exactly
    /// one, but the operation is harness-agnostic — the ledger doesn't
    /// enforce a cardinality limit.
    /// What: [`LedgerError::NotFound`] for an unknown `id`.
    /// Test: `ledger::tests::add_session_is_idempotent`,
    /// `ledger::tests::workstream_from_connector_session_info`.
    pub fn add_session(
        &self,
        id: &str,
        session_id: impl Into<String>,
    ) -> Result<Workstream, LedgerError> {
        let session_id = session_id.into();
        self.mutate(id, |w| {
            if !w.session_ids.contains(&session_id) {
                w.session_ids.push(session_id.clone());
            }
        })
    }

    /// Shared mutate-in-place primitive backing every update method.
    ///
    /// Why: centralizes the read-lock-mutate-check-write cycle and enforces
    /// the DOC-44 §4.1 invariant that `assigned_harness` never changes after
    /// creation — every public mutation method routes through here so the
    /// invariant can't be bypassed by a future addition.
    /// What: locks, reads, finds the target record, snapshots
    /// `assigned_harness`, applies `f`, rejects the whole mutation
    /// (discarding the in-memory change without persisting it) if `f`
    /// changed `assigned_harness`, otherwise bumps `last_updated` and
    /// persists.
    /// Test: `ledger::tests::mutating_assigned_harness_is_rejected`.
    fn mutate(&self, id: &str, f: impl FnOnce(&mut Workstream)) -> Result<Workstream, LedgerError> {
        let _guard = self.lock();
        let mut file = self.read()?;
        let ws = file
            .workstreams
            .iter_mut()
            .find(|w| w.id == id)
            .ok_or_else(|| LedgerError::NotFound(id.to_string()))?;
        let harness_before = ws.assigned_harness;
        f(ws);
        if ws.assigned_harness != harness_before {
            return Err(LedgerError::ImmutableField("assigned_harness"));
        }
        ws.last_updated = Utc::now();
        let updated = ws.clone();
        self.write(&file)?;
        Ok(updated)
    }

    /// Acquire the write lock, recovering from a poisoned mutex.
    ///
    /// Why: a panic inside a previous mutation (e.g. an assertion in test
    /// code) should not permanently wedge every future call on this ledger
    /// instance — the lock only ever guards an in-memory `Vec` clone plus a
    /// file write, so recovering the poisoned guard and continuing is safe.
    fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.write_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn read(&self) -> Result<LedgerFile, LedgerError> {
        match std::fs::read(&self.path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(LedgerFile::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Write `file` atomically via temp-then-`rename` — see module docs'
    /// "Concurrency model" for the guarantee this does and does not make.
    fn write(&self, file: &LedgerFile) -> Result<(), LedgerError> {
        let bytes = serde_json::to_vec_pretty(file)?;
        let tmp = {
            let mut s = self.path.as_os_str().to_owned();
            s.push(".tmp");
            PathBuf::from(s)
        };
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use tempfile::tempdir;

    use super::*;
    use crate::connectors::SessionInfo;
    use crate::workstreams::{Harness, Priority};

    fn sample_new(harness: Harness) -> NewWorkstream {
        NewWorkstream {
            ticket_ref: "#3008".into(),
            title: "heterogeneous ledger".into(),
            assigned_harness: harness,
            priority: Priority::P1,
            target_date: None,
        }
    }

    #[test]
    fn open_creates_state_dir() {
        let tmp = tempdir().unwrap();
        let state_dir = tmp.path().join("nested").join("state");
        let ledger = WorkstreamLedger::open(&state_dir).unwrap();
        assert!(state_dir.exists());
        assert_eq!(ledger.path(), state_dir.join(LEDGER_FILE));
    }

    #[test]
    fn create_assigns_id_and_proposed_status() {
        let tmp = tempdir().unwrap();
        let ledger = WorkstreamLedger::open(tmp.path()).unwrap();
        let ws = ledger.create(sample_new(Harness::Tm)).unwrap();
        assert!(!ws.id.is_empty());
        assert_eq!(ws.status, WorkstreamStatus::Proposed);
        assert_eq!(ws.assigned_harness, Harness::Tm);
        assert!(ws.session_ids.is_empty());
        assert_eq!(ws.created_at, ws.last_updated);
    }

    #[test]
    fn list_returns_all_entries_in_order() {
        let tmp = tempdir().unwrap();
        let ledger = WorkstreamLedger::open(tmp.path()).unwrap();
        let a = ledger.create(sample_new(Harness::Tm)).unwrap();
        let b = ledger.create(sample_new(Harness::Tcode)).unwrap();
        let ids: Vec<String> = ledger.list().unwrap().into_iter().map(|w| w.id).collect();
        assert_eq!(ids, vec![a.id, b.id]);
    }

    #[test]
    fn get_returns_not_found_for_unknown_id() {
        let tmp = tempdir().unwrap();
        let ledger = WorkstreamLedger::open(tmp.path()).unwrap();
        let err = ledger.get("ghost").unwrap_err();
        assert!(matches!(err, LedgerError::NotFound(id) if id == "ghost"));
    }

    #[test]
    fn query_filters_by_harness() {
        let tmp = tempdir().unwrap();
        let ledger = WorkstreamLedger::open(tmp.path()).unwrap();
        ledger.create(sample_new(Harness::Tm)).unwrap();
        ledger.create(sample_new(Harness::Tcode)).unwrap();
        ledger.create(sample_new(Harness::Tm)).unwrap();
        let tm_only = ledger.query(|w| w.assigned_harness == Harness::Tm).unwrap();
        assert_eq!(tm_only.len(), 2);
        assert!(tm_only.iter().all(|w| w.assigned_harness == Harness::Tm));
    }

    #[test]
    fn update_status_persists_and_bumps_last_updated() {
        let tmp = tempdir().unwrap();
        let ledger = WorkstreamLedger::open(tmp.path()).unwrap();
        let created = ledger.create(sample_new(Harness::Tcode)).unwrap();
        let updated = ledger
            .update_status(&created.id, WorkstreamStatus::InProgress)
            .unwrap();
        assert_eq!(updated.status, WorkstreamStatus::InProgress);
        assert!(updated.last_updated >= created.last_updated);

        // Re-read from a fresh ledger handle to prove it was persisted, not
        // just mutated in-memory.
        let reopened = WorkstreamLedger::open(tmp.path()).unwrap();
        let fetched = reopened.get(&created.id).unwrap();
        assert_eq!(fetched.status, WorkstreamStatus::InProgress);
    }

    #[test]
    fn add_session_is_idempotent() {
        let tmp = tempdir().unwrap();
        let ledger = WorkstreamLedger::open(tmp.path()).unwrap();
        let created = ledger.create(sample_new(Harness::Tm)).unwrap();
        ledger.add_session(&created.id, "sess-1").unwrap();
        ledger.add_session(&created.id, "sess-1").unwrap();
        let fetched = ledger.get(&created.id).unwrap();
        assert_eq!(fetched.session_ids, vec!["sess-1".to_string()]);
    }

    #[test]
    fn mutating_assigned_harness_is_rejected() {
        let tmp = tempdir().unwrap();
        let ledger = WorkstreamLedger::open(tmp.path()).unwrap();
        let created = ledger.create(sample_new(Harness::Tm)).unwrap();
        let err = ledger
            .mutate(&created.id, |w| w.assigned_harness = Harness::Tcode)
            .unwrap_err();
        assert!(matches!(
            err,
            LedgerError::ImmutableField("assigned_harness")
        ));
        // The rejected mutation must not have been persisted.
        let fetched = ledger.get(&created.id).unwrap();
        assert_eq!(fetched.assigned_harness, Harness::Tm);
    }

    /// Proves a real, connector-flavored [`SessionInfo`] (Phase 1's wire
    /// type, not a mock) can be attached to a workstream — the ledger's
    /// `session_ids: Vec<String>` accepts exactly the `id` field a
    /// `WorkstreamConnector::create_session`/`list_sessions` call would
    /// return, with no adapter layer needed.
    #[test]
    fn workstream_from_connector_session_info() {
        let tmp = tempdir().unwrap();
        let ledger = WorkstreamLedger::open(tmp.path()).unwrap();

        let session = SessionInfo {
            id: "tm-sess-42".into(),
            name: "tmpm-a1b2c3".into(),
            state: "active".into(),
            task: Some("fix the flaky test".into()),
        };

        let mut new = sample_new(Harness::Tm);
        new.title = session.task.clone().unwrap_or_else(|| session.name.clone());
        let ws = ledger.create(new).unwrap();
        let ws = ledger.add_session(&ws.id, session.id.clone()).unwrap();

        assert_eq!(ws.title, "fix the flaky test");
        assert_eq!(ws.assigned_harness, Harness::Tm);
        assert_eq!(ws.session_ids, vec![session.id]);
    }

    /// Proves the in-process mutex serializes concurrent `create` calls —
    /// N threads each create one workstream on the SAME ledger instance;
    /// the final ledger must contain exactly N distinct records (no lost
    /// updates, no corrupted JSON from an interleaved write). See module
    /// docs' "Concurrency model" for the cross-process caveat this test
    /// does NOT cover.
    #[test]
    fn concurrent_creates_do_not_lose_writes() {
        let tmp = tempdir().unwrap();
        let ledger = Arc::new(WorkstreamLedger::open(tmp.path()).unwrap());
        let threads: Vec<_> = (0..16)
            .map(|i| {
                let ledger = Arc::clone(&ledger);
                thread::spawn(move || {
                    let mut new = sample_new(if i % 2 == 0 {
                        Harness::Tm
                    } else {
                        Harness::Tcode
                    });
                    new.ticket_ref = format!("#{i}");
                    ledger.create(new).unwrap()
                })
            })
            .collect();
        let created: Vec<Workstream> = threads.into_iter().map(|t| t.join().unwrap()).collect();

        let all = ledger.list().unwrap();
        assert_eq!(all.len(), 16);
        let unique_ids: std::collections::HashSet<&String> = all.iter().map(|w| &w.id).collect();
        assert_eq!(unique_ids.len(), 16, "no two creates collided on an id");
        for ws in &created {
            assert!(all.iter().any(|w| w.id == ws.id));
        }
    }
}
