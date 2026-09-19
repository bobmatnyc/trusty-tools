//! The per-session in-flight resume guard (#8233 acceptance item 4).
//!
//! Why: the owner's item 4 is "The supervisor does not act on, or append an
//! error to, a session another path is resuming." Two resume writers raced on
//! the live fleet: the picker's `resume` ran stop → recreate → launch, and the
//! supervisor's 30 s tick entered `resume_auto` for the same session inside
//! that window. The loser typed a second launch line into the same pane and
//! appended its own `[error: …]` to a record the winner was about to mark
//! `Active` — which is what produced "open failed: … runtime failed to start
//! (state=errored)" for a session that then came up fine.
//!
//! State alone cannot express this. A record is `Stopped`/`Errored` for the
//! whole first half of a resume, so a state check admits the second caller;
//! and the pane work, the breaker stamp and the relaunch all happen before the
//! record reaches `Active`. The exclusion has to be held across the WHOLE
//! resume, which is what this module adds.
//!
//! What: a set of session ids that currently have a resume in flight, plus an
//! RAII [`ResumeInFlightGuard`] that removes its id on drop. Drop is what makes
//! the release total — success, every error return, a panic, and a cancelled
//! future all run it, and a stuck entry would make a session permanently
//! unresumable, which is worse than the race it prevents. The set is a
//! `std::sync::Mutex` (not tokio's) precisely so `Drop` can release it without
//! an async context.
//!
//! Test: `crates/trusty-mpm/src/session_manager/resume_in_flight_tests.rs`.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

use super::manager::ManagedError;
use super::record::ManagedSessionId;

/// The in-flight set, shared between the manager and every live guard.
pub(crate) type InFlightSet = Arc<Mutex<HashSet<ManagedSessionId>>>;

/// Exclusive claim on resuming one session, released on drop.
///
/// Why: see this module's header — release must happen on EVERY exit path of
/// the resume, including a panic and a dropped future, so it is tied to the
/// value's lifetime rather than to any code the resume body has to reach.
/// What: holds the id and a handle on the shared set; [`Drop`] removes the id.
/// Never `Clone`, so one claim cannot outlive the resume that took it.
/// Test: `a_dropped_resume_future_releases_the_guard`,
/// `a_failed_resume_releases_the_guard`.
pub(crate) struct ResumeInFlightGuard {
    id: ManagedSessionId,
    set: InFlightSet,
}

impl Drop for ResumeInFlightGuard {
    fn drop(&mut self) {
        // A poisoned lock still holds a usable set: the only code that runs
        // under it is `HashSet` insert/remove, so `into_inner` recovers rather
        // than leaving the id stuck in flight forever.
        self.set
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
    }
}

impl super::SessionManager {
    /// Claim the right to resume `id`, or refuse because someone else holds it.
    ///
    /// Why: this is the whole of #8233 item 4 — the second concurrent resume of
    /// one session must do NOTHING, rather than spawn a second runtime into the
    /// pane, stamp the flap breaker, or append an error to the record the other
    /// path is about to mark `Active`. Callers take it as the first statement
    /// of the resume so that every later step is inside the claim.
    /// What: inserts `id` into the shared set. `HashSet::insert` returning
    /// `false` means a claim is already live, which becomes
    /// [`ManagedError::ResumeInFlight`]; `true` hands back a guard whose drop
    /// removes the id. Claims are per-id, so resumes of different sessions
    /// never see each other.
    /// Test: `a_second_concurrent_resume_of_one_session_is_refused`,
    /// `concurrent_resumes_of_different_sessions_both_proceed`.
    pub(crate) fn begin_resume(
        &self,
        id: &ManagedSessionId,
    ) -> Result<ResumeInFlightGuard, ManagedError> {
        let fresh = self
            .resume_in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(*id);
        if !fresh {
            return Err(ManagedError::ResumeInFlight(id.to_string()));
        }
        Ok(ResumeInFlightGuard {
            id: *id,
            set: Arc::clone(&self.resume_in_flight),
        })
    }
}

#[cfg(test)]
#[path = "resume_in_flight_tests.rs"]
mod tests;
