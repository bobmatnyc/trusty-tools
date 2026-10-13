//! The daemon's hold on the channel route-policy snapshot (#8433 P1, #8454).
//!
//! Why: every daemon channel path (ask_owner, inbound answers) must read one
//! snapshot, so its 5 s refresh and 60 s max age apply daemon-wide.
//! What: [`DaemonState::channel_policy`] builds the snapshot on first use
//! over `config.yaml` under [`DaemonState::user_home`]. [`ChannelHold`] groups
//! the daemon's channel state (this snapshot, the #8433 P2 question table)
//! behind one `DaemonState` field.
//! Test: `daemon_state_holds_snapshot_under_its_user_home`.

use std::sync::OnceLock;

use crate::channels::snapshot::DaemonPolicySnapshot;

use super::DaemonState;

/// The daemon's channel state, one field on [`DaemonState`].
///
/// Why: #8433 P2: one field for every channel hold keeps `core.rs` under the
/// 500-SLOC cap as holds are added.
/// What: the policy snapshot, built on first use, and the question table.
#[derive(Debug, Default)]
pub(crate) struct ChannelHold {
    policy: OnceLock<DaemonPolicySnapshot>,
    // #8433 P2: the question table, opened on first use.
    pub(super) questions: super::question_table::QuestionTableHold,
}

impl DaemonState {
    /// The daemon's channel route-policy snapshot.
    ///
    /// Why: one snapshot per daemon; a test daemon reads its pinned home,
    /// never the operator's (#8545).
    /// What: built on first call, with no I/O until its first check. A
    /// check never waits on git; refreshes run on their own thread (#9777).
    /// Test: `daemon_state_holds_snapshot_under_its_user_home`.
    pub fn channel_policy(&self) -> &DaemonPolicySnapshot {
        self.channels
            .policy
            .get_or_init(|| DaemonPolicySnapshot::for_home(self.user_home()))
    }
}
