//! The `tm sm serve --stdio` stop signal and its flush (#9167 AC5).
//!
//! Why: the stdio server records delivered injects on its own bus producer.
//! A SIGTERM or SIGINT used to end the process with up to one flush interval
//! of those events still buffered, lost without being counted as dropped.
//! What: [`serve_until`] races the stdio loop against a stop signal. On the
//! signal it runs [`before_signal_exit`], the budget the daemon's handler
//! uses (`SIGNAL_FLUSH_BUDGET`), and reports [`Ended::Signalled`] so the
//! caller exits. [`shutdown_signal`] resolves on SIGTERM or SIGINT.
//! Test: `signal_tests.rs`.

use std::future::Future;

use crate::daemon::action_events::before_signal_exit;
use crate::daemon::state::DaemonState;

/// How [`serve_until`] ended.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Ended<T> {
    /// The served future finished (stdin EOF) with this result.
    Served(T),
    /// The stop signal fired and the bus producer was flushed.
    Signalled,
}

/// Run `serve` until it finishes or `stop` resolves; on `stop`, flush the
/// bus producer within the signal budget first.
///
/// Test: `a_signal_flushes_the_buffered_inject_event_9167`.
pub(super) async fn serve_until<T>(
    state: &DaemonState,
    stop: impl Future<Output = ()>,
    serve: impl Future<Output = T>,
) -> Ended<T> {
    tokio::select! {
        served = serve => Ended::Served(served),
        () = stop => {
            before_signal_exit(state, || {}).await;
            Ended::Signalled
        }
    }
}

/// Resolve on SIGTERM or SIGINT (Ctrl-C).
///
/// What: a failed handler registration is logged and falls back to Ctrl-C
/// alone; if that cannot be registered either, it never resolves, so a broken
/// signal setup cannot end the server by itself.
pub(super) async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    r = tokio::signal::ctrl_c() => ctrl_c_or_pend(r).await,
                    _ = term.recv() => {}
                }
            }
            Err(e) => {
                tracing::warn!("sm stdio: failed to register SIGTERM handler: {e}");
                ctrl_c_or_pend(tokio::signal::ctrl_c().await).await;
            }
        }
    }
    #[cfg(not(unix))]
    ctrl_c_or_pend(tokio::signal::ctrl_c().await).await;
}

/// `Ok` is the signal; `Err` is logged and never resolves.
async fn ctrl_c_or_pend(r: std::io::Result<()>) {
    if let Err(e) = r {
        tracing::warn!("sm stdio: failed to register Ctrl-C handler: {e}");
        std::future::pending::<()>().await;
    }
}

#[cfg(test)]
#[path = "signal_tests.rs"]
mod tests;
