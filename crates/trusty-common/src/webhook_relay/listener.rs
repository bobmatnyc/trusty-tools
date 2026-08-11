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
//! #5192 added the other half: with [`WebhookListener::with_processor`] the
//! same process reads the inbox back out and drives the target's pipeline. The
//! schedule is in [`spawn_drain`] and the ordering rules are
//! [`super::drain`]'s; nothing about the receive path changed, because the ack
//! still rests on durability alone and must not start waiting on a review.
//!
//! Test: `tests.rs` — `listener_*`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::RelayDelivery;
use super::drain::{DeliveryProcessor, DrainPolicy, drain_once};
use super::inbox::{Inbox, InboxError};
use super::serve::{
    DeliverySink, LISTENER_SHUTDOWN_FLUSH, ServeOptions, SinkRejection, serve_until,
};
use crate::uds::UdsSecurityError;

/// How often the drain re-runs with no new delivery to prompt it.
///
/// Why: a new delivery wakes the drain immediately, so this interval exists for
/// one case only — an entry that failed retryably and is waiting for whatever
/// broke to come back. Thirty seconds is short enough that a transient GitHub
/// 5xx costs one interval rather than one deploy, and long enough that the
/// retry budget ([`super::DEFAULT_MAX_ATTEMPTS`]) is not spent inside a blip.
pub const DEFAULT_DRAIN_INTERVAL: Duration = Duration::from_secs(30);

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
/// What: a socket path, the [`Inbox`] that makes the ack honest, and — since
/// #5192 — the [`DeliveryProcessor`] that reads the inbox back out. Without a
/// processor the listener is what #5182 shipped: durable receipt and nothing
/// downstream of it.
/// Test: `listener_serves_a_delivery_and_cleans_up_its_socket`,
/// `listener_drains_a_delivery_it_just_accepted`.
pub struct WebhookListener {
    socket: PathBuf,
    inbox: Inbox,
    options: ServeOptions,
    processor: Option<Arc<dyn DeliveryProcessor>>,
    drain_policy: DrainPolicy,
    drain_interval: Duration,
}

impl std::fmt::Debug for WebhookListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookListener")
            .field("socket", &self.socket)
            .field("inbox", &self.inbox)
            .field("options", &self.options)
            .field("has_processor", &self.processor.is_some())
            .field("drain_policy", &self.drain_policy)
            .field("drain_interval", &self.drain_interval)
            .finish()
    }
}

/// An [`Inbox`] that wakes the drain the moment it takes ownership.
///
/// Why: the alternative is a poll interval, and a poll interval is a delivery
/// sitting undone for up to that long for no reason. The notify fires AFTER
/// `take_ownership` returns `Ok`, so the drain never races the fsync that makes
/// the entry findable.
/// Test: `listener_drains_a_delivery_it_just_accepted`.
struct NotifyingInbox {
    inbox: Inbox,
    notify: Arc<tokio::sync::Notify>,
}

impl DeliverySink for NotifyingInbox {
    fn take_ownership(&self, delivery: &RelayDelivery) -> Result<(), SinkRejection> {
        let taken = DeliverySink::take_ownership(&self.inbox, delivery);
        if taken.is_ok() {
            self.notify.notify_one();
        }
        taken
    }
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
            processor: None,
            drain_policy: DrainPolicy::default(),
            drain_interval: DEFAULT_DRAIN_INTERVAL,
        })
    }

    /// Override the per-connection budgets.
    pub fn with_options(mut self, options: ServeOptions) -> Self {
        self.options = options;
        self
    }

    /// Drive held deliveries into `processor` (#5192).
    ///
    /// Without this the listener takes durable ownership and stops there, which
    /// is what #5182 shipped and what #5192 exists to finish.
    pub fn with_processor(mut self, processor: Arc<dyn DeliveryProcessor>) -> Self {
        self.processor = Some(processor);
        self
    }

    /// Override the retry bound and the idle re-drain interval.
    pub fn with_drain_tuning(mut self, policy: DrainPolicy, interval: Duration) -> Self {
        self.drain_policy = policy;
        self.drain_interval = interval;
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
    /// `listener_refuses_to_take_over_a_live_socket`,
    /// `listener_drains_a_delivery_it_just_accepted`,
    /// `listener_drains_what_a_previous_run_left_behind`.
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
            draining = self.processor.is_some(),
            "webhook listener bound; acknowledging only what reaches the inbox"
        );

        let notify = Arc::new(tokio::sync::Notify::new());
        let (sink, drain): (Arc<dyn DeliverySink>, _) = match self.processor.clone() {
            Some(processor) => (
                Arc::new(NotifyingInbox {
                    inbox: self.inbox.clone(),
                    notify: Arc::clone(&notify),
                }),
                Some(spawn_drain(
                    self.inbox.clone(),
                    processor,
                    Arc::clone(&notify),
                    self.drain_policy,
                    self.drain_interval,
                )),
            ),
            None => (Arc::new(self.inbox.clone()), None),
        };

        serve_until(&listener, sink, self.options, shutdown).await;

        // Stop the drain rather than wait on it: a review in flight can outlast
        // console's SIGKILL patience by a wide margin, and waiting would only
        // convert a clean stop into a killed one. Cancelling is safe for the
        // same reason a crash is — the entry stays on disk and the claim's
        // `flock` goes with the dropped fd, so the next run picks it up. See
        // `drain`'s module docs.
        if let Some(handle) = drain {
            handle.abort();
        }

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

/// Run the drain: once at startup, then on every accepted delivery, then on a
/// timer.
///
/// Why each of the three matters, and none is redundant.
/// * **At startup** — the task's first act, without waiting for a delivery to
///   prompt it. Whatever the previous run left behind (SIGKILLed mid-review, a
///   pipeline that was down, a machine that lost power) is claimable again, and
///   nothing else will ever look at it. Without this pass a delivery survives a
///   crash and is then held forever, which is the durable version of losing it.
/// * **On notify**, so a delivery is worked the moment it lands rather than at
///   the next tick.
/// * **On the interval**, because a retryable failure has nothing to wake it.
///
/// The pass runs to completion before the next wait, so two passes never
/// overlap in this process — and if they somehow did, the per-entry `flock`
/// still makes double-processing impossible.
///
/// Test: `listener_drains_a_delivery_it_just_accepted`,
/// `listener_drains_what_a_previous_run_left_behind`.
fn spawn_drain(
    inbox: Inbox,
    processor: Arc<dyn DeliveryProcessor>,
    notify: Arc<tokio::sync::Notify>,
    policy: DrainPolicy,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    let source = inbox.root().display().to_string();
    tokio::spawn(async move {
        loop {
            let report = drain_once(&inbox, processor.as_ref(), policy).await;
            report.log_summary(&source);
            tokio::select! {
                () = notify.notified() => {}
                () = tokio::time::sleep(interval) => {}
            }
        }
    })
}

/// Serve until SIGTERM or SIGINT, the shape a supervised child needs.
///
/// Why: console SIGTERMs the child and waits [`LISTENER_SHUTDOWN_FLUSH`] before
/// escalating to SIGKILL, so the child has to actually observe the signal. A
/// listener holds no unflushed work — it acks only after an fsync — so the only
/// thing this window covers is the one connection it may be mid-way through.
///
/// Receipt only: the inbox is never read back. Use
/// [`run_until_signal_with_processor`] for a target that actually does the
/// work.
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
    run_listener(WebhookListener::open(socket, inbox_root)?).await
}

/// Serve until SIGTERM or SIGINT, draining held deliveries into `processor`
/// (#5192).
///
/// # Errors
///
/// Any [`ListenerError`].
///
/// Test: the drain behaviour is `drain_*` and `listener_drains_*`; this wrapper
/// adds only the signal wait.
pub async fn run_until_signal_with_processor(
    socket: impl Into<PathBuf>,
    inbox_root: impl Into<PathBuf>,
    processor: Arc<dyn DeliveryProcessor>,
) -> Result<(), ListenerError> {
    run_listener(WebhookListener::open(socket, inbox_root)?.with_processor(processor)).await
}

/// Shared body of the two `run_until_signal*` wrappers.
async fn run_listener(listener: WebhookListener) -> Result<(), ListenerError> {
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
