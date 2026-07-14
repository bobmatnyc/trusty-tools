//! In-memory progress registry for asynchronous managed-session provisioning
//! (#2605).
//!
//! Why: `POST /api/v1/sessions/managed` used to run the whole workspace
//! provision (git clone / `clone --bare` + worktree + agent/skill deploy)
//! SYNCHRONOUSLY inside the request handler. On a large repo the clone
//! outlasts the CLI's HTTP timeout, so the POST fails even though the daemon
//! keeps working — and the blocking clone runs on the request path, degrading
//! `/health` responsiveness. The fix moves provisioning onto a background task
//! and returns a job id immediately; this registry is where that background
//! task records live phase/detail progress and its terminal outcome so the
//! poll route (`GET .../{id}/provision-status`) and the CLI can follow along.
//! What: [`ProvisioningLifecycle`] (in-flight vs terminal), [`ProvisioningProgress`]
//! (the per-job snapshot), and [`ProvisioningRegistry`] (a `DashMap` keyed by
//! job id, with begin/update/finish/get + stale-entry pruning). No global
//! state — the registry is a field on `DaemonState`, shared via its `Arc`.
//! Test: `registry_lifecycle_provisioning_to_ready`,
//! `registry_lifecycle_provisioning_to_failed`, `update_stage_ignored_after_terminal`,
//! `prune_stale_removes_only_old_terminal_entries` in the `tests` submodule.

use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;

use crate::core::provisioning_stage::ProvisioningStage;

/// How long a terminal (ready/failed) job snapshot is retained before
/// [`ProvisioningRegistry::begin`] prunes it.
///
/// Why: the registry must not grow without bound over a long-lived daemon, but
/// a terminal entry has to outlive the CLI's poll loop (which reads it once it
/// flips to ready/failed) plus a comfortable margin for a slow/backgrounded
/// operator. Ten minutes is far longer than any poll loop yet keeps the map
/// small.
/// What: the age threshold, measured from `finished_at`, past which a terminal
/// entry is dropped on the next `begin`.
/// Test: `prune_stale_removes_only_old_terminal_entries`.
const TERMINAL_TTL_MINUTES: i64 = 10;

/// Lifecycle of one background provisioning job.
///
/// Why: the poll route and CLI need a single, unambiguous state to branch on —
/// keep waiting, attach, or surface an error — independent of the coarse
/// [`ProvisioningStage`] (which only says WHICH step, not whether the whole job
/// succeeded or failed).
/// What: three states — `Provisioning` (in flight), `Ready` (the session is up
/// and attachable), `Failed` (provisioning errored; `error` carries why).
/// Test: `registry_lifecycle_provisioning_to_ready`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvisioningLifecycle {
    /// Provisioning is still running on the background task.
    Provisioning,
    /// The session is provisioned, spawned, and ready to attach.
    Ready,
    /// Provisioning failed; the record (if any) is left for post-mortem.
    Failed,
}

impl ProvisioningLifecycle {
    /// Stable lowercase wire string for the poll-route JSON `state` field.
    ///
    /// Why: a fixed string keeps the wire contract independent of variant
    /// renames and matches the lowercase state vocabulary the rest of the
    /// managed API already speaks (`active`, `stopped`, …).
    /// What: `"provisioning"` | `"ready"` | `"failed"`.
    /// Test: `registry_lifecycle_provisioning_to_ready`.
    pub fn wire(&self) -> &'static str {
        match self {
            ProvisioningLifecycle::Provisioning => "provisioning",
            ProvisioningLifecycle::Ready => "ready",
            ProvisioningLifecycle::Failed => "failed",
        }
    }

    /// Whether this is a terminal (no further transitions) state.
    ///
    /// Why: stage updates arriving after completion (a late SSE frame) must not
    /// overwrite a terminal snapshot, and only terminal entries are eligible
    /// for TTL pruning.
    /// What: `true` for `Ready`/`Failed`, `false` for `Provisioning`.
    /// Test: `update_stage_ignored_after_terminal`.
    pub fn is_terminal(&self) -> bool {
        !matches!(self, ProvisioningLifecycle::Provisioning)
    }
}

/// A point-in-time snapshot of one background provisioning job.
///
/// Why: the poll route serialises this into its response; keeping every field
/// the CLI might render (coarse stage, fine detail, final id/name, error) in
/// one struct means the route handler is a pure mapping with no extra lookups.
/// What: the lifecycle, the latest coarse [`ProvisioningStage`] and optional
/// fine `detail`, the final `session_id`/`name` (populated on success — the
/// final record id may differ from the job id when the daemon reconnects to an
/// existing session), an `error` (populated on failure), and timestamps.
/// Test: `registry_lifecycle_provisioning_to_ready`.
#[derive(Debug, Clone)]
pub struct ProvisioningProgress {
    /// Current lifecycle state.
    pub lifecycle: ProvisioningLifecycle,
    /// Latest coarse provisioning stage, if any has been observed yet.
    pub stage: Option<ProvisioningStage>,
    /// Fine-grained detail within the current stage (e.g. clone percent).
    pub detail: Option<String>,
    /// Final managed-session id once ready (may differ from the job id on a
    /// reconnect-to-existing-session outcome).
    pub session_id: Option<String>,
    /// tmux session name once ready.
    pub name: Option<String>,
    /// Failure reason once failed.
    pub error: Option<String>,
    /// When the job was registered.
    pub started_at: DateTime<Utc>,
    /// When the job reached a terminal state, if it has.
    pub finished_at: Option<DateTime<Utc>>,
}

impl ProvisioningProgress {
    /// Construct a fresh in-flight snapshot with `started_at = now`.
    fn new_in_flight() -> Self {
        Self {
            lifecycle: ProvisioningLifecycle::Provisioning,
            stage: None,
            detail: None,
            session_id: None,
            name: None,
            error: None,
            started_at: Utc::now(),
            finished_at: None,
        }
    }
}

/// Concurrent registry of background provisioning jobs, keyed by job id.
///
/// Why: the async-spawn handler, its stage-updater task, and the poll-route
/// handler all touch the same job snapshots from different tokio tasks; a
/// `DashMap` gives lock-free-per-entry interior mutability so the registry can
/// live as a plain `DaemonState` field shared via the daemon `Arc` (no global
/// state, no outer `Mutex`).
/// What: `begin`/`update_stage`/`finish_ready`/`finish_failed`/`get` over a
/// `DashMap<String, ProvisioningProgress>`, plus stale-entry pruning driven off
/// [`TERMINAL_TTL_MINUTES`].
/// Test: the `tests` submodule.
#[derive(Debug, Default)]
pub struct ProvisioningRegistry {
    entries: DashMap<String, ProvisioningProgress>,
}

impl ProvisioningRegistry {
    /// Register a new in-flight job under `job_id`, pruning stale terminals first.
    ///
    /// Why: the async-spawn handler calls this before returning `202` so the
    /// very first poll — which can race the background task's first stage
    /// event — always finds an entry (state `provisioning`) rather than a 404.
    /// What: prunes expired terminal entries, then inserts a fresh
    /// [`ProvisioningProgress::new_in_flight`] for `job_id`.
    /// Test: `registry_lifecycle_provisioning_to_ready`.
    pub fn begin(&self, job_id: &str) {
        self.prune_stale();
        self.entries
            .insert(job_id.to_string(), ProvisioningProgress::new_in_flight());
    }

    /// Record the latest coarse stage (and optional fine detail) for `job_id`.
    ///
    /// Why: the stage-updater task translates each `provisioning_stage` SSE
    /// frame for this job into a registry update so the poll route reflects
    /// live movement.
    /// What: if an entry exists and is NOT terminal, sets its `stage` and
    /// `detail`; a stage update after completion (a late frame) is ignored so it
    /// cannot clobber a terminal snapshot.
    /// Test: `update_stage_ignored_after_terminal`.
    pub fn update_stage(&self, job_id: &str, stage: ProvisioningStage, detail: Option<String>) {
        if let Some(mut e) = self.entries.get_mut(job_id)
            && !e.lifecycle.is_terminal()
        {
            e.stage = Some(stage);
            e.detail = detail;
        }
    }

    /// Mark `job_id` ready, recording the final session id and tmux name.
    ///
    /// Why: on a successful background spawn the poll route must hand the CLI
    /// the REAL final session id and name to attach to (which can differ from
    /// the job id when the daemon reconnected to an existing session).
    /// What: sets lifecycle `Ready`, `stage = Complete`, clears `detail`, and
    /// stamps `finished_at`. A no-op if the job is unknown.
    /// Test: `registry_lifecycle_provisioning_to_ready`.
    pub fn finish_ready(&self, job_id: &str, session_id: String, name: String) {
        if let Some(mut e) = self.entries.get_mut(job_id) {
            e.lifecycle = ProvisioningLifecycle::Ready;
            e.stage = Some(ProvisioningStage::Complete);
            e.detail = None;
            e.session_id = Some(session_id);
            e.name = Some(name);
            e.finished_at = Some(Utc::now());
        }
    }

    /// Mark `job_id` failed with a human-readable `error`.
    ///
    /// Why: a provisioning failure (bad ref, clone error, gate refusal) must be
    /// surfaced to the CLI as a clear terminal state, not an indefinite wait.
    /// What: sets lifecycle `Failed`, records `error`, and stamps `finished_at`.
    /// A no-op if the job is unknown.
    /// Test: `registry_lifecycle_provisioning_to_failed`.
    pub fn finish_failed(&self, job_id: &str, error: String) {
        if let Some(mut e) = self.entries.get_mut(job_id) {
            e.lifecycle = ProvisioningLifecycle::Failed;
            e.error = Some(error);
            e.finished_at = Some(Utc::now());
        }
    }

    /// Fetch a clone of the current snapshot for `job_id`, if present.
    ///
    /// Why: the poll route needs an owned snapshot to serialise without holding
    /// a `DashMap` guard across the response build.
    /// What: returns `Some(ProvisioningProgress)` clone or `None`.
    /// Test: `registry_lifecycle_provisioning_to_ready`.
    pub fn get(&self, job_id: &str) -> Option<ProvisioningProgress> {
        self.entries.get(job_id).map(|e| e.clone())
    }

    /// Drop terminal entries whose `finished_at` is older than the TTL.
    ///
    /// Why: bounds the registry's memory over a long-lived daemon without
    /// evicting an in-flight job or a just-completed one the CLI is still about
    /// to read.
    /// What: retains every non-terminal entry and every terminal entry younger
    /// than [`TERMINAL_TTL_MINUTES`].
    /// Test: `prune_stale_removes_only_old_terminal_entries`.
    pub fn prune_stale(&self) {
        let cutoff = Utc::now() - Duration::minutes(TERMINAL_TTL_MINUTES);
        self.entries.retain(|_, e| match e.finished_at {
            Some(finished) => finished >= cutoff,
            None => true,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_lifecycle_provisioning_to_ready() {
        let reg = ProvisioningRegistry::default();
        reg.begin("job-1");

        let snap = reg.get("job-1").expect("entry after begin");
        assert_eq!(snap.lifecycle, ProvisioningLifecycle::Provisioning);
        assert!(snap.stage.is_none());

        reg.update_stage(
            "job-1",
            ProvisioningStage::CloningRepo,
            Some("Receiving objects: 42%".into()),
        );
        let snap = reg.get("job-1").expect("entry after stage");
        assert_eq!(snap.stage, Some(ProvisioningStage::CloningRepo));
        assert_eq!(snap.detail.as_deref(), Some("Receiving objects: 42%"));

        reg.finish_ready("job-1", "sess-9".into(), "tm-repo-01".into());
        let snap = reg.get("job-1").expect("entry after ready");
        assert_eq!(snap.lifecycle, ProvisioningLifecycle::Ready);
        assert_eq!(snap.stage, Some(ProvisioningStage::Complete));
        assert_eq!(snap.session_id.as_deref(), Some("sess-9"));
        assert_eq!(snap.name.as_deref(), Some("tm-repo-01"));
        assert!(snap.detail.is_none());
        assert!(snap.finished_at.is_some());
    }

    #[test]
    fn registry_lifecycle_provisioning_to_failed() {
        let reg = ProvisioningRegistry::default();
        reg.begin("job-2");
        reg.finish_failed("job-2", "workspace provisioning failed: boom".into());

        let snap = reg.get("job-2").expect("entry after failed");
        assert_eq!(snap.lifecycle, ProvisioningLifecycle::Failed);
        assert_eq!(
            snap.error.as_deref(),
            Some("workspace provisioning failed: boom")
        );
        assert!(snap.finished_at.is_some());
    }

    #[test]
    fn update_stage_ignored_after_terminal() {
        let reg = ProvisioningRegistry::default();
        reg.begin("job-3");
        reg.finish_ready("job-3", "sess".into(), "name".into());

        // A late stage frame must not resurrect/clobber the terminal snapshot.
        reg.update_stage("job-3", ProvisioningStage::CloningRepo, None);
        let snap = reg.get("job-3").expect("entry");
        assert_eq!(snap.lifecycle, ProvisioningLifecycle::Ready);
        assert_eq!(snap.stage, Some(ProvisioningStage::Complete));
    }

    #[test]
    fn prune_stale_removes_only_old_terminal_entries() {
        let reg = ProvisioningRegistry::default();

        // In-flight: never pruned.
        reg.begin("in-flight");

        // Fresh terminal: retained.
        reg.begin("fresh-done");
        reg.finish_ready("fresh-done", "s".into(), "n".into());

        // Old terminal: hand-age its finished_at past the TTL, then prune.
        reg.begin("old-done");
        reg.finish_failed("old-done", "err".into());
        if let Some(mut e) = reg.entries.get_mut("old-done") {
            e.finished_at = Some(Utc::now() - Duration::minutes(TERMINAL_TTL_MINUTES + 1));
        }

        reg.prune_stale();

        assert!(reg.get("in-flight").is_some(), "in-flight must survive");
        assert!(
            reg.get("fresh-done").is_some(),
            "fresh terminal must survive"
        );
        assert!(
            reg.get("old-done").is_none(),
            "stale terminal must be pruned"
        );
    }
}
