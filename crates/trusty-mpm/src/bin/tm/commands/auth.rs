//! `tm auth` command handlers — manage the tm-managed `CLAUDE_CODE_OAUTH_TOKEN`
//! store (issue #2246).
//!
//! Why: managed sessions run `claude` under a relocated `CLAUDE_CONFIG_DIR`,
//! whose macOS-Keychain credential entry (keyed by a hash of the dir) diverges
//! from the operator's default login, causing a `/login` "success then
//! immediately not-logged-in" loop. Storing a `CLAUDE_CODE_OAUTH_TOKEN` here
//! and letting the daemon inject it (see `trusty_mpm::runtime::claude_code`)
//! bypasses the Keychain entirely. This file is thin I/O + printing over
//! `trusty_mpm::core::oauth_token`, which owns every actual read/write.
//! What: `set_token`, `clear_token`, `status` — one handler per
//! [`crate::cli::AuthAction`] variant.
//! Test: `set_token_reads_from_explicit_arg`, `set_token_reads_from_stdin`,
//! `status_lines_never_contain_a_token_value`.

use trusty_mpm::core::oauth_token::{self, TokenStatus};

/// Read a token from stdin, trimmed; errors on a blank read.
///
/// Why: the default (no `--token`) path for `tm auth set-token` — keeps the
/// token out of shell history (`claude setup-token | tm auth set-token`).
/// What: reads all of stdin to EOF and trims surrounding whitespace/newlines.
/// Test: `read_token_from_reader_trims_and_rejects_blank`.
fn read_token_from_reader(mut r: impl std::io::Read) -> anyhow::Result<String> {
    let mut buf = String::new();
    r.read_to_string(&mut buf)?;
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        anyhow::bail!(
            "no token provided on stdin — pipe the token (e.g. `claude setup-token | tm auth \
             set-token`) or pass --token <val>"
        );
    }
    Ok(trimmed.to_string())
}

/// `tm auth set-token` handler.
///
/// Why: the single write path for the store — see [`crate::cli::AuthAction::SetToken`].
/// What: uses `token` when given, else reads stdin to EOF; writes it via
/// [`oauth_token::set_token`] (0600) and prints the path (never the value).
/// Test: exercised via `cli_parses_auth_set_token` (parsing) and
/// `oauth_token`'s own round-trip tests (the underlying store).
pub(crate) fn set_token(token: Option<String>, _stdin: bool) -> anyhow::Result<()> {
    let raw = match token {
        Some(t) => t,
        None => read_token_from_reader(std::io::stdin())?,
    };
    let path = oauth_token::set_token(&raw)?;
    println!("tm auth: token stored at {} (mode 0600)", path.display());
    Ok(())
}

/// `tm auth clear-token` handler.
///
/// Why: lets an operator roll back to ambient auth or rotate tokens.
/// What: removes the stored token via [`oauth_token::clear_token`]; a missing
/// file is not an error.
/// Test: exercised via `oauth_token::clear_token_at`'s tests (underlying I/O);
/// this wrapper only adds the print.
pub(crate) fn clear_token() -> anyhow::Result<()> {
    let path = oauth_token::clear_token()?;
    println!("tm auth: token cleared ({})", path.display());
    Ok(())
}

/// Render a [`TokenStatus`] into printable lines, never including the token
/// value (the struct has no field capable of carrying it).
///
/// Why: separating rendering from I/O makes the "never prints the token"
/// guarantee directly testable against arbitrary status values.
/// What: one line per signal, plus an indented path line when a path was
/// probed (present or not, so the operator knows exactly what was checked).
/// Test: `status_lines_never_contain_a_token_value`,
/// `status_lines_report_every_signal`.
fn format_status_lines(status: &TokenStatus) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(format!(
        "stored token: {}",
        if status.stored_token_present {
            "present"
        } else {
            "absent"
        }
    ));
    if let Some(p) = &status.token_path {
        lines.push(format!("  path: {}", p.display()));
    }
    lines.push(format!(
        "{}: {}",
        oauth_token::OAUTH_TOKEN_ENV_VAR,
        if status.env_var_set { "set" } else { "not set" }
    ));
    lines.push(format!(
        "ANTHROPIC_API_KEY: {}",
        if status.anthropic_api_key_set {
            "set"
        } else {
            "not set"
        }
    ));
    lines.push(format!(
        "credentials file: {}",
        if status.credentials_file_present {
            "present"
        } else {
            "absent (this is normal on macOS, which uses the Keychain instead)"
        }
    ));
    if let Some(p) = &status.credentials_path {
        lines.push(format!("  path: {}", p.display()));
    }
    lines
}

/// `tm auth status` handler.
///
/// Why: the fastest way to check whether a managed session risks the #2246
/// login loop before it happens.
/// What: computes [`oauth_token::compute_status`] and prints
/// [`format_status_lines`]. NEVER prints the token value.
/// Test: `status_lines_never_contain_a_token_value` covers the print
/// contract; `oauth_token::compute_status` is tested in its own module.
pub(crate) fn status() -> anyhow::Result<()> {
    println!("tm auth status");
    for line in format_status_lines(&oauth_token::compute_status()) {
        println!("  {line}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn read_token_from_reader_trims_and_rejects_blank() {
        let token = read_token_from_reader("  sk-ant-oat01-fake-token\n".as_bytes()).unwrap();
        assert_eq!(token, "sk-ant-oat01-fake-token");

        let err = read_token_from_reader("   \n".as_bytes());
        assert!(err.is_err(), "blank stdin must be rejected");
    }

    #[test]
    fn status_lines_never_contain_a_token_value() {
        // A status built as if a token WERE present (and stored/env paths
        // resolved) must never leak the fake token value it stands in for —
        // structurally guaranteed, since TokenStatus carries no such field.
        let status = TokenStatus {
            stored_token_present: true,
            token_path: Some(PathBuf::from(
                "/home/bob/.trusty-tools/trusty-mpm/claude-code-oauth.token",
            )),
            env_var_set: true,
            anthropic_api_key_set: false,
            credentials_file_present: true,
            credentials_path: Some(PathBuf::from("/home/bob/.claude/.credentials.json")),
        };
        let lines = format_status_lines(&status);
        let rendered = lines.join("\n");
        assert!(
            !rendered.contains("sk-ant-oat01-fake-token"),
            "status output must never contain a token value: {rendered}"
        );
        assert!(rendered.contains("present"));
        assert!(rendered.contains("claude-code-oauth.token"));
    }

    #[test]
    fn status_lines_report_every_signal() {
        let status = TokenStatus {
            stored_token_present: false,
            token_path: None,
            env_var_set: false,
            anthropic_api_key_set: false,
            credentials_file_present: false,
            credentials_path: None,
        };
        let rendered = format_status_lines(&status).join("\n");
        assert!(rendered.contains("stored token: absent"));
        assert!(rendered.contains("CLAUDE_CODE_OAUTH_TOKEN: not set"));
        assert!(rendered.contains("ANTHROPIC_API_KEY: not set"));
        assert!(rendered.contains("credentials file: absent"));
    }
}
