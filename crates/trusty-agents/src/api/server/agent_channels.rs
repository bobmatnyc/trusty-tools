//! Assistant-bound channel destinations over trusty-channels provider clients.
//!
//! This module is the configuration surface: load, validate, save, and send on
//! a binding. The provider-neutral INBOUND path — which binding owns an arriving
//! message, and whether it may spend a model dispatch — is [`inbound`] (#7427).
pub(crate) mod inbound;

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
    /// Normalized event types that may wake the assistant; empty = any.
    ///
    /// Why (#7609): a binding migrated out of `agent.toml`'s `[[listeners]]`
    /// carries `event_types`, which this shape had nowhere to put. Absent and
    /// empty are the same thing and the key is omitted when empty, so a
    /// channels file written before this field round-trips byte-for-byte.
    /// Test: `agent_channels_round_trips_an_existing_file_byte_for_byte`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub event_types: Vec<String>,
}

// #7609: `Binding` is the stored shape of an Assistant-scope
// `crate::channels::Channel`; these two conversions are the whole of that
// claim, so the migration and slice 5 never hand-copy the field list.
impl From<&Binding> for crate::channels::Channel {
    fn from(binding: &Binding) -> Self {
        Self {
            id: binding.id.clone(),
            name: binding.name.clone(),
            provider: binding.provider.clone(),
            scope: crate::channels::ChannelScope::Assistant,
            target: binding.target.clone(),
            enabled: binding.enabled,
            send_enabled: binding.send_enabled,
            receive_enabled: binding.receive_enabled,
            credential_ref: binding.credential_ref.clone(),
            instructions: binding.instructions.clone(),
            event_types: binding.event_types.clone(),
            wake_filter: binding.filter.clone(),
            ..crate::channels::Channel::default()
        }
    }
}

impl From<&crate::channels::Channel> for Binding {
    fn from(channel: &crate::channels::Channel) -> Self {
        Self {
            id: channel.id.clone(),
            name: channel.name.clone(),
            provider: channel.provider.clone(),
            target: channel.target.clone(),
            enabled: channel.enabled,
            send_enabled: channel.send_enabled,
            receive_enabled: channel.receive_enabled,
            filter: channel.wake_filter.clone(),
            instructions: channel.instructions.clone(),
            credential_ref: channel.credential_ref.clone(),
            event_types: channel.event_types.clone(),
        }
    }
}

impl Binding {
    /// Reject a binding the assistant could not act on.
    ///
    /// Why: runs on every load and every save, so a configuration that cannot
    /// work never reaches the point where a send silently goes nowhere.
    /// What: identity and name limits, then the provider's own rules —
    /// #7427 resolves the provider through [`crate::channels::adapter`], so an
    /// unregistered id (Notion, until `trusty-channels` grows a connector) is
    /// refused here rather than accepted and left inert, and a
    /// `receive_enabled` binding is refused on a
    /// provider whose [`crate::channels::Capabilities`] say it has no inbound
    /// path.
    ///
    /// #7609: `globals` is the harness-wide `[[channels]]` list, because one
    /// shape legitimately carries no destination. A `[[listeners]]` binding
    /// absorbed out of `agent.toml` is an OVERLAY of a global channel — this
    /// assistant's `wake_filter`, `event_types`, `instructions` and `enabled`
    /// over the global's provider, destination and ingest filter — and it never
    /// named a destination, so the target grammar refused it and slice 3 had to
    /// leave it unmigrated. The destination check is skipped when
    /// [`crate::channels::dispatch::is_overlay`] holds, which keeps an
    /// operator-authored blank target refused with the message it always had.
    /// Test: `agent_channels_validates_provider_destination_and_permissions`,
    /// `agent_channels_rejects_unregistered_provider`,
    /// `an_overlay_of_a_global_channel_validates`,
    /// `a_blank_target_with_no_global_is_still_refused`.
    pub(crate) fn validate_in(&self, globals: &[crate::channels::Channel]) -> Result<(), Error> {
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
        // #7609: an overlay of a global channel is the one shape that may carry
        // no destination — see `validate_in`. Keyed on `id`, which is what
        // dispatch matches the global on (#7609 review).
        if !crate::channels::dispatch::is_overlay(&self.id, &self.target, globals)
            && !adapter.validate_target(&self.target)
        {
            // #7427: gworkspace targets are `from:<address>` / `label:<id>`,
            // so the copy can no longer name only the two id-shaped providers.
            return Err(bad(
                "Choose a Slack channel ID, a Telegram chat ID, or a Gmail from:<address> or label:<label> for the selected provider",
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
            event_types: self.event_types.clone(),
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
/// One assistant's saved channel bindings, validated on the way in.
///
/// Why (#7609): the harness-wide `[[channels]]` list is now part of what makes
/// a stored record valid — an overlay's blank target is only legitimate beside
/// a global of the same name — so this reads it before validating. The inbound
/// path already holds that list and uses [`load_at_with`] instead, rather than
/// re-reading the global config once per assistant per event.
/// Test: `agent_channels_revision_preserves_bindings_and_rejects_stale_write`.
pub(crate) async fn load_at(
    dirs: &[PathBuf],
    name: &str,
) -> Result<(PathBuf, String, Vec<Binding>), Error> {
    let globals = crate::mcp::config::GlobalConfig::load().await.channels;
    load_at_with(dirs, name, &globals).await
}

/// [`load_at`] over an already-loaded global channel list.
///
/// Why (#7609 review): an overlay names a global by `id`, and an operator who
/// deletes or re-ids that global leaves the overlay unresolvable. Failing the
/// whole read for it took the assistant off every OTHER destination too — both
/// `read()` and `write_at()` 400, so there was no way back through the UI.
/// What: one unresolvable overlay is DROPPED with a warning and the rest of the
/// file loads. Every other validation failure still fails the read, so a
/// genuinely broken file is never half-loaded; `write_at` keeps rejecting an
/// operator-authored blank target outright. The stored file is untouched — the
/// revision hashes its raw text — so the record survives until a client saves
/// back the list it read.
/// Test: `an_unresolvable_overlay_is_dropped_not_a_whole_file_rejection`.
pub(crate) async fn load_at_with(
    dirs: &[PathBuf],
    name: &str,
    globals: &[crate::channels::Channel],
) -> Result<(PathBuf, String, Vec<Binding>), Error> {
    let path = config_path(dirs, name)?;
    let raw = match tokio::fs::read_to_string(&path).await {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "[]".into(),
        Err(e) => return Err(internal(e)),
    };
    let stored: Vec<Binding> = serde_json::from_str(&raw).map_err(internal)?;
    let mut bindings = Vec::with_capacity(stored.len());
    for binding in stored {
        if binding.target.is_empty()
            && !crate::channels::dispatch::is_overlay(&binding.id, &binding.target, globals)
        {
            tracing::warn!(
                assistant = %name, channel = %binding.id,
                "channel config: this overlay names a global channel this host does not declare; \
                 dropping the record and loading the rest (#7609)"
            );
            continue;
        }
        binding.validate_in(globals)?;
        bindings.push(binding);
    }
    Ok((path, raw, bindings))
}
/// The global-channel ids one assistant's STORED bindings overlay (#8187).
///
/// Why: deleting a global channel orphans every overlay of it — `load_at_with`
/// drops such a record with a warning, so the assistant loses that destination
/// with nothing said to whoever deleted it.
/// [`super::global_channels::delete`] asks this first so it can name the
/// assistants an operator is about to affect.
/// What: the stored file parsed but NOT validated, because an overlay's
/// legitimacy depends on the very global list the caller is about to change.
/// An overlay is a binding with a blank target, keyed by `id` — the key
/// [`crate::channels::dispatch::is_overlay`] matches on. A missing file is no
/// overlays; an unreadable or unparseable one is an `Err` the caller must not
/// read as "no references".
/// Test: `super::tests::global_channels::a_referenced_global_channel_is_not_deleted_without_force`.
pub(super) async fn overlaid_global_ids(
    dirs: &[PathBuf],
    name: &str,
) -> Result<Vec<String>, Error> {
    let path = config_path(dirs, name)?;
    let raw = match tokio::fs::read_to_string(&path).await {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(internal(e)),
    };
    let stored: Vec<Binding> = serde_json::from_str(&raw).map_err(internal)?;
    Ok(stored
        .into_iter()
        .filter(|binding| binding.target.is_empty())
        .map(|binding| binding.id)
        .collect())
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
/// Replace one assistant's stored bindings, under compare-and-swap.
///
/// Why (#7609): the caller audits the write, and an audit line that cannot say
/// how many bindings there were before is not much of a record.
/// What: returns `(before, after)` — the stored count either side of the write.
/// Every refusal (stale revision, invalid binding, duplicate id) writes
/// nothing.
/// Test: `agent_channels_revision_preserves_bindings_and_rejects_stale_write`.
async fn write_at(dirs: &[PathBuf], name: &str, update: Update) -> Result<(usize, usize), Error> {
    let _guard = super::AGENT_CONFIG_WRITE_LOCK.lock().await;
    let (manifest, _) = resolve_agent_paths(dirs, name)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "Assistant not found"))?;
    let _process_lock = crate::knowledge::execution::mutation_guard(&manifest)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    let globals = crate::mcp::config::GlobalConfig::load().await.channels;
    let (path, raw, stored) = load_at_with(dirs, name, &globals).await?;
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
        // #7609: an overlay of a global channel saves through the UI too.
        binding.validate_in(&globals)?;
        if !ids.insert(&binding.id) {
            return Err(bad("Channel binding IDs must be unique"));
        }
    }
    let bytes = serde_json::to_vec_pretty(&update.bindings).map_err(internal)?;
    agent_listeners::atomic_write(&path, &bytes)
        .await
        .map_err(internal)?;
    Ok((stored.len(), update.bindings.len()))
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
        // #8037: a provider whose capabilities CLAIM readable history and whose
        // adapter implements none is a build defect, not a configuration one —
        // it is logged at error level and never rendered as an empty inbox.
        crate::channels::ChannelError::ReadUnsupported(_) => {
            tracing::error!(binding = binding_id, %error, "channel history was claimed available and could not be served");
            err(
                StatusCode::BAD_GATEWAY,
                "Channel history could not be read; check connection and access",
            )
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
        // #7427: a gworkspace `label:` binding can answer an incoming message
        // but has no correspondent of its own to open a thread to.
        crate::channels::ChannelError::Destination { .. } => {
            tracing::warn!(binding = binding_id, %error, "channel operation failed");
            bad(
                "This channel destination cannot start a message; bind a sender address, or reply to an incoming message",
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
/// Prior messages on a bound destination.
///
/// Why (#8037): the Slack Web API call used to be written out here, which is
/// why no other provider could serve history — the stub provider included. The
/// body is now the permission gate plus one adapter call, and
/// `ChannelAdapter::read` owns the provider work.
/// What: a provider whose `can_read` is unset answers `available: false` with
/// the adapter's own reason and never reaches the network. Every other failure
/// maps through [`channel_failure`], so an unresolvable credential is
/// distinguishable from an upstream refusal.
/// Test: `a_stub_channel_sends_and_reads_back_over_http`,
/// `channel_adapter_read_defaults_to_unsupported`.
pub(crate) async fn messages(name: &str, id: &str) -> Result<Value, Error> {
    let binding = bound(name, id, false).await?;
    let adapter = crate::channels::require_adapter(&binding.provider)
        .map_err(|e| channel_failure(&binding.id, e))?;
    let capabilities = adapter.capabilities();
    if !capabilities.can_read {
        return Ok(json!({"available":false,"messages":[],"reason":capabilities.read_reason}));
    }
    let messages = adapter
        .read(&binding)
        .await
        .map_err(|e| channel_failure(&binding.id, e))?;
    Ok(json!({"available":true,"messages":messages}))
}
pub(super) async fn get_route(AxumPath(name): AxumPath<String>) -> Result<Json<Value>, Error> {
    read(&name).await.map(Json)
}
/// `PUT /api/agents/{name}/channels` — replace one assistant's bindings.
///
/// Why (#7609): `writer` is the FIRST argument because extracting it IS the
/// authorization gate — a daemon with no API token configured never reaches
/// this body, even from loopback. See
/// [`crate::api::server::channel_auth`] for why this one write surface is
/// held to more than the loopback bind.
/// What: unchanged otherwise — validate, compare-and-swap, answer with the
/// stored view. One audit line per accepted write.
/// Test: `a_tokenless_daemon_refuses_every_channel_write`,
/// `agent_channels_revision_preserves_bindings_and_rejects_stale_write`.
pub(super) async fn put_route(
    writer: super::channel_auth::ChannelWriter,
    AxumPath(name): AxumPath<String>,
    Json(update): Json<Update>,
) -> Result<Json<Value>, Error> {
    let (before, after) = write_at(&crate::agents::agents_dir_candidates(), &name, update).await?;
    writer.audit(
        "PUT /api/agents/{name}/channels",
        "assistant",
        Some(&name),
        Some(before),
        after,
    );
    read(&name).await.map(Json)
}

/// Replace one assistant's bindings from a MODEL TURN, under the same gate.
///
/// Why (#7609): `platform_settings`'s `settings.patch/channels` op and the
/// `channel` tool's `set` action both write the same file the HTTP route
/// writes, so gating the route alone would leave the model-driven path open on
/// a daemon that refuses the operator's own browser. Neither has a request to
/// extract a [`super::channel_auth::ChannelWriter`] from, so they take the
/// recorded daemon fact instead.
/// What: 401 when this daemon serves no authenticated API; otherwise validate,
/// compare-and-swap, audit, and answer with the stored view.
/// Test: `crate::tools::channel::channel_tests::the_tool_refuses_a_write_on_a_tokenless_daemon`.
pub(crate) async fn write_from_turn(name: &str, update: Update) -> Result<Value, Error> {
    if !super::channel_auth::daemon_token_configured() {
        return Err(err(
            StatusCode::UNAUTHORIZED,
            &super::channel_auth::tool_refusal(),
        ));
    }
    let (before, after) = write_at(&crate::agents::agents_dir_candidates(), name, update).await?;
    super::channel_auth::audit_write(
        "turn:channels",
        "assistant",
        Some(name),
        Some(before),
        after,
        "in-process",
    );
    read(name).await
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

#[cfg(test)]
mod tests {
    use super::*;
    fn binding() -> Binding {
        serde_json::from_value(json!({"id":"team","name":"Team","provider":"slack","target":"C123456","enabled":true,"send_enabled":true})).unwrap()
    }
    #[test]
    fn agent_channels_validates_provider_destination_and_permissions() {
        let mut b = binding();
        assert!(b.validate_in(&[]).is_ok());
        b.target = "https://host".into();
        assert!(b.validate_in(&[]).is_err());
        b.provider = "telegram".into();
        b.target = "123".into();
        // #7427 PR 2: this used to assert `is_err()` — a Telegram binding could
        // not ask for incoming updates because the adapter said the provider
        // had no inbound path at all.
        b.receive_enabled = true;
        assert!(b.validate_in(&[]).is_ok());
        b.target = "C123456".into();
        assert!(b.validate_in(&[]).is_err());
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
        assert!(b.validate_in(&[]).is_ok());
        b.credential_ref = Some("telegram".into());
        assert!(b.validate_in(&[]).is_ok());
        b.credential_ref = Some("slack".into());
        assert!(b.validate_in(&[]).is_err());
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
        let rejection = b.validate_in(&[]).unwrap_err();
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

    /// An overlay whose global this host no longer declares is DROPPED with a
    /// warning; every other binding in the file still loads.
    ///
    /// Why (#7609 review HIGH-4): one unresolvable record used to fail the
    /// whole read, so `GET`/`PUT /api/agents/{name}/channels` both 400'd and
    /// the assistant's OTHER destinations stopped claiming inbound events —
    /// an operator editing `config.toml` could take an assistant off every
    /// channel it had.
    ///
    /// Pre-fix (886d4afdc) this fails at the `unwrap`: `load_at_with`
    /// propagated `validate_in`'s 400 for the blank-target record.
    #[tokio::test]
    async fn an_unresolvable_overlay_is_dropped_not_a_whole_file_rejection() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join("fixture.toml"), "[agent]\nname='fixture'\n")
            .await
            .unwrap();
        let dirs = [dir.path().to_path_buf()];
        let stored = json!([
            {"id":"gmail-personal","name":"gmail-personal","provider":"gworkspace","target":"",
             "enabled":true,"receive_enabled":true},
            {"id":"team","name":"Team","provider":"slack","target":"C123456",
             "enabled":true,"send_enabled":true}
        ]);
        tokio::fs::write(dir.path().join("fixture.channels.json"), stored.to_string())
            .await
            .unwrap();

        // No global of that id: the overlay resolves to nothing.
        let (_, _, bindings) = load_at_with(&dirs, "fixture", &[]).await.unwrap();
        assert_eq!(
            bindings.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(),
            vec!["team"],
            "the unresolvable overlay is dropped, the rest of the file loads"
        );

        // With the global declared, the overlay is a legitimate record again.
        let globals = vec![crate::channels::Channel {
            id: "gmail-personal".into(),
            name: "Personal mail".into(),
            provider: "gmail".into(),
            scope: crate::channels::ChannelScope::Global,
            enabled: true,
            receive_enabled: true,
            ..crate::channels::Channel::default()
        }];
        let (_, _, bindings) = load_at_with(&dirs, "fixture", &globals).await.unwrap();
        assert_eq!(bindings.len(), 2);

        // A record that is invalid for any OTHER reason still fails the read,
        // so a genuinely broken file is not silently half-loaded.
        tokio::fs::write(
            dir.path().join("fixture.channels.json"),
            json!([{"id":"team","name":"Team","provider":"notion","target":"page","enabled":true}])
                .to_string(),
        )
        .await
        .unwrap();
        assert_eq!(
            load_at_with(&dirs, "fixture", &[]).await.unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
    }
}

// #7609: the byte-for-byte guarantee `Binding::event_types` rests on, in its
// own file because this one is at the 500-SLOC cap.
#[cfg(test)]
#[path = "agent_channels/round_trip_tests.rs"]
mod round_trip_tests;

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
