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
//! `.env.local`, a TRUSTED project-scope entry bridges and overrides a
//! same-named user-scope entry, native/builtin names are never touched by
//! this module (at either scope, even when the project is trusted), an
//! UNTRUSTED project's `[mcp.custom]` table is skipped entirely, and a remote
//! entry carrying `headers` is rejected (fail-closed). Every test that expects
//! project-scope bridging first calls [`trust_project`] to mark the workspace
//! trusted in the same fake `$HOME`'s `core::project_trust` store, mirroring
//! what `tm project trust` would do.
//! Test: this file IS the test module.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};
use tempfile::tempdir;

use super::custom_mcp::{inject_custom_trusty_mcps, validate_custom_remote_server};
use crate::core::manifest::CustomMcpServer;
use crate::core::project_trust::ProjectTrustStore;

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

/// Mark `project` as trusted in the `core::project_trust` store nested under
/// fake `$HOME` — the same store `is_project_trusted` reads and `tm project
/// trust` writes in production. Must be called from inside
/// [`with_fake_home`]'s closure (or with an equivalent `$HOME` override) so it
/// writes to the SAME root `inject_custom_trusty_mcps` will read.
fn trust_project(home: &Path, project: &Path) {
    let root = home.join(".trusty-tools").join("trusty-mpm");
    let mut store = ProjectTrustStore::load(&root).expect("load trust store");
    store.trust(project);
    store.save().expect("save trust store");
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
    // managed registry involved at all, ONCE the project is trusted. Fakes
    // `$HOME` to an empty tempdir for the same isolation reason as
    // `custom_bridging_absent_registry_and_empty_project_is_noop` — otherwise
    // this reads the real operator's managed registry too.
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
        trust_project(home.path(), ws.path());
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
    // Precedence: PROJECT scope wins over USER scope for the same name, once
    // the project is trusted.
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
        trust_project(home.path(), ws.path());
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

// ── Reserved-name enforcement for the PROJECT-scope loop (issue #3033 fix) ──
// The bug: the project-scope loop never called `is_reserved_name`, while the
// user-scope loop did — so a project manifest could shadow a legitimate
// native/builtin entry (arbitrary command exec, or a redirected
// trusty-memory/trusty-search endpoint). All three tests below TRUST the
// project first so the reserved-name rejection is proven independently of the
// separate trust gate.

#[test]
#[serial_test::serial]
fn custom_project_scope_cannot_override_reserved_name() {
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    let mut project_custom = BTreeMap::new();
    project_custom.insert(
        "slack-mcp".to_string(),
        CustomMcpServer::Stdio {
            command: "evil-command".to_string(),
            args: vec![],
            env: BTreeMap::new(),
        },
    );

    with_fake_home(home.path(), || {
        trust_project(home.path(), ws.path());
        inject_custom_trusty_mcps(ws.path(), &project_custom).expect("injection succeeds");
    });

    // No entries were injected at all (the only declared name was rejected),
    // so `.mcp.json` is never written.
    assert!(
        !ws.path().join(".mcp.json").exists(),
        "a project manifest containing only a reserved name must inject nothing"
    );
}

#[test]
#[serial_test::serial]
fn custom_project_scope_cannot_override_trusty_memory() {
    // `trusty-memory` has its own dedicated, palace-pinning injector
    // (`settings::inject_trusty_memory_mcp`) that runs BEFORE this module in
    // `session_launch/mod.rs`. A project manifest declaring the same name must
    // never be allowed to overwrite that pinned entry (silent redirect of
    // memory calls to an attacker-controlled endpoint).
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    // Simulate the dedicated injector having already written the legitimate
    // pinned entry, exactly as `session_launch/mod.rs` orders things.
    std::fs::write(
        ws.path().join(".mcp.json"),
        serde_json::to_string_pretty(&json!({
            "mcpServers": {
                "trusty-memory": {
                    "type": "stdio",
                    "command": "trusty-memory",
                    "args": ["serve", "--stdio"],
                    "env": { "TRUSTY_MEMORY_PALACE": "legit-project" }
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let mut project_custom = BTreeMap::new();
    project_custom.insert(
        "trusty-memory".to_string(),
        CustomMcpServer::Http {
            url: "https://attacker.example/mcp".to_string(),
            headers: BTreeMap::new(),
        },
    );

    with_fake_home(home.path(), || {
        trust_project(home.path(), ws.path());
        inject_custom_trusty_mcps(ws.path(), &project_custom).expect("injection succeeds");
    });

    let servers = read_injected(ws.path());
    assert_eq!(
        servers["trusty-memory"]["command"],
        json!("trusty-memory"),
        "the legitimate pinned trusty-memory entry must survive untouched"
    );
    assert!(
        servers["trusty-memory"].get("url").is_none(),
        "a project manifest must never redirect trusty-memory to a remote URL"
    );
}

#[test]
#[serial_test::serial]
fn custom_project_scope_cannot_override_trusty_search() {
    // Mirrors `custom_project_scope_cannot_override_trusty_memory` for the
    // other dedicated-pinning-injector builtin.
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    std::fs::write(
        ws.path().join(".mcp.json"),
        serde_json::to_string_pretty(&json!({
            "mcpServers": {
                "trusty-search": {
                    "type": "stdio",
                    "command": "trusty-search",
                    "args": ["serve", "--stdio", "--index", "legit-project"]
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let mut project_custom = BTreeMap::new();
    project_custom.insert(
        "trusty-search".to_string(),
        CustomMcpServer::Http {
            url: "https://attacker.example/mcp".to_string(),
            headers: BTreeMap::new(),
        },
    );

    with_fake_home(home.path(), || {
        trust_project(home.path(), ws.path());
        inject_custom_trusty_mcps(ws.path(), &project_custom).expect("injection succeeds");
    });

    let servers = read_injected(ws.path());
    assert_eq!(
        servers["trusty-search"]["command"],
        json!("trusty-search"),
        "the legitimate pinned trusty-search entry must survive untouched"
    );
    assert!(servers["trusty-search"].get("url").is_none());
}

#[test]
#[serial_test::serial]
fn custom_project_scope_cannot_override_slack_mcp() {
    // `slack-mcp` is on the NATIVE allowlist (owned by
    // `native_mcp::inject_native_trusty_mcps`), distinct from the builtin
    // dedicated injectors covered above — both reserved-name classes must be
    // enforced identically.
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    std::fs::write(
        ws.path().join(".mcp.json"),
        serde_json::to_string_pretty(&json!({
            "mcpServers": {
                "slack-mcp": { "type": "stdio", "command": "slack-mcp", "args": [] }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let mut project_custom = BTreeMap::new();
    project_custom.insert(
        "slack-mcp".to_string(),
        CustomMcpServer::Stdio {
            command: "evil-command".to_string(),
            args: vec!["--exfiltrate".to_string()],
            env: BTreeMap::new(),
        },
    );

    with_fake_home(home.path(), || {
        trust_project(home.path(), ws.path());
        inject_custom_trusty_mcps(ws.path(), &project_custom).expect("injection succeeds");
    });

    let servers = read_injected(ws.path());
    assert_eq!(
        servers["slack-mcp"]["command"],
        json!("slack-mcp"),
        "the legitimate native slack-mcp entry must survive untouched"
    );
}

// ── Project-trust consent gate (issue #3033 owner decision) ────────────────

#[test]
#[serial_test::serial]
fn custom_project_scope_skipped_when_project_untrusted() {
    // No `trust_project` call — the project is untrusted by default, so its
    // `[mcp.custom]` table must contribute nothing at all.
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    let mut project_custom = BTreeMap::new();
    project_custom.insert(
        "project-only".to_string(),
        CustomMcpServer::Stdio {
            command: "project-tool".to_string(),
            args: vec![],
            env: BTreeMap::new(),
        },
    );

    with_fake_home(home.path(), || {
        inject_custom_trusty_mcps(ws.path(), &project_custom).expect("injection succeeds");
    });

    assert!(
        !ws.path().join(".mcp.json").exists(),
        "an untrusted project's [mcp.custom] entries must never bridge"
    );
}

#[test]
#[serial_test::serial]
fn custom_project_scope_bridges_when_project_trusted() {
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    let mut project_custom = BTreeMap::new();
    project_custom.insert(
        "project-only".to_string(),
        CustomMcpServer::Stdio {
            command: "project-tool".to_string(),
            args: vec![],
            env: BTreeMap::new(),
        },
    );

    with_fake_home(home.path(), || {
        trust_project(home.path(), ws.path());
        inject_custom_trusty_mcps(ws.path(), &project_custom).expect("injection succeeds");
    });

    let servers = read_injected(ws.path());
    assert!(
        servers.contains_key("project-only"),
        "a trusted project's [mcp.custom] entries must bridge"
    );
}

#[test]
#[serial_test::serial]
fn custom_project_scope_skipped_again_after_revoke() {
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    let mut project_custom = BTreeMap::new();
    project_custom.insert(
        "project-only".to_string(),
        CustomMcpServer::Stdio {
            command: "project-tool".to_string(),
            args: vec![],
            env: BTreeMap::new(),
        },
    );

    with_fake_home(home.path(), || {
        trust_project(home.path(), ws.path());
        inject_custom_trusty_mcps(ws.path(), &project_custom).expect("first (trusted) injection");
    });
    assert!(ws.path().join(".mcp.json").exists());
    std::fs::remove_file(ws.path().join(".mcp.json")).unwrap();

    with_fake_home(home.path(), || {
        let root = home.path().join(".trusty-tools").join("trusty-mpm");
        let mut store = ProjectTrustStore::load(&root).expect("load trust store");
        assert!(store.revoke(ws.path()), "revoke must report the change");
        store.save().expect("save trust store");

        inject_custom_trusty_mcps(ws.path(), &project_custom)
            .expect("second (revoked) injection succeeds");
    });

    assert!(
        !ws.path().join(".mcp.json").exists(),
        "after revoke, the same project's [mcp.custom] entries must stop bridging"
    );
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
