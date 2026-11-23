//! Same-user peer authentication for every broker socket.
//!
//! Why: the sockets are mode 0600 in a 0700 directory, and each accepted
//! connection is still checked against the kernel's record of who connected
//! (`protocol.py` `verify_peer`). A file-mode slip must not open the broker
//! to another local user.
//! What: [`peer_uid`] (from `trusty-daemon-common`) reads `LOCAL_PEERCRED`
//! on Darwin and `SO_PEERCRED` on Linux; any other platform answers
//! `UnsupportedPeerCredentials`. The peer uid must equal this process's uid.
//! Test: `same_user_socketpair_passes_peer_auth` (below), and every replay in
//! `tests/parity_broker.rs` runs through it.

use std::os::fd::AsRawFd;

use crate::private;
use crate::refusal::Refusal;

// #11692: the credential read lives in trusty-daemon-common.
pub use trusty_daemon_common::peer::peer_uid;

/// `verify_peer`: refuse unless the peer is this user.
///
/// Why: the broker's first check on every connection, before any byte is read.
/// What: `UnsupportedPeerCredentials` off Darwin/Linux; `Unauthorized` when the
/// credential cannot be read or names another uid. The caller adds
/// `reason: peer_uid` and the audit line.
/// Test: `same_user_socketpair_passes_peer_auth`, `foreign_uid_is_refused`.
pub fn verify_peer(sock: &impl AsRawFd) -> Result<(), Refusal> {
    if cfg!(not(any(target_os = "macos", target_os = "linux"))) {
        return Err(Refusal::new("UnsupportedPeerCredentials"));
    }
    match peer_uid(sock) {
        Ok(uid) => uid_matches(uid, private::uid()),
        Err(_) => Err(Refusal::new("Unauthorized")),
    }
}

/// The comparison itself, split out so a foreign uid can be tested without root.
pub fn uid_matches(peer: u32, me: u32) -> Result<(), Refusal> {
    if peer == me {
        Ok(())
    } else {
        Err(Refusal::new("Unauthorized"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    #[test]
    fn same_user_socketpair_passes_peer_auth() {
        let (a, b) = UnixStream::pair().expect("socketpair");
        assert_eq!(peer_uid(&a).ok(), Some(private::uid()));
        assert!(verify_peer(&b).is_ok());
    }

    #[test]
    fn foreign_uid_is_refused() {
        let me = private::uid();
        let err = uid_matches(me.wrapping_add(1), me).err();
        assert_eq!(err.map(|r| r.code).as_deref(), Some("Unauthorized"));
        assert!(uid_matches(me, me).is_ok());
    }
}
