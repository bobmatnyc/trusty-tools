//! `GET`/`PUT /api/channels` — the harness-wide `[[channels]]` table (#7609
//! slice 5).
//!
//! Why: slices 1–4 gave global channels a model, a storage migration and an
//! inbound dispatch path, but no way to see or change one short of hand-editing
//! `~/.trusty-agents/config.toml`. This is that surface, and it is deliberately
//! the SAME shape as the per-assistant one — a revision, a complete list, a
//! compare-and-swap on write — so the UI (slice 6) has one interaction to
//! implement for both scopes.
//!
//! Precedence is NOT enforced here. A per-assistant binding wins over a global
//! one for the same `(provider, target)`, and that rule lives in
//! [`crate::channels::resolve_channels`], applied on the inbound path by
//! [`crate::channels::dispatch`]. This route only validates and stores: a
//! global channel an assistant happens to shadow is still a legitimate record,
//! and refusing it here would make the shadowing assistant's config unwritable
//! from the other direction.
//!
//! What: the write goes through [`crate::state_writer::atomic_update`] on
//! `config.toml`, taking the same `config.toml.lock` every other writer of that
//! file takes, and edits the document with `toml_edit` so unrelated tables,
//! their key order and their comments survive. The one thing it does NOT
//! preserve is a comment written INSIDE a `[[channels]]` table — the array is
//! replaced wholesale, because the list the client sends is authoritative and
//! there is no key-level correspondence to merge against. The deprecated
//! `[[listeners]]` table is dropped by the same write: leaving it would let
//! `GlobalConfig::absorb_legacy_listeners` re-add a channel the operator just
//! deleted.
//! Test: `crate::api::server::tests::global_channels` — the whole module.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use axum::{Json, http::StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::agent_stores::is_valid_agent_name;
use super::channel_auth::ChannelWriter;
use crate::channels::Channel;

type Error = (StatusCode, Json<Value>);

fn err(code: StatusCode, text: &str) -> Error {
    (code, Json(json!({ "error": text })))
}

fn bad(text: &str) -> Error {
    err(StatusCode::BAD_REQUEST, text)
}

/// The most channels one host may declare, matching the per-assistant cap.
const MAX_CHANNELS: usize = 32;

/// A complete replacement of the global list, guarded by the revision the
/// client read.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GlobalUpdate {
    revision: String,
    channels: Vec<Channel>,
}

/// The compare-and-swap token for the global list.
///
/// Why: hashing the CHANNELS rather than the whole file is deliberate. An
/// unrelated `[mcp]` edit — which any `mcp_*` tool can make — would otherwise
/// invalidate a revision the client is still entitled to write against, and the
/// thing the swap has to protect is the channel list alone.
/// What: SHA-256 over the canonical JSON encoding of the list, which is
/// deterministic for a given list because `Channel`'s field order is its
/// declaration order.
/// Test: `global_channels_round_trip_preserves_config_and_rejects_a_stale_revision`.
pub(super) fn revision(channels: &[Channel]) -> Result<String, Error> {
    // #7609 critic LOW: a serialization failure used to hash to a fixed
    // sentinel, which would have compared EQUAL to another failure and let a
    // compare-and-swap pass on a revision that described nothing.
    let encoded = serde_json::to_vec(channels).map_err(|e| {
        tracing::warn!(error = %e, "channel config: the channel list could not be encoded (#7609)");
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The channel list could not be encoded",
        )
    })?;
    Ok(format!("{:x}", Sha256::digest(&encoded)))
}

/// The global channel list as `config.toml` currently declares it.
///
/// Why: `GlobalConfig::from_toml_str` is the one parser that also absorbs the
/// deprecated `[[listeners]]` table, so reading through it is what makes a
/// legacy entry visible on this route without an operator migrating anything.
/// What: an absent file is an empty list; a file that will not parse is a 500
/// naming the problem, NEVER an empty list — reading a broken file as "no
/// channels declared" would let a write publish that emptiness.
/// Test: `a_malformed_config_is_reported_not_read_as_empty`.
pub(super) fn channels_in(raw: &str) -> Result<Vec<Channel>, Error> {
    crate::mcp::config::GlobalConfig::from_toml_str(raw)
        .map(|config| config.channels)
        .map_err(|e| {
            tracing::warn!(error = %e, "channel config: config.toml did not parse (#7609)");
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The global configuration could not be parsed; fix config.toml and reload",
            )
        })
}

async fn load(path: &Path) -> Result<Vec<Channel>, Error> {
    match tokio::fs::read_to_string(path).await {
        Ok(raw) => channels_in(&raw),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "channel config: config.toml could not be read (#7609)");
            Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The global configuration could not be read",
            ))
        }
    }
}

fn config_path() -> Result<PathBuf, Error> {
    crate::mcp::config::GlobalConfig::config_path().map_err(|e| {
        tracing::warn!(error = %e, "channel config: no global config path (#7609)");
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The global configuration path could not be resolved",
        )
    })
}

/// The payload both routes answer with.
fn view(channels: &[Channel]) -> Result<Value, Error> {
    Ok(json!({
        "scope": "global",
        "revision": revision(channels)?,
        "channels": channels,
        "providers": crate::channels::providers_json(),
    }))
}

/// Reject a global channel the harness could not act on.
///
/// Why: the per-assistant rules, with the two differences a global channel
/// genuinely has. First, its provider may be a CONNECTOR id rather than an
/// adapter id — a migrated Gmail listener declares `gmail`, which addresses
/// events through the `gworkspace` adapter — so the provider is resolved
/// through [`crate::channels::adapter_id`] exactly as dispatch resolves it.
/// Second, an empty target is legitimate and means account-wide, which is what
/// every migrated listener carries; a channel with no destination may still
/// receive, but it cannot SEND, because there is nowhere to send to.
///
/// The credential reference is checked against the adapter's provider family
/// only when that adapter HAS one. `gworkspace` declares none (it authenticates
/// with its provider account), yet a migrated listener's `identity` is stored
/// as `gmail/<identity>` and read back by `Channel::credential_qualifier` — so
/// confining it to the adapter's empty family would refuse every migrated
/// mailbox. For those providers the reference is grammar-checked and nothing
/// more.
/// What: identity limits, provider, destination, permissions, credential
/// reference, the shared filter rules, and `route_to` naming assistants this
/// host actually has.
/// Test: `global_channel_validation_covers_provider_target_and_routes`.
pub(super) fn validate(channel: &Channel, known_assistants: &[String]) -> Result<(), Error> {
    if !is_valid_agent_name(&channel.id)
        || channel.name.trim().is_empty()
        || channel.name.chars().count() > 128
    {
        return Err(bad(
            "Channel ID and name are required (maximum 128 characters)",
        ));
    }
    let Some(adapter) = crate::channels::adapter(crate::channels::adapter_id(&channel.provider))
    else {
        return Err(bad("Unsupported channel provider"));
    };
    if channel.target.is_empty() {
        if channel.send_enabled {
            return Err(bad(
                "A global channel with no destination cannot send; give it a destination or turn sending off",
            ));
        }
    } else if !adapter.validate_target(&channel.target) {
        return Err(bad(
            "Choose a Slack channel ID, a Telegram chat ID, or a Gmail from:<address> or label:<label> for the selected provider",
        ));
    }
    if channel.receive_enabled && !adapter.capabilities().can_receive {
        return Err(bad(&format!(
            "{} incoming updates are not available through this channel integration",
            adapter.display_name()
        )));
    }
    if let Some(reference) = &channel.credential_ref {
        validate_credential(reference, adapter)?;
    }
    // #7609 critic MEDIUM-4: `ingest_filter.label_ids` is passed to Gmail's
    // `history.list`, and `transport` selects the poller. Neither was bounded,
    // so the same 32 x 256 non-control rule `AgentListenerBinding::validate`
    // applies to every other filter list applies here too.
    if channel.ingest_filter.label_ids.len() > 32
        || channel.ingest_filter.label_ids.iter().any(|label| {
            label.trim().is_empty()
                || label.chars().count() > 256
                || label.chars().any(char::is_control)
        })
    {
        return Err(bad(
            "Ingest label filters allow at most 32 nonblank values of at most 256 characters without control characters",
        ));
    }
    // The poller branches on no other value, so anything else is a
    // configuration that silently never polls.
    if channel.transport != crate::listeners::config::default_transport() {
        return Err(bad(
            "Unsupported channel transport; this build polls Gmail history only",
        ));
    }
    for name in &channel.route_to {
        if !known_assistants.iter().any(|known| known == name) {
            return Err(bad(&format!(
                "route_to names `{name}`, which is not an assistant on this host"
            )));
        }
    }
    crate::listeners::config::AgentListenerBinding::from(channel)
        .validate()
        .map_err(|e| bad(&e))
}

/// The credential half of [`validate`], split out so its two branches are
/// legible.
fn validate_credential(
    reference: &str,
    adapter: &'static dyn crate::channels::ChannelAdapter,
) -> Result<(), Error> {
    if adapter.credential_providers().is_empty() {
        // #7609: a connector-scoped identity (`gmail/bob-personal`), not a
        // channel send credential — grammar only, no family to confine it to.
        return trusty_common::credentials::CredentialRef::parse(reference)
            .map(|_| ())
            .map_err(|e| bad(&format!("Channel credential reference: {e}")));
    }
    crate::channels::validate_credential_ref(
        reference,
        adapter.credential_providers(),
        adapter.credential_env_prefix(),
    )
    .map_err(|e| bad(&format!("Channel credential reference: {e}")))
}

/// [`validate`] over the whole list, plus the list-level rules.
///
/// Test: `global_channel_validation_covers_provider_target_and_routes`.
pub(super) fn validate_all(channels: &[Channel], known_assistants: &[String]) -> Result<(), Error> {
    if channels.len() > MAX_CHANNELS {
        return Err(bad(&format!(
            "At most {MAX_CHANNELS} global channels are supported"
        )));
    }
    let mut ids = std::collections::HashSet::new();
    for channel in channels {
        validate(channel, known_assistants)?;
        if !ids.insert(&channel.id) {
            return Err(bad("Channel IDs must be unique"));
        }
    }
    Ok(())
}

/// What one [`persist`] call did.
#[derive(Debug)]
pub(super) enum Persisted {
    /// The list was replaced; the counts either side of the write.
    Written { before: usize, after: usize },
    /// The client's revision no longer described the stored list.
    Conflict,
}

/// Publish `channels` into `path`'s `[[channels]]`, under the file's own lock.
///
/// Why: `config.toml` has other writers — `GlobalConfig::save`, the startup
/// drain, the `mcp_*` tools — so the compare-and-swap is only real if the read
/// it compares against and the write it authorizes happen inside one held lock.
/// What: [`crate::state_writer::atomic_update`] holds `config.toml.lock` across
/// read, decide and publish. The decision is taken again from the LOCKED bytes,
/// so a change that lands between the client's `GET` and this call is a
/// [`Persisted::Conflict`], never an overwrite. Editing through `toml_edit`
/// preserves unrelated tables and comments; see the module doc for the one
/// thing it does not preserve.
///
/// Blocking: the caller runs this on a blocking thread.
/// Test: `global_channels_round_trip_preserves_config_and_rejects_a_stale_revision`,
/// `a_malformed_config_is_reported_not_read_as_empty`.
pub(super) fn persist(
    path: &Path,
    expected_revision: &str,
    channels: &[Channel],
) -> anyhow::Result<Persisted> {
    let rendered = render(channels)?;
    let mut outcome = Persisted::Conflict;
    crate::state_writer::atomic_update(path, |existing| {
        let current_raw = match existing {
            Some(bytes) => String::from_utf8(bytes.to_vec())
                .context("config.toml is not valid UTF-8; nothing was written")?,
            None => String::new(),
        };
        let current = crate::mcp::config::GlobalConfig::from_toml_str(&current_raw)
            .context("config.toml did not parse; nothing was written")?
            .channels;
        let stored_revision = revision(&current)
            .map_err(|_| anyhow::anyhow!("the stored channel list could not be encoded"))?;
        if stored_revision != expected_revision {
            return Ok(None);
        }
        let mut document = current_raw
            .parse::<toml_edit::DocumentMut>()
            .context("config.toml is not an editable document; nothing was written")?;
        match &rendered {
            Some(item) => document["channels"] = item.clone(),
            None => {
                document.remove("channels");
            }
        }
        // #7609: the write publishes the canonical table, so the deprecated
        // spelling goes with it — left behind, `absorb_legacy_listeners` would
        // re-add a channel this write just deleted.
        document.remove("listeners");
        outcome = Persisted::Written {
            before: current.len(),
            after: channels.len(),
        };
        Ok(Some(document.to_string().into_bytes()))
    })?;
    Ok(outcome)
}

/// `channels` as the `[[channels]]` item to splice in, or `None` to remove the
/// key entirely because the list is empty.
fn render(channels: &[Channel]) -> anyhow::Result<Option<toml_edit::Item>> {
    if channels.is_empty() {
        return Ok(None);
    }
    #[derive(serde::Serialize)]
    struct Document<'a> {
        channels: &'a [Channel],
    }
    let rendered =
        toml::to_string_pretty(&Document { channels }).context("encoding [[channels]]")?;
    let document = rendered
        .parse::<toml_edit::DocumentMut>()
        .context("re-reading the encoded [[channels]]")?;
    document
        .get("channels")
        .cloned()
        .map(Some)
        .context("the encoded document declared no [[channels]]")
}

/// Every assistant name this host can route to.
async fn known_assistants() -> Result<Vec<String>, Error> {
    crate::listeners::wake::candidate_agent_names()
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "channel config: the assistant roster could not be read; refusing the write (#7609)");
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The assistant roster could not be read; the write was refused",
            )
        })
}

/// `GET /api/channels` — the harness-wide channel list.
///
/// Why: the read half of the global surface, and the only way a client learns
/// the revision a write must carry.
/// What: the absorbed list (so a deprecated `[[listeners]]` entry shows up as
/// the channel it is), its revision, and the provider capability table the
/// per-assistant view already publishes. Open to any caller the router admits —
/// the write gate does not apply to reads.
/// Test: `global_channels_round_trip_preserves_config_and_rejects_a_stale_revision`.
pub(super) async fn get_route() -> Result<Json<Value>, Error> {
    let channels = load(&config_path()?).await?;
    view(&channels).map(Json)
}

/// `PUT /api/channels` — replace the harness-wide channel list.
///
/// Why: see the module doc. [`ChannelWriter`] is the first argument because
/// extracting it IS the authorization gate (#7609) — a tokenless daemon never
/// reaches this body.
/// What: validates the whole list, then swaps it in against `revision`. A stale
/// revision is 409 and writes nothing. Answers with the stored list, so a
/// client can continue from the new revision without a second round trip.
/// Test: `global_channels_round_trip_preserves_config_and_rejects_a_stale_revision`,
/// `a_tokenless_daemon_refuses_every_channel_write`,
/// `a_token_bearing_channel_write_is_admitted_and_audited`.
pub(super) async fn put_route(
    writer: ChannelWriter,
    Json(update): Json<GlobalUpdate>,
) -> Result<Json<Value>, Error> {
    let (stored, before, after) = apply(update).await?;
    writer.audit("PUT /api/channels", "global", None, Some(before), after);
    Ok(Json(stored))
}

/// Replace the global list from a MODEL TURN, under the same gate.
///
/// Why (#7609): the merged `channel` tool's `set` action writes the same table
/// this route writes, and has no request to extract a [`ChannelWriter`] from.
/// See `super::agent_channels::write_from_turn` for the per-assistant twin.
/// What: 401 when this daemon serves no authenticated API; otherwise the same
/// validate → compare-and-swap → audit sequence.
/// Test: `crate::tools::channel::channel_tests::the_tool_refuses_a_write_on_a_tokenless_daemon`.
pub(crate) async fn write_from_turn(update: GlobalUpdate) -> Result<Value, Error> {
    if !super::channel_auth::daemon_token_configured() {
        return Err(err(
            StatusCode::UNAUTHORIZED,
            &super::channel_auth::tool_refusal(),
        ));
    }
    let (stored, before, after) = apply(update).await?;
    super::channel_auth::audit_write(
        "turn:channels",
        "global",
        None,
        Some(before),
        after,
        "in-process",
    );
    Ok(stored)
}

/// The read half, for a caller with no request of its own.
pub(crate) async fn read_view() -> Result<Value, Error> {
    let channels = load(&config_path()?).await?;
    view(&channels)
}

/// Validate, swap and answer — everything both write entry points share.
///
/// What: `(stored view, count before, count after)`. A stale revision is a 409
/// and writes nothing; an unreadable roster, an unparseable file and a failed
/// publish are each a 500 that writes nothing.
async fn apply(update: GlobalUpdate) -> Result<(Value, usize, usize), Error> {
    let known = known_assistants().await?;
    validate_all(&update.channels, &known)?;
    let path = config_path()?;
    let outcome = tokio::task::spawn_blocking(move || {
        persist(&path, &update.revision, &update.channels).map(|outcome| (outcome, update.channels))
    })
    .await
    .map_err(|e| {
        tracing::warn!(error = %e, "channel config: the global write task failed (#7609)");
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The global configuration could not be saved",
        )
    })?;
    let (outcome, channels) = outcome.map_err(|e| {
        tracing::warn!(error = %e, "channel config: the global write failed (#7609)");
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The global configuration could not be saved",
        )
    })?;
    match outcome {
        Persisted::Conflict => Err(err(
            StatusCode::CONFLICT,
            "Channel settings changed. Reload before saving.",
        )),
        Persisted::Written { before, after } => Ok((view(&channels)?, before, after)),
    }
}
