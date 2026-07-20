//! Command-line interface for the `trusty-gworkspace-mcp` binary.
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

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::api::auth::TokenStorage;
use crate::api::auth::oauth::{DefaultMode, flow, persist_profile_client, run_consent};

/// `trusty-gworkspace-mcp` — Google Workspace MCP server + onboarding CLI.
///
/// Why: One binary serves two roles — the MCP stdio server (no subcommand)
/// and the interactive management CLI (subcommands).
/// What: clap entry point; `command` is `None` for the server path.
/// Test: `no_subcommand_parses`.
#[derive(Debug, Parser)]
#[command(
    name = "trusty-gworkspace-mcp",
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
    ///
    /// Default-profile behavior: if no default profile exists yet, the newly
    /// authorized profile automatically becomes it. If a default already
    /// exists, it is left alone (silently swapping it on a routine re-run
    /// would change which Google account MCP write-tools act against). Use
    /// `--make-default` to explicitly switch the default, or `--no-default`
    /// to guarantee it is never touched.
    Setup {
        /// Profile name to store the token under (default: `gworkspace-mcp`).
        #[arg(long)]
        profile: Option<String>,
        /// Do NOT mark this profile as the default, even if none exists yet.
        #[arg(long, conflicts_with = "make_default")]
        no_default: bool,
        /// Explicitly make this profile the default, displacing any existing
        /// one (a warning is printed naming the displaced profile).
        #[arg(long, conflicts_with = "no_default")]
        make_default: bool,
        /// Do NOT launch a browser; print the consent URL for you to open
        /// manually. Enables re-authenticating from a headless / in-session
        /// context (the loopback callback still captures the redirect).
        #[arg(long = "no-browser", visible_alias = "print-url")]
        no_browser: bool,
        /// Authorize this profile against a SPECIFIC OAuth client instead of
        /// the shared global one (issue #3518). Path to a JSON file — flat
        /// `{"client_id","client_secret"}` or Google's downloaded
        /// `installed`/`web` shape. Persisted to
        /// `~/.gworkspace-mcp/clients/<profile>.json` (0600) and reused on
        /// every future refresh for this profile.
        #[arg(long = "oauth-client", value_name = "PATH")]
        oauth_client: Option<PathBuf>,
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
            make_default,
            no_browser,
            oauth_client,
        } => {
            let profile = flow::effective_profile(profile.as_deref());
            if let Some(path) = &oauth_client {
                let target = persist_profile_client(&profile, path)?;
                println!(
                    "Using OAuth client from {} for profile '{profile}' (persisted to {}).",
                    path.display(),
                    target.display()
                );
            }
            let mode = if no_default {
                DefaultMode::Never
            } else if make_default {
                DefaultMode::Force
            } else {
                DefaultMode::Auto
            };
            let outcome = run_consent(&storage, &profile, mode, !no_browser).await?;
            let suffix = if outcome.default_applied {
                " (default)"
            } else {
                ""
            };
            match &outcome.email {
                Some(email) => {
                    println!(
                        "Authorized {email} as profile '{}'{suffix}.",
                        outcome.profile
                    )
                }
                None => println!(
                    "Authorized profile '{}' (email could not be resolved){suffix}.",
                    outcome.profile
                ),
            }
            Ok(())
        }
        Command::Doctor => doctor::run(&storage).await,
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
            Some(Command::Setup {
                profile: Some(p),
                no_default: false,
                make_default: false,
                no_browser: false,
                oauth_client: None,
            }) if p == "work"
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
                no_default: true,
                make_default: false,
                no_browser: false,
                oauth_client: None,
            })
        ));
    }

    #[test]
    fn setup_make_default_flag_parses() {
        let cli =
            Cli::try_parse_from(["gworkspace-mcp", "setup", "--make-default"]).expect("parse");
        assert!(matches!(
            cli.command,
            Some(Command::Setup {
                profile: None,
                no_default: false,
                make_default: true,
                no_browser: false,
                oauth_client: None,
            })
        ));
    }

    #[test]
    fn setup_no_browser_flag_parses() {
        let cli = Cli::try_parse_from(["gworkspace-mcp", "setup", "--no-browser"]).expect("parse");
        assert!(matches!(
            cli.command,
            Some(Command::Setup {
                no_browser: true,
                ..
            })
        ));
    }

    #[test]
    fn setup_print_url_alias_parses() {
        let cli = Cli::try_parse_from(["gworkspace-mcp", "setup", "--print-url"]).expect("parse");
        assert!(matches!(
            cli.command,
            Some(Command::Setup {
                no_browser: true,
                ..
            })
        ));
    }

    #[test]
    fn setup_no_default_and_make_default_are_mutually_exclusive() {
        let result =
            Cli::try_parse_from(["gworkspace-mcp", "setup", "--no-default", "--make-default"]);
        assert!(
            result.is_err(),
            "--no-default and --make-default must conflict"
        );
    }

    #[test]
    fn setup_oauth_client_flag_parses() {
        let cli = Cli::try_parse_from([
            "gworkspace-mcp",
            "setup",
            "--profile",
            "work",
            "--oauth-client",
            "/tmp/client.json",
        ])
        .expect("parse");
        assert!(matches!(
            cli.command,
            Some(Command::Setup {
                oauth_client: Some(p),
                ..
            }) if p.as_path() == std::path::Path::new("/tmp/client.json")
        ));
    }
}
