//! `tm auth` CLAUDE_CODE_OAUTH_TOKEN store command group (issue #2246).
//!
//! Why: extracted from `cli.rs` (issue #2603) to keep the top-level file
//! under the 500-SLOC production cap.
//! What: [`AuthAction`] — `set-token`/`clear-token`/`status`.
//! Test: `cli_parses_auth_*` in `tests.rs`.

use clap::Subcommand;

/// Actions for the `auth` subcommand (issue #2246).
///
/// Why: scoped sub-actions keep the token lifecycle (store/remove/inspect)
/// under one command group without cluttering the top-level CLI surface.
/// What: `SetToken`, `ClearToken`, `Status` — see [`Command::Auth`]'s doc
/// comment for the full rationale.
/// Test: `cli_parses_auth_set_token`, `cli_parses_auth_set_token_stdin`,
/// `cli_parses_auth_clear_token`, `cli_parses_auth_status`.
#[derive(Debug, Subcommand)]
pub(crate) enum AuthAction {
    /// Store a `CLAUDE_CODE_OAUTH_TOKEN` for managed sessions to use.
    ///
    /// Why: `--token <val>` is convenient for scripting but lands the token in
    /// shell history; the default (no `--token`) reads it from stdin instead
    /// (e.g. `claude setup-token | tm auth set-token`), so this is the safer
    /// path for interactive use.
    /// What: reads the token from `--token` when given, else from stdin
    /// (trimmed of surrounding whitespace/newline), and writes it to
    /// `~/.trusty-tools/trusty-mpm/claude-code-oauth.token` with mode 0600.
    /// Test: `cli_parses_auth_set_token`, `cli_parses_auth_set_token_stdin`.
    SetToken {
        /// The token value. When omitted, read from stdin instead.
        #[arg(long)]
        token: Option<String>,
        /// Explicitly read the token from stdin (the default when `--token`
        /// is omitted; accepted for clarity in scripts/docs).
        #[arg(long)]
        stdin: bool,
    },
    /// Remove the stored token, if present.
    ///
    /// Why: lets an operator roll back to ambient Keychain/`ANTHROPIC_API_KEY`
    /// auth, or rotate to a freshly generated token via a subsequent
    /// `set-token`.
    /// What: deletes the stored token file; a no-op (not an error) when no
    /// token is stored.
    /// Test: `cli_parses_auth_clear_token`.
    ClearToken,
    /// Report the current auth configuration without printing any secret.
    ///
    /// Why: the fastest way for an operator (or `tm doctor`) to see whether a
    /// managed session is at risk of the `CLAUDE_CONFIG_DIR` login loop
    /// (#2246) before it happens.
    /// What: prints presence/absence of the stored token, the
    /// `CLAUDE_CODE_OAUTH_TOKEN`/`ANTHROPIC_API_KEY` env vars, and a
    /// best-effort on-disk credentials check. NEVER prints the token value.
    /// Test: `cli_parses_auth_status`.
    Status,
}
