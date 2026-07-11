//! Boot-time reconciliation of daemon state against live tmux sessions.
//!
//! Why: `reconcile_on_boot` grew large enough that leaving it inline pushed
//! `manager.rs` over its 500-SLOC production cap (a #2379 field addition —
//! `SessionRecord::deliverable_id` — was the field that tipped it over).
//! Extracting it here follows the exact precedent `create.rs`/`reactivate.rs`/
//! `hook_sync.rs` already established for this file: a single, cohesive
//! method moves to its own sibling module rather than the cap being gamed or
//! raised. No behavior changes — this is a pure relocation.
//! What: [`SessionManager::reconcile_on_boot`] lists live tmux sessions,
//! cross-references them against the persisted store (live → `Active`, gone →
//! `Stopped`, unknown-to-the-store → adopted as an external `Active` record),
//! and optionally auto-resumes every session it marked `Stopped`.
//! Test: `manager_reconcile_gone_tmux_yields_stopped`,
//! `manager_reconcile_adopts_new_prefix_session` in `tests.rs`.

use std::path::PathBuf;

use chrono::Utc;
use tracing::{info, warn};

use super::manager::{ManagedError, ReconcileReport, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};

impl SessionManager {
    /// Reconcile daemon state against live tmux sessions after a restart.
    ///
    /// Why: the daemon may have crashed or been restarted while sessions were
    /// running. A persisted record whose tmux session is GONE (e.g. after reboot)
    /// must become `Stopped` (resumable), NOT a "lost" or "orphaned" session —
    /// a stopped runtime does NOT mean the session itself is lost.
    /// What: lists all tmux sessions, filters to managed names (current `tm-`
    /// or legacy `tmpm-`/`trusty-mpm-`, issue #1955) via
    /// [`crate::core::names::is_managed_session_name`], cross-references
    /// against the store: live → `Active`; gone → `Stopped` (unless already
    /// `Decommissioned`). External managed sessions unknown to the store are
    /// adopted as `Active`.
    /// When `auto_resume` is true, all `Stopped` sessions are immediately resumed.
    /// Test: `manager_reconcile_gone_tmux_yields_stopped`,
    /// `manager_reconcile_adopts_new_prefix_session`.
    pub async fn reconcile_on_boot(
        &self,
        auto_resume: bool,
    ) -> Result<ReconcileReport, ManagedError> {
        let live_names: std::collections::HashSet<String> = self
            .tmux
            .list_sessions()
            .unwrap_or_else(|e| {
                warn!("reconcile: list_sessions failed: {e}; assuming no live sessions");
                Vec::new()
            })
            .into_iter()
            .filter(|n| crate::core::names::is_managed_session_name(n))
            .collect();

        let mut report = ReconcileReport::default();
        let mut guard = self.store.write().await;
        let mut all_records = guard.all().await?;

        // Build a set of store-known tmux names.
        let known_names: std::collections::HashSet<String> =
            all_records.iter().map(|r| r.tmux_name.clone()).collect();

        // Collect ids of sessions to auto-resume after the write guard is released.
        let mut to_resume: Vec<ManagedSessionId> = Vec::new();

        // Backfill source_id (#1780) before the loop — one spawn_blocking for
        // all N null-source_id records instead of N blocking git calls inside
        // the async loop. Idempotent; failures are silently skipped per record.
        super::adopt::backfill_source_ids(&mut all_records).await;

        // Reconcile store records against live sessions.
        for mut record in all_records {
            // Decommissioned tombstones are never touched by reconciliation.
            if matches!(record.state, ManagedSessionState::Decommissioned) {
                continue;
            }

            if live_names.contains(&record.tmux_name) {
                // Session is alive — re-adopt as Active.
                record.state = ManagedSessionState::Active;
                report.adopted.push(record.tmux_name.clone());
                info!(name = %record.tmux_name, "reconcile: re-adopted live session");
            } else {
                // Session is gone — mark Stopped (resumable), never Orphaned.
                record.state = ManagedSessionState::Stopped;
                report.stopped.push(record.id.to_string());
                warn!(name = %record.tmux_name, "reconcile: tmux session gone, marked Stopped (workspace intact, resumable)");
                if auto_resume {
                    to_resume.push(record.id);
                }
            }
            guard.upsert(record).await?;
        }

        // Adopt tmux sessions the store has never seen. Issue #2158: an
        // adopted session must never be left as a silent half-record with
        // cwd/workspace_path permanently stubbed to "/unknown" — re-resolve
        // the pane's real working directory via `get_pane_cwd` (the same
        // primitive the idle-auto-stop snapshot uses) so it can be validated
        // and provisioned like any other managed workspace; a pane whose cwd
        // cannot be resolved is left CLEARLY flagged as unmanaged in `task`
        // rather than an indistinguishable-from-normal "adopted session".
        let mut newly_resolved: Vec<(ManagedSessionId, PathBuf)> = Vec::new();
        for name in &live_names {
            if !known_names.contains(name) {
                let resolved_cwd = self.tmux.get_pane_cwd(name).filter(|p| p.is_dir());
                let (cwd, workspace_path, task) = match &resolved_cwd {
                    Some(path) => (
                        path.clone(),
                        Some(path.clone()),
                        "adopted session".to_string(),
                    ),
                    None => (
                        PathBuf::from("/unknown"),
                        None,
                        "adopted session (unmanaged — workspace path could not be resolved)"
                            .to_string(),
                    ),
                };
                let id = ManagedSessionId::new();
                let external = SessionRecord {
                    id,
                    tmux_name: name.clone(),
                    cwd,
                    task,
                    state: ManagedSessionState::Active,
                    created_at: Utc::now(),
                    last_activity_at: None,
                    workspace_path,
                    repo_url: None,
                    branch: None,
                    pending_decision: None,
                    proposed_default: None,
                    correlation: Default::default(),
                    // Externally-created tmux sessions have unknown provenance;
                    // assume the default (claude-code) backend.
                    runtime: crate::runtime::RuntimeKind::default(),
                    // Adopted external sessions are NEVER ephemeral: their
                    // provenance is unknown, so they must never be auto-reaped.
                    ephemeral: false,
                    // Externally-adopted sessions are NEVER SM-owned — the SM
                    // did not create the workspace; decommission must not delete
                    // it (#1511).
                    workspace_owned: false,
                    // External sessions have no tracked source project.
                    source_id: None,
                    claude_session_id: None,
                    scrollback_path: None,
                    last_cwd: None,
                    // External sessions have no known Deliverable linkage.
                    deliverable_id: None,
                };
                if let Some(path) = resolved_cwd {
                    newly_resolved.push((id, path));
                }
                guard.upsert(external).await?;
                report.external_adopted.push(name.clone());
                info!(name = %name, "reconcile: adopted external managed session");
            }
        }

        // Release write guard before auto-resume (which needs its own locks).
        drop(guard);

        // #2306: collapse stale duplicate records per project. Runs after the
        // main reconcile persisted states and the write guard was dropped;
        // decommissions quiesced (no-live-tmux) losers via the safe path and
        // prunes any of them from the pending auto-resume set. See dedup.rs.
        self.dedup_stale_duplicates(&mut to_resume).await?;

        // #2158: best-effort validate + auto-repair the deployed `.claude/`
        // payload for every adopted session whose workspace was resolved
        // above. Non-fatal — a repair failure only leaves the workspace as-is;
        // the operator can run `tm validate --path <dir> --repair` manually.
        for (id, workspace) in newly_resolved {
            let fw = crate::core::paths::FrameworkPaths::for_managed_workspace(&workspace);
            let outcome = crate::core::deploy_validate::validate_and_repair(&fw, &workspace, None);
            if outcome.before.is_complete() {
                continue;
            }
            if outcome.is_complete() {
                info!(id = %id, "reconcile: auto-repaired adopted session's deployment");
            } else {
                warn!(
                    id = %id,
                    gaps = outcome.after.gaps.len(),
                    "reconcile: adopted session's deployment remains incomplete after auto-repair"
                );
            }
        }

        if auto_resume && !to_resume.is_empty() {
            info!(
                "reconcile: auto_resume=true, resuming {} stopped sessions",
                to_resume.len()
            );
            for sid in to_resume {
                if let Err(e) = self.resume(&sid).await {
                    warn!(id = %sid, "reconcile: auto_resume failed: {e}");
                }
            }
        }

        Ok(report)
    }
}
