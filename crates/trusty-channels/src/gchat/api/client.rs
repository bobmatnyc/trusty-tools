//! GchatClient: Chat message create plus Pub/Sub pull and acknowledge, all
//! authenticated with one service-account token.
//!
//! Why: S2 (#9448) builds the outbound route and the inbound question ledger
//! on these three calls. Keeping them in one client with one token cache
//! means S2 never handles a key or a token itself.
//! What: [`GchatClient`] loads the key at construction (failing before any
//! network call on a bad key file), validates resource names, attaches the
//! bearer token, and maps non-2xx responses to [`GchatError::Http`] with the
//! token redacted. A 401 also drops the cached token. Hosts are overridable
//! through [`Endpoints`] so tests run against wiremock.
//! Test: `src/gchat/tests/client_send.rs` —
//! `create_message_sends_bearer_path_and_thread_key`,
//! `create_message_non_2xx_is_typed_and_never_contains_the_token`,
//! `unauthorized_chat_call_drops_the_cached_token`; `tests/gchat_http.rs` —
//! `pull_decodes_events_and_isolates_malformed_messages`,
//! `acknowledge_sends_exactly_the_given_ack_ids`; here
//! `resource_names_reject_path_injection`.

use std::path::Path;

use serde::de::{DeserializeOwned, IgnoredAny};
use serde::{Deserialize, Serialize};

use crate::gchat::api::auth::{Clock, ServiceAccountKey, TokenSource};
use crate::gchat::api::constants::{CHAT_API_BASE, HTTP_TIMEOUT, PUBSUB_API_BASE, TOKEN_URL};
use crate::gchat::api::error::{error_message, transport_error, GchatError};
use crate::gchat::api::events::{PullResponse, PulledMessage, ThreadRef};

/// Where the three Google hosts live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// OAuth token endpoint (full URL).
    pub token_url: String,
    /// Chat API host root, without `/v1`.
    pub chat_base: String,
    /// Pub/Sub API host root, without `/v1`.
    pub pubsub_base: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            token_url: TOKEN_URL.to_string(),
            chat_base: CHAT_API_BASE.to_string(),
            pubsub_base: PUBSUB_API_BASE.to_string(),
        }
    }
}

impl Endpoints {
    /// All three APIs on one host, with the token endpoint at `{base}/token`
    /// (tests point this at a mock server).
    pub fn single_host(base: &str) -> Self {
        let base = base.trim_end_matches('/');
        Self {
            token_url: format!("{base}/token"),
            chat_base: base.to_string(),
            pubsub_base: base.to_string(),
        }
    }
}

/// `messageReplyOption` for `spaces.messages.create`. Chat honours it only in
/// named spaces, and only together with a thread key or thread name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageReplyOption {
    /// Reply in the keyed thread; start a new thread if that fails.
    ReplyFallbackToNewThread,
    /// Reply in the keyed thread, or fail with `NOT_FOUND`.
    ReplyOrFail,
}

impl MessageReplyOption {
    /// The wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReplyFallbackToNewThread => "REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD",
            Self::ReplyOrFail => "REPLY_MESSAGE_OR_FAIL",
        }
    }
}

/// A text message to post as the Chat app.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreateMessage {
    /// Target space, `spaces/{space}`.
    pub space: String,
    /// Message text (Chat caps a message at 32,000 bytes).
    pub text: String,
    /// App-defined thread key; sent as the body's `thread.threadKey`.
    pub thread_key: Option<String>,
    /// Whether the message replies in the keyed thread. `None` with a
    /// `thread_key` set sends `ReplyFallbackToNewThread`: Chat's default
    /// (`MESSAGE_REPLY_OPTION_UNSPECIFIED`) ignores the thread key.
    pub reply_option: Option<MessageReplyOption>,
    /// Idempotency key: a retry with the same id returns the first message.
    pub request_id: Option<String>,
}

/// The created message, as Chat returns it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    /// Message resource name, `spaces/{space}/messages/{message}`.
    #[serde(default)]
    pub name: String,
    /// Message text.
    #[serde(default)]
    pub text: Option<String>,
    /// The thread the message landed in.
    #[serde(default)]
    pub thread: Option<ThreadRef>,
    /// Creation time (RFC 3339).
    #[serde(default)]
    pub create_time: Option<String>,
}

#[derive(Serialize)]
struct MessageBody<'a> {
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread: Option<ThreadKeyBody<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ThreadKeyBody<'a> {
    thread_key: &'a str,
}

/// Authenticated client for Google Chat and Cloud Pub/Sub.
///
/// Why: the single handle S2 holds; it owns the key and the token cache.
/// What: a `reqwest::Client`, a [`TokenSource`] and the [`Endpoints`]. `Debug`
/// prints the service-account email and endpoints only.
/// Test: `debug_output_never_contains_key_material_or_token`.
#[derive(Debug)]
pub struct GchatClient {
    http: reqwest::Client,
    tokens: TokenSource,
    endpoints: Endpoints,
}

impl GchatClient {
    /// Build a client against Google's production hosts from a key-file path.
    ///
    /// Why: the bot's startup path; a bad key fails here, not on first use.
    /// What: see [`GchatClient::with_endpoints`].
    /// Test: `key_file_mode_0644_is_refused_before_any_request`.
    pub fn from_key_file(path: &Path) -> Result<Self, GchatError> {
        Self::with_endpoints(path, Endpoints::default())
    }

    /// Build a client from a key-file path against explicit hosts.
    ///
    /// Why: tests point every call at a mock server.
    /// What: loads the key via [`ServiceAccountKey::from_file`] (mode check
    /// first) and builds the HTTP client. Makes no network call.
    /// Test: `key_file_mode_0644_is_refused_before_any_request`.
    pub fn with_endpoints(path: &Path, endpoints: Endpoints) -> Result<Self, GchatError> {
        let key = ServiceAccountKey::from_file(path)?;
        let http = reqwest::Client::builder()
            .user_agent(concat!("trusty-channels-gchat/", env!("CARGO_PKG_VERSION")))
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(|e| transport_error("oauth2", &e))?;
        let tokens = TokenSource::new(key, http.clone(), endpoints.token_url.clone());
        Ok(Self {
            http,
            tokens,
            endpoints,
        })
    }

    /// Replace the token clock (tests).
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.tokens = self.tokens.with_clock(clock);
        self
    }

    /// Prove the key can obtain an access token, returning only its expiry.
    ///
    /// Why: doctor's token-mint column (#9448 D6) needs the check without
    /// any caller ever holding the bearer token.
    /// What: takes a token from the cache or mints one; returns
    /// `expires_at` (Unix seconds) and drops the token.
    /// Test: `doctor_mints_a_token_online_and_reports_in_use_state`.
    pub(crate) async fn check_token(&self) -> Result<u64, GchatError> {
        Ok(self.tokens.access_token().await?.expires_at())
    }

    /// The token source (crate tests only).
    // #9448 review: crate-private, so no public call hands out a `chat.bot`
    // bearer token without the route check.
    #[cfg(test)]
    pub(crate) fn token_source(&self) -> &TokenSource {
        &self.tokens
    }

    /// Post a text message to a space as the Chat app.
    ///
    /// Why: the outbound half of the question/answer loop; S2 sets a thread
    /// key per question so replies land in, and come back from, one thread.
    /// What: `POST {chat}/v1/{space}/messages` with body
    /// `{"text", "thread": {"threadKey"}}` and query `messageReplyOption` /
    /// `requestId` when set. The deprecated `threadKey` query parameter is not
    /// used; Chat's reference directs callers to `thread.threadKey`. A thread
    /// key with no reply option defaults to `ReplyFallbackToNewThread`, since
    /// Chat's own default ignores the key and would drop the binding.
    /// Test: `create_message_sends_bearer_path_and_thread_key`,
    /// `create_message_with_thread_key_defaults_to_fallback_reply`,
    /// `create_message_non_2xx_is_typed_and_never_contains_the_token`.
    // #9448 ruling 7: crate-private, so every send passes the route check in
    // `GchatChannel::send_question` / `send_review_notice`.
    pub(crate) async fn create_message(
        &self,
        request: &CreateMessage,
    ) -> Result<ChatMessage, GchatError> {
        validate_space(&request.space)?;
        let url = format!(
            "{}/v1/{}/messages",
            self.endpoints.chat_base.trim_end_matches('/'),
            request.space
        );
        let mut query: Vec<(&str, &str)> = Vec::new();
        // #9448: Chat's unspecified option starts a new thread and ignores the key.
        let reply_option = request.reply_option.or(request
            .thread_key
            .as_ref()
            .map(|_| MessageReplyOption::ReplyFallbackToNewThread));
        if let Some(option) = reply_option {
            query.push(("messageReplyOption", option.as_str()));
        }
        if let Some(id) = request.request_id.as_deref() {
            query.push(("requestId", id));
        }
        let body = MessageBody {
            text: &request.text,
            thread: request
                .thread_key
                .as_deref()
                .map(|thread_key| ThreadKeyBody { thread_key }),
        };
        self.post_json("chat", &url, &query, &body).await
    }

    /// Pull up to `max_messages` messages from a subscription.
    ///
    /// Why: the inbound transport; Pub/Sub pull needs no public URL.
    /// What: `POST {pubsub}/v1/{subscription}:pull` with
    /// `{"maxMessages": max(1, n)}` (the API requires a positive value). Each
    /// message's payload is parsed on its own; see [`PulledMessage`]. A
    /// response that is not the pull envelope at all fails the call.
    /// Test: `pull_decodes_events_and_isolates_malformed_messages`.
    pub async fn pull(
        &self,
        subscription: &str,
        max_messages: u32,
    ) -> Result<Vec<PulledMessage>, GchatError> {
        validate_subscription(subscription)?;
        let url = format!(
            "{}/v1/{}:pull",
            self.endpoints.pubsub_base.trim_end_matches('/'),
            subscription
        );
        let body = serde_json::json!({ "maxMessages": max_messages.max(1) });
        let response: PullResponse = self.post_json("pubsub", &url, &[], &body).await?;
        Ok(response.into_pulled())
    }

    /// Acknowledge handled messages so Pub/Sub stops redelivering them.
    ///
    /// Why: an unacknowledged message comes back after its ack deadline.
    /// What: `POST {pubsub}/v1/{subscription}:acknowledge` with
    /// `{"ackIds": ack_ids}`. An empty slice returns `Ok` with no request,
    /// because the API rejects an empty list.
    /// Test: `acknowledge_sends_exactly_the_given_ack_ids`.
    pub async fn acknowledge(
        &self,
        subscription: &str,
        ack_ids: &[String],
    ) -> Result<(), GchatError> {
        validate_subscription(subscription)?;
        if ack_ids.is_empty() {
            return Ok(());
        }
        let url = format!(
            "{}/v1/{}:acknowledge",
            self.endpoints.pubsub_base.trim_end_matches('/'),
            subscription
        );
        let body = serde_json::json!({ "ackIds": ack_ids });
        let _: IgnoredAny = self.post_json("pubsub", &url, &[], &body).await?;
        Ok(())
    }

    /// POST `body` as JSON with the bearer token and decode a 2xx response.
    async fn post_json<B, T>(
        &self,
        api: &'static str,
        url: &str,
        query: &[(&str, &str)],
        body: &B,
    ) -> Result<T, GchatError>
    where
        B: Serialize + ?Sized,
        T: DeserializeOwned,
    {
        let token = self.tokens.access_token().await?;
        let mut builder = self.http.post(url).bearer_auth(token.secret()).json(body);
        if !query.is_empty() {
            builder = builder.query(query);
        }
        let response = builder.send().await.map_err(|e| transport_error(api, &e))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| transport_error(api, &e))?;
        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED {
                self.tokens.invalidate().await;
            }
            return Err(GchatError::Http {
                api,
                status: status.as_u16(),
                message: error_message(&text, &[token.secret()]),
            });
        }
        serde_json::from_str(&text).map_err(|e| GchatError::Decode {
            api,
            reason: e.to_string(),
        })
    }
}

/// True when `s` is one non-empty resource-id segment: no `/`, `?`, `#`, `%`
/// or whitespace that could reshape the request URL, and not the dot segment
/// `.` or `..`, which URL parsing would remove or resolve away.
fn is_segment(s: &str, extra: &[char]) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') || extra.contains(&c)
        })
}

/// True when `name` is `spaces/{space}` with one safe id segment.
///
/// Why: the routes file's `space` field and every send target share one
/// rule (#9448), so a route cannot name a space a send would refuse.
/// What: the `spaces/` prefix, then [`is_segment`] with no extra characters.
/// Test: `resource_names_reject_path_injection`,
/// `space_field_loads_and_a_malformed_space_fails_the_load`.
pub(crate) fn is_space_name(name: &str) -> bool {
    name.strip_prefix("spaces/")
        .is_some_and(|id| is_segment(id, &[]))
}

/// Accept `spaces/{space}` only.
fn validate_space(name: &str) -> Result<(), GchatError> {
    if is_space_name(name) {
        Ok(())
    } else {
        Err(GchatError::InvalidName {
            name: name.to_string(),
            expected: "spaces/{space}",
        })
    }
}

/// Accept `projects/{project}/subscriptions/{subscription}` only. A project
/// may be domain-scoped (`example.com:proj`); a subscription id may hold `+`.
fn validate_subscription(name: &str) -> Result<(), GchatError> {
    let parts: Vec<&str> = name.split('/').collect();
    match parts.as_slice() {
        ["projects", project, "subscriptions", sub]
            if is_segment(project, &[':']) && is_segment(sub, &['+']) =>
        {
            Ok(())
        }
        _ => Err(GchatError::InvalidName {
            name: name.to_string(),
            expected: "projects/{project}/subscriptions/{subscription}",
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_names_reject_path_injection() {
        assert!(validate_space("spaces/AAAA-b_c").is_ok());
        for bad in [
            "spaces/",
            "spaces/A/messages",
            "spaces/A?x=1",
            "rooms/A",
            "spaces/a%2Fb",
            "spaces/.",
            "spaces/..",
        ] {
            assert!(validate_space(bad).is_err(), "{bad} accepted");
        }
        assert!(validate_subscription("projects/example.com:p-1/subscriptions/chat.in+1").is_ok());
        for bad in [
            "projects/p/subscriptions/",
            "projects/p/topics/t",
            "projects/p/subscriptions/s/x",
            "projects/p/subscriptions/s:pull",
            "projects/p q/subscriptions/s",
            "projects/./subscriptions/s",
            "projects/../subscriptions/s",
            "projects/p/subscriptions/.",
            "projects/p/subscriptions/..",
        ] {
            assert!(validate_subscription(bad).is_err(), "{bad} accepted");
        }
    }
}
