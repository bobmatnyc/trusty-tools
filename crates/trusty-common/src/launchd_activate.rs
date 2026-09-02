//! Label-correct, non-destructive LaunchAgent activation (#4919).
//!
//! Why: `install()` then `bootstrap()` was the whole install sequence, and it
//! had three defects that together produced a release-time outage.
//!
//! 1. **It activated the wrong unit.** `bootstrap` boots out and re-bootstraps
//!    THIS config's label. When the label in code had drifted from the label
//!    launchd actually had loaded, the sequence booted out nothing, started a
//!    second unit beside the live one, and left the plist fixes (#4868's
//!    `ExitTimeOut`) in a file launchd never read. Nothing in the old sequence
//!    could notice, because the only label it ever mentioned was its own.
//! 2. **It bounced a daemon that did not need bouncing.** `bootstrap`
//!    unconditionally booted out first, so re-running `service install` with no
//!    configuration change still cost a full stop/start plus the plist's
//!    `ThrottleInterval` before launchd would relaunch — the ~1 minute of
//!    downtime observed during the release.
//! 3. **A failure left the service DOWN.** The bootout happened first and the
//!    bootstrap could then fail (bad plist, missing exe, launchd refusal), with
//!    no path back. The operator was left with no running daemon and an error.
//!
//! What: [`LaunchdConfig::install_and_activate`] evicts the service's known
//! legacy labels, writes the plist, and reloads only when the on-disk unit
//! actually changed — restoring and re-bootstrapping the previous plist if the
//! new one fails to come up.
//!
//! Test: `cargo test -p trusty-common --features unconditional-only
//! launchd_activate`. The `launchctl` calls are side-effecting and are
//! exercised by the daemons' own `service install`; the decision logic that
//! gates them is pure and unit-tested here.
//!
//! [`LaunchdConfig::install_and_activate`]: crate::launchd::LaunchdConfig::install_and_activate
#![cfg(target_os = "macos")]

use anyhow::{Context, Result};

use crate::launchd::{LaunchdConfig, current_uid};
use crate::launchd_labels::{EvictionOutcome, LabelEviction};

/// What an [`install_and_activate`](LaunchdConfig::install_and_activate) call
/// actually did.
///
/// Why: "installed" is not one outcome. An operator needs to know whether their
/// daemon was restarted, whether a stale unit was evicted, and whether a
/// rollback happened — each implies a different next step.
/// What: the three terminal states, plus the labels evicted along the way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activation {
    /// The rendered plist matched the installed one and the label was already
    /// loaded. Nothing was reloaded and the daemon kept running.
    AlreadyCurrent {
        /// Legacy labels evicted even though the canonical unit was current.
        evicted: Vec<String>,
    },
    /// The plist was written and the canonical label bootstrapped.
    Activated {
        /// Legacy labels evicted before the canonical unit was bootstrapped.
        evicted: Vec<String>,
        /// Whether a previously-installed plist was replaced (vs. first install).
        replaced: bool,
    },
}

impl Activation {
    /// Legacy labels this activation booted out.
    #[must_use]
    pub fn evicted(&self) -> &[String] {
        match self {
            Activation::AlreadyCurrent { evicted } | Activation::Activated { evicted, .. } => {
                evicted
            }
        }
    }
}

/// What a rollback managed to do, so the error message can say which.
///
/// Why: "restored" and "the service is down" demand different operator action,
/// and the pre-review code printed the former unconditionally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rollback {
    /// The previous plist was rewritten and is loaded again.
    Restored,
    /// There was no previous unit, so nothing was taken down.
    NothingToRestore,
    /// A previous unit existed and could not be brought back. Service is down.
    Failed,
}

/// What a rollback should DO, decided without touching launchd.
///
/// Why: the two inputs interact in a way that is easy to get wrong and
/// impossible to test through `launchctl` — see [`rollback_plan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollbackPlan {
    /// Rewrite the previous plist and bootstrap it again.
    RestorePrevious,
    /// Keep the plist just written and bootstrap it, because the job that was
    /// running is already gone and this is the only unit left to revive.
    ReviveWritten,
    /// Delete the half-written plist and boot the label out.
    RemoveAndBootout,
}

/// Decide what a failed activation should undo.
///
/// Why (#4919 review, round 2): the round-1 fix stopped the bootout but still
/// left the service down while reporting otherwise. `LaunchdConfig::bootstrap`
/// calls `bootout` FIRST, so by the time rollback runs on the
/// `has_previous == false, was_loaded == true` path, the job that WAS running
/// is already gone. Deleting the plist and returning "nothing was taken down"
/// then produced the worst possible combination: service down, plist gone, and
/// a message saying neither happened.
///
/// The job that was running had no plist on disk, so it cannot be reconstructed
/// — launchd held it in memory. The only unit left that names the same label is
/// the one just written, so reviving THAT is the best available recovery, and
/// failing to revive it must be reported as an outage rather than swallowed.
///
/// What: a previous plist means restore it. With none but a label that was
/// loaded, keep and bootstrap the plist just written. With none and nothing
/// loaded, remove the plist and boot out — that is genuinely the state the
/// attempt found.
/// Test: `rollback_plan_restores_a_previous_unit`,
/// `rollback_plan_revives_the_written_unit_when_a_live_job_was_displaced`,
/// `rollback_plan_boots_out_a_label_it_started_itself`, and the effect tests
/// `rollback_execution_*`.
#[must_use]
pub fn rollback_plan(has_previous: bool, was_loaded: bool) -> RollbackPlan {
    if has_previous {
        RollbackPlan::RestorePrevious
    } else if was_loaded {
        RollbackPlan::ReviveWritten
    } else {
        RollbackPlan::RemoveAndBootout
    }
}

/// Carry out a [`RollbackPlan`] against injected effects.
///
/// Why: the defect this fixes was never in the plan VALUE — round-1 tests
/// asserted the plan and passed while the service was down. It was in what the
/// plan DID. Taking the effects as closures lets the outcome be asserted
/// without launchd.
///
/// What: `write_previous` returns whether the previous plist was restored to
/// disk; `remove_plist` deletes the plist just written; `bootstrap` returns
/// whether the label is loaded afterwards. Returns [`Rollback::Failed`]
/// whenever the service is left down.
/// Test: `rollback_execution_reports_failure_when_revival_fails`,
/// `rollback_execution_keeps_the_written_plist_when_reviving`,
/// `rollback_execution_reports_restored_only_when_bootstrap_succeeds`.
pub fn execute_rollback(
    plan: RollbackPlan,
    write_previous: impl FnOnce() -> bool,
    remove_plist: impl FnOnce(),
    bootstrap: impl FnOnce() -> bool,
) -> Rollback {
    match plan {
        RollbackPlan::RestorePrevious => {
            if !write_previous() {
                return Rollback::Failed;
            }
            if bootstrap() {
                Rollback::Restored
            } else {
                Rollback::Failed
            }
        }
        RollbackPlan::ReviveWritten => {
            // Deliberately does NOT remove the plist: it is the only unit left
            // naming this label, and the job that was running is already gone.
            if bootstrap() {
                Rollback::Restored
            } else {
                Rollback::Failed
            }
        }
        RollbackPlan::RemoveAndBootout => {
            remove_plist();
            Rollback::NothingToRestore
        }
    }
}

/// Decide whether the canonical unit needs reloading.
///
/// Why: the reload IS the outage. Skipping it when nothing changed is what
/// turns a re-run of `service install` from a one-minute gap into a no-op, and
/// keeping the decision pure is what makes that testable without launchd.
///
/// #4919 review: `force` exists because the plist is not the whole unit. A
/// deploy replaces the BINARY behind a byte-identical plist, so content
/// equality would have let `make deploy` finish without ever activating what it
/// just built.
///
/// What: returns `true` when `force` is set, when the rendered plist differs
/// from what is installed, or when the label is not currently loaded. A missing
/// on-disk plist counts as "differs".
/// Test: `reload_needed_*`, `reload_needed_is_true_when_forced`.
#[must_use]
pub fn reload_needed(
    rendered: &str,
    installed: Option<&str>,
    is_loaded: bool,
    force: bool,
) -> bool {
    force || !is_loaded || installed != Some(rendered)
}

impl LaunchdConfig {
    /// Install this agent's plist and make launchd actually run it, evicting the
    /// service's legacy labels first.
    ///
    /// Why: see the module header — the old `install()` + `bootstrap()` pair
    /// could activate a different unit than the one it wrote, bounced the
    /// daemon needlessly, and had no way back from a failed bootstrap.
    ///
    /// What, in order:
    /// 1. Snapshot the currently-installed plist bytes, for rollback.
    /// 2. Boot out every label in `legacy_labels` and delete its plist file, so
    ///    a later `launchctl bootstrap` cannot resurrect a duplicate unit
    ///    contending for the same port and locks (#2938). A legacy label that
    ///    is not loaded is not an error.
    /// 3. If the rendered plist matches what is installed AND the canonical
    ///    label is loaded, stop — the daemon keeps running untouched.
    /// 4. Otherwise write the plist and bootstrap the canonical label.
    /// 5. If the bootstrap fails or the label does not come up, restore the
    ///    snapshot and re-bootstrap it, then return the original error. A
    ///    failed install leaves the service as it found it, never down.
    ///
    /// Preconditions: `legacy_labels` must not contain this config's own label
    /// — evicting it would boot out the unit being installed. Callers pass
    /// [`crate::launchd_labels::legacy_labels_for`], which cannot produce one
    /// (`legacy_labels_are_never_canonical` proves it).
    ///
    /// Test: `reload_needed_*` cover the decision; the `launchctl` effects are
    /// exercised by `trusty-search service install`.
    pub fn install_and_activate(&self, legacy_labels: &[&str]) -> Result<Activation> {
        self.install_and_activate_forced(legacy_labels, false)
    }

    /// [`install_and_activate`](Self::install_and_activate), with the option to
    /// reload even when the rendered unit is unchanged.
    ///
    /// Why: a deploy replaces the binary behind a byte-identical plist, so the
    /// content-equality skip would leave launchd running the old image (#4919
    /// review). `force` is for those paths; ordinary re-installs pass `false`.
    /// Test: `reload_needed_is_true_when_forced`.
    pub fn install_and_activate_forced(
        &self,
        legacy_labels: &[&str],
        force: bool,
    ) -> Result<Activation> {
        debug_assert!(
            !legacy_labels.contains(&self.label.as_str()),
            "a service's own label must never be listed as its legacy alias — \
             evicting it would boot out the unit being installed"
        );

        let plist_path = self.plist_path()?;
        let previous = std::fs::read_to_string(&plist_path).ok();
        let rendered = self.render_plist()?;

        let evicted = self.evict_legacy(legacy_labels);

        // #4919 review: capture liveness BEFORE `install()` overwrites the
        // plist. launchd keeps a job registered after its plist file is
        // deleted, so "no previous plist" does NOT imply "nothing was running"
        // — rolling back on that assumption booted out a live daemon.
        let was_loaded = self.is_loaded();

        if !reload_needed(&rendered, previous.as_deref(), was_loaded, force) {
            return Ok(Activation::AlreadyCurrent { evicted });
        }

        let replaced = previous.is_some();
        self.install()?;

        // #6590: the plist is now correct on disk, but launchd is still running
        // the OLD job definition — it re-reads the file only at `bootstrap`, and
        // `bootstrap_and_verify` boots the old job out first. So the termination
        // below is governed by the PREVIOUS unit's `ExitTimeOut`, which on a
        // pre-#4393 host is launchd's 5 s default. Stop the daemon ourselves
        // first when that window is too short; see `guard_short_grace`.
        self.guard_short_grace(crate::shutdown::TERMINATION_GRACE_SECS);

        match self.bootstrap_and_verify() {
            Ok(()) => Ok(Activation::Activated { evicted, replaced }),
            Err(e) => {
                let restored = self.roll_back(&plist_path, previous.as_deref(), was_loaded);
                Err(e).context(match restored {
                    Rollback::Restored => {
                        "activating the new LaunchAgent failed; the previously \
                         installed unit was restored and reloaded, so the \
                         service is not left down"
                    }
                    Rollback::NothingToRestore => {
                        "activating the new LaunchAgent failed; no unit was \
                         installed beforehand, so nothing was taken down"
                    }
                    Rollback::Failed => {
                        "activating the new LaunchAgent failed AND restoring \
                         the previous unit also failed — THE SERVICE IS DOWN. \
                         Re-run the install, or bootstrap the plist by hand"
                    }
                })
            }
        }
    }

    /// Boot out and delete every legacy label's unit, returning those evicted.
    ///
    /// Why: an upgrade that only bootstraps its new label leaves the old unit
    /// running — two daemons on one port (#2938). Deleting the plist as well is
    /// what stops the next `launchctl bootstrap` from resurrecting it.
    /// #4919 review: also called from `service uninstall`. Removing only the
    /// canonical plist on a not-yet-migrated host printed "nothing to do" and
    /// left the legacy unit loaded — an uninstall that uninstalls nothing.
    ///
    /// What: best-effort per label; a legacy unit that is absent, not loaded, or
    /// refuses to unload never fails the install, because the canonical unit is
    /// still the thing being installed.
    ///
    /// #6290: this fail-soft contract holds only while something IS being
    /// installed to replace the unit. A RETIRED unit has no replacement, so a
    /// caller clearing one wants [`Self::evict_legacy_detailed`], which
    /// distinguishes a failed removal from an absent one.
    pub fn evict_legacy(&self, legacy_labels: &[&str]) -> Vec<String> {
        self.evict_legacy_detailed(legacy_labels)
            .into_iter()
            .filter(|e| e.outcome == EvictionOutcome::Evicted)
            .map(|e| e.label)
            .collect()
    }

    /// Boot out and delete every legacy label's unit, reporting each outcome.
    ///
    /// Why: [`Self::evict_legacy`] answers only "which labels were evicted", so
    /// a label missing from its result could mean either "nothing was there" or
    /// "the removal failed" (#6290). Those need opposite handling — the first is
    /// the steady state, the second leaves a unit loaded and respawning — and a
    /// caller that cannot tell them apart reports success either way.
    /// What: per label, boots out only a unit that is actually loaded, verifies
    /// launchd let go, then deletes the plist; any step failing yields
    /// [`EvictionOutcome::Failed`] with the reason.
    /// Test: `eviction_outcome_only_failed_is_a_failure` proves the outcome
    /// taxonomy this return type carries; the launchctl steps themselves are
    /// exercised through the trusty-installer eviction tests.
    pub fn evict_legacy_detailed(&self, legacy_labels: &[&str]) -> Vec<LabelEviction> {
        legacy_labels
            .iter()
            .map(|legacy| {
                let mut alias = self.clone();
                alias.label = (*legacy).to_string();
                LabelEviction::new(*legacy, alias.evict_one())
            })
            .collect()
    }

    /// Clear THIS config's label, reporting what happened to it.
    ///
    /// What: bootout is issued only when the label is actually loaded (booting
    /// out an unloaded label errors spuriously), and its postcondition is then
    /// verified — `launchctl bootout` exiting 0 is not proof launchd let go, the
    /// same gap [`Self::bootstrap_and_verify`] closes on the way in.
    fn evict_one(&self) -> EvictionOutcome {
        let was_loaded = self.is_loaded();
        let mut failures: Vec<String> = Vec::new();

        if was_loaded {
            if let Err(e) = self.bootout() {
                failures.push(format!("`launchctl bootout` failed: {e}"));
            } else if self.is_loaded() {
                failures
                    .push("still loaded after `launchctl bootout` reported success".to_string());
            }
        }

        let removed = match self.plist_path() {
            Ok(path) if path.exists() => match std::fs::remove_file(&path) {
                Ok(()) => true,
                Err(e) => {
                    failures.push(format!("could not delete {}: {e}", path.display()));
                    false
                }
            },
            Ok(_) => false,
            Err(e) => {
                failures.push(format!("could not resolve the plist path: {e}"));
                false
            }
        };

        if !failures.is_empty() {
            EvictionOutcome::Failed(failures.join("; "))
        } else if was_loaded || removed {
            EvictionOutcome::Evicted
        } else {
            EvictionOutcome::Absent
        }
    }

    /// Stop the daemon ourselves when launchd's own grace would cut it short.
    ///
    /// Why (#6590): `bootstrap` boots the old job out, and launchd bounds that
    /// bootout by the `ExitTimeOut` of the job it LOADED — not by the corrected
    /// plist `install()` just wrote, which it will not read until the bootstrap
    /// that comes after. A unit loaded from a pre-#4393 plist grants
    /// [`crate::launchd_grace::LAUNCHD_DEFAULT_EXIT_TIMEOUT_SECS`], so a daemon
    /// flushing 50 index snapshots was SIGKILLed mid-write and `KeepAlive`
    /// respawned the OLD binary as an orphan holding the port.
    ///
    /// What: reads the window launchd will really grant. When it covers
    /// `required_secs`, or cannot be read at all, nothing happens — the bootout
    /// proceeds as before. When it is too short, the daemon is sent SIGTERM
    /// directly (bounded by nothing launchd controls) and waited for, so the
    /// bootout that follows unloads an already-exited job. Advisory throughout:
    /// a quiesce that fails still falls through to the bootout, which is no
    /// worse than the behaviour this replaces.
    /// Test: the verdict and the wait are unit-tested in `launchd_grace`; the
    /// wiring is exercised by `trusty-search service install`.
    fn guard_short_grace(&self, required_secs: u64) {
        use crate::launchd_grace::{GraceVerdict, Quiesce};

        let GraceVerdict::TooShort {
            active_secs,
            required_secs,
        } = self.active_grace_verdict(required_secs)
        else {
            return;
        };

        tracing::warn!(
            label = %self.label,
            active_secs,
            required_secs,
            "the loaded launchd unit grants less shutdown grace than the daemon \
             needs; stopping it directly before bootout so its flush is not \
             SIGKILLed (#6590)"
        );

        match self.quiesce_before_bootout(required_secs) {
            Quiesce::NotRunning => {}
            Quiesce::Exited { waited_secs } => tracing::info!(
                label = %self.label,
                waited_secs,
                "daemon exited cleanly before bootout"
            ),
            Quiesce::StillRunning => tracing::warn!(
                label = %self.label,
                required_secs,
                "daemon did not exit within its own grace window; the bootout \
                 that follows may still be cut short by launchd"
            ),
        }
    }

    /// Bootstrap this label and confirm launchd actually has it.
    ///
    /// Why: `launchctl bootstrap` exiting 0 is not proof the job is loaded —
    /// #2498 recorded a bootstrap that succeeded while the daemon stayed "not
    /// running". Verifying closes the gap between "the command worked" and "the
    /// service is up", which is the difference the whole #4919 fix turns on.
    fn bootstrap_and_verify(&self) -> Result<()> {
        self.bootstrap()?;
        if !self.is_loaded() {
            anyhow::bail!(
                "launchctl bootstrap reported success but gui/{}/{} is not \
                 loaded (#2498)",
                current_uid(),
                self.label
            );
        }
        Ok(())
    }

    /// Put back the plist that was installed before this attempt and reload it.
    ///
    /// Why: rollback runs on a path that has already failed, so its own errors
    /// must not mask the original one — but they must not be INVISIBLE either.
    /// #4919 review: discarding the restoring `bootstrap()`'s result made the
    /// caller print "the service is not left down" even when the restore had
    /// failed and it was. The outcome is returned so the message can tell the
    /// truth.
    ///
    /// What: binds this config's real filesystem and `launchctl` effects into
    /// [`execute_rollback`], and boots the label out on the remove path.
    /// Test: the decision is covered by `rollback_plan_*` and the outcome by
    /// `rollback_execution_*`; the `launchctl` calls themselves are
    /// side-effecting and are exercised by the daemons' `service install`.
    fn roll_back(
        &self,
        plist_path: &std::path::Path,
        previous: Option<&str>,
        was_loaded: bool,
    ) -> Rollback {
        let plan = rollback_plan(previous.is_some(), was_loaded);
        let outcome = execute_rollback(
            plan,
            || previous.is_some_and(|bytes| std::fs::write(plist_path, bytes).is_ok()),
            || {
                let _ = std::fs::remove_file(plist_path);
            },
            || self.bootstrap().is_ok() && self.is_loaded(),
        );
        if plan == RollbackPlan::RemoveAndBootout {
            let _ = self.bootout();
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: an unchanged plist on a loaded label must not cost a restart — the
    /// unconditional bootout in the old sequence is where the ~1 minute of
    /// release downtime came from (#4919).
    /// What: identical rendered/installed plist plus a loaded label ⇒ no reload.
    /// Test: this is the test.
    #[test]
    fn reload_needed_is_false_when_nothing_changed() {
        assert!(!reload_needed("<plist/>", Some("<plist/>"), true, false));
    }

    /// Why: a unit that existed before the attempt must come back.
    /// What: a previous plist is restored and re-bootstrapped.
    /// Test: this is the test.
    #[test]
    fn rollback_plan_restores_a_previous_unit() {
        assert_eq!(
            rollback_plan(true, true),
            RollbackPlan::RestorePrevious,
            "a unit that was running must be put back"
        );
        assert_eq!(rollback_plan(true, false), RollbackPlan::RestorePrevious);
    }

    /// Why (#4919 review, round 2): `bootstrap` boots out FIRST, so on this path
    /// the job that was running is already gone before rollback runs. Round 1
    /// deleted the plist and reported "nothing was taken down" — service down,
    /// plist gone, message wrong.
    /// What: with no previous plist but a label that WAS loaded, the unit just
    /// written is kept and revived.
    /// Test: this is the test.
    #[test]
    fn rollback_plan_revives_the_written_unit_when_a_live_job_was_displaced() {
        assert_eq!(
            rollback_plan(false, true),
            RollbackPlan::ReviveWritten,
            "the displaced job cannot be reconstructed, so the written unit is \
             the only thing left that can restore service"
        );
    }

    /// Why: the round-1 tests asserted the plan VALUE and passed while the
    /// stated goal — service not left down — was unmet. These assert the EFFECT.
    /// What: reviving without a working bootstrap reports `Failed`, which is
    /// what makes the caller print "THE SERVICE IS DOWN".
    /// Test: this is the test.
    #[test]
    fn rollback_execution_reports_failure_when_revival_fails() {
        let mut removed = false;
        let outcome = execute_rollback(
            RollbackPlan::ReviveWritten,
            || unreachable!("no previous plist on this path"),
            || removed = true,
            || false,
        );
        assert_eq!(
            outcome,
            Rollback::Failed,
            "a failed revival leaves the service down and must say so"
        );
        assert!(
            !removed,
            "the written plist is the only unit naming this label — deleting it \
             removes the last chance of recovery"
        );
    }

    /// Why: the plist just written must survive the revive path, or a later
    /// manual bootstrap has no file to work from.
    /// What: a successful revival reports `Restored` and removes nothing.
    /// Test: this is the test.
    #[test]
    fn rollback_execution_keeps_the_written_plist_when_reviving() {
        let mut removed = false;
        let outcome = execute_rollback(
            RollbackPlan::ReviveWritten,
            || unreachable!("no previous plist on this path"),
            || removed = true,
            || true,
        );
        assert_eq!(outcome, Rollback::Restored);
        assert!(!removed);
    }

    /// Why: writing the previous plist back is not the same as running it. A
    /// restore whose bootstrap fails is an outage, not a recovery.
    /// What: `Restored` only when the write AND the bootstrap both succeed.
    /// Test: this is the test.
    #[test]
    fn rollback_execution_reports_restored_only_when_bootstrap_succeeds() {
        assert_eq!(
            execute_rollback(RollbackPlan::RestorePrevious, || true, || {}, || true),
            Rollback::Restored
        );
        assert_eq!(
            execute_rollback(RollbackPlan::RestorePrevious, || true, || {}, || false),
            Rollback::Failed,
            "a restored file that will not load is still a down service"
        );
        assert_eq!(
            execute_rollback(
                RollbackPlan::RestorePrevious,
                || false,
                || {},
                || { unreachable!("must not bootstrap when the write failed") }
            ),
            Rollback::Failed
        );
    }

    /// Why: when the attempt genuinely started from nothing, leaving its
    /// half-registered job behind would strand a unit that never came up.
    /// What: no previous plist and no live label ⇒ remove and boot out.
    /// Test: this is the test.
    #[test]
    fn rollback_plan_boots_out_a_label_it_started_itself() {
        assert_eq!(rollback_plan(false, false), RollbackPlan::RemoveAndBootout);
        let mut removed = false;
        let outcome = execute_rollback(
            RollbackPlan::RemoveAndBootout,
            || unreachable!("no previous plist on this path"),
            || removed = true,
            || unreachable!("nothing to revive when nothing was running"),
        );
        assert_eq!(outcome, Rollback::NothingToRestore);
        assert!(removed, "a unit that never came up must not be left behind");
    }

    /// Why (#4919 review): the plist is not the whole unit. `make deploy`
    /// replaces the BINARY behind a byte-identical plist, so without `force`
    /// the content-equality skip reported `AlreadyCurrent` and launchd kept
    /// running the old image — a deploy that never deployed.
    /// What: `force` overrides the skip even when nothing else changed.
    /// Test: this is the test.
    #[test]
    fn reload_needed_is_true_when_forced() {
        assert!(
            reload_needed("<plist/>", Some("<plist/>"), true, true),
            "a forced install must activate the binary it just built"
        );
    }

    /// Why: the #4868 plist fixes only reach launchd if a CHANGED plist forces
    /// a reload. Treating "content differs" as skippable would reintroduce the
    /// exact failure — a corrected plist written but never activated.
    /// What: a differing installed plist ⇒ reload, even while loaded.
    /// Test: this is the test.
    #[test]
    fn reload_needed_is_true_when_the_plist_changed() {
        assert!(
            reload_needed(
                "<plist>new</plist>",
                Some("<plist>old</plist>"),
                true,
                false
            ),
            "a changed plist must be activated or its fixes never reach launchd"
        );
    }

    /// Why: a label that is not loaded must be bootstrapped even when the file
    /// on disk already matches — that is the "unloaded nothing" half of #4919,
    /// where the correct plist sat on disk inert.
    /// What: matching content but an unloaded label ⇒ reload.
    /// Test: this is the test.
    #[test]
    fn reload_needed_is_true_when_the_label_is_not_loaded() {
        assert!(reload_needed("<plist/>", Some("<plist/>"), false, false));
    }

    /// Why: a first install has nothing on disk to compare against.
    /// What: no installed plist ⇒ reload.
    /// Test: this is the test.
    #[test]
    fn reload_needed_is_true_on_first_install() {
        assert!(reload_needed("<plist/>", None, false, false));
        assert!(reload_needed("<plist/>", None, true, false));
    }
}
