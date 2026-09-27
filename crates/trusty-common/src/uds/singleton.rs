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
//! 🔴 Invariant (#8759): every caller runs its whole lstat → probe → unlink →
//! bind → listen sequence while holding an exclusive, non-blocking `flock` on
//! `<socket>.lock`. Without it, two starters could both prove the same corpse
//! dead and both unlink and bind — the second unlinking the first's fresh
//! socket — or one could probe the other's socket in the gap between `bind`
//! and `listen`, where a connect is refused, and read it as a corpse. Either
//! way both returned `Ok` and both served. Under the lock, a binder sees only a
//! socket another binder finished listening on, so the probe refuses it; a
//! binder that finds the lock held refuses with
//! [`UdsSecurityError::BindInProgress`]; and one that cannot take the lock at
//! all refuses with [`UdsSecurityError::BindLock`] instead of binding unlocked.
//! The lock file is never removed: unlinking it while another process has it
//! open would let a third lock a fresh inode alongside.
//!
//! `trusty-agents`' `CtrlSocket::bind_singleton` predates this and still carries
//! its own copy; migrating it is a separate change, not a side effect of one
//! that adds two new bind sites.
//!
//! Test: `tests.rs` — `bind_singleton_*` and `takeover_verdict_*`.

use std::fs::{File, OpenOptions, TryLockError};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
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
/// every other. `verdict` is `None` when the caller skipped the probe because
/// the path is not a socket — a non-socket occupant costs no `connect`, since
/// the errno one would return is the very thing that misread it.
/// Test: `takeover_verdict_refuses_a_non_socket_even_when_the_probe_says_dead`,
/// `takeover_verdict_refuses_a_non_socket_that_was_never_probed`,
/// `takeover_verdict_takes_over_a_dead_socket`,
/// `takeover_verdict_refuses_a_served_socket`,
/// `takeover_verdict_refuses_an_inconclusive_probe`.
pub(crate) fn classify_takeover(
    is_socket: bool,
    verdict: Option<SocketVerdict>,
) -> TakeoverVerdict {
    // #7312: bind-failure test hung the CI shard — a Linux connect to a regular
    // file answers ECONNREFUSED, so the probe alone reads it as a dead socket.
    if !is_socket {
        return TakeoverVerdict::NotASocket;
    }
    match verdict {
        Some(SocketVerdict::NotServing) => TakeoverVerdict::TakeOver,
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
/// deletes (#7312). [`UdsSecurityError::BindInProgress`] when another process
/// holds the bind lock, and [`UdsSecurityError::BindLock`] when the lock cannot
/// be taken at all (#8759). Otherwise any [`super::bind_hardened`] error.
///
/// Test: `bind_singleton_takes_over_a_stale_socket_file`,
/// `bind_singleton_racing_takeover_leaves_exactly_one_owner`,
/// `bind_singleton_refuses_while_another_binder_holds_the_lock`,
/// `bind_singleton_fails_closed_when_the_bind_lock_cannot_be_opened`,
/// `bind_singleton_refuses_a_socket_someone_is_serving`,
/// `bind_singleton_refuses_a_regular_file_and_leaves_it_on_disk`,
/// `bind_singleton_hardened_refuses_a_symlink_to_a_dead_socket`,
/// `bind_singleton_hardened_refuses_a_symlink_to_a_live_socket`,
/// `bind_singleton_binds_a_fresh_path`.
pub async fn bind_singleton_hardened(path: &Path) -> Result<UnixListener, UdsSecurityError> {
    bind_singleton_with(path, || async {}).await
}

/// [`bind_singleton_hardened`]'s body, with a hook run after the takeover
/// decision and before the unlink.
///
/// Why: the #8759 race lives between "the probe proved this socket dead" and
/// "unlink it and bind"; a test forces a second binder into exactly that gap
/// by running it inside `before_takeover`, with no sleep standing in for the
/// interleaving.
/// What: identical to [`bind_singleton_hardened`]; `before_takeover` runs only
/// on the [`TakeoverVerdict::TakeOver`] arm.
/// Test: `bind_singleton_racing_takeover_leaves_exactly_one_owner`.
pub(crate) async fn bind_singleton_with<F, Fut>(
    path: &Path,
    before_takeover: F,
) -> Result<UnixListener, UdsSecurityError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    // #8759: the lock lives beside the socket, so the directory is hardened
    // first; the guard is held to the end of this function, past `listen`.
    super::check_sun_path_budget(path)?;
    super::prepare_socket_dir(super::socket_parent(path)?)?;
    let _bind_lock = lock_for_bind(path)?;

    // #7312: `lstat`, not `Path::exists` — the latter follows a symlink and
    // answers only "is something there", which is one of the two inputs the
    // decision below needs. A stat that fails for any other reason is left to
    // `bind_hardened` to report, exactly as `exists()` returning false was.
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        let file_type = meta.file_type();
        let is_socket = std::os::unix::fs::FileTypeExt::is_socket(&file_type);
        // #5182 review: three-state, not `is_ok()`. On macOS a live listener
        // with a saturated accept queue answers ECONNREFUSED, and a probe that
        // simply times out proves nothing — see `uds::probe::SocketVerdict`.
        // Only a verdict the kernel proved licenses an unlink.
        //
        // #7312: a non-socket occupant is refused on its type alone, so it
        // never reaches this connect. The decision below still refuses it on
        // `is_socket` whatever a verdict says, so skipping the probe is a cost
        // saving, not the rule.
        let verdict = if is_socket {
            Some(probe_socket_verdict(path, PROBE_TIMEOUT).await)
        } else {
            None
        };
        match classify_takeover(is_socket, verdict) {
            TakeoverVerdict::TakeOver => {
                before_takeover().await;
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

/// Take the exclusive, non-blocking bind lock beside `path`.
///
/// Why: #8759 — see the invariant in the module doc.
/// What: opens `<path>.lock` (created `0600`, never truncated, never removed)
/// and `try_lock`s it. Contention is [`UdsSecurityError::BindInProgress`];
/// any I/O failure is [`UdsSecurityError::BindLock`]. The returned file holds
/// the lock until dropped.
/// Test: `bind_singleton_refuses_while_another_binder_holds_the_lock`,
/// `bind_singleton_fails_closed_when_the_bind_lock_cannot_be_opened`.
fn lock_for_bind(path: &Path) -> Result<File, UdsSecurityError> {
    let lock_path = bind_lock_path(path);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path);
    let file = match file {
        Ok(file) => file,
        Err(source) => {
            return Err(UdsSecurityError::BindLock {
                path: lock_path,
                source,
            });
        }
    };
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(UdsSecurityError::BindInProgress {
            path: path.to_path_buf(),
        }),
        Err(TryLockError::Error(source)) => Err(UdsSecurityError::BindLock {
            path: lock_path,
            source,
        }),
    }
}

/// `<path>.lock`: the bind lock's file, beside the socket it guards.
pub(crate) fn bind_lock_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}
