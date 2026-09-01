//! `tm doctor` pseudo-terminal headroom probe (#6529).
//!
//! Why: every tmux pane holds one pseudo-terminal, and macOS caps the total at
//! `kern.tty.ptmx_max` — 511 by default. When the orphan-GC stopped reaping,
//! 456 leaked `tm-*` sessions pushed the host past that cap, and the next
//! session spawn failed with a bare `ENXIO` that names neither the limit nor
//! the leak. Nothing reported the pressure before it became a hard failure.
//!
//! What: [`check_pty_headroom`] folds a pty census into a [`DoctorCheck`] via
//! the pure [`build_pty_headroom_check`]. Only the two platform reads are
//! target-gated, behind [`read_pty_census`], which yields `None` off Darwin
//! because `/dev/ttys*` is a Darwin device layout; the check then reports `Ok`
//! with a skip reason. The threshold classification and its constants sit on
//! the non-gated path, so they compile and are tested on every target.
//! Read-only: it counts device nodes and reaps nothing itself.
//! Test: the `tests` module below.

use crate::core::doctor::{CheckStatus, DoctorCheck};

/// Stable check name.
pub(super) const CHECK_NAME: &str = "pty_headroom";

/// Fraction of the cap at which the probe warns.
const WARN_AT: f64 = 0.80;

/// Fraction of the cap at which the probe fails.
const FAIL_AT: f64 = 0.95;

/// Decide the check from an already-gathered count and cap.
///
/// Why: pure, so every threshold branch is testable without a Darwin host, a
/// `sysctl` binary, or a `/dev` walk.
/// What: `Unknown` when either number could not be read — a probe that learned
/// nothing must not read healthy. Otherwise `Fail` at or above [`FAIL_AT`] of
/// the cap, `Warn` at or above [`WARN_AT`], else `Ok`. Every non-`Ok` message
/// names both numbers and the command that reaps the leaked sessions holding
/// them, because the count alone does not tell an operator what to do.
/// Test: `pty_headroom_ok_below_the_warn_line`, `pty_headroom_warns_at_80pct`,
/// `pty_headroom_fails_at_95pct`, `pty_headroom_is_unknown_without_numbers`.
fn build_pty_headroom_check(allocated: Option<usize>, max: Option<usize>) -> DoctorCheck {
    let (Some(allocated), Some(max)) = (allocated, max) else {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            "could not read both `sysctl kern.tty.ptmx_max` and the /dev/ttys* \
             inventory — pseudo-terminal headroom undetermined (#6529)",
        );
    };
    if max == 0 {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            "`kern.tty.ptmx_max` reported 0 — pseudo-terminal headroom undetermined (#6529)",
        );
    }
    let used = allocated as f64 / max as f64;
    let pct = (used * 100.0).round() as u64;
    let detail = format!(
        "{allocated} of {max} pseudo-terminals allocated ({pct}%); every tmux pane \
         holds one, so a leaked session holds one until it is reaped — \
         `tm doctor --fix` does not reap sessions; `tm session prune` and \
         the daemon's orphan-GC do (#6529)"
    );
    if used >= FAIL_AT {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Fail,
            format!("{detail} — a new session spawn will fail with ENXIO at this level"),
        );
    }
    if used >= WARN_AT {
        return DoctorCheck::new(CHECK_NAME, CheckStatus::Warn, detail);
    }
    DoctorCheck::new(
        CHECK_NAME,
        CheckStatus::Ok,
        format!("{allocated} of {max} pseudo-terminals allocated ({pct}%)"),
    )
}

/// Read `kern.tty.ptmx_max`.
///
/// Why: the cap is a runtime sysctl, not a constant — an operator can raise it,
/// and a probe that assumed 511 would misreport a host that did.
/// What: `sysctl -n kern.tty.ptmx_max`, parsed as a count. `None` on any
/// failure, which the caller reports as `Unknown` rather than a default.
/// Test: covered through `check_pty_headroom` on a Darwin host.
#[cfg(target_os = "macos")]
fn read_ptmx_max() -> Option<usize> {
    let out = std::process::Command::new("sysctl")
        .args(["-n", "kern.tty.ptmx_max"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Count the allocated pseudo-terminal slaves.
///
/// Why: macOS creates `/dev/ttysNNN` lazily, one per allocated pty, so the node
/// count IS the in-use count. There is no sysctl reporting it.
/// What: counts `/dev` entries whose name starts with `ttys`. `None` when
/// `/dev` cannot be read.
/// Test: covered through `check_pty_headroom` on a Darwin host.
#[cfg(target_os = "macos")]
fn count_allocated_ptys() -> Option<usize> {
    let entries = std::fs::read_dir("/dev").ok()?;
    Some(
        entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with("ttys"))
            })
            .count(),
    )
}

/// Gather the pty census on hosts that have one.
///
/// Why: #6529 — this is the file's ONLY target-gated decision. Confining the
/// platform split to the reads keeps [`build_pty_headroom_check`] and its
/// thresholds on the non-gated path, so they are neither dead code nor
/// untested off Darwin.
/// What: `Some((allocated, cap))` on macOS, each element still `None` when that
/// individual read failed; `None` where the `/dev/ttys*` inventory does not
/// exist as a concept.
/// Test: `pty_headroom_skips_cleanly_off_darwin`.
#[cfg(target_os = "macos")]
fn read_pty_census() -> Option<(Option<usize>, Option<usize>)> {
    Some((count_allocated_ptys(), read_ptmx_max()))
}

/// No pty census off Darwin — see the macOS variant above.
#[cfg(not(target_os = "macos"))]
fn read_pty_census() -> Option<(Option<usize>, Option<usize>)> {
    None
}

/// Report how much pseudo-terminal capacity the host has left (#6529).
///
/// Why: see the module doc — pty exhaustion presents as an unexplained `ENXIO`
/// on the next session spawn, long after the leak that caused it.
/// What: renders whatever [`read_pty_census`] gathered with
/// [`build_pty_headroom_check`]. A host with no census reports `Ok` naming the
/// skip, because a check that guessed on Linux would report a fiction.
/// Test: `pty_headroom_skips_cleanly_off_darwin` and the pure-verdict tests.
pub(super) fn check_pty_headroom() -> DoctorCheck {
    let Some((allocated, max)) = read_pty_census() else {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            "pseudo-terminal headroom is a macOS-specific probe (`kern.tty.ptmx_max` \
             against the /dev/ttys* inventory) — skipped on this platform",
        );
    };
    build_pty_headroom_check(allocated, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pty_headroom_ok_below_the_warn_line() {
        let check = build_pty_headroom_check(Some(100), Some(511));
        assert_eq!(check.status, CheckStatus::Ok);
        assert!(check.message.contains("100 of 511"), "{}", check.message);
    }

    #[test]
    fn pty_headroom_warns_at_80pct() {
        let check = build_pty_headroom_check(Some(409), Some(511));
        assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
        assert!(check.message.contains("409 of 511"), "{}", check.message);
        assert!(
            check.message.contains("orphan-GC"),
            "the message must name what reaps the sessions holding them: {}",
            check.message
        );
    }

    #[test]
    fn pty_headroom_fails_at_95pct() {
        // The state this probe was written for: 527 allocated against a cap of
        // 511, from 456 leaked tmux sessions the orphan-GC never reaped.
        let check = build_pty_headroom_check(Some(527), Some(511));
        assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
        assert!(check.message.contains("ENXIO"), "{}", check.message);
    }

    #[test]
    fn pty_headroom_is_unknown_without_numbers() {
        assert_eq!(
            build_pty_headroom_check(None, Some(511)).status,
            CheckStatus::Unknown
        );
        assert_eq!(
            build_pty_headroom_check(Some(10), None).status,
            CheckStatus::Unknown
        );
        // A zero cap is not a division to attempt.
        assert_eq!(
            build_pty_headroom_check(Some(10), Some(0)).status,
            CheckStatus::Unknown
        );
    }

    /// The probe always returns a named check, on every platform.
    #[test]
    fn pty_headroom_skips_cleanly_off_darwin() {
        let check = check_pty_headroom();
        assert_eq!(check.name, CHECK_NAME);
        #[cfg(not(target_os = "macos"))]
        assert_eq!(check.status, CheckStatus::Ok);
    }
}
