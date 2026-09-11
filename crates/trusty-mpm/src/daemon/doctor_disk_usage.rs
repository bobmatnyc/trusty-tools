//! `tm doctor` disk-usage-threshold probe (#7497).
//!
//! Why: the `disk.max_usage_pct` gate refuses a worktree at creation time, which
//! is the worst moment to learn the volume is full. This probe puts the same two
//! numbers — what the mount is at, and what the threshold is — on screen before
//! an operator asks for a session, and names the key that moves the line.
//!
//! What: one `disk_usage` check over the mount holding the worktree store.
//! `Ok` below the threshold, `Warn` at or above it (the gate is refusing, but a
//! diagnostic is not itself a failure of the installation), and `Unknown` when
//! the mount could not be measured — a probe that learned nothing must not read
//! healthy. The threshold is the PRODUCTION one: unlike the gate,
//! [`check_disk_usage`] does not disable itself under a test harness, because
//! reporting is not refusing.
//!
//! Test: the `tests` module in `doctor_disk_usage_tests.rs`.

use std::path::Path;

use crate::core::disk_usage_guard::{
    MAX_USAGE_PCT_KEY, MeasuredMount, read_configured, resolve_max_usage_pct,
};
use crate::core::doctor::{CheckStatus, DoctorCheck};

/// Stable check name.
pub(super) const CHECK_NAME: &str = "disk_usage";

/// The pure verdict, separated for hermetic tests.
///
/// Why: all three branches must be provable without the runner's real disk —
/// a check whose test outcome depends on how full the developer's volume is
/// proves nothing.
/// What: `Unknown` when `measured` is `None`; `Warn` at or above
/// `threshold_pct`; otherwise `Ok`. Every message names the mount, the measured
/// percent, the threshold and the config key, because the percentage alone does
/// not tell an operator what to change.
/// Test: `disk_usage_is_ok_below_the_threshold`,
/// `disk_usage_warns_at_the_threshold`,
/// `disk_usage_is_unknown_when_the_mount_cannot_be_measured`.
fn build_disk_usage_check(measured: Option<&MeasuredMount>, threshold_pct: u8) -> DoctorCheck {
    let Some(m) = measured else {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            format!(
                "could not measure the mount holding the worktree store — disk headroom \
                 against the {MAX_USAGE_PCT_KEY} threshold of {threshold_pct}% is undetermined \
                 (#7497)"
            ),
        );
    };
    let detail = format!(
        "{} is at {:.1}% disk usage; {MAX_USAGE_PCT_KEY} threshold is {threshold_pct}%",
        m.mount_point, m.usage_pct
    );
    if m.usage_pct >= f32::from(threshold_pct) {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Warn,
            format!(
                "{detail} — new worktrees are being REFUSED; free space or raise \
                 {MAX_USAGE_PCT_KEY} in the trusty-mpm config (#7497)"
            ),
        );
    }
    DoctorCheck::new(CHECK_NAME, CheckStatus::Ok, detail)
}

/// Report the worktree store's mount against the configured threshold.
///
/// Why: the one line in this file that touches the machine, so the verdict
/// above stays pure.
/// What: measures `repos_root` (falling back to `home` when no repos root is
/// resolved) and classifies it against the operator's `disk.max_usage_pct`, or
/// the built-in default when the key is absent.
/// Test: the pure branches are covered by `build_disk_usage_check`'s tests;
/// this wrapper is exercised by `tm doctor` itself.
pub(super) fn check_disk_usage(repos_root: Option<&Path>, home: &Path) -> DoctorCheck {
    let target = repos_root.unwrap_or(home);
    let threshold = resolve_max_usage_pct(read_configured(home)).threshold_pct;
    build_disk_usage_check(
        crate::core::disk_usage_guard::measure(target).as_ref(),
        threshold,
    )
}

#[cfg(test)]
#[path = "doctor_disk_usage_tests.rs"]
mod tests;
