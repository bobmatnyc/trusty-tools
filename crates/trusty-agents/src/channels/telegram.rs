//! Telegram adapter — send only, and the trait now says so.
//!
//! Why: "Telegram is send-only" was a sentence in `tools/channel.rs`'s prompt
//! text and a hard-coded rejection in `Binding::validate`. Stating it as
//! `can_receive: false` puts it where the validator, the providers listing, and
//! the inbound path all read the same answer. Inbound Telegram is PR 2 of
//! #7427; when it lands it is a `receive` impl here and a flag flip, not a new
//! rejection to delete from three places.
//! What: `sendMessage` for outbound. [`ChannelAdapter::receive`] is left at the
//! trait default, which refuses with
//! [`ChannelError::ReceiveUnsupported`](super::ChannelError::ReceiveUnsupported).
//! Test: `telegram_adapter_is_send_only`,
//! `agent_channels_validates_provider_destination_and_permissions`.

// #7427: Telegram's send arm, and its send-only capability, in one place.
use super::{Capabilities, ChannelAdapter, ChannelError};
use crate::api::server::agent_channels::Binding;
use serde_json::{Value, json};
use trusty_channels::telegram::api::client::BaseClient;
use trusty_channels::telegram::api::constants::TELEGRAM_API_BASE;

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
            can_receive: false,
            receive_reason: "Telegram incoming updates are not supported by this integration",
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

    #[test]
    fn telegram_adapter_is_send_only() {
        let caps = TelegramAdapter.capabilities();
        assert!(caps.can_send);
        assert!(!caps.can_read);
        assert!(!caps.can_receive);
        assert!(TelegramAdapter.validate_target("123"));
        assert!(TelegramAdapter.validate_target("-1001234567890"));
        assert!(TelegramAdapter.validate_target("@trusty_bot"));
        assert!(!TelegramAdapter.validate_target("C123456"));
        assert!(!TelegramAdapter.validate_target("@"));
    }
}
