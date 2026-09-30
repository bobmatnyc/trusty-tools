//! The `tm` commands that reach the daemon over its unix socket only (#6288
//! step 1).
//!
//! Why: a sandboxed session has no loopback TCP (Q2), and the commands it runs
//! from Bash — `tm health`, `tm status`, `tm doctor`, `tm sessions …`,
//! `tm pr …`, `tm ticket` — must not dial `127.0.0.1:7880` or probe the
//! console gateway on 7788. `main`'s shared resolution does both, so these
//! commands are dispatched before it runs. `tm build-lease` already runs
//! before it and posts over the socket itself.
//! What: [`takes`] names the commands; [`run`] resolves the socket once,
//! builds a socket-only [`DaemonClient`], and dispatches. There is no HTTP
//! fallback: an absent socket fails the command with an error naming its path.
//! Two `tm sessions` verbs stay on HTTP until step 2 — `tui` (the operator
//! TUI polls on its own client) and `disk` (the MCP `POST /rpc` surface the
//! stdio-bridge slice moves).
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

/// Run one socket command to completion.
///
/// # Errors
///
/// When the socket path cannot be resolved, or the command fails; a daemon
/// that is not listening is an error naming the socket, never a TCP retry.
pub(crate) async fn run(cli: Cli) -> anyhow::Result<()> {
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

    /// The sandbox-reached commands take the socket path; `tui`, `disk` and
    /// every other command keep `main`'s resolution until step 2.
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
            assert!(!parse(argv), "{argv:?} stays on main's resolution");
        }
    }
}
