//! `tm doctor` log-drain probe (#6535).
//!
//! Why: the drain runs inside the daemon and writes to somebody else's bucket,
//! so nothing on screen would otherwise say whether it is on, where it points,
//! or whether the last pass worked. Its most dangerous failure is the quiet
//! one — a malformed destination or an expired credential leaves the daemon
//! running, logs accumulating locally, and no operator aware of it.
//!
//! What: one `log_drain` check built from two inputs — the config resolution
//! and the last-run record `daemon::log_drain` persists. Read-only: this probe
//! never drains, never connects, and never writes.
//!
//! Test: `tests` submodule.

use std::path::Path;

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::trusty_tools_config::{
    LogDrainConfigError, LogDrainSetting, TrustyToolsConfig, resolve_log_drain,
};

use crate::daemon::log_drain::{DrainOutcome, LogDrainStatus, load_status, state_dir};

/// Build the `log_drain` check for the real config and state directory.
///
/// Why: the thin wrapper — reading config and the status file are the only
/// impure steps, so keeping them out of [`build_log_drain_check`] makes every
/// verdict unit-testable without a home directory.
/// What: resolves the `log_drain:` section against `home` and reads
/// `<framework_root>/log-drain/status.json`.
/// Test: the verdict matrix is covered through [`build_log_drain_check`].
pub(crate) fn check_log_drain(framework_root: &Path, home: &Path) -> DoctorCheck {
    let setting = resolve_log_drain(&TrustyToolsConfig::load(), home);
    let status = load_status(&state_dir(framework_root));
    build_log_drain_check(setting.as_ref(), status.as_ref())
}

/// The pure verdict, separated for hermetic tests.
///
/// Why: five distinguishable states, each mapping to a different operator
/// action. Folding any pair together is how the fail-open case hides.
/// What:
/// - config error → `Fail`, quoting the error, because a malformed section
///   means the scheduler never started and nothing is being uploaded;
/// - disabled → `Ok`, because off is the default and a host that never
///   configured a drain is healthy;
/// - enabled but no run recorded → `Warn`. The daemon may not have reached its
///   first tick, but "enabled and never observed working" is not a healthy
///   report either;
/// - last run `Failed` → `Fail`, quoting the recorded detail;
/// - last run `Success` → `Ok` with the scheme, destination, and counts.
///
/// A last run recorded as `SkippedDisabled` under a now-enabled config reads as
/// the no-run case: the record predates the current setting.
///
/// #6657: the row names EVERY configured destination, and quotes each one's own
/// last-run outcome. One unreachable account among three is a different repair
/// from all three being down, and a row that named only the first would hide
/// the difference.
///
/// Test: `tests::config_error_fails`, `tests::disabled_is_ok`,
/// `tests::enabled_with_no_run_warns`, `tests::a_failed_run_fails`,
/// `tests::a_successful_run_is_ok`,
/// `tests::the_row_lists_every_destination_with_its_own_outcome`.
fn build_log_drain_check(
    setting: Result<&LogDrainSetting, &LogDrainConfigError>,
    status: Option<&LogDrainStatus>,
) -> DoctorCheck {
    let plan = match setting {
        Err(e) => {
            return DoctorCheck::new(
                "log_drain",
                CheckStatus::Fail,
                format!(
                    "`log_drain:` in ~/.trusty-tools/trusty-mpm/config.yaml is invalid, so the \
                     drain never started — {e}"
                ),
            );
        }
        Ok(LogDrainSetting::Disabled) => {
            return DoctorCheck::new(
                "log_drain",
                CheckStatus::Ok,
                "log drain disabled (no `log_drain:` section, or `enabled: false`)",
            );
        }
        Ok(LogDrainSetting::Enabled(plan)) => plan,
    };

    let where_to = plan
        .destinations
        .iter()
        .map(|group| format!("{} → {}", group.scheme(), group.destination_display))
        .collect::<Vec<_>>()
        .join(", ");
    let Some(status) = status.filter(|s| s.outcome != DrainOutcome::SkippedDisabled) else {
        return DoctorCheck::new(
            "log_drain",
            CheckStatus::Warn,
            format!("log drain enabled ({where_to}) but no run has been recorded yet"),
        );
    };
    let outcomes = recorded_outcomes(status);

    match status.outcome {
        DrainOutcome::Success => DoctorCheck::new(
            "log_drain",
            CheckStatus::Ok,
            format!(
                "log drain enabled ({where_to}); last run {} — {outcomes}",
                status.at
            ),
        ),
        // #6535: an errored pass must never read as drained. See
        // `daemon::log_drain`'s module docs on the three-state mapping.
        DrainOutcome::Failed => DoctorCheck::new(
            "log_drain",
            CheckStatus::Fail,
            format!(
                "log drain enabled ({where_to}) but the last run FAILED at {} — {outcomes}",
                status.at
            ),
        ),
        // Filtered out above; kept total rather than `unreachable!`.
        DrainOutcome::SkippedDisabled => DoctorCheck::new(
            "log_drain",
            CheckStatus::Warn,
            format!("log drain enabled ({where_to}) but no run has been recorded yet"),
        ),
    }
}

/// Render each recorded destination's own outcome, one clause apiece (#6657).
///
/// A record written before #6657 carries no per-destination breakdown, so its
/// single `detail` line is used unchanged rather than reported as "no
/// destinations ran".
fn recorded_outcomes(status: &LogDrainStatus) -> String {
    if status.destinations.is_empty() {
        return status.detail.clone();
    }
    status
        .destinations
        .iter()
        .map(|d| {
            let verdict = match d.outcome {
                DrainOutcome::Success => "ok",
                DrainOutcome::Failed => "FAILED",
                DrainOutcome::SkippedDisabled => "skipped",
            };
            format!("{} → {}: {verdict} — {}", d.scheme, d.destination, d.detail)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
#[path = "doctor_log_drain_tests.rs"]
mod tests;
