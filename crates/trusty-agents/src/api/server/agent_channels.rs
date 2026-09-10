//! Assistant-bound channel destinations over trusty-channels provider clients.
use super::{agent_listeners, agent_patch::resolve_agent_paths, agent_stores::is_valid_agent_name};
use crate::listeners::config::{AgentBindingFilter, AgentListenerBinding};
use axum::{Json, extract::Path as AxumPath, http::StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
type Error = (StatusCode, Json<Value>);
fn err(code: StatusCode, text: &str) -> Error {
    (code, Json(json!({"error":text})))
}
fn bad(text: &str) -> Error {
    err(StatusCode::BAD_REQUEST, text)
}
fn internal(_: impl std::fmt::Display) -> Error {
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Channel configuration could not be read or saved",
    )
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Binding {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub target: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub send_enabled: bool,
    #[serde(default)]
    pub receive_enabled: bool,
    #[serde(default)]
    pub filter: AgentBindingFilter,
    #[serde(default)]
    pub instructions: String,
}
impl Binding {
    fn validate(&self) -> Result<(), Error> {
        if !is_valid_agent_name(&self.id)
            || self.name.trim().is_empty()
            || self.name.chars().count() > 128
        {
            return Err(bad(
                "Channel ID and name are required (maximum 128 characters)",
            ));
        }
        let target_valid = match self.provider.as_str() {
            "slack" => {
                self.target.len() >= 2
                    && self.target.len() <= 32
                    && matches!(self.target.chars().next(), Some('C' | 'G' | 'D'))
                    && self
                        .target
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            }
            "telegram" => {
                self.target.parse::<i64>().is_ok()
                    || (self.target.starts_with('@')
                        && self.target.len() > 1
                        && self.target.len() <= 64
                        && self.target[1..]
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_'))
            }
            _ => false,
        };
        if !target_valid {
            return Err(bad(
                "Choose Slack channel ID or Telegram chat ID for the selected provider",
            ));
        }
        if self.provider == "telegram" && self.receive_enabled {
            return Err(bad(
                "Telegram incoming updates are not available through this channel integration",
            ));
        }
        AgentListenerBinding {
            name: self.id.clone(),
            enabled: self.enabled,
            event_types: vec![],
            filter: self.filter.clone(),
            instructions: self.instructions.clone(),
        }
        .validate()
        .map_err(|e| bad(&e))
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Update {
    revision: String,
    bindings: Vec<Binding>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Send {
    text: String,
    revision: String,
}
fn config_path(dirs: &[PathBuf], name: &str) -> Result<PathBuf, Error> {
    if !is_valid_agent_name(name) {
        return Err(bad("Invalid assistant name"));
    }
    resolve_agent_paths(dirs, name)
        .map(|(p, _)| p.with_extension("channels.json"))
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "Assistant not found"))
}
async fn load_at(dirs: &[PathBuf], name: &str) -> Result<(PathBuf, String, Vec<Binding>), Error> {
    let path = config_path(dirs, name)?;
    let raw = match tokio::fs::read_to_string(&path).await {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "[]".into(),
        Err(e) => return Err(internal(e)),
    };
    let bindings: Vec<Binding> = serde_json::from_str(&raw).map_err(internal)?;
    for binding in &bindings {
        binding.validate()?;
    }
    Ok((path, raw, bindings))
}
fn revision(raw: &str) -> String {
    format!("{:x}", Sha256::digest(raw.as_bytes()))
}
fn providers() -> Value {
    json!([
     {"id":"slack","name":"Slack","configured":trusty_channels::slack::api::client::BaseClient::new().is_ok_and(|c|c.has_token()),"can_send":true,"can_read":true,"can_receive":true,"receive_reason":"Automatic updates require the Slack bot listener to be running, the channel paired, and sender authorized"},
     {"id":"telegram","name":"Telegram","configured":trusty_channels::telegram::api::client::BaseClient::new().is_ok_and(|c|c.has_token()),"can_send":true,"can_read":false,"can_receive":false,"receive_reason":"Telegram incoming updates are not supported by this integration"}
    ])
}
pub(crate) async fn read(name: &str) -> Result<Value, Error> {
    let (_, raw, bindings) = load_at(&crate::agents::agents_dir_candidates(), name).await?;
    let listeners = agent_listeners::read(name).await?;
    Ok(
        json!({"agent":name,"revision":revision(&raw),"bindings":bindings,"providers":providers(),"listeners":listeners}),
    )
}
async fn write_at(dirs: &[PathBuf], name: &str, update: Update) -> Result<(), Error> {
    let _guard = super::AGENT_CONFIG_WRITE_LOCK.lock().await;
    let (path, raw, _) = load_at(dirs, name).await?;
    if revision(&raw) != update.revision {
        return Err(err(
            StatusCode::CONFLICT,
            "Channel settings changed. Reload before saving.",
        ));
    }
    if update.bindings.len() > 32 {
        return Err(bad("At most 32 channel bindings are supported"));
    }
    let mut ids = std::collections::HashSet::new();
    for binding in &update.bindings {
        binding.validate()?;
        if !ids.insert(&binding.id) {
            return Err(bad("Channel binding IDs must be unique"));
        }
    }
    let bytes = serde_json::to_vec_pretty(&update.bindings).map_err(internal)?;
    agent_listeners::atomic_write(&path, &bytes)
        .await
        .map_err(internal)
}
async fn bound(name: &str, id: &str, send: bool) -> Result<Binding, Error> {
    let (_, _, bindings) = load_at(&crate::agents::agents_dir_candidates(), name).await?;
    authorized(&bindings, id, send).cloned()
}
fn authorized<'a>(bindings: &'a [Binding], id: &str, send: bool) -> Result<&'a Binding, Error> {
    let binding = bindings.iter().find(|b| b.id == id).ok_or_else(|| {
        err(
            StatusCode::NOT_FOUND,
            "Channel is not bound to this assistant",
        )
    })?;
    if !binding.enabled || (send && !binding.send_enabled) {
        return Err(err(
            StatusCode::FORBIDDEN,
            "This channel operation is disabled for the assistant",
        ));
    }
    Ok(binding)
}
async fn prepare_send(
    dirs: &[PathBuf],
    name: &str,
    id: &str,
    expected_revision: &str,
) -> Result<Binding, Error> {
    let (_, raw, bindings) = load_at(dirs, name).await?;
    if revision(&raw) != expected_revision {
        return Err(err(
            StatusCode::CONFLICT,
            "Channel settings changed. Reload before sending.",
        ));
    }
    authorized(&bindings, id, true).cloned()
}
pub(crate) async fn send(
    name: &str,
    id: &str,
    text: &str,
    expected_revision: &str,
) -> Result<Value, Error> {
    if text.trim().is_empty() || text.chars().count() > 4000 {
        return Err(bad("Message must contain 1–4000 characters"));
    }
    let binding = prepare_send(
        &crate::agents::agents_dir_candidates(),
        name,
        id,
        expected_revision,
    )
    .await?;
    let failure = || {
        err(
            StatusCode::BAD_GATEWAY,
            "Channel provider request failed; check connection and access",
        )
    };
    match binding.provider.as_str() {
        "slack" => {
            let client =
                trusty_channels::slack::api::client::BaseClient::new().map_err(|_| failure())?;
            let result=client.call_method("chat.postMessage",&json!({"channel":binding.target,"text":text,"unfurl_links":false,"unfurl_media":false})).await.map_err(|_|failure())?;
            Ok(json!({"ok":true,"message_id":result["ts"]}))
        }
        "telegram" => {
            let client =
                trusty_channels::telegram::api::client::BaseClient::new().map_err(|_| failure())?;
            let result=client.call_method("sendMessage",&json!({"chat_id":binding.target,"text":text,"link_preview_options":{"is_disabled":true}})).await.map_err(|_|failure())?;
            Ok(json!({"ok":true,"message_id":result["result"]["message_id"]}))
        }
        _ => Err(bad("Unsupported channel provider")),
    }
}
pub(crate) async fn messages(name: &str, id: &str) -> Result<Value, Error> {
    let binding = bound(name, id, false).await?;
    if binding.provider != "slack" {
        return Ok(
            json!({"available":false,"messages":[],"reason":"Telegram bots cannot read prior chat history"}),
        );
    }
    let failure = || {
        err(
            StatusCode::BAD_GATEWAY,
            "Channel history could not be read; check connection and access",
        )
    };
    let client = trusty_channels::slack::api::client::BaseClient::new().map_err(|_| failure())?;
    let result = client
        .call_method(
            "conversations.history",
            &json!({"channel":binding.target,"limit":30}),
        )
        .await
        .map_err(|_| failure())?;
    let messages=result["messages"].as_array().into_iter().flatten().map(|v|json!({"id":v["ts"],"text":v["text"].as_str().unwrap_or("").chars().take(8000).collect::<String>(),"from":v["user"],"timestamp":v["ts"]})).collect::<Vec<_>>();
    Ok(json!({"available":true,"messages":messages}))
}
pub(super) async fn get_route(AxumPath(name): AxumPath<String>) -> Result<Json<Value>, Error> {
    read(&name).await.map(Json)
}
pub(super) async fn put_route(
    AxumPath(name): AxumPath<String>,
    Json(update): Json<Update>,
) -> Result<Json<Value>, Error> {
    write_at(&crate::agents::agents_dir_candidates(), &name, update).await?;
    read(&name).await.map(Json)
}
pub(super) async fn send_route(
    AxumPath((name, id)): AxumPath<(String, String)>,
    Json(body): Json<Send>,
) -> Result<Json<Value>, Error> {
    send(&name, &id, &body.text, &body.revision).await.map(Json)
}
pub(super) async fn messages_route(
    AxumPath((name, id)): AxumPath<(String, String)>,
) -> Result<Json<Value>, Error> {
    messages(&name, &id).await.map(Json)
}

fn receive_selection<'a>(
    bindings: &'a [Binding],
    channel: &str,
    event: &crate::listeners::store::StoredEvent,
    persona_allowed: bool,
) -> (bool, Option<&'a Binding>) {
    let destinations: Vec<_> = bindings
        .iter()
        .filter(|b| b.provider == "slack" && b.target == channel)
        .collect();
    let claimed = !destinations.is_empty();
    let selected = if persona_allowed {
        destinations.into_iter().find(|b| {
            b.enabled
                && b.receive_enabled
                && crate::listeners::wake::binding_matches_event(
                    &AgentListenerBinding {
                        name: event.listener_id.clone(),
                        enabled: true,
                        event_types: vec![],
                        filter: b.filter.clone(),
                        instructions: b.instructions.clone(),
                    },
                    event,
                )
        })
    } else {
        None
    };
    (claimed, selected)
}

/// Existing authenticated Slack intake calls this; only saved assistant destinations match.
pub(crate) async fn receive_slack(
    channel: &str,
    event: &crate::listeners::store::StoredEvent,
    root: &std::path::Path,
    user: &crate::rbac::UserIdentity,
    allowed_personas: Option<&[String]>,
) -> bool {
    let dirs = crate::agents::agents_dir_candidates();
    let Ok(names) = crate::listeners::wake::candidate_agent_names().await else {
        return false;
    };
    let mut claimed = false;
    for name in names {
        let Ok((_, _, bindings)) = load_at(&dirs, &name).await else {
            continue;
        };
        let (bound, binding) = receive_selection(
            &bindings,
            channel,
            event,
            allowed_personas.is_none_or(|allowed| allowed.iter().any(|v| v == &name)),
        );
        claimed |= bound;
        let Some(binding) = binding else {
            continue;
        };
        let prompt =
            crate::listeners::wake::build_wake_prompt(event, None, Some(&binding.instructions));
        let metadata=json!({"kind":"trusty.listener-event","version":1,"listener":binding.name,"event_id":event.id,"event_type":event.event_type,"from":event.from,"subject":event.subject}).to_string();
        let name = name.clone();
        let root = root.to_path_buf();
        let user = user.clone();
        tokio::spawn(async move {
            let _result = crate::listeners::wake::LISTENER_CHAT_EVENT
                .scope(
                    metadata,
                    crate::ctrl::pm_task::run_pm_task_with_persona(
                        &root,
                        &name,
                        &prompt,
                        &[],
                        None,
                        crate::ctrl::config::SessionOverrides {
                            user: Some(user),
                            ..Default::default()
                        },
                    ),
                )
                .await;
            // Incoming updates remain private in the assistant chat; sending requires an explicit UI or tool request.
        });
    }
    claimed
}
#[cfg(test)]
mod tests {
    use super::*;
    fn binding() -> Binding {
        serde_json::from_value(json!({"id":"team","name":"Team","provider":"slack","target":"C123456","enabled":true,"send_enabled":true})).unwrap()
    }
    #[test]
    fn agent_channels_validates_provider_destination_and_permissions() {
        let mut b = binding();
        assert!(b.validate().is_ok());
        b.target = "https://host".into();
        assert!(b.validate().is_err());
        b.provider = "telegram".into();
        b.target = "123".into();
        b.receive_enabled = true;
        assert!(b.validate().is_err());
        let b = binding();
        let list = [b];
        assert!(authorized(&list, "other", true).is_err());
    }
    #[tokio::test]
    async fn agent_channels_revision_preserves_bindings_and_rejects_stale_write() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join("fixture.toml"), "[agent]\nname='fixture'\n")
            .await
            .unwrap();
        let dirs = [dir.path().to_path_buf()];
        let (_, raw, _) = load_at(&dirs, "fixture").await.unwrap();
        let rev = revision(&raw);
        write_at(
            &dirs,
            "fixture",
            Update {
                revision: rev.clone(),
                bindings: vec![binding()],
            },
        )
        .await
        .unwrap();
        assert_eq!(load_at(&dirs, "fixture").await.unwrap().2.len(), 1);
        assert_eq!(
            write_at(
                &dirs,
                "fixture",
                Update {
                    revision: rev,
                    bindings: vec![]
                }
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::CONFLICT
        );
        assert!(load_at(&dirs, "../escape").await.is_err());
    }
}

#[cfg(test)]
mod receive_tests {
    use super::*;
    #[test]
    fn agent_channels_receive_never_falls_back_for_disabled_bound_destination() {
        let mut binding:Binding=serde_json::from_value(json!({"id":"team","name":"Team","provider":"slack","target":"C123","enabled":true,"receive_enabled":true,"filter":{"from":["Owner"]}})).unwrap();
        let event = crate::listeners::store::StoredEvent {
            id: "x".into(),
            listener_id: "slack".into(),
            provider: "slack".into(),
            event_type: "message.im".into(),
            ts: "now".into(),
            from: Some("Owner".into()),
            subject: None,
            snippet: Some("Hello".into()),
            included: true,
            labels: vec![],
        };
        assert!(
            receive_selection(std::slice::from_ref(&binding), "C123", &event, true)
                .1
                .is_some()
        );
        assert_eq!(
            receive_selection(std::slice::from_ref(&binding), "COTHER", &event, true).0,
            false
        );
        assert!(
            receive_selection(std::slice::from_ref(&binding), "C123", &event, false)
                .1
                .is_none()
        );
        binding.receive_enabled = false;
        let (claimed, selected) =
            receive_selection(std::slice::from_ref(&binding), "C123", &event, true);
        assert!(claimed);
        assert!(selected.is_none());
        binding.receive_enabled = true;
        binding.filter.from = vec!["Other".into()];
        assert!(
            receive_selection(std::slice::from_ref(&binding), "C123", &event, true)
                .1
                .is_none()
        );
    }
}

#[cfg(test)]
mod stale_send_tests {
    use super::*;
    #[tokio::test]
    async fn agent_channels_stale_send_rejects_retarget_before_provider() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join("fixture.toml"), "[agent]\nname='fixture'")
            .await
            .unwrap();
        let dirs = [dir.path().to_path_buf()];
        let (path, _, _) = load_at(&dirs, "fixture").await.unwrap();
        let original=json!([{"id":"team","name":"Team","provider":"slack","target":"COLD","enabled":true,"send_enabled":true}]).to_string();
        tokio::fs::write(&path, &original).await.unwrap();
        let changed = original.replace("COLD", "CNEW");
        tokio::fs::write(&path, &changed).await.unwrap();
        assert_eq!(
            prepare_send(&dirs, "fixture", "team", &revision(&original))
                .await
                .unwrap_err()
                .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            prepare_send(&dirs, "fixture", "team", &revision(&changed))
                .await
                .unwrap()
                .target,
            "CNEW"
        );
    }
}
