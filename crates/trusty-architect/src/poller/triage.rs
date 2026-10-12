//! Which of a cycle's new alerts wake the Architect: the seam for
//! `fleet-poll.py`'s `triage_step` (988152c lines 1298-1328).
//!
//! Why: the prototype's rules and its Haiku gate are out of A3 (#9801); the
//! loop still needs the step, and its fallback (wake for every alert, log
//! none) is the prototype's own answer to any triage failure (#9171 A3b-2).
//! What: [`Triage::triage`] returns the alerts that wake and how many were
//! only logged; [`WakeAll`] wakes for all of them.
//! Test: `wake_message_uses_need_you_form_from_cycle`.

use super::state::PollState;
use crate::classify::Alert;

/// Decides which new alerts wake the Architect.
pub trait Triage {
    /// `(alerts that wake, count only logged)` for this cycle's `new` alerts.
    /// May keep its own records in `state`.
    fn triage(
        &self,
        new: &[Alert],
        now: f64,
        state: &mut PollState,
        dry_run: bool,
    ) -> (Vec<Alert>, u64);
}

/// Wakes for every new alert and logs none.
#[derive(Debug, Clone, Copy, Default)]
pub struct WakeAll;

impl Triage for WakeAll {
    fn triage(&self, new: &[Alert], _: f64, _: &mut PollState, _: bool) -> (Vec<Alert>, u64) {
        (new.to_vec(), 0)
    }
}
