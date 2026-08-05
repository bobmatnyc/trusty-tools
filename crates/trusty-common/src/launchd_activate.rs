//! Label-correct, non-destructive LaunchAgent activation (#4868).
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
//! Test: `cargo test -p trusty-common launchd_activate`. The `launchctl` calls
//! are side-effecting and are exercised by the daemons' own `service install`;
//! the decision logic that gates them is pure and unit-tested here.
#![cfg(target_os = "macos")]

use anyhow::{Context, Result};

use crate::launchd::{LaunchdConfig, current_uid};

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
    /// Delete the half-written plist and leave the running job alone.
    RemoveOnly,
    /// Delete the half-written plist and boot the label out.
    RemoveAndBootout,
}

/// Decide what a failed activation should undo.
///
/// Why (#4868 review): the pre-review code treated "no previous plist" as "no
/// service was running" and booted the label out. launchd keeps a job
/// REGISTERED after its plist file is deleted, so that combination is reachable
/// on a real host — and the bootout then took down a daemon that had been
/// running fine, in the name of rolling back.
///
/// What: a previous plist means restore it. With none, the label is left alone
/// if it was already loaded before this attempt, and booted out only if it was
/// not — which is the state the attempt found.
/// Test: `rollback_plan_restores_a_previous_unit`,
/// `rollback_plan_leaves_a_live_label_alone_when_there_was_no_plist`,
/// `rollback_plan_boots_out_a_label_it_started_itself`.
#[must_use]
pub fn rollback_plan(has_previous: bool, was_loaded: bool) -> RollbackPlan {
    if has_previous {
        RollbackPlan::RestorePrevious
    } else if was_loaded {
        RollbackPlan::RemoveOnly
    } else {
        RollbackPlan::RemoveAndBootout
    }
}

/// Decide whether the canonical unit needs reloading.
///
/// Why: the reload IS the outage. Skipping it when nothing changed is what
/// turns a re-run of `service install` from a one-minute gap into a no-op, and
/// keeping the decision pure is what makes that testable without launchd.
///
/// #4868 review: `force` exists because the plist is not the whole unit. A
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
    /// content-equality skip would leave launchd running the old image (#4868
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

        // #4868 review: capture liveness BEFORE `install()` overwrites the
        // plist. launchd keeps a job registered after its plist file is
        // deleted, so "no previous plist" does NOT imply "nothing was running"
        // — rolling back on that assumption booted out a live daemon.
        let was_loaded = self.is_loaded();

        if !reload_needed(&rendered, previous.as_deref(), was_loaded, force) {
            return Ok(Activation::AlreadyCurrent { evicted });
        }

        let replaced = previous.is_some();
        self.install()?;

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
    /// What: best-effort per label; a legacy unit that is absent, not loaded, or
    /// refuses to unload never fails the install, because the canonical unit is
    /// still the thing being installed.
    fn evict_legacy(&self, legacy_labels: &[&str]) -> Vec<String> {
        let mut evicted = Vec::new();
        for legacy in legacy_labels {
            let mut alias = self.clone();
            alias.label = (*legacy).to_string();
            let was_loaded = alias.is_loaded();
            let _ = alias.bootout();
            let removed = alias
                .plist_path()
                .ok()
                .filter(|p| p.exists())
                .map(|p| std::fs::remove_file(p).is_ok())
                .unwrap_or(false);
            if was_loaded || removed {
                evicted.push((*legacy).to_string());
            }
        }
        evicted
    }

    /// Bootstrap this label and confirm launchd actually has it.
    ///
    /// Why: `launchctl bootstrap` exiting 0 is not proof the job is loaded —
    /// #2498 recorded a bootstrap that succeeded while the daemon stayed "not
    /// running". Verifying closes the gap between "the command worked" and "the
    /// service is up", which is the difference the whole #4868 fix turns on.
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
    /// #4868 review: discarding the restoring `bootstrap()`'s result made the
    /// caller print "the service is not left down" even when the restore had
    /// failed and it was. The outcome is returned so the message can tell the
    /// truth.
    ///
    /// What: executes [`rollback_plan`], reporting whether the previous unit is
    /// actually running again.
    /// Test: the decision is covered by `rollback_plan_*`; the `launchctl` calls
    /// are side-effecting and are exercised by the daemons' `service install`.
    fn roll_back(
        &self,
        plist_path: &std::path::Path,
        previous: Option<&str>,
        was_loaded: bool,
    ) -> Rollback {
        match rollback_plan(previous.is_some(), was_loaded) {
            RollbackPlan::RestorePrevious => {
                let bytes = previous.unwrap_or_default();
                if std::fs::write(plist_path, bytes).is_err() {
                    return Rollback::Failed;
                }
                if self.bootstrap().is_err() || !self.is_loaded() {
                    return Rollback::Failed;
                }
                Rollback::Restored
            }
            RollbackPlan::RemoveOnly => {
                let _ = std::fs::remove_file(plist_path);
                Rollback::NothingToRestore
            }
            RollbackPlan::RemoveAndBootout => {
                let _ = std::fs::remove_file(plist_path);
                let _ = self.bootout();
                Rollback::NothingToRestore
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: an unchanged plist on a loaded label must not cost a restart — the
    /// unconditional bootout in the old sequence is where the ~1 minute of
    /// release downtime came from (#4868).
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

    /// Why (#4868 review): launchd keeps a job REGISTERED after its plist file
    /// is deleted, so "no previous plist" does not mean "nothing was running".
    /// The pre-review code booted out on that combination and took down a
    /// healthy daemon in the name of rolling back.
    /// What: with no previous plist but a live label, the job is left alone.
    /// Test: this is the test.
    #[test]
    fn rollback_plan_leaves_a_live_label_alone_when_there_was_no_plist() {
        assert_eq!(
            rollback_plan(false, true),
            RollbackPlan::RemoveOnly,
            "a rollback must never boot out a daemon that was already running"
        );
    }

    /// Why: when the attempt genuinely started from nothing, leaving its
    /// half-registered job behind would strand a unit that never came up.
    /// What: no previous plist and no live label ⇒ remove and boot out.
    /// Test: this is the test.
    #[test]
    fn rollback_plan_boots_out_a_label_it_started_itself() {
        assert_eq!(rollback_plan(false, false), RollbackPlan::RemoveAndBootout);
    }

    /// Why (#4868 review): the plist is not the whole unit. `make deploy`
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
    /// on disk already matches — that is the "unloaded nothing" half of #4868,
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
