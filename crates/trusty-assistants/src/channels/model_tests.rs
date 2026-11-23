//! Coverage for the merged channel model (#7609 slice 1).

use super::*;
use crate::listeners::config::{AgentBindingFilter, AgentListenerBinding, ListenerConfig};

/// The live `~/.trusty-agents/config.toml` entry, reproduced as a fixture —
/// the real file is never read by this crate's tests.
fn live_global_listener() -> ListenerConfig {
    ListenerConfig {
        name: "gmail-personal".into(),
        connector: "gmail".into(),
        identity: Some("bob-personal".into()),
        transport: "history-poll".into(),
        enabled: true,
        poll_interval_secs: 60,
        filter: crate::listeners::config::ListenerFilter {
            label_ids: vec!["INBOX".into()],
        },
    }
}

/// The live `agents/izzie/agent.toml` binding, reproduced as a fixture.
fn live_agent_binding() -> AgentListenerBinding {
    AgentListenerBinding {
        name: "gmail-personal".into(),
        enabled: true,
        instructions: String::new(),
        event_types: vec!["message.received".into()],
        filter: AgentBindingFilter {
            from: vec!["*".into()],
            exclude_labels: vec!["CATEGORY_PROMOTIONS".into()],
            ..AgentBindingFilter::default()
        },
    }
}

#[test]
fn global_listener_becomes_a_global_scope_channel() {
    let channel = Channel::from(live_global_listener());
    assert_eq!(channel.scope, ChannelScope::Global);
    assert_eq!(channel.id, "gmail-personal");
    assert_eq!(channel.name, "gmail-personal");
    assert_eq!(channel.provider, "gmail");
    assert_eq!(channel.target, "", "a migrated listener is account-wide");
    assert_eq!(
        channel.credential_ref.as_deref(),
        Some("gmail/bob-personal")
    );
    assert_eq!(channel.poll_interval_secs, 60);
    assert_eq!(channel.ingest_filter.label_ids, vec!["INBOX".to_string()]);
    assert!(channel.route_to.is_empty(), "route_to defaults empty");
}

#[test]
fn global_listener_round_trips_through_the_channel_model() {
    let listener = live_global_listener();
    let projected = Channel::from(listener.clone()).to_listener_config();
    assert_eq!(projected, listener, "the derived view must be lossless");
}

#[test]
fn a_listener_without_an_identity_migrates_to_no_credential_ref() {
    let listener = ListenerConfig {
        identity: None,
        ..live_global_listener()
    };
    let channel = Channel::from(listener.clone());
    assert_eq!(channel.credential_ref, None);
    assert_eq!(channel.credential_qualifier(), None);
    assert_eq!(channel.to_listener_config(), listener);
}

#[test]
fn credential_qualifier_reads_the_identity_half() {
    let mut channel = Channel::from(live_global_listener());
    assert_eq!(channel.credential_qualifier(), Some("bob-personal"));
    channel.credential_ref = Some("gmail".into());
    assert_eq!(
        channel.credential_qualifier(),
        None,
        "a bare provider names no identity"
    );
    channel.credential_ref = Some("gmail/".into());
    assert_eq!(channel.credential_qualifier(), None);
}

#[test]
fn agent_binding_becomes_an_assistant_scope_channel() {
    let channel = Channel::from_agent_binding(live_agent_binding(), "gmail");
    assert_eq!(channel.scope, ChannelScope::Assistant);
    assert_eq!(channel.id, "gmail-personal");
    assert_eq!(channel.provider, "gmail");
    assert_eq!(channel.event_types, vec!["message.received".to_string()]);
    assert_eq!(channel.wake_filter.from, vec!["*".to_string()]);
    assert_eq!(
        channel.wake_filter.exclude_labels,
        vec!["CATEGORY_PROMOTIONS".to_string()]
    );
    assert!(
        channel.ingest_filter.label_ids.is_empty(),
        "a binding carries no stage-one filter"
    );
}

#[test]
fn agent_binding_round_trips_through_the_channel_model() {
    let binding = live_agent_binding();
    let projected = Channel::from_agent_binding(binding.clone(), "gmail").to_agent_binding();
    assert_eq!(projected, binding, "the derived view must be lossless");
}

#[test]
fn global_view_skips_assistant_scope_channels() {
    let channels = vec![
        Channel::from(live_global_listener()),
        Channel::from_agent_binding(live_agent_binding(), "gmail"),
    ];
    let globals = project(&channels, ChannelScope::Global, Channel::to_listener_config);
    assert_eq!(globals.len(), 1);
    assert_eq!(globals[0], live_global_listener());
    let assistants = project(
        &channels,
        ChannelScope::Assistant,
        Channel::to_agent_binding,
    );
    assert_eq!(assistants, vec![live_agent_binding()]);
}

#[test]
fn scope_is_never_written_into_stored_bytes() {
    let rendered = toml::to_string_pretty(&Channel::from(live_global_listener()))
        .expect("a channel renders as TOML");
    assert!(
        !rendered.contains("scope"),
        "the file's location implies the scope; rendered as:\n{rendered}"
    );
    assert_eq!(ChannelScope::Assistant.as_str(), "assistant");
}

#[test]
fn a_stored_channel_parses_with_only_its_required_keys() {
    let channel: Channel = toml::from_str("id = 'x'\nprovider = 'slack'\n").expect("minimal parse");
    assert_eq!(channel.transport, "history-poll");
    assert_eq!(channel.poll_interval_secs, 180);
    assert!(!channel.enabled);
    assert_eq!(
        channel.scope,
        ChannelScope::Global,
        "scope comes from the caller, not the bytes"
    );
}

#[test]
fn a_stored_channel_clamps_a_below_floor_poll_interval() {
    let channel: Channel = toml::from_str("id = 'x'\nprovider = 'slack'\npoll_interval_secs = 1\n")
        .expect("minimal parse");
    assert_eq!(
        channel.poll_interval_secs,
        crate::listeners::config::MIN_POLL_INTERVAL_SECS
    );
}

#[test]
fn addressing_is_keyed_on_provider_and_target() {
    let a = Channel {
        provider: "slack".into(),
        target: "D1".into(),
        ..Channel::default()
    };
    let b = Channel {
        id: "other".into(),
        ..a.clone()
    };
    assert!(a.addresses_same(&b), "id does not decide addressing");
    let c = Channel {
        target: "D2".into(),
        ..a.clone()
    };
    assert!(!a.addresses_same(&c));
}
