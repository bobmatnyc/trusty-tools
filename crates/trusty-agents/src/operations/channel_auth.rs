//! Authorization and audit for channel WRITES made by the in-process
//! `channel` tool (#7609).
//!
//! Why: a channel binding decides which assistant an inbound message wakes and
//! which destination an outbound one reaches, so whoever can write one can
//! redirect every assistant's traffic. The tool's write actions are refused
//! until the process records a channel-write credential.
//! What: [`record_daemon_credential`] / [`daemon_credential`] hold the one
//! credential this process accepts; [`daemon_token_configured`] is the gate the
//! write paths read, [`tool_refusal`] the refusal they answer with, and
//! [`audit_write`] the record an accepted write leaves. #11632: the HTTP
//! extractor, the per-boot minted credential and the bind rule were cut with
//! the HTTP API; nothing records a credential in this release, so tool writes
//! stay refused, which is the safe default this gate already had.
//! Test: `writes_are_refused_until_a_credential_is_recorded`.
use std::sync::RwLock;

/// The copy every refusal answers with, HTTP and tool alike.
const REFUSAL: &str = "Channel writes require an API token. Start the daemon with --api-token (or \
                       TAGENT_API_TOKEN); this process accepts channel reads only.";

/// The credential THIS PROCESS accepts on a channel write (#7609).
///
/// Why: a tool call has no request to read a credential from, so the process
/// records one. `None` until then, which is the safe default.
static DAEMON_CREDENTIAL: RwLock<Option<String>> = RwLock::new(None);

/// Record the credential this process accepts on a channel write.
///
/// Why/What: see [`DAEMON_CREDENTIAL`]. Idempotent; the last caller wins.
/// Test: `writes_are_refused_until_a_credential_is_recorded`.
pub(crate) fn record_daemon_credential(credential: Option<String>) {
    if let Ok(mut slot) = DAEMON_CREDENTIAL.write() {
        *slot = credential;
    }
}

/// The credential this process accepts, if any.
///
/// Test: `writes_are_refused_until_a_credential_is_recorded`.
pub(crate) fn daemon_credential() -> Option<String> {
    DAEMON_CREDENTIAL.read().ok().and_then(|slot| slot.clone())
}

/// Whether the in-process `channel` tool may perform a write action.
///
/// Test: `writes_are_refused_until_a_credential_is_recorded`.
pub(crate) fn daemon_token_configured() -> bool {
    daemon_credential().is_some()
}

/// The refusal a tool write answers with when [`daemon_token_configured`] is
/// false, in the `<status>: <message>` shape this crate's tools already use.
///
/// Test: `the_tool_refuses_a_write_on_a_tokenless_daemon`.
pub(crate) fn tool_refusal() -> String {
    format!("401 Unauthorized: {REFUSAL}")
}

/// The audit line an accepted channel write leaves.
///
/// Why: the write itself leaves no trace an operator can correlate, so this
/// line is the record.
/// Test: `crate::operations::tests::global_channels::a_credentialed_channel_write_is_admitted_and_audited`,
/// `crate::operations::tests::global_channels::an_unreadable_before_count_audits_as_unknown`.
pub(crate) fn audit_write(
    route: &str,
    scope: &str,
    assistant: Option<&str>,
    before: Option<usize>,
    after: usize,
    remote_addr: &str,
) {
    // #7609 (critic round 3, MEDIUM-2): `before` is an Option because one
    // caller reads it from a file that can fail to load. Rendering that failure
    // as `0` made "the assistant had no bindings" and "we could not tell"
    // indistinguishable in the audit record — the one artifact an operator has
    // to reconstruct what a write changed.
    tracing::info!(
        audit = "channel-write",
        route = route,
        scope = scope,
        assistant = assistant.unwrap_or("-"),
        channels_before = before.map_or_else(|| "unknown".to_string(), |n| n.to_string()),
        channels_after = after,
        token_configured = true,
        remote_addr = remote_addr,
        "channel configuration written (#7609)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tool gate defaults closed and follows what the process recorded.
    ///
    /// Why: `None` until a credential is resolved is the whole safety property
    /// — a process that serves no API, and a daemon started without a token on
    /// a bind that cannot mint one, must both refuse.
    #[test]
    fn writes_are_refused_until_a_credential_is_recorded() {
        // Serialized against the other users of this slot by holding the same
        // lock every `$HOME`-mutating test takes; nothing else writes it.
        let _guard = crate::test_env::lock_home();
        let restore = daemon_credential();
        record_daemon_credential(None);
        assert!(!daemon_token_configured(), "closed by default");
        assert!(tool_refusal().starts_with("401 Unauthorized: "));
        record_daemon_credential(Some("abc".into()));
        assert_eq!(daemon_credential().as_deref(), Some("abc"));
        assert!(daemon_token_configured(), "open once one is recorded");
        record_daemon_credential(restore);
    }
}
