//! `tm doctor` row: can the daemon's resolver actually reach its credentials?
//!
//! Why (#8236 item 8): moving a credential out of a plaintext plist and into
//! the Keychain trades one failure mode for another. A launchd-spawned binary
//! rebuilt by `cargo install` has a NEW cdhash, so its first Keychain read
//! raises a SecurityAgent approval dialog; a measured run took 12 s with a
//! human clicking and timed out when nobody did. With no diagnostic for that,
//! the only symptom is "the overseer stopped working after an update" with
//! nothing in `tm doctor` to explain it. This row explains it.
//!
//! What: [`check_credential_reach`] asks
//! [`trusty_common::credentials::resolve_env_var_bounded`] for each credential
//! the daemon consumes and reports PRESENT / ABSENT / the store's error KIND /
//! TIMED OUT. It never prints a value, and it is bounded by the same constant
//! the daemon uses, so running `tm doctor` can never hang on a dialog either.
//!
//! Test: `doctor_credential_reach_tests.rs`.

use trusty_common::credential_registry::env_var_for;
use trusty_common::credentials::{SecretResolveError, resolve_env_var_bounded};

use crate::core::doctor::{CheckStatus, DoctorCheck};

/// The `tm doctor` row name.
const CHECK_NAME: &str = "credential_reach";

/// Provider keys the daemon itself resolves at runtime.
///
/// Why: not the whole registry — a row that reported every registered provider
/// absent would be noise on every host. These are the four the daemon's own
/// code paths read: the LLM overseer and session-manager inference
/// (`openrouter`, `anthropic`) and the chat channels (`telegram`, `slack`).
/// Test: `row_reports_one_line_per_daemon_credential`.
const DAEMON_PROVIDERS: &[&str] = &["openrouter", "anthropic", "telegram", "slack"];

/// One credential's reachability, as a phrase for the row.
///
/// Why: separated from the row so every arm is testable without a store.
/// What: the variable name plus a verdict word. Never a value.
/// Test: `a_timeout_is_reported_as_a_waiting_dialog`,
/// `an_absent_credential_is_not_an_error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachVerdict {
    /// The canonical environment-variable name.
    pub var: String,
    /// What the resolver answered. Never a value.
    pub verdict: String,
    /// True when the daemon would have the credential disabled.
    pub degraded: bool,
    /// True when a human approval is the likely remedy.
    pub needs_approval: bool,
}

/// Describe one resolver outcome without disclosing it.
///
/// Why (#8236 item 8d): after every `cargo install` the EXPECTED state is a
/// timeout, because the rebuilt binary's first Keychain read waits on a
/// dialog. An operator who cannot tell that apart from "absent" restores the
/// plaintext plist entry, which is the defect coming back.
/// What: one phrase per [`SecretResolveError`] arm, plus the success arm.
/// Test: `a_timeout_is_reported_as_a_waiting_dialog`,
/// `an_absent_credential_is_not_an_error`,
/// `a_store_error_names_its_kind`,
/// `a_present_credential_is_never_printed`,
/// `an_unregistered_name_is_reported_as_unresolvable`.
#[must_use]
pub fn verdict_for(var: &str, outcome: &Result<String, SecretResolveError>) -> ReachVerdict {
    let (verdict, degraded, needs_approval) = match outcome {
        Ok(_) => ("present".to_string(), false, false),
        Err(SecretResolveError::Absent { .. }) => (
            "absent (nothing in the process environment, `.env.local`, or the credential store)"
                .to_string(),
            true,
            false,
        ),
        Err(SecretResolveError::Unregistered { .. }) => (
            "not a registered credential name — it cannot be resolved at all".to_string(),
            true,
            false,
        ),
        Err(SecretResolveError::Timeout { waited_ms, .. }) => (
            format!(
                "timed out after {waited_ms} ms waiting for the credential store — a macOS \
                 Keychain approval dialog may be waiting on screen. This is the expected state \
                 after a `cargo install` rebuild: approve it once (or re-run `tm doctor` from a \
                 terminal and approve), and every later read is immediate"
            ),
            true,
            true,
        ),
        Err(SecretResolveError::Store { kind, .. }) => (
            format!("the credential store could not supply it ({kind})"),
            true,
            matches!(kind, trusty_common::credentials::StoreErrorKind::Keyring),
        ),
        // `SecretResolveError` is `#[non_exhaustive]`. A variant added in
        // trusty-common later has to land here DEGRADED — a row that read a new
        // failure as reachable would be the fail-open this check exists to
        // close. Its `kind()` is a value-free label, so this cannot leak one.
        Err(other) => (format!("unreachable ({})", other.kind()), true, false),
    };
    ReachVerdict {
        var: var.to_string(),
        verdict,
        degraded,
        needs_approval,
    }
}

/// The `credential_reach` doctor row.
///
/// Why/What: see the module docs.
/// `Warn` when a credential the daemon uses is unreachable, `Unknown` when one
/// timed out (the answer is genuinely not known — the read may still land),
/// `Ok` when every one resolves. Never `Fail`: a host that deliberately runs
/// without Telegram is not broken.
/// Test: `row_reports_one_line_per_daemon_credential`,
/// `row_is_unknown_when_a_credential_times_out`.
pub(crate) fn check_credential_reach() -> DoctorCheck {
    // Runs on the blocking pool — see `check_credential_reach_async`, the only
    // caller on an async path. Nothing here may be awaited.
    let verdicts: Vec<ReachVerdict> = DAEMON_PROVIDERS
        .iter()
        .map(|provider| {
            let var = env_var_for(provider).unwrap_or(provider);
            verdict_for(var, &resolve_env_var_bounded(var))
        })
        .collect();
    build_row(&verdicts)
}

/// [`check_credential_reach`], off the async runtime.
///
/// Why (#8236): the row is reached by `GET /api/v1/doctor`, and each of the
/// four providers can park on a `Condvar::wait_timeout` for
/// `STORE_READ_TIMEOUT` — up to ~12 s of a tokio worker thread held by a
/// SecurityAgent dialog, which is the failure this row exists to diagnose.
/// What: `spawn_blocking`, with a join failure (the probe panicked, or the
/// runtime is shutting down) reported as `Unknown` — a row that did not run has
/// not shown the daemon healthy.
/// Test: `the_row_is_produced_off_the_runtime_thread`,
/// `a_probe_that_panics_is_unknown_not_a_lost_row`.
pub(crate) async fn check_credential_reach_async() -> DoctorCheck {
    off_runtime(check_credential_reach).await
}

/// Run a blocking doctor probe on the blocking pool.
///
/// Why: separated from the probe so a test can pass one that REPORTS the thread
/// it ran on, which is the only way to prove the move actually happened.
/// Test: `the_row_is_produced_off_the_runtime_thread`,
/// `a_probe_that_panics_is_unknown_not_a_lost_row`.
async fn off_runtime<F>(probe: F) -> DoctorCheck
where
    F: FnOnce() -> DoctorCheck + Send + 'static,
{
    match tokio::task::spawn_blocking(probe).await {
        Ok(check) => check,
        Err(e) => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            format!(
                "the credential-reach probe did not run ({e}) — whether the daemon can \
                 reach its credentials is UNKNOWN"
            ),
        ),
    }
}

/// The pure verdict behind [`check_credential_reach`].
///
/// Test: `row_reports_one_line_per_daemon_credential`,
/// `row_is_unknown_when_a_credential_times_out`,
/// `row_is_ok_when_every_credential_resolves`.
pub(crate) fn build_row(verdicts: &[ReachVerdict]) -> DoctorCheck {
    let detail = verdicts
        .iter()
        .map(|v| format!("{}: {}", v.var, v.verdict))
        .collect::<Vec<_>>()
        .join("; ");

    let status = if verdicts.iter().any(|v| v.needs_approval) {
        CheckStatus::Unknown
    } else if verdicts.iter().any(|v| v.degraded) {
        CheckStatus::Warn
    } else {
        CheckStatus::Ok
    };
    DoctorCheck::new(CHECK_NAME, status, detail)
}

#[cfg(test)]
#[path = "doctor_credential_reach_tests.rs"]
mod tests;
