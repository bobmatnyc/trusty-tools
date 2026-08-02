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
//! registered for exactly those commands, defaulting to `warn`. Lives in its own module rather than inline in `main.rs` because
//! `main.rs` is already at the 500-SLOC cap `scripts/check_line_cap.sh`
//! enforces.
//!
//! Test: `tests/tm_sessions_instructions_diagnostics.rs`.

use crate::cli::{Command, SessionAction};

/// Whether this invocation is a CLI command whose value is its diagnostics.
///
/// Why: kept as a predicate over the parsed command rather than a flag threaded
/// from the handler, because the subscriber must exist BEFORE the handler runs —
/// the events fire inside `resolve_pm_prompt`, which the handler calls.
/// What: true for `tm sessions instructions` and its deprecated singular alias
/// `tm session instructions`.
/// Test: `instructions_emits_override_diagnostics_on_stderr`.
fn wants_cli_diagnostics(command: &Option<Command>) -> bool {
    matches!(
        command,
        Some(Command::Sessions {
            action: SessionAction::Instructions { .. }
        }) | Some(Command::Session {
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
/// Called at most once per process, before dispatch. `init` would panic on a
/// second global-subscriber registration, which is why the caller gates this
/// against the daemon/supervisor branch rather than running both.
/// Test: `instructions_emits_override_diagnostics_on_stderr`,
/// `declined_overrides_are_reported_without_rust_log_set`,
/// `a_clean_project_produces_no_stderr_noise`.
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
/// Panics on a second call: `init` registers the global subscriber. Both callers
/// are mutually exclusive branches of `main`, which is what keeps that true.
/// Test: `a_clean_project_produces_no_stderr_noise` (the writer choice is
/// asserted by piping the two streams apart).
pub(crate) fn init_stderr_only(default: &str) {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| default.into()),
        )
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
}
