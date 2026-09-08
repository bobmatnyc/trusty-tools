//! One shared "create and hold a directory at owner-only" implementation
//! (#7158).
//!
//! Why: before this module, `trusty-common` alone held three independent
//! bodies solving the same problem — `uds::dir::prepare_socket_dir` (atomic,
//! lstat-based symlink/owner checks, hardcoded `0700`), `webhook_relay::inbox`
//! (unconditional `create_dir_all` + `set_permissions`, no symlink defense,
//! deliberately narrower per its own doc comment), and a fourth, crate-private
//! copy the log-drain manifest cache briefly carried, itself mirroring a FIFTH
//! implementation in `trusty-code::paths::private_state` that `trusty-common`
//! cannot even depend on. Three-plus divergent bug profiles for one
//! specification is a defect the common-entry-point rule exists to close.
//! What: [`ensure_private_dir`] — lstat the leaf FIRST, before any call that
//! would follow a symlink touches it; refuse a symlink or a non-directory
//! outright; and for a genuinely new path, create the whole chain atomically
//! at the caller's `mode` via `DirBuilder::recursive(true)`, which (verified
//! empirically for #7158) applies that mode to every ancestor it creates, not
//! only the leaf — the gap `create_dir_all` then `chmod` leaves, since that
//! ordering briefly exposes every level of the chain at the umask-derived
//! mode and narrows only the last one.
//!
//! Kept independent of the `uds`-feature-gated module deliberately: `uds` (and
//! anything under it) is `#[cfg(all(unix, feature = "uds"))]`, and this
//! module's callers — `log_drain` (feature `log-drain`, no `uds` implication)
//! and `webhook_relay` (feature `webhook-relay`, which DOES imply `uds`, but
//! only coincidentally) — must not have to pull in socket-directory checks
//! just to hold a private cache or spool directory. `uds::dir::prepare_socket_dir`
//! is left as its own implementation rather than rewritten onto this one in
//! this change — its owner-uid check has no equivalent here, and that is a
//! deliberate, security-relevant difference for a socket path shared by
//! multiple local processes, not an oversight. See `uds::dir`'s own
//! `// See #7158` pointer.
//!
//! What is NOT closed by this module: `trusty-code::paths::private_state` and
//! `trusty-agents`' own directory-hardening call sites stay on their own
//! implementations in this change — `trusty-common` cannot depend on either
//! crate, so migrating them means turning this module's logic into the thing
//! THEY call, in a follow-up PR, not something this PR can reach.
//!
//! Test: `tests` below.

use std::path::{Path, PathBuf};

/// The mode this module's canonical private directories are held at: the same
/// bar `~/.ssh` sets, and what `uds::SOCKET_DIR_MODE`,
/// `webhook_relay::inbox::INBOX_DIR_MODE`, and
/// `trusty-code::paths::private_state::PRIVATE_DIR_MODE` all independently
/// converged on.
pub const PRIVATE_DIR_MODE: u32 = 0o700;

/// Why a directory could not be prepared or held at the requested mode.
///
/// What: a plain enum with hand-written [`std::fmt::Display`]/[`std::error::Error`]
/// rather than `thiserror`, so this module stays part of `trusty-common`'s
/// always-compiled surface (`unconditional-only`) instead of requiring the
/// `thiserror` optional dependency a feature would have to pull in.
/// Test: `tests::ensure_private_dir_refuses_a_symlinked_leaf`,
/// `tests::ensure_private_dir_refuses_a_regular_file_leaf`.
#[derive(Debug)]
#[non_exhaustive]
pub enum PrivateDirError {
    /// The directory (or one of its ancestors) could not be created.
    Create {
        /// The path passed to [`ensure_private_dir`].
        path: PathBuf,
        /// Underlying OS error.
        source: std::io::Error,
    },
    /// `lstat` on the leaf path failed for a reason other than "not found".
    Stat {
        /// The path passed to [`ensure_private_dir`].
        path: PathBuf,
        /// Underlying OS error.
        source: std::io::Error,
    },
    /// The leaf path is a symlink. Never followed, never touched.
    Symlink {
        /// The path passed to [`ensure_private_dir`].
        path: PathBuf,
    },
    /// The leaf path exists and is not a directory.
    NotADirectory {
        /// The path passed to [`ensure_private_dir`].
        path: PathBuf,
        /// A short description of what was found instead (`"regular file"`,
        /// `"fifo"`, …).
        found: &'static str,
    },
    /// The leaf pre-existed at a wider mode and could not be narrowed.
    Harden {
        /// The path passed to [`ensure_private_dir`].
        path: PathBuf,
        /// The mode that could not be applied.
        mode: u32,
        /// Underlying OS error.
        source: std::io::Error,
    },
}

impl std::fmt::Display for PrivateDirError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Create { path, source } => {
                write!(
                    f,
                    "could not create private directory {}: {source}",
                    path.display()
                )
            }
            Self::Stat { path, source } => {
                write!(f, "could not stat {}: {source}", path.display())
            }
            Self::Symlink { path } => {
                write!(
                    f,
                    "refusing {}: it is a symlink, not a private directory",
                    path.display()
                )
            }
            Self::NotADirectory { path, found } => write!(
                f,
                "refusing {}: found a {found}, not a directory",
                path.display()
            ),
            Self::Harden { path, mode, source } => write!(
                f,
                "could not tighten {} to {mode:04o}: {source}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for PrivateDirError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Create { source, .. }
            | Self::Stat { source, .. }
            | Self::Harden { source, .. } => Some(source),
            Self::Symlink { .. } | Self::NotADirectory { .. } => None,
        }
    }
}

/// Create `dir` (and any missing ancestors) and hold it at `mode`, refusing a
/// pre-existing symlink or non-directory rather than following it.
///
/// Why: `create_dir_all`, `std::fs::metadata`, and `std::fs::set_permissions`
/// all follow symlinks — an attacker who pre-plants one at the leaf (or at an
/// ancestor a naive `create_dir_all` walks through) silently redirects both
/// the caller's writes and any `chmod` meant to privatize them, with no error
/// (#7158, same defect class `uds::dir::classify_existing_dir` closed for
/// sockets under #5099).
/// What: `lstat`s the leaf FIRST, before any mutating call can reach it.
/// - Not found: creates the whole chain atomically via
///   `DirBuilder::mode(mode).recursive(true)`, which applies `mode` to every
///   directory it creates — ancestors included, not only the leaf (verified
///   empirically for #7158; `create_dir_all` then `chmod` narrows only the
///   last one).
/// - A directory: narrows it to `mode` if its current mode differs.
/// - A symlink or non-directory: refused with a typed error, never touched.
///
/// **Residual race, deliberately not chased** (same acceptance
/// `uds::dir::prepare_socket_dir` documents): an attacker who can already
/// write to the parent could swap the path for a symlink between the `lstat`
/// and the create/narrow call. Closing that fully needs `openat`/`fchmod` on a
/// directory fd; rejecting a symlink found at `lstat` time removes the
/// practical pre-planted-link attack, which is the one this function exists
/// to stop for a directory living under the caller's own home directory (not
/// a shared, world-writable location).
///
/// Test: `tests::ensure_private_dir_creates_new_ancestors_at_mode`,
/// `tests::ensure_private_dir_narrows_a_wide_existing_leaf`,
/// `tests::ensure_private_dir_refuses_a_symlinked_leaf`,
/// `tests::ensure_private_dir_refuses_a_regular_file_leaf`.
pub fn ensure_private_dir(dir: &Path, mode: u32) -> Result<(), PrivateDirError> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) => {
            let ftype = meta.file_type();
            if ftype.is_symlink() {
                return Err(PrivateDirError::Symlink {
                    path: dir.to_path_buf(),
                });
            }
            if !ftype.is_dir() {
                return Err(PrivateDirError::NotADirectory {
                    path: dir.to_path_buf(),
                    found: describe_non_dir(&ftype),
                });
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let current = meta.permissions().mode() & 0o777;
                if current != mode {
                    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).map_err(
                        |source| PrivateDirError::Harden {
                            path: dir.to_path_buf(),
                            mode,
                            source,
                        },
                    )?;
                }
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => create_at_mode(dir, mode),
        Err(source) => Err(PrivateDirError::Stat {
            path: dir.to_path_buf(),
            source,
        }),
    }
}

/// `describe_file_type`'s narrow cousin: only reachable once `is_dir()` and
/// `is_symlink()` have both already been ruled out, so it names only what
/// remains.
fn describe_non_dir(ft: &std::fs::FileType) -> &'static str {
    if ft.is_file() {
        "regular file"
    } else {
        "special file"
    }
}

#[cfg(unix)]
fn create_at_mode(dir: &Path, mode: u32) -> Result<(), PrivateDirError> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .mode(mode)
        .recursive(true)
        .create(dir)
        .map_err(|source| PrivateDirError::Create {
            path: dir.to_path_buf(),
            source,
        })
}

/// Non-Unix targets have no mode bits to apply atomically; create the chain
/// and rely on the platform's own per-user ACLs, matching every other
/// `ensure_dir`-shaped helper in this codebase.
#[cfg(not(unix))]
fn create_at_mode(dir: &Path, _mode: u32) -> Result<(), PrivateDirError> {
    std::fs::create_dir_all(dir).map_err(|source| PrivateDirError::Create {
        path: dir.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
    }

    #[test]
    fn ensure_private_dir_creates_new_ancestors_at_mode() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let nested = tmp.path().join("a").join("b").join("c");

        ensure_private_dir(&nested, PRIVATE_DIR_MODE).expect("create");

        #[cfg(unix)]
        for level in [tmp.path().join("a"), tmp.path().join("a").join("b"), nested] {
            assert_eq!(
                mode_of(&level),
                PRIVATE_DIR_MODE,
                "{} must be 0700, including ancestors this call created",
                level.display()
            );
        }
        #[cfg(not(unix))]
        assert!(nested.is_dir());
    }

    #[test]
    fn ensure_private_dir_narrows_a_wide_existing_leaf() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("wide");
        std::fs::create_dir_all(&dir).expect("mkdir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))
                .expect("chmod wide");
        }

        ensure_private_dir(&dir, PRIVATE_DIR_MODE).expect("narrow");

        #[cfg(unix)]
        assert_eq!(
            mode_of(&dir),
            PRIVATE_DIR_MODE,
            "pre-existing wide dir must be narrowed"
        );
    }

    #[test]
    #[cfg(unix)]
    fn ensure_private_dir_refuses_a_symlinked_leaf() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let real_target = tmp.path().join("real");
        std::fs::create_dir_all(&real_target).expect("mkdir real");
        let leaf = tmp.path().join("leaf");
        std::os::unix::fs::symlink(&real_target, &leaf).expect("symlink");

        let err = ensure_private_dir(&leaf, PRIVATE_DIR_MODE).expect_err("must refuse a symlink");
        assert!(
            matches!(err, PrivateDirError::Symlink { .. }),
            "got {err:?}"
        );

        // The symlink itself must be untouched — neither followed nor chmod'd.
        assert!(
            std::fs::symlink_metadata(&leaf)
                .expect("lstat")
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn ensure_private_dir_refuses_a_regular_file_leaf() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let leaf = tmp.path().join("leaf");
        std::fs::write(&leaf, b"not a directory").expect("write file");

        let err =
            ensure_private_dir(&leaf, PRIVATE_DIR_MODE).expect_err("must refuse a regular file");
        assert!(
            matches!(err, PrivateDirError::NotADirectory { .. }),
            "got {err:?}"
        );
    }
}
