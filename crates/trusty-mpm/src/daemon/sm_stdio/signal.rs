//! The `tm sm serve --stdio` stop signal and its flush (#9167 AC5).
//!
//! Why: the stdio server records delivered injects on its own bus producer.
//! A SIGTERM or SIGINT used to end the process with up to one flush interval
//! of those events still buffered, lost without being counted as dropped.
//! What: [`serve_until`] races the stdio loop against a stop signal. On the
//! signal it runs [`before_signal_exit`], the budget the daemon's handler
//! uses (`SIGNAL_FLUSH_BUDGET`), and reports [`Ended::Signalled`] so the
//! caller exits. The stop signal is
//! [`crate::daemon::stop_signal::StopSignals`] (#10468: SIGHUP included).
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

#[cfg(test)]
#[path = "signal_tests.rs"]
mod tests;
