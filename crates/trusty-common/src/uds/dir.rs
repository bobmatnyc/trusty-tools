//! Socket-directory creation and verification.
//!
//! Why: the containing directory — not the socket's own mode — is what makes a
//! socket unreachable to another uid, because Unix path resolution requires
//! search permission on every component. Getting the directory right is
//! therefore the whole security argument, and it is the part with the sharp
//! edge: `std::fs::metadata` and `set_permissions` both FOLLOW SYMLINKS, so a
//! naive owner+mode check reads the wrong inode when an attacker pre-creates
//! the path as a link (#5099 review finding 1).
//!
//! What: [`prepare_socket_dir`] creates the directory at `0700` atomically, or
//! — when it already exists — refuses a symlink outright and verifies owner and
//! mode on the directory itself. [`classify_existing_dir`] is the pure decision
//! function behind that verification, so every refusal is testable without root.
//!
//! Test: `tests.rs` — `classify_existing_dir_*` for the decisions,
//! `prepare_socket_dir_*` for the filesystem behavior including
//! `prepare_socket_dir_rejects_a_symlink`.

use std::fs::{DirBuilder, Permissions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::Path;

use super::{SOCKET_DIR_MODE, UdsSecurityError, peer::self_uid};

/// What [`prepare_socket_dir`] must do about a directory that already exists.
///
/// Why: separating the decision from the syscalls makes the refusal paths —
/// which otherwise need root or a second uid to reach — testable unprivileged.
/// Test: the `classify_existing_dir_*` tests.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DirVerdict {
    /// Owner and mode are already correct; nothing to do.
    Accept,
    /// Owner is correct but the mode is wider than `0700`; narrow it.
    Narrow,
}

/// Decide whether a pre-existing socket directory is usable, as a pure function.
///
/// Why: the three refusal paths (symlink, foreign owner, unnarrowable mode) are
/// the security-critical ones and the hardest to reach from a test — a symlink
/// swap needs a race, a foreign owner needs root. Taking the `lstat` results as
/// plain arguments makes each decision assertable directly (#5099 review
/// finding 5).
///
/// What: rejects a symlink before anything else — this is the fix for the
/// review's finding 1, where `metadata()` on a symlinked directory reported the
/// *target's* uid and mode, so a link pointing at any directory the running
/// user happens to own passed the owner check and `set_permissions` then
/// chmod'd the target. Then rejects a foreign owner, then reports whether the
/// mode needs narrowing.
///
/// `is_symlink`, `owner`, and `mode` must come from `symlink_metadata` (i.e.
/// `lstat`), never `metadata` — passing followed values reintroduces the bug
/// this function exists to prevent.
///
/// Test: `classify_existing_dir_rejects_a_symlink`,
/// `classify_existing_dir_rejects_a_foreign_owner`,
/// `classify_existing_dir_narrows_a_wide_dir`,
/// `classify_existing_dir_accepts_an_already_correct_dir`.
pub(crate) fn classify_existing_dir(
    path: &Path,
    is_symlink: bool,
    owner: u32,
    mode: u32,
    own_uid: u32,
) -> Result<DirVerdict, UdsSecurityError> {
    if is_symlink {
        return Err(UdsSecurityError::SymlinkDir {
            path: path.to_path_buf(),
        });
    }
    if owner != own_uid {
        return Err(UdsSecurityError::ForeignDirOwner {
            path: path.to_path_buf(),
            owner,
            expected: own_uid,
        });
    }
    if mode & 0o777 == SOCKET_DIR_MODE {
        Ok(DirVerdict::Accept)
    } else {
        Ok(DirVerdict::Narrow)
    }
}

/// Create `dir` at [`SOCKET_DIR_MODE`], or verify and repair it if it exists.
///
/// Why: this is what closes the bind-then-chmod window. Binding a Unix socket
/// creates the file, so a `chmod` after `bind` leaves an interval in which the
/// socket exists at the umask-derived mode. A caller cannot shrink that
/// interval to zero, and the obvious alternative — setting the process umask
/// around the bind — is process-global and therefore racy under a
/// multi-threaded tokio runtime, where a sibling thread creating an unrelated
/// file would silently inherit it. Holding the *directory* at `0700` before the
/// socket is created removes the exposure instead of narrowing it: path
/// resolution requires search permission on every component, so no other uid
/// can traverse to the socket at any point, including during the window.
///
/// What: creates `dir` with the mode passed to `mkdir(2)`, which is atomic —
/// unlike `create_dir_all` followed by `set_permissions`, which reproduces the
/// same ordering bug one level up. Ancestors are created with the default mode
/// because only the leaf holds sockets. If `dir` already exists, its `lstat`
/// results go through [`classify_existing_dir`]: a symlink is refused, a
/// foreign owner is refused, and a wider mode is narrowed.
///
/// **Residual race, deliberately not chased:** an attacker who can write to the
/// parent could in principle swap the directory for a symlink between the
/// `lstat` and the `chmod`. Under `/tmp`'s sticky bit that is impractical (only
/// the owner may rename or unlink an entry), and closing it properly needs
/// `openat`/`fchmod` on a directory fd. Rejecting symlinks removes the
/// pre-created-link attack, which is the practical one.
///
/// Test: `prepare_socket_dir_creates_at_0700`,
/// `prepare_socket_dir_narrows_a_wide_existing_dir`,
/// `prepare_socket_dir_rejects_a_symlink`, `prepare_socket_dir_is_idempotent`.
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

    // #5099 review finding 1: `symlink_metadata` (lstat), NOT `metadata` —
    // the latter follows the link and reports the target's owner and mode.
    let meta = std::fs::symlink_metadata(dir).map_err(|source| UdsSecurityError::CreateDir {
        path: dir.to_path_buf(),
        source,
    })?;
    let verdict = classify_existing_dir(
        dir,
        meta.file_type().is_symlink(),
        meta.uid(),
        meta.permissions().mode(),
        self_uid(),
    )?;

    if verdict == DirVerdict::Narrow {
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
