//! Assistant-owned attachment history and provider message assembly (#7370).
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use trusty_common::chat_attachments::{Attachment, ImageContent, ImageRef, InputAttachment};
use trusty_common::memory_core::store::chat_sessions::ChatMessage;
use trusty_common::memory_rpc::call_memory_tool_at_with_timeout;

pub(crate) const HISTORY_UNAVAILABLE_NOTICE: &str = "Saved image history and chat persistence are unavailable for this turn. The response may lack earlier image context.";

/// Why: a provider prompt cannot guarantee that the API discloses missing image context.
/// What: mark otherwise successful responses partial with a fixed host-generated notice.
/// Test: `history_outage_notice_preserves_success_and_failure_contracts`.
pub(crate) fn apply_history_notice(
    response: &mut crate::api::types::PmResponse,
    unavailable: &AtomicBool,
) {
    if unavailable.load(Ordering::Relaxed)
        && response.status == crate::api::types::PmStatus::Success
    {
        response.status = crate::api::types::PmStatus::Partial;
        response.errors.push(HISTORY_UNAVAILABLE_NOTICE.into());
    }
}

pub(crate) async fn rpc(socket: &Path, method: &str, args: Value) -> Result<Value> {
    let value = call_memory_tool_at_with_timeout(
        socket,
        "tools/call",
        json!({"name":method,"arguments":args}),
        std::time::Duration::from_secs(30),
    )
    .await?;
    anyhow::ensure!(
        value["isError"] != true,
        "Memory operation failed: {}",
        value
    );
    let text = value
        .pointer("/content/0/text")
        .and_then(Value::as_str)
        .context("Missing memory result")?;
    Ok(serde_json::from_str(text)?)
}
pub(crate) struct AttachmentTurn {
    socket: PathBuf,
    namespace: String,
    session: String,
    pub messages: Vec<trusty_common::inference::ChatMessage>,
}
impl AttachmentTurn {
    pub fn initial_messages(
        &self,
        system: String,
    ) -> Result<Vec<async_openai::types::ChatCompletionRequestMessage>> {
        let mut messages = vec![trusty_common::inference::ChatMessage::system(system)];
        messages.extend(self.messages.clone());
        messages
            .into_iter()
            .map(|m| serde_json::from_value(serde_json::to_value(m)?).map_err(anyhow::Error::from))
            .collect()
    }
    pub fn validate_provider(
        &self,
        config: &crate::agents::AgentConfig,
        credential: &str,
        cli: bool,
    ) -> Result<()> {
        anyhow::ensure!(
            !cli,
            "Attachments are unavailable with the Claude CLI provider; select an API provider"
        );
        if self.has_images() {
            let provider = config.agent.provider_id.as_deref().unwrap_or(credential);
            let provider = match provider {
                "anthropic-direct" => "anthropic",
                "ollama" => "local",
                other => other,
            };
            anyhow::ensure!(
                trusty_common::inference::registry::capabilities_for(provider)
                    .is_some_and(|p| p.vision),
                "Provider {provider} does not declare image support; select a vision-capable provider"
            );
        }
        Ok(())
    }
    pub async fn finish(&self, text: &str) -> Result<()> {
        let result = rpc(&self.socket,"chat_session_add_turn",json!({"palace":self.namespace,"session_id":self.session,"role":"assistant","content":text})).await?;
        anyhow::ensure!(
            result["attachments_version"] == 1,
            "Memory daemon did not acknowledge typed history persistence"
        );
        Ok(())
    }
    pub fn has_images(&self) -> bool {
        self.messages.iter().any(|m| !m.images.is_empty())
    }
}

/// Why: follow-up turns must retain images after reload without local asset stores.
/// What: hydrate and validate without writes, then serialize and deduplicate accepted user turns.
/// Test: `attachment_history_hydration_enforces_namespace_and_budget`; durable storage is covered by trusty-common asset_ownership_and_restart_are_enforced.
pub(crate) async fn prepare(
    name: &str,
    input: &str,
    attachments: &[InputAttachment],
    history_unavailable: &AtomicBool,
    validate: impl FnOnce(&AttachmentTurn) -> Result<()>,
) -> Result<Option<AttachmentTurn>> {
    trusty_common::chat_attachments::validate_inputs(attachments)?;
    let owner = name.to_owned();
    let policy =
        tokio::task::spawn_blocking(move || crate::assistants::memory_policy::resolve(&owner))
            .await?;
    let policy = match policy {
        Ok(p) => p,
        Err(_) if attachments.is_empty() => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let socket = crate::memory::trusty_client::default_trusty_socket();
    let session = crate::ctrl::pm_task::session_id_for(name);
    let args = json!({"palace":policy.namespace,"session_id":session});
    let previous = rpc(&socket, "chat_session_get", args.clone()).await;
    let history: Vec<ChatMessage> = match previous {
        Ok(value) => serde_json::from_value(
            value
                .get("history")
                .cloned()
                .context("Invalid chat history")?,
        )?,
        Err(_) if attachments.is_empty() => {
            history_unavailable.store(true, Ordering::Relaxed);
            return Ok(None);
        }
        Err(_) => vec![],
    };
    if attachments.is_empty() && !history.iter().any(|m| !m.attachments.is_empty()) {
        return Ok(None);
    }
    let capabilities = rpc(&socket, "chat_asset_capabilities", json!({}))
        .await
        .context("Upgrade trusty-memory: durable chat attachments are unavailable")?;
    anyhow::ensure!(
        capabilities["version"] == 1 && capabilities["typed_history_attachments"] == true,
        "Memory daemon does not support durable chat attachments"
    );
    let mut messages = Vec::new();
    let mut decoded_total = 0usize;
    for message in history.iter().rev().take(20).rev() {
        messages.push(
            hydrate(
                &socket,
                &policy.namespace,
                &session,
                message,
                &mut decoded_total,
            )
            .await?,
        );
    }
    let mut current_total = 0;
    let current_message = input_message(input, attachments, &mut current_total)?;
    let retry = messages.last() == Some(&current_message);
    if !retry {
        anyhow::ensure!(
            decoded_total.saturating_add(current_total)
                <= trusty_common::chat_attachments::MAX_TOTAL_BYTES,
            "Recent image history exceeds 10 MiB context limit"
        );
        messages.push(current_message);
    }
    let turn = AttachmentTurn {
        socket: socket.clone(),
        namespace: policy.namespace.clone(),
        session: session.clone(),
        messages,
    };
    // #7370: unsupported providers must never create assets or poison durable history.
    validate(&turn)?;
    let dirs = crate::agents::agents_dir_candidates();
    let (manifest, _) = crate::api::server::agent_patch::resolve_agent_paths(&dirs, name)
        .context("Assistant manifest unavailable")?;
    let _guard = crate::knowledge::execution::mutation_guard(&manifest).await?;
    crate::tools::assistant_memory::ensure_palace(&socket, &policy).await?;
    rpc(
        &socket,
        "chat_session_create",
        json!({"palace":policy.namespace,"session_id":session,"title":format!("Chat with {name}")}),
    )
    .await?;
    let refreshed = rpc(&socket, "chat_session_get", args).await?;
    let latest: Vec<ChatMessage> = serde_json::from_value(
        refreshed
            .get("history")
            .cloned()
            .context("Invalid chat history")?,
    )?;
    anyhow::ensure!(
        serde_json::to_value(&latest)? == serde_json::to_value(&history)?,
        "Conversation changed during attachment validation; retry the send"
    );
    if retry {
        return Ok(Some(turn));
    }
    let mut stored = Vec::new();
    for attachment in attachments {
        stored.push(match attachment {
            Attachment::Image { name,mime_type,image } => {
                let response = rpc(&socket,"chat_asset_put",json!({"palace":policy.namespace,"session_id":session,"name":name,"mime_type":mime_type,"data_base64":image.data_base64})).await?;
                let asset_id = response["asset_id"].as_str().context("Memory returned no asset ID")?.to_owned();
                Attachment::Image { name:name.clone(),mime_type:mime_type.clone(),image:ImageRef{asset_id} }
            }
            Attachment::Table { name,source_format,sheets } => Attachment::Table { name:name.clone(),source_format:source_format.clone(),sheets:sheets.clone() },
        });
    }
    let current = ChatMessage {
        role: "user".into(),
        content: input.into(),
        attachments: stored,
    };
    let result = rpc(&socket,"chat_session_add_turn",json!({"palace":policy.namespace,"session_id":session,"role":"user","content":input,"attachments":current.attachments})).await?;
    anyhow::ensure!(
        result["attachments_version"] == 1,
        "Memory daemon did not acknowledge typed history persistence"
    );
    Ok(Some(turn))
}
/// Assemble the current input without touching memory assets.
fn input_message(
    input: &str,
    attachments: &[InputAttachment],
    total: &mut usize,
) -> Result<trusty_common::inference::ChatMessage> {
    let mut message = trusty_common::inference::ChatMessage::user(input);
    for attachment in attachments {
        match attachment {
            Attachment::Image {
                mime_type, image, ..
            } => {
                *total = total.saturating_add(
                    trusty_common::chat_attachments::decode_image(mime_type, &image.data_base64)?
                        .len(),
                );
                anyhow::ensure!(
                    *total <= trusty_common::chat_attachments::MAX_TOTAL_BYTES,
                    "Recent image history exceeds 10 MiB context limit"
                );
                message.images.push(ImageContent {
                    mime_type: mime_type.clone(),
                    data_base64: image.data_base64.clone(),
                });
            }
            Attachment::Table { .. } => message
                .content
                .get_or_insert_default()
                .push_str(&attachment.table_text().unwrap_or_default()),
        }
    }
    Ok(message)
}

async fn hydrate(
    socket: &Path,
    palace: &str,
    session: &str,
    message: &ChatMessage,
    total: &mut usize,
) -> Result<trusty_common::inference::ChatMessage> {
    let mut result = trusty_common::inference::ChatMessage::user(&message.content);
    result.role = message.role.clone();
    for attachment in &message.attachments {
        match attachment {
            Attachment::Image {
                mime_type, image, ..
            } => {
                let value = rpc(
                    socket,
                    "chat_asset_get",
                    json!({"palace":palace,"session_id":session,"asset_id":image.asset_id}),
                )
                .await?;
                anyhow::ensure!(
                    value["mime_type"].as_str() == Some(mime_type),
                    "Stored image MIME mismatch"
                );
                let data = value["data_base64"]
                    .as_str()
                    .context("Stored image has no data")?;
                *total = total.saturating_add(
                    trusty_common::chat_attachments::decode_image(mime_type, data)?.len(),
                );
                anyhow::ensure!(
                    *total <= trusty_common::chat_attachments::MAX_TOTAL_BYTES,
                    "Recent image history exceeds 10 MiB context limit"
                );
                result.images.push(ImageContent {
                    mime_type: mime_type.clone(),
                    data_base64: data.into(),
                });
            }
            Attachment::Table { .. } => result
                .content
                .get_or_insert_default()
                .push_str(&attachment.table_text().unwrap_or_default()),
        }
    }
    Ok(result)
}

pub(crate) async fn get_asset(name: &str, id: &str) -> Result<(String, Vec<u8>)> {
    anyhow::ensure!(uuid::Uuid::parse_str(id).is_ok(), "Invalid asset ID");
    let owner = name.to_owned();
    let policy =
        tokio::task::spawn_blocking(move || crate::assistants::memory_policy::resolve(&owner))
            .await??;
    let socket = crate::memory::trusty_client::default_trusty_socket();
    let session = crate::ctrl::pm_task::session_id_for(name);
    let asset = rpc(
        &socket,
        "chat_asset_get",
        json!({"palace":policy.namespace,"session_id":session,"asset_id":id}),
    )
    .await?;
    let mime = asset["mime_type"]
        .as_str()
        .context("Invalid stored MIME")?
        .to_owned();
    let bytes = trusty_common::chat_attachments::decode_image(
        &mime,
        asset["data_base64"]
            .as_str()
            .context("Missing asset bytes")?,
    )?;
    Ok((mime, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_outage_notice_preserves_success_and_failure_contracts() {
        use crate::api::types::{PmResponse, PmStatus};
        let mut response = PmResponse::running("fixture");
        response.status = PmStatus::Success;
        apply_history_notice(&mut response, &AtomicBool::new(false));
        assert_eq!(response.status, PmStatus::Success);
        assert!(response.errors.is_empty());
        apply_history_notice(&mut response, &AtomicBool::new(true));
        assert_eq!(response.status, PmStatus::Partial);
        assert_eq!(response.errors, [HISTORY_UNAVAILABLE_NOTICE]);
        let mut failed = PmResponse::error("fixture", "Explicit image failure");
        apply_history_notice(&mut failed, &AtomicBool::new(true));
        assert_eq!(failed.status, PmStatus::Failed);
        assert_eq!(failed.errors, ["Explicit image failure"]);
    }
    #[tokio::test]
    async fn attachment_rejection_is_read_only_and_pending_retries_are_idempotent() {
        if std::env::var_os("TRUSTY_ATTACHMENT_ACCEPT_CHILD").is_none() {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().canonicalize().unwrap();
            let result = tokio::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "chat_attachments::tests::attachment_rejection_is_read_only_and_pending_retries_are_idempotent"])
                .env("TRUSTY_ATTACHMENT_ACCEPT_CHILD", "1")
                .env("HOME", root.as_path()).env("TAGENT_PROJECT_DIR", root.as_path())
                .env("TAGENT_CONFIG_DIR", root.as_path().join("agents"))
                .env("TAGENT_ASSISTANTS_DIR", root.as_path().join("homes"))
                .env("TRUSTY_DATA_DIR_OVERRIDE", root.as_path().join("data"))
                .env("TRUSTY_MEMORY_SOCKET", root.as_path().join("memory.sock"))
                .env_remove("OPEN_MPM_CONFIG_DIR").env_remove("OPEN_MPM_PROJECT_DIR")
                .status().await.unwrap();
            assert!(result.success());
            return;
        }
        use std::sync::{Arc, Mutex};
        let dirs = std::path::PathBuf::from(std::env::var_os("TAGENT_CONFIG_DIR").unwrap());
        std::fs::create_dir_all(&dirs).unwrap();
        let raw = "[agent]\nname='fixture'\nrole='assistant'\nmodel='fixture'\ndescription='Synthetic'\n[llm]\nmax_tokens=128\ntemperature=0.0\n[system_prompt]\ncontent='Synthetic'\n";
        std::fs::write(
            dirs.join("assistant.toml"),
            raw.replace("name='fixture'", "name='assistant'"),
        )
        .unwrap();
        std::fs::write(
            dirs.join("fixture.toml"),
            raw.replace("name='fixture'", "name='fixture'\nextends='assistant'"),
        )
        .unwrap();
        let policy = crate::assistants::memory_policy::resolve("fixture").unwrap();
        let namespace = policy.namespace.clone();
        let history = Arc::new(Mutex::new(Vec::<Value>::new()));
        let observed = history.clone();
        let writes = Arc::new(Mutex::new(Vec::<String>::new()));
        let observed_writes = writes.clone();
        let fail_probe = Arc::new(AtomicBool::new(false));
        let probe_control = fail_probe.clone();
        const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAEElEQVR4nGP4z8AARAwQCgAf7gP9i18U1AAAAABJRU5ErkJggg==";
        let asset_id = uuid::Uuid::new_v4().to_string();
        let daemon = crate::uds_mock::spawn(move |method, params| {
            let fail_probe = fail_probe.clone();
            let history = history.clone(); let writes = writes.clone();
            let namespace = namespace.clone(); let asset_id = asset_id.clone();
            let wrapped = method == "tools/call";
            let method = if wrapped { params["name"].as_str().unwrap().to_owned() } else { method.to_owned() };
            let args = if wrapped { params["arguments"].clone() } else { params };
            Box::pin(async move {
                if method == "chat_session_get" && fail_probe.load(Ordering::Relaxed) {
                    return Err(trusty_common::uds::server::RpcError::internal("synthetic history probe unavailable"));
                }
                let value = match method.as_str() {
                    "palace_list" => json!({"palaces":[namespace]}),
                    "chat_session_get" => json!({"history":history.lock().unwrap().clone()}),
                    "chat_asset_capabilities" => json!({"version":1,"typed_history_attachments":true}),
                    "chat_session_create" => json!({}),
                    "chat_asset_get" => json!({"mime_type":"image/png","data_base64":PNG}),
                    "chat_asset_put" => { writes.lock().unwrap().push(method); json!({"asset_id":asset_id}) },
                    "chat_session_add_turn" => { writes.lock().unwrap().push(method); history.lock().unwrap().push(json!({"role":args["role"],"content":args["content"],"attachments":args["attachments"]})); json!({"attachments_version":1}) },
                    _ => panic!("Unexpected memory call: {method}")
                };
                Ok(if wrapped { json!({"content":[{"type":"text","text":value.to_string()}]}) } else { value })
            })
        }).await;
        // The child owns this environment and no other test runs in this process.
        unsafe {
            std::env::set_var("TRUSTY_MEMORY_SOCKET", daemon.socket());
        }
        let attachment: InputAttachment = serde_json::from_value(
            json!({"kind":"image","name":"fixture.png","mime_type":"image/png","data_base64":PNG}),
        )
        .unwrap();
        let config = crate::agents::AgentConfig::by_name("fixture").unwrap();
        let unavailable = AtomicBool::new(false);
        let rejected = prepare(
            "fixture",
            "Describe",
            std::slice::from_ref(&attachment),
            &unavailable,
            |turn| turn.validate_provider(&config, "local", false),
        )
        .await;
        assert!(
            rejected
                .err()
                .unwrap()
                .to_string()
                .contains("does not declare image support")
        );
        assert!(observed.lock().unwrap().is_empty());
        assert!(observed_writes.lock().unwrap().is_empty());
        assert!(
            prepare(
                "fixture",
                "Plain text still works",
                &[],
                &unavailable,
                |_| Ok(())
            )
            .await
            .unwrap()
            .is_none()
        );
        prepare(
            "fixture",
            "Describe",
            std::slice::from_ref(&attachment),
            &unavailable,
            |_| Ok(()),
        )
        .await
        .unwrap()
        .unwrap();
        prepare(
            "fixture",
            "Describe",
            std::slice::from_ref(&attachment),
            &unavailable,
            |_| Ok(()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(observed.lock().unwrap().len(), 1);
        assert_eq!(
            *observed_writes.lock().unwrap(),
            ["chat_asset_put", "chat_session_add_turn"]
        );
        assert!(!unavailable.load(Ordering::Relaxed));
        probe_control.store(true, Ordering::Relaxed);
        assert!(
            prepare("fixture", "Plain follow-up", &[], &unavailable, |_| Ok(()))
                .await
                .unwrap()
                .is_none()
        );
        assert!(unavailable.load(Ordering::Relaxed));
        assert_eq!(observed_writes.lock().unwrap().len(), 2);
        assert!(
            prepare(
                "fixture",
                "Explicit image",
                &[attachment],
                &AtomicBool::new(false),
                |_| Ok(())
            )
            .await
            .is_err()
        );
    }
    #[tokio::test]
    async fn attachment_history_hydration_enforces_namespace_and_budget() {
        const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAEElEQVR4nGP4z8AARAwQCgAf7gP9i18U1AAAAABJRU5ErkJggg==";
        let daemon = crate::uds_mock::spawn(|method, params| {
            assert_eq!(method,"tools/call");
            assert_eq!(params["name"],"chat_asset_get");
            assert_eq!(params["arguments"]["palace"],"own");
            assert_eq!(params["arguments"]["session_id"],"persona-own");
            Box::pin(async move { Ok(json!({"content":[{"type":"text","text":json!({"mime_type":"image/png","data_base64":PNG}).to_string()}]})) })
        }).await;
        let message = ChatMessage {
            role: "user".into(),
            content: "Describe again".into(),
            attachments: vec![Attachment::Image {
                name: "fixture.png".into(),
                mime_type: "image/png".into(),
                image: ImageRef {
                    asset_id: uuid::Uuid::new_v4().to_string(),
                },
            }],
        };
        let mut total = 0;
        let hydrated = hydrate(daemon.socket(), "own", "persona-own", &message, &mut total)
            .await
            .unwrap();
        assert_eq!(hydrated.images[0].data_base64, PNG);
        assert_eq!(hydrated.content.as_deref(), Some("Describe again"));
        assert!(total > 0);
        let mut exhausted = trusty_common::chat_attachments::MAX_TOTAL_BYTES - 1;
        assert!(
            hydrate(
                daemon.socket(),
                "own",
                "persona-own",
                &message,
                &mut exhausted
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("10 MiB")
        );
    }
}
