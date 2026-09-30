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

/// What an explicit `--url` / `TRUSTY_MPM_URL` means for a socket command.
///
/// Why (#6288 critic HIGH 2): these commands reach only the LOCAL daemon. A
/// URL naming another host (a `--tailscale` bind) used to target that host;
/// dropping it silently would let `tm sessions delete` act on the local daemon
/// instead. So a non-loopback URL is refused, and a loopback one — the same
/// daemon the socket reaches — is ignored with one stderr line.
/// What: `Ok(Some(notice))` for a loopback URL, `Ok(None)` for no URL, and an
/// error naming the socket-only rule for anything else, including a URL that
/// does not parse.
/// Test: `an_explicit_url_is_refused_unless_it_is_loopback`,
/// `a_remote_url_is_refused_and_a_loopback_url_is_ignored`.
pub(crate) fn explicit_url_notice(url: Option<&str>) -> anyhow::Result<Option<String>> {
    let Some(url) = url.map(str::trim).filter(|u| !u.is_empty()) else {
        return Ok(None);
    };
    anyhow::ensure!(
        super::managed_merged_prs::is_loopback_url(url),
        "--url/TRUSTY_MPM_URL {url} names a daemon on another host, but this command reaches \
         only the local daemon, over its unix socket (#6288, owner ruling 2026-09-14). Refusing \
         rather than acting on the local daemon; unset --url/TRUSTY_MPM_URL to act locally."
    );
    Ok(Some(format!(
        "tm: ignoring --url/TRUSTY_MPM_URL {url}: this command reaches the local daemon over \
         its unix socket only (#6288)"
    )))
}

/// Run one socket command to completion.
///
/// # Errors
///
/// When the socket path cannot be resolved, the explicit URL names another
/// host, or the command fails; a daemon that is not listening is an error
/// naming the socket, never a TCP retry.
pub(crate) async fn run(cli: Cli) -> anyhow::Result<()> {
    if let Some(notice) = explicit_url_notice(cli.url.as_deref())? {
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

    /// #6288 critic HIGH 2: a remote URL is refused, a loopback one ignored.
    #[test]
    fn an_explicit_url_is_refused_unless_it_is_loopback() {
        use super::explicit_url_notice;
        assert!(explicit_url_notice(None).expect("no url").is_none());
        for url in [
            "http://127.0.0.1:7880",
            "http://localhost:7880",
            "http://[::1]:7880",
        ] {
            let notice = explicit_url_notice(Some(url))
                .expect(url)
                .expect("a notice");
            assert!(notice.contains("ignoring"), "{notice}");
        }
        for url in [
            "http://100.64.0.1:7880",
            "http://mac.tailnet.ts.net:7880",
            "not a url",
        ] {
            let err = explicit_url_notice(Some(url)).expect_err(url).to_string();
            assert!(
                err.contains("#6288") && err.contains("unix socket"),
                "{err}"
            );
        }
    }
}
