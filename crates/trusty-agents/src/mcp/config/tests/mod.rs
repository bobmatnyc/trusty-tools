//! Unit tests for `GlobalConfig` load/save/mutation behavior.
//!
//! Why: These tests hold `HOME_LOCK` (a `std::sync::Mutex`) across async
//! I/O to serialize global $HOME mutation between tests. See
//! `crate::test_env` for the full rationale.
//!
//! Layout: `mod.rs` covers create/load/save; `render_tests.rs` covers role
//! gating, prompt rendering, and the local-inference section. The MCP SERVER
//! surface these files used to cover moved to `crate::mcp::tests` with #7454.
#![allow(clippy::await_holding_lock)]

mod render_tests;

use std::path::PathBuf;

use crate::mcp::config::GlobalConfig;
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

/// #7454: `load_or_create` still writes the default file when absent, and the
/// file it writes still parses. What it no longer does is DESCRIBE MCP
/// servers — those moved to `trusty_mcp`'s shared file (ADR-0060), and the
/// legacy tables this asset still carries exist only as the migration's seed
/// (`crate::mcp::shared::migrate`).
#[tokio::test]
async fn load_or_create_writes_default_when_absent() {
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir();
    unsafe {
        std::env::set_var("HOME", &home);
    }

    let path = GlobalConfig::config_path().unwrap();
    assert!(!path.exists());

    let cfg = GlobalConfig::load_or_create().await.unwrap();
    assert!(path.exists(), "load_or_create must materialise the file");
    assert!(
        cfg.mcp.inject_for_roles.iter().any(|r| r == "ctrl"),
        "the default must still gate the prompt layer on the coordinating roles"
    );
    assert!(
        !cfg.mcp.trust_project_mcp_json,
        "#3266: the .mcp.json trust gate must stay off by default"
    );
}

/// The shipped default asset is the migration's seed, so it must still carry
/// the connectors #7454 moved — a default that drained to nothing would leave
/// a fresh install with no MCP servers at all.
#[tokio::test]
async fn the_default_asset_still_seeds_the_shared_server_file() {
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir();
    unsafe {
        std::env::set_var("HOME", &home);
    }
    GlobalConfig::load_or_create().await.unwrap();

    let legacy = GlobalConfig::config_path().unwrap();
    let raw = std::fs::read_to_string(&legacy).unwrap();
    let (servers, report) = crate::mcp::shared::migrate::convert(&raw);

    for expected in [
        "trusty-mpm",
        "granola-notes",
        "trusty-memory",
        "trusty-search",
    ] {
        assert!(
            servers.iter().any(|s| s.name == expected),
            "{expected} must survive the migration; moved: {}",
            report.summary()
        );
    }
    assert!(
        servers.iter().any(|s| s.name == "gworkspace"),
        "the registry endpoint must migrate too; moved: {}",
        report.summary()
    );
}

#[tokio::test]
async fn load_or_create_reads_existing_file() {
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir();
    unsafe {
        std::env::set_var("HOME", &home);
    }
    let dir = home.join(".trusty-agents");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.toml"),
        "[mcp]\ninject_for_roles = [\"ctrl\"]\ntrust_project_mcp_json = true\n",
    )
    .unwrap();

    let cfg = GlobalConfig::load_or_create().await.unwrap();
    assert_eq!(cfg.mcp.inject_for_roles, ["ctrl"]);
    assert!(cfg.mcp.trust_project_mcp_json);
}

#[tokio::test]
async fn load_returns_documented_defaults_when_absent() {
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir();
    unsafe {
        std::env::set_var("HOME", &home);
    }
    // No side effects: `load()` must not create the file.
    let cfg = GlobalConfig::load().await;
    assert!(!GlobalConfig::config_path().unwrap().exists());
    assert!(cfg.mcp.inject_for_roles.iter().any(|r| r == "pm"));
}

#[tokio::test]
async fn save_and_reload_roundtrip() {
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir();
    unsafe {
        std::env::set_var("HOME", &home);
    }

    let mut cfg = GlobalConfig::default();
    cfg.mcp.inject_for_roles = vec!["ctrl".to_string(), "research".to_string()];
    cfg.mcp.trust_project_mcp_json = true;
    cfg.save().await.unwrap();

    let reloaded = GlobalConfig::load().await;
    assert_eq!(reloaded.mcp.inject_for_roles, ["ctrl", "research"]);
    assert!(reloaded.mcp.trust_project_mcp_json);
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
