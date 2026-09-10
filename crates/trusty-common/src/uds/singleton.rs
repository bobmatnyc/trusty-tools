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
//! 🔴 The takeover fires ONLY on [`SocketVerdict::NotServing`], the verdict the
//! kernel actually proves (ENOENT / ECONNREFUSED), **and only when the path is
//! a socket to begin with**. An ambiguous probe —
//! [`SocketVerdict::Inconclusive`] — is treated as a live owner and refused.
//! The asymmetry is the point: refusing a dead socket costs one failed start
//! that a retry fixes, while unlinking a live one strands its owner on an
//! inode nothing can reach.
//!
//! 🔴 The file-type half of that rule is load-bearing and was missing until
//! #7312. Linux and macOS disagree about which errno a connect to a NON-socket
//! returns: macOS/BSD `unp_connect` answers `ENOTSOCK`, which classifies as
//! `Inconclusive` and refuses, while Linux's `unix_find_other` sets
//! `-ECONNREFUSED` before its `S_ISSOCK` test, which classifies as
//! `NotServing` and licensed an unlink. So on Linux a daemon pointed at a path
//! holding an ordinary file deleted that file and bound over it. The probe
//! cannot be made to answer this — `lstat` can, and does, first.
//! [`super::verify_socket_for_connect`] has always applied that rule on the
//! dialing side; this is its missing half on the binding side.
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
//! Test: `tests.rs` — `bind_singleton_*` and `takeover_verdict_*`.

use std::path::Path;
use std::time::Duration;

use tokio::net::UnixListener;

use super::probe::{SocketVerdict, probe_socket_verdict};
use super::{UdsSecurityError, bind_hardened};

/// How long the takeover probe waits for an answer.
///
/// A local socket answers or refuses in microseconds, so a full second is not a
/// latency budget — it is headroom, so that `Inconclusive` means "the kernel
/// would not answer" rather than "this machine was busy". The supervisor's
/// 200 ms liveness probe runs per request and cannot afford that; this runs
/// once per process start and can. A starved 200 ms probe here read as
/// `Inconclusive` and refused a genuinely stale socket, which is safe but
/// wrong.
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// What [`bind_singleton_hardened`] must do about a path that already exists.
///
/// Why a named decision rather than an inline `match`: the two refusal arms are
/// awkward to reach through real syscalls — `Inconclusive` needs a socket whose
/// connect neither completes nor is refused, and `NotASocket` needs a kernel
/// that reports the file type through the errno, which macOS does and Linux
/// does not. Separating the decision from the syscalls makes every arm testable
/// unprivileged on either platform, the same reason
/// [`super::dir::DirVerdict`] exists.
///
/// Test: the `takeover_verdict_*` tests in `tests.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TakeoverVerdict {
    /// A socket corpse: unlink it and rebind.
    TakeOver,
    /// A socket that is, or may be, serving: refuse and leave it alone.
    Occupied,
    /// Not a socket at all: refuse, and never unlink it.
    NotASocket,
}

/// Decide whether an existing path may be unlinked and rebound.
///
/// Why: see [`TakeoverVerdict`]. The file type is checked FIRST and outranks
/// the probe, because on Linux the probe's answer for a non-socket is
/// indistinguishable from its answer for a dead socket (#7312).
/// What: `NotASocket` whenever `is_socket` is false, whatever the probe said;
/// otherwise `TakeOver` on the one verdict the kernel proved, `Occupied` on
/// every other.
/// Test: `takeover_verdict_refuses_a_non_socket_even_when_the_probe_says_dead`,
/// `takeover_verdict_takes_over_a_dead_socket`,
/// `takeover_verdict_refuses_a_served_socket`,
/// `takeover_verdict_refuses_an_inconclusive_probe`.
pub(crate) fn classify_takeover(is_socket: bool, verdict: SocketVerdict) -> TakeoverVerdict {
    // #7312: bind-failure test hung the CI shard — a Linux connect to a regular
    // file answers ECONNREFUSED, so the probe alone reads it as a dead socket.
    if !is_socket {
        return TakeoverVerdict::NotASocket;
    }
    match verdict {
        SocketVerdict::NotServing => TakeoverVerdict::TakeOver,
        _ => TakeoverVerdict::Occupied,
    }
}

/// Bind `path`, taking over a socket file no process is serving.
///
/// # Errors
///
/// [`UdsSecurityError::AlreadyServing`] when another process answers the path,
/// or when the probe cannot settle the question — a caller must not proceed,
/// because two listeners on one socket means every delivery goes to whichever
/// the kernel picks. [`UdsSecurityError::NotASocketFile`] when the path holds
/// something that is not a socket, which this function refuses rather than
/// deletes (#7312). Otherwise any [`super::bind_hardened`] error.
///
/// Test: `bind_singleton_takes_over_a_stale_socket_file`,
/// `bind_singleton_refuses_a_socket_someone_is_serving`,
/// `bind_singleton_refuses_a_regular_file_and_leaves_it_on_disk`,
/// `bind_singleton_binds_a_fresh_path`.
pub async fn bind_singleton_hardened(path: &Path) -> Result<UnixListener, UdsSecurityError> {
    // #7312: `lstat`, not `Path::exists` — the latter follows a symlink and
    // answers only "is something there", which is one of the two inputs the
    // decision below needs. A stat that fails for any other reason is left to
    // `bind_hardened` to report, exactly as `exists()` returning false was.
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        let file_type = meta.file_type();
        // #5182 review: three-state, not `is_ok()`. On macOS a live listener
        // with a saturated accept queue answers ECONNREFUSED, and a probe that
        // simply times out proves nothing — see `uds::probe::SocketVerdict`.
        // Only a verdict the kernel proved licenses an unlink.
        let verdict = probe_socket_verdict(path, PROBE_TIMEOUT).await;
        match classify_takeover(
            std::os::unix::fs::FileTypeExt::is_socket(&file_type),
            verdict,
        ) {
            TakeoverVerdict::TakeOver => {
                // A corpse from a child that died without cleaning up. Removing
                // it is the whole point of this function; a failure to remove it
                // is reported by the bind that follows.
                let _ = std::fs::remove_file(path);
            }
            TakeoverVerdict::NotASocket => {
                let found = super::dir::describe_file_type(&file_type);
                tracing::debug!(
                    socket = %path.display(),
                    found,
                    "refusing to remove a non-socket sitting on the socket path"
                );
                return Err(UdsSecurityError::NotASocketFile {
                    path: path.to_path_buf(),
                    found: found.to_string(),
                });
            }
            TakeoverVerdict::Occupied => {
                tracing::debug!(
                    socket = %path.display(),
                    ?verdict,
                    "refusing to take over a socket that is not provably unserved"
                );
                return Err(UdsSecurityError::AlreadyServing {
                    path: path.to_path_buf(),
                });
            }
        }
    }
    bind_hardened(path)
}
