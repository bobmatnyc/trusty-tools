//! Waiting for, and taking, one build slot (#8261).
//!
//! Why: the `PreToolUse` hook has a 10-second budget and cannot queue a build,
//! so the waiting happens in the rewritten command itself, bounded below the
//! Bash tool's own timeout so the refusal — naming who holds the machine —
//! reaches the agent instead of a silent kill.
//!
//! What: [`acquire`] loops until a deadline: take the machine-wide admission
//! lock, count the live leases, take the readings, [`decide`], and on admit
//! take the preferred free slot. When no lease can be taken at all — no slot
//! directory, an unopenable `admission.lock`, every candidate slot file broken
//! — [`acquire_unleased`] takes over: the build may run WITHOUT a lease only
//! while the census says fewer than `ceiling` builds are running without one
//! ([`decide_unleased`]); otherwise it waits and times out like a leased one.
//! Test: `acquire_tests.rs`.

use std::time::{Duration, Instant};

use super::admission::{Decision, decide, decide_unleased};
use super::census::Sampler;
use super::config::BuildLeaseConfig;
use super::slots::{HolderRecord, SlotDir, SlotGuard};
use crate::core::builders::BuildersConfig;

/// How one acquire ended.
///
/// Test: every test in `acquire_tests.rs`.
#[derive(Debug)]
#[non_exhaustive]
pub enum Outcome {
    /// A slot is held until the guard drops.
    Leased {
        /// The held slot.
        guard: SlotGuard,
        /// The decision that admitted it.
        decision: Decision,
    },
    /// The deadline passed without admission.
    TimedOut {
        /// The last decision taken.
        decision: Decision,
        /// Who held the slots at that moment.
        holders: Vec<HolderRecord>,
    },
    /// No lease could be taken; the census admitted the build without one.
    Unleased {
        /// Why no lease could be taken.
        why: String,
        /// The census-bounded decision that admitted it.
        decision: Decision,
    },
}

/// What an acquire needs besides its collaborators.
///
/// Test: every test in `acquire_tests.rs`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AcquireParams<'a> {
    /// The `[builders]` section.
    pub builders: &'a BuildersConfig,
    /// The build-lease keys of `[builders]`.
    pub lease: &'a BuildLeaseConfig,
    /// `builders.max_concurrent`, resolved.
    pub ceiling: u32,
    /// How long to wait in total.
    pub wait: Duration,
    /// How long to sleep between refused polls.
    pub poll: Duration,
    /// The checkout root, for slot affinity.
    pub checkout: &'a str,
}

impl<'a> AcquireParams<'a> {
    /// Parameters with the default 2-second poll.
    #[must_use]
    pub fn new(
        builders: &'a BuildersConfig,
        lease: &'a BuildLeaseConfig,
        ceiling: u32,
        wait: Duration,
        checkout: &'a str,
    ) -> Self {
        Self {
            builders,
            lease,
            ceiling,
            wait,
            poll: Duration::from_secs(2),
            checkout,
        }
    }

    /// The same parameters with another poll interval.
    #[must_use]
    pub fn with_poll(mut self, poll: Duration) -> Self {
        self.poll = poll;
        self
    }
}

/// Wait for a slot, bounded by `params.wait`.
///
/// What: [`Outcome::Leased`] as soon as a decision admits and a slot is taken;
/// [`Outcome::TimedOut`] at the deadline; when the admission lock cannot be
/// opened or every candidate slot file is broken, the rest of the wait is
/// [`acquire_unleased`]'s. `on_wait` sees each refused decision.
/// Test: `a_free_machine_leases_immediately`,
/// `the_n_plus_first_waits_then_times_out`, `a_slot_freed_mid_wait_is_taken`,
/// `warn_pressure_times_out_while_the_holder_keeps_its_slot`,
/// `a_broken_lowest_slot_is_skipped`, `an_unopenable_admission_lock_is_bounded`,
/// `unusable_slot_files_fall_back_to_the_census_bound`.
pub fn acquire(
    slots: &SlotDir,
    params: &AcquireParams<'_>,
    sampler: &mut dyn Sampler,
    on_wait: &mut dyn FnMut(&Decision, &[HolderRecord]),
) -> Outcome {
    let deadline = Instant::now() + params.wait;
    loop {
        let admission = match slots.lock_admission(deadline) {
            Ok(Some(lock)) => lock,
            Ok(None) => return timed_out(slots, params, sampler),
            Err(err) => {
                let why = format!(
                    "the admission lock in {} could not be opened: {err}",
                    slots.path().display()
                );
                return acquire_unleased(params, deadline, why, sampler, on_wait);
            }
        };
        let holders = slots.holders();
        let held = u32::try_from(holders.len()).unwrap_or(u32::MAX);
        let readings = sampler.sample(&holders);
        let decision = decide(
            params.builders,
            params.lease,
            params.ceiling,
            held,
            &readings,
        );
        if decision.admit {
            match take_a_slot(slots, params, held) {
                Ok(Some(guard)) => {
                    drop(admission);
                    slots.remember_checkout(guard.slot(), params.checkout);
                    return Outcome::Leased { guard, decision };
                }
                Ok(None) => {}
                Err(why) => {
                    drop(admission);
                    return acquire_unleased(params, deadline, why, sampler, on_wait);
                }
            }
        }
        drop(admission);
        on_wait(&decision, &holders);
        if Instant::now() + params.poll >= deadline {
            return Outcome::TimedOut { decision, holders };
        }
        std::thread::sleep(params.poll);
    }
}

/// The timed-out outcome, with one last reading for the message.
fn timed_out(slots: &SlotDir, params: &AcquireParams<'_>, sampler: &mut dyn Sampler) -> Outcome {
    let holders = slots.holders();
    let readings = sampler.sample(&holders);
    let held = u32::try_from(holders.len()).unwrap_or(u32::MAX);
    let decision = decide(
        params.builders,
        params.lease,
        params.ceiling,
        held,
        &readings,
    );
    Outcome::TimedOut { decision, holders }
}

/// Wait, without any lease, for the census to admit this build.
///
/// Why: see the module doc — an unleased build must not be unbounded.
/// What: polls [`Sampler::sample_unleased`] and [`decide_unleased`] until it
/// admits ([`Outcome::Unleased`]) or `deadline` passes ([`Outcome::TimedOut`]).
/// Test: `an_unopenable_admission_lock_is_bounded`,
/// `unusable_slot_files_fall_back_to_the_census_bound`.
pub fn acquire_unleased(
    params: &AcquireParams<'_>,
    deadline: Instant,
    why: String,
    sampler: &mut dyn Sampler,
    on_wait: &mut dyn FnMut(&Decision, &[HolderRecord]),
) -> Outcome {
    loop {
        let readings = sampler.sample_unleased();
        let decision = decide_unleased(params.lease, params.ceiling, &readings);
        if decision.admit {
            return Outcome::Unleased { why, decision };
        }
        on_wait(&decision, &[]);
        if Instant::now() + params.poll >= deadline {
            return Outcome::TimedOut {
                decision,
                holders: Vec::new(),
            };
        }
        std::thread::sleep(params.poll);
    }
}

/// Take the first free slot in preference order.
///
/// What: candidates are `0..ceiling + held + broken`, so neither a holder left
/// above a lowered ceiling nor a broken slot file hides a usable index — a
/// broken file is skipped, never a reason to disable the cap. `Ok(None)` when
/// every candidate is held; `Err` only when no candidate could be locked and
/// none was held.
fn take_a_slot(
    slots: &SlotDir,
    params: &AcquireParams<'_>,
    held: u32,
) -> Result<Option<SlotGuard>, String> {
    let broken = u32::try_from(slots.broken().len()).unwrap_or(u32::MAX);
    let limit = params
        .ceiling
        .saturating_add(held)
        .saturating_add(broken)
        .max(1);
    let mut errors = Vec::new();
    let mut any_held = false;
    for slot in slots.preference_order(limit, params.checkout) {
        match slots.try_acquire(slot) {
            Ok(Some(guard)) => return Ok(Some(guard)),
            Ok(None) => any_held = true,
            Err(err) => errors.push(format!("slot-{slot}.lock: {err}")),
        }
    }
    if !any_held && !errors.is_empty() {
        return Err(format!(
            "no slot file in {} could be locked ({})",
            slots.path().display(),
            errors.join("; ")
        ));
    }
    Ok(None)
}

#[cfg(test)]
#[path = "acquire_tests.rs"]
mod tests;
