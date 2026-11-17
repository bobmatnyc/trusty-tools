//! `tm presence` — prove the operator is at the Studio (#8878 PR2a, PR2b-1b).
//!
//! Why: a trust-anchor write by the Architect needs Bob's presence, and the
//! pm-guard hook cannot show a prompt (plan-pr2-devices.md §1).
//! What: [`PresenceAction`] — `local` (Touch ID or the login password),
//! `request` and `wait` (ask an enrolled device, PR2b-1b) and `devices`.
//! Test: `cli_parses_presence_local`, `cli_parses_presence_device_verbs_8878`.

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
    /// Ask an enrolled device to approve this session's next trust-anchor
    /// write. Asks only; grants nothing. The action text is shown to the
    /// approver labelled "unverified origin". Approving needs the console
    /// page (#8878 PR2b-2).
    Request {
        /// What the call wants to do, as shown to the approver.
        #[arg(long, default_value = "trust-anchor write by the Architect")]
        action: String,
    },
    /// Wait until this session holds a valid presence grant.
    ///
    /// Exit 0 granted; 75 still pending at the timeout (re-issue the same
    /// command); 1 the named request expired or is gone with no grant.
    Wait {
        /// How long to wait: `300`, `300s` or `5m`.
        #[arg(long, default_value = "5m")]
        timeout: String,
        /// The request id `tm presence request` printed.
        #[arg(long)]
        request: Option<String>,
    },
    /// Enrolled presence devices (passkeys).
    Devices {
        /// Device action to perform.
        #[command(subcommand)]
        action: DevicesAction,
    },
}

/// Actions for `tm presence devices`.
#[derive(Debug, Subcommand)]
pub(crate) enum DevicesAction {
    /// Start enrolling a passkey device (INCOMPLETE until #8878 PR2b-2: no
    /// browser path finishes the ceremony yet).
    ///
    /// Needs Touch ID or the login password at the Studio first; prints the
    /// registration options a browser would answer.
    Add {
        /// A name for the device, e.g. `iphone`.
        name: String,
    },
    /// List enrolled devices.
    List,
    /// Revoke a device: removes its record and every grant it approved.
    /// Needs Touch ID or the login password at the Studio first.
    Revoke {
        /// The credential id `tm presence devices list` shows.
        credential_id: String,
    },
}
