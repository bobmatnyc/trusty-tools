//! `trusty-analyze`'s webhook UDS listener (#5182, ADR-0034 §1).
//!
//! Why: `trusty-console` has been relaying verified GitHub deliveries to
//! `trusty-analyze-webhook.sock` since #5089 step 3, and nothing bound it. Every
//! delivery landed in `RelayOutcome::Unreachable` and stayed pending forever —
//! durable, but never reviewed. This binds the receive end.
//!
//! What: the socket path and the inbox root, and the `run` that serves them.
//! Everything else — the frame contract, the ack ordering, the peer check, the
//! singleton bind — lives in `trusty_common::webhook_relay`, shared with
//! `trusty-review`, because a per-crate copy of an ordering rule is a per-crate
//! chance to get it wrong.
//!
//! 🔴 The delivery is fsync'd to [`inbox_root`] BEFORE the ack is written, and
//! the ack is the only thing that lets console delete its own copy. This process
//! is short-lived and console-supervised: it binds on demand, takes durable
//! ownership, and exits on SIGTERM. Draining the inbox into the analysis pipeline
//! is a separate step; what matters here is that nothing is lost while it waits.
//!
//! The legacy HTTP route (`POST /webhooks/github`, `service::handlers::review`) is
//! untouched and still live. Retiring it is
//! [#5181](https://github.com/bobmatnyc/trusty-tools/issues/5181); both paths
//! coexist until then.
//!
//! Test: `webhook_listener_tests.rs`.

use std::path::PathBuf;

use trusty_common::webhook_relay::{analyze_socket_path, WebhookListener};

/// Directory name the inbox occupies under the crate's data directory.
const INBOX_DIR_NAME: &str = "webhook-inbox";

/// The socket console dials for this service.
///
/// Why: resolved from the shared contract rather than spelled here, so the
/// sender and the receiver cannot disagree about the path — the failure mode
/// that disagreement produces is a delivery that silently never arrives.
/// Test: `socket_path_matches_the_shared_contract`.
pub fn socket_path() -> PathBuf {
    analyze_socket_path()
}

/// Where durably-owned deliveries are held until they are reviewed.
///
/// # Errors
///
/// When the platform data directory cannot be resolved or created.
///
/// Test: `inbox_root_lives_under_the_review_data_dir`.
pub fn inbox_root() -> anyhow::Result<PathBuf> {
    Ok(trusty_common::resolve_data_dir("trusty-analyze")?.join(INBOX_DIR_NAME))
}

/// Build the listener without binding, so a caller can inspect it first.
///
/// # Errors
///
/// [`trusty_common::webhook_relay::ListenerError::Inbox`] when the inbox cannot be prepared, or an
/// `anyhow::Error` when the data directory cannot be resolved.
///
/// Test: `listener_opens_against_a_temp_inbox`.
pub fn listener() -> anyhow::Result<WebhookListener> {
    Ok(WebhookListener::open(socket_path(), inbox_root()?)?)
}

/// Bind the socket and serve until SIGTERM or SIGINT.
///
/// Why: the whole body of `trusty-analyze webhook-listen`, which is what console
/// spawns on demand. It exits when signalled rather than running resident,
/// which is milestone `tm 1.3.5` criterion (c).
///
/// # Errors
///
/// When the inbox cannot be prepared, or the socket cannot be bound — including
/// when another instance already serves it, which is refused rather than taken
/// over.
///
/// Test: the serving behaviour is covered in `trusty-common`
/// (`listener_serves_a_delivery_and_cleans_up_its_socket`); this wrapper carries
/// only the two path resolutions, which have their own tests.
pub async fn run() -> anyhow::Result<()> {
    let socket = socket_path();
    let inbox = inbox_root()?;
    tracing::info!(
        socket = %socket.display(),
        inbox = %inbox.display(),
        "starting the trusty-analyze webhook listener"
    );
    trusty_common::webhook_relay::run_until_signal(socket, inbox).await?;
    Ok(())
}

#[cfg(test)]
#[path = "webhook_listener_tests.rs"]
mod tests;
