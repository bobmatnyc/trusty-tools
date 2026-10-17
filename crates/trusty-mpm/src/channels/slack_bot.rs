//! The daemon's Slack bot: `chat.postMessage` only (#9856, #8433 P4).
//!
//! Why: `ask_owner` must post one question DM with the daemon-held bot
//! token and nothing else. The `tm slack` bot (`crate::slack`) is a separate
//! product with its own token flow and stays apart. This is a Fail-Open
//! Check site: a send whose outcome is not known must never read as "not
//! sent", or the caller would reissue its question id.
//! What: [`bot_ref`] reads the host ceiling's Slack `bot_ref`;
//! [`resolve_bot_token`] resolves it through the credential registry and the
//! daemon's credential door (`secret_source`), and
//! refuses every key but [`SLACK_BOT_REF`]; [`SlackBot`] posts to
//! [`SLACK_API_BASE`], a constant (never an env var); [`classify`] turns a
//! reply into [`Posted`] or a [`SendFailure`].
//! Test: `slack_bot_tests` in `ask_owner_tests.rs`.

use std::path::Path;
use std::time::Duration;

use trusty_channels::policy::{Channel, HostChannel, parse_host};
use trusty_common::credential_registry::env_var_for;
use trusty_common::credentials::Secret;

/// The production Slack Web API base. Never read from the environment.
pub const SLACK_API_BASE: &str = "https://slack.com/api";

/// The one credential key whose token may post (#8454 S2c).
pub const SLACK_BOT_REF: &str = "slack";

/// The user-token key, refused on every channel path (#8454 ruling).
pub const SLACK_USER_REF: &str = "slack-user";

/// How long one `chat.postMessage` may take before its outcome is unknown.
pub const POST_TIMEOUT: Duration = Duration::from_secs(10);

/// A posted message: the DM channel Slack used and the message timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Posted {
    /// The DM channel id (`D…`).
    pub channel: String,
    /// The message `ts`; the question's thread.
    pub ts: String,
}

/// Why a post did not yield a [`Posted`].
///
/// Why: the caller keeps the question id reserved in every arm, and must
/// tell the asker whether the owner may have seen the message.
/// What: `Rejected` — Slack or the connection proves nothing was posted;
/// `Unknown` — a timeout, a 5xx or an unreadable reply, so it may have been;
/// `Unrecorded` — Slack accepted it, but the reply names no DM thread.
/// Test: `classify_maps_status_and_body`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendFailure {
    /// Nothing was posted; the text is a content-free code.
    Rejected(String),
    /// It may have been posted.
    Unknown(String),
    /// It was posted, but no DM thread can be recorded.
    Unrecorded(String),
}

/// Posts one message. The daemon's is [`SlackBot`]; tests count calls.
pub trait SlackPost: Send + Sync {
    /// `chat.postMessage` `text` to `channel` (a `U…` user id opens the DM).
    fn post(
        &self,
        token: &Secret<String>,
        channel: &str,
        text: &str,
    ) -> Result<Posted, SendFailure>;
}

/// The daemon's Slack Web API client.
#[derive(Debug, Clone)]
pub struct SlackBot {
    base: String,
}

impl SlackBot {
    /// The production client, over [`SLACK_API_BASE`].
    pub fn production() -> Self {
        Self::with_base(SLACK_API_BASE)
    }

    /// A client over `base`; a sandbox config names a loopback base.
    pub fn with_base(base: impl Into<String>) -> Self {
        Self { base: base.into() }
    }
}

impl SlackPost for SlackBot {
    /// Blocking: callers run it on the blocking pool.
    fn post(
        &self,
        token: &Secret<String>,
        channel: &str,
        text: &str,
    ) -> Result<Posted, SendFailure> {
        let client = reqwest::blocking::Client::builder()
            .timeout(POST_TIMEOUT)
            .build()
            .map_err(|_| SendFailure::Rejected("client_build".into()))?;
        let body = serde_json::json!({ "channel": channel, "text": text });
        let sent = client
            .post(format!("{}/chat.postMessage", self.base))
            .bearer_auth(token.expose())
            .json(&body)
            .send();
        let resp = match sent {
            Ok(resp) => resp,
            // A refused connect sent no request; anything later may have.
            Err(e) if e.is_connect() => return Err(SendFailure::Rejected("connect".into())),
            Err(e) if e.is_timeout() => return Err(SendFailure::Unknown("timeout".into())),
            Err(_) => return Err(SendFailure::Unknown("transport".into())),
        };
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .map_err(|_| SendFailure::Unknown("body".into()))?;
        classify(status, &text)
    }
}

/// The outcome of one `chat.postMessage` reply.
///
/// What: a 5xx or an undecodable 200 is `Unknown`; 429 and every other
/// non-200 is `Rejected` naming the status; `ok: false` is `Unknown` for
/// Slack's server-side codes (`fatal_error`, `internal_error`,
/// `service_unavailable`, `request_timeout`) and `Rejected` with Slack's
/// error code for every other; `ok: true` is `Posted` only with a `D…` channel and a
/// `ts`, else `Unrecorded`.
/// Test: `classify_maps_status_and_body`.
pub fn classify(status: u16, body: &str) -> Result<Posted, SendFailure> {
    if status >= 500 {
        return Err(SendFailure::Unknown(format!("http_{status}")));
    }
    if status != 200 {
        return Err(SendFailure::Rejected(format!("http_{status}")));
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Err(SendFailure::Unknown("undecodable_reply".into()));
    };
    if v.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        let code = v
            .get("error")
            .and_then(serde_json::Value::as_str)
            .map_or_else(|| "not_ok".to_owned(), code_of);
        // #9856: Slack's server-side error states may follow a partly
        // successful post, so they must not read as "not sent".
        if UNKNOWN_OUTCOME_CODES.contains(&code.as_str()) {
            return Err(SendFailure::Unknown(code));
        }
        return Err(SendFailure::Rejected(code));
    }
    let field = |k: &str| v.get(k).and_then(serde_json::Value::as_str).unwrap_or("");
    let (channel, ts) = (field("channel"), field("ts"));
    if !channel.starts_with('D') || ts.is_empty() {
        return Err(SendFailure::Unrecorded("no_dm_thread".into()));
    }
    Ok(Posted {
        channel: channel.to_owned(),
        ts: ts.to_owned(),
    })
}

/// Slack `ok: false` codes after which the post may have partly succeeded.
const UNKNOWN_OUTCOME_CODES: [&str; 4] = [
    "fatal_error",
    "internal_error",
    "service_unavailable",
    "request_timeout",
];

/// Slack's error code, kept to `[a-z0-9_]` and 64 chars.
fn code_of(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
        .take(64)
        .collect()
}

/// The host ceiling's Slack `bot_ref` under `home`.
///
/// Why: the bot ref is the host owner's choice, read where the daemon's
/// policy is read (`config.yaml`), never from a request.
/// What: reads and parses the host file; `Err` naming the fault when the
/// file is missing or refused, Slack is absent or disabled, or no `bot_ref`
/// is set. Blocking.
/// Test: `bot_ref_is_read_from_the_host_ceiling`.
pub fn bot_ref(home: Option<&Path>) -> Result<String, String> {
    host_slack(home)?
        .bot_ref()
        .map(str::to_owned)
        .ok_or_else(|| "the slack connection names no bot_ref".to_owned())
}

/// The host ceiling's enabled Slack channel under `home`.
///
/// What: reads and parses the host file; `Err` naming the fault when the
/// file is missing or refused, or Slack is absent or disabled. Blocking.
/// #9923: shared by [`bot_ref`] and the Socket Mode `app_ref` read.
/// Test: `bot_ref_is_read_from_the_host_ceiling`,
/// `daemon_holds_app_token_via_app_ref_only`.
pub fn host_slack(home: Option<&Path>) -> Result<HostChannel, String> {
    let home = home.ok_or("no home directory, so no host ceiling")?;
    let path = trusty_common::crate_config::crate_config_path_at(home, "trusty-mpm");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("host ceiling {} unreadable: {}", path.display(), e.kind()))?;
    let ceiling = parse_host(&text, Some(home)).map_err(|e| e.to_string())?;
    ceiling
        .channel(Channel::Slack)
        .filter(|c| c.enabled())
        .cloned()
        .ok_or_else(|| "slack is not enabled in the host ceiling".to_owned())
}

/// Resolve `bot_ref`'s token through the credential registry.
///
/// Why: #8454 A14: no user token is ever loaded on a channel path.
/// What: refuses any key but [`SLACK_BOT_REF`] (so [`SLACK_USER_REF`]
/// never resolves), maps the key to its registry variable, then reads it
/// through the daemon's one credential door,
/// `secret_source::resolve_store_first_gated` (#9121: nothing in a sandboxed
/// daemon; #9936: the store before env and `.env.local`, the same door as
/// `app_ref`). Blocking (Keychain). The error names the kind, never a value.
/// Test: `resolve_bot_token_refuses_the_user_token`,
/// `slack_refs_resolve_the_store_before_the_environment_9936`.
pub fn resolve_bot_token(bot_ref: &str) -> Result<Secret<String>, String> {
    check_bot_ref(bot_ref)?;
    let var = env_var_for(bot_ref)
        .ok_or_else(|| format!("bot_ref {bot_ref:?} is not in the credential registry"))?;
    // #9936: store first, so a session's env cannot pick the daemon's Slack app.
    crate::secret_source::resolve_store_first_gated(var)
        .map(Secret::new)
        .map_err(|e| format!("{var} could not be resolved: {}", e.kind()))
}

/// `Ok` only for [`SLACK_BOT_REF`]; the refusal names the key's role.
pub fn check_bot_ref(bot_ref: &str) -> Result<(), String> {
    match bot_ref {
        SLACK_BOT_REF => Ok(()),
        SLACK_USER_REF => Err("bot_ref names the slack user token, which is never used".into()),
        other => Err(format!("bot_ref {other:?} is not the slack bot token")),
    }
}
