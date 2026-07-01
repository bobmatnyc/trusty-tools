//! Runtime-exit reconciliation for managed sessions (#1814).
//!
//! Why: a managed-clone session (`tm launch` / agent delegation) spins up a tmux
//! pane and runs `claude` inside it. When that inner `claude` process EXITS but
//! the tmux session itself stays alive, the pane falls back to a bare login shell
//! (`zsh`/`bash`/…) with no client attached. The existing reapers miss this case:
//! [`crate::daemon::state::DaemonState::reap_dead_managed_sessions`] (#1744) only
//! transitions a session to `Stopped` when its whole tmux *session* has
//! disappeared, and the orphan-GC ([`crate::daemon::orphan_gc`]) deliberately
//! KEEPS any session tracked by the store. So a tracked `Active` record whose
//! runtime has exited stays marked `active` forever — 30+ such phantom sessions
//! accumulated for a single project on 2026-06-29. Per the SESSION LIFECYCLE
//! principle (session ≠ process), such a session must transition `active` →
//! `stopped` (resumable, workspace + record intact), NOT be decommissioned.
//!
//! What: [`pane_runtime_exited`] is the pure per-pane gate (a bare shell with no
//! live agent child); [`find_runtime_exited`] maps live panes + store records to
//! the set of `Active` session ids whose runtime has exited; [`stop_runtime_exited`]
//! is the async reconcile step that drives those ids through the EXISTING
//! [`SessionManager::stop`] path (best-effort kill of the leftover shell + mark
//! `Stopped`); [`reap_runtime_exited_managed`] is the thin tmux-facing wrapper the
//! `reap_loop` calls each tick.
//!
//! Safety: reuses the orphan-GC's conservative, fail-closed primitives —
//! [`crate::daemon::orphan_gc::is_idle_shell`] (an allowlist of known shells; any
//! unrecognised command is treated as ACTIVE and kept) and the
//! [`ChildLivenessProbe`] belt-and-braces gate (a pane momentarily showing a shell
//! while `claude` is mid-spawn is spared because its shell PID still has a live
//! child). Because `stop` is non-destructive (the workspace and record survive and
//! the session is resumable), this reconciler errs toward correctness of state
//! without risking data loss even in the unlikely false-positive case.
//!
//! Test: `pane_runtime_exited_*`, `find_runtime_exited_*`, and the end-to-end
//! `stop_runtime_exited_transitions_active_to_stopped` /
//! `stop_runtime_exited_keeps_running_session` in the `tests` module below drive a
//! real [`SessionManager`] backed by [`crate::session_manager::FakeNoopTmuxDriver`]
//! (no real tmux) and assert the record transition.

use tracing::{info, warn};

use crate::daemon::orphan_gc::{ChildLivenessProbe, PaneInfo, is_idle_shell};
use crate::daemon::tmux::TmuxDriver;
use crate::session_manager::{
    ManagedSessionId, ManagedSessionState, SessionManager, SessionRecord,
};

/// True when the runtime inside `pane` has exited (pane dropped back to a shell).
///
/// Why: this is the single, conservative gate that decides whether a pane's agent
/// process is gone. Reusing the orphan-GC's [`is_idle_shell`] allowlist and the
/// [`ChildLivenessProbe`] second gate keeps the "is it really dead?" logic in one
/// audited place and makes it fail CLOSED — anything not provably a bare, childless
/// shell is treated as still-running.
/// What: returns `true` only when `pane_current_command` is a recognised bare shell
/// AND the `probe` finds no live child under the pane's shell PID (catching a
/// `claude` that is mid-spawn while the pane momentarily reports a shell).
/// Test: `pane_runtime_exited_true_for_bare_shell`,
/// `pane_runtime_exited_false_for_agent`, `pane_runtime_exited_false_with_live_child`.
pub fn pane_runtime_exited(pane: &PaneInfo, probe: &dyn ChildLivenessProbe) -> bool {
    is_idle_shell(&pane.pane_current_command) && !probe.has_live_child(pane.pane_pid)
}

/// Collect the ids of `Active` sessions whose tmux pane is present but exited.
///
/// Why: the reconcile step must act ONLY on sessions that are (a) still tracked as
/// `Active`, (b) still have a live tmux pane, and (c) whose pane's runtime has
/// exited. Sessions whose whole tmux session has DISAPPEARED are intentionally left
/// to [`crate::daemon::state::DaemonState::reap_dead_managed_sessions`] (#1744) so
/// the two reapers never double-act on the same record.
/// What: builds the set of session names that have at least one pane and the set
/// that have at least one *non-exited* pane (a session with any live agent pane is
/// kept). Then returns every `Active` record whose `tmux_name` is present but has
/// no live pane. Pure — no I/O — so the whole decision is unit-testable.
/// Test: `find_runtime_exited_selects_only_exited_active`,
/// `find_runtime_exited_skips_missing_pane`, `find_runtime_exited_skips_non_active`.
pub fn find_runtime_exited(
    records: &[SessionRecord],
    panes: &[PaneInfo],
    probe: &dyn ChildLivenessProbe,
) -> Vec<ManagedSessionId> {
    use std::collections::HashSet;

    let mut present: HashSet<&str> = HashSet::new();
    let mut live: HashSet<&str> = HashSet::new();
    for pane in panes {
        present.insert(pane.session_name.as_str());
        if !pane_runtime_exited(pane, probe) {
            // A session with ANY still-running pane is kept, even if a sibling
            // pane has dropped to a shell.
            live.insert(pane.session_name.as_str());
        }
    }

    records
        .iter()
        .filter(|r| matches!(r.state, ManagedSessionState::Active))
        .filter(|r| present.contains(r.tmux_name.as_str()) && !live.contains(r.tmux_name.as_str()))
        .map(|r| r.id)
        .collect()
}

/// Reconcile step: mark every runtime-exited `Active` session `Stopped`.
///
/// Why: this is the self-healing action for #1814. It routes through the EXISTING
/// [`SessionManager::stop`] path rather than reimplementing the transition, so the
/// leftover shell is killed, the workspace/record are preserved, and the session
/// remains resumable — exactly the SESSION LIFECYCLE contract.
/// What: computes the exited-session ids via [`find_runtime_exited`], calls
/// `manager.stop` on each (best-effort; a failure is logged and never aborts the
/// rest), and returns the count transitioned. Takes an already-gathered `panes`
/// slice so it is fully testable with a fake tmux driver and no real tmux binary.
/// Test: `stop_runtime_exited_transitions_active_to_stopped`,
/// `stop_runtime_exited_keeps_running_session`.
pub async fn stop_runtime_exited(
    manager: &SessionManager,
    panes: &[PaneInfo],
    probe: &(dyn ChildLivenessProbe + Sync),
) -> usize {
    let records = manager.list().await;
    let exited = find_runtime_exited(&records, panes, probe);
    let mut stopped = 0usize;
    for id in exited {
        match manager.stop(&id).await {
            Ok(record) => {
                stopped += 1;
                info!(
                    id = %id,
                    name = %record.tmux_name,
                    "runtime-reap: marked managed session Stopped (runtime process exited, #1814)"
                );
            }
            Err(e) => warn!(
                id = %id,
                "runtime-reap: failed to mark managed session Stopped: {e}"
            ),
        }
    }
    stopped
}

/// Tmux-facing wrapper: gather live managed panes, then reconcile runtime exits.
///
/// Why: the periodic `reap_loop` already holds a live [`TmuxDriver`]; this thin
/// adapter enumerates the host's managed panes (name + `pane_current_command` +
/// shell PID) and hands them to the pure [`stop_runtime_exited`] step, keeping the
/// tmux subprocess call out of the testable core.
/// What: runs [`TmuxDriver::list_managed_panes`]; on failure logs a warning and
/// reaps nothing this tick (fail-safe — better a stale `Active` record than a
/// wrongly-stopped live session). Otherwise delegates to [`stop_runtime_exited`]
/// with the production [`crate::daemon::orphan_gc::ProcessTreeProbe`] and returns
/// the count transitioned.
/// Test: the reconcile logic is unit-tested via [`stop_runtime_exited`] /
/// [`find_runtime_exited`]; this wrapper is a thin adapter exercised end-to-end by
/// the `reap_loop` against a live tmux.
pub async fn reap_runtime_exited_managed(
    manager: &SessionManager,
    driver: &TmuxDriver,
    probe: &(dyn ChildLivenessProbe + Sync),
) -> usize {
    let panes = match driver.list_managed_panes() {
        Ok(p) => p,
        Err(e) => {
            warn!("runtime-reap skipped — list_managed_panes failed: {e}");
            return 0;
        }
    };
    stop_runtime_exited(manager, &panes, probe).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::orphan_gc::AlwaysIdleProbe;
    use crate::session_manager::{FakeNoopTmuxDriver, SessionManager};
    use std::sync::Arc;

    /// A probe that always reports a live child — forces the belt-and-braces gate
    /// to spare a pane even when it momentarily shows a bare shell.
    struct AlwaysLiveProbe;
    impl ChildLivenessProbe for AlwaysLiveProbe {
        fn has_live_child(&self, _pane_pid: Option<u32>) -> bool {
            true
        }
    }

    fn pane(name: &str, cmd: &str) -> PaneInfo {
        PaneInfo {
            session_name: name.to_string(),
            pane_current_command: cmd.to_string(),
            pane_pid: Some(4242),
        }
    }

    #[test]
    fn pane_runtime_exited_true_for_bare_shell() {
        // A pane that dropped back to a login shell with no live child is exited.
        assert!(pane_runtime_exited(
            &pane("tmpm-a", "zsh"),
            &AlwaysIdleProbe
        ));
        assert!(pane_runtime_exited(
            &pane("tmpm-a", "-bash"),
            &AlwaysIdleProbe
        ));
    }

    #[test]
    fn pane_runtime_exited_false_for_agent() {
        // A pane still running the agent is NOT exited, whatever the probe says.
        assert!(!pane_runtime_exited(
            &pane("tmpm-a", "claude"),
            &AlwaysIdleProbe
        ));
        assert!(!pane_runtime_exited(
            &pane("tmpm-a", "node"),
            &AlwaysIdleProbe
        ));
    }

    #[test]
    fn pane_runtime_exited_false_with_live_child() {
        // Even a bare-shell pane is spared when a child (claude mid-spawn) is live.
        assert!(!pane_runtime_exited(
            &pane("tmpm-a", "zsh"),
            &AlwaysLiveProbe
        ));
    }

    /// Build an isolated SessionManager on a FakeNoopTmuxDriver and seed one
    /// `Active` record. Mirrors the `#1744` reap test harness so no real tmux is
    /// spawned. Returns the manager and the seeded session id.
    async fn seed_active(
        dir: &tempfile::TempDir,
        tmux_name_task: &str,
    ) -> (Arc<SessionManager>, ManagedSessionId) {
        let mgr = SessionManager::new(dir.path(), Arc::new(FakeNoopTmuxDriver))
            .await
            .expect("session manager");
        let mgr = Arc::new(mgr);
        let record = mgr
            .create(
                tmux_name_task.into(),
                Some(std::path::PathBuf::from("/tmp/test-runtime-reap")),
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create");
        let id = record.id;
        mgr.set_workspace(
            &id,
            std::path::PathBuf::from("/tmp/test-runtime-reap"),
            ManagedSessionState::Active,
        )
        .await
        .expect("set Active");
        (mgr, id)
    }

    #[tokio::test]
    async fn find_runtime_exited_selects_only_exited_active() {
        // Given an Active session whose pane reports a bare shell, the pure finder
        // must return exactly its id.
        let tmp = tempfile::TempDir::new().unwrap();
        let (mgr, id) = seed_active(&tmp, "reap-me").await;
        let record = mgr.get(&id).await.expect("get");
        let panes = vec![pane(&record.tmux_name, "zsh")];
        let exited = find_runtime_exited(&[record], &panes, &AlwaysIdleProbe);
        assert_eq!(exited, vec![id]);
    }

    #[tokio::test]
    async fn find_runtime_exited_skips_missing_pane() {
        // A session whose tmux pane is entirely GONE is left to the #1744 reaper,
        // never selected here (avoids double-acting on the same record).
        let tmp = tempfile::TempDir::new().unwrap();
        let (mgr, id) = seed_active(&tmp, "gone").await;
        let record = mgr.get(&id).await.expect("get");
        let exited = find_runtime_exited(&[record], &[], &AlwaysIdleProbe);
        assert!(exited.is_empty(), "missing pane must not be selected");
    }

    #[tokio::test]
    async fn find_runtime_exited_skips_non_active() {
        // An already-Stopped session whose pane shows a shell must NOT be
        // re-selected — the finder acts only on `Active` records.
        let tmp = tempfile::TempDir::new().unwrap();
        let (mgr, id) = seed_active(&tmp, "already-stopped").await;
        let record = mgr.get(&id).await.expect("get");
        let panes = vec![pane(&record.tmux_name, "zsh")];
        // Flip the record to Stopped before classifying.
        let mut stopped_record = record;
        stopped_record.state = ManagedSessionState::Stopped;
        let exited = find_runtime_exited(&[stopped_record], &panes, &AlwaysIdleProbe);
        assert!(exited.is_empty(), "non-Active records must be skipped");
    }

    #[tokio::test]
    async fn stop_runtime_exited_transitions_active_to_stopped() {
        // End-to-end (#1814): an Active managed session whose tmux pane has fallen
        // back to a bare shell must be transitioned to Stopped (resumable), NOT
        // decommissioned. Uses FakeNoopTmuxDriver — no real tmux.
        let tmp = tempfile::TempDir::new().unwrap();
        let (mgr, id) = seed_active(&tmp, "exited").await;
        let record = mgr.get(&id).await.expect("get");
        let panes = vec![pane(&record.tmux_name, "zsh")];

        let stopped = stop_runtime_exited(&mgr, &panes, &AlwaysIdleProbe).await;
        assert_eq!(stopped, 1, "exactly one session must be stopped");

        let after = mgr.get(&id).await.expect("get after reap");
        assert_eq!(
            after.state,
            ManagedSessionState::Stopped,
            "runtime-exited Active session must become Stopped (resumable), #1814"
        );
    }

    #[tokio::test]
    async fn stop_runtime_exited_keeps_running_session() {
        // A session whose pane still runs `claude` must be left Active untouched.
        let tmp = tempfile::TempDir::new().unwrap();
        let (mgr, id) = seed_active(&tmp, "running").await;
        let record = mgr.get(&id).await.expect("get");
        let panes = vec![pane(&record.tmux_name, "claude")];

        let stopped = stop_runtime_exited(&mgr, &panes, &AlwaysIdleProbe).await;
        assert_eq!(stopped, 0, "a running agent session must not be stopped");

        let after = mgr.get(&id).await.expect("get after reap");
        assert_eq!(
            after.state,
            ManagedSessionState::Active,
            "a session still running claude must stay Active"
        );
    }
}
