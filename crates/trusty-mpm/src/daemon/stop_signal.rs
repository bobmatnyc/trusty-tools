//! The stop signals `tm daemon` and `tm sm serve --stdio` settle on (#10468).
//!
//! Why: both processes buffer action events and settle them (send or spool)
//! before exit, but only on SIGTERM and SIGINT. A SIGHUP — a closed terminal,
//! a killed tmux pane — took the default action and ended the process with
//! the buffer unsettled and uncounted.
//! What: [`StopSignals::install`] registers SIGTERM, SIGINT and SIGHUP at once,
//! so no signal can arrive between the decision to stop on it and its
//! handler; [`StopSignals::recv`] resolves on the first of them. A SIGHUP
//! the process inherited as ignored (`nohup`) stays ignored.
//! Test: `stop_signal_tests.rs`; the SIGHUP settle end to end by
//! `a_sighup_settles_the_buffered_inject_event_10468`.

#[cfg(unix)]
use tokio::signal::unix::{Signal, SignalKind};

/// The registered stop signals of this process.
pub struct StopSignals {
    #[cfg(unix)]
    streams: Vec<(&'static str, Signal)>,
}

impl StopSignals {
    /// Register the stop signals now. Call inside a tokio runtime.
    ///
    /// What: SIGTERM, SIGINT and, unless the process inherited it as ignored,
    /// SIGHUP. A failed registration is logged and that signal is skipped.
    /// Test: `install_registers_sighup_with_term_and_int_10468`.
    pub fn install() -> Self {
        #[cfg(unix)]
        {
            Self::install_with(tokio::signal::unix::signal, sighup_inherited_ignored())
        }
        #[cfg(not(unix))]
        {
            Self {}
        }
    }

    /// [`Self::install`] with the registration and the `nohup` probe given.
    ///
    /// Test: `a_failed_registration_is_skipped_and_never_stops_10468`,
    /// `an_inherited_ignored_sighup_is_left_ignored_10468`.
    #[cfg(unix)]
    pub(crate) fn install_with(
        mut register: impl FnMut(SignalKind) -> std::io::Result<Signal>,
        hup_ignored: bool,
    ) -> Self {
        let mut kinds = vec![
            ("SIGTERM", SignalKind::terminate()),
            ("SIGINT", SignalKind::interrupt()),
        ];
        // #10468: a hangup settles like SIGTERM; `nohup` keeps it ignored.
        if !hup_ignored {
            kinds.push(("SIGHUP", SignalKind::hangup()));
        }
        let streams = kinds
            .into_iter()
            .filter_map(|(name, kind)| match register(kind) {
                Ok(signal) => Some((name, signal)),
                Err(e) => {
                    tracing::warn!("failed to register the {name} handler: {e}");
                    None
                }
            })
            .collect();
        Self { streams }
    }

    /// The names of the registered signals, in registration order.
    #[cfg(unix)]
    pub(crate) fn names(&self) -> Vec<&'static str> {
        self.streams.iter().map(|(name, _)| *name).collect()
    }

    /// Resolve on the first stop signal and return its name.
    ///
    /// What: with nothing registered it never resolves, so a broken signal
    /// setup cannot end the process by itself.
    pub async fn recv(mut self) -> &'static str {
        #[cfg(unix)]
        {
            std::future::poll_fn(|cx| {
                for (name, signal) in &mut self.streams {
                    if let std::task::Poll::Ready(Some(())) = signal.poll_recv(cx) {
                        return std::task::Poll::Ready(*name);
                    }
                }
                std::task::Poll::Pending
            })
            .await
        }
        #[cfg(not(unix))]
        {
            let _ = &mut self;
            if let Err(e) = tokio::signal::ctrl_c().await {
                tracing::warn!("failed to register the Ctrl-C handler: {e}");
                std::future::pending::<()>().await;
            }
            "Ctrl-C"
        }
    }
}

/// Whether SIGHUP is set to `SIG_IGN`, as `nohup` leaves it.
#[cfg(unix)]
fn sighup_inherited_ignored() -> bool {
    // SAFETY: a null `act` only reads the current disposition into `old`.
    unsafe {
        let mut old: libc::sigaction = std::mem::zeroed();
        libc::sigaction(libc::SIGHUP, std::ptr::null(), &mut old) == 0
            && old.sa_sigaction == libc::SIG_IGN
    }
}

#[cfg(all(test, unix))]
#[path = "stop_signal_tests.rs"]
mod tests;
