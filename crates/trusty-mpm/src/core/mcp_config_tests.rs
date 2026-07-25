//! Unit tests for `mcp_config` — split out to keep the production module
//! under the 500-SLOC cap (mirrors the `native_mcp_tests.rs` /
//! `custom_mcp_tests.rs` split pattern already used in this crate).
//! What: CRUD round-trips for the user-scope `mcpServers` map, the
//! `BUILTIN_MANAGED_MCP_SERVERS`/`UNCONDITIONAL_BUILTIN_MCP_SERVERS`/
//! `CONDITIONAL_BUILTIN_MCP_SERVERS` drift guard, and the trust-derivation
//! coverage for [`super::managed_mcp_server_names`],
//! [`super::launch_trusted_mcp_names_from`], and
//! [`super::resolve_conditional_mcp_toggles`] — including the issue #3934
//! regression (a conditional builtin must drop out of the trust list when its
//! injector toggle is off).
//! Test: this file IS the test module.

use super::*;
use tempfile::TempDir;

fn stdio(command: &str, args: &[&str]) -> Value {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    build_stdio_entry(command, &args, &Map::new())
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn strip_arg_separator_removes_only_leading() {
    // The npx shape: a leading `--` is dropped exactly once.
    let args = strings(&["--", "-y", "@modelcontextprotocol/server-github"]);
    assert_eq!(
        strip_arg_separator(&args),
        &["-y", "@modelcontextprotocol/server-github"]
    );
    // Only the FIRST token is stripped — a second leading `--` survives.
    let doubled = strings(&["--", "--", "-y"]);
    assert_eq!(strip_arg_separator(&doubled), &["--", "-y"]);
}

#[test]
fn strip_arg_separator_preserves_deeper_and_absent() {
    // A `--` that is not the first token is a real argument — keep it.
    let deeper = strings(&["run", "--", "tool"]);
    assert_eq!(strip_arg_separator(&deeper), &["run", "--", "tool"]);
    // No separator at all → unchanged.
    let clean = strings(&["-y", "pkg"]);
    assert_eq!(strip_arg_separator(&clean), &["-y", "pkg"]);
    // Empty slice → empty (no panic).
    let empty: Vec<String> = Vec::new();
    assert!(strip_arg_separator(&empty).is_empty());
}

#[test]
fn build_stdio_entry_has_expected_shape() {
    let e = stdio("echo", &["hi"]);
    assert_eq!(e["type"], "stdio");
    assert_eq!(e["command"], "echo");
    assert_eq!(e["args"], serde_json::json!(["hi"]));
    assert!(e.get("env").is_none(), "env omitted when empty");
}

#[test]
fn build_stdio_entry_with_env() {
    let mut env = Map::new();
    env.insert("API_KEY".into(), Value::String("xxx".into()));
    let e = build_stdio_entry("srv", &[], &env);
    assert_eq!(e["env"]["API_KEY"], "xxx");
    assert_eq!(e["args"], serde_json::json!([]), "args always present");
}

#[test]
fn build_remote_entry_http() {
    let e = build_remote_entry(McpTransport::Http, "https://x/mcp", &Map::new());
    assert_eq!(e["type"], "http");
    assert_eq!(e["url"], "https://x/mcp");
    assert!(e.get("headers").is_none());
    // sse shares the shape with a different discriminant.
    let s = build_remote_entry(McpTransport::Sse, "https://x/sse", &Map::new());
    assert_eq!(s["type"], "sse");
}

#[test]
fn build_remote_entry_with_headers() {
    let mut h = Map::new();
    h.insert("Authorization".into(), Value::String("Bearer t".into()));
    let e = build_remote_entry(McpTransport::Http, "https://x", &h);
    assert_eq!(e["headers"]["Authorization"], "Bearer t");
}

#[test]
fn add_then_get_roundtrips() {
    let tmp = TempDir::new().unwrap();
    // config dir does not exist yet — add must create it + a fresh {}.
    let cfg = tmp.path().join("claude-config");
    assert!(add_server(&cfg, "echo", stdio("echo", &["hi"])).unwrap());
    let got = get_server(&cfg, "echo").unwrap().expect("server present");
    assert_eq!(got["command"], "echo");
    // The file must be valid JSON with a top-level mcpServers map.
    let text = std::fs::read_to_string(cfg.join(CLAUDE_JSON)).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    assert!(v["mcpServers"]["echo"].is_object());
}

#[test]
fn add_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("cc");
    assert!(add_server(&cfg, "echo", stdio("echo", &["hi"])).unwrap());
    // Second identical add → no change, no rewrite.
    assert!(!add_server(&cfg, "echo", stdio("echo", &["hi"])).unwrap());
}

#[test]
fn add_preserves_other_keys() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("cc");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
        cfg.join(CLAUDE_JSON),
        r#"{"oauthAccount":{"keep":"me"},"mcpServers":{"pre":{"type":"stdio","command":"pre","args":[]}}}"#,
    )
    .unwrap();
    add_server(&cfg, "echo", stdio("echo", &["hi"])).unwrap();
    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(cfg.join(CLAUDE_JSON)).unwrap()).unwrap();
    assert_eq!(v["oauthAccount"]["keep"], "me", "unrelated key preserved");
    assert!(v["mcpServers"]["pre"].is_object(), "existing server kept");
    assert!(v["mcpServers"]["echo"].is_object(), "new server added");
}

#[test]
fn remove_existing_returns_true() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("cc");
    add_server(&cfg, "echo", stdio("echo", &["hi"])).unwrap();
    assert!(remove_server(&cfg, "echo").unwrap());
    assert!(get_server(&cfg, "echo").unwrap().is_none());
}

#[test]
fn remove_absent_returns_false() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("cc");
    assert!(!remove_server(&cfg, "nope").unwrap());
}

#[test]
fn get_absent_returns_none() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("cc");
    assert!(get_server(&cfg, "nope").unwrap().is_none());
}

#[test]
fn list_returns_all_added() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("cc");
    add_server(&cfg, "a", stdio("a", &[])).unwrap();
    add_server(
        &cfg,
        "b",
        build_remote_entry(McpTransport::Http, "https://b", &Map::new()),
    )
    .unwrap();
    let all = list_servers(&cfg).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all["a"]["type"], "stdio");
    assert_eq!(all["b"]["type"], "http");
}

#[test]
fn add_quarantines_malformed_json() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("cc");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(cfg.join(CLAUDE_JSON), b"{ not valid json !!!").unwrap();
    // add must succeed despite the corrupt file.
    add_server(&cfg, "echo", stdio("echo", &["hi"])).unwrap();
    assert!(
        cfg.join(".claude.json.corrupt").exists(),
        "corrupt file quarantined, not deleted"
    );
    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(cfg.join(CLAUDE_JSON)).unwrap()).unwrap();
    assert!(v["mcpServers"]["echo"].is_object());
}

#[test]
fn builtin_server_entry_matches_known_servers() {
    // Every name in the builtin list resolves to a stdio launch entry.
    for name in BUILTIN_MANAGED_MCP_SERVERS {
        let e = builtin_server_entry(name).expect("builtin resolves");
        assert_eq!(e["type"], "stdio");
        assert_eq!(e["command"], *name, "command matches the binary name");
        assert!(e["args"].is_array(), "args always present");
    }
    // The exact launch args must stay pinned (parity with ensure_mcp_config).
    assert_eq!(
        builtin_server_entry("trusty-memory").unwrap()["args"],
        serde_json::json!(["serve", "--stdio"])
    );
    assert_eq!(
        builtin_server_entry("trusty-mpm").unwrap()["args"],
        serde_json::json!(["serve", "--stdio"])
    );
    assert_eq!(
        builtin_server_entry("trusty-search").unwrap()["args"],
        serde_json::json!(["serve"])
    );
    // Unknown names yield None.
    assert!(builtin_server_entry("not-a-builtin").is_none());
}

#[test]
fn builtin_mcp_server_split_unions_to_full_set() {
    // Drift guard: UNCONDITIONAL ∪ CONDITIONAL must always equal the full
    // reserved-name set, sorted, with no overlap or omission.
    let mut split: Vec<&str> = UNCONDITIONAL_BUILTIN_MCP_SERVERS
        .iter()
        .chain(CONDITIONAL_BUILTIN_MCP_SERVERS.iter())
        .copied()
        .collect();
    split.sort_unstable();
    assert_eq!(split, BUILTIN_MANAGED_MCP_SERVERS);
}

#[test]
fn managed_mcp_server_names_defaults_to_builtin() {
    let names = managed_mcp_server_names(&serde_json::json!({}), true, true);
    assert_eq!(
        names,
        vec![
            "trusty-memory",
            "trusty-mpm",
            "trusty-review",
            "trusty-search"
        ]
    );
}

#[test]
fn managed_mcp_server_names_unions_builtin_with_configured() {
    let config = serde_json::json!({
        "mcpServers": {
            "aaa-custom": { "type": "stdio", "command": "x", "args": [] },
            "trusty-memory": { "type": "stdio", "command": "trusty-memory", "args": [] }
        }
    });
    let names = managed_mcp_server_names(&config, true, true);
    // Built-in four + the custom server, sorted + deduped (trusty-memory
    // appears once despite being in both the builtin list and the config).
    assert_eq!(
        names,
        vec![
            "aaa-custom",
            "trusty-memory",
            "trusty-mpm",
            "trusty-review",
            "trusty-search"
        ]
    );
}

#[test]
fn managed_mcp_server_names_excludes_disabled_conditional_builtins() {
    // Issue #3934: when a manifest disables the trusty-memory/trusty-search
    // injector for this run, the derived trust list must NOT include the
    // name (unrelated to the config's own `tm mcp add` registry, which is
    // a SEPARATE, always-trusted union source — see the next test for
    // that source's own, unrelated collision behaviour).
    let config = serde_json::json!({
        "mcpServers": {
            "aaa-custom": { "type": "stdio", "command": "x", "args": [] }
        }
    });
    let names = managed_mcp_server_names(&config, false, false);
    assert!(
        !names.contains(&"trusty-memory".to_string()),
        "trusty-memory must be excluded when its injector did not run: {names:?}"
    );
    assert!(!names.contains(&"trusty-search".to_string()));
    // The unconditional two are always present regardless.
    assert!(names.contains(&"trusty-mpm".to_string()));
    assert!(names.contains(&"trusty-review".to_string()));
}

#[test]
fn managed_mcp_server_names_registry_collision_is_a_separate_trusted_source() {
    // Documents the (correct, unrelated) behaviour a naive reading of the
    // #3934 fix might expect to change: the `config` parameter is the
    // operator's OWN `tm mcp add` registry (`<config_dir>/.claude.json`'s
    // top-level `mcpServers`), a source this module's existing security
    // model already treats as trusted (see the module doc) — NOT the
    // workspace's `.mcp.json` a cloned repo controls. A registry entry
    // happening to be named `trusty-memory` is unioned in regardless of
    // the conditional-injector toggle, exactly like any other registry
    // name; the toggle only gates the FRAMEWORK BUILTIN union member.
    let config = serde_json::json!({
        "mcpServers": {
            "trusty-memory": { "type": "stdio", "command": "trusty-memory", "args": [] }
        }
    });
    let names = managed_mcp_server_names(&config, false, false);
    assert!(
        names.contains(&"trusty-memory".to_string()),
        "a registry-sourced name is trusted independently of the conditional-builtin toggle: {names:?}"
    );
}

#[test]
fn managed_mcp_server_names_partial_conditional_toggle() {
    // Only trusty_search disabled: trusty-memory still trusted, trusty-search not.
    let names = managed_mcp_server_names(&serde_json::json!({}), true, false);
    assert!(names.contains(&"trusty-memory".to_string()));
    assert!(!names.contains(&"trusty-search".to_string()));
}

#[test]
fn mcp_server_names_reads_workspace_mcp_json() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join(".mcp.json"),
        serde_json::json!({
            "mcpServers": {
                "trusty-mpm": {"type": "stdio", "command": "trusty-mpm", "args": []},
                "tickets-mcp": {"type": "stdio", "command": "tickets-mcp", "args": []},
            }
        })
        .to_string(),
    )
    .unwrap();
    let names = mcp_server_names(tmp.path());
    assert_eq!(names, vec!["tickets-mcp", "trusty-mpm"], "sorted keys");
}

#[test]
fn mcp_server_names_empty_when_absent() {
    let tmp = TempDir::new().unwrap();
    // No .mcp.json written — must not fail, must yield an empty vector.
    assert!(mcp_server_names(tmp.path()).is_empty());
}

#[test]
fn launch_trusted_mcp_names_from_unions_registry() {
    // Why (#3926): the interactive `tm launch` path must trust the SAME
    // provenance-safe set as the daemon-managed path — builtin four union
    // the operator's own `tm mcp add` registry, regardless of what a
    // cloned repo's workspace `.mcp.json` separately declares (this
    // function never reads a workspace's `.mcp.json` keys — the
    // conditional-builtin toggles are supplied explicitly by the caller,
    // issue #3934).
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("claude-config");
    add_server(&cfg, "slack-mcp", stdio("slack-mcp", &["serve"])).unwrap();

    let names = launch_trusted_mcp_names_from(&cfg, true, true);
    assert_eq!(
        names,
        vec![
            "slack-mcp",
            "trusty-memory",
            "trusty-mpm",
            "trusty-review",
            "trusty-search"
        ]
    );
}

#[test]
fn launch_trusted_mcp_names_from_defaults_to_builtin_when_absent() {
    // Why (#3926): a fresh install with no `tm mcp add` registry yet must
    // still trust exactly the framework builtin four — never error, never
    // an empty list (the four builtins always launch, so they must always
    // be pre-approved).
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("does-not-exist");

    let names = launch_trusted_mcp_names_from(&cfg, true, true);
    assert_eq!(
        names,
        vec![
            "trusty-memory",
            "trusty-mpm",
            "trusty-review",
            "trusty-search"
        ]
    );
}

#[test]
fn launch_trusted_mcp_names_from_excludes_disabled_conditional_builtins() {
    // Issue #3934 (the actual attack, unit-level): a manifest disabling
    // the trusty-memory injector for this run must drop the name from
    // the trust list even though the operator's own registry (or, in the
    // real attack, a spoofed workspace `.mcp.json` entry the caller does
    // not even consult here) might otherwise suggest it belongs.
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("claude-config");

    let names = launch_trusted_mcp_names_from(&cfg, false, false);
    assert!(!names.contains(&"trusty-memory".to_string()));
    assert!(!names.contains(&"trusty-search".to_string()));
    assert!(names.contains(&"trusty-mpm".to_string()));
    assert!(names.contains(&"trusty-review".to_string()));
}

#[test]
fn resolve_conditional_mcp_toggles_defaults_to_both_on() {
    // No manifest at all (compiled-in default): both conditional builtins
    // are enabled, matching today's zero-regression behaviour.
    let tmp_home = TempDir::new().unwrap();
    let tmp_project = TempDir::new().unwrap();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    let (memory, search) = resolve_conditional_mcp_toggles(&fw, tmp_project.path());
    assert!(memory);
    assert!(search);
}

#[test]
fn resolve_conditional_mcp_toggles_honors_project_manifest_toggle() {
    // Issue #3934: a project-scope manifest.toml (the attack's vector —
    // git-tracked, arrives WITH a cloned repo) disabling trusty_memory
    // must resolve to `inject_trusty_memory == false`, and must NOT affect
    // the untouched trusty_search toggle.
    let tmp_home = TempDir::new().unwrap();
    let tmp_project = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp_project.path().join(".trusty-mpm")).unwrap();
    std::fs::write(
        tmp_project.path().join(".trusty-mpm").join("manifest.toml"),
        "[mcp]\ntrusty_memory = false\n",
    )
    .unwrap();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    let (memory, search) = resolve_conditional_mcp_toggles(&fw, tmp_project.path());
    assert!(!memory, "project manifest toggle must be honored");
    assert!(search, "untouched toggle must keep its default");
}
