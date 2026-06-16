//! `tm sm serve --stdio` — launch the SM JSON-RPC over STDIO adapter (#1291).
//!
//! Why: the SM's PRIMARY, API-first interface (DOC-14 §1A.1). A parent
//! `claude-mpm`/PM drives every `sm.*` method headlessly over newline-delimited
//! JSON-RPC to validate ALL SM functionality before any UI exists (§1A.2). Unlike
//! the generic `tm serve --stdio` MCP bridge (which PROXIES to the HTTP daemon's
//! `/rpc`), the SM adapter runs IN-PROCESS: it builds the SM core + the managed
//! session-manager surface directly (the same handles the daemon wires) so the
//! 13 `sm.*` methods map straight onto them with no extra hop.
//! What: [`run_sm_serve`] initialises a STDERR-ONLY tracing subscriber (so stdout
//! stays a clean JSON-RPC channel), builds an in-process
//! [`DaemonState`](trusty_mpm::daemon::state::DaemonState), and runs
//! [`run_sm_stdio`](trusty_mpm::daemon::sm_stdio::run_sm_stdio) to EOF.
//! Test: dispatch behaviour is covered by `daemon::sm_stdio::tests`; this thin
//! wiring is exercised at runtime via `tm sm serve --stdio`.

use anyhow::{Result, bail};

use crate::cli::CoordinatorAction;

/// Run the `tm sm serve` subcommand.
///
/// Why: dispatch entry for the SM stdio adapter. Only `--stdio` is a real mode
/// today; any other invocation is a usage error (the HTTP/TUI surfaces are
/// separate tickets), so we fail loudly rather than silently no-op.
/// What: for `Serve { stdio: true }`, inits stderr-only tracing and runs the
/// adapter; for `Serve { stdio: false }`, returns an actionable error.
/// Test: runtime; the adapter dispatch is unit-tested in `daemon::sm_stdio`.
pub(crate) async fn run_sm_serve(action: CoordinatorAction) -> Result<()> {
    let CoordinatorAction::Serve { stdio } = action;
    if !stdio {
        bail!("`tm sm serve` currently requires --stdio (the JSON-RPC over STDIO adapter)");
    }

    // STDOUT is the JSON-RPC channel — route ALL tracing to stderr. `try_init` is
    // idempotent across test binaries and a no-op if a subscriber already exists.
    use tracing_subscriber::util::SubscriberInitExt as _;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .finish()
        .try_init();

    let state = trusty_mpm::daemon::state::DaemonState::shared();
    trusty_mpm::daemon::sm_stdio::run_sm_stdio(state).await
}
