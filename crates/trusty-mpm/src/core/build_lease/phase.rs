//! A lease's two phases: compiling, then running (#8692).
//!
//! Why: one slot flock used to be both the compile-capacity count and the
//! target-directory ownership, so a `cargo run` server or a long `cargo test`
//! run held compile capacity it no longer used, and every other leased build
//! waited behind it. Cargo drops its build lock (`<profile>/.cargo-lock`)
//! before it runs test binaries or execs a `cargo run` program, so that lock
//! says when compilation is over.
//!
//! What: [`Phase`] is persisted in the holder record; a record without it
//! reads as [`Phase::Compile`], so a record an older `tm` wrote always counts
//! for capacity. [`PhaseTracker`] is the one-way flip: it moves to
//! [`Phase::Run`] after it has seen the build lock held once and then free on
//! [`FREE_READS_TO_FLIP`] consecutive polls. A probe error reads as held, and
//! a failed record write keeps the phase at [`Phase::Compile`] and is retried
//! on the next poll, so every failure keeps the slot counted. A lock never seen
//! held never flips: the build keeps its capacity until it exits, as before.
//! Only a lease whose target plan is the slot-owned `Slot` plan flips
//! ([`flip_lock_dir`]); a `Keep` or `Unset` lease stays compiling until it
//! exits, as before, until PR B (#9223) locks shared directories.
//! A run-phase holder keeps its flock and its target directory until it exits;
//! only the capacity count changes. Its later compilers (a `cargo watch`
//! rebuild) are left to the census, which counts them as foreign.
//! Test: `phase_tests.rs`.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::target_dir::TargetPlan;

/// How often the holder reads cargo's build lock.
pub const PHASE_POLL: Duration = Duration::from_millis(250);

/// Consecutive free reads, after one held read, that end the compile phase.
///
/// Why: one free read can fall between two cargo invocations of a wrapper
/// script, or between cargo's resolve and its compile; two reads 250 ms apart
/// make that gap unlikely to flip a build that is still compiling.
pub const FREE_READS_TO_FLIP: u8 = 2;

/// What a leased command is doing now.
///
/// Test: `a_record_without_a_phase_counts_for_capacity`,
/// `an_older_reader_ignores_the_phase_field`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Phase {
    /// Compiling, or not yet known to be done: counts against the ceiling.
    #[default]
    Compile,
    /// Cargo released its build lock: owns its slot, not compile capacity.
    Run,
}

/// Where the flip stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum State {
    /// The build lock has not been seen held yet.
    #[default]
    Waiting,
    /// Seen held; `free` consecutive free reads since.
    SeenHeld { free: u8 },
    /// The flip is decided but not yet written to the record.
    Unwritten,
    /// The record says [`Phase::Run`]; nothing more to do.
    Written,
}

/// The one-way compile-to-run flip for one leased command.
///
/// Test: every test in `phase_tests.rs`.
#[derive(Debug, Default)]
pub struct PhaseTracker {
    state: State,
}

impl PhaseTracker {
    /// A tracker that has seen nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// One poll: read the build lock with `probe`, and persist the flip with
    /// `write` once it is due.
    ///
    /// What: `probe` returns whether cargo's build lock is held; an `Err`
    /// reads as held. `write` is called only when the flip is due, again on
    /// each later poll until it succeeds, and never after. Returns the phase
    /// the record now holds. Once [`Phase::Run`] is written, neither closure
    /// runs again.
    /// Test: `a_lock_never_seen_held_never_flips`,
    /// `one_free_read_after_a_held_one_does_not_flip`,
    /// `two_free_reads_after_a_held_one_flip`,
    /// `a_probe_error_reads_as_held`,
    /// `a_failed_write_stays_compile_and_retries`.
    pub fn tick(
        &mut self,
        probe: impl FnOnce() -> std::io::Result<bool>,
        write: impl FnOnce() -> std::io::Result<()>,
    ) -> Phase {
        if self.state == State::Written {
            return Phase::Run;
        }
        if self.state != State::Unwritten {
            // #8692: an unreadable lock is a held lock — never a reason to flip.
            let held = probe().unwrap_or(true);
            self.state = match (self.state, held) {
                (_, true) => State::SeenHeld { free: 0 },
                (State::SeenHeld { free }, false) if free + 1 >= FREE_READS_TO_FLIP => {
                    State::Unwritten
                }
                (State::SeenHeld { free }, false) => State::SeenHeld { free: free + 1 },
                (state, false) => state,
            };
        }
        if self.state == State::Unwritten && write().is_ok() {
            self.state = State::Written;
            return Phase::Run;
        }
        Phase::Compile
    }

    /// Whether the run phase is written, so polling can stop.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.state == State::Written
    }
}

/// The directory whose build lock ends the compile phase, or `None` when
/// this lease must stay [`Phase::Compile`] until it exits.
///
/// What: `Some(target)` only for a [`TargetPlan::Slot`] lease — the slot's
/// own directory, which no other lease builds in while the flock is held.
/// A `Keep` or `Unset` plan builds in a directory another build can share,
/// so a foreign `.cargo-lock` there could flip this lease while it still
/// compiles, and a second lease could rebuild binaries its tests use (#9223).
/// Test: `only_a_slot_plan_has_a_flip_lock_dir`,
/// `a_shared_dir_lease_keeps_its_capacity_while_its_test_binary_runs_8692`.
#[must_use]
pub fn flip_lock_dir(plan: &TargetPlan, target: Option<&str>) -> Option<PathBuf> {
    // #8692: slot-owned dirs only; PR B (#9223) adds a dir lock and lifts this.
    match plan {
        TargetPlan::Slot { .. } => target.map(PathBuf::from),
        _ => None,
    }
}

#[cfg(test)]
#[path = "phase_tests.rs"]
mod tests;
