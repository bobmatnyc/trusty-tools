//! The adapter registry: provider id → [`ChannelAdapter`].
//!
//! Why: a binding's `provider` is a free string from a config file. One lookup
//! table decides which strings are real, so "unsupported provider" is one
//! answer rather than a `_ => false` arm repeated per call site. Notion is
//! absent from this table on purpose — `trusty-channels` has no Notion
//! connector, and a binding naming it must be refused rather than saved and
//! silently inert until one appears.
//! What: a static slice of the adapters this build carries.
//! [`adapter`] resolves one; [`providers_json`] renders the whole table for the
//! channel view, which is why that listing can no longer disagree with what the
//! adapters actually do.
//! Test: `channel_registry_resolves_known_providers_and_rejects_notion`,
//! `channel_providers_json_reports_registry_capabilities`.

// #7427: one table, four former `match binding.provider` sites.
use super::ChannelAdapter;
use super::ChannelError;
use super::slack::SlackAdapter;
use super::telegram::TelegramAdapter;
use serde_json::{Value, json};

/// Every channel provider this build supports, in display order.
static ADAPTERS: &[&dyn ChannelAdapter] = &[&SlackAdapter, &TelegramAdapter];

/// The adapter for `provider`, or `None` when the id is unsupported.
///
/// Test: `channel_registry_resolves_known_providers_and_rejects_notion`.
pub(crate) fn adapter(provider: &str) -> Option<&'static dyn ChannelAdapter> {
    ADAPTERS.iter().copied().find(|a| a.provider() == provider)
}

/// The adapter for `provider`, or [`ChannelError::UnsupportedProvider`].
///
/// Why: the operating paths (`send`, `messages`) want the same failure type as
/// everything else they can hit, so one mapping turns an adapter failure into
/// an HTTP answer.
/// Test: `channel_registry_resolves_known_providers_and_rejects_notion`.
pub(crate) fn require_adapter(provider: &str) -> Result<&'static dyn ChannelAdapter, ChannelError> {
    adapter(provider).ok_or_else(|| ChannelError::UnsupportedProvider(provider.to_string()))
}

/// The providers listing the channel view renders.
///
/// Why: previously a hand-written JSON literal in `agent_channels`, which meant
/// the UI's idea of a provider's capabilities and the code's could drift.
/// Test: `channel_providers_json_reports_registry_capabilities`.
pub(crate) fn providers_json() -> Value {
    Value::Array(
        ADAPTERS
            .iter()
            .map(|a| {
                let caps = a.capabilities();
                json!({
                    "id": a.provider(),
                    "name": a.display_name(),
                    "configured": a.configured(),
                    "can_send": caps.can_send,
                    "can_read": caps.can_read,
                    "can_receive": caps.can_receive,
                    "receive_reason": caps.receive_reason,
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_registry_resolves_known_providers_and_rejects_notion() {
        assert_eq!(adapter("slack").map(|a| a.provider()), Some("slack"));
        assert_eq!(adapter("telegram").map(|a| a.provider()), Some("telegram"));
        // Pins the current state: there is no `trusty-channels` Notion
        // connector, so a Notion binding is unsupported (#7427 PR 3).
        assert!(adapter("notion").is_none());
        assert!(adapter("gworkspace").is_none());
        assert!(adapter("").is_none());
    }

    #[test]
    fn channel_providers_json_reports_registry_capabilities() {
        let providers = providers_json();
        let list = providers.as_array().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["id"], json!("slack"));
        assert_eq!(list[0]["can_send"], json!(true));
        assert_eq!(list[0]["can_receive"], json!(true));
        assert_eq!(list[1]["id"], json!("telegram"));
        assert_eq!(list[1]["can_send"], json!(true));
        assert_eq!(list[1]["can_receive"], json!(false));
        assert!(!list.iter().any(|p| p["id"] == json!("notion")));
    }
}
