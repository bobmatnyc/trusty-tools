//! Wait for launchd to finish a bootout before bootstrapping again (#6618).
//!
//! Why: a live restart issued `bootout` and `bootstrap` back to back. launchd
//! unloads a job ASYNCHRONOUSLY — `launchctl bootout` returns as soon as the
//! request is accepted, not when the job is gone — so the bootstrap arrived
//! while the previous instance was still registered and launchd refused it with
//! `Bootstrap failed: 5: Input/output error`. The same command seconds later
//! succeeded, which is what identifies the failure as a race rather than a bad
//! plist or a disabled service.
//!
//! [`crate::launchd_activate`] already solved the other half of this window for
//! `service install` (#6590): a unit whose loaded `ExitTimeOut` is shorter than
//! the daemon's flush gets a directly-delivered SIGTERM first. The restart path
//! predates that and reached neither guard, so this module orders BOTH around
//! one sequence and both callers share it.
//!
//! What: [`await_unload`](crate::launchd_restart::await_unload) polls until
//! launchd stops reporting the label, and
//! [`restart_sequence`](crate::launchd_restart::restart_sequence) orders the
//! whole bounce — quiesce, boot out, wait,
//! bootstrap, and one retry after a second wait. Every effect is injected, so
//! the ORDERING is what the tests assert, without a real unit.
//! [`crate::launchd::LaunchdConfig::restart_gracefully`] binds the real
//! `launchctl` calls, the real signal, and the real clock.
//!
//! Test: `cargo test -p trusty-common --features unconditional-only
//! launchd_restart`.
#![cfg(target_os = "macos")]

use std::time::Duration;

use crate::launchd::LaunchdConfig;

/// What a bounded wait for launchd to unload a label observed.
///
/// Why: "the label went away" and "the budget ran out with it still there" lead
/// to the same next action — attempt the bootstrap — but they must be
/// distinguishable in the failure message, because only the second explains a
/// bootstrap that fails again for the same reason it failed the first time.
/// What: both arms carry the seconds waited, so a message never has to say
/// "some time".
/// Test: `await_unload_is_immediate_when_the_label_is_already_gone`,
/// `await_unload_waits_for_the_label_to_disappear`,
/// `await_unload_gives_up_at_the_budget`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Unload {
    /// launchd no longer reports the label. A bootstrap now cannot race it.
    Gone {
        /// Seconds waited before the label was observed gone.
        waited_secs: u64,
    },
    /// The label was still registered when the budget ran out.
    StillLoaded {
        /// Seconds waited — the whole budget.
        waited_secs: u64,
    },
}

impl Unload {
    /// Seconds this wait spent, whichever way it ended.
    #[must_use]
    pub fn waited_secs(&self) -> u64 {
        match self {
            Unload::Gone { waited_secs } | Unload::StillLoaded { waited_secs } => *waited_secs,
        }
    }

    /// Whether launchd had actually let go of the label.
    #[must_use]
    pub fn is_gone(&self) -> bool {
        matches!(self, Unload::Gone { .. })
    }

    /// One phrase naming both the duration and the outcome, for a message.
    fn phrase(&self) -> String {
        match self {
            Unload::Gone { waited_secs } => format!("{waited_secs}s (label gone)"),
            Unload::StillLoaded { waited_secs } => {
                format!("{waited_secs}s (label STILL registered)")
            }
        }
    }
}

/// Poll until launchd stops reporting `is_loaded`, bounded by `budget_secs`.
///
/// Why: this is the wait the #6618 restart never performed. `launchctl bootout`
/// reports that the unload was ACCEPTED; the job stays registered while launchd
/// terminates it, and a bootstrap issued in that window is refused. Polling the
/// same `launchctl print` the rest of this crate uses is the only observation of
/// "actually gone" available.
///
/// What: probes once before waiting at all — the overwhelmingly common case is a
/// job that has already unloaded, and that case must cost nothing. Then one
/// probe per tick for at most `budget_secs`. Effects are injected: `is_loaded`
/// is the probe and `tick` is the wait between probes.
///
/// # Postconditions
///
/// [`Unload::Gone`] means `is_loaded` answered `false`. A budget of `0` can only
/// return `Gone { waited_secs: 0 }` or `StillLoaded { waited_secs: 0 }`.
///
/// Test: `await_unload_is_immediate_when_the_label_is_already_gone`,
/// `await_unload_waits_for_the_label_to_disappear`,
/// `await_unload_gives_up_at_the_budget`.
pub fn await_unload(
    budget_secs: u64,
    mut is_loaded: impl FnMut() -> bool,
    mut tick: impl FnMut(),
) -> Unload {
    if !is_loaded() {
        return Unload::Gone { waited_secs: 0 };
    }
    for waited_secs in 1..=budget_secs {
        tick();
        if !is_loaded() {
            return Unload::Gone { waited_secs };
        }
    }
    Unload::StillLoaded {
        waited_secs: budget_secs,
    }
}

/// What a completed restart did, so the caller can report the wait it paid.
///
/// Why: an operator reading `tctl restart` output needs to see that the wait
/// happened — a silent fix for a race is indistinguishable from the race not
/// having occurred, and the two demand different follow-up when it recurs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Restarted {
    /// What the wait preceding the SUCCESSFUL bootstrap observed.
    pub unload: Unload,
    /// `launchctl bootstrap` calls it took: `1`, or `2` after the retry.
    pub attempts: u32,
}

/// Order one launchd bounce so no step races the one before it.
///
/// Why: the ordering IS the fix. Quiescing after the bootout is useless (launchd
/// has already begun the termination the quiesce exists to pre-empt), and
/// bootstrapping before the wait is the #6618 defect verbatim. Injecting every
/// effect is what lets that order be asserted rather than described.
///
/// What, in order: (1) `quiesce` — the #6590 guard, which stops the daemon
/// directly when the LOADED unit's grace is too short for its flush; (2)
/// `bootout`; (3) `await_unload`; (4) `bootstrap`. A bootstrap that still fails
/// gets exactly one retry, and only after a SECOND wait — retrying immediately
/// would repeat the losing race, which is what makes a bare retry the wrong fix.
///
/// # Preconditions
///
/// `bootout` must not be fail-open: its `Err` aborts before any bootstrap, so a
/// unit that could not be stopped is never bootstrapped on top of itself.
///
/// # Errors
///
/// When `bootout` fails, or when both bootstrap attempts fail — the message then
/// names the label and both waits.
///
/// Test: `restart_waits_for_the_unload_before_bootstrapping`,
/// `restart_retries_the_bootstrap_once_after_a_second_wait`,
/// `restart_error_names_the_label_and_the_waits`,
/// `restart_does_not_bootstrap_when_the_bootout_fails`.
pub fn restart_sequence(
    label: &str,
    quiesce: impl FnOnce(),
    bootout: impl FnOnce() -> Result<(), String>,
    mut await_unload: impl FnMut() -> Unload,
    mut bootstrap: impl FnMut() -> Result<(), String>,
) -> Result<Restarted, String> {
    quiesce();
    bootout()?;

    let first_wait = await_unload();
    let Err(first_error) = bootstrap() else {
        return Ok(Restarted {
            unload: first_wait,
            attempts: 1,
        });
    };

    // #6618: the retry is what the operator did by hand, but it only worked
    // because seconds had passed. Waiting again before it is the difference
    // between a retry and a second roll of the same dice.
    let second_wait = await_unload();
    match bootstrap() {
        Ok(()) => Ok(Restarted {
            unload: second_wait,
            attempts: 2,
        }),
        Err(second_error) => Err(format!(
            "restarting {label} failed: booted out successfully, then `launchctl \
             bootstrap` failed twice. After waiting {} — {first_error}. After a \
             further {} — {second_error}",
            first_wait.phrase(),
            second_wait.phrase()
        )),
    }
}

impl LaunchdConfig {
    /// Bounce this unit, giving launchd time to finish the bootout (#6618).
    ///
    /// Why/What: see [`restart_sequence`] — this binds it to the real
    /// `launchctl`, the real SIGTERM (through
    /// [`crate::launchd_activate`]'s #6590 guard), and a one-second clock. The
    /// wait budget is [`crate::shutdown::termination_grace`], the same window
    /// the daemon plans its flush inside: launchd cannot deregister the job
    /// before the process it is terminating has gone, so a shorter budget would
    /// give up while the unload was still legitimately in progress.
    ///
    /// # Errors
    ///
    /// As [`restart_sequence`].
    ///
    /// Test: the ordering and the retry are unit-tested over the injected
    /// sequence; the `launchctl` calls themselves are side-effecting and are
    /// exercised by `tctl restart`.
    pub fn restart_gracefully(&self) -> anyhow::Result<Restarted> {
        let budget_secs = crate::shutdown::termination_grace().as_secs();
        restart_sequence(
            &self.label,
            || self.guard_short_grace(budget_secs),
            || self.bootout().map_err(|e| format!("{e:#}")),
            || self.await_unload(budget_secs),
            || self.bootstrap().map_err(|e| format!("{e:#}")),
        )
        .map_err(anyhow::Error::msg)
    }

    /// [`await_unload`] over this label's real `launchctl print` and clock.
    fn await_unload(&self, budget_secs: u64) -> Unload {
        await_unload(
            budget_secs,
            || self.is_loaded(),
            || std::thread::sleep(Duration::from_secs(1)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// The label the observed #6618 failure named.
    const LABEL: &str = "com.trusty.memory";

    /// The refusal launchd actually returned when the bootstrap raced the
    /// bootout.
    const RACE_ERROR: &str = "launchctl bootstrap gui/502 \
                              …/com.trusty.memory.plist failed: Bootstrap \
                              failed: 5: Input/output error";

    /// Records the order effects ran in, so ordering is asserted not described.
    #[derive(Default)]
    struct Steps(RefCell<Vec<&'static str>>);

    impl Steps {
        fn push(&self, step: &'static str) {
            self.0.borrow_mut().push(step);
        }
        fn taken(&self) -> Vec<&'static str> {
            self.0.borrow().clone()
        }
    }

    /// Why (#6618): this is the defect. The pre-fix restart ran
    /// `bootout` then `bootstrap` with nothing between them, so the bootstrap
    /// reached launchd while the label was still registered. Asserting the step
    /// ORDER — not merely that a wait exists somewhere — is what makes the test
    /// fail against that sequence.
    /// What: a clean restart records quiesce, bootout, the wait, and exactly one
    /// bootstrap, in that order.
    /// Test: itself.
    #[test]
    fn restart_waits_for_the_unload_before_bootstrapping() {
        let steps = Steps::default();
        let outcome = restart_sequence(
            LABEL,
            || steps.push("quiesce"),
            || {
                steps.push("bootout");
                Ok(())
            },
            || {
                steps.push("await_unload");
                Unload::Gone { waited_secs: 3 }
            },
            || {
                steps.push("bootstrap");
                Ok(())
            },
        )
        .expect("a clean bounce succeeds");

        assert_eq!(
            steps.taken(),
            vec!["quiesce", "bootout", "await_unload", "bootstrap"],
            "the bootstrap must not be issued until launchd has released the label"
        );
        assert_eq!(
            outcome,
            Restarted {
                unload: Unload::Gone { waited_secs: 3 },
                attempts: 1,
            }
        );
    }

    /// Why (#6618): the operator's manual remedy was a plain retry, and it
    /// worked only because seconds had elapsed. A retry that skips the second
    /// wait re-runs the losing race, so the wait between the two attempts is the
    /// part under test.
    /// What: a first bootstrap that fails is followed by a SECOND wait and then
    /// a second bootstrap; the report says it took two attempts.
    /// Test: itself.
    #[test]
    fn restart_retries_the_bootstrap_once_after_a_second_wait() {
        let steps = Steps::default();
        let attempts = RefCell::new(0_u32);
        let outcome = restart_sequence(
            LABEL,
            || steps.push("quiesce"),
            || {
                steps.push("bootout");
                Ok(())
            },
            || {
                steps.push("await_unload");
                Unload::Gone { waited_secs: 2 }
            },
            || {
                steps.push("bootstrap");
                *attempts.borrow_mut() += 1;
                if *attempts.borrow() == 1 {
                    Err(RACE_ERROR.to_owned())
                } else {
                    Ok(())
                }
            },
        )
        .expect("the retry recovers the observed race");

        assert_eq!(
            steps.taken(),
            vec![
                "quiesce",
                "bootout",
                "await_unload",
                "bootstrap",
                "await_unload",
                "bootstrap"
            ],
            "the retry must wait for the unload again, not re-roll the same race"
        );
        assert_eq!(outcome.attempts, 2);
    }

    /// Why: an error that says only "bootstrap failed" leaves the operator
    /// unable to tell a race from a broken plist — which is precisely the
    /// confusion #6618 was filed out of.
    /// What: both attempts failing yields a message carrying the label, both
    /// waits, and both underlying errors.
    /// Test: itself.
    #[test]
    fn restart_error_names_the_label_and_the_waits() {
        let err = restart_sequence(
            LABEL,
            || {},
            || Ok(()),
            || Unload::StillLoaded { waited_secs: 60 },
            || Err(RACE_ERROR.to_owned()),
        )
        .expect_err("two failed bootstraps are an error");

        assert!(err.contains(LABEL), "the label must be named: {err}");
        assert!(
            err.contains("60s (label STILL registered)"),
            "the wait and its outcome must be named: {err}"
        );
        assert!(
            err.contains("Input/output error"),
            "launchd's own reason must survive: {err}"
        );
    }

    /// Why: a bootout that failed leaves the old instance running. Bootstrapping
    /// on top of it is the #4230 two-daemons-one-port shape, so the failure must
    /// abort the sequence rather than fall through.
    /// What: an erroring bootout reaches neither the wait nor the bootstrap.
    /// Test: itself.
    #[test]
    fn restart_does_not_bootstrap_when_the_bootout_fails() {
        let steps = Steps::default();
        let err = restart_sequence(
            LABEL,
            || steps.push("quiesce"),
            || Err("launchctl bootout failed: Operation not permitted".to_owned()),
            || {
                steps.push("await_unload");
                unreachable!("no wait after a bootout that failed")
            },
            || {
                steps.push("bootstrap");
                unreachable!("never bootstrap on top of a unit still running")
            },
        )
        .expect_err("a failed bootout is an error");

        assert!(err.contains("Operation not permitted"));
        assert_eq!(steps.taken(), vec!["quiesce"]);
    }

    /// Why: the common case is a job that has already unloaded, and paying a
    /// tick for it would add a second to every restart on every host.
    /// What: an already-absent label returns immediately, having never ticked.
    /// Test: itself.
    #[test]
    fn await_unload_is_immediate_when_the_label_is_already_gone() {
        let outcome = await_unload(60, || false, || unreachable!("nothing to wait for"));
        assert_eq!(outcome, Unload::Gone { waited_secs: 0 });
        assert!(outcome.is_gone());
    }

    /// Why (#6618): the observed race resolved in seconds — the operator's
    /// retry succeeded on the first try after a short pause. The wait must
    /// therefore outlive a couple of ticks and stop as soon as the label goes,
    /// rather than sleeping the whole budget.
    /// What: a label present for three probes and gone on the fourth reports
    /// three ticks, well inside a 60 s budget.
    /// Test: itself.
    #[test]
    fn await_unload_waits_for_the_label_to_disappear() {
        let probes = RefCell::new(0_u64);
        let ticks = RefCell::new(0_u64);
        let outcome = await_unload(
            60,
            || {
                *probes.borrow_mut() += 1;
                *probes.borrow() <= 3
            },
            || *ticks.borrow_mut() += 1,
        );
        assert_eq!(outcome, Unload::Gone { waited_secs: 3 });
        assert_eq!(*ticks.borrow(), 3, "one probe per tick, no busy loop");
    }

    /// Why: a wedged unit must not stall `tctl restart` forever — the wait is
    /// bounded, and the caller still attempts the bootstrap afterwards.
    /// What: a label that never goes away consumes exactly the budget.
    /// Test: itself.
    #[test]
    fn await_unload_gives_up_at_the_budget() {
        let ticks = RefCell::new(0_u64);
        let outcome = await_unload(4, || true, || *ticks.borrow_mut() += 1);
        assert_eq!(outcome, Unload::StillLoaded { waited_secs: 4 });
        assert!(!outcome.is_gone());
        assert_eq!(*ticks.borrow(), 4);
    }
}
