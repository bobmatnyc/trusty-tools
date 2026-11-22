//! The `tagent` command line after the #11632 cut.
//!
//! Why: the PM orchestrator, REPL, workflow modes and the HTTP API (`--api`,
//! `--serve`, `--port`) were removed with the second harness, so the old
//! `runtime::run` dispatcher had nothing left to dispatch. What remains is the
//! stdio MCP server and the daemon-less `config` verbs, which were already
//! dispatched ahead of the old startup path.
//! What: [`run`] routes argv to `--version`, `--help`, `config …`,
//! `config profile …` or `mcp-serve`; anything else is a usage error. No path
//! here opens a network listener.
//! Test: `tests/version_provenance.rs`, `tests/help_exit_code.rs`,
//! `tests/config_mount.rs`, `tests/no_network_listener.rs`.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};

/// How long process shutdown waits for background tasks after [`run`] returns.
///
/// Why (#3655): a runtime dropped at the end of `main` waits on the blocking
/// pool with no ceiling, so one task stuck in a syscall makes `tagent`
/// un-exitable. Healthy runs pay nothing — the wait returns as soon as the
/// pool drains.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// The usage text `--help` prints.
const USAGE: &str = "\
tagent — Trusty Agents assistant core

Usage:
  tagent --version             Print the version and the commit that built it
  tagent --help                Print this help
  tagent config <verb> …       Manage inference provider credentials (API keys)
  tagent config profile <verb> Show or set the user profile
  tagent mcp-serve             Serve the dispatch_task tool as a stdio MCP server
";

/// Launch the binary: own the tokio runtime, run [`run`] on it, and tear the
/// runtime down with a bounded wait.
///
/// Why (#3655): see [`SHUTDOWN_GRACE`].
/// What: builds a multi-thread runtime, blocks on [`run`], then calls
/// `shutdown_timeout`. `run()`'s own result is returned unchanged.
/// Test: `tests/version_provenance.rs` drives the binary through here.
pub fn run_to_completion() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build the tokio runtime")?;
    let result = runtime.block_on(run());

    let started = Instant::now();
    runtime.shutdown_timeout(SHUTDOWN_GRACE);
    if started.elapsed() >= SHUTDOWN_GRACE {
        tracing::warn!(
            grace_secs = SHUTDOWN_GRACE.as_secs(),
            "background tasks did not finish within the shutdown grace; exiting anyway"
        );
    }
    result
}

/// What `tagent` does for one argv.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Version,
    Help,
    Config,
    ConfigProfile,
    McpServe,
    Unknown(String),
}

/// Classify argv into the one [`Command`] it asks for.
///
/// Why: a pure function, so the removal of `--api`/`--port` is testable
/// without spawning the binary.
/// What: `--version`/`-V` anywhere wins (the old startup path checked the
/// whole argv); otherwise the first argument picks the verb. No argument
/// prints help.
/// Test: `cli::tests::removed_modes_are_unknown`, `cli::tests::verbs_route`.
fn classify(args: &[String]) -> Command {
    if args.iter().skip(1).any(|a| a == "--version" || a == "-V") {
        return Command::Version;
    }
    match args.get(1).map(String::as_str) {
        None | Some("--help") | Some("-h") | Some("help") => Command::Help,
        Some("config") if args.get(2).map(String::as_str) == Some("profile") => {
            Command::ConfigProfile
        }
        Some("config") => Command::Config,
        Some("mcp-serve") => Command::McpServe,
        Some(other) => Command::Unknown(other.to_string()),
    }
}

/// Library entry point for the `tagent` binary.
///
/// Why: kept in the library so a downstream launcher can call
/// `install_plugins(...)` first, as before the cut.
/// What: installs the rustls crypto provider (#4825), then runs the verb
/// [`classify`] picks. An unknown argument — including the removed `--api`,
/// `--serve` and `--port` — prints the usage on stderr and fails.
/// Test: `tests/no_network_listener.rs`, `tests/help_exit_code.rs`.
pub async fn run() -> Result<()> {
    crate::tls::install_crypto_provider()?;
    let args: Vec<String> = std::env::args().collect();
    match classify(&args) {
        Command::Version => {
            println!("{}", crate::build_info::version_string());
            Ok(())
        }
        Command::Help => {
            print!("{USAGE}");
            Ok(())
        }
        Command::Config => run_config_subcommand(&args[1..]).await,
        Command::ConfigProfile => run_config_profile_subcommand(&args[3..]),
        Command::McpServe => crate::mcp_serve::run_mcp_serve().await,
        Command::Unknown(arg) => {
            eprint!("{USAGE}");
            anyhow::bail!(
                "unknown argument `{arg}`; the HTTP API and harness modes were removed (#11632)"
            )
        }
    }
}

/// Parse and run the universal `config` command (#2405) from an argv tail.
///
/// Why: mounts the one inference-provider credential CLI every primary binary
/// shares (epic #2400 Wave 1).
/// What: `argv_tail` starts at the literal `config` token (clap treats it as
/// the program name). `parse_from` renders help and errors and exits as clap
/// normally would; on success the command runs against the shared store.
/// Test: `tests/config_mount.rs` drives the built binary with `config --help`
/// and `config keys list`.
async fn run_config_subcommand(argv_tail: &[String]) -> Result<()> {
    use clap::Parser as _;

    /// Thin `Parser` wrapper so the shared `ConfigCommand` args can be parsed
    /// from an argv tail.
    #[derive(clap::Parser)]
    #[command(
        name = "tagent config",
        about = "Manage inference provider configuration (API keys)."
    )]
    struct ConfigParser {
        #[command(flatten)]
        cmd: trusty_common::inference::config::ConfigCommand,
    }

    ConfigParser::parse_from(argv_tail).cmd.run().await
}

/// Parse and run `tagent config profile show|set` (#3406 follow-up).
///
/// Why: gives `~/.trusty-agents/user.toml` (name/email/timezone/location) a
/// CLI instead of hand-edited TOML.
/// What: `argv_tail` starts after the literal `profile` token. `show` prints
/// the profile (or "not set"); `set` overwrites only the flags passed and
/// saves.
/// Test: `tests/config_mount.rs::profile_show_then_set_round_trips`.
fn run_config_profile_subcommand(argv_tail: &[String]) -> Result<()> {
    use crate::identity::user_profile::UserProfile;
    use clap::Parser as _;

    /// `tagent config profile <verb>`.
    #[derive(clap::Parser)]
    #[command(name = "tagent config profile", about = "Manage the user profile.")]
    struct ProfileParser {
        #[command(subcommand)]
        verb: ProfileVerb,
    }

    #[derive(clap::Subcommand)]
    enum ProfileVerb {
        /// Print the current profile (or "not set").
        Show,
        /// Update one or more profile fields, creating the profile if absent.
        Set {
            #[arg(long)]
            name: Option<String>,
            #[arg(long)]
            email: Option<String>,
            #[arg(long)]
            timezone: Option<String>,
            #[arg(long)]
            location: Option<String>,
        },
    }

    let parsed = ProfileParser::parse_from(
        std::iter::once("tagent config profile".to_string()).chain(argv_tail.iter().cloned()),
    );

    match parsed.verb {
        ProfileVerb::Show => match UserProfile::load() {
            Some(p) => {
                println!("name:     {}", p.name);
                println!("email:    {}", p.email.as_deref().unwrap_or("(not set)"));
                println!("timezone: {}", p.timezone.as_deref().unwrap_or("(not set)"));
                println!("location: {}", p.location.as_deref().unwrap_or("(not set)"));
                println!("saved at: {}", UserProfile::profile_path().display());
            }
            None => println!(
                "No profile set. Set TAGENT_USER_NAME/_EMAIL/_TIMEZONE/_LOCATION in \
                 the environment, or run `tagent config profile set --name <name>`."
            ),
        },
        ProfileVerb::Set {
            name,
            email,
            timezone,
            location,
        } => {
            let mut profile = UserProfile::load().unwrap_or_default();
            if let Some(name) = name {
                profile.name = name;
            }
            if email.is_some() {
                profile.email = email;
            }
            if timezone.is_some() {
                profile.timezone = timezone;
            }
            if location.is_some() {
                profile.location = location;
            }
            if profile.created_at.is_empty() {
                profile.created_at = chrono::Utc::now().to_rfc3339();
            }
            if !profile.is_complete() {
                anyhow::bail!("refusing to save a profile with no name — pass --name");
            }
            profile.save().context("failed to save user profile")?;
            println!(
                "Saved profile for {} to {}.",
                profile.name,
                UserProfile::profile_path().display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        std::iter::once("tagent")
            .chain(parts.iter().copied())
            .map(String::from)
            .collect()
    }

    /// Why (#11632): the HTTP API and harness modes are gone; their flags must
    /// not fall through to anything that could start them.
    /// Test: itself.
    #[test]
    fn removed_modes_are_unknown() {
        for flag in [
            "--api",
            "--serve",
            "--port",
            "--ctrl",
            "--workflow",
            "--agent",
        ] {
            assert_eq!(
                classify(&argv(&[flag, "8080"])),
                Command::Unknown(flag.to_string()),
                "{flag}"
            );
        }
    }

    /// Why: each kept verb reaches its own handler.
    /// Test: itself.
    #[test]
    fn verbs_route() {
        assert_eq!(classify(&argv(&[])), Command::Help);
        assert_eq!(classify(&argv(&["-h"])), Command::Help);
        assert_eq!(classify(&argv(&["mcp-serve"])), Command::McpServe);
        assert_eq!(
            classify(&argv(&["config", "keys", "list"])),
            Command::Config
        );
        assert_eq!(
            classify(&argv(&["config", "profile", "show"])),
            Command::ConfigProfile
        );
        assert_eq!(classify(&argv(&["mcp-serve", "-V"])), Command::Version);
    }
}
