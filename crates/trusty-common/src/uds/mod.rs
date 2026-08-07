//! Permission enforcement for Unix-domain sockets — the `0600` guarantee
//! ADR-0031 and ADR-0032 cite as an existing property.
//!
//! Why: both ADRs rest their case for UDS-over-loopback-TCP on "a loopback port
//! is reachable by any local process, a `0600` socket is not". Until #5099 no
//! production code in this workspace called `set_permissions` on any socket —
//! every hit was a test fixture. Sockets were created at the process umask
//! (commonly `0755`) and two of the three path conventions placed them in
//! `$TMPDIR` falling back to a shared, world-writable `/tmp`. This module makes
//! the claimed guarantee real, in one place, so a behavior fix lands once
//! rather than at four bind sites. ADR-0034 §3 ("The trust boundary") states
//! the requirement: `0700` directory, `0600` socket, peer-uid check on accept.
//!
//! What: four primitives, in the order a daemon uses them.
//!   - [`scratch_socket_dir`] resolves a per-uid directory under the system
//!     scratch space, replacing the bare `$TMPDIR`-with-`/tmp`-fallback.
//!   - [`bind_hardened`] creates that directory at `0700`, binds, and sets the
//!     socket to `0600` before returning it to the caller.
//!   - [`connect_hardened`] is the dialer's half: it verifies the directory and
//!     socket a client is about to trust before writing anything to it.
//!   - [`ensure_peer_is_self`] refuses an accepted connection whose peer uid is
//!     not this process's own, which is what turns the permission bits into an
//!     enforced boundary rather than a documented intention.
//!
//! **What this does and does not guarantee.** With the directory held at `0700`
//! and owned by this uid, no other unprivileged user can traverse to the socket
//! — that is the load-bearing property, and it holds from the moment the
//! directory is created. It is *not* an unconditional claim about every path
//! this module can be handed: a caller that points it at a directory an
//! attacker can rename or unlink entries in retains a residual swap race that
//! only `openat`/`fchmod` on a directory fd could close. Symlink pre-creation,
//! which is the practical version of that attack, is refused outright, as is a
//! non-directory sitting at the socket-directory path — see
//! [`dir::prepare_socket_dir`]. Root is not defended against and cannot be.
//!
//! Deliberately not a transport: there is no framing or JSON-RPC here. #5089
//! step 1 builds the shared UDS transport module; this is the security layer
//! that module mounts, including the dialer it needs, not a competing
//! implementation of it.
//!
//! Test: `tests.rs` — directory and socket modes after a real bind, the
//! pre-existing-wide-directory repair, symlink refusal, the pure decision
//! functions behind every refusal, and the `sun_path` budget pre-check.

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

pub mod dir;
mod peer;

pub use dir::prepare_socket_dir;
pub use peer::{ensure_peer_is_self, peer_uid, self_uid};

use std::fs::Permissions;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use tokio::net::{UnixListener, UnixStream};

/// Mode every socket-bearing directory is created at and verified against.
///
/// Owner-only `rwx`. Unix path resolution needs search (`x`) permission on
/// every directory component, so a `0700` directory makes every socket inside
/// it unreachable to any other uid regardless of the socket's own mode.
pub const SOCKET_DIR_MODE: u32 = 0o700;

/// Mode every bound socket is set to before its first `accept`.
pub const SOCKET_MODE: u32 = 0o600;

/// Failures that mean a socket could not be made private, or could not be
/// trusted. Every variant is fatal to the operation — none is a "log and
/// continue" condition, because continuing would use a socket wider than the
/// ADRs promise.
///
/// `#[non_exhaustive]`: this enum gained two variants across two review rounds
/// of #5099 alone, so it will keep growing as the checks tighten. The attribute
/// costs nothing while `trusty-common` sits unpublished at 0.30.0 against a
/// published 0.28.1, and stops being free the moment 0.30.0 ships — after which
/// each new variant would be a breaking change. On an enum it constrains
/// *matching* only (external crates need a wildcard arm); it does not bar
/// constructing variants, which is the separate effect the attribute has on a
/// struct. Matching this exhaustively from outside `trusty-common` was never
/// possible anyway — every consumer converts it (`io::Error::other`, `?` into
/// `anyhow`, or `Display` in a log) rather than inspecting variants.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UdsSecurityError {
    /// The socket path is a bare filename with no directory component, so
    /// there is nothing to harden.
    #[error("socket path {path} has no parent directory to harden")]
    NoParent {
        /// The offending socket path.
        path: PathBuf,
    },

    /// The socket path does not fit the kernel's `sockaddr_un.sun_path` buffer.
    #[error(
        "socket path is {len} bytes but the kernel's sun_path buffer holds {capacity} \
         (including its NUL terminator): {path}"
    )]
    PathTooLong {
        /// The path that overflowed.
        path: PathBuf,
        /// Length of `path` in bytes.
        len: usize,
        /// Bytes available in `sockaddr_un.sun_path` on this platform.
        capacity: usize,
    },

    /// Creating the socket directory failed.
    #[error("create socket directory {path}: {source}")]
    CreateDir {
        /// Directory that could not be created.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// The socket directory is a symlink. Refused outright: `metadata` and
    /// `set_permissions` follow links, so verifying a symlinked directory reads
    /// and chmods the wrong inode (#5099 review finding 1).
    #[error("socket directory {path} is a symlink; refusing to trust it")]
    SymlinkDir {
        /// The symlinked path.
        path: PathBuf,
    },

    /// The socket-directory path exists but is not a directory. Refused before
    /// anything chmods it — a regular file here used to be narrowed to `0700`
    /// on its way to a confusing `ENOTDIR` from `bind` (#5099 review round 2).
    #[error("socket directory {path} is a {found}, not a directory")]
    NotADirectory {
        /// The offending path.
        path: PathBuf,
        /// What `lstat` actually found there.
        found: String,
    },

    /// The directory already existed but belongs to a different uid. Repairing
    /// its mode would not make it safe — another user controls its contents —
    /// so this fails closed instead.
    #[error("socket directory {path} is owned by uid {owner}, not {expected}")]
    ForeignDirOwner {
        /// Directory whose ownership was rejected.
        path: PathBuf,
        /// uid that actually owns it.
        owner: u32,
        /// uid this process runs as.
        expected: u32,
    },

    /// The directory existed at a wider mode and could not be narrowed.
    #[error("narrow socket directory {path} to {mode:04o}: {source}")]
    HardenDir {
        /// Directory that could not be narrowed.
        path: PathBuf,
        /// Mode that was being applied.
        mode: u32,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// `UnixListener::bind` failed.
    #[error("bind unix socket at {path}: {source}")]
    Bind {
        /// Socket path that could not be bound.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// The socket bound but could not be narrowed to [`SOCKET_MODE`].
    #[error("narrow socket {path} to {mode:04o}: {source}")]
    Chmod {
        /// Socket that could not be narrowed.
        path: PathBuf,
        /// Mode that was being applied.
        mode: u32,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// A client refused to dial a socket that failed verification.
    #[error("refusing to connect to {path}: {reason}")]
    UntrustedSocket {
        /// The socket that was refused.
        path: PathBuf,
        /// Which property failed.
        reason: String,
    },

    /// Could not `lstat` a path while verifying it ahead of a connect. Distinct
    /// from [`UdsSecurityError::CreateDir`]: a dialer creates nothing, so
    /// reporting a creation failure there named an action never attempted
    /// (#5099 review round 2, finding 2).
    #[error("stat {path} while verifying it for connect: {source}")]
    StatForConnect {
        /// Path that could not be stat'd.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// `UnixStream::connect` failed.
    #[error("connect to unix socket at {path}: {source}")]
    Connect {
        /// Socket path that could not be dialled.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// Reading the connected peer's credentials failed.
    #[error("read peer credentials: {source}")]
    PeerCred {
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// The connected peer runs as a different uid.
    #[error("refused connection from uid {peer}; this process runs as uid {expected}")]
    ForeignPeer {
        /// uid on the other end of the connection.
        peer: u32,
        /// uid this process runs as.
        expected: u32,
    },

    /// No peer-credential syscall is wired up for this target.
    #[error("peer-credential checks are not implemented for this platform")]
    UnsupportedPlatform,
}

/// Bytes available in this platform's `sockaddr_un.sun_path`, NUL included.
///
/// Why: 104 on macOS, 108 on Linux. Rust rejects an over-long path before the
/// syscall with a bare `invalid argument` that names neither the limit nor the
/// offending path, which is a poor diagnostic for a value assembled from
/// `$TMPDIR` plus a palace name (#5099 review finding 4).
/// What: derived from the actual struct layout rather than hardcoded.
/// Test: `sun_path_capacity_is_platform_plausible`.
pub fn sun_path_capacity() -> usize {
    size_of::<libc::sockaddr_un>() - std::mem::offset_of!(libc::sockaddr_un, sun_path)
}

/// Reject a socket path that cannot fit the kernel's address buffer.
///
/// Why: turns `invalid argument` into a message naming the budget, the actual
/// length, and the path — the difference between "the daemon is broken" and
/// "this palace name is 12 characters too long".
/// What: compares the path's byte length against [`sun_path_capacity`], leaving
/// room for the NUL terminator.
/// Test: `check_sun_path_budget_accepts_a_path_that_fits` and
/// `check_sun_path_budget_rejects_an_over_long_path`.
pub fn check_sun_path_budget(path: &Path) -> Result<(), UdsSecurityError> {
    let len = path.as_os_str().as_encoded_bytes().len();
    let capacity = sun_path_capacity();
    if len >= capacity {
        return Err(UdsSecurityError::PathTooLong {
            path: path.to_path_buf(),
            len,
            capacity,
        });
    }
    Ok(())
}

/// Per-uid directory under the system scratch space for daemon sockets.
///
/// Why: the convention this replaces was `$TMPDIR`, falling back to `/tmp`. On
/// macOS `$TMPDIR` is a per-user `/var/folders/…/T/` that is already `0700`, so
/// the fallback never bit there. On a Linux host with `TMPDIR` unset it
/// resolved to `/tmp` — mode `1777`, owned by root — which can be neither
/// narrowed to `0700` nor trusted, so any socket in it was reachable by every
/// local user. Interposing a uid-keyed subdirectory gives a directory this
/// process owns and can hold at `0700` on both platforms uniformly.
///
/// What: `<$TMPDIR or /tmp>/trusty-<uid>`. The name is kept short on purpose:
/// `sun_path` is 104 bytes on macOS and macOS' `$TMPDIR` already consumes
/// roughly half of that, so every byte here comes out of the budget available
/// to palace names (see `trusty-memory`'s `bm25_supervisor_concurrency` tests).
///
/// Test: `scratch_socket_dir_is_uid_keyed`; the `$TMPDIR` resolution rules are
/// asserted against [`scratch_socket_dir_from`] so no test has to mutate the
/// process-global `TMPDIR` (which reddens unrelated sibling tests — see the
/// `trusty-mpm` `tmpdir-cross-test-pollution` fragment).
pub fn scratch_socket_dir() -> PathBuf {
    scratch_socket_dir_from(std::env::var("TMPDIR").ok().as_deref(), self_uid())
}

/// [`scratch_socket_dir`] with its two environment inputs passed explicitly.
///
/// Why: keeps the resolution rules unit-testable without `set_var`. A test that
/// mutates `TMPDIR` is visible to every concurrently-running sibling in the
/// same test binary, and `tempfile` honors it.
/// What: treats an absent, empty, or whitespace-only `tmpdir` as `/tmp`.
/// Test: `scratch_socket_dir_from_uses_tmpdir_when_set`,
/// `scratch_socket_dir_from_falls_back_to_tmp`.
pub fn scratch_socket_dir_from(tmpdir: Option<&str>, uid: u32) -> PathBuf {
    let base = match tmpdir {
        Some(p) if !p.trim().is_empty() => PathBuf::from(p),
        _ => PathBuf::from("/tmp"),
    };
    base.join(format!("trusty-{uid}"))
}

/// Resolve a socket path's parent, rejecting a bare filename.
///
/// `Path::parent` yields `Some("")` — not `None` — for a bare filename, so the
/// empty case has to be filtered explicitly or a relative socket name would be
/// treated as living in an unhardened cwd.
fn socket_parent(path: &Path) -> Result<&Path, UdsSecurityError> {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| UdsSecurityError::NoParent {
            path: path.to_path_buf(),
        })
}

/// Bind a listener at `path` with its directory at `0700` and the socket at
/// `0600`.
///
/// Why: the single entry point every daemon binds through, so the permission
/// contract cannot drift between the four bind sites that previously each
/// called `UnixListener::bind` bare (`trusty-embedderd`, `trusty-bm25-daemon`,
/// and two in `trusty-agents`). See #5099.
///
/// What: checks the `sun_path` budget, hardens the parent directory via
/// [`prepare_socket_dir`], binds, then narrows the socket to [`SOCKET_MODE`]
/// before returning — so the listener is already `0600` when the caller first
/// calls `accept`. Deliberately does *not* remove a stale socket file:
/// `CtrlSocket::bind_singleton` must probe before clobbering, and folding an
/// unconditional unlink in here would break that singleton guarantee. Callers
/// that want stale-file cleanup keep doing it themselves, immediately before
/// this call.
///
/// The `0600` step is defence in depth, not the race fix — the `0700` directory
/// is what makes the socket unreachable during the window between `bind` and
/// `chmod`. [`prepare_socket_dir`]'s docs carry the reasoning and the residual
/// race it does not close.
///
/// Test: `bind_hardened_sets_socket_0600_and_dir_0700`,
/// `bind_hardened_socket_is_connectable_after_hardening`,
/// `bind_hardened_rejects_an_over_long_path`.
pub fn bind_hardened(path: &Path) -> Result<UnixListener, UdsSecurityError> {
    check_sun_path_budget(path)?;
    prepare_socket_dir(socket_parent(path)?)?;

    let listener = UnixListener::bind(path).map_err(|source| UdsSecurityError::Bind {
        path: path.to_path_buf(),
        source,
    })?;

    std::fs::set_permissions(path, Permissions::from_mode(SOCKET_MODE)).map_err(|source| {
        UdsSecurityError::Chmod {
            path: path.to_path_buf(),
            mode: SOCKET_MODE,
            source,
        }
    })?;

    Ok(listener)
}

/// Verify a socket and its directory, then connect.
///
/// Why: hardening only the bind side leaves the client trusting whatever is at
/// the path. A daemon that predates this change, or a socket an attacker
/// planted, still answers — and the supervisor's adopt-an-existing-socket path
/// means the daemon's own `ForeignDirOwner` check may never run, because it
/// never spawns when something is already listening (#5099 review finding 3).
/// The dialer has to do its own verification.
///
/// What: refuses unless the parent directory is a non-symlink `0700` directory
/// owned by this uid, and the socket itself is a non-symlink socket owned by
/// this uid at mode `0600`. Then connects.
///
/// This is a check-then-use, so a sufficiently privileged attacker could swap
/// the target between the `lstat` and the `connect`. It is not trying to win
/// that race — it is closing the case that actually occurs, where a wrong-mode
/// or wrong-owner socket persists and would otherwise be dialled silently.
///
/// Test: `connect_hardened_accepts_a_properly_hardened_socket`,
/// `connect_hardened_refuses_a_world_readable_socket`,
/// `connect_hardened_refuses_a_socket_in_a_wide_directory`,
/// `connect_hardened_refuses_a_regular_file`.
pub async fn connect_hardened(path: &Path) -> Result<UnixStream, UdsSecurityError> {
    check_sun_path_budget(path)?;
    verify_socket_for_connect(path)?;

    UnixStream::connect(path)
        .await
        .map_err(|source| UdsSecurityError::Connect {
            path: path.to_path_buf(),
            source,
        })
}

/// The filesystem half of [`connect_hardened`], split out so it is testable
/// without standing up a listener.
///
/// Test: the `connect_hardened_*` tests plus `verify_socket_for_connect_*`.
pub fn verify_socket_for_connect(path: &Path) -> Result<(), UdsSecurityError> {
    let own = self_uid();
    let refuse = |reason: String| UdsSecurityError::UntrustedSocket {
        path: path.to_path_buf(),
        reason,
    };

    let parent = socket_parent(path)?;
    let dmeta =
        std::fs::symlink_metadata(parent).map_err(|source| UdsSecurityError::StatForConnect {
            path: parent.to_path_buf(),
            source,
        })?;
    if dmeta.file_type().is_symlink() {
        return Err(UdsSecurityError::SymlinkDir {
            path: parent.to_path_buf(),
        });
    }
    if !dmeta.file_type().is_dir() {
        return Err(UdsSecurityError::NotADirectory {
            path: parent.to_path_buf(),
            found: dir::describe_file_type(&dmeta.file_type()).to_string(),
        });
    }
    if dmeta.uid() != own {
        return Err(UdsSecurityError::ForeignDirOwner {
            path: parent.to_path_buf(),
            owner: dmeta.uid(),
            expected: own,
        });
    }
    let dmode = dmeta.permissions().mode() & 0o777;
    if dmode != SOCKET_DIR_MODE {
        return Err(refuse(format!(
            "containing directory {} is mode {dmode:04o}, not {SOCKET_DIR_MODE:04o}",
            parent.display()
        )));
    }

    let smeta =
        std::fs::symlink_metadata(path).map_err(|source| UdsSecurityError::StatForConnect {
            path: path.to_path_buf(),
            source,
        })?;
    let ftype = smeta.file_type();
    if ftype.is_symlink() {
        return Err(refuse("socket path is a symlink".to_string()));
    }
    if !std::os::unix::fs::FileTypeExt::is_socket(&ftype) {
        return Err(refuse("path is not a socket".to_string()));
    }
    if smeta.uid() != own {
        return Err(refuse(format!(
            "socket is owned by uid {}, not {own}",
            smeta.uid()
        )));
    }
    let smode = smeta.permissions().mode() & 0o777;
    if smode != SOCKET_MODE {
        return Err(refuse(format!(
            "socket is mode {smode:04o}, not {SOCKET_MODE:04o}"
        )));
    }
    Ok(())
}
