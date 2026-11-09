//! `tm presence` — prove the operator is at the Studio (#8878 PR2a).
//!
//! Why: a trust-anchor write by the Architect needs Bob's presence, and the
//! pm-guard hook cannot show a prompt (plan-pr2-devices.md §1).
//! What: [`PresenceAction`] — `local` (Touch ID or the login password).
//! Device approvals are #8878 PR2b.
//! Test: `cli_parses_presence_local`.

use clap::Subcommand;

/// Actions for the `presence` subcommand.
#[derive(Debug, Subcommand)]
pub(crate) enum PresenceAction {
    /// Approve the Architect's trust-anchor writes with Touch ID or the
    /// login password at this Mac.
    ///
    /// Run from the bound Architect session. Writes a sealed grant for this
    /// session's `claude` that lasts while it is used at least every 120
    /// minutes. Refuses, writing nothing, for any other session, over SSH,
    /// with no GUI session, or when the prompt is cancelled.
    Local,
}
