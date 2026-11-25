//! Same-user peer authentication for every broker socket.
//!
//! Why: the sockets are 0600 in a 0700 directory, and each accepted
//! connection is still checked against the kernel's record of who connected
//! (spike `broker.py:46-52`, `194-198`). A file-mode slip must not open the
//! broker to another local user.
//! What: [`peer_uid`] (from `trusty-daemon-common`) reads the kernel
//! credential; [`admit`] refuses an unreadable credential and any uid but ours.
//! Test: `same_user_socketpair_passes_peer_auth`, `foreign_or_unreadable_peer_is_refused`
//! in `tests/broker_server.rs`.

use std::io;
use std::os::fd::AsRawFd;

// #11692: the credential read lives in trusty-daemon-common.
pub use trusty_daemon_common::peer::peer_uid;

use crate::private;

/// Why a peer was refused; `code` is the reply and audit code.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {detail}")]
pub struct PeerRefused {
    /// `foreign_uid` or `peer_unverified`.
    pub code: &'static str,
    /// Human-readable detail.
    pub detail: String,
}

/// Admit a peer whose credential read gave `peer` when this process is `me`.
///
/// Why: split from the socket so a foreign uid is testable without root.
/// What: an unreadable credential is `peer_unverified`, another uid is
/// `foreign_uid`; only `Ok(me)` passes. Both refusals fail closed.
/// Test: `foreign_or_unreadable_peer_is_refused`.
pub fn admit(peer: io::Result<u32>, me: u32) -> Result<u32, PeerRefused> {
    match peer {
        Ok(uid) if uid == me => Ok(uid),
        Ok(uid) => Err(PeerRefused {
            code: "foreign_uid",
            detail: format!("peer uid {uid} is not {me}"),
        }),
        Err(e) => Err(PeerRefused {
            code: "peer_unverified",
            detail: e.to_string(),
        }),
    }
}

/// [`admit`] for a connected socket against this process's uid.
pub fn verify_peer(sock: &impl AsRawFd) -> Result<u32, PeerRefused> {
    admit(peer_uid(sock), private::uid())
}
