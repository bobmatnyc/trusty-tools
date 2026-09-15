//! Authorization and audit for channel WRITES, HTTP and tool alike (#7609).
//!
//! Why: a channel binding decides which assistant an inbound message wakes and
//! which destination an outbound one reaches, so whoever can write one can
//! redirect every assistant's traffic. The rest of this API leans on the
//! loopback bind as its control (#3329), which is enough for a surface that
//! spawns work on the caller's own behalf and is not enough here.
//!
//! What this control DOES achieve: an off-host caller cannot write a channel —
//! it has neither the operator token nor a credential minted for this boot —
//! and a page served from a NON-LOOPBACK origin cannot read the minted
//! credential, because `/api/config` withholds it from such an `Origin` and the
//! same-origin CORS layer withholds the response body besides.
//!
//! What it does NOT achieve, and the scope is worth stating precisely: EVERY
//! loopback origin is trusted. `routes::same_origin_ok` admits any
//! `http://127.0.0.1:*` / `http://localhost:*`, and `same_origin_cors` reflects
//! them, so a page the operator happens to have open on another local port —
//! a dev server on `:5173`, say — can read the credential. That is the same
//! same-user carve-out as the paragraph below rather than a separate gap: a
//! process running as the operator is trusted throughout. It can read the
//! operator's own environment just as easily. Excluding it would need an
//! OS-level capability this codebase does not have, and the loopback doctrine
//! already assumes the same-user boundary everywhere else.
//! What: [`ChannelWriteAuth`] carries the two credentials a channel write may
//! present, inserted by `routes::build_router_with_origins`. [`ChannelWriter`]
//! is the extractor every channel write handler takes FIRST — it verifies the
//! presented bearer against either, before the body is even parsed, and carries
//! the caller identity the audit line names. [`daemon_credential`] answers the
//! same question for the in-process `channel` tool, which has no request to
//! extract from.
//!
//! TWO credentials, and the distinction is the whole security property (critic
//! round 3, CRITICAL):
//!
//! - The OPERATOR token (`--api-token` / `TAGENT_API_TOKEN`) authorizes the whole
//!   API. It is accepted on a channel write and is NEVER disclosed by any
//!   route and NEVER written to disk. An earlier revision disclosed it on
//!   `/api/config` — which `auth_middleware` exempts — so `curl` against a
//!   LAN-bound daemon with no `Origin` header read the operator's token and
//!   then had the whole API.
//! - The MINTED credential ([`mint_ephemeral`]) exists only on a LOOPBACK bind,
//!   is regenerated per boot, and authorizes CHANNEL WRITES and nothing else.
//!   It is the one the served UI reads from `/api/config`, which is the only
//!   way it leaves the process. It is minted whether or not an operator token is
//!   configured, so the UI never needs the operator's secret to save.
//!
//! One layering consequence, stated because it is easy to read the above as
//! more than it is: on a daemon that DOES have an operator token,
//! `auth_middleware` wraps every `/api/*` route from the outside, so a request
//! carrying only the minted credential is refused before [`ChannelWriter`] ever
//! runs. The minted credential is therefore what a TOKENLESS daemon's UI uses;
//! on a tokened one the UI already holds the operator token for every other
//! call and presents that. [`ChannelWriter`] accepts either regardless, which
//! is what keeps this gate correct on its own terms rather than depending on a
//! middleware above it.
//! Test: `a_configured_token_is_never_disclosed_on_the_config_probe`.
//!
//! A router with neither refuses every write, which is the safe default for a
//! non-loopback bind and for a process that serves no API at all.
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

/// The credentials THIS router accepts on a channel write.
///
/// Why: a per-router value rather than a process global, so a test can build
/// one tokenless and one token-bearing router in the same process and get a
/// deterministic answer from each. The two fields are NOT interchangeable —
/// see the module doc: `minted` is disclosed and published, `operator` never
/// is.
#[derive(Clone, Debug, Default)]
pub(super) struct ChannelWriteAuth {
    /// The operator's own API token, accepted here and never disclosed.
    pub(super) operator: Option<String>,
    /// This boot's channel-write credential, minted for a loopback bind.
    pub(super) minted: Option<String>,
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

/// This boot's minted channel-write credential, if the bind admits one.
///
/// Why (#7609, critic round 3): the rule is a decision, so it is a function and
/// not two lines inside `serve_with_config` that no test can reach. A LOOPBACK
/// bind mints one per boot — whether or not an operator token is configured,
/// because the UI the daemon serves must be able to save WITHOUT being handed
/// the operator's own secret. A non-loopback bind mints nothing: the value is
/// disclosed on `/api/config`, and `auth_middleware` exempts that route, so a
/// minted credential on a LAN-reachable surface would be readable by anyone who
/// can reach the port.
/// What: `Some` iff `bind` is loopback. The operator token is deliberately not
/// an input — it is accepted by [`ChannelWriter`] and never disclosed, and
/// conflating the two is precisely the defect this signature prevents.
/// Test: `the_minting_rule_follows_the_bind`.
pub(super) fn minted_credential(bind: std::net::IpAddr) -> Option<String> {
    bind.is_loopback().then(mint_ephemeral)
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
        let auth = parts.extensions.get::<ChannelWriteAuth>().cloned();
        let accepted: Vec<String> = auth
            .map(|a| [a.operator, a.minted].into_iter().flatten().collect())
            .unwrap_or_default();
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
        // EITHER credential is accepted: the operator's token authorizes the
        // whole API and so certainly this, and the minted one authorizes exactly
        // this and nothing else.
        let authorized = presented.is_some_and(|presented| {
            accepted
                .iter()
                .any(|expected| super::auth::bearer_token_matches(expected, presented))
        });
        if !authorized {
            tracing::warn!(
                audit = "channel-write-refused",
                path = %parts.uri.path(),
                remote_addr = %remote_addr,
                credentials_available = accepted.len(),
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
    /// Test: `crate::api::server::tests::global_channels::a_credentialed_channel_write_is_admitted_and_audited`.
    pub(super) fn audit(
        &self,
        route: &str,
        scope: &str,
        assistant: Option<&str>,
        before: Option<usize>,
        after: usize,
    ) {
        audit_write(route, scope, assistant, before, after, &self.remote_addr);
    }
}

/// The audit line itself, shared by the HTTP writer and the tool.
///
/// Why: the tool has no `ChannelWriter` — it never saw a request — but it owes
/// the same record, and two `tracing::info!` sites would drift.
/// Test: `crate::api::server::tests::global_channels::a_credentialed_channel_write_is_admitted_and_audited`,
/// `crate::api::server::tests::global_channels::the_listener_alias_audits_an_accepted_write`.
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

    /// Minting follows the BIND and nothing else.
    ///
    /// Why (critic round 3): an earlier signature took the operator token as an
    /// input and returned it when present, which is how that token ended up
    /// disclosed on `/api/config`. Taking only the bind makes that
    /// unrepresentable.
    #[test]
    fn the_minting_rule_follows_the_bind() {
        let loopback = std::net::IpAddr::from(std::net::Ipv4Addr::LOCALHOST);
        let lan = std::net::IpAddr::from(std::net::Ipv4Addr::new(192, 168, 1, 10));

        let minted = minted_credential(loopback).expect("loopback mints");
        assert_eq!(minted.len(), 64);
        assert_ne!(
            minted_credential(loopback).expect("again"),
            minted,
            "a fresh value per call"
        );
        assert_eq!(
            minted_credential(lan),
            None,
            "a non-loopback bind mints nothing, whatever else is configured"
        );
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
