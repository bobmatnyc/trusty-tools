//! The whole body of a console-supervised webhook listener (#5182).
//!
//! Why: `trusty-analyze` and `trusty-review` need identical behaviour here —
//! bind the socket console dials, take durable ownership of what arrives, exit
//! on SIGTERM, unlink on the way out. Two copies of that is one copy plus the
//! drift the common-entry-point rule exists to stop, and the ordering rule the
//! copies would share is the one property this whole change turns on.
//!
//! What: [`WebhookListener::open`] resolves the inbox and the socket without
//! binding anything, so a misconfigured data directory fails before the socket
//! exists; [`WebhookListener::run`] binds and serves until the caller's shutdown
//! future resolves; [`run_until_signal`] is the binary-shaped wrapper that waits
//! for SIGTERM or SIGINT. The socket file is removed on exit so the next
//! supervised spawn binds a fresh path rather than taking over a corpse.
//!
//! Test: `tests.rs` — `listener_*`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::inbox::{Inbox, InboxError};
use super::serve::{DeliverySink, LISTENER_SHUTDOWN_FLUSH, ServeOptions, serve_until};
use crate::uds::UdsSecurityError;

/// Why a webhook listener could not start, or could not stay up.
///
/// Every variant is fatal to serving. A listener that cannot bind must exit
/// non-zero rather than run on: console probes the socket to decide whether the
/// spawn worked, and a live process with no socket would look like a slow start
/// forever.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ListenerError {
    /// The durable inbox could not be opened.
    #[error("open the webhook inbox: {0}")]
    Inbox(#[from] InboxError),

    /// The socket could not be bound, or someone else already serves it.
    #[error("bind the webhook socket at {path}: {source}")]
    Bind {
        /// Socket that could not be bound.
        path: PathBuf,
        /// Why the bind failed.
        #[source]
        source: UdsSecurityError,
    },
}

/// A bound-on-demand webhook listener for one service.
///
/// Why: the receive side of ADR-0034's relay, and the reason a target does not
/// have to be resident — console spawns this, it binds, takes ownership of the
/// delivery, and exits.
/// What: a socket path plus the [`Inbox`] that makes the ack honest.
/// Test: `listener_serves_a_delivery_and_cleans_up_its_socket`.
#[derive(Debug)]
pub struct WebhookListener {
    socket: PathBuf,
    inbox: Inbox,
    options: ServeOptions,
}

impl WebhookListener {
    /// Resolve the inbox and the socket path without binding.
    ///
    /// # Errors
    ///
    /// [`ListenerError::Inbox`] when the inbox directory cannot be created or
    /// narrowed to owner-only.
    ///
    /// Test: `listener_open_creates_the_inbox_before_binding`.
    pub fn open(
        socket: impl Into<PathBuf>,
        inbox_root: impl Into<PathBuf>,
    ) -> Result<Self, ListenerError> {
        Ok(Self {
            socket: socket.into(),
            inbox: Inbox::open(inbox_root)?,
            options: ServeOptions::default(),
        })
    }

    /// Override the per-connection budgets.
    pub fn with_options(mut self, options: ServeOptions) -> Self {
        self.options = options;
        self
    }

    /// Socket this listener binds.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The durable inbox behind the ack.
    pub fn inbox(&self) -> &Inbox {
        &self.inbox
    }

    /// Bind and serve until `shutdown` resolves, then unlink the socket.
    ///
    /// # Errors
    ///
    /// [`ListenerError::Bind`] when the path cannot be bound — including when
    /// another instance is already serving it, which is a refusal rather than a
    /// takeover (see [`crate::uds::bind_singleton_hardened`]).
    ///
    /// Test: `listener_serves_a_delivery_and_cleans_up_its_socket`,
    /// `listener_refuses_to_take_over_a_live_socket`.
    pub async fn run(
        self,
        shutdown: impl std::future::Future<Output = ()> + Send,
    ) -> Result<(), ListenerError> {
        let listener = crate::uds::bind_singleton_hardened(&self.socket)
            .await
            .map_err(|source| ListenerError::Bind {
                path: self.socket.clone(),
                source,
            })?;

        tracing::info!(
            socket = %self.socket.display(),
            inbox = %self.inbox.root().display(),
            "webhook listener bound; acknowledging only what reaches the inbox"
        );

        let sink: Arc<dyn DeliverySink> = Arc::new(self.inbox.clone());
        serve_until(&listener, sink, self.options, shutdown).await;

        // 🔴 #5182 review: unlink BEFORE the listener is dropped, which is why
        // `serve_until` borrows it. With the order reversed there is a window in
        // which nothing answers the path but the file is still there — a
        // successor probes, reads "corpse", unlinks and rebinds, and then this
        // process's `remove_file` deletes the successor's fresh socket, leaving
        // it alive and permanently unreachable. That is #5085's shape, and
        // inside the supervisor only #5085's own reap ordering closes it; an
        // operator's Ctrl-C'd listener has no such protection.
        //
        // A failure here is not worth an exit code — `bind_singleton_hardened`
        // handles a leftover file.
        if let Err(e) = std::fs::remove_file(&self.socket) {
            tracing::debug!(socket = %self.socket.display(), error = %e, "socket already gone");
        }
        drop(listener);
        Ok(())
    }
}

/// Serve until SIGTERM or SIGINT, the shape a supervised child needs.
///
/// Why: console SIGTERMs the child and waits [`LISTENER_SHUTDOWN_FLUSH`] before
/// escalating to SIGKILL, so the child has to actually observe the signal. A
/// listener holds no unflushed work — it acks only after an fsync — so the only
/// thing this window covers is the one connection it may be mid-way through.
///
/// # Errors
///
/// Any [`ListenerError`], plus a failure to install the signal handlers.
///
/// Test: exercised by each target crate's `webhook_listener` module; the signal
/// wait itself carries no logic to test.
pub async fn run_until_signal(
    socket: impl Into<PathBuf>,
    inbox_root: impl Into<PathBuf>,
) -> Result<(), ListenerError> {
    let listener = WebhookListener::open(socket, inbox_root)?;
    debug_assert!(
        listener.options.read_timeout <= LISTENER_SHUTDOWN_FLUSH,
        "a connection must settle inside the flush budget console waits out"
    );
    listener.run(wait_for_termination()).await
}

/// Resolve on SIGTERM or SIGINT; never resolve if neither can be installed.
///
/// A failed handler install is logged rather than returned: the alternative is
/// refusing to serve at all, which loses deliveries to protect a shutdown path.
/// `kill_on_drop` in console's supervisor still reaps the child.
async fn wait_for_termination() {
    let mut term = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "could not install the SIGTERM handler");
            return std::future::pending().await;
        }
    };
    let mut int = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "could not install the SIGINT handler");
            term.recv().await;
            return;
        }
    };
    tokio::select! {
        _ = term.recv() => tracing::info!("webhook listener received SIGTERM; shutting down"),
        _ = int.recv() => tracing::info!("webhook listener received SIGINT; shutting down"),
    }
}
