//! The one channel type listeners and channel bindings both collapse into
//! (#7609, slice 1).
//!
//! Why: the same idea — "this assistant talks to that place" — was modelled
//! three times. `ListenerConfig` described a harness-level inbound poller,
//! `AgentListenerBinding` described which of its events wake one assistant,
//! and `Binding` described a two-way channel an assistant sends on. An
//! operator had to learn three shapes, two files and two vocabularies for one
//! concept, and the owner's 2026-09-12 ruling is that channels are ONE thing.
//! [`Channel`] is that thing; the three legacy shapes become projections of
//! it, so nothing downstream has to change in the same release.
//! What: [`Channel`] carries the union of the three shapes. [`ChannelScope`]
//! records where it was stored — `Global` for `~/.trusty-agents/config.toml`'s
//! `[[channels]]`, `Assistant` for a per-assistant `*.channels.json` — and is
//! derived from the file it was read out of rather than written into it (a
//! stored `scope` could disagree with its own location). The `From`/`to_*`
//! pairs are lossless for every field the legacy shape declared, which is what
//! lets `GlobalConfig::listeners()` and `AgentConfig::listeners()` keep
//! answering exactly what the removed fields held.
//! Test: `crate::channels::model_tests` — the whole module.

// #7609: slice 1, the merged data model. Dispatch (slice 4), routes/tools
// (slice 5) and the UI (slice 6) still read the projections below.
use crate::listeners::config::{
    AgentBindingFilter, AgentListenerBinding, ListenerConfig, ListenerFilter,
    default_poll_interval_secs, default_transport, deserialize_clamped_poll_interval,
};
use serde::{Deserialize, Serialize};

/// Where a channel is stored, and therefore whose channel it is.
///
/// Why: precedence ([`super::resolve_channels`]) and the derived legacy views
/// both need to tell a harness-wide channel from one assistant's own, and the
/// answer is the file it came from. Deriving it on load keeps a hand-edited
/// `scope` key from claiming something the file's location contradicts.
/// Test: `global_listener_becomes_a_global_scope_channel`,
/// `agent_binding_becomes_an_assistant_scope_channel`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ChannelScope {
    /// Declared in `~/.trusty-agents/config.toml` as `[[channels]]`.
    #[default]
    Global,
    /// Declared in one assistant's `<name>.channels.json` / `agent.channels.json`.
    Assistant,
}

impl ChannelScope {
    /// The wire spelling any API view uses for this scope.
    ///
    /// Why: [`Channel::scope`] is deliberately absent from stored bytes, so the
    /// JSON views slice 5 adds have to name it explicitly rather than lean on
    /// the derive.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Assistant => "assistant",
        }
    }
}

/// One channel: a provider, a destination, and the rules for traffic each way.
///
/// Why: see the module doc. The field set is the union of the three legacy
/// shapes, so a migration never has to drop something it cannot express.
/// What: `id` is the per-file storage key (a migrated global listener keeps
/// its listener `name` as `id`). `target` is the provider destination; the
/// empty string means account-wide, which is what a migrated global listener
/// gets — precedence keys on `(provider, target)`, so an empty target can only
/// collide with another empty target. `ingest_filter` is stage one (what is
/// fetched from the provider at all) and `wake_filter` is stage two (which of
/// those events wake an assistant); neither filter type changed shape.
/// `route_to` names the assistants a Global channel fans out to — empty by
/// default, and read by slice 4, not here (owner ruling 2026-09-14, option A).
/// `scope` is `#[serde(skip)]`: the file the channel lives in already says it.
/// `transport` stays because `crate::listeners::poll` still reads it; slice 7
/// may remove it once the poller takes its transport from the provider.
/// Test: `crate::channels::model_tests` — the whole module.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Channel {
    /// Stable per-file key, referenced by bindings and by the API.
    pub id: String,
    /// Display name; equals `id` for anything migrated out of `[[listeners]]`.
    #[serde(default)]
    pub name: String,
    /// Provider id (`gmail`, `slack`, `telegram`, `gworkspace`, …).
    pub provider: String,
    /// Set from the file this channel was read out of; never stored.
    #[serde(skip)]
    pub scope: ChannelScope,
    /// Provider destination; empty means account-wide.
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub send_enabled: bool,
    #[serde(default)]
    pub receive_enabled: bool,
    /// Connector plumbing `crate::listeners::poll` reads; see the type doc.
    #[serde(default = "default_transport")]
    pub transport: String,
    /// Floored at `crate::listeners::config::MIN_POLL_INTERVAL_SECS` on parse,
    /// by the same deserializer `ListenerConfig` uses.
    #[serde(
        default = "default_poll_interval_secs",
        deserialize_with = "deserialize_clamped_poll_interval"
    )]
    pub poll_interval_secs: u64,
    /// `trusty_common::credentials::CredentialRef` text — `provider` or
    /// `provider/qualifier`, never a credential value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
    #[serde(default)]
    pub instructions: String,
    /// Normalized event types that may wake a bound assistant; empty = any.
    #[serde(default)]
    pub event_types: Vec<String>,
    /// Assistants a Global channel routes to; empty by default (slice 4).
    #[serde(default)]
    pub route_to: Vec<String>,
    /// Stage one: what is fetched from the provider at all.
    #[serde(default)]
    pub ingest_filter: ListenerFilter,
    /// Stage two: which fetched events wake an assistant.
    #[serde(default)]
    pub wake_filter: AgentBindingFilter,
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            provider: String::new(),
            scope: ChannelScope::Global,
            target: String::new(),
            enabled: false,
            send_enabled: false,
            receive_enabled: false,
            transport: default_transport(),
            poll_interval_secs: default_poll_interval_secs(),
            credential_ref: None,
            instructions: String::new(),
            event_types: Vec::new(),
            route_to: Vec::new(),
            ingest_filter: ListenerFilter::default(),
            wake_filter: AgentBindingFilter::default(),
        }
    }
}

impl Channel {
    /// The qualifier half of `credential_ref`, which is the legacy
    /// `ListenerConfig::identity`.
    ///
    /// Why: a migrated global listener's `identity = "bob-personal"` becomes
    /// `credential_ref = "gmail/bob-personal"` so it parses under the existing
    /// `CredentialRef` grammar. The poller still wants the bare profile name,
    /// and one accessor here keeps it from re-implementing that split.
    /// What: `Some(qualifier)` for `provider/qualifier`; `None` for a bare
    /// `provider` or no reference at all.
    /// Test: `credential_qualifier_reads_the_identity_half`.
    pub fn credential_qualifier(&self) -> Option<&str> {
        self.credential_ref
            .as_deref()
            .and_then(|r| r.split_once('/'))
            .map(|(_, qualifier)| qualifier)
            .filter(|qualifier| !qualifier.is_empty())
    }

    /// Whether this channel and `other` address the same place.
    ///
    /// Why: precedence is keyed on `(provider, target)` — see
    /// [`super::resolve_channels`] — and both the resolver and the per-assistant
    /// migration ask this question.
    pub fn addresses_same(&self, other: &Self) -> bool {
        self.provider == other.provider && self.target == other.target
    }

    /// One `config.toml` `[[listeners]]` entry as a Global channel.
    ///
    /// What: `id` and `name` both take the listener `name` (its stable key);
    /// `provider` takes `connector`; `target` is empty (account-wide);
    /// `identity` becomes `credential_ref = "<provider>/<identity>"`, or `None`
    /// when the listener named no identity. `receive_enabled` mirrors
    /// `enabled` because a listener is inbound by definition, and
    /// `send_enabled` is false because a listener never sent anything. Neither
    /// is read back by [`Channel::to_listener_config`], so the projection is
    /// lossless for every field the listener actually declared.
    /// Test: `global_listener_becomes_a_global_scope_channel`,
    /// `global_listener_round_trips_through_the_channel_model`.
    pub fn from_listener(listener: ListenerConfig) -> Self {
        let credential_ref = listener
            .identity
            .as_deref()
            .filter(|identity| !identity.is_empty())
            .map(|identity| format!("{}/{identity}", listener.connector));
        Self {
            id: listener.name.clone(),
            name: listener.name,
            provider: listener.connector,
            scope: ChannelScope::Global,
            enabled: listener.enabled,
            receive_enabled: listener.enabled,
            transport: listener.transport,
            poll_interval_secs: listener.poll_interval_secs,
            credential_ref,
            ingest_filter: listener.filter,
            ..Self::default()
        }
    }

    /// This channel as the `[[listeners]]` entry it projects back to.
    ///
    /// Why: `GlobalConfig::listeners()` answers with these, so
    /// `crate::listeners::poll`, `knowledge_pipeline` and the listener API see
    /// exactly what the removed `listeners` field held.
    /// Test: `global_listener_round_trips_through_the_channel_model`.
    pub fn to_listener_config(&self) -> ListenerConfig {
        ListenerConfig {
            name: self.id.clone(),
            connector: self.provider.clone(),
            identity: self.credential_qualifier().map(str::to_string),
            transport: self.transport.clone(),
            enabled: self.enabled,
            poll_interval_secs: self.poll_interval_secs,
            filter: self.ingest_filter.clone(),
        }
    }

    /// One `agent.toml` `[[listeners]]` binding as an Assistant channel.
    ///
    /// Why: `AgentListenerBinding` names a global listener but records no
    /// provider of its own, so the caller supplies the provider from the
    /// global channel it binds to — `""` when the binding names one this host
    /// has not configured, which `AgentConfig::from_toml_str` cannot resolve
    /// because it holds no global config.
    /// What: the binding's `filter` is stage two, so it lands in `wake_filter`;
    /// `target` stays empty because the binding never named a destination.
    /// Test: `agent_binding_becomes_an_assistant_scope_channel`,
    /// `agent_binding_round_trips_through_the_channel_model`.
    pub fn from_agent_binding(binding: AgentListenerBinding, provider: &str) -> Self {
        Self {
            id: binding.name.clone(),
            name: binding.name,
            provider: provider.to_string(),
            scope: ChannelScope::Assistant,
            enabled: binding.enabled,
            receive_enabled: binding.enabled,
            instructions: binding.instructions,
            event_types: binding.event_types,
            wake_filter: binding.filter,
            ..Self::default()
        }
    }

    /// This channel as the per-agent binding it projects back to.
    ///
    /// Test: `agent_binding_round_trips_through_the_channel_model`.
    pub fn to_agent_binding(&self) -> AgentListenerBinding {
        AgentListenerBinding {
            name: self.id.clone(),
            enabled: self.enabled,
            instructions: self.instructions.clone(),
            event_types: self.event_types.clone(),
            filter: self.wake_filter.clone(),
        }
    }
}

impl From<ListenerConfig> for Channel {
    fn from(listener: ListenerConfig) -> Self {
        Self::from_listener(listener)
    }
}

impl From<&Channel> for ListenerConfig {
    fn from(channel: &Channel) -> Self {
        channel.to_listener_config()
    }
}

impl From<&Channel> for AgentListenerBinding {
    fn from(channel: &Channel) -> Self {
        channel.to_agent_binding()
    }
}

/// Project a channel list back to the legacy listener shape, one scope only.
///
/// Why: `GlobalConfig::listeners()` and `AgentConfig::listeners()` are the two
/// derived views #7609 promises existing consumers, and both are "filter by
/// scope, then map" — worth one helper rather than two near-identical bodies.
/// Test: `global_view_skips_assistant_scope_channels`.
pub fn project<T>(channels: &[Channel], scope: ChannelScope, map: fn(&Channel) -> T) -> Vec<T> {
    channels
        .iter()
        .filter(|channel| channel.scope == scope)
        .map(map)
        .collect()
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod model_tests;
