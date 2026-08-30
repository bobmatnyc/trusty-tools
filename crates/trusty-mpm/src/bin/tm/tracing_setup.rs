//! Tracing subscriber setup for the short-lived CLI inspection commands
//! (issue #4573).
//!
//! Why: `main.rs` registered a subscriber only for the long-running modes
//! (`Command::Daemon`, `Command::Supervisor`), which was fine while every other
//! subcommand printed its own results. It stopped being fine for
//! `tm sessions instructions`: that command's entire diagnostic surface —
//! `claude_md_sections::ProjectOverrides::log` (one event per applied, declined,
//! duplicated or shadowed marker), `claude_md_sections::warn_unapplied`,
//! `instruction_overrides::read_override`, and `resolve_pm_prompt`'s
//! composer-source event — is `tracing`, and with no subscriber registered all
//! of it was dropped. `RUST_LOG=trusty_mpm=info tm sessions instructions --dir
//! <project>` emitted ZERO bytes on stderr, so the ONE command an operator runs
//! to ask "why didn't my override apply?" could not answer.
//!
//! In the #4573 reproduction that mattered concretely: a `CLAUDE.md` CORE block
//! deleted the Prohibitions and Circuit Breakers tables, and the operator's only
//! signal was their absence from 28 KB of output.
//!
//! What: [`init_cli_diagnostics_if_wanted`] — a stderr-only `fmt` subscriber
//! registered for exactly those commands, defaulting to `warn`. Lives in its own
//! module rather than inline in `main.rs` because `main.rs` is already at the
//! 500-SLOC cap `scripts/check_line_cap.sh` enforces.
//!
//! Test: `tests/tm_sessions_instructions_diagnostics.rs`.

use crate::cli::{CatalogAction, Command, SessionAction};

/// Whether this invocation is a CLI command whose value is its diagnostics.
///
/// Why: kept as a predicate over the parsed command rather than a flag threaded
/// from the handler, because the subscriber must exist BEFORE the handler runs —
/// the events fire inside `resolve_pm_prompt`, which the handler calls.
///
/// #4878 widened it past `tm sessions instructions` to every in-process path
/// that runs a DEPLOY. Deployment legitimately declines a file — a
/// checksum-frozen skill, an unreadable ledger, a raced merge — and each
/// decline is a `warn!`/`error!` written to end silent staleness (#4840, and
/// PRs #4848 / #4876 / #4908). With no subscriber those events went nowhere, so
/// the file stayed stale forever with no signal on the four paths an operator
/// runs by hand.
/// What: true for `tm sessions instructions` and its deprecated singular alias;
/// for bare `tm` (`None`), which deploys in-process on both the launch and the
/// in-place relaunch path; for `tm doctor`, whose `--fix-skills` redeploys; and
/// for `tm catalog apply`, which redeploys the manifest-selected roster.
/// Test: `wants_cli_diagnostics_covers_every_in_process_deploy_path`,
/// `wants_cli_diagnostics_skips_paths_that_own_stdout`,
/// `instructions_emits_override_diagnostics_on_stderr`.
fn wants_cli_diagnostics(command: &Option<Command>) -> bool {
    matches!(
        command,
        // Bare `tm`: `commands::launch` and the in-place relaunch path both
        // deploy in-process before handing off to Claude Code.
        None | Some(Command::Doctor { .. })
            | Some(Command::Catalog {
                action: CatalogAction::Apply { .. }
            })
            | Some(Command::Sessions {
                action: SessionAction::Instructions { .. }
            })
            | Some(Command::Session {
                action: SessionAction::Instructions { .. }
            })
    )
}

/// Register a stderr-only subscriber when `command` is a CLI inspection command.
///
/// Why the default filter is `warn` and not `info`: an operator debugging a
/// silently-ignored override must not have to already know to set `RUST_LOG`
/// before the framework will admit it declined their block — the decline IS the
/// answer they came for. Applied-override `info` lines stay below the default so
/// an ordinary run is not narrated; `RUST_LOG=trusty_mpm=info` opts into those.
///
/// Why stderr, explicitly: `tracing_subscriber::fmt` writes to stdout by
/// default, `tm sessions instructions` prints the resolved system prompt to
/// stdout, and this binary's daemon and MCP modes require stdout stay free for
/// JSON-RPC framing. The `with_writer` call is load-bearing.
///
/// Called at most once per process, before dispatch; the caller still gates
/// this against the daemon/supervisor branch so those keep their file-rotating
/// subscriber.
/// Test: `instructions_emits_override_diagnostics_on_stderr`,
/// `declined_overrides_are_reported_without_rust_log_set`,
/// `a_clean_project_produces_no_override_diagnostics`.
pub(crate) fn init_cli_diagnostics_if_wanted(command: &Option<Command>) {
    if wants_cli_diagnostics(command) {
        init_stderr_only("warn");
    }
}

/// Install a stderr-only `fmt` subscriber, honouring `RUST_LOG` over `default`.
///
/// Why stderr, explicitly: `tracing_subscriber::fmt` writes to STDOUT by
/// default, and every mode of this binary needs stdout for something else —
/// `tm sessions instructions` prints the resolved system prompt there, and the
/// daemon and MCP modes need it free for JSON-RPC framing. The `with_writer`
/// call is the load-bearing line in this function, not decoration.
///
/// Idempotent: `try_init` returns an error instead of panicking when a global
/// subscriber is already registered, and that error is discarded. #4878 widened
/// [`wants_cli_diagnostics`] to cover bare `tm`, so a path that also installs
/// its own subscriber would otherwise abort the process rather than log — and
/// the project convention is `try_init` for exactly this reason.
/// Test: `init_stderr_only_is_idempotent`,
/// `a_clean_project_produces_no_override_diagnostics` (the writer choice is
/// asserted by piping the two streams apart).
pub(crate) fn init_stderr_only(default: &str) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| default.into()),
        )
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    use crate::cli::Cli;

    /// Parse a real argv the way `main` does, so the predicate is asserted
    /// against the command clap actually produces.
    fn command_for(argv: &[&str]) -> Option<crate::cli::Command> {
        Cli::try_parse_from(argv)
            .unwrap_or_else(|e| panic!("argv {argv:?} must parse: {e}"))
            .command
    }

    #[test]
    fn wants_cli_diagnostics_covers_every_in_process_deploy_path() {
        // #4878: each of these runs a deploy in-process and emits its declines
        // as `tracing` events. Before the fix only `sessions instructions`
        // registered a subscriber, so every warning here went nowhere.
        for argv in [
            vec!["tm"],
            vec!["tm", "doctor"],
            vec!["tm", "doctor", "--fix-skills"],
            vec!["tm", "catalog", "apply"],
            vec!["tm", "catalog", "apply", "--force", "--prune"],
            vec!["tm", "sessions", "instructions"],
            vec!["tm", "session", "instructions"],
        ] {
            assert!(
                wants_cli_diagnostics(&command_for(&argv)),
                "{argv:?} runs an in-process deploy and must get a subscriber"
            );
        }
    }

    #[test]
    fn wants_cli_diagnostics_skips_paths_that_own_stdout() {
        // The daemon/supervisor branch installs its own file-rotating
        // subscriber, and `tm serve --stdio` needs stdout for JSON-RPC framing.
        // Neither deploys in-process, so neither is widened here.
        for argv in [
            vec!["tm", "serve", "--stdio"],
            vec!["tm", "status"],
            vec!["tm", "catalog", "ls"],
        ] {
            assert!(
                !wants_cli_diagnostics(&command_for(&argv)),
                "{argv:?} must not register the CLI diagnostics subscriber"
            );
        }
    }

    #[test]
    fn init_stderr_only_is_idempotent() {
        // `try_init`, not `init`: a second registration must return an error
        // that is discarded, never abort the process. Other tests in this
        // binary may already have installed a subscriber, so this asserts the
        // no-panic property in either order.
        init_stderr_only("warn");
        init_stderr_only("warn");
    }
}
