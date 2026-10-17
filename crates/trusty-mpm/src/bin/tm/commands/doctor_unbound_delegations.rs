//! `tm doctor` row: delegation records of unbound sessions awaiting the stale (#9007).
//!
//! Why: #8980 made SessionEnd staling fail closed, so a session with no
//! `claude` binding keeps its delegation records until the age-based sweep or
//! `tm repair delegation`. The owner accepted that on condition `tm doctor`
//! makes it visible. The records live in the daemon's memory, so the count
//! arrives on the one `/health` sample the other daemon rows already share.
//! What: [`unbound_delegations_row`] folds that sample into a [`DoctorCheck`].
//! Zero is `Ok`; a positive count is `Warn`; every case where no count was
//! read is `Warn` or `Unknown`, never `Ok`.
//! Test: `doctor_unbound_delegations_tests.rs`.

use trusty_mpm::client::HealthSnapshot;
use trusty_mpm::core::doctor::{CheckStatus, DoctorCheck};

/// The row's name in `tm doctor` output.
pub(crate) const CHECK_NAME: &str = "unbound_delegations";

/// Render the unbound-session delegation count as a doctor row.
///
/// Why: see the module doc. `None` means no daemon answered, so nothing could
/// be read; that is not a pass.
/// What: `None` snapshot is `Warn` (the daemon row names why); a daemon that
/// predates the field, or one that reports a read error, is `Unknown` with the
/// reason; a count of 0 is `Ok`; any other count is `Warn` naming it and the remedy.
/// Test: `no_daemon_is_a_warning_not_a_pass_9007`,
/// `a_daemon_without_the_field_is_unknown_9007`,
/// `a_read_error_is_reported_never_a_pass_9007`, `zero_is_ok_9007`,
/// `a_positive_count_warns_and_names_the_remedy_9007`.
pub(crate) fn unbound_delegations_row(snapshot: Option<&HealthSnapshot>) -> DoctorCheck {
    // #9007: every arm that read no count is Warn or Unknown, never Ok.
    let Some(snapshot) = snapshot else {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Warn,
            "not checked: the delegation records live in the daemon, and none answered".to_string(),
        );
    };
    match (
        &snapshot.unbound_delegations,
        &snapshot.unbound_delegations_error,
    ) {
        (_, Some(why)) => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            format!("the daemon could not count delegation records of unbound sessions: {why}"),
        ),
        (None, None) => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            "the running daemon predates this count; restart it to read it".to_string(),
        ),
        (Some(0), None) => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            "no live delegation record belongs to an unbound session".to_string(),
        ),
        (Some(n), None) => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Warn,
            format!(
                "{n} live delegation record(s) belong to sessions with no bound claude and wait \
                 for the age-based stale sweep, because SessionEnd cannot stale them. List them \
                 with `tm repair delegation --list <dir>`; clear one with `tm repair delegation`"
            ),
        ),
    }
}

#[cfg(test)]
#[path = "doctor_unbound_delegations_tests.rs"]
mod doctor_unbound_delegations_tests;
