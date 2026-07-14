//! Command-line interface for the `gworkspace-mcp` binary.
//!
//! Why: The binary was stdio-only (no arg parsing) and relied on the Python
//! CLI for onboarding. Issue #2631 adds native `setup` / `doctor` / `accounts`
//! subcommands so the crate stands alone. Crucially, invoking the binary with
//! NO subcommand must preserve today's behavior (run the stdio MCP server) so
//! existing `.mcp.json` wiring keeps working.
//! What: A clap-derived `Cli` whose `command` is optional. `dispatch` runs the
//! chosen subcommand; when `command` is `None` the caller falls through to the
//! MCP server. All subcommand output goes to stdout; logs stay on stderr.
//! Test: `no_subcommand_parses` and `parses_each_subcommand` verify the parser
//! contract; the handlers themselves are tested in their own modules.

pub mod accounts;
pub mod doctor;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::api::auth::TokenStorage;
use crate::api::auth::oauth::{flow, run_consent};

/// `gworkspace-mcp` — Google Workspace MCP server + onboarding CLI.
///
/// Why: One binary serves two roles — the MCP stdio server (no subcommand)
/// and the interactive management CLI (subcommands).
/// What: clap entry point; `command` is `None` for the server path.
/// Test: `no_subcommand_parses`.
#[derive(Debug, Parser)]
#[command(
    name = "gworkspace-mcp",
    about = "Google Workspace MCP server (run with no subcommand) + OAuth onboarding CLI",
    long_about = None,
    version
)]
pub struct Cli {
    /// Optional management subcommand; when omitted the MCP stdio server runs.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Top-level management subcommands.
///
/// Why: Groups the onboarding surface (`setup`, `doctor`, `accounts`) under a
/// single greppable enum.
/// What: Each variant maps to one handler in `dispatch`.
/// Test: `parses_each_subcommand`.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run interactive OAuth consent and mint a token into tokens.json.
    Setup {
        /// Profile name to store the token under (default: `gworkspace-mcp`).
        #[arg(long)]
        profile: Option<String>,
        /// Do NOT mark this profile as the default.
        #[arg(long)]
        no_default: bool,
    },
    /// Check OAuth client credentials and token state.
    Doctor,
    /// Inspect and manage stored accounts.
    Accounts {
        #[command(subcommand)]
        action: AccountsAction,
    },
}

/// `accounts` sub-subcommands.
///
/// Why: Account management is naturally list/select/delete.
/// What: Maps to [`accounts`] module handlers.
/// Test: `parses_each_subcommand`.
#[derive(Debug, Subcommand)]
pub enum AccountsAction {
    /// List all authorized profiles.
    List,
    /// Set the default profile by name.
    Default {
        /// Profile name to make default.
        name: String,
    },
    /// Remove a profile from local storage (does not revoke Google's grant).
    Remove {
        /// Profile name to remove.
        name: String,
    },
}

/// Execute the parsed subcommand.
///
/// Why: Single dispatch point the binary calls when a subcommand is present.
/// What: Builds a default [`TokenStorage`] and routes to the matching handler.
/// `setup` runs the async consent flow; the rest are synchronous.
/// Test: Handlers are unit-tested in their modules; this wiring is exercised
/// end-to-end only for the non-network paths.
pub async fn dispatch(command: Command) -> Result<()> {
    let storage = TokenStorage::new();
    match command {
        Command::Setup {
            profile,
            no_default,
        } => {
            let profile = flow::effective_profile(profile.as_deref());
            let outcome = run_consent(&storage, &profile, !no_default).await?;
            match &outcome.email {
                Some(email) => println!(
                    "Authorized {email} as profile '{}'{}.",
                    outcome.profile,
                    if no_default { "" } else { " (default)" }
                ),
                None => println!(
                    "Authorized profile '{}' (email could not be resolved).",
                    outcome.profile
                ),
            }
            Ok(())
        }
        Command::Doctor => doctor::run(&storage),
        Command::Accounts { action } => match action {
            AccountsAction::List => accounts::list(&storage),
            AccountsAction::Default { name } => accounts::set_default(&storage, &name),
            AccountsAction::Remove { name } => accounts::remove(&storage, &name),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_subcommand_parses() {
        let cli = Cli::try_parse_from(["gworkspace-mcp"]).expect("parse");
        assert!(
            cli.command.is_none(),
            "bare invocation must leave command None so the MCP server runs"
        );
    }

    #[test]
    fn parses_each_subcommand() {
        let setup =
            Cli::try_parse_from(["gworkspace-mcp", "setup", "--profile", "work"]).expect("setup");
        assert!(matches!(
            setup.command,
            Some(Command::Setup { profile: Some(p), no_default: false }) if p == "work"
        ));

        let doctor = Cli::try_parse_from(["gworkspace-mcp", "doctor"]).expect("doctor");
        assert!(matches!(doctor.command, Some(Command::Doctor)));

        let list = Cli::try_parse_from(["gworkspace-mcp", "accounts", "list"]).expect("list");
        assert!(matches!(
            list.command,
            Some(Command::Accounts {
                action: AccountsAction::List
            })
        ));

        let default = Cli::try_parse_from(["gworkspace-mcp", "accounts", "default", "work"])
            .expect("default");
        assert!(matches!(
            default.command,
            Some(Command::Accounts { action: AccountsAction::Default { name } }) if name == "work"
        ));

        let remove =
            Cli::try_parse_from(["gworkspace-mcp", "accounts", "remove", "work"]).expect("remove");
        assert!(matches!(
            remove.command,
            Some(Command::Accounts { action: AccountsAction::Remove { name } }) if name == "work"
        ));
    }

    #[test]
    fn setup_no_default_flag_parses() {
        let cli = Cli::try_parse_from(["gworkspace-mcp", "setup", "--no-default"]).expect("parse");
        assert!(matches!(
            cli.command,
            Some(Command::Setup {
                profile: None,
                no_default: true
            })
        ));
    }
}
