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
//! `oauth_token_check_ok_when_not_relocated`,
//! `oauth_token_check_ok_when_managed_tier_holds_login` (all in `doctor_tests.rs`).

use std::path::Path;

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
    let managed = crate::core::trusty_tools_config::managed_claude_config_dir();
    let status = crate::core::oauth_token::compute_status();
    // #10581: a login already stored in the managed tier cannot loop.
    let login = managed.as_deref().and_then(managed_tier_login);
    build_oauth_token_check(
        managed.is_some(),
        status.stored_token_present,
        status.env_var_set,
        login.as_deref(),
    )
}

/// Evidence that Claude Code already holds a login keyed to the managed
/// `CLAUDE_CONFIG_DIR` `dir` (#10581).
///
/// Why: a supervisor install runs `/login` inside the managed tier, so the
/// login lives there and no stored token is needed.
/// What: `Some(<file>)` when `<dir>/.credentials.json` is a file (the Linux
/// store), or when `<dir>/.claude.json` carries the `oauthAccount` object
/// Claude Code writes on login (the macOS secret stays in the Keychain, which
/// this never reads — #9121). `None` on absence, an unreadable file or bad
/// JSON, so the caller keeps its `Warn`.
/// Test: `oauth_token_check_ok_when_managed_tier_holds_login`.
pub(super) fn managed_tier_login(dir: &Path) -> Option<String> {
    let file = dir.join(".credentials.json");
    if file.is_file() {
        return Some(file.display().to_string());
    }
    let file = dir.join(".claude.json");
    let text = std::fs::read_to_string(&file).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("oauthAccount")
        .is_some_and(serde_json::Value::is_object)
        .then(|| format!("{} oauthAccount", file.display()))
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
/// #10581: `managed_login` (from [`managed_tier_login`]) names a login
/// already stored under the managed tier; it turns that `Warn` into `Ok`.
/// Test: `oauth_token_check_warns_when_relocated_with_no_token_or_key`,
/// `oauth_token_check_ok_when_token_stored`,
/// `oauth_token_check_ok_when_env_var_set`,
/// `oauth_token_check_warns_when_only_api_key_set`,
/// `oauth_token_check_ok_when_not_relocated`,
/// `oauth_token_check_ok_when_managed_tier_holds_login`.
pub(super) fn build_oauth_token_check(
    relocated: bool,
    stored_token_present: bool,
    env_var_set: bool,
    managed_login: Option<&str>,
) -> DoctorCheck {
    let token_available = stored_token_present || env_var_set;
    if relocated
        && !token_available
        && let Some(login) = managed_login
    {
        // #10581: the managed tier holds its own login, so it cannot loop.
        DoctorCheck::new(
            "oauth_token",
            CheckStatus::Ok,
            format!(
                "managed sessions authenticate with the login stored under the managed \
                 CLAUDE_CONFIG_DIR ({login}); no CLAUDE_CODE_OAUTH_TOKEN needed"
            ),
        )
    } else if relocated && !token_available {
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
