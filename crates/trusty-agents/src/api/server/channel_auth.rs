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
//! What: [`ChannelWriteAuth`] carries the CREDENTIAL a channel write must
//! present, inserted by `routes::build_router_with_origins`. [`ChannelWriter`]
//! is the extractor every channel write handler takes FIRST — it verifies that
//! credential before the body is even parsed, and carries the caller identity
//! the audit line names. [`daemon_credential`] answers the same question for the
//! in-process `channel` tool, which has no request to extract from.
//!
//! The credential is the operator's `--api-token` when there is one. When there
//! is not, and the bind is loopback, `serve_with_config` MINTS an ephemeral one
//! at boot ([`mint_ephemeral`]) and hands it to the UI it serves — see
//! `routes::config_route`. That keeps the served Channels tab working on the
//! default tokenless daemon (critic HIGH-4) without installing the router-wide
//! `auth_middleware`, which would have broken every other client of a
//! historically open loopback API. A credential that is never handed out — a
//! process that serves no API at all — refuses every write, which is the safe
//! default.
//!
//! Reads are untouched: the gate is on the write handlers only, and the
//! router-wide same-origin guard is unchanged.
//! Test: `crate::api::server::tests::global_channels` — the tokenless-refusal
//! and audit cases; `writes_are_refused_until_a_token_is_recorded`.

use std::sync::RwLock;

use axum::{
    Json,
    extract::{ConnectInfo, FromRequestParts},
    http::{StatusCode, header, request::Parts},
};
use serde_json::{Value, json};

/// The copy every refusal answers with, HTTP and tool alike.
const REFUSAL: &str = "Channel writes require an API token. Start the daemon with --api-token (or \
                       TAGENT_API_TOKEN); this process accepts channel reads only.";

/// The credential THIS router requires on a channel write.
///
/// Why: a per-router value rather than a process global, so a test can build
/// one tokenless and one token-bearing router in the same process and get a
/// deterministic answer from each.
#[derive(Clone, Debug)]
pub(super) struct ChannelWriteAuth {
    pub(super) credential: Option<String>,
}

/// The credential THIS PROCESS accepts on a channel write (#7609).
///
/// Why: the `channel` tool's write actions take the same gate as the HTTP
/// routes, and a tool call has no request to read an extension from. Recorded
/// at process start — by `runtime::startup` for every process, and again by
/// `routes::serve_with_config` once an ephemeral credential has been minted
/// (critic HIGH-3: recording it only in the serve path left a REPL with
/// `TAGENT_API_TOKEN` exported permanently refusing its own writes).
/// `None` until then, which is the safe default.
static DAEMON_CREDENTIAL: RwLock<Option<String>> = RwLock::new(None);

/// Record the credential this process accepts on a channel write.
///
/// Why/What: see [`DAEMON_CREDENTIAL`]. Idempotent; the last caller wins, and
/// the two callers agree except that `serve_with_config` may upgrade `None` to
/// a minted credential.
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

/// A fresh 32-byte credential, hex-encoded.
///
/// Why (critic HIGH-4): `tagent --api` defaults tokenless and no sidecar spawn
/// passes `--api-token`, so gating channel writes on a CONFIGURED token alone
/// would 401 the Channels tab the daemon itself serves. A loopback daemon mints
/// one instead and hands it to that UI, so the write is still authenticated —
/// against a secret an off-host caller cannot have — without turning on
/// router-wide auth for every other client.
/// What: 32 bytes from the OS RNG as 64 hex characters. Regenerated per boot, so
/// it cannot be stolen from a stale file and replayed against the next process.
/// Test: `an_ephemeral_credential_is_unique_per_call`.
pub(super) fn mint_ephemeral() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().fold(String::with_capacity(64), |mut out, b| {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// The refusal a tool write answers with when [`daemon_token_configured`] is
/// false, in the `<status>: <message>` shape this crate's tools already use.
///
/// Test: `the_tool_refuses_a_write_on_a_tokenless_daemon`.
pub(crate) fn tool_refusal() -> String {
    format!("401 Unauthorized: {REFUSAL}")
}

/// The credential a daemon with this configuration accepts on a channel write.
///
/// Why (#7609, critic HIGH-4): the rule is a decision, so it is a function and
/// not three lines inside `serve_with_config` that no test can reach. A
/// configured token IS the credential and disables minting. A tokenless
/// LOOPBACK bind mints one per boot, because the UI the daemon serves has to be
/// able to save and an off-host caller cannot obtain the value. A tokenless
/// non-loopback bind gets `None` and refuses every channel write — that
/// configuration is already refused outright by `serve_with_config`, and
/// minting for it would hand a LAN-reachable surface a credential the operator
/// never chose.
/// Test: `the_minting_rule_follows_the_bind`.
pub(super) fn channel_write_credential(
    configured: Option<String>,
    bind: std::net::IpAddr,
) -> Option<String> {
    configured.or_else(|| bind.is_loopback().then(mint_ephemeral))
}

/// The file the channel-write credential is published to, beside `http_addr`.
///
/// Why NOT inside `http_addr` itself: `trusty_common::read_daemon_addr` returns
/// that file's whole trimmed contents as an address, and
/// `resolve_daemon_base_url` builds a URL from it — appending anything would
/// break every existing reader. This is the same discovery DIRECTORY, resolved
/// by the same `trusty_common::data_dir`, so a local client that already finds
/// `http_addr` finds this beside it.
const CREDENTIAL_FILENAME: &str = "channel_write_credential";

fn credential_path() -> anyhow::Result<std::path::PathBuf> {
    Ok(trusty_common::data_dir::resolve_data_dir("trusty-agents")?.join(CREDENTIAL_FILENAME))
}

/// Publish the channel-write credential for local, non-browser clients.
///
/// Why (#7609, critic HIGH-4): the served UI reads the credential from
/// `/api/config`, but a client with no browser — the trusty-console proxy, a
/// script, the Tauri sidecar's Rust half — has no such bootstrap. The discovery
/// directory this daemon already writes `http_addr` into is where such a client
/// looks for it.
/// What: writes the credential owner-only (`0600` at creation on unix; the
/// directory is inside the per-user profile elsewhere). Best-effort: a failure
/// is logged by the caller and costs those clients the credential, never the
/// daemon's start.
/// Test: `a_published_credential_is_owner_only_and_removed`.
pub(super) fn publish_credential(credential: &str) -> anyhow::Result<()> {
    use std::io::Write as _;
    let path = credential_path()?;
    // Remove first so the mode below applies to a file this call CREATES,
    // rather than leaving a pre-existing wider mode in place.
    let _ = std::fs::remove_file(&path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    file.write_all(credential.as_bytes())?;
    Ok(())
}

/// Remove the published credential. Best-effort; a stale file is a credential
/// that no longer authorizes anything, because the next boot mints a new one.
pub(super) fn remove_published_credential() {
    if let Ok(path) = credential_path() {
        let _ = std::fs::remove_file(path);
    }
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
        let expected = parts
            .extensions
            .get::<ChannelWriteAuth>()
            .and_then(|auth| auth.credential.clone());
        let remote_addr = parts
            .extensions
            .get::<ConnectInfo<std::net::SocketAddr>>()
            .map_or_else(|| "unknown".to_string(), |info| info.0.to_string());
        let presented = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .map(str::trim);
        // The credential is verified HERE and not only by `auth_middleware`,
        // because on a tokenless loopback daemon that middleware is not
        // installed at all — the minted credential would otherwise gate nothing.
        let authorized = match (&expected, presented) {
            (Some(expected), Some(presented)) => {
                super::auth::bearer_token_matches(expected, presented)
            }
            _ => false,
        };
        if !authorized {
            tracing::warn!(
                audit = "channel-write-refused",
                path = %parts.uri.path(),
                remote_addr = %remote_addr,
                credential_available = expected.is_some(),
                credential_presented = presented.is_some(),
                "refused a channel write: no matching channel-write credential (#7609)"
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

    /// Minting follows the bind, and never overrides a configured credential.
    #[test]
    fn the_minting_rule_follows_the_bind() {
        let loopback = std::net::IpAddr::from(std::net::Ipv4Addr::LOCALHOST);
        let lan = std::net::IpAddr::from(std::net::Ipv4Addr::new(192, 168, 1, 10));

        assert_eq!(
            channel_write_credential(Some("operator".into()), loopback).as_deref(),
            Some("operator"),
            "a configured credential disables minting"
        );
        assert_eq!(
            channel_write_credential(Some("operator".into()), lan).as_deref(),
            Some("operator")
        );
        let minted = channel_write_credential(None, loopback).expect("loopback mints");
        assert_eq!(minted.len(), 64);
        assert_eq!(
            channel_write_credential(None, lan),
            None,
            "a tokenless LAN bind mints nothing"
        );
    }

    /// A published credential is owner-only and goes away on request.
    ///
    /// Why: it is a secret in a shared-machine directory, so the mode is part
    /// of the contract, and a file that outlived its process would be a
    /// credential nothing accepts.
    #[test]
    fn a_published_credential_is_owner_only_and_removed() {
        let _guard = crate::test_env::lock_home();
        let home = tempfile::tempdir().expect("tempdir");
        unsafe {
            std::env::set_var("HOME", home.path());
        }
        publish_credential("deadbeef").expect("publish");
        let path = credential_path().expect("path");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "deadbeef");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "owner-only");
        }
        // Republishing over an existing file keeps the mode.
        publish_credential("cafe").expect("republish");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "cafe");
        remove_published_credential();
        assert!(!path.exists(), "removed with the daemon");
    }

    /// A minted credential is 64 hex characters and never repeats.
    ///
    /// Why: it is the only thing standing between a loopback caller and a
    /// channel write on a tokenless daemon, so a predictable or reused value
    /// would make the gate decorative.
    #[test]
    fn an_ephemeral_credential_is_unique_per_call() {
        let first = mint_ephemeral();
        let second = mint_ephemeral();
        assert_eq!(first.len(), 64);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }
}
