//! `doctor` subcommand: config / credentials health check.
//!
//! Why: OAuth onboarding fails in a handful of predictable ways (missing
//! client creds, no tokens, expired refresh). A one-shot diagnostic tells the
//! user exactly what to fix before they hit a cryptic API error.
//! What: Checks client-credential resolution and the tokens.json state, then
//! prints a checklist. Never mutates anything; read-only.
//! Test: `report_lines` builds the checklist from injected state so the output
//! is asserted without touching the environment.

use anyhow::Result;

use crate::api::auth::TokenStorage;
use crate::api::auth::oauth::resolve_client_creds;

/// Run the doctor check and print a report to stdout.
///
/// Why: Human-facing entry point for `gworkspace-mcp doctor`.
/// What: Probes credentials + storage, prints each line from [`report_lines`],
/// and returns `Ok` regardless of findings (the report *is* the result).
/// Test: Delegates to `report_lines`, which is unit-tested.
pub fn run(storage: &TokenStorage) -> Result<()> {
    let creds_ok = resolve_client_creds().is_ok();
    let accounts = storage.list_accounts().unwrap_or_default();
    let has_default = accounts.iter().any(|(_, _, d)| *d);

    for line in report_lines(creds_ok, accounts.len(), has_default) {
        println!("{line}");
    }
    Ok(())
}

/// Build the diagnostic checklist lines from resolved state.
///
/// Why: Separating formatting from I/O makes the exact wording testable and
/// keeps `run` trivial.
/// What: Returns an ordered `Vec<String>` — one line per check plus a final
/// summary hint when something is wrong.
/// Test: `report_lines_flags_missing_creds`, `report_lines_all_green`.
pub fn report_lines(creds_ok: bool, account_count: usize, has_default: bool) -> Vec<String> {
    let mut lines = vec!["gworkspace-mcp doctor".to_string(), String::new()];

    lines.push(format!("[{}] OAuth client credentials", mark(creds_ok)));
    if !creds_ok {
        lines.push(
            "      Set GOOGLE_OAUTH_CLIENT_ID / GOOGLE_OAUTH_CLIENT_SECRET, or write \
             ~/.gworkspace-mcp/oauth_client.json"
                .to_string(),
        );
    }

    lines.push(format!(
        "[{}] Authorized accounts: {account_count}",
        mark(account_count > 0)
    ));
    if account_count == 0 {
        lines.push("      Run `gworkspace-mcp setup` to authorize an account.".to_string());
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
        assert!(joined.contains("[!] OAuth client credentials"));
        assert!(joined.contains("GOOGLE_OAUTH_CLIENT_ID"));
        assert!(joined.contains("Run `gworkspace-mcp setup`"));
        assert!(joined.contains("need attention"));
    }

    #[test]
    fn report_lines_all_green() {
        let lines = report_lines(true, 2, true);
        let joined = lines.join("\n");
        assert!(joined.contains("[+] OAuth client credentials"));
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
}
