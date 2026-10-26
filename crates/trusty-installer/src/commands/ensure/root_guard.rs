//! Root refusal for `tctl ensure` (#10320).
//!
//! Why: under `sudo`, `ensure` rewrote `./.mcp.json` and its `.bak` as
//! root-owned, resolved the daemon sockets under root's data dir, and could not
//! reach the user's launchd domain. The fix is to refuse, fail-closed, before
//! any file write or socket dial — never to drop privileges.
//! What: [`check`] is the pure decision over an effective uid; [`effective_uid`]
//! reads the real one; [`guarded`] runs the provisioning closure only when the
//! check passes.
//! Test: `guard_tests` in this file.

/// Exit code `tctl ensure` returns when it refuses to run as root.
pub const EXIT_ROOT_REFUSED: i32 = 1;

/// Message printed when `tctl ensure` runs with effective uid 0.
pub const ROOT_REFUSAL: &str = "tctl ensure: refusing to run as root (effective uid 0). \
Do not run it with sudo: it would write root-owned files into this project and \
resolve the daemons under root's data directory. Re-run it as your normal user. \
If a file is already root-owned (for example ./.mcp.json or ./.mcp.json.bak), \
fix it with: sudo chown $USER <file>";

/// Decide whether `ensure` may run under the given effective uid.
///
/// Why: the decision must be testable without real root, so it takes the uid
/// as a value instead of reading it.
/// What: `Some(0)` is refused with [`ROOT_REFUSAL`]; any other uid, or `None`
/// (a platform with no uid), is allowed.
/// Test: `check_refuses_uid_zero`, `check_allows_non_root`.
pub fn check(euid: Option<u32>) -> Result<(), &'static str> {
    match euid {
        Some(0) => Err(ROOT_REFUSAL),
        _ => Ok(()),
    }
}

/// Effective uid of this process; `None` on a non-Unix target.
#[cfg(unix)]
pub fn effective_uid() -> Option<u32> {
    // SAFETY: geteuid(2) takes no arguments, touches no memory and cannot fail.
    Some(unsafe { libc::geteuid() })
}

/// Effective uid of this process; `None` on a non-Unix target.
#[cfg(not(unix))]
pub fn effective_uid() -> Option<u32> {
    None
}

/// Run `provision` only when [`check`] allows `euid`.
///
/// Why: placing the check in front of the whole provisioning closure is what
/// guarantees no `.mcp.json`, `.bak`, `.tmp.*` write and no socket dial happens
/// as root.
/// What: on refusal prints [`ROOT_REFUSAL`] to stderr and returns
/// [`EXIT_ROOT_REFUSED`] without calling `provision`; otherwise returns
/// `provision()`'s exit code unchanged.
/// Test: `guarded_root_never_provisions`, `guarded_non_root_runs_provision`.
pub fn guarded(euid: Option<u32>, provision: impl FnOnce() -> i32) -> i32 {
    match check(euid) {
        Err(msg) => {
            eprintln!("{msg}");
            EXIT_ROOT_REFUSED
        }
        Ok(()) => provision(),
    }
}

#[cfg(test)]
mod guard_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn check_refuses_uid_zero() {
        let msg = check(Some(0)).expect_err("uid 0 must be refused");
        assert!(msg.contains("sudo"), "{msg}");
        assert!(msg.contains("root"), "{msg}");
        assert!(msg.contains("sudo chown $USER <file>"), "{msg}");
    }

    #[test]
    fn check_allows_non_root() {
        assert_eq!(check(Some(501)), Ok(()));
        assert_eq!(check(Some(1000)), Ok(()));
        assert_eq!(check(None), Ok(()));
    }

    #[test]
    fn guarded_root_never_provisions() {
        let called = Cell::new(false);
        let code = guarded(Some(0), || {
            called.set(true);
            0
        });
        assert_ne!(code, 0, "root must exit non-zero");
        assert_eq!(code, EXIT_ROOT_REFUSED);
        assert!(!called.get(), "provisioning ran as root");
    }

    #[test]
    fn guarded_non_root_runs_provision() {
        let called = Cell::new(false);
        let code = guarded(Some(1000), || {
            called.set(true);
            4
        });
        assert!(called.get());
        assert_eq!(code, 4, "non-root exit code must pass through unchanged");
    }
}
