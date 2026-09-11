//! Telegram adapter — sends and, since #7427 PR 2, receives.
//!
//! Why: "Telegram is send-only" used to be a sentence in `tools/channel.rs`'s
//! prompt text and a hard-coded rejection in `Binding::validate`. Stating it as
//! a [`Capabilities`] flag put it where the validator, the providers listing,
//! and the inbound path all read the same answer — so turning inbound on is a
//! flag flip and a [`ChannelAdapter::receive`] impl here, with no rejection to
//! delete from three other places.
//! What: `sendMessage` for outbound. For inbound, the wake-prompt construction,
//! returned as a [`WakePrompt`] the caller dispatches — the adapter never
//! spawns. [`poll_token`] resolves the credential the long-poll loop in
//! `crate::telegram` authenticates with, through the same reference the binding
//! names.
//! Test: `telegram_adapter_reports_send_and_receive`,
//! `telegram_adapter_receive_names_the_binding_in_its_metadata`,
//! `telegram_poll_token_refuses_a_credential_outside_the_family`,
//! `agent_channels_validates_provider_destination_and_permissions`.

// #7427: Telegram's send arm, its capabilities, and its inbound half, in one
// place.
use super::{Capabilities, ChannelAdapter, ChannelError, InboundEvent, WakePrompt};
use crate::api::server::agent_channels::Binding;
use serde_json::{Value, json};
use trusty_channels::telegram::api::client::BaseClient;
use trusty_channels::telegram::api::constants::TELEGRAM_API_BASE;
use trusty_common::credentials::Secret;

/// Telegram, over the Bot API.
pub(crate) struct TelegramAdapter;

/// Credential-registry keys a Telegram binding may send as. Telegram has one
/// secret, so the family has one member.
pub(crate) const TELEGRAM_CREDENTIALS: &[&str] = &["telegram"];

/// Build the client this binding's credential selects. See `slack::client_at`.
fn client_at(binding: &Binding, base_url: &str) -> Result<BaseClient, ChannelError> {
    let reference = binding
        .credential_ref
        .as_deref()
        .unwrap_or(TELEGRAM_CREDENTIALS[0]);
    let token = super::resolve_credential(reference, TELEGRAM_CREDENTIALS, "TELEGRAM_")?;
    BaseClient::with_endpoint(base_url, Some(token.into_inner())).map_err(|_| {
        ChannelError::Provider {
            provider: "telegram",
        }
    })
}

/// Resolve the credential the Telegram long-poll loop authenticates with.
///
/// Why (#7427): `run_telegram_bot` read `TELEGRAM_BOT_TOKEN` with
/// `std::env::var`, which is a second credential-resolution path beside the
/// authority the bindings use — the exact defect this repo's common-entry-point
/// rule names. The loop now resolves the same reference a binding names, so the
/// identity that polls and the identity that replies are one credential.
/// What: `reference` is the binding's `credential_ref`; `None` means the
/// adapter's default key, which maps to `TELEGRAM_BOT_TOKEN` — so a host that
/// configured nothing new keeps the token it had. Confinement to Telegram's own
/// provider family is [`super::resolve_credential`]'s, unchanged.
/// Test: `telegram_poll_token_refuses_a_credential_outside_the_family`.
pub(crate) fn poll_token(reference: Option<&str>) -> Result<Secret<String>, ChannelError> {
    super::resolve_credential(
        reference.unwrap_or(TELEGRAM_CREDENTIALS[0]),
        TELEGRAM_CREDENTIALS,
        "TELEGRAM_",
    )
}

/// `sendMessage` against `base_url`. [`ChannelAdapter::send`] is this with the
/// real API root.
///
/// Test: `telegram_adapter_send_uses_the_credential_the_binding_names`.
async fn send_message(
    binding: &Binding,
    text: &str,
    base_url: &str,
) -> Result<Value, ChannelError> {
    let client = client_at(binding, base_url)?;
    let result = client
        .call_method(
            "sendMessage",
            &json!({"chat_id":binding.target,"text":text,"link_preview_options":{"is_disabled":true}}),
        )
        .await
        .map_err(|_| ChannelError::Provider {
            provider: "telegram",
        })?;
    Ok(json!({"ok":true,"message_id":result["result"]["message_id"]}))
}

#[async_trait::async_trait]
impl ChannelAdapter for TelegramAdapter {
    fn provider(&self) -> &'static str {
        "telegram"
    }

    fn display_name(&self) -> &'static str {
        "Telegram"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            can_send: true,
            can_read: false,
            can_receive: true,
            receive_reason: "Automatic updates require the Telegram long-poll gateway to be running and the chat ID to match this binding's destination",
            read_reason: "Telegram bots cannot read prior chat history",
        }
    }

    fn configured(&self) -> bool {
        super::resolve_credential(TELEGRAM_CREDENTIALS[0], TELEGRAM_CREDENTIALS, "TELEGRAM_")
            .is_ok()
    }

    fn credential_providers(&self) -> &'static [&'static str] {
        TELEGRAM_CREDENTIALS
    }

    fn credential_env_prefix(&self) -> &'static str {
        "TELEGRAM_"
    }

    /// A numeric chat ID, or an `@username`.
    fn validate_target(&self, target: &str) -> bool {
        target.parse::<i64>().is_ok()
            || (target.starts_with('@')
                && target.len() > 1
                && target.len() <= 64
                && target[1..]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_'))
    }

    async fn send(&self, binding: &Binding, text: &str) -> Result<Value, ChannelError> {
        send_message(binding, text, TELEGRAM_API_BASE).await
    }

    /// One inbound Telegram message becomes the same wake envelope Slack
    /// produces (DOC-60 §8).
    ///
    /// Why: the assistant must not be able to tell which channel woke it apart
    /// from the event metadata. Building the prompt here, with
    /// `build_wake_prompt`, is what keeps the two providers on one envelope
    /// rather than two prompt formats that drift.
    /// What: the ask-first preamble, the binding's own instructions, and the
    /// untrusted event fields, plus the `trusty.listener-event` metadata naming
    /// the binding. No knowledge intake: the intake catalogue has one channel
    /// source kind, `SourceKind::Slack`, and adding a Telegram one is a
    /// `knowledge_pipeline` change outside this crate slice's scope.
    /// Test: `telegram_adapter_receive_names_the_binding_in_its_metadata`.
    async fn receive(
        &self,
        binding: &Binding,
        event: InboundEvent<'_>,
    ) -> Result<Option<WakePrompt>, ChannelError> {
        // #7427: Telegram inbound, through the same envelope as Slack.
        let e = event.event;
        let prompt =
            crate::listeners::wake::build_wake_prompt(e, None, Some(&binding.instructions));
        let metadata = json!({"kind":"trusty.listener-event","version":1,"listener":binding.name,"event_id":e.id,"event_type":e.event_type,"from":e.from,"subject":e.subject}).to_string();
        Ok(Some(WakePrompt { prompt, metadata }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::credentials::test_env::EnvVarGuard;
    use std::sync::{Arc, Mutex};

    fn binding_with(credential_ref: Option<&str>) -> Binding {
        let mut value = json!({
            "id":"team","name":"Team","provider":"telegram","target":"123456",
            "enabled":true,"send_enabled":true
        });
        if let Some(reference) = credential_ref {
            value["credential_ref"] = json!(reference);
        }
        serde_json::from_value(value).unwrap()
    }

    /// Stand-in Telegram that records the token from the `/bot<token>/` path.
    async fn mock_telegram(seen: Arc<Mutex<Option<String>>>) -> String {
        let app =
            axum::Router::new().route(
                "/{token}/sendMessage",
                axum::routing::post(
                    move |axum::extract::Path(token): axum::extract::Path<String>,
                          _body: String| async move {
                        *seen.lock().unwrap() = Some(token);
                        axum::Json(json!({"ok":true,"result":{"message_id":7427}}))
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    /// The token on the wire is the one `credential_ref` resolves, and the
    /// default resolves the same registry key when the binding names none.
    #[tokio::test]
    #[serial_test::serial(channel_credentials)]
    async fn telegram_adapter_send_uses_the_credential_the_binding_names() {
        let _bot = EnvVarGuard::set("TELEGRAM_BOT_TOKEN", "7427:not-a-real-token");

        let seen = Arc::new(Mutex::new(None));
        let base = mock_telegram(Arc::clone(&seen)).await;

        send_message(&binding_with(None), "hello", &base)
            .await
            .unwrap();
        assert_eq!(
            seen.lock().unwrap().clone(),
            Some("bot7427:not-a-real-token".to_string())
        );

        *seen.lock().unwrap() = None;
        send_message(&binding_with(Some("telegram")), "hello", &base)
            .await
            .unwrap();
        assert_eq!(
            seen.lock().unwrap().clone(),
            Some("bot7427:not-a-real-token".to_string())
        );

        // A credential outside Telegram's family never reaches the wire.
        *seen.lock().unwrap() = None;
        assert!(
            send_message(&binding_with(Some("slack")), "hello", &base)
                .await
                .is_err()
        );
        assert!(seen.lock().unwrap().is_none());
    }

    /// Pre-#7427 PR 2 this asserted `!caps.can_receive`; the flip is the whole
    /// behaviour change, so the assertion moves with it.
    #[test]
    fn telegram_adapter_reports_send_and_receive() {
        let caps = TelegramAdapter.capabilities();
        assert!(caps.can_send);
        assert!(!caps.can_read);
        assert!(caps.can_receive);
        assert!(TelegramAdapter.validate_target("123"));
        assert!(TelegramAdapter.validate_target("-1001234567890"));
        assert!(TelegramAdapter.validate_target("@trusty_bot"));
        assert!(!TelegramAdapter.validate_target("C123456"));
        assert!(!TelegramAdapter.validate_target("@"));
    }

    /// A Telegram update on a bound chat produces the Slack-shaped wake
    /// envelope, and the metadata names the binding that matched.
    ///
    /// Pre-change this test fails at the first assertion: `receive` was the
    /// trait default, which answers `Err(ReceiveUnsupported("telegram"))`.
    #[tokio::test]
    async fn telegram_adapter_receive_names_the_binding_in_its_metadata() {
        let mut binding = binding_with(None);
        binding.name = "Owner DM".into();
        binding.instructions = "Reply only about scheduling.".into();
        let event = crate::listeners::store::StoredEvent {
            id: "telegram:123456:42".into(),
            listener_id: "telegram".into(),
            provider: "telegram".into(),
            event_type: "message.private".into(),
            ts: "2026-09-11T00:00:00Z".into(),
            from: Some("Masa".into()),
            subject: None,
            snippet: Some("Move the 3pm".into()),
            included: true,
            labels: vec![],
        };
        let wake = TelegramAdapter
            .receive(
                &binding,
                InboundEvent {
                    agent: "fixture",
                    event: &event,
                },
            )
            .await
            .expect("telegram receive is supported")
            .expect("a bound inbound message earns a wake");

        let metadata: Value = serde_json::from_str(&wake.metadata).unwrap();
        assert_eq!(metadata["kind"], json!("trusty.listener-event"));
        assert_eq!(metadata["listener"], json!("Owner DM"));
        assert_eq!(metadata["event_id"], json!("telegram:123456:42"));
        assert_eq!(metadata["event_type"], json!("message.private"));
        // Same envelope Slack builds: the binding's instructions and the
        // untrusted-data framing both reach the prompt.
        assert!(wake.prompt.contains("Reply only about scheduling."));
        assert!(wake.prompt.contains("Untrusted event data"));
        assert!(wake.prompt.contains("Move the 3pm"));
    }

    /// The poll loop's credential is the binding's, confined to Telegram's own
    /// family — and an unresolvable one is an error the caller must act on, not
    /// an empty token it can poll with.
    #[test]
    #[serial_test::serial(channel_credentials)]
    fn telegram_poll_token_refuses_a_credential_outside_the_family() {
        let _bot = EnvVarGuard::set("TELEGRAM_BOT_TOKEN", "7427:not-a-real-token");
        assert_eq!(poll_token(None).unwrap().expose(), "7427:not-a-real-token");
        assert_eq!(
            poll_token(Some("telegram")).unwrap().expose(),
            "7427:not-a-real-token"
        );
        for foreign in ["slack", "github", "env:TELEGRAM_BOT_TOKEN", ""] {
            assert!(
                poll_token(Some(foreign)).is_err(),
                "`{foreign}` must not authenticate the Telegram poll loop"
            );
        }
    }
}
