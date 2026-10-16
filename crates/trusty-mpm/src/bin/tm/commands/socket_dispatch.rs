//! The `tm` commands that reach the daemon over its unix socket only (#6288
//! step 1).
//!
//! Why: a sandboxed session has no loopback TCP (Q2), and the commands it runs
//! from Bash — `tm health`, `tm status`, `tm doctor`, `tm sessions …`,
//! `tm pr …`, `tm ticket` — must not dial `127.0.0.1:7880` or probe the
//! console gateway on 7788. Since the #6288 client cutover `main`'s shared
//! resolution reaches the socket too; these commands keep their early
//! dispatch because they also skip `main`'s `reqwest` client entirely.
//! What: [`takes`] names the commands; [`run`] resolves the socket once,
//! builds a socket-only [`DaemonClient`], and dispatches. There is no HTTP
//! fallback: an absent socket fails the command with an error naming its path.
//! `tm sessions tui` and `tm sessions disk` run through `main`, whose
//! resolution is the same socket locator.
//! Test: `tests/tm_cli_socket.rs`.

use trusty_mpm::client::DaemonClient;

use crate::cli::{Cli, Command, SessionAction};

/// True when `command` runs over the daemon socket and skips `main`'s HTTP
/// resolution.
///
/// Test: `socket_dispatch_takes_the_sandbox_commands`.
pub(crate) fn takes(command: &Option<Command>) -> bool {
    match command {
        Some(
            Command::Health
            | Command::Status
            | Command::Doctor { .. }
            | Command::Ticket { .. }
            | Command::Pr { .. },
        ) => true,
        Some(Command::Sessions { action } | Command::Session { action }) => !matches!(
            action,
            SessionAction::Tui { .. } | SessionAction::Disk { .. }
        ),
        _ => false,
    }
}

/// True when `command`'s `main` arm never reads the daemon URL.
///
/// Why (#6288): a remote `--url` is refused before dispatch, but a command that
/// ignores the URL (`tm daemon`, `tm supervisor`, `tm statusline`) was never
/// asking for that host and must not be blocked by it.
/// What: an allowlist of the arms in `main` that take neither `client` nor
/// `url`. A command not listed keeps the refusal (fails closed).
/// Test: `tests/tm_client_socket_cutover_6288.rs`
/// (`a_remote_url_does_not_block_a_command_that_ignores_it`).
pub(crate) fn ignores_url(command: &Option<Command>) -> bool {
    matches!(
        command,
        Some(
            Command::Stop
                | Command::Validate { .. }
                | Command::Hooks { .. }
                | Command::Agent { .. }
                | Command::Generate { .. }
                | Command::Tui { .. }
                | Command::Slack { .. }
                | Command::Install { .. }
                | Command::Reinstall(_)
                | Command::PromptFeedback(_)
                | Command::Divert { .. }
                | Command::Memory { .. }
                | Command::Compress { .. }
                | Command::Wait(_)
                | Command::BuildLease(_)
                | Command::Daemon { .. }
                | Command::Supervisor { .. }
                | Command::Fleet { .. }
                | Command::Architect { .. }
                | Command::Env { .. }
                | Command::Secrets { .. }
                | Command::Content { .. }
                | Command::Worktrees(_)
                | Command::Services { .. }
                | Command::Auth { .. }
                | Command::Catalog { .. }
                | Command::Issue { .. }
                | Command::Meta { .. }
                | Command::Load { .. }
                | Command::Path { .. }
                | Command::ShellInit { .. }
                | Command::Login { .. }
                | Command::Rm { .. }
                | Command::Update { .. }
                | Command::Statusline
                | Command::CommitTrailers(_)
                | Command::Banner { .. }
                | Command::Mcp { .. }
        )
    )
}

/// Run one socket command to completion.
///
/// # Errors
///
/// When the socket path cannot be resolved, the explicit URL names another
/// host, or the command fails; a daemon that is not listening is an error
/// naming the socket, never a TCP retry.
pub(crate) async fn run(cli: Cli) -> anyhow::Result<()> {
    // #6288 Q3: the one rule every client applies (`core::discovery`).
    if let Some(notice) = trusty_mpm::core::explicit_url_notice(cli.url.as_deref())? {
        eprintln!("{notice}");
    }
    let daemon = DaemonClient::from_resolved_socket()?;
    let result = match cli.command {
        Some(Command::Health) => super::misc::health(&daemon).await,
        Some(Command::Status) => super::status_daemon::run(&daemon).await,
        Some(Command::Doctor { flags }) => super::doctor_local::doctor(&daemon, &flags).await,
        Some(Command::Ticket {
            issue,
            system,
            notes,
            runtime,
        }) => super::ticket::ticket(&daemon, issue, system, notes, runtime).await,
        Some(Command::Pr { cmd }) => super::pr::run(cmd, &daemon).await,
        Some(Command::Sessions { action }) => {
            sessions(&daemon, &cli.account, cli.account_token_stdin, action).await
        }
        Some(Command::Session { action }) => {
            // #2116: the singular alias still says so, once.
            super::session::emit_top_level_alias_notice();
            sessions(&daemon, &cli.account, cli.account_token_stdin, action).await
        }
        _ => unreachable!("socket_dispatch::run called for a command `takes` refused"),
    };
    // A prune-idle that found the Session Manager unavailable is a graceful
    // no-op with its own exit code, which the pause skill branches on.
    if let Err(err) = &result
        && matches!(
            err.downcast_ref::<super::prune::PruneError>(),
            Some(super::prune::PruneError::SmUnavailable)
        )
    {
        std::process::exit(super::prune::EXIT_SM_UNAVAILABLE);
    }
    result
}

/// `tm sessions …` with the global `--account` applied (#8914).
async fn sessions(
    daemon: &DaemonClient,
    account: &Option<String>,
    token_stdin: bool,
    action: SessionAction,
) -> anyhow::Result<()> {
    let token = if token_stdin {
        Some(super::session_account::read_stdin_token(
            std::io::stdin().lock(),
        )?)
    } else {
        None
    };
    let (account, token) = (account.as_deref(), token.as_deref());
    super::session_account::session_as_account(daemon, action, account, token).await
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::takes;
    use crate::cli::Cli;

    /// The sandbox-reached commands take the early socket path; `tui`, `disk`
    /// and every other command use `main`'s socket resolution.
    #[test]
    fn socket_dispatch_takes_the_sandbox_commands() {
        let parse = |argv: &[&str]| {
            let cli = Cli::try_parse_from(std::iter::once("tm").chain(argv.iter().copied()))
                .expect("parses");
            takes(&cli.command)
        };
        for argv in [
            &["health"][..],
            &["status"],
            &["doctor"],
            &["ticket", "12"],
            &["pr", "cleanup", "5"],
            &["sessions", "list"],
            &["session", "ls"],
            &["sessions", "prune-worktrees"],
        ] {
            assert!(parse(argv), "{argv:?} must run over the socket");
        }
        for argv in [
            &["sessions", "tui"][..],
            &["sessions", "disk"],
            &["ls"],
            &["events"],
        ] {
            assert!(!parse(argv), "{argv:?} uses main's resolution");
        }
    }
}
