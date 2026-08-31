//! Unit tests for `GlobalConfig` load/save/mutation behavior.
//!
//! Why: These tests hold `HOME_LOCK` (a `std::sync::Mutex`) across async
//! I/O to serialize global $HOME mutation between tests. See
//! `crate::test_env` for the full rationale.
//!
//! Layout: `mod.rs` covers create/load/save/service-mutation; `render_tests.rs`
//! covers role gating, prompt/list rendering, and the local-inference section.
#![allow(clippy::await_holding_lock)]

mod render_tests;

use std::path::PathBuf;

use crate::mcp::config::{GlobalConfig, McpService, McpTool};
use crate::test_env::HOME_LOCK;

/// Create a unique tempdir under the system temp for HOME sandboxing.
///
/// Why: Several tests point `$HOME` at a throwaway dir to exercise the
/// config-on-disk paths without touching the developer's real config.
/// Test: Used by the load/save tests in this module + `render_tests`.
pub(super) fn tempdir() -> PathBuf {
    let p = std::env::temp_dir().join(format!("trusty-agents-mcp-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[tokio::test]
async fn load_or_create_writes_default_when_absent() {
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir();
    unsafe {
        std::env::set_var("HOME", &home);
    }

    let cfg = GlobalConfig::load_or_create()
        .await
        .expect("create default config");

    let path = home.join(".trusty-agents").join("config.toml");
    assert!(
        path.exists(),
        "config file should exist after load_or_create"
    );

    // Defaults after ADR-0014 (#256, native-MCP retire) + #3203/#3204:
    // trusty-mpm (enabled, native), granola-notes (enabled), trusty-memory
    // (enabled, native — moved off the dead OpenRPC tool_registry.endpoints
    // stub), trusty-search (same), duetto-memory (disabled). slack-user-proxy
    // was retired as a dead external stub; gworkspace-mcp was removed
    // (#3204) — its static tool list had drifted from the real binary and is
    // superseded by the OpenRPC "gworkspace" tool_registry.endpoints entry.
    assert_eq!(cfg.mcp.services.len(), 5);
    let tm = cfg
        .mcp
        .services
        .iter()
        .find(|s| s.name == "trusty-mpm")
        .expect("trusty-mpm present in defaults (#3203)");
    assert!(tm.enabled);
    let mem = cfg
        .mcp
        .services
        .iter()
        .find(|s| s.name == "trusty-memory")
        .expect("trusty-memory present as a live mcp.services entry");
    assert!(mem.enabled, "trusty-memory must ship enabled, not DISABLED");
    let search = cfg
        .mcp
        .services
        .iter()
        .find(|s| s.name == "trusty-search")
        .expect("trusty-search present as a live mcp.services entry");
    assert!(
        search.enabled,
        "trusty-search must ship enabled, not DISABLED"
    );
    assert!(
        !cfg.mcp
            .services
            .iter()
            .any(|s| s.name == "slack-user-proxy"),
        "slack-user-proxy retired per ADR-0014"
    );
    assert!(
        !cfg.mcp.services.iter().any(|s| s.name == "gworkspace-mcp"),
        "gworkspace-mcp mcp.services entry removed (#3204); live path is the \
         OpenRPC tool_registry.endpoints \"gworkspace\" entry"
    );
    let granola = cfg
        .mcp
        .services
        .iter()
        .find(|s| s.name == "granola-notes")
        .expect("granola-notes present in defaults (#256)");
    assert!(granola.enabled);
    let duetto = cfg
        .mcp
        .services
        .iter()
        .find(|s| s.name == "duetto-memory")
        .expect("duetto-memory present in defaults (#256)");
    assert!(
        !duetto.enabled,
        "duetto-memory should be disabled by default"
    );
    assert_eq!(duetto.transport, "http");
    assert_eq!(
        duetto.url.as_deref(),
        Some("https://mcp-services.dev.duettosystems.com/memory/mcp")
    );
    // No native local integrations in the registry — those are wired into
    // the harness directly (kuzu-memory, mcp-vector-search).
    assert!(
        !cfg.mcp.services.iter().any(|s| s.name == "kuzu-memory"),
        "kuzu-memory must not appear in MCP registry"
    );
    assert!(
        !cfg.mcp
            .services
            .iter()
            .any(|s| s.name == "mcp-vector-search"),
        "mcp-vector-search must not appear in MCP registry"
    );
    assert!(cfg.mcp.inject_for_roles.contains(&"ctrl".to_string()));
    assert!(cfg.mcp.inject_for_roles.contains(&"pm".to_string()));
}

#[tokio::test]
async fn load_or_create_reads_existing_file() {
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir();
    unsafe {
        std::env::set_var("HOME", &home);
    }
    let cfg_dir = home.join(".trusty-agents");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("config.toml"),
        r#"
[mcp]
inject_for_roles = ["ctrl"]

[[mcp.services]]
name = "custom"
description = "a custom service"
command = "echo"
transport = "stdio"
enabled = true
"#,
    )
    .unwrap();

    let cfg = GlobalConfig::load_or_create()
        .await
        .expect("load existing config");
    assert_eq!(cfg.mcp.inject_for_roles, vec!["ctrl".to_string()]);
    assert_eq!(cfg.mcp.services.len(), 1);
    assert_eq!(cfg.mcp.services[0].name, "custom");
}

#[tokio::test]
async fn load_returns_documented_defaults_when_absent() {
    // (#244, #245) load() must not create the file (unlike load_or_create),
    // but must return the documented defaults (native trusty-mpm,
    // granola-notes, duetto-memory) so prompt-build paths see the same registry
    // that `load_or_create` would write.
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir();
    unsafe {
        std::env::set_var("HOME", &home);
    }
    let cfg = GlobalConfig::load().await;
    let path = home.join(".trusty-agents").join("config.toml");
    assert!(!path.exists(), "load() must not create the config file");
    // #245/#256 + ADR-0014 + #3203/#3204: defaults mirror DEFAULT_CONFIG_TOML —
    // 5 services (trusty-mpm, granola-notes, trusty-memory, trusty-search,
    // duetto-memory) after slack-user-proxy retire and gworkspace-mcp
    // removal. trusty-memory/trusty-search moved here from the dead
    // tool_registry.endpoints OpenRPC stubs (see default-config.toml).
    assert_eq!(cfg.mcp.services.len(), 5);
    assert!(cfg.mcp.services.iter().any(|s| s.name == "trusty-mpm"));
    assert!(!cfg.mcp.services.iter().any(|s| s.name == "gworkspace-mcp"));
    assert!(
        !cfg.mcp
            .services
            .iter()
            .any(|s| s.name == "slack-user-proxy")
    );
    assert!(cfg.mcp.services.iter().any(|s| s.name == "granola-notes"));
    assert!(cfg.mcp.services.iter().any(|s| s.name == "duetto-memory"));
    let mem = cfg
        .mcp
        .services
        .iter()
        .find(|s| s.name == "trusty-memory")
        .expect("trusty-memory present in defaults");
    assert!(mem.enabled, "trusty-memory must ship enabled, not DISABLED");
    let search = cfg
        .mcp
        .services
        .iter()
        .find(|s| s.name == "trusty-search")
        .expect("trusty-search present in defaults");
    assert!(
        search.enabled,
        "trusty-search must ship enabled, not DISABLED"
    );
}

#[tokio::test]
async fn save_and_reload_roundtrip() {
    // (#244) save() then load() must round-trip identically.
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir();
    unsafe {
        std::env::set_var("HOME", &home);
    }
    let mut cfg = GlobalConfig::default();
    cfg.mcp.inject_for_roles = vec!["ctrl".to_string(), "pm".to_string()];
    cfg.mcp.services.push(McpService {
        name: "test-svc".to_string(),
        description: "A test service".to_string(),
        command: "test-cmd".to_string(),
        args: vec!["arg1".to_string()],
        env: std::collections::HashMap::new(),
        url: None,
        transport: "stdio".to_string(),
        enabled: true,
        tools: vec![McpTool {
            name: "test_tool".to_string(),
            description: "A test tool".to_string(),
        }],
        discover: false,
    });
    cfg.save().await.expect("save should succeed");
    let reloaded = GlobalConfig::load().await;
    assert_eq!(reloaded.mcp.services.len(), 1);
    assert_eq!(reloaded.mcp.services[0].name, "test-svc");
    assert_eq!(reloaded.mcp.services[0].tools.len(), 1);
    assert_eq!(reloaded.mcp.services[0].tools[0].name, "test_tool");
    assert!(reloaded.mcp.services[0].enabled);
}

/// #3766: `[providers] default_provider_id` must survive a save driven by an
/// unrelated setting.
///
/// Why: `save()` re-serializes only the fields declared on `GlobalConfig`, so
/// a `[providers]` table parsed by some OTHER reader would be silently erased
/// the next time anything wrote this file. That is not hypothetical — the
/// ordinary `/local on` / `/local off` commands
/// (`repl::commands::routing::handle_local_command_into`) and the four `mcp_*`
/// mutators all load, mutate one field, and save. This test drives that exact
/// sequence, so it fails if the section is ever demoted back out of the
/// modelled schema.
/// Test: itself.
#[tokio::test]
async fn providers_section_survives_an_unrelated_save() {
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir();
    unsafe {
        std::env::set_var("HOME", &home);
    }

    // An operator sets the policy by hand.
    let dir = home.join(".trusty-agents");
    std::fs::create_dir_all(&dir).expect("create config dir");
    std::fs::write(
        dir.join("config.toml"),
        "[providers]\ndefault_provider_id = \"bedrock\"\n",
    )
    .expect("write config");

    let mut cfg = GlobalConfig::load_or_create().await.expect("load config");
    assert_eq!(
        cfg.providers.default_provider_id.as_deref(),
        Some("bedrock"),
        "the hand-written policy must be read back"
    );

    // What `/local on` does: mutate one unrelated field, then save.
    cfg.local_inference.enabled = !cfg.local_inference.enabled;
    cfg.save().await.expect("save should succeed");

    let reloaded = GlobalConfig::load().await;
    assert_eq!(
        reloaded.providers.default_provider_id.as_deref(),
        Some("bedrock"),
        "an unrelated save must not erase the operator's provider policy"
    );
    // The on-disk text is what the next process parses, so assert it directly.
    let on_disk = std::fs::read_to_string(dir.join("config.toml")).expect("read saved config");
    assert!(
        on_disk.contains("default_provider_id"),
        "the saved file dropped [providers]:\n{on_disk}"
    );
}

/// #3766: an absent `[providers]` table is unset, not an error.
///
/// Why: every existing `config.toml` predates the section, and the shipped
/// default leaves it commented out. Absent must round-trip as "no policy".
/// Test: itself.
#[tokio::test]
async fn providers_section_defaults_to_unset() {
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir();
    unsafe {
        std::env::set_var("HOME", &home);
    }

    let cfg = GlobalConfig::load_or_create()
        .await
        .expect("create default");
    assert_eq!(cfg.providers.default_provider_id, None);

    cfg.save().await.expect("save should succeed");
    assert_eq!(
        GlobalConfig::load().await.providers.default_provider_id,
        None
    );
}

#[tokio::test]
async fn save_publishes_by_rename_leaving_the_old_file_intact() {
    // audit 2026-08-19: `save()` documented an atomic write but ran a plain
    // `tokio::fs::write`, which truncates the live config in place — a crash
    // between truncate and the last byte leaves a torn config that the next
    // `load()` silently replaces with defaults. Temp-then-rename swaps the
    // directory entry instead, so the previously published inode is never
    // written to and stays a complete, parseable config.
    //
    // The hard link is the observer: it names the inode that `save()` #1
    // published. After `save()` #2 it must still read the OLD content. Under
    // an in-place `fs::write` it would read the NEW content, because both
    // names point at the one inode being overwritten.
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir();
    unsafe {
        std::env::set_var("HOME", &home);
    }

    let mut cfg = GlobalConfig::default();
    cfg.mcp.inject_for_roles = vec!["ctrl".to_string()];
    cfg.save().await.expect("first save");

    let path = home.join(".trusty-agents").join("config.toml");
    let published = std::fs::read_to_string(&path).expect("read first save");

    let witness = home.join(".trusty-agents").join("witness.toml");
    std::fs::hard_link(&path, &witness).expect("hard link the published inode");

    cfg.mcp.inject_for_roles = vec!["pm".to_string()];
    cfg.save().await.expect("second save");

    let republished = std::fs::read_to_string(&path).expect("read second save");
    assert_ne!(
        republished, published,
        "the target must carry the second save's content"
    );
    assert_eq!(
        std::fs::read_to_string(&witness).expect("read witness"),
        published,
        "the inode published by the first save must be untouched — proof the \
         second save published by rename rather than truncating in place"
    );
}

#[tokio::test]
async fn save_leaves_no_scratch_file_behind() {
    // audit 2026-08-19: temp-then-rename must clean up after itself — a
    // scratch file left in `~/.trusty-agents/` is visible clutter, and an
    // orphaned one is indistinguishable from an interrupted write.
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir();
    unsafe {
        std::env::set_var("HOME", &home);
    }

    let mut cfg = GlobalConfig::default();
    cfg.mcp.inject_for_roles = vec!["ctrl".to_string()];
    cfg.save().await.expect("save should succeed");

    let dir = home.join(".trusty-agents");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .expect("read config dir")
        .map(|e| {
            e.expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["config.toml".to_string()],
        "save() must leave exactly the published config, no scratch files"
    );

    let reloaded = GlobalConfig::load().await;
    assert_eq!(reloaded.mcp.inject_for_roles, vec!["ctrl".to_string()]);
}

#[tokio::test]
async fn add_service_replaces_existing() {
    // (#244) add_service with a name that already exists replaces, not appends.
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir();
    unsafe {
        std::env::set_var("HOME", &home);
    }
    let mut cfg = GlobalConfig::default();
    cfg.add_service(McpService {
        name: "x".to_string(),
        description: "first".to_string(),
        command: "a".to_string(),
        args: vec![],
        env: std::collections::HashMap::new(),
        url: None,
        transport: "stdio".to_string(),
        enabled: true,
        tools: vec![],
        discover: false,
    })
    .await
    .unwrap();
    cfg.add_service(McpService {
        name: "x".to_string(),
        description: "second".to_string(),
        command: "b".to_string(),
        args: vec![],
        env: std::collections::HashMap::new(),
        url: None,
        transport: "stdio".to_string(),
        enabled: true,
        tools: vec![],
        discover: false,
    })
    .await
    .unwrap();
    assert_eq!(cfg.mcp.services.len(), 1);
    assert_eq!(cfg.mcp.services[0].description, "second");
    assert_eq!(cfg.mcp.services[0].command, "b");
}

#[tokio::test]
async fn remove_service_returns_correct_bool() {
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir();
    unsafe {
        std::env::set_var("HOME", &home);
    }
    let mut cfg = GlobalConfig::default();
    cfg.add_service(McpService {
        name: "x".to_string(),
        description: "d".to_string(),
        command: "c".to_string(),
        args: vec![],
        env: std::collections::HashMap::new(),
        url: None,
        transport: "stdio".to_string(),
        enabled: true,
        tools: vec![],
        discover: false,
    })
    .await
    .unwrap();
    assert!(cfg.remove_service("x").await.unwrap());
    assert!(!cfg.remove_service("x").await.unwrap());
    assert!(cfg.mcp.services.is_empty());
}

#[tokio::test]
async fn enable_disable_toggles_flag() {
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir();
    unsafe {
        std::env::set_var("HOME", &home);
    }
    let mut cfg = GlobalConfig::default();
    cfg.add_service(McpService {
        name: "x".to_string(),
        description: "d".to_string(),
        command: "c".to_string(),
        args: vec![],
        env: std::collections::HashMap::new(),
        url: None,
        transport: "stdio".to_string(),
        enabled: false,
        tools: vec![],
        discover: false,
    })
    .await
    .unwrap();
    assert!(cfg.enable_service("x").await.unwrap());
    assert!(cfg.mcp.services[0].enabled);
    assert!(cfg.disable_service("x").await.unwrap());
    assert!(!cfg.mcp.services[0].enabled);
    // Unknown name returns false.
    assert!(!cfg.enable_service("missing").await.unwrap());
    assert!(!cfg.disable_service("missing").await.unwrap());
}

// --- [[listeners]] section (#3820, DOC-54 SPEC-AGENTS-06) ---------------

#[tokio::test]
async fn listeners_section_defaults_empty() {
    // A `config.toml` with no `[[listeners]]` entries (every file that
    // predates this field) must still parse, with an empty list.
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cfg = GlobalConfig::from_toml_str("").expect("empty config parses");
    assert!(cfg.listeners.is_empty());
}

#[tokio::test]
async fn listeners_section_round_trips() {
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir();
    unsafe {
        std::env::set_var("HOME", &home);
    }
    let mut cfg = GlobalConfig::default();
    cfg.listeners
        .push(crate::listeners::config::ListenerConfig {
            name: "gmail-personal".to_string(),
            connector: "gmail".to_string(),
            identity: Some("bob-personal".to_string()),
            transport: "history-poll".to_string(),
            enabled: false,
            poll_interval_secs: 180,
            filter: crate::listeners::config::ListenerFilter {
                label_ids: vec!["INBOX".to_string()],
            },
        });
    cfg.save().await.expect("save should succeed");
    let reloaded = GlobalConfig::load().await;
    assert_eq!(reloaded.listeners.len(), 1);
    assert_eq!(reloaded.listeners[0].name, "gmail-personal");
    assert_eq!(
        reloaded.listeners[0].identity.as_deref(),
        Some("bob-personal")
    );
    assert!(
        !reloaded.listeners[0].enabled,
        "round-trips disabled-by-default"
    );
    assert_eq!(
        reloaded.listeners[0].filter.label_ids,
        vec!["INBOX".to_string()]
    );
}
