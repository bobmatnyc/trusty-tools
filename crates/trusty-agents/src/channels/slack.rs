//! Slack adapter — the reference implementation of [`ChannelAdapter`].
//!
//! Why: Slack was the first provider to both send and receive, and it is still
//! the one adapter that exercises every method on the trait — Telegram gained
//! inbound in #7427 PR 2 but has no readable history. The code here is
//! the code that used to sit inline in `agent_channels::send`'s `"slack"` arm
//! and in `agent_channels`'s inbound path; behaviour is unchanged, and the
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

/// Credential-registry keys a Slack binding may send as. `slack` is the
/// default — the same key `BaseClient::new()` resolves — so a binding that
/// names nothing keeps the pre-#7427 token.
pub(crate) const SLACK_CREDENTIALS: &[&str] = &["slack", "slack-user", "slack-app"];

/// Build the client this binding's credential selects, against `base_url`.
///
/// Why: one resolution path for both cases. A binding that names no credential
/// resolves the default registry key, which is the key `BaseClient::new()`
/// resolves through the same authority — so naming one changes which credential
/// is used and nothing else. `base_url` exists so a test can put a mock server
/// where Slack is and observe the token that actually reaches the wire.
fn client_at(binding: &Binding, base_url: &str) -> Result<BaseClient, ChannelError> {
    let reference = binding
        .credential_ref
        .as_deref()
        .unwrap_or(SLACK_CREDENTIALS[0]);
    let token = super::resolve_credential(reference, SLACK_CREDENTIALS, "SLACK_")?;
    BaseClient::with_endpoint(base_url, Some(token.into_inner()))
        .map_err(|_| ChannelError::Provider { provider: "slack" })
}

/// `chat.postMessage` against `base_url`. [`ChannelAdapter::send`] is this with
/// the real API root.
///
/// Test: `slack_adapter_send_uses_the_credential_the_binding_names`.
async fn post_message(
    binding: &Binding,
    text: &str,
    base_url: &str,
) -> Result<Value, ChannelError> {
    let client = client_at(binding, base_url)?;
    let result = client
        .call_method(
            "chat.postMessage",
            &json!({"channel":binding.target,"text":text,"unfurl_links":false,"unfurl_media":false}),
        )
        .await
        .map_err(|_| ChannelError::Provider { provider: "slack" })?;
    Ok(json!({"ok":true,"message_id":result["ts"]}))
}

/// `conversations.history` against `base_url`. [`ChannelAdapter::read`] is this
/// with the real API root.
///
/// What: the 30 most recent messages, projected onto the shape the channel view
/// renders. Message text is truncated at 8,000 characters, exactly as the
/// pre-#8037 inline body truncated it.
/// Test: `slack_adapter_read_uses_the_credential_the_binding_names`.
async fn conversation_history(
    binding: &Binding,
    base_url: &str,
) -> Result<Vec<Value>, ChannelError> {
    let client = client_at(binding, base_url)?;
    let result = client
        .call_method(
            "conversations.history",
            &json!({"channel":binding.target,"limit":30}),
        )
        .await
        .map_err(|_| ChannelError::Provider { provider: "slack" })?;
    Ok(result["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| {
            json!({
                "id": v["ts"],
                "text": v["text"].as_str().unwrap_or("").chars().take(8000).collect::<String>(),
                "from": v["user"],
                "timestamp": v["ts"],
            })
        })
        .collect())
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
        super::resolve_credential(SLACK_CREDENTIALS[0], SLACK_CREDENTIALS, "SLACK_").is_ok()
    }

    fn credential_providers(&self) -> &'static [&'static str] {
        SLACK_CREDENTIALS
    }

    fn credential_env_prefix(&self) -> &'static str {
        "SLACK_"
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
        post_message(binding, text, SLACK_API_BASE).await
    }

    /// `conversations.history` for the bound channel, newest last.
    ///
    /// Why (#8037): this body was `agent_channels::messages`'s inline Slack
    /// client — the last provider arm left in the HTTP layer. It also built its
    /// client with `BaseClient::new()`, so a read authenticated as the PROCESS
    /// default while a send on the same binding authenticated as
    /// `credential_ref`; going through [`client_at`] makes one binding one
    /// identity.
    /// Test: `slack_adapter_read_uses_the_credential_the_binding_names`.
    async fn read(&self, binding: &Binding) -> Result<Vec<Value>, ChannelError> {
        conversation_history(binding, SLACK_API_BASE).await
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
    use crate::channels::credentials::test_env::EnvVarGuard;
    use std::sync::{Arc, Mutex};

    fn binding_with(credential_ref: Option<&str>) -> Binding {
        let mut value = json!({
            "id":"team","name":"Team","provider":"slack","target":"C123456",
            "enabled":true,"send_enabled":true
        });
        if let Some(reference) = credential_ref {
            value["credential_ref"] = json!(reference);
        }
        serde_json::from_value(value).unwrap()
    }

    /// Stand-in Slack that records the bearer token it was called with.
    ///
    /// Why: `BaseClient` exposes only `has_token()`, so the only way to prove
    /// which credential a send actually used is to read it off the wire.
    async fn mock_slack(seen: Arc<Mutex<Option<String>>>) -> String {
        let history_seen = Arc::clone(&seen);
        let app = axum::Router::new()
            .route(
                "/chat.postMessage",
                axum::routing::post(
                    move |headers: axum::http::HeaderMap, _body: String| async move {
                        *seen.lock().unwrap() = headers
                            .get(axum::http::header::AUTHORIZATION)
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string);
                        axum::Json(json!({"ok":true,"ts":"1700000000.000100"}))
                    },
                ),
            )
            // #8037: the history read moved out of `agent_channels::messages`
            // into `conversation_history`, so the mock has to serve it too.
            .route(
                "/conversations.history",
                axum::routing::post(
                    move |headers: axum::http::HeaderMap, _body: String| async move {
                        *history_seen.lock().unwrap() = headers
                            .get(axum::http::header::AUTHORIZATION)
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string);
                        axum::Json(json!({"ok":true,"messages":[
                            {"ts":"1700000000.000100","text":"hello there","user":"U1"}
                        ]}))
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

    /// The token on the wire follows `credential_ref`, not the process default.
    #[tokio::test]
    #[serial_test::serial(channel_credentials)]
    async fn slack_adapter_send_uses_the_credential_the_binding_names() {
        let _bot = EnvVarGuard::set("SLACK_BOT_TOKEN", "xoxb-7427-default");
        let _app = EnvVarGuard::set("SLACK_APP_TOKEN", "xapp-7427-named");

        let seen = Arc::new(Mutex::new(None));
        let base = mock_slack(Arc::clone(&seen)).await;

        post_message(&binding_with(None), "hello", &base)
            .await
            .unwrap();
        assert_eq!(
            seen.lock().unwrap().clone(),
            Some("Bearer xoxb-7427-default".to_string())
        );

        post_message(&binding_with(Some("slack-app")), "hello", &base)
            .await
            .unwrap();
        assert_eq!(
            seen.lock().unwrap().clone(),
            Some("Bearer xapp-7427-named".to_string())
        );
    }

    /// The history read authenticates as the binding, not the process default.
    ///
    /// Why (#8037): the pre-change body in `agent_channels::messages` built its
    /// client with `BaseClient::new()`, so one binding read as one identity and
    /// sent as another. Pre-change this test cannot even be written — there was
    /// no `read` on the adapter to call.
    #[tokio::test]
    #[serial_test::serial(channel_credentials)]
    async fn slack_adapter_read_uses_the_credential_the_binding_names() {
        let _bot = EnvVarGuard::set("SLACK_BOT_TOKEN", "xoxb-8037-default");
        let _app = EnvVarGuard::set("SLACK_APP_TOKEN", "xapp-8037-named");
        let seen = Arc::new(Mutex::new(None));
        let base = mock_slack(Arc::clone(&seen)).await;

        let messages = conversation_history(&binding_with(Some("slack-app")), &base)
            .await
            .expect("history");
        assert_eq!(
            seen.lock().unwrap().clone(),
            Some("Bearer xapp-8037-named".to_string())
        );
        assert_eq!(
            messages
                .iter()
                .map(|m| m["text"].clone())
                .collect::<Vec<_>>(),
            vec![json!("hello there")]
        );
        assert_eq!(messages[0]["from"], json!("U1"));
    }

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
