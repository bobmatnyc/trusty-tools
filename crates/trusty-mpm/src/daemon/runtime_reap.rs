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
//! is the async reconcile step that drives those ids through
//! [`SessionManager::mark_runtime_exited_stopped`] — marks the record `Stopped`
//! WITHOUT killing the tmux pane (#2023 A; see the note below on why this is
//! deliberately NOT [`SessionManager::stop`]); [`reap_runtime_exited_managed`] is
//! the thin tmux-facing wrapper the `reap_loop` calls each tick.
//!
//! Safety: reuses the orphan-GC's conservative, fail-closed primitives —
//! [`crate::daemon::orphan_gc::is_idle_shell`] (an allowlist of known shells; any
//! unrecognised command is treated as ACTIVE and kept) and the
//! [`ChildLivenessProbe`] belt-and-braces gate (a pane momentarily showing a shell
//! while `claude` is mid-spawn is spared because its shell PID still has a live
//! child). Because the reconcile action only flips the record's state (the
//! workspace, record, AND tmux pane all survive untouched), this reconciler errs
//! toward correctness of state without risking data loss OR an unwanted pane
//! teardown even in the unlikely false-positive case.
//!
//! #2023 A: this reaper previously drove the SAME [`SessionManager::stop`] path
//! used by an explicit `tm session stop` — which gracefully terminates the
//! runtime and then `kill_session`s the tmux pane. That killed a pane the runtime
//! reaper itself observed was ALREADY just an idle shell (no runtime left to
//! terminate), destroying a pane a human might still be looking at ~60s after
//! their `claude` process exited. [`SessionManager::mark_runtime_exited_stopped`]
//! is the non-destructive counterpart: it transitions the record exactly like
//! `stop` does, but never calls `graceful_terminate_runtime` / `kill_session`, so
//! the now-idle-shell pane is left alive. Explicit `tm session stop` and
//! `decommission` are UNCHANGED — they still call `stop` /
//! `graceful_terminate_runtime` and kill the pane, because those are
//! human/client-initiated teardown requests, not self-healing state reconciles.
//!
//! Test: `pane_runtime_exited_*`, `session_has_live_pane_*`, `find_runtime_exited_*`,
//! and the end-to-end `stop_runtime_exited_transitions_active_to_stopped` /
//! `stop_runtime_exited_keeps_running_session` /
//! `stop_runtime_exited_does_not_kill_pane` in the `tests` module below drive a
//! real [`SessionManager`] backed by [`crate::session_manager::FakeNoopTmuxDriver`]
//! (no real tmux) and assert the record transition (and, for the last, that
//! `kill_session` is never called — #2023 A).

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

/// True when ANY pane belonging to `tmux_name` still shows a live runtime.
///
/// Why (#2463): a manually-split managed session can have more than one pane
/// sharing the same `tmux_name`. Any single-pane check (e.g. `panes.iter().find(...)`)
/// is at the mercy of `tmux list-panes -a` ordering — if an idle pane happens to
/// sort before a live one, a genuinely-live session gets misclassified as idle.
/// [`find_runtime_exited`] already gets this right by aggregating across every
/// pane for a session; this helper extracts that same any-pane-live rule as a
/// single, reusable, per-session primitive so every call site (the runtime
/// reaper here and the `SessionEnd` gate in `daemon::api`) agrees on what "the
/// session's pane(s) are live" means.
/// What: returns `true` iff at least one pane in `panes` has `session_name ==
/// tmux_name` AND is NOT [`pane_runtime_exited`]. A session with no matching
/// pane at all (already gone, or enumeration failed) returns `false` — there is
/// nothing to protect. Pure — no I/O.
/// Test: `session_has_live_pane_true_when_any_pane_live`,
/// `session_has_live_pane_false_when_all_panes_idle`,
/// `session_has_live_pane_false_when_no_pane_matches`.
pub fn session_has_live_pane(
    tmux_name: &str,
    panes: &[PaneInfo],
    probe: &dyn ChildLivenessProbe,
) -> bool {
    panes
        .iter()
        .filter(|p| p.session_name == tmux_name)
        .any(|p| !pane_runtime_exited(p, probe))
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
/// Why: this is the self-healing action for #1814. Per #2023 A it routes
/// through [`SessionManager::mark_runtime_exited_stopped`] — NOT
/// [`SessionManager::stop`] — so the record transition (workspace/record
/// preserved, session remains resumable) happens WITHOUT killing the tmux
/// pane the runtime just vacated. `stop` remains reserved for explicit
/// `tm session stop` / decommission requests.
/// What: computes the exited-session ids via [`find_runtime_exited`], calls
/// `manager.mark_runtime_exited_stopped` on each (best-effort; a failure is
/// logged and never aborts the rest), and returns the count transitioned.
/// Takes an already-gathered `panes` slice so it is fully testable with a fake
/// tmux driver and no real tmux binary.
/// Test: `stop_runtime_exited_transitions_active_to_stopped`,
/// `stop_runtime_exited_keeps_running_session`,
/// `stop_runtime_exited_does_not_kill_pane`.
pub async fn stop_runtime_exited(
    manager: &SessionManager,
    panes: &[PaneInfo],
    probe: &(dyn ChildLivenessProbe + Sync),
) -> usize {
    let records = manager.list().await;
    let exited = find_runtime_exited(&records, panes, probe);
    let mut stopped = 0usize;
    for id in exited {
        match manager.mark_runtime_exited_stopped(&id).await {
            Ok(record) => {
                stopped += 1;
                info!(
                    id = %id,
                    name = %record.tmux_name,
                    "runtime-reap: marked managed session Stopped (runtime process exited, #1814, pane left alive #2023)"
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
    use crate::session_manager::{
        FakeNoopTmuxDriver, ManagedError, ManagedTmuxDriver, SessionManager,
    };
    use std::sync::{Arc, Mutex};

    /// A tmux driver that records every `kill_session` call (#2023 A).
    ///
    /// Why: [`FakeNoopTmuxDriver`] is a zero-sized unit struct with no interior
    /// mutability (it is shared by dozens of other tests as a bare no-op), so it
    /// cannot be extended with a call counter without risking those callers. This
    /// spy is local to this test module and exists solely to prove the
    /// runtime-exit reconcile path (#2023 A) never reaches `kill_session` — the
    /// pane must be left alive.
    /// What: every method besides `kill_session`/`set_environment` is a silent
    /// no-op (mirroring `FakeNoopTmuxDriver`); `kill_session` pushes `name` onto
    /// `kill_calls`; `set_environment` (#2157 item 3) pushes `(name, key,
    /// value)` onto `env_set_calls` before returning `Ok(())`.
    /// Test: `stop_runtime_exited_does_not_kill_pane`,
    /// `stop_runtime_exited_heals_stale_pane_env`.
    #[derive(Default)]
    struct KillSpyTmuxDriver {
        kill_calls: Mutex<Vec<String>>,
        env_set_calls: Mutex<Vec<(String, String, String)>>,
    }

    impl ManagedTmuxDriver for KillSpyTmuxDriver {
        fn create_session(&self, _name: &str, _workdir: &str) -> Result<(), ManagedError> {
            Ok(())
        }
        fn kill_session(&self, name: &str) -> Result<(), ManagedError> {
            self.kill_calls.lock().unwrap().push(name.to_owned());
            Ok(())
        }
        fn send_line(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
            Ok(())
        }
        fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
            Ok(String::new())
        }
        fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
            Ok(Vec::new())
        }
        fn set_environment(&self, name: &str, key: &str, value: &str) -> Result<(), ManagedError> {
            self.env_set_calls.lock().unwrap().push((
                name.to_owned(),
                key.to_owned(),
                value.to_owned(),
            ));
            Ok(())
        }
    }

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
            pane_id: None,
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

    #[test]
    fn session_has_live_pane_true_when_any_pane_live() {
        // #2463: a manually-split session where `tmux list-panes -a` returns the
        // IDLE pane before the LIVE one must still be classified as live overall —
        // ordering must never matter.
        let panes = vec![pane("tmpm-split", "zsh"), pane("tmpm-split", "claude")];
        assert!(
            session_has_live_pane("tmpm-split", &panes, &AlwaysIdleProbe),
            "any live pane in the session must make the whole session live"
        );
    }

    #[test]
    fn session_has_live_pane_false_when_all_panes_idle() {
        let panes = vec![pane("tmpm-split", "zsh"), pane("tmpm-split", "-bash")];
        assert!(
            !session_has_live_pane("tmpm-split", &panes, &AlwaysIdleProbe),
            "a session whose every pane is idle must not be classified as live"
        );
    }

    #[test]
    fn session_has_live_pane_false_when_no_pane_matches() {
        let panes = vec![pane("some-other-session", "claude")];
        assert!(
            !session_has_live_pane("tmpm-gone", &panes, &AlwaysIdleProbe),
            "no matching pane means nothing to protect — must fail open (not live)"
        );
    }

    #[test]
    fn session_has_live_pane_single_pane_unchanged() {
        // Parity check: single-pane sessions (the common case) behave exactly as
        // the old `.find()`-based check did.
        assert!(session_has_live_pane(
            "tmpm-a",
            &[pane("tmpm-a", "claude")],
            &AlwaysIdleProbe
        ));
        assert!(!session_has_live_pane(
            "tmpm-a",
            &[pane("tmpm-a", "zsh")],
            &AlwaysIdleProbe
        ));
    }

    /// Build an isolated SessionManager on a FakeNoopTmuxDriver and seed one
    /// `Active` record. Mirrors the `#1744` reap test harness so no real tmux is
    /// spawned. Returns the manager and the seeded session id.
    async fn seed_active(
        dir: &tempfile::TempDir,
        tmux_name_task: &str,
    ) -> (Arc<SessionManager>, ManagedSessionId) {
        seed_active_with_driver(dir, tmux_name_task, Arc::new(FakeNoopTmuxDriver)).await
    }

    /// Same as [`seed_active`] but with a caller-supplied driver (#2023 A), so a
    /// test can inject the [`KillSpyTmuxDriver`] to observe `kill_session` calls.
    async fn seed_active_with_driver(
        dir: &tempfile::TempDir,
        tmux_name_task: &str,
        driver: Arc<dyn ManagedTmuxDriver>,
    ) -> (Arc<SessionManager>, ManagedSessionId) {
        let mgr = SessionManager::new(dir.path(), driver)
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

    #[tokio::test]
    async fn stop_runtime_exited_does_not_kill_pane() {
        // #2023 A: the runtime-exit reconcile must mark the record Stopped
        // WITHOUT ever calling kill_session — the pane (now a bare shell) must
        // be left alive for the operator, unlike an explicit `tm session stop`.
        let tmp = tempfile::TempDir::new().unwrap();
        let driver = Arc::new(KillSpyTmuxDriver::default());
        let (mgr, id) =
            seed_active_with_driver(&tmp, "exited-no-kill", driver.clone() as Arc<_>).await;
        let record = mgr.get(&id).await.expect("get");
        let panes = vec![pane(&record.tmux_name, "zsh")];

        let stopped = stop_runtime_exited(&mgr, &panes, &AlwaysIdleProbe).await;
        assert_eq!(stopped, 1, "exactly one session must be stopped");

        let after = mgr.get(&id).await.expect("get after reap");
        assert_eq!(
            after.state,
            ManagedSessionState::Stopped,
            "runtime-exited Active session must become Stopped"
        );
        assert!(
            driver.kill_calls.lock().unwrap().is_empty(),
            "runtime-exit reconcile must NEVER kill the tmux pane (#2023 A)"
        );
    }

    #[tokio::test]
    async fn stop_runtime_exited_heals_stale_pane_env() {
        // #2157 item 3: when the reaper confirms a pane's runtime has exited, it
        // must durably publish TM_MANAGED_SESSION_ID into that pane's tmux
        // session — healing panes that never got the durable publish at spawn
        // time (e.g. created by a pre-#2157 build).
        let tmp = tempfile::TempDir::new().unwrap();
        let driver = Arc::new(KillSpyTmuxDriver::default());
        let (mgr, id) =
            seed_active_with_driver(&tmp, "exited-heal-env", driver.clone() as Arc<_>).await;
        let record = mgr.get(&id).await.expect("get");
        let panes = vec![pane(&record.tmux_name, "zsh")];

        let stopped = stop_runtime_exited(&mgr, &panes, &AlwaysIdleProbe).await;
        assert_eq!(stopped, 1, "exactly one session must be stopped");

        let env_sets = driver.env_set_calls.lock().unwrap();
        assert!(
            env_sets
                .iter()
                .any(|(name, key, value)| name == &record.tmux_name
                    && key == "TM_MANAGED_SESSION_ID"
                    && value == &id.to_string()),
            "runtime-exit reconcile must heal the pane's tmux env: {env_sets:?}"
        );
    }
}
