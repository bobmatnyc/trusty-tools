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

/// The drain takes the legacy table OUT, and keeps the comment above it.
///
/// Why this reverses the slice-2 contract (#7609 slice 7): the in-memory absorb
/// that made a left-behind `[[listeners]]` harmless is gone, so a table the
/// drain leaves would be an inert listener. Pre-change this fails at the first
/// assertion — the drain appended and left the table in place.
#[test]
fn the_drain_removes_the_legacy_listeners_table_and_keeps_its_comment() {
    let dir = tempfile::tempdir().expect("tempdir");
    let annotated = format!("# an operator note above the table\n{LIVE_GLOBAL_CONFIG}");
    let path = write(dir.path(), "config.toml", &annotated);
    migrate_global_if_absent(&path)
        .expect("migration succeeds")
        .expect("migration happened");

    let after = std::fs::read_to_string(&path).expect("read back");
    assert!(
        !after.contains("[[listeners]]"),
        "the retired table is gone:\n{after}"
    );
    assert!(
        after.contains("# an operator note above the table"),
        "the comment above it survives:\n{after}"
    );
    let table: toml::Table = toml::from_str(&after).expect("still parses");
    assert!(
        read_key::<ListenerConfig>(&table, "listeners", &path)
            .expect("no legacy table to parse")
            .is_none(),
        "and nothing re-reads it"
    );
    let listeners = read_key::<Channel>(&table, "channels", &path)
        .expect("the migrated table parses")
        .expect("the migrated table is present");
    assert_eq!(listeners.len(), 1);
    assert_eq!(
        listeners[0].credential_ref.as_deref(),
        Some("gmail/bob-personal")
    );
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

/// The live account-wide gmail global: an overlay of it IS storable now.
fn account_wide_gmail_global() -> Channel {
    Channel::from(ListenerConfig {
        name: "gmail-personal".into(),
        connector: "gmail".into(),
        identity: Some("bob-personal".into()),
        transport: "history-poll".into(),
        enabled: true,
        poll_interval_secs: 60,
        filter: ListenerFilter {
            label_ids: vec!["INBOX".into()],
        },
    })
}

/// The live izzie binding names an ACCOUNT-WIDE global, so it has no
/// destination of its own — and slice 4 stores it anyway, as an overlay.
///
/// Why (#7609): this is the open item slices 1-3 left. `storable_records`
/// skipped the binding because no adapter accepts an empty destination, so
/// izzie's configuration stayed in `agent.toml` and dispatch could not read it
/// from the channels file at all.
#[test]
fn an_overlay_of_an_account_wide_global_is_now_stored() {
    let dir = tempfile::tempdir().expect("tempdir");
    let agent = write(dir.path(), "agent.toml", LIVE_AGENT_TOML);
    let channels = dir.path().join("agent.channels.json");
    let globals = vec![account_wide_gmail_global()];

    let report = migrate_agent_channels_if_absent(&agent, &channels, &globals)
        .expect("succeeds")
        .expect("the overlay is storable");
    assert_eq!(report.channels, vec!["gmail-personal".to_string()]);

    let raw = std::fs::read_to_string(&channels).expect("read back");
    let stored: Vec<crate::api::server::agent_channels::Binding> =
        serde_json::from_str(&raw).expect("the written file must parse as bindings");
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].provider, "gworkspace",
        "the provider is the adapter that addresses this global's events"
    );
    assert!(
        stored[0].target.is_empty(),
        "an overlay names no destination"
    );
    assert!(
        stored[0].credential_ref.is_none(),
        "an overlay never sends, so it carries no send credential"
    );
    // The read path accepts exactly what the migration wrote — this is the
    // invariant that keeps the channel view from answering 500.
    assert!(stored[0].validate_in(&globals).is_ok());
    assert_eq!(
        std::fs::read_to_string(&agent).expect("read back"),
        LIVE_AGENT_TOML,
        "agent.toml must be left untouched"
    );
}

/// A binding the channel store would still reject stays in `agent.toml`.
///
/// Why: `agent_channels::load_at` validates every record it reads, so writing
/// one it would refuse turns a working channel view into a 500. Slice 4 moved
/// the gate to that same validation, so the case that remains unstorable is a
/// global whose provider this build carries no adapter for.
#[test]
fn an_unstorable_binding_is_left_in_agent_toml() {
    let dir = tempfile::tempdir().expect("tempdir");
    let agent = write(dir.path(), "agent.toml", LIVE_AGENT_TOML);
    let channels = dir.path().join("agent.channels.json");
    let globals = vec![Channel {
        provider: "notion".into(),
        ..account_wide_gmail_global()
    }];
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
    spawn_startup_migration(
        vec![dir.path().to_path_buf()],
        vec![storable_global()],
        Some(dir.path().join("config.toml")),
    );

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
        migrate_assistant_channels(
            &dirs,
            &[storable_global()],
            Some(&dir.path().join("config.toml"))
        )
        .is_empty(),
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

/// A retired `[[listeners]]` table is REPORTED, not folded, and its entries are
/// inert.
///
/// Pre-change (`origin/main`) this fails at the first assertion: the parse
/// folded the legacy binding into `channels`, so the list had one entry.
#[test]
fn a_residual_agent_listeners_table_is_reported_not_absorbed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let agent = write(dir.path(), "izzie.toml", &live_agent_manifest());
    let cfg = crate::agents::AgentConfig::load(&agent).expect("agent still loads");
    assert!(
        cfg.channels.is_empty(),
        "the retired table is not folded into channels: {:?}",
        cfg.channels
    );
    assert!(cfg.listeners().is_empty(), "so nothing wakes from it");
    assert!(
        cfg.residual_listeners
            .as_ref()
            .is_some_and(|table| table.len() == 1),
        "but the table is visible, so its presence can be reported"
    );
    assert!(
        !dir.path().join("izzie.channels.json").exists(),
        "a parse must never write"
    );
}

/// A manifest's own `[[channels]]` is what the parse reads, scoped to the file.
#[test]
fn an_agent_channel_carries_the_assistant_scope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = live_agent_manifest();
    let both = format!(
        "{manifest}\n[[channels]]\nid = \"gmail-personal\"\nname = \"gmail-personal\"\nprovider = \"slack\"\nenabled = true\n"
    );
    let agent = write(dir.path(), "izzie.toml", &both);
    let cfg = crate::agents::AgentConfig::load(&agent).expect("agent loads");
    assert_eq!(cfg.channels.len(), 1, "the declared channel, and only it");
    assert_eq!(cfg.channels[0].scope, ChannelScope::Assistant);
    assert_eq!(cfg.channels[0].provider, "slack");
    assert_eq!(cfg.listeners().len(), 1);
}

#[test]
fn the_retirement_notice_fires_at_most_once() {
    // `Once`-guarded, so calling it repeatedly must not panic and must not
    // re-arm; the observable contract is that it is idempotent.
    for _ in 0..3 {
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

/// The sweep appends the assistant to the global's `route_to`, so the pair
/// moves from dispatch source (3) to source (2) with no manual edit.
#[test]
fn the_sweep_backfills_route_to_for_a_legacy_binding() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "izzie.toml", &assistant_instance_manifest());
    let config = write(
        dir.path(),
        "config.toml",
        "# keep me\n[[channels]]\nid = \"gmail-personal\"\nname = \"gmail-personal\"\nprovider = \"gmail\"\nenabled = true\n",
    );
    let globals = vec![account_wide_gmail_global()];

    migrate_assistant_channels(&[dir.path().to_path_buf()], &globals, Some(&config));

    let raw = std::fs::read_to_string(&config).expect("read back");
    assert!(
        raw.contains("route_to = [\"izzie\"]"),
        "the sweep must name the assistant in route_to: {raw}"
    );
    assert!(raw.starts_with("# keep me"), "comments survive: {raw}");
}

/// With no backfill target the sweep still SEEDS every assistant's channels
/// file; only the `route_to` half is skipped.
///
/// Why (#7609 review MEDIUM-2): `if let Ok(config_path)` in
/// `api::server::routes` skipped the whole sweep when the global config path
/// would not resolve, silently retiring a migration that was unconditional
/// before slice 4.
#[test]
fn the_sweep_seeds_channels_with_no_backfill_target() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "izzie.toml", &assistant_instance_manifest());
    let globals = vec![account_wide_gmail_global()];

    let reports = migrate_assistant_channels(&[dir.path().to_path_buf()], &globals, None);

    assert_eq!(reports.len(), 1, "the seeding half still runs: {reports:?}");
    assert!(
        dir.path().join("izzie.channels.json").exists(),
        "the assistant's channels file is seeded with no config path to backfill"
    );
}

/// The backfill writes once and never again, and it edits the document rather
/// than re-serializing it.
#[test]
fn the_route_to_backfill_is_idempotent_and_keeps_comments() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = write(
        dir.path(),
        "config.toml",
        "# operator comment\n[mcp]\ninject_for_roles = [\"ctrl\"]\n\n[[channels]]\nid = \"gmail-personal\"\nname = \"gmail-personal\"\nprovider = \"gmail\"\n",
    );
    assert!(
        backfill_route_to(&config, "gmail-personal", "izzie").expect("succeeds"),
        "the first run writes"
    );
    let first = std::fs::read_to_string(&config).expect("read back");
    assert!(first.contains("# operator comment"));
    assert!(first.contains("inject_for_roles"));
    assert!(first.contains("route_to = [\"izzie\"]"), "{first}");

    assert!(
        !backfill_route_to(&config, "gmail-personal", "izzie").expect("succeeds"),
        "a name already present writes nothing"
    );
    assert_eq!(std::fs::read_to_string(&config).expect("read back"), first);

    // A second assistant appends beside the first.
    assert!(backfill_route_to(&config, "gmail-personal", "cto-assistant").expect("succeeds"));
    let second = std::fs::read_to_string(&config).expect("read back");
    assert!(second.contains("izzie"), "{second}");
    assert!(second.contains("cto-assistant"), "{second}");

    // A channel id this file does not declare writes nothing at all.
    assert!(!backfill_route_to(&config, "absent", "izzie").expect("succeeds"));
    assert_eq!(std::fs::read_to_string(&config).expect("read back"), second);
}

/// The global channels a daemon holds in memory for `LIVE_GLOBAL_CONFIG`.
///
/// Why: `api::server::routes` passes exactly what `GlobalConfig::load` parsed,
/// so deriving the fixture the same way keeps the legacy in-memory absorb
/// (#7609 requirement 4) inside the regression rather than beside it.
fn globals_as_a_daemon_loads_them(raw: &str) -> Vec<Channel> {
    crate::mcp::config::GlobalConfig::from_toml_str(raw)
        .expect("the fixture config parses")
        .channels
}

/// A daemon start — not the REPL — drains the global config, exactly once.
///
/// Why (#7609): `migrate_global_if_absent` was reachable only from
/// `GlobalConfig::load_or_create`, and only the REPL routing command calls
/// that. A supervised `tagent --api` / `tagent --slack` left `config.toml`
/// byte-identical across every restart, so the `route_to` backfill had no
/// `[[channels]]` entry to append the bound assistant to.
///
/// #7609 slice 7: a daemon that has not yet drained sees NO global channel —
/// the in-memory absorb that used to cover the gap is gone, which is exactly
/// why the drain has to run at startup rather than only in the REPL.
#[test]
fn a_daemon_start_drains_the_global_config_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = write(dir.path(), "config.toml", LIVE_GLOBAL_CONFIG);
    write(dir.path(), "izzie.toml", &assistant_instance_manifest());
    let globals = globals_as_a_daemon_loads_them(LIVE_GLOBAL_CONFIG);
    assert!(
        globals.is_empty(),
        "an undrained legacy table yields no channel: {globals:?}"
    );

    let first = run_startup_migration(Some(&config), &[dir.path().to_path_buf()], &globals);

    let moved = first
        .global
        .expect("the drain must not fail")
        .expect("the first start must drain the legacy table");
    assert_eq!(moved.channels, vec!["gmail-personal".to_string()]);
    let channels = channels_in(&config);
    assert_eq!(channels.len(), 1, "one migrated channel: {channels:?}");
    assert_eq!(channels[0].id, "gmail-personal");
    assert_eq!(
        globals_as_a_daemon_loads_them(&std::fs::read_to_string(&config).expect("read back"))
            .iter()
            .map(|channel| channel.id.as_str())
            .collect::<Vec<_>>(),
        vec!["gmail-personal"],
        "the NEXT daemon load sees the drained channel, which is the whole point \
         of draining at startup"
    );
    let after_first = std::fs::read_to_string(&config).expect("read back");
    assert!(
        after_first.contains("route_to = [\"izzie\"]"),
        "the drain must run before the sweep so the backfill has a channel to \
         name the bound assistant in: {after_first}"
    );

    // A second supervised start of the same daemon.
    let second = run_startup_migration(Some(&config), &[dir.path().to_path_buf()], &globals);

    assert!(
        second
            .global
            .expect("the second drain must not fail")
            .is_none(),
        "a second start must drain nothing"
    );
    assert_eq!(
        channels_in(&config).len(),
        1,
        "a second start must not append a duplicate [[channels]] entry"
    );
    assert_eq!(
        std::fs::read_to_string(&config).expect("read back"),
        after_first,
        "a second start must not touch the file at all"
    );
}

/// A global config the drain cannot write still starts the daemon, and says
/// what the failure costs.
///
/// Why (#7609): the drain runs on the startup path of a supervised process. A
/// read-only or hand-broken `config.toml` is an operator problem to log, never
/// a reason to refuse to boot — and it must not take the assistant half of the
/// sweep down with it.
///
/// Why the log text is asserted (#7609 slice 7 review): the alarm used to
/// promise the daemon "starts on the in-memory absorb instead". That absorb is
/// gone, so the promise was false and the operator had no reason to act. The
/// wording is the only notice they get, which makes it behaviour.
#[test]
fn a_daemon_start_survives_a_malformed_global_config() {
    let logs = crate::test_env::CaptureWriter::default();
    let _log_guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_ansi(false)
            .finish(),
    );
    let dir = tempfile::tempdir().expect("tempdir");
    // `[[channels]]` is present but its `id` is not a string, so the drain
    // reports a parse error instead of reading it as "already migrated".
    let body = "[[channels]]\nid = 42\n";
    let config = write(dir.path(), "config.toml", body);
    write(dir.path(), "izzie.toml", &assistant_instance_manifest());
    let globals = vec![account_wide_gmail_global()];

    let report = run_startup_migration(Some(&config), &[dir.path().to_path_buf()], &globals);

    match &report.global {
        Err(ChannelMigrationError::Parse { key, path, .. }) => {
            assert_eq!(*key, "channels");
            assert!(path.contains("config.toml"), "the path is named: {path}");
        }
        other => panic!("the drain must REPORT the failure, not swallow it: {other:?}"),
    }
    assert_eq!(
        std::fs::read_to_string(&config).expect("read back"),
        body,
        "a failed drain leaves the operator's file untouched"
    );
    assert_eq!(
        report.assistants.len(),
        1,
        "the assistant half still runs after a failed drain: {report:?}"
    );
    let logged = logs.contents();
    assert!(
        logged.contains("its entries are INERT")
            && logged.contains("move them to [[channels]] by hand"),
        "the alarm must state the real consequence and the hand edit: {logged}"
    );
    assert!(
        !logged.contains("in-memory absorb"),
        "the retired absorb must never be offered as a fallback again: {logged}"
    );
}

/// The shared runtime startup hook drains, not just the function it delegates
/// to.
///
/// Why (#7609 review): `spawn_global_migration` is the wiring every non-`--api`
/// `tagent` start depends on. Proving only `run_startup_migration` leaves the
/// spawn, the empty assistant roster and the `Some(config_path)` argument
/// untested — and the empty roster is the one argument shape no other test
/// passes through the spawn.
///
/// Single-threaded, unlike `the_assistant_sweep_never_blocks_its_caller`: that
/// test needs a second worker because it parks one on a held lock, this one has
/// nothing to block on. A 2-worker runtime here made the suite's pre-existing
/// unsynchronized-`$HOME` tests (`stores::binding::tests`,
/// `mcp::config::tests`) fail 3 runs out of 3 by widening their race window;
/// current-thread passed 3 of 3. See the #8064 lock-per-process-global work.
#[tokio::test]
async fn the_startup_hook_drains_the_global_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = write(dir.path(), "config.toml", LIVE_GLOBAL_CONFIG);

    spawn_global_migration(config.clone());

    // Poll the drain's own result rather than sleeping a fixed interval: the
    // hook is fire-and-forget, so there is nothing to await.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let channels = loop {
        let channels = channels_in(&config);
        if !channels.is_empty() {
            break channels;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the startup hook never drained: {}",
            std::fs::read_to_string(&config).expect("read back")
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    assert_eq!(channels.len(), 1, "one migrated channel: {channels:?}");
    assert_eq!(channels[0].id, "gmail-personal");
}
