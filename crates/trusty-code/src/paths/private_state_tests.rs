//! Unit tests for the private `~/.trusty-code/` state root (#5426).
//!
//! Why: the permissions property is the one an operator cannot see and cannot
//! easily audit, so it needs a test that actually reads the mode back off disk.
//! What: layout, creation mode, tightening an existing permissive directory, and
//! the predicate that rejects a permissive mode.
//! Test: this file IS the test module.

use super::*;

/// Whether this process runs as root (which ignores permission bits).
#[cfg(unix)]
fn running_as_root() -> bool {
    // SAFETY: `geteuid` takes no arguments, touches no memory, and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// The private state directory is `<home>/.trusty-code`.
///
/// Why: pins the layout so it cannot drift from the project-side constant.
/// What: an explicit home; no environment mutation.
/// Test: this function IS the test.
#[test]
fn private_state_dir_at_is_dot_trusty_code() {
    let home = std::path::Path::new("/tmp/some-home");
    assert_eq!(
        private_state_dir_at(home),
        home.join(TRUSTY_CODE_DIRNAME),
        "private state must live at <home>/.trusty-code"
    );
}

/// The production wrapper agrees with the hermetic core.
///
/// Why: the wrapper's only job is home resolution; a divergence would put
/// production state somewhere the tests never look.
/// What: compares `private_state_dir()` against `private_state_dir_at(home)` on
/// a machine that has a home directory. Reads nothing and creates nothing.
/// Test: this function IS the test.
#[test]
fn private_state_dir_matches_home_when_available() {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    assert_eq!(private_state_dir(), private_state_dir_at(&home));
}

/// A freshly created private state directory is `0700`.
///
/// Why: `create_dir_all` alone applies the process umask, which on a default
/// `022` machine leaves transcripts world-readable.
/// What: creates the directory under a temp home and reads the mode back.
/// Test: this function IS the test.
#[cfg(unix)]
#[test]
fn ensure_creates_restrictive_dir() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = ensure_private_state_dir_at(tmp.path()).expect("create private state dir");

    assert!(dir.is_dir());
    let mode = std::fs::metadata(&dir).expect("stat").permissions().mode() & 0o777;
    assert_eq!(mode, PRIVATE_DIR_MODE, "expected 0700, got {mode:o}");
    assert!(is_restrictive(&dir).expect("stat"));
}

/// An existing permissive directory is tightened, not left as found.
///
/// Why: `~/.trusty-code` already exists on every machine that has run `tcode`,
/// so getting only fresh installs right would leave the actual fleet exposed.
/// What: pre-creates the directory at `0755`, runs `ensure_private_state_dir_at`,
/// and asserts the mode became `0700`.
/// Test: this function IS the test.
#[cfg(unix)]
#[test]
fn ensure_tightens_a_permissive_existing_dir() {
    use std::os::unix::fs::PermissionsExt;

    if running_as_root() {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = private_state_dir_at(tmp.path());
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    assert!(
        !is_restrictive(&dir).expect("stat"),
        "precondition: 0755 must not count as restrictive"
    );

    let created = ensure_private_state_dir_at(tmp.path()).expect("ensure");

    assert_eq!(created, dir);
    let mode = std::fs::metadata(&dir).expect("stat").permissions().mode() & 0o777;
    assert_eq!(mode, PRIVATE_DIR_MODE, "expected 0700, got {mode:o}");
}

/// A permissive mode is rejected by the predicate.
///
/// Why: this is the assertion an audit — and the `tcode config paths`
/// diagnostic — relies on; a predicate that returned `true` for `0755` would
/// make every other permissions test vacuous.
/// What: stages `0750`, `0705`, and `0700` and asserts only the last is private.
/// Test: this function IS the test.
#[cfg(unix)]
#[test]
fn permissive_mode_is_not_restrictive() {
    use std::os::unix::fs::PermissionsExt;

    if running_as_root() {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("state");
    std::fs::create_dir_all(&dir).expect("mkdir");

    for permissive in [0o750, 0o705, 0o777] {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(permissive)).expect("chmod");
        assert!(
            !is_restrictive(&dir).expect("stat"),
            "{permissive:o} exposes bits to group or other and must not count as private"
        );
    }

    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    assert!(is_restrictive(&dir).expect("stat"));
}

/// A missing directory surfaces the I/O error rather than claiming privacy.
///
/// Why: `is_restrictive` returning `Ok(true)` for an absent path would let an
/// audit pass on a directory that does not exist.
/// What: asserts `Err` for a path that was never created.
/// Test: this function IS the test.
#[test]
fn is_restrictive_errors_on_a_missing_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(is_restrictive(&tmp.path().join("nope")).is_err());
}
