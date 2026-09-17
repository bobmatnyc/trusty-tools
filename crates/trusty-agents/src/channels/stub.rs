//! The test-only stub provider: send and read a channel with no credential.
//!
//! Why (#8037): every registered adapter authenticates against a live service,
//! so `POST /api/agents/{name}/channels/{id}/send` answered 502 "Channel
//! credential could not be resolved" and `GET .../messages` answered 502
//! "Channel history could not be read" on any host without a real Slack,
//! Telegram or Gmail secret. End-to-end verification of the channel surface was
//! therefore impossible without production credentials. This adapter closes
//! that by keeping the traffic in process: a send appends to an in-memory
//! outbox, a read hands that outbox back, and an inbound event builds the same
//! [`WakePrompt`] envelope every other provider builds.
//!
//! What makes it unreachable in production: [`enabled`] reads
//! [`ENABLE_ENV`] on every lookup, and [`super::registry::adapter`] consults
//! the stub table ONLY when it answers true. A daemon started without that
//! variable resolves `stub` to no adapter at all, so `Binding::validate`,
//! `global_channels::validate` and the inbound dispatcher each refuse a stub
//! channel exactly as they refuse `notion` — the record cannot be saved, and a
//! hand-written one wakes nobody. The providers listing omits it too, so the
//! Channels UI never offers it.
//! Test: `a_default_environment_has_no_stub_provider`,
//! `the_stub_outbox_round_trips_a_send_into_a_read`,
//! `stub_receive_builds_the_shared_wake_envelope`.

// #8037: a provider that needs no credential, so send/read/inbound can be
// exercised end to end.
use super::{Capabilities, ChannelAdapter, ChannelError, InboundEvent, WakePrompt};
use crate::api::server::agent_channels::Binding;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};

/// The environment variable that admits the stub adapter.
///
/// Why: a variable rather than a config key, because a config key would have to
/// be readable — and therefore writable — through the very channel routes this
/// provider exists to test. An environment variable is set by whoever starts
/// the process, which is the operator, and it cannot be turned on by an API
/// caller.
pub(crate) const ENABLE_ENV: &str = "TAGENT_STUB_CHANNEL";

/// Whether this process admits the stub provider.
///
/// What: true when [`ENABLE_ENV`] is set to anything other than the empty
/// string, `0`, `false` or `off`. Read per call rather than cached, so a test
/// can turn it on and off around one case; the read is reached only after the
/// real adapter table has already missed.
/// Test: `a_default_environment_has_no_stub_provider`.
pub(crate) fn enabled() -> bool {
    match std::env::var(ENABLE_ENV) {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "off"
        ),
        Err(_) => false,
    }
}

/// Everything sent through the stub, newest last, keyed by destination.
///
/// Why: the read half has to answer with what the send half accepted, or the
/// round trip proves nothing. Keyed by `target` rather than by binding id so
/// two assistants bound to one destination see one conversation, which is what
/// the real providers do.
static OUTBOX: LazyLock<Mutex<HashMap<String, Vec<Value>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Drop every recorded message. Test scaffolding; the outbox is process-global.
#[cfg(test)]
pub(crate) fn clear_outbox() {
    OUTBOX
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
}

/// The in-process provider a stub channel talks to.
pub(crate) struct StubAdapter;

#[async_trait::async_trait]
impl ChannelAdapter for StubAdapter {
    fn provider(&self) -> &'static str {
        "stub"
    }

    fn display_name(&self) -> &'static str {
        "Stub (test only)"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            can_send: true,
            can_read: true,
            can_receive: true,
            receive_reason: "Test-only provider: inbound events arrive through POST /api/channels/{id}/inbound",
            read_reason: "",
        }
    }

    /// Configured exactly when admitted: there is no credential to resolve.
    fn configured(&self) -> bool {
        enabled()
    }

    /// No credential family at all, so a `credential_ref` on a stub channel is
    /// grammar-checked and nothing more — see `global_channels::validate`.
    fn credential_providers(&self) -> &'static [&'static str] {
        &[]
    }

    fn credential_env_prefix(&self) -> &'static str {
        "TAGENT_STUB_"
    }

    /// Any short, printable destination. The stub addresses by equality like
    /// Slack and Telegram, so the shape only has to be stable and loggable.
    fn validate_target(&self, target: &str) -> bool {
        !target.is_empty() && target.chars().count() <= 128 && !target.chars().any(char::is_control)
    }

    /// Record `text` against the binding's destination.
    ///
    /// Test: `the_stub_outbox_round_trips_a_send_into_a_read`.
    async fn send(&self, binding: &Binding, text: &str) -> Result<Value, ChannelError> {
        let mut outbox = OUTBOX.lock().unwrap_or_else(PoisonError::into_inner);
        let messages = outbox.entry(binding.target.clone()).or_default();
        let id = format!("stub-{}-{}", binding.target, messages.len());
        messages.push(json!({
            "id": id,
            "text": text,
            "from": binding.name,
            "timestamp": messages.len(),
        }));
        Ok(json!({"ok": true, "message_id": id}))
    }

    /// Everything [`StubAdapter::send`] recorded for this destination.
    ///
    /// Test: `the_stub_outbox_round_trips_a_send_into_a_read`.
    async fn read(&self, binding: &Binding) -> Result<Vec<Value>, ChannelError> {
        Ok(OUTBOX
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&binding.target)
            .cloned()
            .unwrap_or_default())
    }

    /// One injected event becomes the same wake envelope Slack and Telegram
    /// produce (DOC-60 §8).
    ///
    /// Why: an injected event is only worth testing if it travels the path a
    /// real one travels, so the prompt is built by the same
    /// `build_wake_prompt` and the metadata carries the same
    /// `trusty.listener-event` shape. No knowledge intake: the intake catalogue
    /// has no stub source kind, and adding one would put test traffic in the
    /// assistant's knowledge store.
    /// Test: `stub_receive_builds_the_shared_wake_envelope`.
    async fn receive(
        &self,
        binding: &Binding,
        event: InboundEvent<'_>,
    ) -> Result<Option<WakePrompt>, ChannelError> {
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

    fn binding(target: &str) -> Binding {
        serde_json::from_value(json!({
            "id":"stub-desk","name":"Stub Desk","provider":"stub","target":target,
            "enabled":true,"send_enabled":true,"receive_enabled":true
        }))
        .expect("fixture binding")
    }

    /// The default environment admits no stub provider anywhere.
    ///
    /// Why (#8037): "not reachable in production config" is the whole safety
    /// property. The registry lookup is what every validator and the inbound
    /// dispatcher consult, so a miss there is a stub channel that cannot be
    /// saved and cannot wake anybody.
    #[test]
    #[serial_test::serial(channel_credentials)]
    fn a_default_environment_has_no_stub_provider() {
        let _off = EnvVarGuard::set(ENABLE_ENV, "");
        assert!(!enabled(), "an empty value is off");
        assert!(
            crate::channels::adapter("stub").is_none(),
            "the registry resolves nothing for `stub` by default"
        );
        assert!(
            !crate::channels::providers_json()
                .as_array()
                .is_some_and(|list| list.iter().any(|p| p["id"] == json!("stub"))),
            "the providers listing never offers it"
        );
        assert!(
            binding("desk").validate_in(&[]).is_err(),
            "a stub binding cannot be saved"
        );

        // And the opposite arm, so the test cannot pass by the lookup being
        // broken outright.
        let _on = EnvVarGuard::set(ENABLE_ENV, "1");
        assert!(enabled());
        assert_eq!(
            crate::channels::adapter("stub").map(|a| a.provider()),
            Some("stub")
        );
        assert!(binding("desk").validate_in(&[]).is_ok());

        // `0`, `false` and `off` are refusals, not values.
        for value in ["0", "false", "OFF"] {
            let _guard = EnvVarGuard::set(ENABLE_ENV, value);
            assert!(!enabled(), "`{value}` does not admit the stub");
        }
    }

    /// A send is readable back through the same binding.
    #[tokio::test]
    #[serial_test::serial(channel_stub_outbox)]
    async fn the_stub_outbox_round_trips_a_send_into_a_read() {
        clear_outbox();
        let desk = binding("desk");
        let other = binding("other-desk");
        assert!(StubAdapter.read(&desk).await.expect("read").is_empty());

        let ack = StubAdapter.send(&desk, "first").await.expect("send");
        assert_eq!(ack["ok"], json!(true));
        StubAdapter.send(&desk, "second").await.expect("send");
        StubAdapter.send(&other, "elsewhere").await.expect("send");

        let messages = StubAdapter.read(&desk).await.expect("read");
        assert_eq!(
            messages
                .iter()
                .map(|m| m["text"].clone())
                .collect::<Vec<_>>(),
            vec![json!("first"), json!("second")],
            "one destination reads back only its own traffic"
        );
        assert_eq!(StubAdapter.read(&other).await.expect("read").len(), 1);
        clear_outbox();
    }

    /// An inbound stub event carries the shared envelope and names the binding.
    #[tokio::test]
    async fn stub_receive_builds_the_shared_wake_envelope() {
        let event = crate::listeners::store::StoredEvent {
            id: "evt-1".into(),
            listener_id: "stub-desk".into(),
            provider: "stub".into(),
            event_type: "message".into(),
            ts: "now".into(),
            from: Some("Owner".into()),
            subject: Some("Ping".into()),
            snippet: Some("are you there".into()),
            included: true,
            labels: vec![],
        };
        let binding = binding("desk");
        let wake = StubAdapter
            .receive(
                &binding,
                InboundEvent {
                    agent: "fixture",
                    event: &event,
                },
            )
            .await
            .expect("receive")
            .expect("a stub event earns a wake");
        assert!(wake.prompt.contains("are you there"), "{}", wake.prompt);
        let metadata: Value = serde_json::from_str(&wake.metadata).expect("metadata is JSON");
        assert_eq!(metadata["kind"], json!("trusty.listener-event"));
        assert_eq!(metadata["listener"], json!("Stub Desk"));
        assert_eq!(metadata["event_id"], json!("evt-1"));
    }
}
