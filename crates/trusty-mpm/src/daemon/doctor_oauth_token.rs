//! `tm doctor` managed-session OAuth-token probe (#2246).
//!
//! Why: split out of `doctor.rs` (#7422) to bring that file back under the
//! 500-SLOC production cap when the `session_scope` check was added, following
//! the sibling-`doctor_*.rs` pattern every other check already uses. Purely
//! mechanical — no behaviour or assertion changes.
//! What: [`check_oauth_token_config`] (reads the ambient state) and
//! [`build_oauth_token_check`] (the pure verdict).
//! Test: `oauth_token_check_warns_when_relocated_with_no_token_or_key`,
//! `oauth_token_check_ok_when_token_stored`,
//! `oauth_token_check_ok_when_env_var_set`,
//! `oauth_token_check_warns_when_only_api_key_set`,
//! `oauth_token_check_ok_when_not_relocated` (all in `doctor_tests.rs`).

use crate::core::doctor::{CheckStatus, DoctorCheck};

/// Probe for the `CLAUDE_CONFIG_DIR`-keyed OAuth login-loop risk (issue #2246).
///
/// Why: every managed spawn relocates `CLAUDE_CONFIG_DIR` to the tm-owned
/// config home; on macOS the Keychain credential Claude Code reads is keyed by
/// a hash of that path, so a `/login` run inside a managed session can
/// diverge from the login stored under the operator's default config dir and
/// loop between "login successful" and "not logged in". Storing a
/// `CLAUDE_CODE_OAUTH_TOKEN` (`tm auth set-token`) bypasses the Keychain
/// entirely — this probe warns BEFORE an operator hits the loop rather than
/// after.
/// What: `Warn` when the managed config dir resolves (true in virtually every
/// real environment — see [`crate::core::trusty_tools_config::managed_claude_config_dir`])
/// AND no OAuth token will resolve for a managed spawn — i.e. neither a stored
/// token file NOR the `CLAUDE_CODE_OAUTH_TOKEN` env var is set (the two sources
/// [`crate::core::oauth_token::resolve_oauth_token`] consults, via
/// [`crate::core::oauth_token::compute_status`]); `Ok` otherwise. Ambient
/// `ANTHROPIC_API_KEY` deliberately does NOT count — every managed spawn strips
/// it with `env -u ANTHROPIC_API_KEY` (see [`crate::runtime::claude_code`]), so
/// relying on it still triggers the #2246 login loop while doctor would
/// otherwise falsely report `Ok`. Never `Fail` — a first `/login` still
/// frequently succeeds even without a stored token, so this is advisory.
/// Test: `oauth_token_check_warns_when_relocated_with_no_token_or_key`,
/// `oauth_token_check_ok_when_token_stored`,
/// `oauth_token_check_warns_when_only_api_key_set`.
pub(super) fn check_oauth_token_config() -> DoctorCheck {
    let relocated = crate::core::trusty_tools_config::managed_claude_config_dir().is_some();
    let status = crate::core::oauth_token::compute_status();
    build_oauth_token_check(relocated, status.stored_token_present, status.env_var_set)
}

/// Pure verdict for [`check_oauth_token_config`], separated for hermetic
/// testing without mutating real process env / home state.
///
/// Why: keeping the branch logic pure makes both the warn and ok paths
/// unit-testable without redirecting `HOME` or the process env.
/// What: `token_available = stored_token_present || env_var_set` — exactly the
/// two sources a managed spawn's [`crate::core::oauth_token::resolve_oauth_token`]
/// consults. `Warn` when `relocated && !token_available`, else `Ok`. Ambient
/// `ANTHROPIC_API_KEY` is intentionally NOT an input: managed spawns scrub it,
/// so it can never satisfy the managed-session auth requirement (#2246 doctor
/// false-negative fix).
/// Test: `oauth_token_check_warns_when_relocated_with_no_token_or_key`,
/// `oauth_token_check_ok_when_token_stored`,
/// `oauth_token_check_ok_when_env_var_set`,
/// `oauth_token_check_warns_when_only_api_key_set`,
/// `oauth_token_check_ok_when_not_relocated`.
pub(super) fn build_oauth_token_check(
    relocated: bool,
    stored_token_present: bool,
    env_var_set: bool,
) -> DoctorCheck {
    let token_available = stored_token_present || env_var_set;
    if relocated && !token_available {
        DoctorCheck::new(
            "oauth_token",
            CheckStatus::Warn,
            "managed sessions relocate CLAUDE_CONFIG_DIR but no CLAUDE_CODE_OAUTH_TOKEN is \
             configured — a `/login` inside a managed session may loop between \"successful\" \
             and \"not logged in\" (issue #2246). Note an ambient ANTHROPIC_API_KEY does NOT \
             help: managed spawns strip it. Run `claude setup-token` then `tm auth set-token`",
        )
    } else {
        DoctorCheck::new(
            "oauth_token",
            CheckStatus::Ok,
            "managed-session auth looks configured",
        )
    }
}
