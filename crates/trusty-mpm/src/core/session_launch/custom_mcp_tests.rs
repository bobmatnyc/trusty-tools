//! Unit tests for the CUSTOM (non-native) MCP fleet bridge (issue #2739
//! follow-up).
//!
//! Why: split out of the injector module (`custom_mcp.rs`, a 500-SLOC-capped
//! production file) so coverage can grow under the 1500-SLOC test cap,
//! mirroring the `native_mcp_tests.rs` split.
//! What: drives [`super::custom_mcp::inject_custom_trusty_mcps`] against a
//! tempdir managed `.claude.json` registry (user scope) and a directly-built
//! `[mcp.custom]` map (project scope, as `HarnessPlan::from_manifest` would
//! resolve it), asserting: a clean-URL custom http server bridges (the
//! acceptance example), a custom stdio server bridges with its `env` routed to
//! `.env.local`, a project-scope entry bridges and overrides a same-named
//! user-scope entry, native/builtin names are never touched by this module,
//! and a remote entry carrying `headers` is rejected (fail-closed).
//! Test: this file IS the test module.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};
use tempfile::tempdir;

use super::custom_mcp::{inject_custom_trusty_mcps, validate_custom_remote_server};
use crate::core::manifest::CustomMcpServer;

const FAKE_TOKEN: &str = "custom-secret-DO-NOT-COMMIT";

/// `git init -q` a workspace so `.env.local` exclusion actually runs.
fn git_init(dir: &Path) {
    let status = std::process::Command::new("git")
        .arg("init")
        .arg("-q")
        .arg(dir)
        .status()
        .expect("git must be on PATH to run this test");
    assert!(status.success(), "git init failed");
}

/// Write a managed `.claude.json` with a mix of native, builtin, and custom
/// (stdio + http) servers.
fn seed_managed_registry(config_dir: &Path) {
    std::fs::create_dir_all(config_dir).unwrap();
    let registry = json!({
        "mcpServers": {
            "slack-mcp": { "type": "stdio", "command": "slack-mcp", "args": [] },
            "trusty-memory": { "type": "stdio", "command": "trusty-memory", "args": ["serve", "--stdio"] },
            "duetto-memory": {
                "type": "http",
                "url": "https://mcp-services.dev.duettosystems.com/memory/mcp"
            },
            "my-local-tool": {
                "type": "stdio",
                "command": "my-tool",
                "args": ["serve"],
                "env": { "MY_TOOL_TOKEN": FAKE_TOKEN }
            }
        }
    });
    std::fs::write(
        config_dir.join(".claude.json"),
        serde_json::to_string_pretty(&registry).unwrap(),
    )
    .unwrap();
}

fn read_injected(workspace: &Path) -> serde_json::Map<String, Value> {
    let text = std::fs::read_to_string(workspace.join(".mcp.json")).unwrap();
    let value: Value = serde_json::from_str(&text).unwrap();
    value["mcpServers"].as_object().cloned().unwrap()
}

/// Point `$HOME` at `home` for the body, restoring it afterwards even on panic
/// (mirrors `native_mcp_tests::with_fake_home`).
fn with_fake_home<F: FnOnce()>(home: &Path, body: F) {
    let prev = std::env::var("HOME").ok();
    // SAFETY: callers are `#[serial_test::serial]`, so no other thread races
    // this set/restore; the restore runs regardless of a panic in `body`.
    unsafe { std::env::set_var("HOME", home) };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
    match prev {
        Some(p) => unsafe { std::env::set_var("HOME", p) },
        None => unsafe { std::env::remove_var("HOME") },
    }
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

#[test]
#[serial_test::serial]
fn custom_user_scope_http_server_bridges() {
    // Acceptance criterion: a clean-URL custom http server registered
    // user-scope (the duetto-memory shape) must land in the session's
    // effective `.mcp.json`.
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    let managed_dir = home
        .path()
        .join(".trusty-tools")
        .join("trusty-mpm")
        .join("claude-config");
    seed_managed_registry(&managed_dir);

    with_fake_home(home.path(), || {
        inject_custom_trusty_mcps(ws.path(), &BTreeMap::new()).expect("injection succeeds");
    });

    let servers = read_injected(ws.path());
    let entry = servers
        .get("duetto-memory")
        .expect("custom http server bridged");
    assert_eq!(entry["type"], json!("http"));
    assert_eq!(
        entry["url"],
        json!("https://mcp-services.dev.duettosystems.com/memory/mcp")
    );
    assert!(entry.get("headers").is_none());
}

#[test]
#[serial_test::serial]
fn custom_user_scope_stdio_server_bridges_with_secret_routing() {
    // A custom stdio server's `env` must never land in `.mcp.json` — it is
    // routed to `.env.local`, exactly like the native allowlist.
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    git_init(ws.path());
    let managed_dir = home
        .path()
        .join(".trusty-tools")
        .join("trusty-mpm")
        .join("claude-config");
    seed_managed_registry(&managed_dir);

    with_fake_home(home.path(), || {
        inject_custom_trusty_mcps(ws.path(), &BTreeMap::new()).expect("injection succeeds");
    });

    let servers = read_injected(ws.path());
    let entry = servers
        .get("my-local-tool")
        .expect("custom stdio server bridged");
    assert_eq!(entry["command"], json!("my-tool"));
    assert!(entry.get("env").is_none(), "env stripped from .mcp.json");

    let mcp_json = std::fs::read_to_string(ws.path().join(".mcp.json")).unwrap();
    assert!(!mcp_json.contains(FAKE_TOKEN));

    let env_local = std::fs::read_to_string(ws.path().join(".env.local"))
        .expect(".env.local written for the custom stdio secret");
    assert!(env_local.contains("MY_TOOL_TOKEN"));
    assert!(env_local.contains(FAKE_TOKEN));
}

#[test]
#[serial_test::serial]
fn custom_project_scope_server_bridges() {
    // A project-scope `[mcp.custom]` entry must bridge on its own, with no
    // managed registry involved at all. Fakes `$HOME` to an empty tempdir for
    // the same isolation reason as `custom_bridging_absent_registry_and_empty_project_is_noop`
    // — otherwise this reads the real operator's managed registry too.
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    let mut project_custom = BTreeMap::new();
    project_custom.insert(
        "project-only".to_string(),
        CustomMcpServer::Stdio {
            command: "project-tool".to_string(),
            args: vec!["run".to_string()],
            env: BTreeMap::new(),
        },
    );

    with_fake_home(home.path(), || {
        inject_custom_trusty_mcps(ws.path(), &project_custom).expect("injection succeeds");
    });

    let servers = read_injected(ws.path());
    let entry = servers.get("project-only").expect("project entry bridged");
    assert_eq!(entry["command"], json!("project-tool"));
    assert_eq!(entry["args"], json!(["run"]));
}

#[test]
#[serial_test::serial]
fn custom_project_scope_overrides_user_scope_on_name_collision() {
    // Precedence: PROJECT scope wins over USER scope for the same name.
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    let managed_dir = home
        .path()
        .join(".trusty-tools")
        .join("trusty-mpm")
        .join("claude-config");
    seed_managed_registry(&managed_dir); // registers "duetto-memory" (http, user-scope)

    let mut project_custom = BTreeMap::new();
    project_custom.insert(
        "duetto-memory".to_string(),
        CustomMcpServer::Http {
            url: "https://project-override.example/mcp".to_string(),
            headers: BTreeMap::new(),
        },
    );

    with_fake_home(home.path(), || {
        inject_custom_trusty_mcps(ws.path(), &project_custom).expect("injection succeeds");
    });

    let servers = read_injected(ws.path());
    assert_eq!(
        servers["duetto-memory"]["url"],
        json!("https://project-override.example/mcp"),
        "project-scope definition must win over the user-scope registry entry"
    );
}

#[test]
#[serial_test::serial]
fn custom_bridging_skips_native_and_builtin_names() {
    // `slack-mcp` (native, owned by `native_mcp`) and `trusty-memory` (builtin,
    // owned by its dedicated pinning injector) must never be touched here.
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    let managed_dir = home
        .path()
        .join(".trusty-tools")
        .join("trusty-mpm")
        .join("claude-config");
    seed_managed_registry(&managed_dir);

    with_fake_home(home.path(), || {
        inject_custom_trusty_mcps(ws.path(), &BTreeMap::new()).expect("injection succeeds");
    });

    // No .mcp.json write happens for the reserved names (they're skipped, not
    // merely overwritten with the same value), but the file DOES exist because
    // of the two custom entries — assert those reserved keys are simply absent.
    let servers = read_injected(ws.path());
    assert!(
        !servers.contains_key("slack-mcp"),
        "native server must not be touched by the custom bridge"
    );
    assert!(
        !servers.contains_key("trusty-memory"),
        "builtin dedicated-injector server must not be touched by the custom bridge"
    );
}

#[test]
#[serial_test::serial]
fn custom_bridging_absent_registry_and_empty_project_is_noop() {
    // No managed registry (fresh install) and no project-scope entries → no
    // workspace `.mcp.json` written at all. Must fake `$HOME` to an empty
    // tempdir — otherwise this reads the REAL operator's managed registry
    // (whatever `tm mcp add` entries actually exist on this machine) and is
    // not a no-op at all, exactly the isolation `with_fake_home` exists for.
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    with_fake_home(home.path(), || {
        inject_custom_trusty_mcps(ws.path(), &BTreeMap::new()).expect("no-op succeeds");
    });
    assert!(!ws.path().join(".mcp.json").exists());
}

// ── validate_custom_remote_server: direct unit coverage ─────────────────────

#[test]
fn remote_custom_server_without_headers_is_injected() {
    let entry = json!({ "type": "http", "url": "https://clean.example/mcp" });
    let public = validate_custom_remote_server(&entry, "clean").expect("accepted");
    assert_eq!(public["type"], json!("http"));
    assert_eq!(public["url"], json!("https://clean.example/mcp"));
}

#[test]
fn remote_custom_server_with_headers_is_rejected() {
    // Why: the flagged security decision — no safe delivery channel for HTTP
    // auth headers today, so a headers-bearing remote entry fails closed.
    let entry = json!({
        "type": "http",
        "url": "https://secured.example/mcp",
        "headers": { "Authorization": format!("Bearer {FAKE_TOKEN}") }
    });
    assert!(validate_custom_remote_server(&entry, "secured").is_none());
}

#[test]
fn remote_custom_server_rejects_unknown_field() {
    let entry = json!({ "type": "http", "url": "https://x.example/mcp", "extra": "field" });
    assert!(validate_custom_remote_server(&entry, "x").is_none());
}

#[test]
fn remote_custom_server_rejects_missing_url() {
    let entry = json!({ "type": "http" });
    assert!(validate_custom_remote_server(&entry, "no-url").is_none());
}

#[test]
fn remote_custom_server_rejects_empty_url() {
    let entry = json!({ "type": "sse", "url": "" });
    assert!(validate_custom_remote_server(&entry, "blank-url").is_none());
}
