//! One adapter per channel provider, behind a single [`ChannelAdapter`] trait.
//!
//! Why: `api::server::agent_channels` used to carry a `match binding.provider`
//! arm per provider in `send`, a second provider test in `messages`, a third in
//! `Binding::validate`, and a hand-written `providers()` JSON literal. Adding a
//! provider meant editing four places that could disagree, and the file was
//! approaching the 500-SLOC cap. Provider knowledge now lives in one adapter
//! per provider and `agent_channels` dispatches through the registry.
//! What: [`ChannelAdapter`] states what a provider can do ([`Capabilities`]),
//! what destination text it accepts, how it sends, and how it turns an inbound
//! event into a [`WakePrompt`]. [`registry::adapter`] resolves a provider id;
//! an id with no adapter is unsupported rather than silently inert.
//! [`credentials`] resolves a binding's `credential_ref` to a
//! [`trusty_common::credentials::Secret`]; [`status`] holds the per-binding
//! dispatch-failure counter the assistant's channel view reads.
//! Test: `channel_registry_resolves_known_providers_and_rejects_notion`,
//! `channel_adapter_receive_defaults_to_unsupported`,
//! `channel_credential_ref_resolves_env_scheme_and_rejects_others`,
//! `channel_dispatch_failure_is_counted_per_binding`.

// #7427: adapter model for two-way channel connectors (epic #7425 item b).
pub(crate) mod credentials;
mod registry;
mod slack;
pub(crate) mod status;
mod telegram;

pub(crate) use credentials::{resolve_credential, validate_credential_ref};
pub(crate) use registry::{adapter, providers_json, require_adapter};

use crate::api::server::agent_channels::Binding;
use crate::listeners::store::StoredEvent;
use serde_json::Value;

/// What a provider can do, and the user-facing reason when it cannot.
///
/// Why: `providers()` publishes these to the UI, `Binding::validate` refuses a
/// `receive_enabled` binding on a send-only provider, and `messages()` refuses
/// history on a provider that has none. One struct keeps those three answers
/// from drifting apart.
/// Test: `channel_registry_resolves_known_providers_and_rejects_notion`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Capabilities {
    /// The provider accepts outbound messages.
    pub can_send: bool,
    /// The provider can be asked for prior messages on the bound destination.
    pub can_read: bool,
    /// The provider delivers inbound events to the assistant.
    pub can_receive: bool,
    /// Shown beside `can_receive` in the channel view.
    pub receive_reason: &'static str,
    /// Shown when a read is refused because [`Capabilities::can_read`] is false.
    pub read_reason: &'static str,
}

/// An inbound provider event, addressed to one assistant.
///
/// Why: [`ChannelAdapter::receive`] needs both the event and the assistant it
/// was matched to — the Slack intake pipeline is keyed by assistant name.
pub(crate) struct InboundEvent<'a> {
    /// Assistant the bound destination belongs to.
    pub agent: &'a str,
    /// The stored inbound event.
    pub event: &'a StoredEvent,
}

/// A wake dispatch an inbound event earned: the prompt, and its event metadata.
///
/// Why: the adapter owns how a provider's event becomes prompt text; the caller
/// owns spawning the dispatch and recording its outcome.
pub(crate) struct WakePrompt {
    /// Prompt text handed to the assistant.
    pub prompt: String,
    /// Serialized `trusty.listener-event` metadata scoped around the dispatch.
    pub metadata: String,
}

/// Why a channel operation did not happen.
///
/// Why: `agent_channels` maps these onto HTTP statuses, so the reasons have to
/// be distinguishable — an unsupported provider is the caller's mistake (400),
/// a credential that will not resolve and a provider that rejected the request
/// are both upstream (502) but need different operator guidance.
/// What: no variant carries a credential value. [`ChannelError::Credential`]
/// names the scheme and the failure, never the resolved secret.
/// Test: `channel_credential_ref_resolves_env_scheme_and_rejects_others`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ChannelError {
    /// No adapter is registered for this provider id.
    #[error("unsupported channel provider `{0}`")]
    UnsupportedProvider(String),

    /// The provider has no inbound path in this build.
    #[error("provider `{0}` does not deliver inbound events")]
    ReceiveUnsupported(&'static str),

    /// The binding's `credential_ref` could not be resolved. Never the value.
    #[error("credential reference could not be resolved: {0}")]
    Credential(String),

    /// The provider client could not be built or the request failed.
    #[error("channel provider `{provider}` request failed")]
    Provider {
        /// Provider id whose request failed.
        provider: &'static str,
    },
}

/// One provider's half of the channel model: capabilities, destinations, send,
/// receive.
///
/// Why: a binding names a provider; everything that differs between providers
/// has to resolve from that one string. Without this trait each difference is a
/// `match` arm somewhere in the HTTP layer, and the arms drift — the send path
/// knew about Telegram, the receive path did not, and only a comment recorded
/// that Telegram is send-only.
/// What: [`ChannelAdapter::capabilities`] and
/// [`ChannelAdapter::validate_target`] answer without any network call, so
/// validation and the providers listing are cheap. [`ChannelAdapter::send`] and
/// [`ChannelAdapter::receive`] do the provider work.
/// [`ChannelAdapter::receive`] defaults to
/// [`ChannelError::ReceiveUnsupported`], so a send-only provider implements
/// nothing and still refuses inbound correctly.
/// Test: `channel_registry_resolves_known_providers_and_rejects_notion`,
/// `channel_adapter_receive_defaults_to_unsupported`.
#[async_trait::async_trait]
pub(crate) trait ChannelAdapter: Send + Sync {
    /// The provider id a `Binding.provider` must equal to select this adapter.
    fn provider(&self) -> &'static str;

    /// Display name for the channel view.
    fn display_name(&self) -> &'static str;

    /// What this provider can do.
    fn capabilities(&self) -> Capabilities;

    /// Whether a credential for this provider is present in the environment.
    fn configured(&self) -> bool;

    /// Whether `target` is a well-formed destination for this provider.
    fn validate_target(&self, target: &str) -> bool;

    /// Send `text` to the binding's destination, returning the provider's
    /// acknowledgement.
    async fn send(&self, binding: &Binding, text: &str) -> Result<Value, ChannelError>;

    /// Turn an inbound event into a wake dispatch, or `Ok(None)` when the event
    /// earns none.
    ///
    /// Why: the default refuses, so inbound support is opt-in per provider.
    async fn receive(
        &self,
        binding: &Binding,
        event: InboundEvent<'_>,
    ) -> Result<Option<WakePrompt>, ChannelError> {
        let _ = (binding, event);
        Err(ChannelError::ReceiveUnsupported(self.provider()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SendOnly;

    #[async_trait::async_trait]
    impl ChannelAdapter for SendOnly {
        fn provider(&self) -> &'static str {
            "send-only"
        }
        fn display_name(&self) -> &'static str {
            "Send Only"
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                can_send: true,
                can_read: false,
                can_receive: false,
                receive_reason: "no inbound",
                read_reason: "no history",
            }
        }
        fn configured(&self) -> bool {
            false
        }
        fn validate_target(&self, target: &str) -> bool {
            !target.is_empty()
        }
        async fn send(&self, _binding: &Binding, _text: &str) -> Result<Value, ChannelError> {
            Ok(serde_json::json!({"ok":true}))
        }
    }

    #[tokio::test]
    async fn channel_adapter_receive_defaults_to_unsupported() {
        let binding: Binding = serde_json::from_value(serde_json::json!({
            "id":"team","name":"Team","provider":"send-only","target":"X","enabled":true
        }))
        .unwrap();
        let event = crate::listeners::store::StoredEvent {
            id: "x".into(),
            listener_id: "send-only".into(),
            provider: "send-only".into(),
            event_type: "message".into(),
            ts: "now".into(),
            from: None,
            subject: None,
            snippet: None,
            included: true,
            labels: vec![],
        };
        let outcome = SendOnly
            .receive(
                &binding,
                InboundEvent {
                    agent: "fixture",
                    event: &event,
                },
            )
            .await;
        assert!(matches!(
            outcome,
            Err(ChannelError::ReceiveUnsupported("send-only"))
        ));
    }
}
