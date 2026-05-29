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
    let p = std::env::temp_dir().join(format!("open-mpm-mcp-{}", uuid::Uuid::new_v4()));
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

    let path = home.join(".open-mpm").join("config.toml");
    assert!(
        path.exists(),
        "config file should exist after load_or_create"
    );

    // Defaults (#256): gworkspace-mcp (enabled), slack-user-proxy (disabled),
    // granola-notes (enabled), duetto-memory (disabled).
    assert_eq!(cfg.mcp.services.len(), 4);
    let gw = cfg
        .mcp
        .services
        .iter()
        .find(|s| s.name == "gworkspace-mcp")
        .expect("gworkspace-mcp present in defaults");
    assert!(gw.enabled);
    let slack = cfg
        .mcp
        .services
        .iter()
        .find(|s| s.name == "slack-user-proxy")
        .expect("slack-user-proxy present in defaults");
    assert!(!slack.enabled);
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
    let cfg_dir = home.join(".open-mpm");
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
    // but must return the documented defaults (gworkspace-mcp +
    // slack-user-proxy) so prompt-build paths see the same registry that
    // `load_or_create` would write.
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir();
    unsafe {
        std::env::set_var("HOME", &home);
    }
    let cfg = GlobalConfig::load().await;
    let path = home.join(".open-mpm").join("config.toml");
    assert!(!path.exists(), "load() must not create the config file");
    // #245/#256: defaults now mirror DEFAULT_CONFIG_TOML — 4 services
    // (gworkspace-mcp, slack-user-proxy, granola-notes, duetto-memory).
    assert_eq!(cfg.mcp.services.len(), 4);
    assert!(cfg.mcp.services.iter().any(|s| s.name == "gworkspace-mcp"));
    assert!(
        cfg.mcp
            .services
            .iter()
            .any(|s| s.name == "slack-user-proxy")
    );
    assert!(cfg.mcp.services.iter().any(|s| s.name == "granola-notes"));
    assert!(cfg.mcp.services.iter().any(|s| s.name == "duetto-memory"));
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
        url: None,
        transport: "stdio".to_string(),
        enabled: true,
        tools: vec![McpTool {
            name: "test_tool".to_string(),
            description: "A test tool".to_string(),
        }],
    });
    cfg.save().await.expect("save should succeed");
    let reloaded = GlobalConfig::load().await;
    assert_eq!(reloaded.mcp.services.len(), 1);
    assert_eq!(reloaded.mcp.services[0].name, "test-svc");
    assert_eq!(reloaded.mcp.services[0].tools.len(), 1);
    assert_eq!(reloaded.mcp.services[0].tools[0].name, "test_tool");
    assert!(reloaded.mcp.services[0].enabled);
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
        url: None,
        transport: "stdio".to_string(),
        enabled: true,
        tools: vec![],
    })
    .await
    .unwrap();
    cfg.add_service(McpService {
        name: "x".to_string(),
        description: "second".to_string(),
        command: "b".to_string(),
        args: vec![],
        url: None,
        transport: "stdio".to_string(),
        enabled: true,
        tools: vec![],
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
        url: None,
        transport: "stdio".to_string(),
        enabled: true,
        tools: vec![],
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
        url: None,
        transport: "stdio".to_string(),
        enabled: false,
        tools: vec![],
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
