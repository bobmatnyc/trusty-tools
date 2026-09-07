//! Private mutable state beneath `~/.trusty-code/` (#5426, epic #2892).
//!
//! Why: transcripts, logs, compression telemetry, daemon discovery files, and
//! channel cursors are per-user runtime state, not project configuration. They
//! were already landing in `~/.trusty-code` via `workstreams::default_data_dir`,
//! but nothing named that directory as PRIVATE and nothing set its mode, so a
//! default-umask machine left `0755` — world-readable transcripts of whatever
//! the harness was asked to work on.
//!
//! What: [`private_state_dir_at`] is the hermetic resolver (an explicit `$HOME`,
//! so tests never touch a real home directory and never mutate the environment);
//! [`private_state_dir`] is the production wrapper.
//! [`ensure_private_state_dir_at`] creates the tree and TIGHTENS the mode to
//! `0700` on Unix — including on a directory that already existed with a
//! permissive mode, since the common case is an upgrade over an existing
//! `~/.trusty-code`. [`is_restrictive`] is the predicate the tests and the
//! `tcode config paths` diagnostic both assert on.
//!
//! Test: `paths::private_state_tests::*`.

use std::io;
use std::path::{Path, PathBuf};

use super::TRUSTY_CODE_DIRNAME;

/// The Unix mode private state is held at: owner-only, no group, no other.
///
/// Why: `0700` is the same bar `~/.ssh` sets, and the state here (transcripts,
/// daemon tokens' neighbours, logs) is comparably sensitive.
/// What: `0o700`.
/// Test: `paths::private_state_tests::ensure_tightens_a_permissive_existing_dir`.
#[cfg(unix)]
pub const PRIVATE_DIR_MODE: u32 = 0o700;

/// The permission bits that must be clear for a directory to count as private.
///
/// Why: comparing the full mode would fail on a directory that is `0700` plus a
/// sticky or setgid bit, which is not a privacy problem.
/// What: `0o077` — every group and other bit.
/// Test: `paths::private_state_tests::permissive_mode_is_not_restrictive`.
#[cfg(unix)]
pub const NON_OWNER_BITS: u32 = 0o077;

/// Resolve the private state directory beneath an explicit home directory.
///
/// Why: the hermetic core. Every test points `home` at a temp dir, so none of
/// them reads or writes the developer's real `~/.trusty-code` and none needs to
/// mutate `$HOME` (an env mutation would race every other test in the binary).
/// What: `<home>/.trusty-code`.
/// Test: `paths::private_state_tests::private_state_dir_at_is_dot_trusty_code`.
pub fn private_state_dir_at(home: &Path) -> PathBuf {
    home.join(TRUSTY_CODE_DIRNAME)
}

/// Resolve the production private state directory, `~/.trusty-code`.
///
/// Why: the one place `dirs::home_dir()` is consulted for this purpose, so the
/// no-home fallback is decided once instead of per call site.
/// What: [`private_state_dir_at`] on `dirs::home_dir()`. With no resolvable home
/// — a stripped container, a daemon launched with no `$HOME` — it falls back to
/// a process-relative `.trusty-code` and logs at `warn` with the path used,
/// rather than panicking a harness that is otherwise fine.
/// Test: `paths::private_state_tests::private_state_dir_matches_home_when_available`.
pub fn private_state_dir() -> PathBuf {
    match dirs::home_dir() {
        Some(home) => private_state_dir_at(&home),
        None => {
            let fallback = PathBuf::from(TRUSTY_CODE_DIRNAME);
            // #5426: fail open, but never silently — a relative state directory
            // follows the process's working directory, which is rarely intended.
            tracing::warn!(
                path = %fallback.display(),
                "no home directory could be resolved; falling back to a \
                 working-directory-relative trusty-code state directory"
            );
            fallback
        }
    }
}

/// Create the private state directory and tighten it to owner-only.
///
/// Why: creating it with `create_dir_all` alone applies the process umask, which
/// on a default `022` machine yields `0755`. An upgrade path matters as much as
/// a fresh install: `~/.trusty-code` already exists on every machine that has
/// run `tcode`, so this tightens an EXISTING permissive directory rather than
/// only getting new ones right.
/// What: `create_dir_all`, then on Unix `set_permissions` to
/// [`PRIVATE_DIR_MODE`] whenever any of [`NON_OWNER_BITS`] is set. A failure to
/// tighten is logged at `warn` with the path and the mode observed, and the
/// directory is still returned — the harness must run on a filesystem that
/// cannot represent Unix modes. Non-Unix targets create the directory and rely
/// on the platform's own per-user home ACLs.
/// Test: `paths::private_state_tests::ensure_creates_restrictive_dir`,
/// `paths::private_state_tests::ensure_tightens_a_permissive_existing_dir`.
pub fn ensure_private_state_dir_at(home: &Path) -> io::Result<PathBuf> {
    let dir = private_state_dir_at(home);
    std::fs::create_dir_all(&dir)?;
    harden(&dir);
    Ok(dir)
}

/// Create and tighten the production `~/.trusty-code`.
///
/// Why: the production wrapper over [`ensure_private_state_dir_at`], so callers
/// never resolve the home directory themselves.
/// What: [`private_state_dir`] then the same create-and-tighten sequence.
/// Test: covered hermetically by `ensure_private_state_dir_at`'s tests; the
/// wrapper adds only home resolution.
pub fn ensure_private_state_dir() -> io::Result<PathBuf> {
    let dir = private_state_dir();
    std::fs::create_dir_all(&dir)?;
    harden(&dir);
    Ok(dir)
}

/// Tighten `dir` to [`PRIVATE_DIR_MODE`] when any non-owner bit is set.
///
/// Why: separated from [`ensure_private_state_dir_at`] so the "already private"
/// case does a single `metadata` call and no write at all.
/// What: no-op when [`is_restrictive`] already holds; otherwise
/// `set_permissions`, logging a `warn` if that fails.
/// Test: `paths::private_state_tests::ensure_tightens_a_permissive_existing_dir`.
#[cfg(unix)]
fn harden(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::metadata(dir) else {
        return;
    };
    let mode = meta.permissions().mode();
    if mode & NON_OWNER_BITS == 0 {
        return;
    }
    if let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(PRIVATE_DIR_MODE))
    {
        tracing::warn!(
            path = %dir.display(),
            mode = format!("{:o}", mode & 0o777),
            error = %e,
            "could not tighten the trusty-code private state directory to 0700; \
             its contents may be readable by other users on this machine"
        );
    }
}

/// Non-Unix targets have no mode bits to tighten.
#[cfg(not(unix))]
fn harden(_dir: &Path) {}

/// Whether a directory's permissions keep it private to its owner.
///
/// Why: the assertion the permissive-permissions test makes, and the field the
/// `tcode config paths` diagnostic reports, must come from ONE rule.
/// What: on Unix, `true` when none of [`NON_OWNER_BITS`] is set; `Err` when the
/// directory cannot be stat'd. On non-Unix targets, always `true` — the platform
/// has no comparable bits and the home directory's ACL governs.
/// Test: `paths::private_state_tests::permissive_mode_is_not_restrictive`,
/// `paths::private_state_tests::ensure_creates_restrictive_dir`.
#[cfg(unix)]
pub fn is_restrictive(dir: &Path) -> io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(dir)?.permissions().mode();
    Ok(mode & NON_OWNER_BITS == 0)
}

/// See the Unix variant — non-Unix targets have no mode bits to inspect.
#[cfg(not(unix))]
pub fn is_restrictive(dir: &Path) -> io::Result<bool> {
    std::fs::metadata(dir)?;
    Ok(true)
}

#[cfg(test)]
#[path = "private_state_tests.rs"]
mod private_state_tests;
