//! Whether the live tmux session a record names is that record's own (#8935).
//!
//! Why: every teardown reached its runtime by tmux NAME, and a name is
//! reusable. A stale record whose session ended long ago still names whatever
//! session took the name since, so stopping or deleting it killed an unrelated
//! live session — on 2026-09-30, the relaunched Architect. The record's
//! `pane_id` is tmux's own `%N` id, which one tmux server never hands out
//! twice: a later session that reuses the name gets a new pane id.
//! What: [`RuntimeOwnership`] and [`runtime_ownership`], the one
//! classification the stop/decommission teardown and the delete guard share,
//! and [`RuntimeTeardown`], what a teardown reports back to its caller.
//! Test: `runtime_identity_tests.rs`.

use super::manager::{ManagedError, ManagedTmuxDriver};
use super::record::SessionRecord;

/// How the live tmux state relates to one record's runtime (#8935).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeOwnership {
    /// No tmux session carries the record's name.
    Absent,
    /// The named session holds the record's own pane, `pane_id`.
    Owned {
        /// The record's `%N` pane id, confirmed present in the session.
        pane_id: String,
    },
    /// A live session carries the name but not the record's pane: another
    /// session took the name. Carries the operator-facing reason.
    Foreign(String),
    /// A live session carries the name and the record's ownership of it
    /// cannot be proved either way. Carries the operator-facing reason.
    Unverifiable(String),
}

impl RuntimeOwnership {
    /// Whether the live session might be this record's runtime.
    ///
    /// What: `true` for `Owned` and `Unverifiable` — the delete guard treats
    /// both as running; `false` for `Absent` and `Foreign`.
    /// Test: `delete_refuses_an_unverifiable_live_name_without_force`.
    pub fn may_be_ours(&self) -> bool {
        matches!(self, Self::Owned { .. } | Self::Unverifiable(_))
    }

    /// The operator-facing note for a record-only action, or `None` when no
    /// live session carries the name.
    ///
    /// Why: a delete never touches tmux, so whenever a session with the
    /// record's name is still up the command output must say it was left
    /// running and whose it is.
    /// Test: `deleting_a_stale_record_leaves_the_live_session_and_says_so`.
    pub fn left_running_note(&self, name: &str) -> Option<String> {
        match self {
            Self::Absent => None,
            Self::Owned { pane_id } => Some(format!(
                "tmux session '{name}' still runs this record's pane {pane_id}; \
                 the record was removed and the session left running"
            )),
            Self::Foreign(why) | Self::Unverifiable(why) => {
                Some(format!("{why}; it was left running"))
            }
        }
    }
}

/// Classify the live tmux state against `record`'s runtime (#8935).
///
/// Why: see the module doc — the name alone cannot say whose session it is.
/// What: `Absent` when no session carries `record.tmux_name`; otherwise
/// `Owned` when the session's pane list holds `record.pane_id`, `Foreign` when
/// it does not, and `Unverifiable` when the record has no pane id or the pane
/// list cannot be read. `Err` only when the session probe itself fails, so
/// each caller picks its own direction for "cannot tell" (#5859).
/// Test: `runtime_identity_tests.rs`.
pub fn runtime_ownership(
    record: &SessionRecord,
    tmux: &dyn ManagedTmuxDriver,
) -> Result<RuntimeOwnership, ManagedError> {
    let name = record.tmux_name.as_str();
    if !tmux.session_exists_checked(name)? {
        return Ok(RuntimeOwnership::Absent);
    }
    let Some(pane_id) = record.pane_id.as_deref() else {
        return Ok(RuntimeOwnership::Unverifiable(format!(
            "tmux session '{name}' is live, and this record carries no pane id \
             to prove the session is its own"
        )));
    };
    Ok(match tmux.pane_exists_checked(name, pane_id) {
        Some(true) => RuntimeOwnership::Owned {
            pane_id: pane_id.to_owned(),
        },
        Some(false) => RuntimeOwnership::Foreign(format!(
            "tmux session '{name}' is live but does not hold this record's pane \
             {pane_id}, so another session now uses the name"
        )),
        None => RuntimeOwnership::Unverifiable(format!(
            "tmux session '{name}' is live, and its panes could not be listed \
             to find this record's pane {pane_id}"
        )),
    })
}

/// What a stop or decommission teardown did to the tmux runtime (#8935).
///
/// #8935 critic round: the two left-running verdicts are separate variants,
/// because a caller that destroys state must tell "another session holds the
/// name" (safe to act record-only) from "ownership could not be proved"
/// (the live session may be this record's, so destroying state is unsafe).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeTeardown {
    /// No tmux session carried the record's name; nothing was signalled.
    Absent,
    /// The record's own pane was signalled and its session reclaimed.
    Terminated,
    /// A live session carries the name and does not hold the record's pane:
    /// another session took the name. Nothing was killed. Carries the reason.
    Foreign(String),
    /// A live session carries the name and the record's ownership of it could
    /// not be proved — no pane id, an unreadable pane list, or a failed probe.
    /// Nothing was killed. Carries the reason.
    Unproven(String),
}

impl RuntimeTeardown {
    /// The operator-facing note when the runtime was left running.
    pub fn left_running(&self) -> Option<&str> {
        match self {
            Self::Foreign(why) | Self::Unproven(why) => Some(why),
            Self::Absent | Self::Terminated => None,
        }
    }
}
