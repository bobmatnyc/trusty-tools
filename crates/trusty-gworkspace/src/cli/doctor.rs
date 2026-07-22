//! `doctor` subcommand: config / credentials health check + live token probe.
//!
//! Why: OAuth onboarding fails in a handful of predictable ways (missing
//! client creds, no tokens, an expired/revoked refresh token). A one-shot
//! diagnostic tells the user exactly what to fix — and, crucially, for a dead
//! refresh token it names the exact `trusty-gworkspace-mcp setup --profile <name>`
//! command to run — before they hit a cryptic API error mid-session.
//! What: Checks client-credential resolution and tokens.json state (read-only),
//! then, when credentials are available, live-probes each stored profile's
//! refresh token against Google's token endpoint (bounded per-profile timeout,
//! no persistence) and reports OK / DEAD / UNKNOWN per profile.
//! Test: `report_lines` (static checks) and `health_lines` (per-profile
//! classification) build their output from injected state so the exact wording
//! is asserted without touching the network. See `report_lines_flags_missing_creds`,
//! `report_lines_all_green`, `single_account_counts_as_default`,
//! `health_lines_ok_shows_email`, `health_lines_dead_names_setup_command`,
//! `health_lines_unknown_is_graceful`.

use std::time::Duration;

use anyhow::Result;

use crate::api::auth::TokenStorage;
use crate::api::auth::oauth::errors::is_invalid_grant;
use crate::api::auth::oauth::flow::ClientCreds;
use crate::api::auth::oauth::{
    profile_client_source, resolve_client_creds, resolve_client_creds_for_profile,
};
use crate::api::constants::OAUTH_TOKEN_URL;

/// Per-profile refresh-token health, as classified by the live probe.
///
/// Why: The doctor must distinguish a genuinely dead refresh token (actionable:
/// re-auth) from a transient network failure (not actionable: try again later),
/// and never hard-fail the whole diagnostic because Google was unreachable.
/// What: `Live` — the refresh token minted a fresh access token (with its
/// lifetime); `Dead` — Google returned `invalid_grant`; `Unknown` — offline,
/// timed out, or an unexpected non-`invalid_grant` error.
/// Test: `health_lines_ok_shows_email`, `health_lines_dead_names_setup_command`,
/// `health_lines_unknown_is_graceful`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeResult {
    /// Refresh succeeded; the new access token lives `expires_in` seconds.
    Live { expires_in: Option<i64> },
    /// Google rejected the refresh token as expired or revoked.
    Dead,
    /// Could not determine validity (offline, timeout, or unexpected error).
    Unknown,
}

/// Per-profile probe timeout. Bounds each network check so `doctor` stays
/// responsive even with many profiles or a flaky connection.
const PROBE_TIMEOUT: Duration = Duration::from_secs(6);

/// Run the doctor check and print a report to stdout.
///
/// Why: Human-facing entry point for `trusty-gworkspace-mcp doctor`.
/// What: Prints the static credential/storage checks, then live-probes each
/// stored profile using ITS OWN resolved OAuth client (issue #3518:
/// per-profile client if configured, else the global one — see
/// `resolve_client_creds_for_profile`) and prints its health, labeled with
/// which client it used. Always returns `Ok` (the report *is* the result);
/// never mutates tokens.json.
/// Test: Delegates to `report_lines` and `health_lines`, both unit-tested.
pub async fn run(storage: &TokenStorage) -> Result<()> {
    let global_creds_ok = resolve_client_creds().is_ok();
    let accounts = storage.list_accounts().unwrap_or_default();
    let has_default = accounts.iter().any(|(_, _, d)| *d);

    for line in report_lines(global_creds_ok, accounts.len(), has_default) {
        println!("{line}");
    }

    if accounts.is_empty() {
        return Ok(());
    }

    println!();
    println!("Per-profile refresh-token health:");
    // Never fall back to `Client::default()` on builder failure: that client
    // has no request timeout, so a hung endpoint would stall doctor
    // indefinitely. If the bounded client can't be built, report every
    // profile as Unknown rather than probe without a timeout.
    match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(http) => {
            for (name, email, _is_default) in &accounts {
                let client_label = profile_client_source(name).label();
                let result = match resolve_client_creds_for_profile(name) {
                    Ok(creds) => probe_profile(&http, storage, &creds, name).await,
                    Err(_) => ProbeResult::Unknown,
                };
                for line in health_lines(name, email.as_deref(), &client_label, &result) {
                    println!("{line}");
                }
            }
        }
        Err(e) => {
            eprintln!("      Could not build a bounded HTTP client: {e}");
            for (name, email, _is_default) in &accounts {
                let client_label = profile_client_source(name).label();
                for line in
                    health_lines(name, email.as_deref(), &client_label, &ProbeResult::Unknown)
                {
                    println!("{line}");
                }
            }
        }
    }

    Ok(())
}

/// Live-probe a single profile's refresh token without persisting anything.
///
/// Why: The durable credential is the refresh token; validating it (by minting
/// a throwaway access token) is the only reliable "is this account still
/// usable?" check. Doctor is read-only, so this deliberately does NOT call
/// `OAuthManager::refresh` (which would rewrite tokens.json).
/// What: Loads the stored refresh token, POSTs `grant_type=refresh_token`, and
/// classifies the outcome into a [`ProbeResult`]. Any network/timeout error or
/// missing refresh token becomes `Unknown`; `invalid_grant` becomes `Dead`.
/// Test: Network path is integration-only; the classification wording is
/// covered via `health_lines_*`.
async fn probe_profile(
    http: &reqwest::Client,
    storage: &TokenStorage,
    creds: &ClientCreds,
    profile: &str,
) -> ProbeResult {
    let refresh_token = match storage.get_profile(profile) {
        Ok(Some(stored)) => match stored.token.refresh_token {
            Some(rt) => rt,
            None => return ProbeResult::Unknown,
        },
        _ => return ProbeResult::Unknown,
    };

    let params = [
        ("client_id", creds.client_id.as_str()),
        ("client_secret", creds.client_secret.as_str()),
        ("refresh_token", refresh_token.as_str()),
        ("grant_type", "refresh_token"),
    ];
    let resp = match http.post(OAUTH_TOKEN_URL).form(&params).send().await {
        Ok(r) => r,
        Err(_) => return ProbeResult::Unknown,
    };
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if status.is_success() {
        let expires_in = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("expires_in").and_then(|x| x.as_i64()));
        ProbeResult::Live { expires_in }
    } else if is_invalid_grant(&body) {
        ProbeResult::Dead
    } else {
        ProbeResult::Unknown
    }
}

/// Build the static (offline) diagnostic checklist lines from resolved state.
///
/// Why: Separating formatting from I/O makes the exact wording testable and
/// keeps `run` trivial.
/// What: Returns an ordered `Vec<String>` — one line per check plus a final
/// summary hint when something is wrong.
/// Test: `report_lines_flags_missing_creds`, `report_lines_all_green`.
pub fn report_lines(creds_ok: bool, account_count: usize, has_default: bool) -> Vec<String> {
    let mut lines = vec!["trusty-gworkspace-mcp doctor".to_string(), String::new()];

    lines.push(format!(
        "[{}] Global OAuth client credentials",
        mark(creds_ok)
    ));
    if !creds_ok {
        lines.push(
            "      Set GOOGLE_OAUTH_CLIENT_ID / GOOGLE_OAUTH_CLIENT_SECRET, or write \
             ~/.gworkspace-mcp/oauth_client.json (a profile with its own per-profile client, \
             see `setup --oauth-client`, does not need this)."
                .to_string(),
        );
    }

    lines.push(format!(
        "[{}] Authorized accounts: {account_count}",
        mark(account_count > 0)
    ));
    if account_count == 0 {
        lines.push("      Run `trusty-gworkspace-mcp setup` to authorize an account.".to_string());
    }

    lines.push(format!(
        "[{}] Default profile selected",
        mark(has_default || account_count == 1)
    ));

    let all_ok = creds_ok && account_count > 0;
    lines.push(String::new());
    lines.push(if all_ok {
        "All checks passed.".to_string()
    } else {
        "Some checks need attention (see above).".to_string()
    });
    lines
}

/// Build the per-profile health lines from a probe result.
///
/// Why: The live-probe verdict must render identically whether it came from a
/// real network call or a test — and a `Dead` profile must always print the
/// exact re-auth command so the fix is copy-pasteable. Issue #3518: each line
/// also names which OAuth client (`global` or `per-profile (<path>)`) the
/// profile actually used, so a misconfigured per-profile client is
/// diagnosable straight from `doctor` output.
/// What: `Live` → one OK line (email + access-token lifetime + client);
/// `Dead` → a failure line plus the `trusty-gworkspace-mcp setup --profile
/// <name>` command; `Unknown` → one graceful "could not determine" line
/// (never fatal).
/// Test: `health_lines_ok_shows_email`, `health_lines_dead_names_setup_command`,
/// `health_lines_unknown_is_graceful`.
pub fn health_lines(
    profile: &str,
    email: Option<&str>,
    client_label: &str,
    result: &ProbeResult,
) -> Vec<String> {
    let who = email.unwrap_or("email unknown");
    match result {
        ProbeResult::Live { expires_in } => {
            let expiry = match expires_in {
                Some(secs) => format!("access token valid ~{secs}s"),
                None => "access token valid".to_string(),
            };
            vec![format!(
                "[+] {profile}: OK ({who}, {expiry}, client: {client_label})"
            )]
        }
        ProbeResult::Dead => vec![
            format!(
                "[!] {profile}: DEAD — refresh token for {who} is expired or revoked \
                 (client: {client_label})"
            ),
            format!("      re-authenticate with: trusty-gworkspace-mcp setup --profile {profile}"),
        ],
        ProbeResult::Unknown => vec![format!(
            "[?] {profile}: UNKNOWN — could not reach Google to verify ({who}, client: \
             {client_label}); check your connection"
        )],
    }
}

/// Render a pass/fail check mark.
fn mark(ok: bool) -> char {
    if ok { '+' } else { '!' }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_lines_flags_missing_creds() {
        let lines = report_lines(false, 0, false);
        let joined = lines.join("\n");
        assert!(joined.contains("[!] Global OAuth client credentials"));
        assert!(joined.contains("GOOGLE_OAUTH_CLIENT_ID"));
        assert!(joined.contains("Run `trusty-gworkspace-mcp setup`"));
        assert!(joined.contains("need attention"));
    }

    #[test]
    fn report_lines_all_green() {
        let lines = report_lines(true, 2, true);
        let joined = lines.join("\n");
        assert!(joined.contains("[+] Global OAuth client credentials"));
        assert!(joined.contains("Authorized accounts: 2"));
        assert!(joined.contains("All checks passed."));
        assert!(!joined.contains("need attention"));
    }

    #[test]
    fn single_account_counts_as_default() {
        let lines = report_lines(true, 1, false);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("[+] Default profile selected"))
        );
    }

    #[test]
    fn health_lines_ok_shows_email() {
        let lines = health_lines(
            "work",
            Some("user@example.com"),
            "global",
            &ProbeResult::Live {
                expires_in: Some(3599),
            },
        );
        let joined = lines.join("\n");
        assert!(joined.contains("[+] work: OK"), "OK marker: {joined}");
        assert!(joined.contains("user@example.com"), "email shown: {joined}");
        assert!(joined.contains("3599s"), "expiry shown: {joined}");
        assert!(joined.contains("client: global"), "client shown: {joined}");
        assert!(
            !joined.contains("setup --profile"),
            "a healthy profile must not suggest re-auth: {joined}"
        );
    }

    #[test]
    fn health_lines_dead_names_setup_command() {
        let lines = health_lines(
            "work",
            Some("user@example.com"),
            "per-profile (/home/u/.gworkspace-mcp/clients/work.json)",
            &ProbeResult::Dead,
        );
        let joined = lines.join("\n");
        assert!(joined.contains("DEAD"), "dead marker: {joined}");
        assert!(joined.contains("expired or revoked"), "cause: {joined}");
        assert!(
            joined.contains("client: per-profile ("),
            "per-profile client label shown: {joined}"
        );
        assert!(
            joined.contains("trusty-gworkspace-mcp setup --profile work"),
            "must name the exact re-auth command for the dead profile: {joined}"
        );
    }

    #[test]
    fn health_lines_unknown_is_graceful() {
        let lines = health_lines("work", None, "global", &ProbeResult::Unknown);
        let joined = lines.join("\n");
        assert!(joined.contains("UNKNOWN"), "unknown marker: {joined}");
        assert!(
            joined.contains("could not reach Google"),
            "graceful offline wording: {joined}"
        );
        assert!(
            !joined.contains("DEAD"),
            "unreachable must not be misreported as dead: {joined}"
        );
        assert!(
            !joined.contains("setup --profile"),
            "unknown must not push a re-auth (token may be fine): {joined}"
        );
    }
}
