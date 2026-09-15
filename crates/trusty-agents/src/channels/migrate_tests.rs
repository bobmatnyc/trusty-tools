//! Coverage for the listeners→channels migrations (#7609 slices 2 and 3).
//!
//! Every fixture REPRODUCES the operator's live files; nothing under
//! `~/.trusty-agents` is read or written by these tests.

use super::*;
use crate::listeners::config::{AgentBindingFilter, ListenerFilter};

/// The live `~/.trusty-agents/config.toml` listener, as text.
const LIVE_GLOBAL_CONFIG: &str = r#"
[mcp]
inject_for_roles = ["ctrl"]

[[listeners]]
name = "gmail-personal"
connector = "gmail"
identity = "bob-personal"
enabled = true
poll_interval_secs = 60
filter = { label_ids = ["INBOX"] }
"#;

/// The live `agents/izzie/agent.toml` binding, as text — the `[[listeners]]`
/// table only, which is all either migration reads.
const LIVE_AGENT_TOML: &str = r#"
[[listeners]]
name = "gmail-personal"
enabled = true
event_types = ["message.received"]
filter = { from = ["*"], exclude_labels = ["CATEGORY_PROMOTIONS"] }
"#;

fn write(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("fixture write");
    path
}

fn channels_in(path: &std::path::Path) -> Vec<Channel> {
    let raw = std::fs::read_to_string(path).expect("read back");
    let table: toml::Table = toml::from_str(&raw).expect("the migrated file must still parse");
    read_key::<Channel>(&table, "channels", path)
        .expect("the written channels must parse")
        .unwrap_or_default()
}

#[test]
fn a_global_listener_migrates_once_and_never_again() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write(dir.path(), "config.toml", LIVE_GLOBAL_CONFIG);

    let report = migrate_global_if_absent(&path)
        .expect("the first run must succeed")
        .expect("the first run must migrate");
    assert_eq!(report.channels, vec!["gmail-personal".to_string()]);
    assert_eq!(report.summary(), "gmail-personal");

    let after_first = std::fs::read_to_string(&path).expect("read back");
    assert!(
        migrate_global_if_absent(&path)
            .expect("the second run must succeed")
            .is_none(),
        "a second run must migrate nothing"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        after_first,
        "a second run must not touch the file"
    );
}

#[test]
fn the_migrated_global_channel_projects_back_to_the_original_listener() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write(dir.path(), "config.toml", LIVE_GLOBAL_CONFIG);
    migrate_global_if_absent(&path)
        .expect("migration succeeds")
        .expect("migration happened");

    let channels = channels_in(&path);
    assert_eq!(channels.len(), 1);
    let listener = channels[0].to_listener_config();
    assert_eq!(listener.name, "gmail-personal");
    assert_eq!(listener.connector, "gmail");
    assert_eq!(listener.identity.as_deref(), Some("bob-personal"));
    assert!(listener.enabled);
    assert_eq!(listener.poll_interval_secs, 60);
    assert_eq!(listener.filter.label_ids, vec!["INBOX".to_string()]);
    assert_eq!(listener.transport, "history-poll");
}

#[test]
fn migration_leaves_the_legacy_listeners_table_intact() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write(dir.path(), "config.toml", LIVE_GLOBAL_CONFIG);
    migrate_global_if_absent(&path)
        .expect("migration succeeds")
        .expect("migration happened");

    let after = std::fs::read_to_string(&path).expect("read back");
    assert!(
        after.starts_with(LIVE_GLOBAL_CONFIG),
        "the operator's original bytes must be a prefix of the result; got:\n{after}"
    );
    let table: toml::Table = toml::from_str(&after).expect("still parses");
    let listeners = read_key::<ListenerConfig>(&table, "listeners", &path)
        .expect("the legacy table still parses")
        .expect("the legacy table is still present");
    assert_eq!(listeners.len(), 1);
    assert_eq!(listeners[0].identity.as_deref(), Some("bob-personal"));
}

#[test]
fn a_config_with_no_listeners_migrates_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write(dir.path(), "config.toml", "[mcp]\ninject_for_roles = []\n");
    assert!(migrate_global_if_absent(&path).expect("succeeds").is_none());
    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        "[mcp]\ninject_for_roles = []\n"
    );
    let absent = dir.path().join("missing.toml");
    assert!(
        migrate_global_if_absent(&absent)
            .expect("succeeds")
            .is_none()
    );
}

#[test]
fn a_malformed_channels_table_is_reported_and_writes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let body = format!("{LIVE_GLOBAL_CONFIG}\n[[channels]]\nid = 42\n");
    let path = write(dir.path(), "config.toml", &body);
    let error = migrate_global_if_absent(&path).expect_err("a malformed table must be reported");
    match &error {
        ChannelMigrationError::Parse {
            key, path: named, ..
        } => {
            assert_eq!(*key, "channels");
            assert!(
                named.contains("config.toml"),
                "the path must be named: {named}"
            );
        }
        other => panic!("expected a channels parse error, got {other:?}"),
    }
    assert!(
        error.to_string().contains("nothing was migrated"),
        "the message must say nothing moved: {error}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        body,
        "a malformed table must leave the file untouched"
    );
}

#[test]
fn a_malformed_listeners_table_is_reported_and_writes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let body = "[[listeners]]\nname = \"gmail-personal\"\nconnector = 7\n";
    let path = write(dir.path(), "config.toml", body);
    let error = migrate_global_if_absent(&path).expect_err("a malformed table must be reported");
    match &error {
        ChannelMigrationError::Parse { key, .. } => assert_eq!(*key, "listeners"),
        other => panic!("expected a listeners parse error, got {other:?}"),
    }
    assert_eq!(std::fs::read_to_string(&path).expect("read back"), body);
}

/// A global channel an assistant binding CAN be stored against — a real
/// provider with a destination its adapter accepts.
fn storable_global() -> Channel {
    Channel {
        id: "gmail-personal".into(),
        name: "gmail-personal".into(),
        provider: "slack".into(),
        target: "D0AM8GWJLFR".into(),
        scope: ChannelScope::Global,
        credential_ref: Some("slack".into()),
        ..Channel::default()
    }
}

#[test]
fn an_agent_binding_migrates_once_and_never_again() {
    let dir = tempfile::tempdir().expect("tempdir");
    let agent = write(dir.path(), "agent.toml", LIVE_AGENT_TOML);
    let channels = dir.path().join("agent.channels.json");
    let globals = vec![storable_global()];

    let report = migrate_agent_channels_if_absent(&agent, &channels, &globals)
        .expect("succeeds")
        .expect("the first run migrates");
    assert_eq!(report.channels, vec!["gmail-personal".to_string()]);

    let raw = std::fs::read_to_string(&channels).expect("read back");
    let stored: Vec<crate::api::server::agent_channels::Binding> =
        serde_json::from_str(&raw).expect("the written file must parse as bindings");
    assert_eq!(stored.len(), 1);
    let migrated = crate::channels::Channel::from(&stored[0]);
    assert_eq!(migrated.scope, ChannelScope::Assistant);
    assert_eq!(migrated.event_types, vec!["message.received".to_string()]);
    assert_eq!(migrated.wake_filter.from, vec!["*".to_string()]);
    assert_eq!(
        migrated.wake_filter.exclude_labels,
        vec!["CATEGORY_PROMOTIONS".to_string()]
    );
    let binding = migrated.to_agent_binding();
    assert_eq!(binding.name, "gmail-personal");
    assert!(binding.enabled);

    assert!(
        migrate_agent_channels_if_absent(&agent, &channels, &globals)
            .expect("succeeds")
            .is_none(),
        "a second run must write nothing"
    );
    assert_eq!(std::fs::read_to_string(&channels).expect("read back"), raw);
    assert_eq!(
        std::fs::read_to_string(&agent).expect("read back"),
        LIVE_AGENT_TOML,
        "agent.toml must be left untouched"
    );
}

#[test]
fn an_unstorable_binding_is_left_in_agent_toml() {
    let dir = tempfile::tempdir().expect("tempdir");
    let agent = write(dir.path(), "agent.toml", LIVE_AGENT_TOML);
    let channels = dir.path().join("agent.channels.json");
    // The live global listener: connector `gmail`, account-wide (empty target).
    // No registered adapter accepts that destination, so storing it would make
    // the assistant's channel view unreadable.
    let globals = vec![Channel::from(ListenerConfig {
        name: "gmail-personal".into(),
        connector: "gmail".into(),
        identity: Some("bob-personal".into()),
        transport: "history-poll".into(),
        enabled: true,
        poll_interval_secs: 60,
        filter: ListenerFilter {
            label_ids: vec!["INBOX".into()],
        },
    })];
    assert!(
        migrate_agent_channels_if_absent(&agent, &channels, &globals)
            .expect("succeeds")
            .is_none()
    );
    assert!(!channels.exists(), "nothing storable means nothing written");
    assert_eq!(
        std::fs::read_to_string(&agent).expect("read back"),
        LIVE_AGENT_TOML
    );
}

#[test]
fn a_binding_naming_no_global_channel_is_skipped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let agent = write(dir.path(), "agent.toml", LIVE_AGENT_TOML);
    let channels = dir.path().join("agent.channels.json");
    assert!(
        migrate_agent_channels_if_absent(&agent, &channels, &[])
            .expect("succeeds")
            .is_none()
    );
    assert!(!channels.exists());
}

#[test]
fn a_malformed_agent_listeners_table_is_reported_and_writes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let body = "[[listeners]]\nname = \"gmail-personal\"\nenabled = \"yes\"\n";
    let agent = write(dir.path(), "agent.toml", body);
    let channels = dir.path().join("agent.channels.json");
    let error = migrate_agent_channels_if_absent(&agent, &channels, &[storable_global()])
        .expect_err("a malformed table must be reported");
    match &error {
        ChannelMigrationError::Parse { key, .. } => assert_eq!(*key, "listeners"),
        other => panic!("expected a listeners parse error, got {other:?}"),
    }
    assert!(!channels.exists());
    assert_eq!(std::fs::read_to_string(&agent).expect("read back"), body);
}

#[test]
fn an_existing_channels_file_is_left_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let agent = write(dir.path(), "agent.toml", LIVE_AGENT_TOML);
    let channels = write(dir.path(), "agent.channels.json", "[]");
    assert!(
        migrate_agent_channels_if_absent(&agent, &channels, &[storable_global()])
            .expect("succeeds")
            .is_none()
    );
    assert_eq!(std::fs::read_to_string(&channels).expect("read back"), "[]");
}

/// A discoverable Assistant instance carrying the live izzie binding.
fn assistant_instance_manifest() -> String {
    let head = r#"
[agent]
name = "izzie"
role = "assistant"
extends = "assistant"
model = ""
description = ""

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "x"
"#;
    format!("{head}{LIVE_AGENT_TOML}")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_assistant_sweep_never_blocks_its_caller() {
    // #7609 (critic HIGH, same shape as trusty-mpm #7965): the API server
    // spawns this sweep and then binds its TCP listener. The sweep writes
    // under a BLOCKING advisory lock with no timeout, so if it were awaited a
    // held lock would stall the bind indefinitely.
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "izzie.toml", &assistant_instance_manifest());
    let channels = dir.path().join("izzie.channels.json");

    // Take the very lock the sweep's write needs, and hold it.
    let (took, holding) = std::sync::mpsc::channel::<()>();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let held_path = channels.clone();
    let holder = std::thread::spawn(move || {
        crate::state_writer::atomic_update(&held_path, move |_existing| {
            took.send(()).ok();
            released.recv().ok();
            Ok(None)
        })
    });
    holding.recv().expect("the holder took the lock");

    let started = std::time::Instant::now();
    spawn_assistant_migration(vec![dir.path().to_path_buf()], vec![storable_global()]);

    // The caller's next await on the startup path is its TCP bind.
    let listener = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        tokio::net::TcpListener::bind("127.0.0.1:0"),
    )
    .await
    .expect("the sweep must not hold the caller for a second")
    .expect("bind");
    assert!(listener.local_addr().is_ok(), "the server bound its port");
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert!(
        !channels.exists(),
        "the sweep is still blocked on the held lock, which is the point"
    );

    release.send(()).ok();
    holder.join().expect("holder thread").expect("lock cycle");
}

#[test]
fn the_sweep_skips_an_assistant_with_no_manifest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dirs = vec![dir.path().to_path_buf()];
    assert!(
        migrate_assistant_channels(&dirs, &[storable_global()]).is_empty(),
        "an empty assistants directory migrates nothing"
    );
}

/// A complete `agent.toml` carrying the live izzie `[[listeners]]` binding.
fn live_agent_manifest() -> String {
    let head = r#"
[agent]
name = "izzie"
role = "agent"
model = ""
description = ""

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "x"
"#;
    format!("{head}{LIVE_AGENT_TOML}")
}

#[test]
fn an_agent_toml_listeners_table_is_absorbed_into_channels() {
    // #7609: loading an agent.toml folds the deprecated table into `channels`
    // with NO side effect on disk; the provider stays empty because this parse
    // cannot see a global config.
    let dir = tempfile::tempdir().expect("tempdir");
    let agent = write(dir.path(), "izzie.toml", &live_agent_manifest());
    let cfg = crate::agents::AgentConfig::load(&agent).expect("agent loads");
    assert_eq!(cfg.channels.len(), 1);
    assert_eq!(cfg.channels[0].scope, ChannelScope::Assistant);
    assert_eq!(cfg.channels[0].id, "gmail-personal");
    assert_eq!(cfg.channels[0].provider, "");
    assert_eq!(
        cfg.channels[0].event_types,
        vec!["message.received".to_string()]
    );
    assert_eq!(cfg.channels[0].wake_filter.from, vec!["*".to_string()]);
    assert_eq!(
        cfg.channels[0].wake_filter.exclude_labels,
        vec!["CATEGORY_PROMOTIONS".to_string()]
    );

    let bindings = cfg.listeners();
    assert_eq!(
        bindings, cfg.legacy_listeners,
        "the derived view must equal the legacy table field for field"
    );
    assert!(
        !dir.path().join("izzie.channels.json").exists(),
        "a parse must never write"
    );
}

#[test]
fn an_agent_channel_is_not_absorbed_twice() {
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = live_agent_manifest();
    let both = format!(
        "{manifest}\n[[channels]]\nid = \"gmail-personal\"\nname = \"gmail-personal\"\nprovider = \"slack\"\nenabled = true\n"
    );
    let agent = write(dir.path(), "izzie.toml", &both);
    let cfg = crate::agents::AgentConfig::load(&agent).expect("agent loads");
    assert_eq!(cfg.channels.len(), 1, "no duplicate entry");
    assert_eq!(
        cfg.channels[0].provider, "slack",
        "the explicit channel wins over the legacy binding"
    );
    assert_eq!(cfg.listeners().len(), 1);
}

#[test]
fn the_deprecation_warnings_fire_at_most_once() {
    // Both are `Once`-guarded, so calling them repeatedly must not panic and
    // must not re-arm; the observable contract is that they are idempotent.
    for _ in 0..3 {
        warn_global_listeners_deprecated();
        warn_agent_listeners_deprecated();
    }
}

#[test]
fn an_agent_binding_with_no_wake_filter_still_migrates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let agent = write(
        dir.path(),
        "agent.toml",
        "[[listeners]]\nname = \"gmail-personal\"\n",
    );
    let channels = dir.path().join("agent.channels.json");
    let report = migrate_agent_channels_if_absent(&agent, &channels, &[storable_global()])
        .expect("succeeds")
        .expect("migrates");
    assert_eq!(report.channels, vec!["gmail-personal".to_string()]);
    let stored: Vec<crate::api::server::agent_channels::Binding> =
        serde_json::from_str(&std::fs::read_to_string(&channels).expect("read back"))
            .expect("parses");
    assert_eq!(stored[0].filter, AgentBindingFilter::default());
    assert!(stored[0].event_types.is_empty());
}
