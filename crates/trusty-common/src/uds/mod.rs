//! Permission enforcement for Unix-domain sockets — the `0600` guarantee
//! ADR-0031 and ADR-0032 cite as an existing property.
//!
//! Why: both ADRs rest their case for UDS-over-loopback-TCP on "a loopback
//! port is reachable by any local process, a `0600` socket is not". Until
//! #5099 no production code in this workspace called `set_permissions` on any
//! socket — every hit was a test fixture. Sockets were created at the process
//! umask (commonly `0755`) and two of the three path conventions placed them
//! in `$TMPDIR` falling back to a shared, world-writable `/tmp`. This module
//! makes the claimed guarantee real, in one place, so a behavior fix lands
//! once rather than at four bind sites.
//!
//! What: three primitives, in the order a daemon uses them.
//!   - [`scratch_socket_dir`] resolves a per-uid directory under the system
//!     scratch space, replacing the bare `$TMPDIR`-with-`/tmp`-fallback.
//!   - [`bind_hardened`] creates that directory at `0700`, binds, and sets the
//!     socket to `0600` before returning it to the caller.
//!   - [`ensure_peer_is_self`] refuses an accepted connection whose peer uid is
//!     not this process's own, which is what turns the permission bits into an
//!     enforced boundary rather than a documented intention.
//!
//! Deliberately not a transport: there is no framing, dialing, or JSON-RPC
//! here. #5089 step 1 builds the shared UDS transport module; this is the
//! security layer that module mounts, not a competing implementation of it.
//!
//! Test: `tests.rs` — directory and socket modes after a real bind, the
//! pre-existing-wide-directory repair path, the foreign-owner refusal, and the
//! same-uid peer accept. Cross-uid rejection is `#[ignore]`d (needs two uids).

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

mod peer;

pub use peer::{ensure_peer_is_self, peer_uid, self_uid};

use std::fs::{DirBuilder, Permissions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use tokio::net::UnixListener;

/// Mode every socket-bearing directory is created at and verified against.
///
/// Owner-only `rwx`. Unix path resolution needs search (`x`) permission on
/// every directory component, so a `0700` directory makes every socket inside
/// it unreachable to any other uid regardless of the socket's own mode.
pub const SOCKET_DIR_MODE: u32 = 0o700;

/// Mode every bound socket is set to before its first `accept`.
pub const SOCKET_MODE: u32 = 0o600;

/// Failures that mean a socket could not be made private. Every variant is
/// fatal to the bind — none is a "log and continue" condition, because
/// continuing would leave a socket wider than the ADRs promise.
#[derive(Debug, thiserror::Error)]
pub enum UdsSecurityError {
    /// The socket path is a bare filename with no directory component, so
    /// there is nothing to harden.
    #[error("socket path {path} has no parent directory to harden")]
    NoParent {
        /// The offending socket path.
        path: PathBuf,
    },

    /// Creating the socket directory failed.
    #[error("create socket directory {path}: {source}")]
    CreateDir {
        /// Directory that could not be created.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: io::Error,
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
        source: io::Error,
    },

    /// `UnixListener::bind` failed.
    #[error("bind unix socket at {path}: {source}")]
    Bind {
        /// Socket path that could not be bound.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: io::Error,
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
        source: io::Error,
    },

    /// Reading the connected peer's credentials failed.
    #[error("read peer credentials: {source}")]
    PeerCred {
        /// Underlying OS error.
        #[source]
        source: io::Error,
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

/// Per-uid directory under the system scratch space for daemon sockets.
///
/// Why: the convention this replaces was `$TMPDIR`, falling back to `/tmp`.
/// On macOS `$TMPDIR` is a per-user `/var/folders/…/T/` that is already
/// `0700`, so the fallback never bit there. On a Linux host with `TMPDIR`
/// unset it resolved to `/tmp` — mode `1777`, owned by root — which can be
/// neither narrowed to `0700` nor trusted, so any socket in it was reachable
/// by every local user. Interposing a uid-keyed subdirectory gives a directory
/// this process owns and can hold at `0700` on both platforms uniformly.
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
/// Why: keeps the resolution rules unit-testable without `set_var`. A test
/// that mutates `TMPDIR` is visible to every concurrently-running sibling in
/// the same test binary, and `tempfile` honors it.
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

/// Create `dir` at [`SOCKET_DIR_MODE`], or verify and repair it if it exists.
///
/// Why: this is what closes the bind-then-chmod window. Binding a Unix socket
/// creates the file, so a `chmod` after `bind` leaves an interval in which the
/// socket exists at the umask-derived mode. A caller cannot shrink that
/// interval to zero, and the obvious alternative — setting the process umask
/// around the bind — is process-global and therefore racy under a
/// multi-threaded tokio runtime, where a sibling thread creating an unrelated
/// file would silently inherit it. Holding the *directory* at `0700` before
/// the socket is created removes the exposure instead of narrowing it: path
/// resolution requires search permission on every component, so no other uid
/// can name the socket at any point, including during the window.
///
/// What: creates `dir` with the mode passed to `mkdir(2)`, which is atomic —
/// unlike `create_dir_all` followed by `set_permissions`, which reproduces the
/// same ordering bug one level up. Ancestors are created with the default mode
/// because only the leaf holds sockets. If `dir` already exists, its owner must
/// be this uid (a foreign owner fails closed rather than being repaired) and a
/// wider mode is narrowed.
///
/// Test: `prepare_socket_dir_creates_at_0700`,
/// `prepare_socket_dir_narrows_a_wide_existing_dir`, and
/// `prepare_socket_dir_rejects_foreign_owner` (`#[ignore]`d — needs two uids).
pub fn prepare_socket_dir(dir: &Path) -> Result<(), UdsSecurityError> {
    // `Path::parent` yields `Some("")` for a bare relative name; `create_dir_all`
    // on an empty path fails, so filter it out rather than branching twice.
    if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|source| UdsSecurityError::CreateDir {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    // #5099: mkdir(2) applies the mode atomically, so the directory is never
    // observable at a wider mode. `create_dir_all` + `set_permissions` is not
    // an acceptable substitute here.
    match DirBuilder::new().mode(SOCKET_DIR_MODE).create(dir) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(source) => {
            return Err(UdsSecurityError::CreateDir {
                path: dir.to_path_buf(),
                source,
            });
        }
    }

    let meta = std::fs::metadata(dir).map_err(|source| UdsSecurityError::CreateDir {
        path: dir.to_path_buf(),
        source,
    })?;
    let expected = self_uid();
    if meta.uid() != expected {
        return Err(UdsSecurityError::ForeignDirOwner {
            path: dir.to_path_buf(),
            owner: meta.uid(),
            expected,
        });
    }
    if meta.permissions().mode() & 0o777 != SOCKET_DIR_MODE {
        std::fs::set_permissions(dir, Permissions::from_mode(SOCKET_DIR_MODE)).map_err(
            |source| UdsSecurityError::HardenDir {
                path: dir.to_path_buf(),
                mode: SOCKET_DIR_MODE,
                source,
            },
        )?;
    }
    Ok(())
}

/// Bind a listener at `path` with its directory at `0700` and the socket at
/// `0600`.
///
/// Why: the single entry point every daemon binds through, so the permission
/// contract cannot drift between the four bind sites that previously each
/// called `UnixListener::bind` bare (`trusty-embedderd`, `trusty-bm25-daemon`,
/// and two in `trusty-agents`). See #5099.
///
/// What: hardens the parent directory via [`prepare_socket_dir`], binds, then
/// narrows the socket to [`SOCKET_MODE`] before returning — so the listener is
/// already `0600` when the caller first calls `accept`. Deliberately does *not*
/// remove a stale socket file: `CtrlSocket::bind_singleton` must probe before
/// clobbering, and folding an unconditional unlink in here would break that
/// singleton guarantee. Callers that want stale-file cleanup keep doing it
/// themselves, immediately before this call.
///
/// The `0600` step is defence in depth, not the race fix — the `0700`
/// directory is what makes the socket unreachable during the window between
/// `bind` and `chmod`. [`prepare_socket_dir`]'s docs carry the reasoning.
///
/// Test: `bind_hardened_sets_socket_0600_and_dir_0700` and
/// `bind_hardened_socket_is_connectable_after_hardening`.
pub fn bind_hardened(path: &Path) -> Result<UnixListener, UdsSecurityError> {
    // `Path::parent` yields `Some("")` — not `None` — for a bare filename, so
    // the empty case has to be filtered explicitly or a relative socket name
    // would bind into an unhardened cwd.
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| UdsSecurityError::NoParent {
            path: path.to_path_buf(),
        })?;
    prepare_socket_dir(dir)?;

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
