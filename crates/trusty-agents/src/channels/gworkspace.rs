//! Gmail as a two-way channel binding: inbound off the mailbox listener,
//! outbound through `trusty-gworkspace`'s own Gmail send.
//!
//! Why: Gmail already reached an assistant, but only down the listener path —
//! `listeners::poll` fetched a message, stored it, and `listeners::wake` woke
//! whichever agent declared a matching `[[listeners]]` binding. That path is
//! addressed by SUBSCRIBER: the agent config says which listener it follows,
//! and nothing in it names a correspondent. A channel binding is addressed the
//! other way round — the operator names a destination and the assistant answers
//! on it — which is what makes a Gmail conversation a DM-equivalent rather than
//! a feed. This adapter is that second addressing, reusing the listener's poll
//! for transport so there is no second mailbox reader.
//!
//! What: [`GworkspaceAdapter::receive`] consumes the `StoredEvent` the Gmail
//! poll already built, so nothing here talks to Gmail on the inbound side.
//! [`ChannelAdapter::addresses`] is what confines an event to a binding: a
//! target is `from:<address-or-glob>` or `label:<label-id>`, and a message
//! matching no binding is never claimed, so it falls through to the listener
//! wake exactly as before. Outbound is
//! `trusty_gworkspace::api::services::gmail::messages::compose_email` behind
//! [`GmailSend`], which exists because `compose_email` takes a concrete
//! `BaseClient` and writes `GMAIL_API_BASE` into its own URLs — there is no
//! endpoint to point at a mock server, so the seam is a trait in this crate
//! rather than a second HTTP client here.
//!
//! Credentials: a gworkspace binding names none. Gmail authenticates with the
//! per-account OAuth token in `trusty_gworkspace`'s `TokenStorage`, and the
//! credential registry (`trusty_common::credentials::REGISTRY`) has no key for
//! it — its one Google entry, `google-oauth`, is the OAuth *client secret* used
//! to refresh those tokens, not a credential an email can be sent as. So
//! [`ChannelAdapter::credential_providers`] is empty and `Binding::validate`
//! refuses any `credential_ref` on this provider. That refusal is also the
//! separate-grant rule of DOC-63 §7.1b in force: an OKG source's read-scoped
//! Google credential is a different grant from a channel's send credential, and
//! naming one here does not reach the other. Routing the gworkspace account
//! through the credential authority is deferred; when it lands, this adapter
//! gains a provider family and the refusal below becomes a confinement.
//!
//! Test: `gworkspace_adapter_addresses_sender_and_label_targets`,
//! `gworkspace_adapter_validates_target_grammar`,
//! `gworkspace_send_threads_a_reply_and_opens_a_fresh_message`,
//! `gworkspace_send_refuses_a_fresh_message_to_a_glob_sender`,
//! `gworkspace_receive_builds_a_wake_prompt_naming_the_binding`,
//! `channel_gworkspace_binding_cannot_name_an_okg_source_credential`,
//! `agent_channels_gmail_binding_claims_only_its_own_correspondent`.

// #7427: Gmail joins Slack and Telegram as a binding-addressed channel.
use super::{Capabilities, ChannelAdapter, ChannelError, InboundEvent, WakePrompt};
use crate::api::server::agent_channels::Binding;
use crate::listeners::store::StoredEvent;
use serde_json::{Value, json};

/// Gmail, over the gworkspace connector.
pub(crate) struct GworkspaceAdapter;

/// The provider id a binding names to select this adapter.
const PROVIDER: &str = "gworkspace";

/// Credential-registry keys a gworkspace binding may send as: none. See the
/// module note — the send credential is a gworkspace account token, not a
/// registry key, and the authority integration is deferred.
pub(crate) const GWORKSPACE_CREDENTIALS: &[&str] = &[];

/// What a binding's `target` addresses.
///
/// Why: Gmail has no single destination id to compare against. The two things
/// an operator can usefully bind are a correspondent and a label, and they
/// match an event by different fields, so the grammar distinguishes them once
/// and every consumer reads the parse rather than re-splitting the string.
/// Test: `gworkspace_adapter_validates_target_grammar`.
#[derive(Debug, PartialEq, Eq)]
enum Destination<'a> {
    /// `from:<address>`, matched with the listener's own sender globs.
    Sender(&'a str),
    /// `label:<label-id>`, matched against the message's Gmail label ids.
    Label(&'a str),
}

/// Parse a binding target, or `None` when it is not one this provider accepts.
///
/// What: `from:` requires an `@` after any leading `*` so a bare word cannot be
/// saved as an address pattern; `label:` requires a non-empty id. Neither may
/// contain whitespace, which is what keeps a target a single token in logs and
/// in the wake metadata.
/// Test: `gworkspace_adapter_validates_target_grammar`.
fn parse_target(target: &str) -> Option<Destination<'_>> {
    if let Some(address) = target.strip_prefix("from:") {
        let address = address.trim();
        let core = address.strip_prefix('*').unwrap_or(address);
        return (address.len() <= 320
            && core.contains('@')
            && !address.chars().any(char::is_whitespace))
        .then_some(Destination::Sender(address));
    }
    if let Some(label) = target.strip_prefix("label:") {
        let label = label.trim();
        return (!label.is_empty()
            && label.len() <= 128
            && !label.chars().any(char::is_whitespace))
        .then_some(Destination::Label(label));
    }
    None
}

/// The Gmail message id an event carries.
///
/// Why: a reply needs the message it answers, and `compose_email`'s `reply`
/// action takes exactly that — it fetches the original itself for the thread id
/// and the `In-Reply-To`/`References` headers, so nothing here has to carry
/// them. `StoredEvent::id` is `<listener>:<gmail message id>` (see
/// `listeners::poll::poll_once`), so the id is already in hand.
/// Test: `gworkspace_receive_builds_a_wake_prompt_naming_the_binding`.
fn gmail_message_id(event: &StoredEvent) -> Option<&str> {
    event
        .id
        .rsplit_once(':')
        .map(|(_, id)| id)
        .filter(|id| !id.is_empty())
}

/// The one Gmail write this channel performs.
///
/// Why: `compose_email` takes a concrete `BaseClient` and builds its URLs from
/// the `GMAIL_API_BASE` constant, so a test cannot put a mock server where
/// Google is. This trait is the thinnest seam that makes the composed request
/// observable without adding a second HTTP client to this crate.
/// Test: `gworkspace_send_threads_a_reply_and_opens_a_fresh_message`.
#[async_trait::async_trait]
pub(crate) trait GmailSend: Send + Sync {
    /// Hand `args` to Gmail's compose surface and return its response.
    async fn compose(&self, args: Value) -> Result<Value, ChannelError>;
}

/// The live seam: gworkspace's own `compose_email`, on a default-account
/// client.
struct LiveGmail;

#[async_trait::async_trait]
impl GmailSend for LiveGmail {
    async fn compose(&self, args: Value) -> Result<Value, ChannelError> {
        let client = trusty_gworkspace::api::client::BaseClient::new()
            .map_err(|_| ChannelError::Provider { provider: PROVIDER })?;
        trusty_gworkspace::api::services::gmail::messages::compose_email(&client, args)
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "gworkspace channel: Gmail compose failed");
                ChannelError::Provider { provider: PROVIDER }
            })
    }
}

/// Build the Gmail request one send becomes.
///
/// Why: the two shapes are the whole of the outbound contract, and both are
/// worth pinning without a network call. A reply carries only the message id —
/// `compose_email` defaults the recipient and subject from the original and
/// rides its thread — while a fresh message has to address the binding's own
/// correspondent.
/// What: two kinds of binding can answer an incoming message and nothing else,
/// and both are [`ChannelError::Destination`], a 400, rather than a request
/// Gmail would reject. A `label:` binding has no address at all. A `from:` GLOB
/// — `from:*@example.com`, which `parse_target` accepts and the inbound path
/// matches against many senders — names a SET of correspondents, so it has no
/// one address either; composing `to: "*@example.com"` would hand Gmail a
/// literal asterisk as a recipient (#7427 code-critic MEDIUM).
/// Test: `gworkspace_send_threads_a_reply_and_opens_a_fresh_message`,
/// `gworkspace_send_refuses_a_fresh_message_to_a_glob_sender`.
fn compose_args(
    binding: &Binding,
    text: &str,
    reply_to: Option<&str>,
) -> Result<Value, ChannelError> {
    if let Some(message_id) = reply_to {
        return Ok(json!({"action":"reply","message_id":message_id,"body":text}));
    }
    match parse_target(&binding.target) {
        Some(Destination::Sender(address)) if !address.contains('*') => {
            Ok(json!({"action":"send","to":address,"subject":binding.name,"body":text}))
        }
        _ => Err(ChannelError::Destination {
            target: binding.target.clone(),
        }),
    }
}

/// Send through `gmail`. [`ChannelAdapter::send`] is this with the live seam.
///
/// Test: `gworkspace_send_threads_a_reply_and_opens_a_fresh_message`.
async fn send_via(
    gmail: &dyn GmailSend,
    binding: &Binding,
    text: &str,
    reply_to: Option<&str>,
) -> Result<Value, ChannelError> {
    let result = gmail
        .compose(compose_args(binding, text, reply_to)?)
        .await?;
    Ok(json!({"ok":true,"message_id":result["id"]}))
}

/// The Gmail message this turn is answering, when the turn was woken by an
/// inbound message on THIS binding.
///
/// Why: the persona calls the `channel` tool from inside the wake dispatch, and
/// `LISTENER_CHAT_EVENT` is the metadata that dispatch scoped around it — so
/// the turn's own origin is readable without threading an argument through the
/// HTTP layer, the tool, and `agent_channels::send`. A turn started any other
/// way (the Channels tab, a scheduled task) reads nothing and opens a fresh
/// message instead.
/// What: the binding id in the metadata has to equal this binding's, so a turn
/// woken on one binding cannot reply into another binding's thread.
/// Test: `gworkspace_reply_context_is_confined_to_its_own_binding`.
fn inbound_reply_context(binding: &Binding) -> Option<String> {
    let metadata = crate::listeners::wake::LISTENER_CHAT_EVENT
        .try_with(Clone::clone)
        .ok()?;
    reply_context_from(&metadata, &binding.id)
}

/// The reply target inside one wake-metadata string, for `binding_id`.
///
/// Test: `gworkspace_reply_context_is_confined_to_its_own_binding`.
fn reply_context_from(metadata: &str, binding_id: &str) -> Option<String> {
    let value: Value = serde_json::from_str(metadata).ok()?;
    if value.get("binding").and_then(Value::as_str) != Some(binding_id) {
        return None;
    }
    value
        .get("gmail_message_id")
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[async_trait::async_trait]
impl ChannelAdapter for GworkspaceAdapter {
    fn provider(&self) -> &'static str {
        PROVIDER
    }

    fn display_name(&self) -> &'static str {
        "Google Workspace (Gmail)"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            can_send: true,
            // History belongs to the mailbox listener, which is the reader for
            // this provider; a second reader here would poll Gmail twice.
            can_read: false,
            can_receive: true,
            receive_reason: "Automatic updates require a Gmail listener polling the bound mailbox, and a message matching this binding's sender or label",
            read_reason: "Gmail history is read by the mailbox listener, not the channel",
        }
    }

    fn configured(&self) -> bool {
        trusty_gworkspace::api::client::BaseClient::new()
            .ok()
            .and_then(|client| client.storage().list_accounts().ok())
            .is_some_and(|accounts| !accounts.is_empty())
    }

    fn credential_providers(&self) -> &'static [&'static str] {
        GWORKSPACE_CREDENTIALS
    }

    fn credential_env_prefix(&self) -> &'static str {
        "GOOGLE_"
    }

    fn validate_target(&self, target: &str) -> bool {
        parse_target(target).is_some()
    }

    fn addresses(&self, target: &str, destination: &str, event: &StoredEvent) -> bool {
        match parse_target(target) {
            // The listener's own sender globs, so `from:*@example.com` means
            // the same thing in a binding target as in a listener filter.
            Some(Destination::Sender(pattern)) => {
                crate::listeners::wake::sender_glob_matches(destination, pattern)
            }
            Some(Destination::Label(label)) => event
                .labels
                .iter()
                .any(|value| value.eq_ignore_ascii_case(label)),
            None => false,
        }
    }

    async fn send(&self, binding: &Binding, text: &str) -> Result<Value, ChannelError> {
        send_via(
            &LiveGmail,
            binding,
            text,
            inbound_reply_context(binding).as_deref(),
        )
        .await
    }

    async fn receive(
        &self,
        binding: &Binding,
        event: InboundEvent<'_>,
    ) -> Result<Option<WakePrompt>, ChannelError> {
        let e = event.event;
        let prompt =
            crate::listeners::wake::build_wake_prompt(e, None, Some(&binding.instructions));
        let metadata = json!({
            "kind":"trusty.listener-event","version":1,
            "listener":binding.name,"binding":binding.id,
            "event_id":e.id,"event_type":e.event_type,
            "from":e.from,"subject":e.subject,
            "gmail_message_id":gmail_message_id(e),
        })
        .to_string();
        Ok(Some(WakePrompt { prompt, metadata }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn binding_with(target: &str) -> Binding {
        serde_json::from_value(json!({
            "id":"family","name":"Family mail","provider":"gworkspace","target":target,
            "enabled":true,"send_enabled":true,"receive_enabled":true
        }))
        .unwrap()
    }

    fn gmail_event(from: &str, labels: &[&str]) -> StoredEvent {
        StoredEvent {
            id: "gmail-personal:19abc".into(),
            listener_id: "gmail-personal".into(),
            provider: "gmail".into(),
            event_type: "message.received".into(),
            ts: "2026-09-11T00:00:00Z".into(),
            from: Some(from.into()),
            subject: Some("Dinner".into()),
            snippet: Some("Are we still on?".into()),
            included: true,
            labels: labels.iter().map(|l| (*l).to_string()).collect(),
        }
    }

    /// Records the args a send composed, and answers as Gmail would.
    struct RecordingGmail(Arc<Mutex<Vec<Value>>>);

    #[async_trait::async_trait]
    impl GmailSend for RecordingGmail {
        async fn compose(&self, args: Value) -> Result<Value, ChannelError> {
            self.0.lock().unwrap().push(args);
            Ok(json!({"id":"sent-1","threadId":"t-1"}))
        }
    }

    /// Pre-change this test does not compile: there was no gworkspace adapter,
    /// so no target grammar to parse.
    #[test]
    fn gworkspace_adapter_validates_target_grammar() {
        for good in [
            "from:alice@example.com",
            "from:*@example.com",
            "label:INBOX",
            "label:Label_7",
        ] {
            assert!(
                GworkspaceAdapter.validate_target(good),
                "`{good}` must be a valid gworkspace target"
            );
        }
        for bad in [
            "alice@example.com",
            "from:alice",
            "from:",
            "label:",
            "label:Two Words",
            "C123456",
            "",
        ] {
            assert!(
                !GworkspaceAdapter.validate_target(bad),
                "`{bad}` must not be a valid gworkspace target"
            );
        }
        assert_eq!(
            parse_target("from:alice@example.com"),
            Some(Destination::Sender("alice@example.com"))
        );
        assert_eq!(
            parse_target("label:INBOX"),
            Some(Destination::Label("INBOX"))
        );
    }

    /// A binding claims only the messages its target names.
    ///
    /// Pre-change this test does not compile: `ChannelAdapter` had no
    /// `addresses`, because Slack and Telegram compared their target to a
    /// destination id by equality inside the inbound path.
    #[test]
    fn gworkspace_adapter_addresses_sender_and_label_targets() {
        let alice = gmail_event("Alice <alice@example.com>", &["INBOX"]);
        let bob = gmail_event("bob@example.com", &["INBOX", "CATEGORY_PROMOTIONS"]);
        let sender = |event: &StoredEvent| event.from.clone().unwrap_or_default();

        let bound = "from:alice@example.com";
        assert!(GworkspaceAdapter.addresses(bound, &sender(&alice), &alice));
        assert!(!GworkspaceAdapter.addresses(bound, &sender(&bob), &bob));

        // A glob, spelled the way a listener filter spells one.
        assert!(GworkspaceAdapter.addresses("from:*@example.com", &sender(&bob), &bob));

        // A label target ignores the sender and reads the message's labels.
        let promo = "label:CATEGORY_PROMOTIONS";
        assert!(GworkspaceAdapter.addresses(promo, &sender(&bob), &bob));
        assert!(!GworkspaceAdapter.addresses(promo, &sender(&alice), &alice));

        // An unparseable target addresses nothing at all.
        assert!(!GworkspaceAdapter.addresses("alice@example.com", &sender(&alice), &alice));
    }

    /// A turn woken by an inbound message replies into its thread; any other
    /// turn opens a fresh message to the bound address.
    ///
    /// Pre-change this test does not compile: there was no gworkspace send.
    #[tokio::test]
    async fn gworkspace_send_threads_a_reply_and_opens_a_fresh_message() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let gmail = RecordingGmail(Arc::clone(&seen));
        let binding = binding_with("from:alice@example.com");

        let replied = send_via(&gmail, &binding, "On my way", Some("19abc"))
            .await
            .unwrap();
        assert_eq!(replied["ok"], json!(true));
        assert_eq!(replied["message_id"], json!("sent-1"));

        send_via(&gmail, &binding, "New thread", None)
            .await
            .unwrap();

        let sent = seen.lock().unwrap().clone();
        assert_eq!(sent[0]["action"], json!("reply"));
        assert_eq!(sent[0]["message_id"], json!("19abc"));
        assert_eq!(sent[0]["body"], json!("On my way"));
        // A reply names no recipient: `compose_email` takes it, the subject and
        // the thread from the message being answered.
        assert!(sent[0].get("to").is_none());

        assert_eq!(sent[1]["action"], json!("send"));
        assert_eq!(sent[1]["to"], json!("alice@example.com"));
        assert_eq!(sent[1]["subject"], json!("Family mail"));

        // A label binding can answer a message but has no address of its own.
        let label = binding_with("label:CATEGORY_PROMOTIONS");
        assert!(matches!(
            send_via(&gmail, &label, "hello", None).await,
            Err(ChannelError::Destination { .. })
        ));
        assert!(
            send_via(&gmail, &label, "hello", Some("19abc"))
                .await
                .is_ok()
        );
    }

    /// A glob sender target can answer a message but cannot open a fresh one.
    ///
    /// Why: `from:*@example.com` is a valid binding target — the inbound path
    /// matches it against every sender at that domain — but it is not an
    /// address. A non-reply send on it has no one recipient to name.
    ///
    /// Pre-change the first assertion fails: `compose_args` matched any
    /// `Destination::Sender` and composed `to: "*@example.com"`, handing Gmail a
    /// literal asterisk as a recipient.
    #[tokio::test]
    async fn gworkspace_send_refuses_a_fresh_message_to_a_glob_sender() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let gmail = RecordingGmail(Arc::clone(&seen));

        for glob in ["from:*@example.com", "from:alice@*"] {
            let binding = binding_with(glob);
            assert!(
                matches!(
                    send_via(&gmail, &binding, "hello", None).await,
                    Err(ChannelError::Destination { .. })
                ),
                "`{glob}` must not compose a fresh message"
            );
            // A reply rides the thread of the message it answers, which is one
            // real correspondent however many the binding matches.
            assert!(
                send_via(&gmail, &binding, "hello", Some("19abc"))
                    .await
                    .is_ok()
            );
        }

        // An exact address is unaffected.
        send_via(&gmail, &binding_with("from:alice@example.com"), "hi", None)
            .await
            .unwrap();

        let sent = seen.lock().unwrap().clone();
        assert_eq!(sent.len(), 3, "two replies and one fresh message");
        assert!(
            sent[..2]
                .iter()
                .all(|args| args["action"] == json!("reply"))
        );
        assert_eq!(sent[2]["action"], json!("send"));
        assert_eq!(sent[2]["to"], json!("alice@example.com"));
    }

    /// The wake prompt names the binding and carries the message a reply needs.
    ///
    /// Pre-change this test does not compile: gworkspace had no adapter, so
    /// `ChannelAdapter::receive`'s default refused every Gmail event.
    #[tokio::test]
    async fn gworkspace_receive_builds_a_wake_prompt_naming_the_binding() {
        let binding = binding_with("from:alice@example.com");
        let event = gmail_event("Alice <alice@example.com>", &["INBOX"]);
        let wake = GworkspaceAdapter
            .receive(
                &binding,
                InboundEvent {
                    agent: "izzie",
                    event: &event,
                },
            )
            .await
            .unwrap()
            .expect("an addressed Gmail message earns a wake");

        assert!(wake.prompt.contains("Dinner"));
        let metadata: Value = serde_json::from_str(&wake.metadata).unwrap();
        assert_eq!(metadata["listener"], json!("Family mail"));
        assert_eq!(metadata["binding"], json!("family"));
        assert_eq!(metadata["gmail_message_id"], json!("19abc"));
        assert_eq!(metadata["from"], json!("Alice <alice@example.com>"));
    }

    /// A turn woken on one binding cannot reply into another binding's thread.
    #[test]
    fn gworkspace_reply_context_is_confined_to_its_own_binding() {
        let metadata =
            json!({"kind":"trusty.listener-event","binding":"family","gmail_message_id":"19abc"})
                .to_string();
        assert_eq!(
            reply_context_from(&metadata, "family"),
            Some("19abc".to_string())
        );
        assert_eq!(reply_context_from(&metadata, "work"), None);
        // A turn with no listener metadata at all, and one whose event is not
        // a Gmail message, both open a fresh message.
        assert_eq!(reply_context_from("not json", "family"), None);
        assert_eq!(
            reply_context_from(&json!({"binding":"family"}).to_string(), "family"),
            None
        );
    }

    #[test]
    fn gworkspace_adapter_reports_send_and_receive_without_history() {
        let caps = GworkspaceAdapter.capabilities();
        assert!(caps.can_send && caps.can_receive);
        assert!(!caps.can_read);
        assert_eq!(GworkspaceAdapter.provider(), "gworkspace");
    }
}
