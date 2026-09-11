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
    /// Name of the credential this binding sends as, never a credential value.
    ///
    /// Why (#7427): every send used to authenticate with whichever
    /// process-global token was in the environment, so two assistants bound to
    /// two workspaces could not send as different identities and nothing in the
    /// binding recorded which credential it meant. `None` keeps that behaviour.
    /// What: a `trusty_common::credentials::CredentialRef` — `slack`,
    /// `slack-app`, `telegram`, or a qualified `slack/second-workspace`. It is
    /// confined to the adapter's own provider family, so a Slack binding cannot
    /// name `github`; see [`crate::channels::credentials`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
}
impl Binding {
    /// Reject a binding the assistant could not act on.
    ///
    /// Why: runs on every load and every save, so a configuration that cannot
    /// work never reaches the point where a send silently goes nowhere.
    /// What: identity and name limits, then the provider's own rules —
    /// #7427 resolves the provider through [`crate::channels::adapter`], so an
    /// unregistered id (Notion, gworkspace) is refused here rather than
    /// accepted and left inert, and a `receive_enabled` binding is refused on a
    /// provider whose [`crate::channels::Capabilities`] say it has no inbound
    /// path.
    /// Test: `agent_channels_validates_provider_destination_and_permissions`,
    /// `agent_channels_rejects_unregistered_provider`.
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if !is_valid_agent_name(&self.id)
            || self.name.trim().is_empty()
            || self.name.chars().count() > 128
        {
            return Err(bad(
                "Channel ID and name are required (maximum 128 characters)",
            ));
        }
        let Some(adapter) = crate::channels::adapter(&self.provider) else {
            return Err(bad("Unsupported channel provider"));
        };
        if !adapter.validate_target(&self.target) {
            return Err(bad(
                "Choose Slack channel ID or Telegram chat ID for the selected provider",
            ));
        }
        if self.receive_enabled && !adapter.capabilities().can_receive {
            return Err(bad(&format!(
                "{} incoming updates are not available through this channel integration",
                adapter.display_name()
            )));
        }
        if let Some(reference) = &self.credential_ref
            && let Err(e) = crate::channels::validate_credential_ref(
                reference,
                adapter.credential_providers(),
                adapter.credential_env_prefix(),
            )
        {
            return Err(bad(&format!("Channel credential reference: {e}")));
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
pub(crate) async fn load_at(
    dirs: &[PathBuf],
    name: &str,
) -> Result<(PathBuf, String, Vec<Binding>), Error> {
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
/// The channel view's payload: bindings, what each provider can do, and how the
/// bindings have actually been behaving.
///
/// Why (#7427): `providers` now comes from the adapter registry rather than a
/// hand-written literal, and `status` publishes the per-binding
/// dispatch-failure counter so a binding that is dropping inbound wakes says so
/// instead of reading as healthy.
/// Test: `channel_providers_json_reports_registry_capabilities`,
/// `channel_dispatch_failure_is_counted_per_binding`.
pub(crate) async fn read(name: &str) -> Result<Value, Error> {
    let (_, raw, bindings) = load_at(&crate::agents::agents_dir_candidates(), name).await?;
    let listeners = agent_listeners::read(name).await?;
    Ok(
        json!({"agent":name,"revision":revision(&raw),"bindings":bindings,"providers":crate::channels::providers_json(),"listeners":listeners,"status":crate::channels::status::status_json(name)}),
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
/// Map an adapter failure onto the HTTP answer the caller sees.
///
/// Why (#7427): the statuses and copy are the ones this endpoint returned
/// before the adapter split, so the UI and the `channel` tool are unchanged. A
/// credential that will not resolve is separated out because the operator fix
/// is different — the binding names a credential this host does not have.
fn channel_failure(binding_id: &str, error: crate::channels::ChannelError) -> Error {
    match error {
        crate::channels::ChannelError::UnsupportedProvider(_) => {
            tracing::warn!(binding = binding_id, %error, "channel operation failed");
            bad("Unsupported channel provider")
        }
        crate::channels::ChannelError::ReceiveUnsupported(_) => {
            tracing::warn!(binding = binding_id, %error, "channel operation failed");
            bad("This provider does not deliver incoming updates")
        }
        // #7427: a credential that will not resolve stops delivery outright and
        // keeps stopping it until an operator changes the binding or the store.
        // `warn` put it below the default filter of a daemon that is otherwise
        // healthy, so the operator saw a channel that simply never answered.
        crate::channels::ChannelError::Credential(_) => {
            tracing::error!(
                binding = binding_id,
                %error,
                "channel credential could not be resolved; this binding cannot send or receive until it is fixed"
            );
            err(
                StatusCode::BAD_GATEWAY,
                "Channel credential could not be resolved; check the binding's credential reference",
            )
        }
        crate::channels::ChannelError::Provider { .. } => {
            tracing::warn!(binding = binding_id, %error, "channel operation failed");
            err(
                StatusCode::BAD_GATEWAY,
                "Channel provider request failed; check connection and access",
            )
        }
    }
}
/// The credential reference the Telegram long-poll gateway authenticates with.
///
/// Why (#7427): one process polls `getUpdates` for every assistant, so the loop
/// needs one credential before it knows which binding an update will match. The
/// first enabled receiving Telegram binding names it; `None` means no assistant
/// configured one and the adapter's default key applies, which is the token the
/// gateway used before this change.
/// What: scans the same assistant roster the inbound path scans. An assistant
/// whose channel file will not load is skipped rather than failing the scan —
/// `Binding::validate` already refused it at save time, and a single bad file
/// must not take the gateway down for every other assistant.
/// Test: `agent_channels_poll_credential_ref_reads_the_first_receiving_binding`.
pub(crate) async fn telegram_poll_credential_ref() -> Option<String> {
    let dirs = crate::agents::agents_dir_candidates();
    let names = crate::listeners::wake::candidate_agent_names().await.ok()?;
    for name in names {
        let Ok((_, _, bindings)) = load_at(&dirs, &name).await else {
            continue;
        };
        if let Some(reference) = first_telegram_credential_ref(&bindings) {
            return Some(reference);
        }
    }
    None
}
/// The credential reference of the first enabled receiving Telegram binding.
///
/// Why: split out of [`telegram_poll_credential_ref`] so the selection rule is
/// testable without an assistant roster on disk.
/// Test: `agent_channels_poll_credential_ref_reads_the_first_receiving_binding`.
fn first_telegram_credential_ref(bindings: &[Binding]) -> Option<String> {
    bindings
        .iter()
        .find(|b| b.provider == "telegram" && b.enabled && b.receive_enabled)
        .and_then(|b| b.credential_ref.clone())
}
/// Send user-requested text to a saved destination.
///
/// Why (#7427): the per-provider arms moved into the adapters, so this function
/// is the permission and freshness gate and nothing else — it still refuses an
/// oversized message, a stale revision, and a binding whose send is disabled,
/// all before the provider is contacted.
/// Test: `agent_channels_stale_send_rejects_retarget_before_provider`.
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
    let adapter = crate::channels::require_adapter(&binding.provider)
        .map_err(|e| channel_failure(&binding.id, e))?;
    adapter
        .send(&binding, text)
        .await
        .map_err(|e| channel_failure(&binding.id, e))
}
pub(crate) async fn messages(name: &str, id: &str) -> Result<Value, Error> {
    let binding = bound(name, id, false).await?;
    let capabilities = crate::channels::require_adapter(&binding.provider)
        .map_err(|e| channel_failure(&binding.id, e))?
        .capabilities();
    if !capabilities.can_read {
        return Ok(json!({"available":false,"messages":[],"reason":capabilities.read_reason}));
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

/// Which saved destination, if any, this inbound event belongs to.
///
/// Why (#7427 PR 2): the provider was hard-coded to `"slack"`, so a Telegram
/// update could not select a binding however it was configured. Taking the
/// provider as an argument is what lets one inbound path serve both, and it is
/// what confines a Telegram update to bindings that name a Telegram chat id —
/// a message from an unbound chat matches nothing and is never dispatched.
/// What: returns `(claimed, selected)` — `claimed` is true when any binding
/// names this destination at all (so the gateway knows the assistant owns this
/// conversation even when the binding is disabled), `selected` is the one
/// enabled, receive-enabled binding whose filter the event passes.
/// Test: `agent_channels_receive_never_falls_back_for_disabled_bound_destination`,
/// `agent_channels_inbound_ignores_an_unbound_telegram_chat`.
fn receive_selection<'a>(
    bindings: &'a [Binding],
    provider: &str,
    channel: &str,
    event: &crate::listeners::store::StoredEvent,
    persona_allowed: bool,
) -> (bool, Option<&'a Binding>) {
    let destinations: Vec<_> = bindings
        .iter()
        .filter(|b| b.provider == provider && b.target == channel)
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

/// The one inbound path: an authenticated provider event, matched against every
/// assistant's saved destinations and dispatched as a wake.
///
/// Why (#7427): this was `receive_slack`, with `"slack"` written into its
/// binding filter, so Telegram had to grow a second dispatch path of its own —
/// which is how the two ended up on different prompt shapes and different
/// failure handling. One function, with the provider as an argument, is what
/// makes DOC-60 §8's "one envelope regardless of channel" true rather than
/// merely intended. The wake dispatch also used to be spawned and its result
/// discarded (`let _result = …`), so a failed `run_pm_task_with_persona`
/// produced no log line and no visible change — the binding read as healthy
/// while every inbound message was dropped. The dispatch now goes through
/// [`crate::channels::status::record_dispatch`], which logs at error level and
/// increments the per-binding counter the channel view reads.
/// What: per assistant, selects the bound destination this event matches, asks
/// the provider's adapter for a wake prompt, and spawns the dispatch. Returns
/// whether any assistant claimed the destination — a caller with its own
/// fallback (the Slack session map, the Telegram long-poll gateway) uses that
/// to decide whether to handle the message itself.
/// Test: `agent_channels_receive_never_falls_back_for_disabled_bound_destination`,
/// `agent_channels_inbound_ignores_an_unbound_telegram_chat`,
/// `channel_dispatch_failure_is_counted_per_binding`.
pub(crate) async fn receive_inbound(
    provider: &str,
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
            provider,
            channel,
            event,
            allowed_personas.is_none_or(|allowed| allowed.iter().any(|v| v == &name)),
        );
        claimed |= bound;
        let Some(binding) = binding else {
            continue;
        };
        let Some(adapter) = crate::channels::adapter(&binding.provider) else {
            continue;
        };
        let wake = adapter
            .receive(
                binding,
                crate::channels::InboundEvent {
                    agent: &name,
                    event,
                },
            )
            .await;
        let binding_id = binding.id.clone();
        let wake = match wake {
            Ok(Some(wake)) => wake,
            Ok(None) => continue,
            Err(e) => {
                // #7427: an inbound event that cannot even produce a prompt is
                // counted too, not dropped.
                tracing::error!(assistant = %name, binding = %binding_id, %e, "channel inbound could not be prepared");
                crate::channels::status::record_failure(&name, &binding_id, &e.to_string());
                continue;
            }
        };
        let name = name.clone();
        let root = root.to_path_buf();
        let user = user.clone();
        tokio::spawn(async move {
            let dispatch = crate::listeners::wake::LISTENER_CHAT_EVENT.scope(
                wake.metadata,
                crate::ctrl::pm_task::run_pm_task_with_persona(
                    &root,
                    &name,
                    &wake.prompt,
                    &[],
                    None,
                    crate::ctrl::config::SessionOverrides {
                        user: Some(user),
                        ..Default::default()
                    },
                ),
            );
            // Incoming updates remain private in the assistant chat; sending
            // requires an explicit UI or tool request. #7427 keeps Telegram on
            // that same rule: a turn woken by a Telegram message reaches the
            // chat only when the persona calls the `channel` tool, which routes
            // through `send` above and therefore `TelegramAdapter::send`. The
            // reply is never auto-posted, so a binding with `send_enabled`
            // false can receive without being able to answer.
            crate::channels::status::record_dispatch(&name, &binding_id, dispatch).await;
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
        // #7427 PR 2: this used to assert `is_err()` — a Telegram binding could
        // not ask for incoming updates because the adapter said the provider
        // had no inbound path at all.
        b.receive_enabled = true;
        assert!(b.validate().is_ok());
        b.target = "C123456".into();
        assert!(b.validate().is_err());
        let b = binding();
        let list = [b];
        assert!(authorized(&list, "other", true).is_err());
    }
    /// A receiving Telegram binding saves, and its credential reference stays
    /// confined to Telegram's own family.
    ///
    /// Pre-change the first assertion fails: `Binding::validate` refused every
    /// `receive_enabled` Telegram binding, so inbound could not be configured.
    #[test]
    fn agent_channels_telegram_binding_accepts_receive_enabled() {
        let mut b: Binding = serde_json::from_value(json!({
            "id":"owner","name":"Owner DM","provider":"telegram","target":"123456",
            "enabled":true,"send_enabled":true,"receive_enabled":true
        }))
        .unwrap();
        assert!(b.validate().is_ok());
        b.credential_ref = Some("telegram".into());
        assert!(b.validate().is_ok());
        b.credential_ref = Some("slack".into());
        assert!(b.validate().is_err());
    }
    #[test]
    fn agent_channels_poll_credential_ref_reads_the_first_receiving_binding() {
        let telegram = |receive: bool, reference: Option<&str>| -> Binding {
            let mut value = json!({
                "id":"owner","name":"Owner DM","provider":"telegram","target":"123456",
                "enabled":true,"receive_enabled":receive
            });
            if let Some(reference) = reference {
                value["credential_ref"] = json!(reference);
            }
            serde_json::from_value(value).unwrap()
        };
        assert_eq!(
            first_telegram_credential_ref(&[binding(), telegram(true, Some("telegram"))]),
            Some("telegram".to_string())
        );
        // A send-only binding never chooses the poll loop's identity.
        assert_eq!(
            first_telegram_credential_ref(&[telegram(false, Some("telegram"))]),
            None
        );
        // Configured nothing: the adapter's default key applies.
        assert_eq!(first_telegram_credential_ref(&[telegram(true, None)]), None);
    }
    /// Pins the current state until `trusty-channels` grows a Notion connector
    /// (#7427 PR 3): a provider with no registered adapter is refused at save
    /// time rather than stored and left silently inert.
    #[test]
    fn agent_channels_rejects_unregistered_provider() {
        let mut b = binding();
        b.provider = "notion".into();
        b.target = "some-notion-page".into();
        let rejection = b.validate().unwrap_err();
        assert_eq!(rejection.0, StatusCode::BAD_REQUEST);
        assert_eq!(
            rejection.1.0["error"],
            json!("Unsupported channel provider")
        );
        assert!(!providers_contains("notion"));
    }
    fn providers_contains(id: &str) -> bool {
        crate::channels::providers_json()
            .as_array()
            .is_some_and(|list| list.iter().any(|p| p["id"] == json!(id)))
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
            receive_selection(
                std::slice::from_ref(&binding),
                "slack",
                "C123",
                &event,
                true
            )
            .1
            .is_some()
        );
        assert!(
            !receive_selection(
                std::slice::from_ref(&binding),
                "slack",
                "COTHER",
                &event,
                true
            )
            .0
        );
        assert!(
            receive_selection(
                std::slice::from_ref(&binding),
                "slack",
                "C123",
                &event,
                false
            )
            .1
            .is_none()
        );
        binding.receive_enabled = false;
        let (claimed, selected) = receive_selection(
            std::slice::from_ref(&binding),
            "slack",
            "C123",
            &event,
            true,
        );
        assert!(claimed);
        assert!(selected.is_none());
        binding.receive_enabled = true;
        binding.filter.from = vec!["Other".into()];
        assert!(
            receive_selection(
                std::slice::from_ref(&binding),
                "slack",
                "C123",
                &event,
                true
            )
            .1
            .is_none()
        );
    }

    /// A Telegram update selects only the binding naming its own chat id, and a
    /// message from an unbound chat is never dispatched.
    ///
    /// Pre-change this test does not compile: `receive_selection` filtered on a
    /// literal `"slack"`, so no Telegram binding could ever be selected and the
    /// function took no provider to ask about.
    #[test]
    fn agent_channels_inbound_ignores_an_unbound_telegram_chat() {
        let binding: Binding = serde_json::from_value(json!({
            "id":"owner","name":"Owner DM","provider":"telegram","target":"123456",
            "enabled":true,"receive_enabled":true
        }))
        .unwrap();
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
        let bound = std::slice::from_ref(&binding);
        let (claimed, selected) = receive_selection(bound, "telegram", "123456", &event, true);
        assert!(claimed);
        assert_eq!(selected.map(|b| b.id.as_str()), Some("owner"));

        // An unbound chat id: not claimed, not selected, so the gateway's own
        // fallback keeps handling it.
        let (claimed, selected) = receive_selection(bound, "telegram", "999999", &event, true);
        assert!(!claimed);
        assert!(selected.is_none());

        // The same chat id under another provider is a different destination.
        assert!(!receive_selection(bound, "slack", "123456", &event, true).0);
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
