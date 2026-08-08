//! Binding a socket exactly one process at a time may own (#5182).
//!
//! Why: a console-supervised child is spawned, killed, and respawned on demand,
//! and a child that dies without unlinking its socket leaves a file that makes
//! the next `bind` fail with `EADDRINUSE`. [`super::bind_hardened`] deliberately
//! does NOT unlink for it — folding an unconditional unlink in there would let
//! any process silently steal a socket another one is live on. So the
//! probe-then-take-over decision belongs in its own entry point, where the
//! condition it turns on is stated once.
//!
//! What: [`bind_singleton_hardened`] asks whether anything is actually
//! answering. If something is, it refuses rather than clobbering a live owner.
//! If nothing is, the file is a corpse: it is unlinked and rebound through
//! [`super::bind_hardened`], so the replacement is `0600` in a `0700` directory
//! exactly as a first bind would be.
//!
//! The probe is a bare `connect`, not [`super::connect_hardened`], and that is
//! deliberate. The question here is only "is a process serving this path", and
//! a verification failure (wrong mode, wrong owner) does not answer it — a live
//! server with a wrong-mode socket would be misread as a corpse and unlinked
//! out from under itself.
//!
//! `trusty-agents`' `CtrlSocket::bind_singleton` predates this and still carries
//! its own copy; migrating it is a separate change, not a side effect of one
//! that adds two new bind sites.
//!
//! Test: `tests.rs` — `bind_singleton_*`.

use std::path::Path;

use tokio::net::{UnixListener, UnixStream};

use super::{UdsSecurityError, bind_hardened};

/// Bind `path`, taking over a socket file no process is serving.
///
/// # Errors
///
/// [`UdsSecurityError::AlreadyServing`] when another process answers the path —
/// a caller must not proceed, because two listeners on one socket means every
/// delivery goes to whichever the kernel picks. Otherwise any
/// [`super::bind_hardened`] error.
///
/// Test: `bind_singleton_takes_over_a_stale_socket_file`,
/// `bind_singleton_refuses_a_socket_someone_is_serving`,
/// `bind_singleton_binds_a_fresh_path`.
pub async fn bind_singleton_hardened(path: &Path) -> Result<UnixListener, UdsSecurityError> {
    if path.exists() {
        if UnixStream::connect(path).await.is_ok() {
            return Err(UdsSecurityError::AlreadyServing {
                path: path.to_path_buf(),
            });
        }
        // Nothing answered, so the file is left over from a child that died
        // without cleaning up. Removing it is the whole point of this function;
        // a failure to remove it is reported by the bind that follows.
        let _ = std::fs::remove_file(path);
    }
    bind_hardened(path)
}
