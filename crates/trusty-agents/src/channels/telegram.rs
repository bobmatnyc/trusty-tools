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

/// Build the client this binding's credential selects. See `slack::client`.
fn client(binding: &Binding) -> Result<BaseClient, ChannelError> {
    let failed = || ChannelError::Provider {
        provider: "telegram",
    };
    match binding.credential_ref.as_deref() {
        None => BaseClient::new().map_err(|_| failed()),
        Some(reference) => {
            let token = super::resolve_credential(reference)?;
            BaseClient::with_endpoint(TELEGRAM_API_BASE, Some(token.into_inner()))
                .map_err(|_| failed())
        }
    }
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
        BaseClient::new().is_ok_and(|c| c.has_token())
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
        let client = client(binding)?;
        let result = client
            .call_method(
                "sendMessage",
                &json!({"chat_id":binding.target,"text":text,"link_preview_options":{"is_disabled":true}}),
            )
            .await
            .map_err(|_| ChannelError::Provider { provider: "telegram" })?;
        Ok(json!({"ok":true,"message_id":result["result"]["message_id"]}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
