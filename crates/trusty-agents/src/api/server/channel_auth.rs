//! Authorization and audit for channel WRITES, HTTP and tool alike (#7609).
//!
//! Why: a channel binding decides which assistant an inbound message wakes and
//! which destination an outbound one reaches, so whoever can write one can
//! redirect every assistant's traffic. The rest of this API leans on the
//! loopback bind as its control (#3329), which is enough for a surface that
//! spawns work on the caller's own behalf and is not enough here: any process
//! on the host — or any page the operator's browser loads that finds a way past
//! the same-origin guard — would be able to re-point the mailbox. This module
//! is the one extra control: a channel write requires the daemon to have been
//! started with a bearer token, and every accepted write leaves an audit line.
//! What: [`ChannelWriteAuth`] is the per-router fact ("was a token configured"),
//! inserted by `routes::build_router_with_origins`. [`ChannelWriter`] is the
//! extractor every channel write handler takes FIRST — it 401s a tokenless
//! daemon before the body is even parsed, and carries the caller identity the
//! audit line names. [`daemon_token_configured`] answers the same question for
//! the in-process `channel` tool, which has no request to extract from; it is
//! recorded once by `routes::serve_with_config`.
//!
//! Reads are untouched: the gate is on the write handlers only, and the
//! router-wide same-origin guard is unchanged.
//! Test: `crate::api::server::tests::global_channels` — the tokenless-refusal
//! and audit cases; `writes_are_refused_until_a_token_is_recorded`.

use std::sync::atomic::{AtomicBool, Ordering};

use axum::{
    Json,
    extract::{ConnectInfo, FromRequestParts},
    http::{StatusCode, request::Parts},
};
use serde_json::{Value, json};

/// The copy every refusal answers with, HTTP and tool alike.
const REFUSAL: &str = "Channel writes require an API token. Start the daemon with --api-token (or \
                       TAGENT_API_TOKEN); a tokenless daemon accepts reads only.";

/// Whether THIS router was built with a bearer token configured.
///
/// Why: a per-router fact rather than a process global, so a test can build one
/// tokenless and one token-bearing router in the same process and get a
/// deterministic answer from each.
#[derive(Clone, Copy, Debug)]
pub(super) struct ChannelWriteAuth {
    pub(super) token_configured: bool,
}

/// Whether the RUNNING daemon was started with a bearer token (#7609).
///
/// Why: the `channel` tool's write actions take the same gate as the HTTP
/// routes, and a tool call has no request to read an extension from. Recorded
/// once by `routes::serve_with_config`; `false` until then, which is the safe
/// default for a process that is not serving an authenticated API at all.
static DAEMON_TOKEN_CONFIGURED: AtomicBool = AtomicBool::new(false);

/// Record whether this process serves an authenticated API.
///
/// Why/What: see [`DAEMON_TOKEN_CONFIGURED`]. Called once per process, before
/// the listener starts accepting.
/// Test: `writes_are_refused_until_a_token_is_recorded`.
pub(super) fn record_daemon_token(configured: bool) {
    DAEMON_TOKEN_CONFIGURED.store(configured, Ordering::Relaxed);
}

/// Whether the in-process `channel` tool may perform a write action.
///
/// Test: `writes_are_refused_until_a_token_is_recorded`.
pub(crate) fn daemon_token_configured() -> bool {
    DAEMON_TOKEN_CONFIGURED.load(Ordering::Relaxed)
}

/// The refusal a tool write answers with when [`daemon_token_configured`] is
/// false, in the `<status>: <message>` shape this crate's tools already use.
///
/// Test: `the_tool_refuses_a_write_on_a_tokenless_daemon`.
pub(crate) fn tool_refusal() -> String {
    format!("401 Unauthorized: {REFUSAL}")
}

/// An authorized channel writer, plus the caller identity the audit names.
///
/// Why: extracting it is the gate. A handler that takes a `ChannelWriter` as
/// its first argument cannot run on a tokenless daemon, and cannot forget to
/// audit — the only way to get one is to pass the check, and the only thing it
/// does is emit the line.
/// Test: `crate::api::server::tests::global_channels::a_tokenless_daemon_refuses_every_channel_write`.
pub(super) struct ChannelWriter {
    remote_addr: String,
}

impl<S> FromRequestParts<S> for ChannelWriter
where
    S: Send + Sync,
{
    type Rejection = (StatusCode, Json<Value>);

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        // #7609: channel writes redirect inbound traffic for every assistant;
        // never tokenless.
        let configured = parts
            .extensions
            .get::<ChannelWriteAuth>()
            .is_some_and(|auth| auth.token_configured);
        let remote_addr = parts
            .extensions
            .get::<ConnectInfo<std::net::SocketAddr>>()
            .map_or_else(|| "unknown".to_string(), |info| info.0.to_string());
        if !configured {
            tracing::warn!(
                audit = "channel-write-refused",
                path = %parts.uri.path(),
                remote_addr = %remote_addr,
                "refused a channel write on a daemon with no API token configured (#7609)"
            );
            return Err((StatusCode::UNAUTHORIZED, Json(json!({ "error": REFUSAL }))));
        }
        Ok(Self { remote_addr })
    }
}

impl ChannelWriter {
    /// Say that a write was accepted, and by whom.
    ///
    /// Why: the write itself leaves no trace an operator can correlate — the
    /// file it changes carries no history — so this line is the record.
    /// What: one `info` event per accepted write, naming the route, the scope,
    /// the assistant when the write was scoped to one, the stored count either
    /// side of the write, and the caller identity actually available (the
    /// remote address when `ConnectInfo` is wired, `unknown` otherwise; the
    /// token is not logged, only that one was required).
    /// Test: `crate::api::server::tests::global_channels::a_token_bearing_channel_write_is_admitted_and_audited`.
    pub(super) fn audit(
        &self,
        route: &str,
        scope: &str,
        assistant: Option<&str>,
        before: usize,
        after: usize,
    ) {
        audit_write(route, scope, assistant, before, after, &self.remote_addr);
    }
}

/// The audit line itself, shared by the HTTP writer and the tool.
///
/// Why: the tool has no `ChannelWriter` — it never saw a request — but it owes
/// the same record, and two `tracing::info!` sites would drift.
/// Test: `crate::api::server::tests::global_channels::a_token_bearing_channel_write_is_admitted_and_audited`.
pub(crate) fn audit_write(
    route: &str,
    scope: &str,
    assistant: Option<&str>,
    before: usize,
    after: usize,
    remote_addr: &str,
) {
    tracing::info!(
        audit = "channel-write",
        route = route,
        scope = scope,
        assistant = assistant.unwrap_or("-"),
        channels_before = before,
        channels_after = after,
        token_configured = true,
        remote_addr = remote_addr,
        "channel configuration written (#7609)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tool gate defaults closed and follows what the daemon recorded.
    ///
    /// Why: `false` until `serve_with_config` says otherwise is the whole
    /// safety property — a REPL process, a test binary, or a daemon started
    /// without `--api-token` must all refuse.
    #[test]
    fn writes_are_refused_until_a_token_is_recorded() {
        // Serialized against the other users of this flag by holding the same
        // lock every `$HOME`-mutating test takes; nothing else writes it.
        let _guard = crate::test_env::lock_home();
        let restore = daemon_token_configured();
        record_daemon_token(false);
        assert!(!daemon_token_configured(), "closed by default");
        assert!(tool_refusal().starts_with("401 Unauthorized: "));
        record_daemon_token(true);
        assert!(daemon_token_configured(), "open once a token is recorded");
        record_daemon_token(restore);
    }
}
