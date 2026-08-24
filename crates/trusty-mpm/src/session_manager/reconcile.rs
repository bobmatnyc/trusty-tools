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
//!
//! **An unobservable tmux is not an empty tmux.** The live set comes from
//! [`SessionManager::observed_live_managed_names`], which refuses rather than
//! reporting an empty set it cannot stand behind, and reconciliation skips
//! every liveness-derived decision when it refuses. Before #5856 this file
//! read a failed `list_sessions()` as zero live sessions, which marked every
//! running session `Stopped` and, under `auto_resume`, queued each one for a
//! relaunch it did not need.
//!
//! **A stop somebody asked for survives a daemon restart.** The gone arm reads
//! `Stopped` records too, and before #6194 it re-queued every one of them for
//! auto-resume — so a session the operator stopped came back the next time the
//! daemon booted, exactly as it came back on the next supervisor tick. The arm
//! now writes [`StopCause::Unexpected`] only when no cause is recorded yet, and
//! queues through [`SessionRecord::is_auto_resumable`].
//!
//! **A terminal record with a live tmux session is reported, not repaired.**
//! Such a record is provably self-contradictory, but nothing distinguishes its
//! two causes. It is either a session wrongly tombstoned by a dedup pass that
//! misread an unobservable tmux as an empty one (the failure
//! [`SessionManager::observed_live_managed_names`] now refuses),
//! or the case #2777 designed for: a correctly-decommissioned session whose
//! pane lingers as a bare shell printing "run `tm` to relaunch", which the
//! operator may well be attached to. Reviving on liveness would resurrect
//! every one of those. `decommission` also clears `workspace_path`, so an
//! automatic revive would restore a record the picker keys on incomplete
//! fields. #2777 made revival an explicit in-pane operator action for these
//! reasons and this keeps it one — the loop logs the contradiction and the
//! command that resolves it.
//!
//! **The adopt path owes the same hygiene the create path gives.** It did not,
//! and the two defects that came of that share one shape: the loop decided
//! from a snapshot instead of from the pane in front of it. It minted a random
//! id for every name its pre-loop snapshot did not carry, so a second adopter
//! holding its own stale snapshot wrote a second record for one pane (#6117 —
//! the id now derives from the tmux name, see
//! [`ManagedSessionId::for_adopted_tmux_name`]); and it adopted a pane whose
//! cwd would not resolve, which made that pane permanently unreapable because
//! being tracked is exactly what the orphan-GC keys on (#6118 — such a pane is
//! now declined, and reported in [`ReconcileReport::adoption_declined`]).
//!
//! **A test's tmux session is never adopted, however it leaked.** A test that
//! spawns a real tmux session cannot clean it up when its process dies hard —
//! a SIGKILL, a `cargo test` timeout and an aborted run all skip `Drop`, so the
//! RAII guard PR #6125 added covers the panic path and nothing beyond it. Three
//! `tm-deadrt<pid>-01` sessions leaked that way on 2026-08-24, this loop adopted
//! each one, and the operator's picker grew three `(active)` ghosts that
//! outlived the tmux sessions. Names carrying
//! [`trusty_common::session_naming::RESERVED_TEST_PREFIX`] are now refused here
//! and reported in [`ReconcileReport::reserved_test_refused`] — a decision made
//! from the name alone, so it holds for every leak shape whether or not any
//! test-side cleanup ran. Refusal also hands the leaked pane back to
//! `daemon::orphan_gc`, which kills an idle shell with no live child; adopting
//! it was what made it permanent.
//!
//! **A record one of those already produced is tombstoned whether its pane
//! lives or not**, via [`SessionRecord::is_leaked_test_adoption`], and that
//! symmetry is the point: re-adopting a LIVE one as `Active` restored its
//! orphan-GC immunity, the reaper then stamped the dead pane `Unexpected`
//! (auto-resumable), the supervisor RECREATED the tmux session, and the next
//! boot re-adopted it — a loop that ran three times in one day's daemon log.
//! [`SessionRecord::is_auto_resumable`] refuses the same records, which closes
//! the window between a reaper stop and the next boot. The predicate asks for
//! an adopted PROVENANCE as well as a reserved name, so a session the daemon
//! CREATED for a project legitimately named `xtest-…` keeps ordinary
//! stop/resume behavior.
//!
//! **Declining is a kill decision, so the probe behind it retries.** An
//! undeclared pane is orphan-GC input, and that GC kills an idle-shell pane
//! with no live child after two sweeps. See
//! [`super::adopt::resolve_adoptable_cwd`] for why the retry lives inside the
//! single boot observation rather than across two of them.
//!
//! Test: `manager_reconcile_gone_tmux_yields_stopped`,
//! `manager_reconcile_adopts_new_prefix_session` in `tests.rs`;
//! `reconcile_refuses_to_stop_sessions_when_tmux_cannot_be_observed`,
//! `reconcile_never_revives_a_terminal_record_with_a_live_session` and
//! `reconcile_warns_that_a_terminal_record_has_a_live_tmux_session` in
//! `dedup_tests.rs`.

use std::path::{Path, PathBuf};

use chrono::Utc;
use tracing::{info, warn};

use super::manager::{ManagedError, ReconcileReport, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord, StopCause};

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
    /// `Decommissioned`). A managed session unknown to the store is adopted as
    /// `Active` under an id derived from its tmux name (#6117), and only when
    /// its pane resolves a working directory (#6118) and its name is not in the
    /// test-owned reserved namespace (#6116) — see this module's doc.
    /// When `auto_resume` is true, every session this pass marked `Stopped`
    /// whose stop was not asked for is immediately resumed (#6194 — a record
    /// already carrying [`StopCause::Deliberate`] keeps it and is left down).
    ///
    /// When tmux cannot be observed at all, every liveness-derived decision is
    /// skipped: no record changes state, nothing is adopted, and nothing is
    /// queued for auto-resume. See
    /// [`Self::observed_live_managed_names`]. The liveness-independent work
    /// still runs — the #4400 `pending_decision` backfill on terminal records,
    /// and the dedup / deploy-validate / auto-resume tail below, each of which
    /// makes its own decision about a tmux it cannot see.
    /// Test: `manager_reconcile_gone_tmux_yields_stopped`,
    /// `manager_reconcile_adopts_new_prefix_session`;
    /// `reconcile_refuses_to_stop_sessions_when_tmux_cannot_be_observed`;
    /// `reconcile_declines_to_adopt_a_pane_whose_cwd_cannot_be_resolved`,
    /// `repeated_reconcile_of_one_unresolvable_pane_stays_at_zero_records`,
    /// `a_declined_pane_is_reapable_by_the_orphan_gc`,
    /// `repeated_reconcile_of_one_resolvable_pane_keeps_one_record`;
    /// `boot_reconcile_never_requeues_a_deliberately_stopped_session`,
    /// `boot_reconcile_still_auto_resumes_a_session_lost_with_the_daemon`
    /// (#6194) in `stop_cause_tests.rs`;
    /// `reconcile_refuses_to_adopt_a_reserved_test_session`,
    /// `reconcile_tombstones_an_adopted_reserved_record_whose_tmux_is_gone`,
    /// `a_live_leaked_test_pane_is_never_readopted_or_recreated`
    /// (#6116) in `naming_tests.rs`.
    pub async fn reconcile_on_boot(
        &self,
        auto_resume: bool,
    ) -> Result<ReconcileReport, ManagedError> {
        // #5856: an unobservable tmux is not an empty tmux. `None` means tmux
        // was never successfully asked — it is NOT a set of zero live
        // sessions, and no record's state may be derived from it.
        let live_names: Option<std::collections::HashSet<String>> =
            match self.observed_live_managed_names() {
                Ok(names) => Some(names),
                Err(e) => {
                    warn!(
                        "reconcile: tmux liveness could not be observed ({e}); leaving every \
                         record's state untouched — no session is marked Stopped, queued for \
                         auto-resume, or adopted without positive evidence about its pane"
                    );
                    None
                }
            };

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
            // Terminal tombstones (`Decommissioned` OR `Deleted`) are never
            // touched by reconciliation. Previously this only checked
            // `Decommissioned` by hand, so every soft-deleted (`Deleted`)
            // record was silently resurrected to `Stopped` on daemon boot —
            // `is_terminal()` is the single source of truth for "terminal"
            // and must be used here so a future terminal variant can never
            // slip past this check again the way `Deleted` did.
            if record.state.is_terminal() {
                // #5856: a tombstoned record whose tmux session is LIVE is a
                // provable contradiction — `tm session info` reports
                // `"attached": true` beside `"state": "decommissioned"` — and
                // the picker hides the row either way. Report it; never
                // auto-revive it (see this module's doc).
                // #6116: not for a tombstoned test adoption, where a live pane
                // is the EXPECTED state — the record is dropped while the
                // orphan-GC still has two sweeps of work to do on the pane —
                // and the advice to reactivate it is exactly wrong.
                if live_names
                    .as_ref()
                    .is_some_and(|live| live.contains(&record.tmux_name))
                    && !record.is_leaked_test_adoption()
                {
                    warn!(
                        id = %record.id,
                        name = %record.tmux_name,
                        state = ?record.state,
                        "reconcile: terminal record has a LIVE tmux session — the picker hides \
                         this row. If the session is genuinely in use, revive it with: curl -sS \
                         -X POST $TRUSTY_MPM_URL/api/v1/sessions/managed/{}/reactivate",
                        record.id
                    );
                }
                // #4400 backfill: rows tombstoned before the decommission-path
                // fix landed can still carry a stale `pending_decision` (the
                // `FleetMetrics` filter is defense in depth, not a cure — the
                // stored field itself should not linger either). One-time
                // clear on the next boot reconcile, since this loop already
                // visits every record.
                if record.pending_decision.is_some() || record.proposed_default.is_some() {
                    record.pending_decision = None;
                    record.proposed_default = None;
                    report.stale_decisions_cleared.push(record.id.to_string());
                    warn!(
                        id = %record.id,
                        name = %record.tmux_name,
                        "reconcile: cleared stale pending_decision on terminal record (#4400 backfill)"
                    );
                    guard.upsert(record).await?;
                }
                continue;
            }

            // #6116: BEFORE the live/gone branch, because liveness is what the
            // self-sustaining loop fed on — a live leaked pane was re-adopted
            // Active, which restored its orphan-GC immunity, and once the pane
            // died the reaper stamped it auto-resumable and the supervisor
            // recreated the tmux session for the next pass to re-adopt. The
            // record is a leaked test adoption either way, so the answer is the
            // same either way: tombstone it and let the orphan-GC have the pane.
            // Needs no live set, so it also runs when tmux is unobservable.
            if record.is_leaked_test_adoption() {
                record.set_lifecycle_state(ManagedSessionState::Deleted, Utc::now());
                record.pending_decision = None;
                record.proposed_default = None;
                report.reserved_test_swept.push(record.id.to_string());
                warn!(
                    id = %record.id,
                    name = %record.tmux_name,
                    "reconcile: tombstoned an adopted reserved test-namespace record (#6116) — \
                     a leaked test session is not a session to keep, resume or relaunch; its \
                     pane goes back to the orphan-GC"
                );
                guard.upsert(record).await?;
                continue;
            }

            // #5856: without an observed live set there is no evidence either
            // way, so the record keeps whatever state it already has. The
            // pre-#5856 code fell through to the `else` arm here and marked
            // every live session `Stopped` — under `auto_resume` it then
            // queued each one for a relaunch it did not need.
            let Some(live_names) = live_names.as_ref() else {
                continue;
            };

            if live_names.contains(&record.tmux_name) {
                // Session is alive — re-adopt as Active.
                record.state = ManagedSessionState::Active;
                // #6194: it is running, so whatever stopped it last no longer
                // describes it.
                record.stop_cause = None;
                report.adopted.push(record.tmux_name.clone());
                info!(name = %record.tmux_name, "reconcile: re-adopted live session");
            } else {
                // Session is gone — mark Stopped (resumable), never Orphaned.
                record.state = ManagedSessionState::Stopped;
                // #6194: this ran while the daemon was down, so nothing here can
                // say whether the tmux session was killed or lost to a reboot —
                // `get_or_insert` keeps a Deliberate cause an earlier stop
                // already recorded, and calls an otherwise-unattributed
                // disappearance Unexpected, which is the post-reboot restore
                // `auto_resume` was built for.
                record.stop_cause.get_or_insert(StopCause::Unexpected);
                report.stopped.push(record.id.to_string());
                warn!(name = %record.tmux_name, "reconcile: tmux session gone, marked Stopped (workspace intact, resumable)");
                if auto_resume && record.is_auto_resumable() {
                    to_resume.push(record.id);
                }
            }
            guard.upsert(record).await?;
        }

        // Canonical workspace paths already tracked by a non-terminal record
        // (#3396): the external-adopt loop below must never mint a SECOND
        // identity for a worktree an existing record already owns, mirroring
        // the fix `decide_native_registration` (#3599) applied to the
        // native-process discovery path. Built from the FRESH post-reconcile
        // state (the per-record loop above already re-upserted every live/gone
        // transition through `guard`), so a record whose tmux session just
        // went live this pass is included too.
        let known_workspaces: std::collections::HashSet<PathBuf> = guard
            .all()
            .await?
            .iter()
            .filter(|r| !matches!(r.state, ManagedSessionState::Decommissioned))
            .filter_map(|r| r.workspace_path.clone())
            // #6118: one spelling of the sentinel, shared with every other
            // reader of it.
            .filter(|p| p.as_path() != Path::new(super::record::UNRESOLVED_PATH_SENTINEL))
            .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
            .collect();

        // Adopt tmux sessions the store has never seen. Issue #2158: an
        // adopted session must never be left as a silent half-record with
        // cwd/workspace_path permanently stubbed to "/unknown" — re-resolve
        // the pane's real working directory via `get_pane_cwd` (the same
        // primitive the idle-auto-stop snapshot uses) so it can be validated
        // and provisioned like any other managed workspace.
        // #5856: `flatten` yields nothing when liveness was never observed, so
        // an unreachable tmux adopts no external session either.
        let mut newly_resolved: Vec<(ManagedSessionId, PathBuf)> = Vec::new();
        for name in live_names.iter().flatten() {
            if known_names.contains(name) {
                continue;
            }
            // #6116: the test suite owns this namespace; a session in it is a
            // leak from a hard-killed test run, never work to track.
            if crate::core::names::is_reserved_test_session_name(name) {
                warn!(
                    name = %name,
                    "reconcile: refusing to adopt a reserved test-namespace session (#6116) — \
                     it leaked from a test process that died before its cleanup ran. Left \
                     untracked for the orphan-GC, which kills an idle shell with no live child \
                     on two consecutive sweeps"
                );
                report.reserved_test_refused.push(name.clone());
                continue;
            }
            // #6118: a pane whose cwd will not resolve is NOT adopted. #2158
            // adopted it anyway, flagged `unmanaged` in `task` with a
            // `/unknown` cwd, and that record was unkillable by design: the
            // `tm ls` auto-prune keeps any record whose tmux name is live, and
            // the orphan-GC keeps any pane a registry names — so adoption
            // itself is what granted a leaked idle `sh` pane permanent
            // immunity. 55 of 103 records in the reporting store were these.
            //
            // Declining leaves the pane untracked, which hands it to
            // `daemon::orphan_gc`. That is a reap path, not a no-op: an idle
            // shell with no live child is KILLED after two 60-second sweeps
            // (a pane running an agent is kept and warned about instead). The
            // probe therefore decides a pane's life, which is why
            // `resolve_adoptable_cwd` retries a failed one — see its doc.
            let Some(resolved_cwd) = super::adopt::resolve_adoptable_cwd(&*self.tmux, name).await
            else {
                warn!(
                    name = %name,
                    "reconcile: declining external-adopt — the pane's working directory did not \
                     resolve on any attempt, so there is nothing to track, resume or attach to \
                     (#6118). The pane is left to the orphan-GC, which kills it only if it is an \
                     idle shell with no live child on two consecutive sweeps; `tmux attach -t \
                     <name>` still reaches it until then"
                );
                report.adoption_declined.push(name.clone());
                continue;
            };
            // #3396: a live tmux session unknown BY NAME can still resolve
            // to a workspace an EXISTING record already tracks — e.g. a
            // renamed/second tmux session fronting the same worktree.
            // Minting a second record here is exactly the duplicate-record
            // defect; skip adoption and surface the crossed mapping loudly
            // instead so an operator can reconcile the tmux_name drift
            // (`tm sessions ls`, `tmux list-panes`) rather than the daemon
            // silently doubling the identity.
            let canon =
                std::fs::canonicalize(&resolved_cwd).unwrap_or_else(|_| resolved_cwd.clone());
            if known_workspaces.contains(&canon) {
                warn!(
                    name = %name,
                    workspace = %resolved_cwd.display(),
                    "reconcile: skipping external-adopt — live tmux session resolves to \
                     a workspace already tracked by another managed session record; its \
                     tmux_name may be stale/crossed (#3396) — investigate with `tm sessions \
                     ls` and `tmux list-panes` rather than auto-adopting a duplicate"
                );
                continue;
            }
            // #6117: derived from the tmux name, never random — `known_names`
            // is a pre-loop snapshot, and a concurrent adopter holding its own
            // stale copy would otherwise write a SECOND record for this pane
            // under a fresh id. Same name, same store key, one record.
            let id = ManagedSessionId::for_adopted_tmux_name(name);
            let external = SessionRecord {
                id,
                tmux_name: name.clone(),
                cwd: resolved_cwd.clone(),
                // #6116: one spelling, shared with the predicate that reads it
                // back as this record's adopted provenance.
                task: super::record::ADOPTED_TASK.to_string(),
                state: ManagedSessionState::Active,
                created_at: Utc::now(),
                last_activity_at: None,
                workspace_path: Some(resolved_cwd.clone()),
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
                // Best-effort capture (#2453 review finding 1, round 2) —
                // the adopted pane already exists, so this is available
                // immediately, mirroring `adopt.rs`'s explicit adoption path.
                pane_id: self.tmux.get_pane_id(name),
                injection_status: Default::default(),
                worktree_owner: None,
                terminal_at: None,
                stop_cause: None,
            };
            newly_resolved.push((id, resolved_cwd));
            guard.upsert(external).await?;
            report.external_adopted.push(name.clone());
            info!(name = %name, "reconcile: adopted external managed session");
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
