//! The `tm issue transition` → tracker sync hook (#8448).
//!
//! Why: the `phases` block is regenerated from child state, and until this hook
//! the regeneration ran only when a session remembered `tm issue epic sync`
//! after a transition — O3 of the plan is that a phase closing never leaves a
//! stale tracker without anyone remembering. The obvious wrong implementation
//! fails open: the label advances, the sync throws, the error becomes a warning,
//! and the tracker is stale with nothing recording it. Here the sync's failure
//! is the command's failure (AC1), and the message says which tracker is stale
//! and how to repair it.
//! What: [`transition_with_tracker_sync`] runs the ordinary
//! [`super::super::ops::transition`], then — for a phase-titled issue that is
//! not a no-op — [`sync_tracker_of_phase`] reads its parent and syncs it. An
//! issue whose title is not `[EPIC_<n> PHASE_<m>]` costs no `EpicBackend` call
//! at all, and a phase-titled issue with no parent costs one parent read and no
//! tracker read or write (AC2).
//! Test: `transition_hook_regenerates_the_parent_trackers_block`,
//! `transition_hook_fails_the_command_when_the_sync_fails`,
//! `transition_hook_makes_no_epic_call_for_a_non_phase_title`,
//! `transition_hook_reads_no_tracker_for_a_parentless_phase`,
//! `transition_hook_skips_a_no_op_transition`,
//! `transition_hook_names_the_tracker_when_the_parent_read_fails`.

use crate::commands::issue::config::StateModel;
use crate::commands::issue::ops::{self, TransitionReport};
use crate::commands::ticket::system::TicketSystem;

use super::backend::EpicBackend;
use super::render;
use super::sync::{self, SyncReport};

/// A transition and the tracker sync it triggered, if any.
///
/// Test: `transition_hook_regenerates_the_parent_trackers_block`.
#[derive(Debug)]
pub(crate) struct HookedTransition {
    /// What the transition itself did.
    pub(crate) report: TransitionReport,
    /// The tracker sync, when the issue is a linked phase and the transition
    /// was not a no-op.
    pub(crate) synced: Option<SyncReport>,
}

/// `tm issue transition`, followed by the phase's tracker sync.
///
/// Why: one function composes the two so the dispatcher cannot swallow the
/// second half — the fail-open shape AC1 exists to forbid is a `?` missing at
/// exactly this seam.
/// What: runs the transition; a no-op (#8003) changed nothing and syncs
/// nothing; otherwise hands the issue's title — read by the transition's own
/// `validate`, so a non-phase costs no extra call — to
/// [`sync_tracker_of_phase`], whose error is this function's error.
/// Test: see the module doc.
pub(crate) fn transition_with_tracker_sync<S: TicketSystem, B: EpicBackend>(
    sys: &S,
    epic: &B,
    model: &StateModel,
    issue: u64,
    to: &str,
    note: Option<&str>,
) -> anyhow::Result<HookedTransition> {
    let report = ops::transition(sys, model, issue, to, note)?;
    if report.no_op {
        return Ok(HookedTransition {
            report,
            synced: None,
        });
    }
    let synced = sync_tracker_of_phase(epic, issue, &report.title, to)?;
    Ok(HookedTransition { report, synced })
}

/// Regenerate the tracker of a phase issue that just changed state.
///
/// Why: called AFTER the label moved, so every failure arm has to say two
/// things — that the transition itself stands, and which tracker is now stale
/// — or an operator reading the exit code would retry a transition that is
/// already a no-op and never learn the tracker needs `sync`.
/// What: `Ok(None)` for a title that is not a phase title (no call made) or a
/// phase with no parent (one parent read). Otherwise syncs the parent. A parent
/// read that fails names the tracker by the title's embedded number; a sync
/// that fails names the parent. Both carry the `tm issue epic sync <n>` repair.
/// Test: `transition_hook_fails_the_command_when_the_sync_fails`,
/// `transition_hook_names_the_tracker_when_the_parent_read_fails`.
pub(crate) fn sync_tracker_of_phase<B: EpicBackend>(
    epic: &B,
    issue: u64,
    title: &str,
    to: &str,
) -> anyhow::Result<Option<SyncReport>> {
    // #8448 AC2: the title is the gate. Not a phase title → not a call.
    if render::phase_number_of(title).is_none() {
        return Ok(None);
    }
    let parent = match epic.parent(issue) {
        Ok(parent) => parent,
        Err(e) => {
            let by_title = render::epic_number_of(title)
                .map_or_else(|| "its tracker".to_string(), |n| format!("tracker #{n}"));
            let repair = render::epic_number_of(title)
                .map_or_else(|| "<tracker>".to_string(), |n| n.to_string());
            anyhow::bail!(
                "#{issue} is now `{to}`, but its parent could not be read ({e}) — {by_title} \
                 is stale until `tm issue epic sync {repair}` runs"
            );
        }
    };
    let Some(tracker) = parent else {
        return Ok(None);
    };
    match sync::sync(epic, tracker) {
        Ok(report) => Ok(Some(report)),
        // #8448 AC1: the label has moved; the caller must not read success.
        Err(e) => anyhow::bail!(
            "#{issue} is now `{to}`, but tracker #{tracker}'s phases block was not regenerated \
             ({e}) — it is stale until `tm issue epic sync {tracker}` runs"
        ),
    }
}
