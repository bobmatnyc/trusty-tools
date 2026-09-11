//! Slack adapter — the reference implementation of [`ChannelAdapter`].
//!
//! Why: Slack is the only provider that both sends and receives today, so it is
//! the one adapter that exercises every method on the trait. The code here is
//! the code that used to sit inline in `agent_channels::send`'s `"slack"` arm
//! and in `agent_channels::receive_slack`; behaviour is unchanged, and the
//! existing `agent_channels` tests still pin it.
//! What: `chat.postMessage` for outbound. For inbound, the knowledge-intake
//! pass and the wake-prompt construction, returned as a [`WakePrompt`] the
//! caller dispatches — the adapter never spawns.
//! Test: `agent_channels_receive_never_falls_back_for_disabled_bound_destination`,
//! `agent_channels_validates_provider_destination_and_permissions`,
//! `slack_adapter_reports_send_and_receive`.

// #7427: provider knowledge moved out of the HTTP layer into one adapter.
use super::{Capabilities, ChannelAdapter, ChannelError, InboundEvent, WakePrompt};
use crate::api::server::agent_channels::Binding;
use serde_json::{Value, json};
use trusty_channels::slack::api::client::BaseClient;
use trusty_channels::slack::api::constants::SLACK_API_BASE;

/// Slack, over the Web API.
pub(crate) struct SlackAdapter;

/// Build the client this binding's credential selects.
///
/// Why: a binding with no `credential_ref` keeps the pre-#7427 behaviour and
/// uses the process-global bot token; a binding that names one sends as that
/// credential and fails loudly when it will not resolve, rather than falling
/// back to an identity the binding did not name.
fn client(binding: &Binding) -> Result<BaseClient, ChannelError> {
    let failed = || ChannelError::Provider { provider: "slack" };
    match binding.credential_ref.as_deref() {
        None => BaseClient::new().map_err(|_| failed()),
        Some(reference) => {
            let token = super::resolve_credential(reference)?;
            BaseClient::with_endpoint(SLACK_API_BASE, Some(token.into_inner()))
                .map_err(|_| failed())
        }
    }
}

#[async_trait::async_trait]
impl ChannelAdapter for SlackAdapter {
    fn provider(&self) -> &'static str {
        "slack"
    }

    fn display_name(&self) -> &'static str {
        "Slack"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            can_send: true,
            can_read: true,
            can_receive: true,
            receive_reason: "Automatic updates require the Slack bot listener to be running, the channel paired, and sender authorized",
            read_reason: "",
        }
    }

    fn configured(&self) -> bool {
        BaseClient::new().is_ok_and(|c| c.has_token())
    }

    /// A Slack conversation ID: `C`/`G`/`D` followed by uppercase alphanumerics.
    fn validate_target(&self, target: &str) -> bool {
        target.len() >= 2
            && target.len() <= 32
            && matches!(target.chars().next(), Some('C' | 'G' | 'D'))
            && target
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    }

    async fn send(&self, binding: &Binding, text: &str) -> Result<Value, ChannelError> {
        let client = client(binding)?;
        let result = client
            .call_method(
                "chat.postMessage",
                &json!({"channel":binding.target,"text":text,"unfurl_links":false,"unfurl_media":false}),
            )
            .await
            .map_err(|_| ChannelError::Provider { provider: "slack" })?;
        Ok(json!({"ok":true,"message_id":result["ts"]}))
    }

    async fn receive(
        &self,
        binding: &Binding,
        event: InboundEvent<'_>,
    ) -> Result<Option<WakePrompt>, ChannelError> {
        crate::api::server::knowledge_pipeline::intake::slack(event.agent, binding, event.event)
            .await;
        let prompt = crate::listeners::wake::build_wake_prompt(
            event.event,
            None,
            Some(&binding.instructions),
        );
        let e = event.event;
        let metadata = json!({"kind":"trusty.listener-event","version":1,"listener":binding.name,"event_id":e.id,"event_type":e.event_type,"from":e.from,"subject":e.subject}).to_string();
        Ok(Some(WakePrompt { prompt, metadata }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_adapter_reports_send_and_receive() {
        let caps = SlackAdapter.capabilities();
        assert!(caps.can_send && caps.can_read && caps.can_receive);
        assert!(SlackAdapter.validate_target("C123456"));
        assert!(SlackAdapter.validate_target("D0ABCDEF"));
        assert!(!SlackAdapter.validate_target("https://host"));
        assert!(!SlackAdapter.validate_target("c123456"));
        assert!(!SlackAdapter.validate_target("C"));
    }
}
