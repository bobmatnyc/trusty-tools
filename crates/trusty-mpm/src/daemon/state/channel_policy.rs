//! The daemon's hold on the channel route-policy snapshot (#8433 P1, #8454).
//!
//! Why: every daemon channel path (ask_owner, inbound answers) must read one
//! snapshot, so its 5 s refresh and 60 s max age apply daemon-wide.
//! What: [`DaemonState::channel_policy`] builds the snapshot on first use
//! over `config.yaml` under [`DaemonState::user_home`].
//! Test: `daemon_state_holds_snapshot_under_its_user_home`.

use crate::channels::snapshot::DaemonPolicySnapshot;

use super::DaemonState;

impl DaemonState {
    /// The daemon's channel route-policy snapshot.
    ///
    /// Why: one snapshot per daemon; a test daemon reads its pinned home,
    /// never the operator's (#8545).
    /// What: built on first call, with no I/O until its first check. Its
    /// checks may run git; call them from a blocking context.
    /// Test: `daemon_state_holds_snapshot_under_its_user_home`.
    pub fn channel_policy(&self) -> &DaemonPolicySnapshot {
        self.channel_policy
            .get_or_init(|| DaemonPolicySnapshot::for_home(self.user_home()))
    }
}
